// MCP Resource Access Tool
//
// This tool lets the LLM agent read data from an MCP server's "resources".
//
// # What are MCP resources?
//
// Unlike MCP *tools* (which execute code), MCP *resources* are read-only data
// items the server exposes — think of them like files or database rows.  Each
// resource has a URI (e.g. `file:///home/user/notes.txt`) and can be fetched
// with `resources/read`.
//
// # When would an agent use this?
//
// 1. After the agent calls `mcp_list_resources` (a companion tool, or from
//    an MCP server that auto-publishes a resources list).
// 2. When the LLM decides it needs the content of a specific resource.
//
// # Tool name
//
// `mcp_read_resource` — consistent with our `mcp_` prefix convention used in
// `McpToolBridge`.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::mcp::types::McpResourceContent;
use crate::tools::{Tool, ToolContext, ToolResult};

// ─── McpReadResourceTool ──────────────────────────────────────────────────────

/// A built-in tool that reads a resource from the connected MCP server.
///
/// Exposes the MCP `resources/read` method to the LLM as a normal tool.
/// The agent calls it with a `uri` argument (obtained from listing resources or
/// from prior knowledge), and gets back the resource's text content.
///
/// # Error handling
///
/// - If no MCP server is connected (`ctx.mcp_client == None`), returns an
///   error result explaining how to connect.
/// - If `resources/read` fails (server error, unknown URI, etc.), returns an
///   error result with the server's error message.
/// - Binary resources (no `text` field) return a placeholder message.
#[allow(dead_code)]
pub struct McpReadResourceTool;

#[async_trait]
impl Tool for McpReadResourceTool {
    fn name(&self) -> &str {
        "mcp_read_resource"
    }

    fn description(&self) -> &str {
        "Read the content of an MCP resource by URI. \
         Use after listing resources (or when you know the URI) to fetch data \
         from the connected MCP server."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "The URI of the resource to read, e.g. \
                                    'file:///home/user/notes.txt'. \
                                    Obtain this from mcp_list_resources or from \
                                    an MCP tool that returned a resource reference."
                }
            },
            "required": ["uri"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult> {
        // ── 1. Extract and validate the `uri` argument ───────────────────────
        let uri = args["uri"].as_str().unwrap_or("").trim().to_string();
        if uri.is_empty() {
            return Ok(ToolResult {
                output: "Missing required parameter 'uri'. \
                         Provide the resource URI (e.g. 'file:///path/to/file')."
                    .to_string(),
                is_error: true,
            });
        }

        // ── 2. Check that an MCP server is connected ──────────────────────────
        //
        // `ctx.mcp_client` is `None` when no MCP server has been connected for
        // this session.  Rather than crashing, we return a helpful error so the
        // LLM can inform the user.
        let client = match &ctx.mcp_client {
            Some(c) => Arc::clone(c),
            None => {
                return Ok(ToolResult {
                    output: "No MCP server is connected. \
                             Start xcodeai with an MCP server configured in config.json, \
                             or use the /connect command to attach one at runtime."
                        .to_string(),
                    is_error: true,
                });
            }
        };

        // ── 3. Call resources/read on the MCP server ──────────────────────────
        //
        // We lock the mutex for the duration of the request.  MCP over stdio
        // is strictly sequential — only one request can be in-flight at once.
        let mut locked = client.lock().await;
        match locked.read_resource(&uri).await {
            Ok(contents) => {
                let output = format_resource_contents(&contents);
                Ok(ToolResult {
                    output,
                    is_error: false,
                })
            }
            Err(e) => Ok(ToolResult {
                output: format!("Failed to read MCP resource '{}': {}", uri, e),
                is_error: true,
            }),
        }
    }
}

// ─── format_resource_contents ─────────────────────────────────────────────────

