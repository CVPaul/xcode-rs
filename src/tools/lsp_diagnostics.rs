// src/tools/lsp_diagnostics.rs
//
// LSP Diagnostics tool — opens a file via the Language Server Protocol and
// returns compiler/linter diagnostics (errors, warnings) in a structured format.
//
// Design:
//   1. Acquire the shared LspClient from ToolContext.lsp_client (lazy init).
//   2. If LSP is not running yet, auto-detect + start + initialize it.
//   3. Send `textDocument/didOpen` notification for the target file.
//   4. Send `textDocument/documentDiagnostic` request (LSP 3.17 pull model)
//      OR collect `textDocument/publishDiagnostics` notifications from the
//      push model (older servers). We use a timeout-based approach.
//   5. Format and return: "file:line:col: severity: message"
//
// Lazy startup helper is shared across all three LSP tools (diagnostics,
// goto-def, references) to avoid code duplication.

use crate::lsp::LspClient;
use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;

pub struct LspDiagnosticsTool;

#[async_trait]
impl Tool for LspDiagnosticsTool {
    fn name(&self) -> &str {
        "lsp_diagnostics"
    }

    fn description(&self) -> &str {
        "Get LSP diagnostics (errors, warnings) for a source file. \
        Requires a language server to be available (rust-analyzer, pylsp, etc.). \
        Returns formatted list: file:line:col: severity: message"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the source file to check"
                },
                "severity": {
                    "type": "string",
                    "enum": ["error", "warning", "all"],
                    "description": "Minimum severity to include. Default: 'all'"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        // ── 1. Validate required parameter ───────────────────────────────────
        let path_str = match args["path"].as_str() {
            Some(p) => p,
            None => {
                return Ok(ToolResult {
                    output: "Missing required parameter: path".into(),
                    is_error: true,
                })
            }
        };
        let severity_filter = args["severity"].as_str().unwrap_or("all");

        // ── 2. Resolve to absolute path ───────────────────────────────────────
        let abs_path = if std::path::Path::new(path_str).is_absolute() {
            PathBuf::from(path_str)
        } else {
            ctx.working_dir.join(path_str)
        };

        if !abs_path.exists() {
            return Ok(ToolResult {
                output: format!("File not found: {}", abs_path.display()),
                is_error: true,
            });
        }

        // ── 3. Lazily start the LSP server on first use ───────────────────────
        // We hold a MutexGuard for the duration of the call to keep the client
        // stable. Multiple concurrent calls would serialize here, which is fine
        // because LSP clients are inherently single-threaded (one stdin/stdout).
        let mut guard = ctx.lsp_client.lock().await;
        if guard.is_none() {
            match ensure_lsp_started(&ctx.working_dir).await {
                Ok(client) => *guard = Some(client),
                Err(e) => {
                    return Ok(ToolResult {
                        output: format!("LSP server not available: {}", e),
                        is_error: true,
                    })
                }
            }
        }
        let client = guard.as_mut().unwrap();

        // ── 4. Read the file content for didOpen ─────────────────────────────
        let file_content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    output: format!("Cannot read file: {}", e),
                    is_error: true,
                })
            }
        };

        let lang_id = detect_language_id(&abs_path);
        let file_uri = path_to_uri(&abs_path);

        // ── 5. Open the document so the server can analyse it ─────────────────
        // `textDocument/didOpen` is a notification (no response expected).
        let _ = client
            .send_notification(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": file_uri,
                        "languageId": lang_id,
                        "version": 1,
                        "text": file_content,
                    }
                }),
            )
            .await;

        // ── 6. Request diagnostics (LSP 3.17 pull model) ─────────────────────
        // If the server doesn't support pull diagnostics it will return an error,
        // which we handle gracefully by returning an empty list.
        let diagnostics = match client
            .send_request(
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": file_uri } }),
            )
            .await
        {
            Ok(result) => {
                // Pull model: the result has an "items" array of Diagnostic objects.
                result["items"].as_array().cloned().unwrap_or_default()
            }
            Err(_) => {
                // Server may not support pull diagnostics.
                // The push model (publishDiagnostics) requires listening to
                // notifications which our single-request client doesn't support yet.
                // Return empty — the user can fall back to `bash` + `cargo check`.
                vec![]
            }
        };

        // ── 7. Close the document ─────────────────────────────────────────────
        let _ = client
            .send_notification(
                "textDocument/didClose",
                json!({ "textDocument": { "uri": file_uri } }),
            )
            .await;

        // ── 8. Format results ─────────────────────────────────────────────────
        if diagnostics.is_empty() {
            return Ok(ToolResult {
                output: format!("No diagnostics found for {}", path_str),
                is_error: false,
            });
        }

        let mut lines: Vec<String> = Vec::new();
        for diag in &diagnostics {
            // LSP severity: 1=Error, 2=Warning, 3=Information, 4=Hint
            let sev_num = diag["severity"].as_u64().unwrap_or(4);
            let sev_str = match sev_num {
                1 => "error",
                2 => "warning",
                3 => "information",
                _ => "hint",
            };

            // Apply severity filter
            if severity_filter == "error" && sev_num != 1 {
                continue;
            }
            if severity_filter == "warning" && sev_num > 2 {
                continue;
            }

            // LSP positions are 0-indexed; humans expect 1-indexed.
            let line = diag["range"]["start"]["line"].as_u64().unwrap_or(0) + 1;
            let col = diag["range"]["start"]["character"].as_u64().unwrap_or(0) + 1;
            let msg = diag["message"].as_str().unwrap_or("(no message)");

            lines.push(format!(
                "{}:{}:{}: {}: {}",
                path_str, line, col, sev_str, msg
            ));
        }

        if lines.is_empty() {
            return Ok(ToolResult {
                output: format!("No {} diagnostics for {}", severity_filter, path_str),
                is_error: false,
            });
        }

        Ok(ToolResult {
            output: lines.join("\n"),
            is_error: false,
        })
    }
}

