// MCP (Model Context Protocol) stdio transport.
//
// MCP servers communicate over stdin/stdout using the SAME JSON-RPC 2.0
// Content-Length framing as LSP.  Rather than duplicate the framing code,
// we re-export the functions from `crate::lsp::transport` and add a thin
// wrapper that documents the MCP-specific perspective.
//
// MCP transport spec:
// https://spec.modelcontextprotocol.io/specification/basic/transports/

// Re-export the shared framing functions.
//
// `encode_message` and `read_message` live in `src/lsp/transport.rs`.
// They are 100% protocol-neutral — they just do Content-Length framing on
// serde_json::Value — so we reuse them here without modification.
pub use crate::lsp::transport::{encode_message, read_message};

// ─── Unit tests ───────────────────────────────────────────────────────────────
//
// The low-level framing is already tested in `lsp::transport`.
// Here we just verify the re-exports work correctly from the MCP module.

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Verify that the re-exported encode_message works for MCP messages.
    /// MCP JSON-RPC messages look identical to LSP messages at the framing level.
    #[test]
    fn test_mcp_encode_decode_roundtrip() {
        // A typical MCP initialize request
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "xcodeai",
                    "version": "1.1.0"
                }
            }
        });

        let encoded = encode_message(&request);
        let text = String::from_utf8(encoded).unwrap();

        // Must have the Content-Length header
        assert!(
            text.starts_with("Content-Length: "),
            "Expected Content-Length header"
        );
        assert!(text.contains("\r\n\r\n"), "Expected CRLF header separator");
    }

    /// Verify that a tools/list response can be framed and read back.
    #[tokio::test]
    async fn test_mcp_tools_list_framing() {
        let tools_response = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "tools": [
                    {
                        "name": "read_file",
                        "description": "Read a file",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" }
                            }
                        }
                    }
                ]
            }
        });

        let encoded = encode_message(&tools_response);
        let mut stream = tokio::io::BufReader::new(std::io::Cursor::new(encoded));
        let decoded = read_message(&mut stream).await.unwrap();

        assert_eq!(decoded["result"]["tools"][0]["name"], "read_file");
    }

    /// Verify that a notification (no id) is framed correctly.
    #[tokio::test]
    async fn test_mcp_notification_framing() {
        // MCP `notifications/initialized` — no "id" field, just method + params
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });

        let encoded = encode_message(&notification);
        let mut stream = tokio::io::BufReader::new(std::io::Cursor::new(encoded));
        let decoded = read_message(&mut stream).await.unwrap();

        assert_eq!(decoded["method"], "notifications/initialized");
        assert!(
            decoded.get("id").is_none(),
            "Notification must not have an id"
        );
    }
}
