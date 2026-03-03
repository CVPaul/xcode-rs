// src/tools/lsp_goto_def.rs
//
// LSP Goto-Definition tool — asks the language server where a symbol at a
// given cursor position is defined.
//
// Uses: textDocument/definition request
// Returns: "file:line:col" for each definition location found.
//
// The `parse_locations` helper is `pub(crate)` so lsp_references can reuse it.

use crate::tools::lsp_diagnostics::{detect_language_id, ensure_lsp_started, path_to_uri};
use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

pub struct LspGotoDefTool;

#[async_trait]
impl Tool for LspGotoDefTool {
    fn name(&self) -> &str {
        "lsp_goto_definition"
    }

    fn description(&self) -> &str {
        "Find where a symbol is defined using the Language Server Protocol. \
        Provide the file path and cursor position (0-indexed line and character). \
        Returns file:line:col for each definition location."
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

        // ── 5. Send the definition request ────────────────────────────────────
        let result = client
            .send_request(
                "textDocument/definition",
                json!({
                    "textDocument": { "uri": file_uri },
                    "position": { "line": line, "character": character }
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
                output: format!("LSP definition request failed: {}", e),
                is_error: true,
            }),
            Ok(val) => {
                let locations = parse_locations(&val);
                if locations.is_empty() {
                    Ok(ToolResult {
                        output: "No definition found.".into(),
                        is_error: false,
                    })
                } else {
                    Ok(ToolResult {
                        output: locations.join("\n"),
                        is_error: false,
                    })
                }
            }
        }
    }
}

// ─── Location parser ─────────────────────────────────────────────────────────

/// Parse an LSP `Location | Location[] | LocationLink[]` value into
/// human-readable `"file:line:col"` strings (1-indexed).
///
/// `pub(crate)` so `lsp_references` can reuse it without duplication.
pub(crate) fn parse_locations(val: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();

    // The response can be a single object or an array
    let items: Vec<&serde_json::Value> = if val.is_array() {
        val.as_array().unwrap().iter().collect()
    } else if val.is_object() {
        vec![val]
    } else {
        return out;
    };

    for item in items {
        // `Location` has "uri" + "range"
        // `LocationLink` has "targetUri" + "targetSelectionRange" / "targetRange"
        let uri = item["uri"]
            .as_str()
            .or_else(|| item["targetUri"].as_str())
            .unwrap_or("?");

        let range = item
            .get("range")
            .or_else(|| item.get("targetSelectionRange"))
            .or_else(|| item.get("targetRange"));

        let (line, col) = if let Some(r) = range {
            (
                r["start"]["line"].as_u64().unwrap_or(0) + 1,
                r["start"]["character"].as_u64().unwrap_or(0) + 1,
            )
        } else {
            (1, 1)
        };

        // Strip the `file://` prefix for readability
        let path = uri.strip_prefix("file://").unwrap_or(uri);
        out.push(format!("{}:{}:{}", path, line, col));
    }

    out
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
            permissions: vec![],
            formatters: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn test_lsp_goto_def_metadata() {
        let tool = LspGotoDefTool;
        assert_eq!(tool.name(), "lsp_goto_definition");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
        assert!(required.contains(&json!("line")));
        assert!(required.contains(&json!("character")));
    }

    #[tokio::test]
    async fn test_lsp_goto_def_missing_params() {
        let tool = LspGotoDefTool;
        let result = tool.execute(json!({}), &ctx()).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Missing required parameter"));
    }

    #[tokio::test]
    async fn test_lsp_goto_def_nonexistent_file() {
        let tool = LspGotoDefTool;
        let result = tool
            .execute(
                json!({ "path": "/nonexistent/file.rs", "line": 0, "character": 0 }),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(result.is_error);
    }

    // ── parse_locations unit tests (no network needed) ────────────────────

    #[test]
    fn test_parse_locations_null() {
        // Server may return JSON null when no definition is found
        let locs = parse_locations(&serde_json::Value::Null);
        assert!(locs.is_empty());
    }

    #[test]
    fn test_parse_locations_single_location() {
        let val = json!({
            "uri": "file:///tmp/foo.rs",
            "range": {
                "start": { "line": 9, "character": 3 },
                "end":   { "line": 9, "character": 7 }
            }
        });
        let locs = parse_locations(&val);
        assert_eq!(locs, vec!["/tmp/foo.rs:10:4"]);
    }

    #[test]
    fn test_parse_locations_array() {
        let val = json!([
            {
                "uri": "file:///a.rs",
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end":   { "line": 0, "character": 1 }
                }
            },
            {
                "uri": "file:///b.rs",
                "range": {
                    "start": { "line": 4, "character": 2 },
                    "end":   { "line": 4, "character": 5 }
                }
            }
        ]);
        let locs = parse_locations(&val);
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[0], "/a.rs:1:1");
        assert_eq!(locs[1], "/b.rs:5:3");
    }

    #[test]
    fn test_parse_locations_location_link() {
        // LocationLink uses targetUri + targetSelectionRange
        let val = json!({
            "targetUri": "file:///target.rs",
            "targetSelectionRange": {
                "start": { "line": 19, "character": 7 },
                "end":   { "line": 19, "character": 13 }
            },
            "targetRange": {
                "start": { "line": 18, "character": 0 },
                "end":   { "line": 21, "character": 1 }
            }
        });
        let locs = parse_locations(&val);
        // targetSelectionRange is preferred over targetRange
        assert_eq!(locs, vec!["/target.rs:20:8"]);
    }
}