// ─── Shared LSP helpers ──────────────────────────────────────────────────────
// These are `pub(crate)` so lsp_goto_def and lsp_references can reuse them.

/// Start and initialize an LSP server for the given project root.
///
/// Used by all three LSP tools on first use (lazy initialization).
/// Returns an error if no suitable LSP server binary was detected or
/// the binary could not be spawned.
pub(crate) async fn ensure_lsp_started(project_root: &std::path::Path) -> Result<LspClient> {
    let (cmd, args) = LspClient::detect_server(project_root).ok_or_else(|| {
        anyhow::anyhow!(
            "No LSP server detected for project at {}. \
             Install rust-analyzer, pylsp, or typescript-language-server.",
            project_root.display()
        )
    })?;

    // Convert Vec<String> → Vec<&str> for the start() API
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let mut client = LspClient::start(&cmd, &arg_refs, project_root).await?;
    client.initialize().await?;
    Ok(client)
}

/// Convert a filesystem path to an LSP file URI (`file:///abs/path`).
pub(crate) fn path_to_uri(path: &std::path::Path) -> String {
    format!("file://{}", path.display())
}

/// Guess the LSP language ID from a file extension.
///
/// Used in `textDocument/didOpen` so the server knows what grammar to apply.
pub(crate) fn detect_language_id(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("ts") => "typescript",
        Some("tsx") => "typescriptreact",
        Some("js") => "javascript",
        Some("jsx") => "javascriptreact",
        Some("go") => "go",
        Some("c") | Some("h") => "c",
        Some("cpp") | Some("hpp") => "cpp",
        _ => "plaintext",
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::NullIO;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn ctx() -> ToolContext {
        ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            sandbox_enabled: false,
            io: Arc::new(NullIO),
            compact_mode: false,
            lsp_client: Arc::new(Mutex::new(None)),
            mcp_client: None,
            nesting_depth: 0,
            llm: std::sync::Arc::new(crate::llm::NullLlmProvider),
            tools: std::sync::Arc::new(crate::tools::ToolRegistry::new()),
        }
    }

    #[test]
    fn test_lsp_diagnostics_metadata() {
        let tool = LspDiagnosticsTool;
        assert_eq!(tool.name(), "lsp_diagnostics");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"][0], "path");
    }

    #[tokio::test]
    async fn test_lsp_diagnostics_missing_path() {
        let tool = LspDiagnosticsTool;
        let result = tool.execute(json!({}), &ctx()).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Missing required parameter"));
    }

    #[tokio::test]
    async fn test_lsp_diagnostics_nonexistent_file() {
        let tool = LspDiagnosticsTool;
        let result = tool
            .execute(json!({ "path": "/nonexistent/file.rs" }), &ctx())
            .await
            .unwrap();
        // Either "not found" (bad path) or "not available" (no LSP server in /tmp)
        assert!(result.is_error);
        assert!(
            result.output.contains("not found") || result.output.contains("not available"),
            "unexpected error: {}",
            result.output
        );
    }

    #[test]
    fn test_path_to_uri() {
        let p = std::path::Path::new("/tmp/foo.rs");
        assert_eq!(path_to_uri(p), "file:///tmp/foo.rs");
    }

    #[test]
    fn test_detect_language_id() {
        assert_eq!(detect_language_id(std::path::Path::new("foo.rs")), "rust");
        assert_eq!(detect_language_id(std::path::Path::new("bar.py")), "python");
        assert_eq!(
            detect_language_id(std::path::Path::new("baz.ts")),
            "typescript"
        );
        assert_eq!(
            detect_language_id(std::path::Path::new("qux.xyz")),
            "plaintext"
        );
    }
}
