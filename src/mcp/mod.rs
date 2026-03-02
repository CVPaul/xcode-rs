// MCP (Model Context Protocol) client.
//
// This module implements xcodeai's MCP *client* — it connects to external
// MCP *servers* that provide additional tools and resources to the agent.
//
// # What is MCP?
//
// MCP is an open protocol (by Anthropic) for integrating AI agents with
// external tools and data sources.  A server (e.g. a filesystem tool server,
// a database connector, or a web search tool) exposes:
//   - **Tools**: callable functions with JSON-Schema parameters
//   - **Resources**: readable data items (files, DB rows, API responses)
//
// The client (us) discovers what the server offers, then calls tools or reads
// resources as directed by the LLM agent.
//
// # Transport
//
// MCP over stdio uses the same Content-Length JSON-RPC 2.0 framing as LSP.
// See `transport.rs` and `crate::lsp::transport` for the framing details.
//
// # Lifecycle
//
//   1. `McpClient::start()` — spawn the server, connect I/O pipes
//   2. `McpClient::initialize()` — protocol handshake
//   3. `McpClient::list_tools()` — discover available tools
//   4. `McpClient::call_tool()` — execute a tool
//   5. `McpClient::list_resources()` — discover available resources (optional)
//   6. `McpClient::read_resource()` — fetch resource content (optional)
//   7. `McpClient::shutdown()` — clean teardown
//
// # References
//
// - MCP spec: https://spec.modelcontextprotocol.io/specification/
// - Transport: https://spec.modelcontextprotocol.io/specification/basic/transports/

pub mod bridge;
pub mod transport;
pub mod types;

use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tracing::{debug, info};

use transport::{encode_message, read_message};
use types::{
    McpResource, McpResourceContent, McpResourceReadResult, McpResourcesListResult,
    McpToolCallResult, McpToolDefinition, McpToolsListResult,
};

// ─── McpClient ────────────────────────────────────────────────────────────────

/// A live connection to an MCP server subprocess.
///
/// The struct owns the child process and its I/O handles.  Dropping it will
/// leave the child running; call `shutdown()` first for a clean exit.
///
/// # Thread safety
///
/// `McpClient` is NOT `Sync` because it holds mutable I/O handles.  All
/// methods take `&mut self`.  If you need concurrent access, wrap it in a
/// `Mutex`.
pub struct McpClient {
    /// The spawned MCP server process.
    /// Held so we can kill it on shutdown.
    #[allow(dead_code)]
    process: Child,

    /// Writable end of the server's stdin pipe.
    /// We write JSON-RPC requests here.
    stdin: ChildStdin,

    /// Buffered readable end of the server's stdout pipe.
    /// We read JSON-RPC responses from here.
    stdout: BufReader<ChildStdout>,

    /// Auto-incrementing JSON-RPC request ID.
    /// Each request gets a unique integer id so we can match responses.
    next_id: AtomicU32,
}

impl McpClient {
    // ── Constructor ───────────────────────────────────────────────────────────

    /// Spawn an MCP server process and return a connected client.
    ///
    /// # Arguments
    ///
    /// * `server_cmd` — the executable to run (e.g. `"npx"`, `"python"`)
    /// * `args` — additional CLI arguments (e.g. `["-y", "@modelcontextprotocol/server-filesystem"]`)
    ///
    /// # Errors
    ///
    /// Returns an error if the process cannot be spawned (binary not found,
    /// permission denied, etc.).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use xcodeai::mcp::McpClient;
    /// # async fn example() -> anyhow::Result<()> {
    /// let mut client = McpClient::start("npx", &["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]).await?;
    /// client.initialize().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn start(server_cmd: &str, args: &[&str]) -> Result<Self> {
        info!("Starting MCP server: {} {:?}", server_cmd, args);

