use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use globset::Glob;
use std::time::SystemTime;
use walkdir::WalkDir;

/// GlobSearchTool finds files matching a glob pattern, sorted by modification
/// time (newest first), up to 100 results.
pub struct GlobSearchTool;

#[async_trait]
impl Tool for GlobSearchTool {
    fn name(&self) -> &str {
        "glob_search"
    }

    fn description(&self) -> &str {
        "Search for files matching a glob pattern (e.g., '**/*.rs', 'src/**/*.toml'). \
        Results are sorted by modification time (newest first), max 100 files. \
        Returns absolute paths, one per line."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files (e.g., '**/*.rs')"
                },
                "path": {
                    "type": "string",
                    "description": "Root directory to search from (default: working directory)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        let pattern = match args["pattern"].as_str() {
            Some(p) => p.to_string(),
            None => {
                return Ok(ToolResult {
                    output: "Missing required argument: pattern".to_string(),
                    is_error: true,
                });
            }
        };

        // Use provided path or fall back to the session working directory
        let root = if let Some(p) = args["path"].as_str() {
            std::path::PathBuf::from(p)
        } else {
            ctx.working_dir.clone()
        };

        // Compile the glob pattern
        let glob = match Glob::new(&pattern) {
            Ok(g) => g.compile_matcher(),
            Err(e) => {
                return Ok(ToolResult {
                    output: format!("Invalid glob pattern '{}': {}", pattern, e),
                    is_error: true,
                });
            }
        };

        // Walk the directory tree and collect matching files with their mtimes
        let mut matches: Vec<(SystemTime, std::path::PathBuf)> = Vec::new();

        for entry in WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            // Only match files, not directories
            if !entry.file_type().is_file() {
                continue;
            }

            // Get the path relative to root for glob matching
            let abs_path = entry.path();
            let rel_path = abs_path.strip_prefix(&root).unwrap_or(abs_path);

            // Test the relative path string against the glob
            let rel_str = rel_path.to_string_lossy();
            if glob.is_match(rel_str.as_ref()) {
                // Get mtime for sorting (default to UNIX_EPOCH if unavailable)
                let mtime = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                matches.push((mtime, abs_path.to_path_buf()));
            }
        }

        if matches.is_empty() {
            return Ok(ToolResult {
                output: format!("No files found matching pattern: {}", pattern),
                is_error: false,
            });
        }

        // Sort by mtime descending (newest first)
        matches.sort_by(|a, b| b.0.cmp(&a.0));

        // Limit to 100 results
        matches.truncate(100);

        let lines: Vec<String> = matches
            .into_iter()
            .map(|(_, path)| path.to_string_lossy().to_string())
            .collect();

        Ok(ToolResult {
            output: lines.join("\n"),
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn ctx_with_dir(dir: &TempDir) -> ToolContext {
        ToolContext {
            working_dir: dir.path().to_path_buf(),
            sandbox_enabled: false,
            confirm_destructive: false,
        }
    }
    #[tokio::test]
    async fn test_glob_search_finds_files() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("hello.rs"), "fn main() {}").unwrap();
        fs::write(tmp.path().join("world.rs"), "fn other() {}").unwrap();
        fs::write(tmp.path().join("readme.md"), "# readme").unwrap();

        let tool = GlobSearchTool;
        let args = serde_json::json!({ "pattern": "**/*.rs" });
        let result = tool.execute(args, &ctx_with_dir(&tmp)).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("hello.rs") || result.output.contains("world.rs"));
        assert!(!result.output.contains("readme.md"));
    }

    #[tokio::test]
    async fn test_glob_search_no_matches() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("hello.txt"), "text").unwrap();

        let tool = GlobSearchTool;
        let args = serde_json::json!({ "pattern": "**/*.rs" });
        let result = tool.execute(args, &ctx_with_dir(&tmp)).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("No files found"));
    }

    #[tokio::test]
    async fn test_glob_search_missing_pattern() {
        let tmp = TempDir::new().unwrap();
        let tool = GlobSearchTool;
        let args = serde_json::json!({});
        let result = tool.execute(args, &ctx_with_dir(&tmp)).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Missing required argument"));
    }

    #[tokio::test]
    async fn test_glob_search_invalid_pattern() {
        let tmp = TempDir::new().unwrap();
        let tool = GlobSearchTool;
        // An unclosed bracket is an invalid glob
        let args = serde_json::json!({ "pattern": "[invalid" });
        let result = tool.execute(args, &ctx_with_dir(&tmp)).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Invalid glob pattern"));
    }

    #[tokio::test]
    async fn test_glob_search_custom_path() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file.toml"), "[package]").unwrap();

        let tool = GlobSearchTool;
        // Search from sub directory explicitly
        let args = serde_json::json!({
            "pattern": "**/*.toml",
            "path": sub.to_str().unwrap()
        });
        let ctx = ToolContext {
            working_dir: PathBuf::from("/tmp"),
            sandbox_enabled: false,
            confirm_destructive: false,
        };
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("file.toml"));
    }
}
