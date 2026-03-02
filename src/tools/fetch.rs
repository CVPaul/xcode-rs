use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::time::Duration;

pub struct FetchTool;

#[async_trait]
impl Tool for FetchTool {
    fn name(&self) -> &str {
        "fetch"
    }

    fn description(&self) -> &str {
        "Fetch content from a URL and return it as text. Supports format options: 'text' (raw), 'markdown' (HTML converted to markdown, default), 'html' (raw HTML)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                },
                "format": {
                    "type": "string",
                    "description": "Output format: 'text', 'markdown' (default), 'html'"
                },
                "max_length": {
                    "type": "integer",
                    "description": "Maximum characters to return (default: 50000)"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        let url = match args["url"].as_str() {
            Some(u) => u,
            None => {
                return Ok(ToolResult {
                    output: "Missing required argument: url".to_string(),
                    is_error: true,
                });
            }
        };
        let format = args["format"].as_str().unwrap_or("markdown");
        let mut max_length = args["max_length"].as_u64().unwrap_or(50000) as usize;
        if ctx.compact_mode {
            max_length = max_length.min(10000);
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("xcodeai/2.0 (autonomous coding agent)")
            .build()?;
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult {
                    output: format!("Network error: {}", e),
                    is_error: true,
                });
            }
        };
        if !resp.status().is_success() {
            return Ok(ToolResult {
                output: format!("HTTP error: {}", resp.status()),
                is_error: true,
            });
        }
        let content_type = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolResult {
                    output: format!("Failed to read response body: {}", e),
                    is_error: true,
                });
            }
        };
        let mut output = match format {
            "text" => {
                if content_type.contains("html") {
                    strip_html_tags(&body)
                } else {
                    body
                }
            }
            "html" => body,
            _ => {
                if content_type.contains("html") {
                    html_to_markdown(&body)
                } else {
                    body
                }
            }
        };
        if output.len() > max_length {
            output.truncate(max_length);
        }
        Ok(ToolResult {
            output,
            is_error: false,
        })

    }
}


fn strip_html_tags(html: &str) -> String {
    let re = regex::Regex::new(r"<[^>]+>").unwrap();
    re.replace_all(html, "").to_string()
}

fn html_to_markdown(html: &str) -> String {
    htmd::convert(html).unwrap_or_else(|_| strip_html_tags(html))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;
    use serde_json::json;

    fn ctx() -> ToolContext {
        use std::sync::Arc;
        use crate::io::NullIO;
        use crate::llm::NullLlmProvider;
        use crate::tools::ToolRegistry;
        ToolContext {
            working_dir: std::env::current_dir().unwrap(),
            sandbox_enabled: false,
            io: Arc::new(NullIO),
            compact_mode: false,
            lsp_client: Arc::new(tokio::sync::Mutex::new(None)),
            mcp_client: None,
            nesting_depth: 0,
            llm: Arc::new(NullLlmProvider),
            tools: Arc::new(ToolRegistry::new()),
        }
    }

    #[tokio::test]
    async fn test_fetch_missing_url() {
        let tool = FetchTool;
        let args = json!({});
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Missing required argument"));
    }

    #[tokio::test]
    async fn test_fetch_invalid_url() {
        let tool = FetchTool;
        let args = json!({"url": "http://invalid.invalid"});
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Network error") || result.output.contains("HTTP error"));
    }

    #[tokio::test]
    async fn test_fetch_success() {
        // Test the HTML-to-markdown conversion logic directly
        // (reqwest doesn't support file:// URLs, and real HTTP requires network)
        let html = "<h1>Hello World</h1><p>Some text</p>";
        let md = super::html_to_markdown(html);
        assert!(md.contains("Hello World"));
        assert!(md.contains("Some text"));

        let stripped = super::strip_html_tags(html);
        assert!(stripped.contains("Hello World"));
        assert!(!stripped.contains("<h1>"));
    }
}
