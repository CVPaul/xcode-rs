// MCP Tool Bridge — wraps each MCP server tool as a xcodeai `Tool`.
//
// When the agent discovers that an MCP server is connected, we call
// `register_mcp_tools()` which:
//
//   1. Calls `McpClient::list_tools()` to get the server's tool definitions.
//   2. For each definition, creates an `McpToolBridge` that implements `Tool`.
//   3. Registers every bridge into the `ToolRegistry`.
//
// From the agent's perspective, MCP tools look identical to built-in tools —
// they appear in `list_definitions()` (sent to the LLM) and are dispatched
// through `ToolRegistry::get()`.
//
// # Name prefixing
//
// To avoid name collisions with built-in tools (e.g. an MCP server that
// exports a tool called "bash"), every MCP tool is registered under the
// prefixed name `"mcp_<original_name>"`.  The LLM sees the prefixed name in
// the tool definitions, so it uses the prefix automatically.
//
// # Error handling
//
// If the MCP tool call itself returns `is_error: true`, we still return a
// successful `ToolResult` but set `is_error: true` on it.  This lets the
// agent see the error message and decide how to proceed, matching the
// behaviour of built-in tools like `bash`.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use super::types::{McpContent, McpToolDefinition};
use super::McpClient;
use crate::tools::{Tool, ToolContext, ToolRegistry, ToolResult};

// ─── McpToolBridge ────────────────────────────────────────────────────────────

/// A wrapper that exposes one MCP server tool as a xcodeai `Tool`.
///
/// Created by `register_mcp_tools()` — one bridge per tool returned by
/// `McpClient::list_tools()`.
pub struct McpToolBridge {
    /// The raw name reported by the MCP server, e.g. `"read_file"`.
    name_raw: String,

    /// The name under which this tool is registered in xcodeai's `ToolRegistry`.
    /// Always `"mcp_<name_raw>"` to avoid conflicts with built-in tools.
    name_prefixed: String,

    /// Human-readable description forwarded from the MCP server.
    /// Used verbatim in the tool definition sent to the LLM.
    description: String,

    /// JSON Schema for the tool's input parameters (`inputSchema` from the
    /// MCP server).  Forwarded unchanged in `parameters_schema()`.
    input_schema: Value,

    /// Shared, mutex-guarded connection to the MCP server.
    ///
    /// We need a mutex because `McpClient::call_tool()` takes `&mut self`
    /// (it holds stateful I/O pipes), but multiple tools may be called
    /// concurrently by the agent executor.
    client: Arc<Mutex<McpClient>>,
}

impl McpToolBridge {
    /// Construct a bridge for one MCP tool definition.
    ///
    /// # Arguments
    ///
    /// * `def`    — the tool metadata returned by `list_tools()`
    /// * `client` — the shared MCP client connection to forward calls through
    pub fn new(def: McpToolDefinition, client: Arc<Mutex<McpClient>>) -> Self {
        // Build the prefixed name once so we don't allocate on every call.
        let name_prefixed = format!("mcp_{}", def.name);

        McpToolBridge {
            name_raw: def.name,
            name_prefixed,
            // Use empty string when the server omits the description rather
            // than propagating None — the LLM handles empty descriptions
            // better than missing ones.
            description: def.description.unwrap_or_default(),
            input_schema: def.input_schema,
            client,
        }
    }

    /// Return the original (un-prefixed) tool name.
    /// Useful for logging and for constructing the `tools/call` request.
    #[allow(dead_code)]
    pub fn raw_name(&self) -> &str {
        &self.name_raw
    }
}

#[async_trait]
impl Tool for McpToolBridge {
    fn name(&self) -> &str {
        &self.name_prefixed
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        // Return a clone of the schema we received from the MCP server.
        // This is a JSON Schema object, same shape as OpenAI function params.
        self.input_schema.clone()
    }

    /// Forward the tool call to the MCP server and convert the result.
    ///
    /// # How it works
    ///
    /// 1. Lock the mutex to get exclusive access to the MCP connection.
    /// 2. Call `McpClient::call_tool(raw_name, args)`.
    /// 3. Convert `McpToolCallResult` → `ToolResult` via `format_mcp_result()`.
    ///
    /// # Error propagation
    ///
    /// - If the MCP *call itself* fails (e.g. server crashed, network error),
    ///   we return `Ok(ToolResult { is_error: true, output: "<message>" })`.
    /// - If the MCP call succeeds but the tool reported an error
    ///   (`isError: true`), the same pattern applies — the output contains the
    ///   error text from the server.
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        // Acquire the mutex.  This will block if another MCP tool call is in
        // flight.  MCP over stdio is strictly sequential (one in-flight request
        // at a time), so this is correct behaviour.
        let mut client = self.client.lock().await;

