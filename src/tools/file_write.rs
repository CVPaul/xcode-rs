use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;

pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file, creating parent directories if needed."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (relative to working_dir or absolute)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = match args["path"].as_str() {
            Some(p) => p.to_string(),
            None => {
                return Ok(ToolResult {
                    output: "Error: 'path' parameter is required".to_string(),
                    is_error: true,
                });
            }
        };

        let content = match args["content"].as_str() {
            Some(c) => c.to_string(),
            None => {
                return Ok(ToolResult {
                    output: "Error: 'content' parameter is required".to_string(),
                    is_error: true,
                });
            }
        };

        let path = resolve_path(&path_str, &ctx.working_dir);

        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Ok(ToolResult {
                    output: format!(
                        "Error: failed to create directories for '{}': {}",
                        path_str, e
                    ),
                    is_error: true,
                });
            }
        }

        let bytes = content.len();
        if let Err(e) = std::fs::write(&path, &content) {
            return Ok(ToolResult {
                output: format!("Error: failed to write file '{}': {}", path_str, e),
                is_error: true,
            });
        }

        Ok(ToolResult {
            output: format!("Written {} bytes to {}", bytes, path_str),
            is_error: false,
        })
    }
}

fn resolve_path(path_str: &str, working_dir: &PathBuf) -> PathBuf {
    let p = PathBuf::from(path_str);
    if p.is_absolute() {
        p
    } else {
        working_dir.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            working_dir: dir.to_path_buf(),
            sandbox_enabled: false,
        }
    }

    #[tokio::test]
    async fn test_file_write_creates_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("output.txt");
        let ctx = make_ctx(dir.path());

        let tool = FileWriteTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "path": path.to_string_lossy().to_string(),
                    "content": "hello world\n"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("bytes"));
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "hello world\n");
    }

    #[tokio::test]
    async fn test_file_write_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a").join("b").join("c.txt");
        let ctx = make_ctx(dir.path());

        let tool = FileWriteTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "path": path.to_string_lossy().to_string(),
                    "content": "nested content"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(path.exists());
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "nested content");
    }
}
