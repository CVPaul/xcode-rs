use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use ignore::WalkBuilder;
use std::fs;
use std::path::PathBuf;

pub struct ListDirectoryTool;

#[async_trait]
impl Tool for ListDirectoryTool {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List directory contents with file metadata. Respects .gitignore patterns by default. Shows files and subdirectories with their types."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list (default: working directory). Relative paths resolved from working_dir."
                },
                "recursive": {
                    "type": "boolean",
                    "description": "If true, list recursively up to max_depth (default: false)"
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum recursion depth when recursive=true (default: 3)"
                },
                "ignore_gitignore": {
                    "type": "boolean",
                    "description": "If true, do NOT respect .gitignore (show all files). Default: false (respects .gitignore)"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        let path = args["path"].as_str().map(|s| s.to_string());
        let recursive = args["recursive"].as_bool().unwrap_or(false);
        let max_depth = args["max_depth"].as_u64().unwrap_or(3) as usize;
        let ignore_gitignore = args["ignore_gitignore"].as_bool().unwrap_or(false);

        let root = match path {
            Some(ref p) => {
                let pb = PathBuf::from(p);
                if pb.is_absolute() {
                    pb
                } else {
                    ctx.working_dir.join(pb)
                }
            }
            None => ctx.working_dir.clone(),
        };

        let abs_root = match fs::canonicalize(&root) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    output: format!("Error: could not resolve path '{}': {}", root.display(), e),
                    is_error: true,
                });
            }
        };

        let mut files = Vec::new();
        let mut dirs = Vec::new();
        let mut total_entries = 0;
        let mut truncated = false;
        let mut walker = WalkBuilder::new(&abs_root);
walker.max_depth(if recursive { Some(max_depth) } else { Some(1) });
        if ignore_gitignore {
            walker.ignore(false).git_ignore(false).git_global(false).git_exclude(false);
        }
        let walker = walker.build();

        for entry in walker {
            match entry {
                Ok(e) => {
                    let path = e.path();
                    if path == abs_root {
                        continue;
                    }
                    if e.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                        dirs.push(path.to_path_buf());
                    } else if e.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                        files.push(path.to_path_buf());
                    }
                    total_entries += 1;
                    if total_entries >= 500 {
                        truncated = true;
                        break;
                    }
                }
                Err(err) => {
                    return Ok(ToolResult {
                        output: format!("Error reading directory: {}", err),
                        is_error: true,
                    });
                }
            }
        }

        files.sort();
        dirs.sort();

        let mut output = format!("Directory: {}\n", abs_root.display());
        for d in &dirs {
            output.push_str(&format!("  [dir]  {}/\n", d.strip_prefix(&abs_root).unwrap_or(d).display()));
        }
        for f in &files {
            let meta = fs::metadata(f).ok();
            let size = meta.map(|m| m.len()).unwrap_or(0);
            output.push_str(&format!("  [file] {} ({} bytes)\n", f.strip_prefix(&abs_root).unwrap_or(f).display(), size));
        }
        if truncated {
            output.push_str("(Output truncated to 500 entries)\n");
        }
        output.push_str(&format!("\nTotal: {} files, {} directories", files.len(), dirs.len()));

        Ok(ToolResult {
            output,
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs::{self, File};
    use std::io::Write;
    use crate::tools::ToolContext;

    fn test_ctx(dir: &std::path::Path) -> ToolContext {
    ToolContext {
        working_dir: dir.to_path_buf(),
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
    async fn test_ls_current_dir() {
        let tmp = tempdir().unwrap();
        let file_path = tmp.path().join("foo.txt");
        File::create(&file_path).unwrap();
        let subdir = tmp.path().join("bar");
        fs::create_dir(&subdir).unwrap();
        let tool = ListDirectoryTool;
        let args = serde_json::json!({});
        let ctx = test_ctx(tmp.path());
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.output.contains("[file] foo.txt"));
        assert!(result.output.contains("[dir]  bar/"));
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn test_ls_nonexistent() {
        let tmp = tempdir().unwrap();
        let tool = ListDirectoryTool;
        let args = serde_json::json!({"path": "does_not_exist"});
        let ctx = test_ctx(tmp.path());
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("could not resolve path"));
    }

    #[tokio::test]
    async fn test_ls_recursive() {
        let tmp = tempdir().unwrap();
        let subdir = tmp.path().join("sub");
        fs::create_dir(&subdir).unwrap();
        let file1 = tmp.path().join("a.txt");
        let file2 = subdir.join("b.txt");
        File::create(&file1).unwrap();
        File::create(&file2).unwrap();
        let tool = ListDirectoryTool;
        let args = serde_json::json!({"recursive": true, "max_depth": 2});
        let ctx = test_ctx(tmp.path());
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.output.contains("[file] a.txt"));
        assert!(result.output.contains("[file] sub/b.txt"));
        assert!(result.output.contains("[dir]  sub/"));
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn test_ls_respects_gitignore() {
        let tmp = tempdir().unwrap();
        let file1 = tmp.path().join("visible.txt");
        let file2 = tmp.path().join("hidden.txt");
        File::create(&file1).unwrap();
        File::create(&file2).unwrap();
        // The ignore crate needs a .git dir to recognize the repo root and respect .gitignore
        fs::create_dir(tmp.path().join(".git")).unwrap();
        let gitignore = tmp.path().join(".gitignore");
        let mut f = File::create(&gitignore).unwrap();
        writeln!(f, "hidden.txt").unwrap();
        let tool = ListDirectoryTool;
        let args = serde_json::json!({});
        let ctx = test_ctx(tmp.path());
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.output.contains("[file] visible.txt"));
        assert!(!result.output.contains("hidden.txt"));
        assert!(!result.is_error);
    }
}