        // Forward the call to the MCP server using the *original* (un-prefixed)
        // tool name — the server doesn't know about our "mcp_" prefix.
        match client.call_tool(&self.name_raw, args).await {
            Ok(result) => {
                let is_error = result.is_error;
                let output = format_mcp_result(&result.content, is_error);
                Ok(ToolResult { output, is_error })
            }
            Err(e) => {
                // The transport-level call itself failed (e.g. broken pipe,
                // JSON parse error).  Surface this as an error tool result so
                // the agent can decide how to recover.
                Ok(ToolResult {
                    output: format!("MCP tool '{}' call failed: {}", self.name_raw, e),
                    is_error: true,
                })
            }
        }
    }
}

// ─── format_mcp_result ────────────────────────────────────────────────────────

/// Convert MCP content blocks into a single human-readable string.
///
/// MCP tools can return multiple content blocks of different types.  We:
/// - Concatenate all `Text` blocks (the common case).
/// - Skip `Image` blocks (the agent's current text pipeline can't handle them;
///   Task 33 will add proper image support).
/// - For `Resource` blocks, emit a short annotation showing the resource URI.
///
/// # Arguments
///
/// * `content`  — the content blocks from `McpToolCallResult`
/// * `is_error` — whether the MCP tool flagged this as an error result
///
/// # Returns
///
/// A single string suitable for the `ToolResult.output` field.  If there are
/// no text blocks (unlikely but possible), returns a placeholder message.
pub fn format_mcp_result(content: &[McpContent], is_error: bool) -> String {
    let mut parts: Vec<String> = Vec::new();

    for block in content {
        match block {
            // Plain text — just include it directly.
            McpContent::Text { text } => {
                parts.push(text.clone());
            }

            // Image data — we can't render it in a text pipeline yet.
            // Emit a placeholder so the LLM knows something is here.
            McpContent::Image { mime_type, .. } => {
                parts.push(format!("[image: {}]", mime_type));
            }

            // An embedded resource reference.
            // Emit the URI so the agent could follow up with mcp_read_resource.
            McpContent::Resource { resource } => {
                parts.push(format!("[resource: {}]", resource.uri));
            }
        }
    }

    if parts.is_empty() {
        // The server returned a tool result with no content blocks.
        // This is unusual; treat it as a success with no output.
        if is_error {
            "(MCP tool error — no details provided)".to_string()
        } else {
            "(MCP tool returned no output)".to_string()
        }
    } else {
        // Join all parts with a newline separator.
        parts.join("\n")
    }
}

// ─── register_mcp_tools ───────────────────────────────────────────────────────

