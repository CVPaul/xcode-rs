// JSON-RPC framing used by LspClient (see lsp/mod.rs).

/// JSON-RPC 2.0 message framing for the Language Server Protocol.
///
/// LSP uses a simple HTTP-like header format over stdin/stdout pipes:
///
/// ```text
/// Content-Length: 97\r\n
/// \r\n
/// {"jsonrpc":"2.0","id":1,"method":"initialize","params":{...}}
/// ```
///
/// This module provides two functions:
///  - `encode_message` — serialize a JSON value into that framed byte sequence
///  - `read_message`   — read one frame from an async reader, parse the JSON
use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};

/// Encode a JSON value as a Content-Length-framed LSP message.
///
/// The resulting bytes can be written directly to an LSP server's stdin.
///
/// # Example
///
/// ```ignore
/// let msg = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
/// let bytes = encode_message(&msg);
/// // bytes starts with "Content-Length: ..."
/// ```
pub fn encode_message(value: &Value) -> Vec<u8> {
    // Serialize to a compact JSON string (no pretty-print — LSP servers
    // don't care, but compact is safer for streaming pipes).
    let body = value.to_string();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());

    // Pre-allocate to avoid reallocations
    let mut out = Vec::with_capacity(header.len() + body.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(body.as_bytes());
    out
}

/// Read one JSON-RPC message from an async buffered reader.
///
/// The function:
/// 1. Reads header lines until it finds the `Content-Length: N` header.
/// 2. Skips the blank line that separates headers from body.
/// 3. Reads exactly N bytes for the JSON body.
/// 4. Parses and returns the JSON value.
///
/// Any other headers (e.g., `Content-Type`) are silently ignored.
///
/// Returns an error if the stream ends prematurely or if the JSON is malformed.
pub async fn read_message<R>(reader: &mut R) -> Result<Value>
where
    R: AsyncBufReadExt + Unpin,
{
    // ── 1. Read headers ─────────────────────────────────────────────────────
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        let bytes_read = reader
            .read_line(&mut line)
            .await
            .context("Failed to read LSP header line")?;

        if bytes_read == 0 {
            bail!("LSP server closed the connection unexpectedly");
        }

        // Trim the CRLF or LF that read_line leaves on the string
        let trimmed = line.trim_end();

        if trimmed.is_empty() {
            // Blank line → end of headers, body follows
            break;
        }

        // Parse `Content-Length: <number>` (case-insensitive per HTTP spec)
        if let Some(rest) = trimmed.to_ascii_lowercase().strip_prefix("content-length:") {
            let n: usize = rest
                .trim()
                .parse()
                .context("Invalid Content-Length value in LSP header")?;
            content_length = Some(n);
        }
        // All other headers are intentionally ignored.
    }

    // ── 2. Validate we got a Content-Length ─────────────────────────────────
    let length = content_length.context("LSP message is missing Content-Length header")?;

    // ── 3. Read exactly `length` bytes for the JSON body ────────────────────
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .await
        .context("Failed to read LSP message body")?;

    // ── 4. Parse the JSON ────────────────────────────────────────────────────
    let value = serde_json::from_slice(&body).with_context(|| {
        format!(
            "Failed to parse LSP JSON body: {:?}",
            String::from_utf8_lossy(&body)
        )
    })?;

    Ok(value)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    /// Helper: build a byte stream that contains one encoded LSP message.
    fn make_stream(value: &Value) -> BufReader<std::io::Cursor<Vec<u8>>> {
        let bytes = encode_message(value);
        BufReader::new(std::io::Cursor::new(bytes))
    }

    #[test]
    fn test_encode_message_format() {
        let msg = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
        let bytes = encode_message(&msg);
        let text = String::from_utf8(bytes).unwrap();

        // Must start with Content-Length header
        assert!(
            text.starts_with("Content-Length: "),
            "Expected Content-Length header"
        );

        // Header and body must be separated by \r\n\r\n
        assert!(text.contains("\r\n\r\n"), "Expected CRLF separator");

        // The body (after the blank line) should be valid JSON
        let body_start = text.find("\r\n\r\n").unwrap() + 4;
        let body = &text[body_start..];
        let _: Value = serde_json::from_str(body).expect("Body should be valid JSON");
    }

    #[test]
    fn test_encode_content_length_matches_body() {
        let msg = serde_json::json!({"method": "test", "params": {"key": "value"}});
        let bytes = encode_message(&msg);
        let text = String::from_utf8(bytes).unwrap();

        // Parse the Content-Length value
        let cl_line = text.lines().next().unwrap();
        let cl_val: usize = cl_line
            .strip_prefix("Content-Length: ")
            .unwrap()
            .parse()
            .unwrap();

        // The body after \r\n\r\n should be exactly cl_val bytes
        let body_start = text.find("\r\n\r\n").unwrap() + 4;
        let body = &text[body_start..];
        assert_eq!(body.len(), cl_val);
    }

    #[tokio::test]
    async fn test_roundtrip_simple() {
        let original = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "textDocument/definition",
            "params": {"textDocument": {"uri": "file:///src/main.rs"}, "position": {"line": 10, "character": 5}}
        });

        let mut stream = make_stream(&original);
        let decoded = read_message(&mut stream).await.unwrap();

        assert_eq!(original, decoded);
    }

    #[tokio::test]
    async fn test_roundtrip_notification() {
        // Notifications have no "id" field
        let original = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        });

        let mut stream = make_stream(&original);
        let decoded = read_message(&mut stream).await.unwrap();

        assert_eq!(original, decoded);
    }

    #[tokio::test]
    async fn test_roundtrip_large_body() {
        // Large body to stress-test the length-framing
        let big_string: String = "x".repeat(10_000);
        let original = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"content": big_string}
        });

        let mut stream = make_stream(&original);
        let decoded = read_message(&mut stream).await.unwrap();

        assert_eq!(original, decoded);
    }

    #[tokio::test]
    async fn test_read_multiple_messages_sequentially() {
        // Two messages concatenated in the same byte stream
        let msg1 = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
        let msg2 = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"shutdown"});

        let mut bytes = encode_message(&msg1);
        bytes.extend(encode_message(&msg2));

        let mut stream = tokio::io::BufReader::new(std::io::Cursor::new(bytes));

        let decoded1 = read_message(&mut stream).await.unwrap();
        let decoded2 = read_message(&mut stream).await.unwrap();

        assert_eq!(decoded1, msg1);
        assert_eq!(decoded2, msg2);
    }

    #[tokio::test]
    async fn test_missing_content_length_returns_error() {
        // A message with no Content-Length header
        let raw = b"X-Custom-Header: foo\r\n\r\n{\"jsonrpc\":\"2.0\"}";
        let mut stream = tokio::io::BufReader::new(std::io::Cursor::new(raw.to_vec()));
        let result = read_message(&mut stream).await;
        assert!(result.is_err(), "Should fail without Content-Length");
    }
}
