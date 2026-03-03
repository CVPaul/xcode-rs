use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

pub struct DisplayImageTool;

#[async_trait]
impl Tool for DisplayImageTool {
    fn name(&self) -> &str {
        "display_image"
    }

    fn description(&self) -> &str {
        "Display an image directly in the terminal. Supports PNG, JPG, GIF, BMP, WebP and more. \
         The image is rendered inline using the best available terminal graphics protocol \
         (Kitty, iTerm2, Sixel, or Unicode fallback). Use this whenever you generate a chart, \
         diagram, screenshot, or any visual output that the user should see."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the image file (relative to working_dir or absolute)"
                },
                "width": {
                    "type": "integer",
                    "description": "Max display width in terminal columns (default: 80)"
                },
                "height": {
                    "type": "integer",
                    "description": "Max display height in terminal rows (default: 25)"
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

        let width = args["width"].as_u64().map(|w| w as u32);
        let height = args["height"].as_u64().map(|h| h as u32);

        let path = resolve_path(&path_str, &ctx.working_dir);

        if !path.exists() {
            return Ok(ToolResult {
                output: format!("Error: file not found: {}", path_str),
                is_error: true,
            });
        }

        // Check that it looks like an image file
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let image_exts = [
            "png", "jpg", "jpeg", "gif", "bmp", "webp", "tiff", "tif", "ico", "svg",
        ];
        if !image_exts.contains(&ext.as_str()) {
            return Ok(ToolResult {
                output: format!(
                    "Warning: '{}' may not be an image file (extension: .{}). Attempting display anyway.",
                    path_str, ext
                ),
                is_error: false,
            });
        }

        let conf = viuer::Config {
            width,
            height,
            absolute_offset: false,
            ..Default::default()
        };

        match viuer::print_from_file(&path, &conf) {
            Ok((w, h)) => Ok(ToolResult {
                output: format!(
                    "Image displayed: {} ({}x{} cells)",
                    path_str, w, h
                ),
                is_error: false,
            }),
            Err(e) => Ok(ToolResult {
                output: format!("Error displaying image: {}", e),
                is_error: true,
            }),
        }
    }
}

fn resolve_path(path: &str, working_dir: &Path) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        working_dir.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ctx(dir: &Path) -> ToolContext {
        ToolContext {
            working_dir: dir.to_path_buf(),
            sandbox_enabled: false,
            io: Arc::new(crate::io::NullIO),
            compact_mode: false,
            lsp_client: Arc::new(tokio::sync::Mutex::new(None)),
            mcp_client: None,
            nesting_depth: 0,
            llm: Arc::new(crate::llm::NullLlmProvider),
            tools: Arc::new(crate::tools::ToolRegistry::new()),
            permissions: vec![],
            formatters: std::collections::HashMap::new(),
        }
    }

    #[tokio::test]
    async fn test_display_image_missing_path() {
        let tool = DisplayImageTool;
        let args = serde_json::json!({});
        let result = tool.execute(args, &ctx(Path::new("/tmp"))).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("'path' parameter is required"));
    }

    #[tokio::test]
    async fn test_display_image_file_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = DisplayImageTool;
        let args = serde_json::json!({"path": "nonexistent.png"});
        let result = tool.execute(args, &ctx(tmp.path())).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("file not found"));
    }

    #[tokio::test]
    async fn test_display_image_non_image_ext() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("data.txt");
        std::fs::write(&file, "not an image").unwrap();
        let tool = DisplayImageTool;
        let args = serde_json::json!({"path": "data.txt"});
        let result = tool.execute(args, &ctx(tmp.path())).await.unwrap();
        // Should warn but not error
        assert!(!result.is_error);
        assert!(result.output.contains("may not be an image"));
    }
}