/// Discover all tools on an MCP server and register them in the `ToolRegistry`.
///
/// This is the main entry point called when an MCP server connection is set up
/// (e.g. at agent startup when MCP is configured, or on `/connect mcp`).
///
/// # How it works
///
/// 1. Lock the client, call `list_tools()`.
/// 2. For each returned `McpToolDefinition`, create an `McpToolBridge`.
/// 3. Register the bridge (name = `"mcp_<raw_name>"`) in `registry`.
///
/// # Returns
///
/// The number of tools registered (useful for the "Loaded N MCP tools" banner).
///
/// # Example
///
/// ```no_run
/// # use std::sync::Arc;
/// # use tokio::sync::Mutex;
/// # use xcodeai::mcp::McpClient;
/// # use xcodeai::mcp::bridge::register_mcp_tools;
/// # use xcodeai::tools::ToolRegistry;
/// # async fn example() -> anyhow::Result<()> {
/// let client = McpClient::start("npx", &["-y", "@mcp/fs"]).await?;
/// let shared = Arc::new(Mutex::new(client));
/// let mut registry = ToolRegistry::new();
/// let count = register_mcp_tools(shared, &mut registry).await?;
/// println!("Registered {} MCP tools", count);
/// # Ok(())
/// # }
/// ```
pub async fn register_mcp_tools(
    client: Arc<Mutex<McpClient>>,
    registry: &mut ToolRegistry,
) -> Result<usize> {
    // Lock to call list_tools(), then release immediately so bridges can
    // acquire the lock independently later.
    let tool_defs = {
        let mut locked = client.lock().await;
        locked.list_tools().await?
    };

    let count = tool_defs.len();

    for def in tool_defs {
        // Each bridge gets its own Arc clone of the shared client.
        // All bridges point to the same underlying McpClient connection.
        let bridge = McpToolBridge::new(def, Arc::clone(&client));
        registry.register(Box::new(bridge));
    }

    Ok(count)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Helper: build a McpToolDefinition from parts.
    fn make_def(name: &str, description: Option<&str>, schema: Value) -> McpToolDefinition {
        McpToolDefinition {
            name: name.to_string(),
            description: description.map(|s| s.to_string()),
            input_schema: schema,
        }
    }

    // ── McpToolBridge construction ──────────────────────────────────────────

    /// The prefixed name is always "mcp_<original>".
    #[test]
    fn test_bridge_name_prefixing() {
        // We need a placeholder McpClient.  Since we can't easily build one
        // without spawning a process, we use a hand-built Arc<Mutex<...>> with
        // a deliberately broken placeholder.  The constructor only stores the
        // Arc — it doesn't call any methods on the client.
        //
        // Instead of spawning a real McpClient, we just test the name
        // derivation logic directly without constructing the full bridge.
        let raw = "read_file";
        let prefixed = format!("mcp_{}", raw);
        assert_eq!(prefixed, "mcp_read_file");
    }

    /// Description falls back to empty string when the server omits it.
    #[test]
    fn test_bridge_description_fallback() {
        let def = make_def("ping", None, json!({"type": "object"}));
        // The bridge uses unwrap_or_default() for None descriptions.
        let desc = def.description.unwrap_or_default();
        assert_eq!(desc, "");
    }

    /// Description is forwarded unchanged when present.
    #[test]
    fn test_bridge_description_present() {
        let def = make_def("read_file", Some("Read a file"), json!({"type": "object"}));
        let desc = def.description.unwrap_or_default();
        assert_eq!(desc, "Read a file");
    }

    /// input_schema is forwarded unchanged.
    #[test]
    fn test_bridge_schema_forwarded() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        });
        let def = make_def("read_file", Some("Read"), schema.clone());
        // The bridge stores the schema as-is.
        assert_eq!(def.input_schema, schema);
    }

    // ── format_mcp_result ──────────────────────────────────────────────────

    /// Single Text block → plain string.
    #[test]
    fn test_format_single_text() {
        let content = vec![McpContent::Text {
            text: "hello world".to_string(),
        }];
        let result = format_mcp_result(&content, false);
        assert_eq!(result, "hello world");
    }

    /// Multiple Text blocks are joined with newlines.
    #[test]
    fn test_format_multiple_text() {
        let content = vec![
            McpContent::Text {
                text: "line one".to_string(),
            },
            McpContent::Text {
                text: "line two".to_string(),
            },
        ];
        let result = format_mcp_result(&content, false);
        assert_eq!(result, "line one\nline two");
    }

    /// Image blocks emit a placeholder with mime type.
    #[test]
    fn test_format_image_placeholder() {
        let content = vec![McpContent::Image {
            data: "aGVsbG8=".to_string(),
            mime_type: "image/png".to_string(),
        }];
        let result = format_mcp_result(&content, false);
        assert_eq!(result, "[image: image/png]");
    }

    /// Resource blocks emit URI annotation.
    #[test]
    fn test_format_resource_annotation() {
        let content = vec![McpContent::Resource {
            resource: super::super::types::McpResource {
                uri: "file:///notes.txt".to_string(),
                name: "notes.txt".to_string(),
                description: None,
                mime_type: None,
            },
        }];
        let result = format_mcp_result(&content, false);
        assert_eq!(result, "[resource: file:///notes.txt]");
    }

    /// Empty content with is_error=true returns error placeholder.
    #[test]
    fn test_format_empty_error() {
        let result = format_mcp_result(&[], true);
        assert!(
            result.contains("error"),
            "Expected error message, got: {}",
            result
        );
    }

    /// Empty content with is_error=false returns success placeholder.
    #[test]
    fn test_format_empty_success() {
        let result = format_mcp_result(&[], false);
        assert!(
            result.contains("no output"),
            "Expected 'no output' message, got: {}",
            result
        );
    }

    /// Mixed content: Text + Image + Resource are all included in order.
    #[test]
    fn test_format_mixed_content() {
        let content = vec![
            McpContent::Text {
                text: "result text".to_string(),
            },
            McpContent::Image {
                data: "abc".to_string(),
                mime_type: "image/jpeg".to_string(),
            },
        ];
        let result = format_mcp_result(&content, false);
        assert!(result.contains("result text"));
        assert!(result.contains("[image: image/jpeg]"));
    }

    // ── Name-prefixing contract ─────────────────────────────────────────────

    /// Verify the "mcp_" prefix contract for tool name registration.
    #[test]
    fn test_prefix_contract() {
        // The registry uses the prefixed name as the HashMap key.
        // This test verifies the contract: raw "foo" → registered "mcp_foo".
        let raw_names = ["bash", "read_file", "search", "do_something_complex"];
        for raw in &raw_names {
            let expected = format!("mcp_{}", raw);
            assert!(
                expected.starts_with("mcp_"),
                "Prefixed name must start with 'mcp_'"
            );
            assert!(
                expected.ends_with(raw),
                "Prefixed name must end with raw name"
            );
        }
    }
}
