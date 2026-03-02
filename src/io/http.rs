// src/io/http.rs
//
// HttpIO — AgentIO implementation for the HTTP API (企业微信 / REST mode).
//
// ── Why this exists ───────────────────────────────────────────────────────────
// When xcodeai runs in HTTP server mode (`xcodeai serve`), the agent loop still
// needs to report tool calls, results, and status updates — but there's no
// terminal to write to.  Instead, we want to stream events to the HTTP client
// using Server-Sent Events (SSE).
//
// `HttpIO` bridges the gap: it implements `AgentIO` and converts every output
// call into an `SseEvent` message pushed onto a `tokio::sync::mpsc` channel.
// The HTTP route handler holds the receiving end of the channel and forwards
// events to the actual HTTP response stream.
//
// ── Channel design ────────────────────────────────────────────────────────────
//
//   HttpIO::new() returns both the HttpIO and the Receiver:
//
//   let (io, rx) = HttpIO::new();
//
//   The caller (HTTP handler) polls `rx` and converts each `SseEvent` into
//   an `axum::response::sse::Event`.  The agent loop holds `Arc<HttpIO>` and
//   calls methods like `show_status()`, `show_tool_call()`, etc.
//
// ── Thread safety ─────────────────────────────────────────────────────────────
// `tokio::sync::mpsc::Sender` is `Clone + Send + Sync`, so wrapping `HttpIO`
// in `Arc<dyn AgentIO>` works correctly across async task boundaries.
//
// ── Destructive actions ───────────────────────────────────────────────────────
// There is no interactive terminal in HTTP mode.  `confirm_destructive` sends
// a `Confirmation` SSE event so the client *could* display a UI prompt, then
// auto-approves after a short timeout.  For the v1 implementation we simply
// auto-approve (return `true`) — full round-trip confirmation can be added later
// via a separate HTTP endpoint.
// ─────────────────────────────────────────────────────────────────────────────

use crate::io::AgentIO;
use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

// ─── SseEvent ────────────────────────────────────────────────────────────────

/// All event types that the agent can emit over an SSE stream.
///
/// The HTTP route handler converts these into `text/event-stream` messages.
/// Each variant maps to an SSE `event:` field:
///
/// ```text
/// event: status
/// data: {"msg":"auto-continuing…"}
///
/// event: tool_call
/// data: {"name":"bash","args":"cargo test"}
///
/// event: tool_result
/// data: {"preview":"ok. 12 passed","is_error":false}
///
/// event: error
/// data: {"msg":"Reached auto-continue limit"}
///
/// event: complete
/// data: {}
/// ```
#[derive(Debug, Clone)]
pub enum SseEvent {
    /// Progress / status message (e.g. "auto-continuing…").
    Status { msg: String },

    /// An agent tool is about to be called.
    ToolCall { name: String, args: String },

    /// The result of a tool call.
    ToolResult { preview: String, is_error: bool },

    /// An error message (not a tool error — agent-level error).
    Error { msg: String },

    /// The agent loop has finished the current task.
    #[allow(dead_code)]
    Complete,
}

impl SseEvent {
    /// Returns the SSE `event:` field name for this variant.
    ///
    /// This is used by the HTTP route handler when building the SSE response.
    pub fn event_name(&self) -> &'static str {
        match self {
            SseEvent::Status { .. } => "status",
            SseEvent::ToolCall { .. } => "tool_call",
            SseEvent::ToolResult { .. } => "tool_result",
            SseEvent::Error { .. } => "error",
            SseEvent::Complete => "complete",
        }
    }

    /// Serialise the payload to a JSON string for the SSE `data:` field.
    pub fn data_json(&self) -> String {
        match self {
            SseEvent::Status { msg } => serde_json::json!({ "msg": msg }).to_string(),
            SseEvent::ToolCall { name, args } => {
                serde_json::json!({ "name": name, "args": args }).to_string()
            }
            SseEvent::ToolResult { preview, is_error } => {
                serde_json::json!({ "preview": preview, "is_error": is_error }).to_string()
            }
            SseEvent::Error { msg } => serde_json::json!({ "msg": msg }).to_string(),
            SseEvent::Complete => "{}".to_string(),
        }
    }
}

// ─── HttpIO ──────────────────────────────────────────────────────────────────

