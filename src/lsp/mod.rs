// `LspClient` is used by the three LSP tools in `src/tools/lsp_diagnostics.rs`,
// `src/tools/lsp_goto_def.rs`, and `src/tools/lsp_references.rs`.

/// LSP (Language Server Protocol) client.
///
/// This module manages the lifecycle of an LSP server subprocess and
/// provides request/response communication via JSON-RPC 2.0 framing
/// (see `transport.rs`).
///
/// # Lifecycle
///
/// 1. `LspClient::start()` — spawn the server process, grab its stdin/stdout
/// 2. `LspClient::initialize()` — send the `initialize` request + `initialized` notification
/// 3. (use send_request / send_notification for LSP features)
/// 4. `LspClient::shutdown()` — send `shutdown` request + `exit` notification, kill process
///
/// # Error Handling
///
/// All methods return `anyhow::Result`. If the LSP server crashes or returns
/// a protocol error, the error bubbles up to the caller. The caller (tool
/// implementation) is responsible for deciding whether to retry or give up.
pub mod transport;

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tracing::{debug, info};

use transport::{encode_message, read_message};

// ─── LspClient ────────────────────────────────────────────────────────────────

/// An active connection to an LSP server subprocess.
///
/// The struct owns the child process and its I/O handles.  Drop it to kill
/// the server (prefer calling `shutdown()` first for a clean exit).
pub struct LspClient {
    /// The child process — held so we can `kill()` it on drop if needed.
    /// Marked allow(dead_code) because rustc can't see it's used in shutdown().
    #[allow(dead_code)]
    process: Child,
    /// Writable end of the server's stdin pipe.
    stdin: ChildStdin,
    /// Buffered readable end of the server's stdout pipe.
    stdout: BufReader<ChildStdout>,
    /// Auto-incrementing request ID counter.
    /// JSON-RPC ids must be unique per connection; we just use 1, 2, 3, …
    next_id: AtomicU32,
}

impl LspClient {
    // ── Constructor ───────────────────────────────────────────────────────────

    /// Spawn an LSP server and return a client connected to it.
    ///
    /// # Arguments
    ///
    /// * `server_cmd` — the executable to run (e.g. `"rust-analyzer"`)
    /// * `args`       — additional CLI arguments
    /// * `project_root` — set as the working directory of the server process
    ///
    /// The server's stderr is inherited (so its log messages appear in xcodeai's
    /// terminal output) which makes debugging server issues much easier.
    pub async fn start(server_cmd: &str, args: &[&str], project_root: &Path) -> Result<Self> {
        info!(
            "Starting LSP server: {} {:?} in {:?}",
            server_cmd, args, project_root
        );

        let mut command = tokio::process::Command::new(server_cmd);
        command
            .args(args)
            .current_dir(project_root)
            // stdin/stdout are the JSON-RPC channel
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // stderr goes to the terminal so we can see server logs
            .stderr(std::process::Stdio::inherit());

        let mut child = command.spawn().with_context(|| {
            format!(
                "Failed to spawn LSP server '{}'. Is it installed?",
                server_cmd
            )
        })?;

        // Take ownership of the I/O pipes.  These Options are always Some
        // immediately after spawn when we asked for Stdio::piped().
        let stdin = child
            .stdin
            .take()
            .context("LSP server stdin was not piped")?;
        let stdout_raw = child
            .stdout
            .take()
            .context("LSP server stdout was not piped")?;
        let stdout = BufReader::new(stdout_raw);

        Ok(LspClient {
            process: child,
            stdin,
            stdout,
            next_id: AtomicU32::new(1),
        })
    }

    // ── Request / notification ─────────────────────────────────────────────

    /// Send a JSON-RPC request and wait for the matching response.
    ///
    /// The request is given a unique integer id.  This function waits for
    /// exactly ONE message from the server that has the same id in its
    /// `"id"` field.  Any notification messages that arrive first are
    /// silently discarded (this is intentional — proper notification
    /// handling is the responsibility of the LSP tool layer, not here).
    ///
    /// Returns the `"result"` field of the response, or propagates a JSON-RPC
    /// error if the `"error"` field is present.
    pub async fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        debug!("LSP → {method} (id={id})");
        self.write_message(&message).await?;

