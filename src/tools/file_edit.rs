use crate::tools::{Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use anyhow::Result;
use std::path::PathBuf;

pub struct FileEditTool;

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing a unique string with a new string. Fails if old_string not found or found multiple times."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit (relative to working_dir or absolute)"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact string to find and replace (must be unique in the file)"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement string"
                }
            },
            "required": ["path", "old_string", "new_string"]
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

        let old_string = match args["old_string"].as_str() {
            Some(s) => s.to_string(),
            None => {
                return Ok(ToolResult {
                    output: "Error: 'old_string' parameter is required".to_string(),
                    is_error: true,
                });
            }
        };

        let new_string = match args["new_string"].as_str() {
            Some(s) => s.to_string(),
            None => {
                return Ok(ToolResult {
                    output: "Error: 'new_string' parameter is required".to_string(),
                    is_error: true,
                });
            }
        };

        let path = resolve_path(&path_str, &ctx.working_dir);

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    output: format!("Error: failed to read file '{}': {}", path_str, e),
                    is_error: true,
                });
            }
        };

        let count = content.matches(old_string.as_str()).count();

        if count == 0 {
            return Ok(ToolResult {
                output: format!("Error: old_string not found in file '{}'", path_str),
                is_error: true,
            });
        }

        if count > 1 {
            return Ok(ToolResult {
                output: format!(
                    "Error: old_string found {} times in '{}', must be unique",
                    count, path_str
                ),
                is_error: true,
            });
        }

        let new_content = content.replacen(old_string.as_str(), &new_string, 1);

        if let Err(e) = std::fs::write(&path, &new_content) {
            return Ok(ToolResult {
                output: format!("Error: failed to write file '{}': {}", path_str, e),
                is_error: true,
            });
        }

        Ok(ToolResult {
            output: format!("Successfully edited '{}'", path_str),
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
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            working_dir: dir.to_path_buf(),
            sandbox_enabled: false,
        }
    }

    #[tokio::test]
    async fn test_file_edit_success() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "hello world").unwrap();
        writeln!(f, "foo bar").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let ctx = make_ctx(f.path().parent().unwrap());

        let tool = FileEditTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "path": path,
                    "old_string": "foo bar",
                    "new_string": "baz qux"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "Expected success, got: {}", result.output);
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(content.contains("baz qux"));
        assert!(!content.contains("foo bar"));
    }

    #[tokio::test]
    async fn test_file_edit_not_found() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "hello world").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let ctx = make_ctx(f.path().parent().unwrap());

        let tool = FileEditTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "path": path,
                    "old_string": "this does not exist",
                    "new_string": "something"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }

    #[tokio::test]
    async fn test_file_edit_multiple_matches() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "duplicate").unwrap();
        writeln!(f, "duplicate").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let ctx = make_ctx(f.path().parent().unwrap());

        let tool = FileEditTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "path": path,
                    "old_string": "duplicate",
                    "new_string": "unique"
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("times"));
    }
}