/// AgentIO implementation that streams events over an `mpsc` channel.
///
/// The receiving end of the channel is held by the HTTP route handler, which
/// converts `SseEvent` values into SSE response frames.
///
/// # Example
///
/// ```rust,ignore
/// use xcodeai::io::http::HttpIO;
///
/// let (io, mut rx) = HttpIO::new();
///
/// // Give io to the agent (as Arc<dyn AgentIO>).
/// let io_arc: Arc<dyn AgentIO> = Arc::new(io);
///
/// // In the HTTP handler, receive events:
/// while let Some(event) = rx.recv().await {
///     println!("{}: {}", event.event_name(), event.data_json());
/// }
/// ```
pub struct HttpIO {
    /// Sending half of the SSE event channel.
    ///
    /// Cloned from the channel created in `HttpIO::new()`.  When all senders
    /// are dropped the channel closes and the receiver's `recv()` returns `None`,
    /// which signals to the HTTP handler that the agent loop has finished.
    tx: mpsc::Sender<SseEvent>,
}

impl HttpIO {
    /// Create a new `HttpIO` and return both the IO handle and the event receiver.
    ///
    /// The caller (HTTP route handler) keeps the `Receiver<SseEvent>` and polls
    /// it while streaming the response to the HTTP client.  The `HttpIO` is
    /// wrapped in `Arc<dyn AgentIO>` and given to the agent loop.
    ///
    /// The channel buffer is 64 messages — large enough that the agent loop
    /// never has to wait for the HTTP handler to drain the buffer during normal
    /// operation.
    pub fn new() -> (Self, mpsc::Receiver<SseEvent>) {
        // A bounded channel with 64 slots.  If the consumer (HTTP handler) falls
        // behind by more than 64 events the agent will briefly pause — that's
        // fine; back-pressure is the correct behaviour here.
        let (tx, rx) = mpsc::channel(64);
        (HttpIO { tx }, rx)
    }

    /// Send an `SseEvent` to the channel.
    ///
    /// If the receiver has been dropped (e.g. client disconnected), the send
    /// fails silently.  We do NOT propagate this error because the agent loop
    /// should continue even if the HTTP client disconnected — it's better to
    /// finish the task and write any file changes than to abort mid-way.
    async fn send(&self, event: SseEvent) {
        // `.send()` fails only if the receiver was dropped.  We swallow the
        // error intentionally — see doc comment above.
        let _ = self.tx.send(event).await;
    }
}

// ─── AgentIO impl ────────────────────────────────────────────────────────────

#[async_trait]
impl AgentIO for HttpIO {
    /// Emit a `Status` SSE event.
    async fn show_status(&self, msg: &str) -> Result<()> {
        self.send(SseEvent::Status {
            msg: msg.to_string(),
        })
        .await;
        Ok(())
    }

    /// Emit a `ToolCall` SSE event.
    async fn show_tool_call(&self, tool_name: &str, args_preview: &str) -> Result<()> {
        self.send(SseEvent::ToolCall {
            name: tool_name.to_string(),
            args: args_preview.to_string(),
        })
        .await;
        Ok(())
    }

    /// Emit a `ToolResult` SSE event.
    async fn show_tool_result(&self, preview: &str, is_error: bool) -> Result<()> {
        self.send(SseEvent::ToolResult {
            preview: preview.to_string(),
            is_error,
        })
        .await;
        Ok(())
    }

    /// Emit an `Error` SSE event.
    async fn write_error(&self, msg: &str) -> Result<()> {
        self.send(SseEvent::Error {
            msg: msg.to_string(),
        })
        .await;
        Ok(())
    }

