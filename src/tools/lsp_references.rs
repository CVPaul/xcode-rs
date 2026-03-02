// src/tools/lsp_references.rs
//
// LSP Find-References tool — asks the language server for all usages of a
// symbol at a given cursor position.
//
// Uses: textDocument/references request
// Returns: "file:line:col" for each reference location found.
//
// Reuses `parse_locations` from lsp_goto_def and shared helpers from
// lsp_diagnostics to avoid code duplication.

use crate::tools::lsp_diagnostics::{detect_language_id, ensure_lsp_started, path_to_uri};
use crate::tools::lsp_goto_def::parse_locations;
use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

pub struct LspReferencesTool;

#[async_trait]
impl Tool for LspReferencesTool {
    fn name(&self) -> &str {
        "lsp_find_references"
    }

    fn description(&self) -> &str {
        "Find all usages/references to a symbol using the Language Server Protocol. \
        Provide the file path and cursor position (0-indexed). \
        Returns file:line:col for each reference location."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the source file"
                },
                "line": {
                    "type": "integer",
                    "description": "0-indexed line number of the cursor position"
                },
                "character": {
                    "type": "integer",
                    "description": "0-indexed character offset on the line"
                },
                "include_declaration": {
                    "type": "boolean",
                    "description": "Include the declaration site in results (default: true)"
                }
            },
            "required": ["path", "line", "character"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        // ── 1. Validate required parameters ──────────────────────────────────
        let path_str = match args["path"].as_str() {
            Some(p) => p,
            None => {
                return Ok(ToolResult {
                    output: "Missing required parameter: path".into(),
                    is_error: true,
                })
            }
        };
        let line = match args["line"].as_u64() {
            Some(l) => l,
            None => {
                return Ok(ToolResult {
                    output: "Missing required parameter: line".into(),
                    is_error: true,
                })
            }
        };
        let character = match args["character"].as_u64() {
            Some(c) => c,
            None => {
                return Ok(ToolResult {
                    output: "Missing required parameter: character".into(),
                    is_error: true,
                })
            }
        };
        // Whether to include the declaration site itself in the results.
        // Defaults to true, matching the LSP spec default.
        let include_declaration = args["include_declaration"].as_bool().unwrap_or(true);

        // ── 2. Resolve path ───────────────────────────────────────────────────
        let abs_path = if std::path::Path::new(path_str).is_absolute() {
            std::path::PathBuf::from(path_str)
        } else {
            ctx.working_dir.join(path_str)
        };

        if !abs_path.exists() {
            return Ok(ToolResult {
                output: format!("File not found: {}", abs_path.display()),
                is_error: true,
            });
        }

        // ── 3. Lazy LSP startup ───────────────────────────────────────────────
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

        let file_uri = path_to_uri(&abs_path);
        let lang_id = detect_language_id(&abs_path);

        // ── 4. Open the document ──────────────────────────────────────────────
        let content = std::fs::read_to_string(&abs_path).unwrap_or_default();
        let _ = client
            .send_notification(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": file_uri,
                        "languageId": lang_id,
                        "version": 1,
                        "text": content
                    }
                }),
            )
            .await;

        // ── 5. Send the references request ────────────────────────────────────
        // The `context.includeDeclaration` flag controls whether the symbol's
        // own definition site is included alongside its call sites.
        let result = client
            .send_request(
                "textDocument/references",
                json!({
                    "textDocument": { "uri": file_uri },
                    "position": { "line": line, "character": character },
                    "context": { "includeDeclaration": include_declaration }
                }),
            )
            .await;

        // ── 6. Close the document ─────────────────────────────────────────────
        let _ = client
            .send_notification(
                "textDocument/didClose",
                json!({ "textDocument": { "uri": file_uri } }),
            )
            .await;

        // ── 7. Format and return results ──────────────────────────────────────
        match result {
            Err(e) => Ok(ToolResult {
                output: format!("LSP references request failed: {}", e),
                is_error: true,
            }),
            Ok(val) => {
                let locations = parse_locations(&val);
                if locations.is_empty() {
                    Ok(ToolResult {
                        output: "No references found.".into(),
                        is_error: false,
                    })
                } else {
                    Ok(ToolResult {
                        output: format!(
                            "Found {} reference(s):\n{}",
                            locations.len(),
                            locations.join("\n")
                        ),
                        is_error: false,
                    })
                }
            }
        }
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
    fn test_lsp_references_metadata() {
        let tool = LspReferencesTool;
        assert_eq!(tool.name(), "lsp_find_references");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
        assert!(required.contains(&json!("line")));
        assert!(required.contains(&json!("character")));
        // include_declaration is optional
        assert!(!required.contains(&json!("include_declaration")));
    }

    #[tokio::test]
    async fn test_lsp_references_missing_params() {
        let tool = LspReferencesTool;
        let result = tool.execute(json!({}), &ctx()).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Missing required parameter"));
    }

    #[tokio::test]
    async fn test_lsp_references_nonexistent_file() {
        let tool = LspReferencesTool;
        let result = tool
            .execute(
                json!({ "path": "/nonexistent/file.rs", "line": 0, "character": 0 }),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_lsp_references_include_declaration_default() {
        // With no include_declaration param, should default to true (not error out)
        let tool = LspReferencesTool;
        // /nonexistent won't get to the LSP call, but we can verify it accepted the params
        let result = tool
            .execute(
                json!({ "path": "/nonexistent/file.rs", "line": 5, "character": 10 }),
                &ctx(),
            )
            .await
            .unwrap();
        // Still an error (file not found), but NOT a missing-param error
        assert!(result.is_error);
        assert!(!result.output.contains("Missing required parameter"));
    }
}