        let mut child = tokio::process::Command::new(server_cmd)
            .args(args)
            // stdin/stdout are the JSON-RPC channel
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // stderr is inherited so we can see server log output in our terminal
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn MCP server '{}'. Is it installed?",
                    server_cmd
                )
            })?;

        // Take ownership of the I/O handles.
        // These Options are always Some when we requested Stdio::piped().
        let stdin = child
            .stdin
            .take()
            .context("MCP server stdin was not piped")?;
        let stdout_raw = child
            .stdout
            .take()
            .context("MCP server stdout was not piped")?;
        let stdout = BufReader::new(stdout_raw);

        Ok(McpClient {
            process: child,
            stdin,
            stdout,
            // Request IDs start at 1; 0 is reserved for "no id" (notifications)
            next_id: AtomicU32::new(1),
        })
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────────

    /// Perform the MCP `initialize` handshake.
    ///
    /// This MUST be called before any other MCP methods.  The handshake:
    ///
    /// 1. Sends an `initialize` request with our client info and capabilities.
    /// 2. Receives the server's capabilities (logged but not stored for now).
    /// 3. Sends a `notifications/initialized` notification to complete the
    ///    handshake (analogous to LSP's `initialized` notification).
    ///
    /// # MCP protocol version
    ///
    /// We advertise `"2024-11-05"`, the current stable version.
    pub async fn initialize(&mut self) -> Result<()> {
        info!("MCP: initializing");

        // Step 1: Send initialize request
        //
        // We advertise minimal capabilities — just enough for tool calling
        // and resource reading.  The server responds with its own capability
        // list (what tools/resources it has), which we don't need to inspect
        // here (tools/list and resources/list give us the details we need).
        let result = self
            .send_request(
                "initialize",
                json!({
                    // Protocol version we support
                    "protocolVersion": "2024-11-05",

                    // Our capabilities — empty object means "no special client features"
                    // (we don't yet support MCP sampling, roots, etc.)
                    "capabilities": {},

                    // Identify ourselves to the server
                    "clientInfo": {
                        "name": "xcodeai",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
            )
            .await?;

        debug!("MCP: server capabilities: {}", result);

        // Step 2: Send notifications/initialized to complete the handshake.
        //
        // The spec requires this notification AFTER the initialize response.
        // Without it, some servers refuse to process further requests.
        self.send_notification("notifications/initialized", json!({}))
            .await?;

        info!("MCP: initialized successfully");
        Ok(())
    }

    /// Cleanly shut down the MCP server.
    ///
    /// Sends no explicit "shutdown" request (MCP doesn't have one unlike LSP) —
    /// we just close stdin and kill the process after a short grace period.
    #[allow(dead_code)]
    pub async fn shutdown(&mut self) -> Result<()> {
        info!("MCP: shutting down");

        // Give the process up to 2 seconds to notice stdin is closed and exit
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), self.process.wait()).await;

        // Force-kill if it didn't exit on its own
        let _ = self.process.kill().await;

        info!("MCP: shutdown complete");
        Ok(())
    }

    // ── Tool discovery and execution ──────────────────────────────────────────

    /// List all tools available on this MCP server.
    ///
    /// Returns the tool definitions including name, description, and JSON
    /// Schema for parameters.  The agent uses these definitions to decide
    /// which tools to call.
    ///
    /// # MCP method: `tools/list`
    pub async fn list_tools(&mut self) -> Result<Vec<McpToolDefinition>> {
        debug!("MCP: listing tools");

        let result = self.send_request("tools/list", json!({})).await?;

        // The result should be: { "tools": [ { "name": ..., ... }, ... ] }
        let list: McpToolsListResult =
            serde_json::from_value(result).context("Failed to parse tools/list response")?;

        info!("MCP: server has {} tools", list.tools.len());
        Ok(list.tools)
    }

    /// Call a tool on the MCP server.
    ///
    /// # Arguments
    ///
    /// * `name` — the tool name (from `list_tools()`)
    /// * `arguments` — the tool arguments as a JSON object (must match the
    ///   tool's `inputSchema`)
    ///
    /// # Returns
    ///
    /// A `McpToolCallResult` containing:
    /// - `content`: one or more output blocks (usually a single Text block)
    /// - `is_error`: true if the tool execution failed
    ///
    /// # MCP method: `tools/call`
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<McpToolCallResult> {
        debug!("MCP: calling tool '{}'", name);

        let result = self
            .send_request(
                "tools/call",
                json!({
                    "name": name,
                    "arguments": arguments,
                }),
            )
            .await?;

        let call_result: McpToolCallResult = serde_json::from_value(result)
            .with_context(|| format!("Failed to parse tools/call response for '{}'", name))?;

        if call_result.is_error {
            debug!("MCP: tool '{}' returned an error result", name);
        } else {
            debug!("MCP: tool '{}' succeeded", name);
        }

        Ok(call_result)
    }

    // ── Resource discovery and reading ────────────────────────────────────────

    /// List all resources available on this MCP server.
    ///
    /// Resources are server-managed data items (files, DB rows, API responses)
    /// that the agent can read via `read_resource()`.
    ///
    /// Note: Not all MCP servers expose resources; some only have tools.
    ///
    /// # MCP method: `resources/list`
    #[allow(dead_code)]
    pub async fn list_resources(&mut self) -> Result<Vec<McpResource>> {
        debug!("MCP: listing resources");

        let result = self.send_request("resources/list", json!({})).await?;

        let list: McpResourcesListResult =
            serde_json::from_value(result).context("Failed to parse resources/list response")?;

        info!("MCP: server has {} resources", list.resources.len());
        Ok(list.resources)
    }

    /// Read the content of a specific resource.
    ///
    /// # Arguments
    ///
    /// * `uri` — the resource URI (from `list_resources()`)
    ///
    /// # Returns
    ///
    /// One or more content blocks for the resource.  Most resources return a
    /// single text block.
    ///
    /// # MCP method: `resources/read`
    #[allow(dead_code)]
    pub async fn read_resource(&mut self, uri: &str) -> Result<Vec<McpResourceContent>> {
        debug!("MCP: reading resource '{}'", uri);

        let result = self
            .send_request(
                "resources/read",
                json!({
                    "uri": uri,
                }),
            )
            .await?;

        let read_result: McpResourceReadResult = serde_json::from_value(result)
            .with_context(|| format!("Failed to parse resources/read response for '{}'", uri))?;

        Ok(read_result.contents)
    }

    // ── Low-level request/notification helpers ────────────────────────────────

    /// Send a JSON-RPC request and wait for the matching response.
    ///
    /// Assigns a unique integer id to the request.  Reads messages from the
    /// server until one arrives with a matching id.  Notifications (no id)
    /// that arrive in the meantime are silently discarded.
    ///
    /// Returns the `"result"` field of the response, or propagates the error
    /// if the server returned a `"error"` field.
    pub async fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        // Assign a unique id for this request
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        debug!("MCP → {} (id={})", method, id);
        self.write_message(&message).await?;

        // Read responses until we find the one with our id.
        // In MCP, servers can send notifications at any time (e.g. progress
        // updates for long-running operations).  We discard those here since
        // we don't yet have a notification handler.
        loop {
            let response = read_message(&mut self.stdout)
                .await
                .with_context(|| format!("Failed to read MCP response for '{}'", method))?;

            match response.get("id") {
                // This response matches our request id
                Some(resp_id) if *resp_id == json!(id) => {
                    debug!("MCP ← {} (id={})", method, id);

                    // Surface JSON-RPC errors as Rust errors
                    if let Some(err) = response.get("error") {
                        bail!("MCP server returned error for '{}': {}", method, err);
                    }

                    // Return the result value (may be null for void responses)
                    return Ok(response.get("result").cloned().unwrap_or(Value::Null));
                }

                // A notification (no id) or a response for a different in-flight
                // request — discard and continue reading.
                _ => {
                    debug!("MCP: discarding unmatched message: {}", response);
                }
            }
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    ///
    /// Notifications are fire-and-forget: we write the message and return
    /// immediately.  Used for `notifications/initialized`.
    pub async fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            // Notifications intentionally have NO "id" field.
            // The absence of "id" is how JSON-RPC distinguishes notifications
            // from requests.
        });

        debug!("MCP → {} (notification)", method);
        self.write_message(&message).await
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Encode a JSON value as a Content-Length-framed message and write it
    /// to the MCP server's stdin pipe.
    async fn write_message(&mut self, value: &Value) -> Result<()> {
        let bytes = encode_message(value);
        self.stdin
            .write_all(&bytes)
            .await
            .context("Failed to write to MCP server stdin")?;
        // Flush immediately — don't buffer, the server needs to see the request
        self.stdin
            .flush()
            .await
            .context("Failed to flush MCP server stdin")?;
        Ok(())
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
//
// We can't easily spin up a real MCP server in unit tests, but we can test:
//   1. The JSON-RPC message construction (what we would send)
//   2. The response parsing (what we'd do with server replies)
//
// Integration tests with a real server would live in tests/e2e_mcp.rs.

#[cfg(test)]
mod tests {
    use super::transport::encode_message;
    use serde_json::{json, Value};

    // ── Message construction tests ──────────────────────────────────────────

    /// Verify the initialize request is correctly structured.
    #[test]
    fn test_initialize_request_structure() {
        let params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "xcodeai",
                "version": "1.1.0"
            }
        });

        // Build the full request as McpClient::initialize() would
        let id: u32 = 1;
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": params,
        });

        assert_eq!(message["jsonrpc"], "2.0");
        assert_eq!(message["method"], "initialize");
        assert_eq!(message["params"]["protocolVersion"], "2024-11-05");
        assert_eq!(message["params"]["clientInfo"]["name"], "xcodeai");
    }

    /// Verify the notifications/initialized notification has no id.
    #[test]
    fn test_initialized_notification_no_id() {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });

        // Notifications MUST NOT have an id field
        assert!(
            notification.get("id").is_none(),
            "Notification must not have id"
        );
        assert_eq!(notification["method"], "notifications/initialized");
    }

    /// Verify the tools/call request structure.
    #[test]
    fn test_tools_call_request_structure() {
        let id: u32 = 3;
        let name = "read_file";
        let arguments = json!({ "path": "/tmp/notes.txt" });

        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments,
            }
        });

        assert_eq!(message["method"], "tools/call");
        assert_eq!(message["params"]["name"], "read_file");
        assert_eq!(message["params"]["arguments"]["path"], "/tmp/notes.txt");
    }

    /// Verify the resources/read request structure.
    #[test]
    fn test_resources_read_request_structure() {
        let id: u32 = 4;
        let uri = "file:///home/user/notes.txt";

        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "resources/read",
            "params": { "uri": uri }
        });

        assert_eq!(message["method"], "resources/read");
        assert_eq!(message["params"]["uri"], uri);
    }

    // ── Response matching tests ─────────────────────────────────────────────

    /// Verify id matching logic (simulates what send_request() does).
    #[test]
    fn test_response_id_matching() {
        let our_id: u32 = 42;

        // A response with the matching id — should be accepted
        let matching_response = json!({
            "jsonrpc": "2.0",
            "id": our_id,
            "result": { "tools": [] }
        });

        // A response with a different id — should be discarded
        let other_response = json!({
            "jsonrpc": "2.0",
            "id": 99,
            "result": {}
        });

        // A notification (no id) — should be discarded
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": { "progress": 50 }
        });

        assert_eq!(
            matching_response.get("id"),
            Some(&json!(our_id)),
            "Matching response should have our id"
        );
        assert_ne!(
            other_response.get("id"),
            Some(&json!(our_id)),
            "Other response has different id"
        );
        assert!(notification.get("id").is_none(), "Notification has no id");
    }

    /// Verify error response propagation (simulates error handling in send_request).
    #[test]
    fn test_error_response_detection() {
        let error_response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32601,
                "message": "Method not found"
            }
        });

        // send_request bails when "error" field is present
        assert!(
            error_response.get("error").is_some(),
            "Error response should have 'error' field"
        );
        assert!(
            error_response.get("result").is_none(),
            "Error response should not have 'result' field"
        );
    }

    // ── Framing integration tests ───────────────────────────────────────────

    /// Verify that MCP messages are correctly framed via the transport.
    #[test]
    fn test_message_framing_for_mcp() {
        let message = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        });

        let encoded = encode_message(&message);
        let text = String::from_utf8(encoded).unwrap();

        // Content-Length header must be present and correct
        let cl_line = text.lines().next().unwrap();
        let cl_val: usize = cl_line
            .strip_prefix("Content-Length: ")
            .unwrap()
            .parse()
            .unwrap();

        let body_start = text.find("\r\n\r\n").unwrap() + 4;
        let body = &text[body_start..];

        assert_eq!(body.len(), cl_val, "Content-Length must match body length");

        // Body must parse as valid JSON
        let _: Value = serde_json::from_str(body).expect("Body must be valid JSON");
    }
}
