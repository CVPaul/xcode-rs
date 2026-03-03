use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use globset::Glob;
use regex::Regex;
use walkdir::WalkDir;

/// GrepSearchTool searches file contents for lines matching a regex pattern.
/// Supports optional file glob filter (include). Returns up to 200 matching lines.
pub struct GrepSearchTool;

#[async_trait]
impl Tool for GrepSearchTool {
    fn name(&self) -> &str {
        "grep_search"
    }

    fn description(&self) -> &str {
        "Search file contents for lines matching a regular expression. \
        Optionally filter files by a glob pattern (e.g., '*.rs'). \
        Returns up to 200 matching lines in format: 'filepath:line_num: content'."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression to search for (e.g., 'fn main', 'use std::')"
                },
                "path": {
                    "type": "string",
                    "description": "Root directory to search from (default: working directory)"
                },
                "include": {
                    "type": "string",
                    "description": "Optional glob pattern to filter which files to search (e.g., '*.rs', '*.{rs,toml}')"
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

        // Compile regex — return error if invalid
        let regex = match Regex::new(&pattern) {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult {
                    output: format!("Invalid regex pattern '{}': {}", pattern, e),
                    is_error: true,
                });
            }
        };

        // Root directory to search
        let root = if let Some(p) = args["path"].as_str() {
            std::path::PathBuf::from(p)
        } else {
            ctx.working_dir.clone()
        };

        // Optional file include glob filter
        let include_matcher = if let Some(inc) = args["include"].as_str() {
            match Glob::new(inc) {
                Ok(g) => Some(g.compile_matcher()),
                Err(e) => {
                    return Ok(ToolResult {
                        output: format!("Invalid include glob '{}': {}", inc, e),
                        is_error: true,
                    });
                }
            }
        } else {
            None
        };

        let mut matches: Vec<String> = Vec::new();
        const MAX_LINES: usize = 200;

        'outer: for entry in WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            // Only process regular files
            if !entry.file_type().is_file() {
                continue;
            }

            let abs_path = entry.path();

            // Apply include glob filter if specified
            if let Some(ref matcher) = include_matcher {
                let filename = abs_path
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default();
                // Also try matching against the full relative path for patterns like "src/*.rs"
                let rel_path = abs_path
                    .strip_prefix(&root)
                    .unwrap_or(abs_path)
                    .to_string_lossy()
                    .to_string();

                let file_matches =
                    matcher.is_match(filename.as_ref()) || matcher.is_match(rel_path.as_str());

                if !file_matches {
                    continue;
                }
            }

            // Read file as UTF-8, skip binary/unreadable files
            let content = match std::fs::read_to_string(abs_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Search lines for pattern
            for (line_idx, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    matches.push(format!(
                        "{}:{}: {}",
                        abs_path.to_string_lossy(),
                        line_idx + 1, // 1-based line number
                        line
                    ));

                    if matches.len() >= MAX_LINES {
                        break 'outer;
                    }
                }
            }
        }

        if matches.is_empty() {
            return Ok(ToolResult {
                output: format!("No matches found for pattern: {}", pattern),
                is_error: false,
            });
        }

        Ok(ToolResult {
            output: matches.join("\n"),
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn ctx_with_dir(dir: &TempDir) -> ToolContext {
        ToolContext {
            working_dir: dir.path().to_path_buf(),
            sandbox_enabled: false,
            io: std::sync::Arc::new(crate::io::NullIO),
            compact_mode: false,
            lsp_client: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            mcp_client: None,
            nesting_depth: 0,
            llm: std::sync::Arc::new(crate::llm::NullLlmProvider),
            tools: std::sync::Arc::new(crate::tools::ToolRegistry::new()),
            permissions: vec![],
            formatters: std::collections::HashMap::new(),
        }
    }
    #[tokio::test]
    async fn test_grep_finds_matches() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        let tool = GrepSearchTool;
        let args = serde_json::json!({ "pattern": "fn main" });
        let result = tool.execute(args, &ctx_with_dir(&tmp)).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("fn main"));
        assert!(result.output.contains("main.rs"));
        assert!(result.output.contains(":1:"));
    }

    #[tokio::test]
    async fn test_grep_no_matches() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("hello.rs"), "fn hello() {}").unwrap();

        let tool = GrepSearchTool;
        let args = serde_json::json!({ "pattern": "fn main" });
        let result = tool.execute(args, &ctx_with_dir(&tmp)).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("No matches found"));
    }

    #[tokio::test]
    async fn test_grep_include_filter() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("code.rs"), "use std::path;").unwrap();
        fs::write(tmp.path().join("config.toml"), "use = 'value'").unwrap();

        let tool = GrepSearchTool;
        // Only search .rs files
        let args = serde_json::json!({ "pattern": "use", "include": "*.rs" });
        let result = tool.execute(args, &ctx_with_dir(&tmp)).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("code.rs"));
        assert!(!result.output.contains("config.toml"));
    }

    #[tokio::test]
    async fn test_grep_invalid_regex() {
        let tmp = TempDir::new().unwrap();
        let tool = GrepSearchTool;
        // Unclosed bracket is invalid regex
        let args = serde_json::json!({ "pattern": "[invalid" });
        let result = tool.execute(args, &ctx_with_dir(&tmp)).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Invalid regex"));
    }

    #[tokio::test]
    async fn test_grep_missing_pattern() {
        let tmp = TempDir::new().unwrap();
        let tool = GrepSearchTool;
        let args = serde_json::json!({});
        let result = tool.execute(args, &ctx_with_dir(&tmp)).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Missing required argument"));
    }
}
