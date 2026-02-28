use crate::tools::{Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use anyhow::Result;
use std::path::PathBuf;

pub struct FileReadTool;

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read a file and return its contents with line numbers. Supports offset and limit parameters."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to working_dir or absolute)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start from (1-indexed, default: 1)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return (default: all)"
                }
            },
            "required": ["path"]
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

        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        let offset = args["offset"].as_u64().unwrap_or(1).saturating_sub(1) as usize;
        let limit = args["limit"].as_u64().map(|l| l as usize).unwrap_or(total);

        let end = (offset + limit).min(total);
        let selected = &lines[offset.min(total)..end];

        let output = selected
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}: {}", offset + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(ToolResult {
            output,
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
    async fn test_file_read_existing_file() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "line one").unwrap();
        writeln!(f, "line two").unwrap();
        writeln!(f, "line three").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let ctx = make_ctx(f.path().parent().unwrap());

        let tool = FileReadTool;
        let result = tool
            .execute(serde_json::json!({ "path": path }), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("1: line one"));
        assert!(result.output.contains("2: line two"));
        assert!(result.output.contains("3: line three"));
    }

    #[tokio::test]
    async fn test_file_read_missing_file() {
        let ctx = make_ctx(std::path::Path::new("/tmp"));
        let tool = FileReadTool;
        let result = tool
            .execute(
                serde_json::json!({ "path": "/tmp/this_file_does_not_exist_xcode_test.txt" }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("Error"));
    }

    #[tokio::test]
    async fn test_file_read_with_offset_limit() {
        let mut f = NamedTempFile::new().unwrap();
        for i in 1..=10 {
            writeln!(f, "line {}", i).unwrap();
        }
        let path = f.path().to_string_lossy().to_string();
        let ctx = make_ctx(f.path().parent().unwrap());

        let tool = FileReadTool;
        let result = tool
            .execute(
                serde_json::json!({ "path": path, "offset": 3, "limit": 3 }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("3: line 3"));
        assert!(result.output.contains("4: line 4"));
        assert!(result.output.contains("5: line 5"));
        assert!(!result.output.contains("line 1\n") || result.output.starts_with("3:"));
        assert!(!result.output.contains("6: line 6"));
    }
}
