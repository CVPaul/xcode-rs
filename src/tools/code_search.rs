use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;

/// Code search tool using ripgrep (rg) for fast codebase-wide search.
/// Similar to Sourcegraph's code search — supports regex, file type filtering,
/// and context lines.
pub struct CodeSearchTool;

#[async_trait]
impl Tool for CodeSearchTool {
    fn name(&self) -> &str {
        "code_search"
    }

    fn description(&self) -> &str {
        "Search code across the project using ripgrep. Supports regex patterns, \
         file type filtering, and context lines. Returns matching lines with \
         file paths and line numbers. Use this for broad codebase exploration."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search pattern (regex supported)"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: project root)"
                },
                "file_type": {
                    "type": "string",
                    "description": "File type filter, e.g. 'rs', 'ts', 'py'"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Number of context lines around matches (default: 2)"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Case-sensitive search (default: true)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (default: 50)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        let query = match args["query"].as_str() {
            Some(q) => q,
            None => {
                return Ok(ToolResult {
                    output: "Missing required argument: query".to_string(),
                    is_error: true,
                });
            }
        };

        let search_path = args["path"]
            .as_str()
            .map(|p| ctx.working_dir.join(p))
            .unwrap_or_else(|| ctx.working_dir.clone());

        let file_type = args["file_type"].as_str();
        let context_lines = args["context_lines"].as_u64().unwrap_or(2);
        let case_sensitive = args["case_sensitive"].as_bool().unwrap_or(true);
        let max_results = args["max_results"].as_u64().unwrap_or(50);

        // Build ripgrep command
        let mut cmd = tokio::process::Command::new("rg");
        cmd.arg("--line-number")
            .arg("--color=never")
            .arg("--no-heading")
            .arg(format!("--max-count={}", max_results))
            .arg(format!("--context={}", context_lines));

        if !case_sensitive {
            cmd.arg("--ignore-case");
        }

        if let Some(ft) = file_type {
            cmd.arg("--type").arg(ft);
        }

        cmd.arg("--").arg(query).arg(&search_path);

        let output = match cmd.output().await {
            Ok(o) => o,
            Err(_e) => {
                // ripgrep not installed — fall back to grep
                return self.fallback_grep(query, &search_path, context_lines, case_sensitive, max_results).await;
            }
        };

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let result = self.format_output(&stdout, &ctx.working_dir, max_results as usize);
            Ok(ToolResult {
                output: if result.is_empty() {
                    "No matches found.".to_string()
                } else {
                    result
                },
                is_error: false,
            })
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.code() == Some(1) {
                // Exit code 1 = no matches (not an error)
                Ok(ToolResult {
                    output: "No matches found.".to_string(),
                    is_error: false,
                })
            } else {
                Ok(ToolResult {
                    output: format!("Search failed: {}", stderr),
                    is_error: true,
                })
            }
        }
    }
}

impl CodeSearchTool {
    fn format_output(&self, raw: &str, working_dir: &std::path::Path, max_results: usize) -> String {
        let wd = working_dir.to_string_lossy();
        let mut count = 0;
        let mut output = String::new();
        for line in raw.lines() {
            // Strip the working directory prefix for cleaner output
            let clean = if line.starts_with(wd.as_ref()) {
                line[wd.len()..].trim_start_matches('/')
            } else {
                line
            };
            output.push_str(clean);
            output.push('\n');
            // Count actual match lines (not context/separator lines)
            if !line.starts_with("--") {
                count += 1;
                if count >= max_results {
                    output.push_str(&format!("\n... truncated at {} matches\n", max_results));
                    break;
                }
            }
        }
        output
    }

    async fn fallback_grep(
        &self,
        query: &str,
        search_path: &std::path::Path,
        context_lines: u64,
        case_sensitive: bool,
        max_results: u64,
    ) -> Result<ToolResult> {
        let mut cmd = tokio::process::Command::new("grep");
        cmd.arg("-rn")
            .arg("--color=never")
            .arg(format!("-C{}", context_lines))
            .arg(format!("-m{}", max_results));

        if !case_sensitive {
            cmd.arg("-i");
        }

        cmd.arg("--").arg(query).arg(search_path);

        let output = match cmd.output().await {
            Ok(o) => o,
            Err(e) => {
                return Ok(ToolResult {
                    output: format!("Neither ripgrep nor grep available: {}", e),
                    is_error: true,
                });
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(ToolResult {
            output: if stdout.is_empty() {
                "No matches found.".to_string()
            } else {
                stdout.to_string()
            },
            is_error: false,
        })
    }
}