        // Read responses until we find the one matching our id.
        // We discard notifications (messages without "id") that arrive first.
        loop {
            let response = read_message(&mut self.stdout)
                .await
                .with_context(|| format!("Failed to read LSP response for method '{}'", method))?;

            // Check if this response's id matches ours.
            // The id can be a number or a string in JSON-RPC; we compare as
            // JSON values so both work.
            match response.get("id") {
                Some(resp_id) if *resp_id == json!(id) => {
                    debug!("LSP ← {method} (id={id})");

                    // If the server sent an error, surface it as an anyhow error.
                    if let Some(err) = response.get("error") {
                        bail!("LSP server returned error for '{}': {}", method, err);
                    }

                    // Return the result (may be null for void responses)
                    return Ok(response.get("result").cloned().unwrap_or(Value::Null));
                }
                // A notification (no "id") or a response for a different request —
                // discard and keep reading.
                _ => {
                    debug!("LSP: discarding unmatched message: {}", response);
                }
            }
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    ///
    /// Notifications are fire-and-forget: we write the message and return
    /// immediately without waiting for any reply.
    pub async fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        debug!("LSP → {method} (notification)");
        self.write_message(&message).await
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────────

    /// Perform the LSP `initialize` handshake.
    ///
    /// This MUST be called before any other LSP requests.  The handshake
    /// consists of:
    /// 1. An `initialize` request (client sends capabilities, server replies
    ///    with its own capabilities).
    /// 2. An `initialized` notification (client confirms it's ready).
    ///
    /// We send a minimal capabilities object — just enough for the tools we
    /// implement (diagnostics, goto-definition, find-references).
    pub async fn initialize(&mut self) -> Result<()> {
        info!("LSP: initializing");

        // The `rootUri` tells the server which project to index.  We use the
        // server's working directory (set in `start()`), converted to a file URI.
        // For simplicity we pass `null` here; most servers default to cwd.
        let _result = self
            .send_request(
                "initialize",
                json!({
                    // LSP process id — used for server to auto-exit if client dies.
                    "processId": std::process::id(),
                    // Minimal client info
                    "clientInfo": {
                        "name": "xcodeai",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    // null rootUri means "use current working directory"
                    "rootUri": null,
                    // Capabilities we advertise to the server.
                    // We only claim what we actually use to keep this simple.
                    "capabilities": {
                        "textDocument": {
                            "synchronization": {
                                "didOpen": true,
                                "didClose": true,
                            },
                            "publishDiagnostics": {
                                "relatedInformation": false,
                            },
                        },
                    },
                }),
            )
            .await?;

        // After receiving the `initialize` response, we MUST send `initialized`
        // notification to complete the handshake.  Only then will the server
        // start processing other requests.
        self.send_notification("initialized", json!({})).await?;

        info!("LSP: initialized successfully");
        Ok(())
    }

    /// Cleanly shut down the LSP server.
    ///
    /// Sends the `shutdown` request (server acknowledges), then the `exit`
    /// notification (server process exits), then kills the process if it
    /// hasn't exited within a short grace period.
    #[allow(dead_code)]
    pub async fn shutdown(&mut self) -> Result<()> {
        info!("LSP: shutting down");

        // `shutdown` is a proper request — server must reply before exit
        let _ = self.send_request("shutdown", json!(null)).await;

        // `exit` is a notification — server should exit its process now
        let _ = self.send_notification("exit", json!(null)).await;

        // Give the server up to 2 seconds to exit cleanly, then kill it
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), self.process.wait()).await;

        // Force-kill in case it didn't exit
        let _ = self.process.kill().await;

        info!("LSP: shutdown complete");
        Ok(())
    }

    // ── Auto-detection ────────────────────────────────────────────────────────

    /// Detect the appropriate LSP server for a project directory.
    ///
    /// Inspects the project root for language-specific marker files:
    /// - `Cargo.toml`       → `rust-analyzer`
    /// - `package.json`     → `typescript-language-server --stdio`
    /// - `pyproject.toml`   → `pylsp`
    /// - `setup.py`         → `pylsp`
    ///
    /// Returns `Some((command, args))` if a server was detected, or `None`
    /// if no supported language was found in the directory.
    ///
    /// Note: This does NOT check whether the server binary is installed.
    /// Callers should handle `LspClient::start()` errors gracefully.
    pub fn detect_server(project_root: &Path) -> Option<(String, Vec<String>)> {
        // rust-analyzer: Rust projects
        if project_root.join("Cargo.toml").exists() {
            return Some(("rust-analyzer".to_string(), vec![]));
        }

        // typescript-language-server: Node.js / TypeScript projects
        if project_root.join("package.json").exists() {
            return Some((
                "typescript-language-server".to_string(),
                vec!["--stdio".to_string()],
            ));
        }

        // pylsp (python-lsp-server): Python projects
        if project_root.join("pyproject.toml").exists() || project_root.join("setup.py").exists() {
            return Some(("pylsp".to_string(), vec![]));
        }

        // pyright: also common for Python — check for pyrightconfig.json
        if project_root.join("pyrightconfig.json").exists() {
            return Some(("pyright".to_string(), vec!["--stdio".to_string()]));
        }

        None
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Encode a JSON value as an LSP message and write it to the server's stdin.
    async fn write_message(&mut self, value: &Value) -> Result<()> {
        let bytes = encode_message(value);
        self.stdin
            .write_all(&bytes)
            .await
            .context("Failed to write to LSP server stdin")?;
        // Flush so the server sees the message immediately
        self.stdin
            .flush()
            .await
            .context("Failed to flush LSP server stdin")?;
        Ok(())
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// detect_server uses file presence — test with a real temp dir.
    #[test]
    fn test_detect_rust_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

        let result = LspClient::detect_server(dir.path());
        assert!(result.is_some());
        let (cmd, args) = result.unwrap();
        assert_eq!(cmd, "rust-analyzer");
        assert!(args.is_empty());
    }

    #[test]
    fn test_detect_typescript_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();

        let result = LspClient::detect_server(dir.path());
        assert!(result.is_some());
        let (cmd, _args) = result.unwrap();
        assert_eq!(cmd, "typescript-language-server");
    }

    #[test]
    fn test_detect_python_project_pyproject() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").unwrap();

        let result = LspClient::detect_server(dir.path());
        assert!(result.is_some());
        let (cmd, _args) = result.unwrap();
        assert_eq!(cmd, "pylsp");
    }

    #[test]
    fn test_detect_python_project_setup_py() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("setup.py"), "").unwrap();

        let result = LspClient::detect_server(dir.path());
        assert!(result.is_some());
        let (cmd, _args) = result.unwrap();
        assert_eq!(cmd, "pylsp");
    }

    #[test]
    fn test_detect_no_project() {
        // Empty directory — no language marker files
        let dir = tempfile::tempdir().unwrap();
        let result = LspClient::detect_server(dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_prefers_rust_over_python_if_both() {
        // If somehow both Cargo.toml and pyproject.toml exist, prefer Rust
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").unwrap();

        let result = LspClient::detect_server(dir.path());
        let (cmd, _) = result.unwrap();
        assert_eq!(cmd, "rust-analyzer", "Rust should be preferred");
    }
}