/// Convert the resource content blocks returned by `resources/read` into a
/// single string suitable for the agent to read.
///
/// MCP resources can have multiple content blocks, but in practice most have
/// just one.  We join text blocks with newlines and skip binary blobs (we
/// can't embed them in the agent's text context).
#[allow(dead_code)]
fn format_resource_contents(contents: &[McpResourceContent]) -> String {
    // Collect text from all content blocks that have text.
    let text_parts: Vec<&str> = contents.iter().filter_map(|c| c.text.as_deref()).collect();

    if text_parts.is_empty() {
        // The server returned a resource with no text content (binary only).
        // This can happen for image resources, audio files, etc.
        "(Resource has no text content — it may be a binary resource)".to_string()
    } else {
        text_parts.join("\n")
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Tool metadata ──────────────────────────────────────────────────────

    /// Tool name must match the `mcp_` prefix convention.
    #[test]
    fn test_tool_name() {
        let tool = McpReadResourceTool;
        assert_eq!(tool.name(), "mcp_read_resource");
    }

    /// Description is non-empty and mentions "uri".
    #[test]
    fn test_tool_description_mentions_uri() {
        let tool = McpReadResourceTool;
        assert!(
            tool.description().to_lowercase().contains("uri"),
            "Description should mention 'uri'"
        );
    }

    /// Parameters schema is an object with a required `uri` string field.
    #[test]
    fn test_tool_schema() {
        let tool = McpReadResourceTool;
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["uri"]["type"], "string");
        let required = &schema["required"];
        assert!(
            required
                .as_array()
                .map(|a| a.iter().any(|v| v == "uri"))
                .unwrap_or(false),
            "uri must be in the required array"
        );
    }

    // ── format_resource_contents ───────────────────────────────────────────

    /// Single text block → just that text.
    #[test]
    fn test_format_single_text() {
        let contents = vec![McpResourceContent {
            uri: "file:///notes.txt".to_string(),
            mime_type: Some("text/plain".to_string()),
            text: Some("hello world".to_string()),
            blob: None,
        }];
        let result = format_resource_contents(&contents);
        assert_eq!(result, "hello world");
    }

    /// Multiple text blocks → joined with newline.
    #[test]
    fn test_format_multiple_text_blocks() {
        let contents = vec![
            McpResourceContent {
                uri: "file:///a.txt".to_string(),
                mime_type: None,
                text: Some("line one".to_string()),
                blob: None,
            },
            McpResourceContent {
                uri: "file:///b.txt".to_string(),
                mime_type: None,
                text: Some("line two".to_string()),
                blob: None,
            },
        ];
        let result = format_resource_contents(&contents);
        assert_eq!(result, "line one\nline two");
    }

    /// No text blocks (binary only) → placeholder message.
    #[test]
    fn test_format_binary_only() {
        let contents = vec![McpResourceContent {
            uri: "file:///image.png".to_string(),
            mime_type: Some("image/png".to_string()),
            text: None,
            blob: Some("aGVsbG8=".to_string()),
        }];
        let result = format_resource_contents(&contents);
        assert!(
            result.contains("binary"),
            "Expected binary placeholder, got: {}",
            result
        );
    }

    /// Empty content slice → placeholder message.
    #[test]
    fn test_format_empty() {
        let result = format_resource_contents(&[]);
        assert!(
            result.contains("no text content"),
            "Expected no-text placeholder, got: {}",
            result
        );
    }

    // ── execute() edge cases (no live MCP needed) ─────────────────────────

    /// Empty URI returns an error result without panicking.
    #[tokio::test]
    async fn test_execute_empty_uri() {
        use crate::io::NullIO;
        use crate::tools::ToolContext;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let tool = McpReadResourceTool;
        let ctx = ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            sandbox_enabled: false,
            io: Arc::new(NullIO),
            compact_mode: false,
            lsp_client: Arc::new(Mutex::new(None)),
            mcp_client: None,
            nesting_depth: 0,
            llm: Arc::new(crate::llm::NullLlmProvider),
            tools: Arc::new(crate::tools::ToolRegistry::new()),
            permissions: vec![],
            formatters: std::collections::HashMap::new(),
        };

        let result = tool
            .execute(serde_json::json!({ "uri": "" }), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result.output.contains("Missing"),
            "Expected 'Missing' in error, got: {}",
            result.output
        );
    }

    /// When no MCP client is connected, execute() returns a helpful error.
    #[tokio::test]
    async fn test_execute_no_mcp_client() {
        use crate::io::NullIO;
        use crate::tools::ToolContext;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let tool = McpReadResourceTool;
        let ctx = ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            sandbox_enabled: false,
            io: Arc::new(NullIO),
            compact_mode: false,
            lsp_client: Arc::new(Mutex::new(None)),
            mcp_client: None,
            nesting_depth: 0,
            llm: Arc::new(crate::llm::NullLlmProvider),
            tools: Arc::new(crate::tools::ToolRegistry::new()),
            permissions: vec![],
            formatters: std::collections::HashMap::new(),
        };

        let result = tool
            .execute(serde_json::json!({ "uri": "file:///notes.txt" }), &ctx)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result.output.contains("No MCP server"),
            "Expected 'No MCP server' in error, got: {}",
            result.output
        );
    }
}