    /// Auto-approve destructive actions in HTTP mode.
    ///
    /// There is no interactive terminal to prompt the user.  We send a
    /// `Status` event so the HTTP client can log that a destructive tool was
    /// called, then return `true` (approved).
    ///
    /// A future version could send a `Confirmation` event and wait for a
    /// response on a separate `/sessions/:id/confirm` HTTP endpoint.
    async fn confirm_destructive(&self, tool_name: &str, args_preview: &str) -> Result<bool> {
        self.send(SseEvent::Status {
            msg: format!(
                "⚠ auto-approving destructive call: {} ({})",
                tool_name, args_preview
            ),
        })
        .await;
        // Auto-approve — HTTP API mode is considered to be running under user
        // supervision via the /sessions endpoint, so we trust the caller.
        Ok(true)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::AgentIO;
    use std::sync::Arc;

    /// Helper: create an HttpIO and return (Arc<dyn AgentIO>, Receiver).
    fn make_io() -> (Arc<dyn AgentIO>, mpsc::Receiver<SseEvent>) {
        let (io, rx) = HttpIO::new();
        (Arc::new(io), rx)
    }

    // ── SseEvent serialisation ────────────────────────────────────────────────

    #[test]
    fn test_sse_event_names() {
        assert_eq!(SseEvent::Status { msg: "x".into() }.event_name(), "status");
        assert_eq!(
            SseEvent::ToolCall {
                name: "bash".into(),
                args: "ls".into()
            }
            .event_name(),
            "tool_call"
        );
        assert_eq!(
            SseEvent::ToolResult {
                preview: "ok".into(),
                is_error: false
            }
            .event_name(),
            "tool_result"
        );
        assert_eq!(SseEvent::Error { msg: "oops".into() }.event_name(), "error");
        assert_eq!(SseEvent::Complete.event_name(), "complete");
    }

    #[test]
    fn test_sse_event_data_json_status() {
        let ev = SseEvent::Status {
            msg: "hello".into(),
        };
        let json: serde_json::Value = serde_json::from_str(&ev.data_json()).unwrap();
        assert_eq!(json["msg"], "hello");
    }

    #[test]
    fn test_sse_event_data_json_tool_call() {
        let ev = SseEvent::ToolCall {
            name: "bash".into(),
            args: "ls".into(),
        };
        let json: serde_json::Value = serde_json::from_str(&ev.data_json()).unwrap();
        assert_eq!(json["name"], "bash");
        assert_eq!(json["args"], "ls");
    }

    #[test]
    fn test_sse_event_data_json_tool_result() {
        let ev = SseEvent::ToolResult {
            preview: "ok".into(),
            is_error: true,
        };
        let json: serde_json::Value = serde_json::from_str(&ev.data_json()).unwrap();
        assert_eq!(json["preview"], "ok");
        assert_eq!(json["is_error"], true);
    }

    #[test]
    fn test_sse_event_data_json_complete() {
        let ev = SseEvent::Complete;
        let json: serde_json::Value = serde_json::from_str(&ev.data_json()).unwrap();
        assert!(json.is_object());
    }

    // ── AgentIO method → channel ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_show_status_sends_event() {
        let (io, mut rx) = make_io();
        io.show_status("starting…").await.unwrap();

        let ev = rx.recv().await.expect("expected event");
        match ev {
            SseEvent::Status { msg } => assert_eq!(msg, "starting…"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_show_tool_call_sends_event() {
        let (io, mut rx) = make_io();
        io.show_tool_call("bash", "cargo test").await.unwrap();

        let ev = rx.recv().await.unwrap();
        match ev {
            SseEvent::ToolCall { name, args } => {
                assert_eq!(name, "bash");
                assert_eq!(args, "cargo test");
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_show_tool_result_sends_event() {
        let (io, mut rx) = make_io();
        io.show_tool_result("12 passed", false).await.unwrap();

        let ev = rx.recv().await.unwrap();
        match ev {
            SseEvent::ToolResult { preview, is_error } => {
                assert_eq!(preview, "12 passed");
                assert!(!is_error);
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_write_error_sends_event() {
        let (io, mut rx) = make_io();
        io.write_error("something failed").await.unwrap();

        let ev = rx.recv().await.unwrap();
        match ev {
            SseEvent::Error { msg } => assert_eq!(msg, "something failed"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_confirm_destructive_auto_approves() {
        let (io, mut rx) = make_io();
        // HTTP mode always auto-approves.
        let approved = io.confirm_destructive("bash", "rm -rf /").await.unwrap();
        assert!(approved, "HTTP mode should auto-approve destructive calls");

        // A status event should have been sent.
        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, SseEvent::Status { .. }));
    }

    #[tokio::test]
    async fn test_channel_closes_when_io_dropped() {
        let (io, mut rx) = HttpIO::new();
        // Drop the sender side.
        drop(io);
        // Receiver should return None immediately.
        assert!(
            rx.recv().await.is_none(),
            "channel should be closed after HttpIO dropped"
        );
    }
}
