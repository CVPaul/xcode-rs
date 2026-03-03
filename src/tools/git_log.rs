// src/tools/git_log.rs
//
// Git log tool — shows recent commit history, optionally filtered by file path
// or searched by commit message content.
//
// For Rust learners: A tool can expose multiple output formats through a single
// parameter.  Here we handle `format: "oneline"` vs `format: "full"` inside
// execute() rather than making two separate tools, which keeps the LLM's
// tool set small while giving it flexibility.

use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;

pub struct GitLogTool;

#[async_trait]
impl Tool for GitLogTool {
    fn name(&self) -> &str {
        "git_log"
    }

    fn description(&self) -> &str {
        "Show recent git commit history. Supports filtering by file path, \
        searching commit messages, and choosing between compact (oneline) and \
        detailed (full) output formats."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "count": {
                    "type": "integer",
                    "description": "Maximum number of commits to show (default: 10)"
                },
                "path": {
                    "type": "string",
                    "description": "Only show commits that touched this file or directory"
                },
                "format": {
                    "type": "string",
                    "enum": ["oneline", "full"],
                    "description": "Output format: 'oneline' for compact hash+message, 'full' for author/date/body (default: oneline)"
                },
                "search": {
                    "type": "string",
                    "description": "Filter commits whose message contains this string (case-insensitive grep)"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        let count = args["count"].as_u64().unwrap_or(10).max(1) as usize;
        let format = args["format"].as_str().unwrap_or("oneline");
        let path_str = args["path"].as_str().unwrap_or("").to_string();
        let search = args["search"].as_str().unwrap_or("").to_string();

        // Build git log argument list.
        let mut git_args: Vec<String> = vec!["log".to_string()];

        // Number of commits.
        git_args.push(format!("-{}", count));

        // Output format.
        match format {
            "full" => {
                // Shows: commit hash, author, date, and full message body.
                git_args.push("--format=fuller".to_string());
            }
            _ => {
                // Default: one line per commit — hash + subject.
                git_args.push("--oneline".to_string());
            }
        }

        // Case-insensitive grep of commit messages.
        if !search.is_empty() {
            // --grep filters by message; -i makes it case-insensitive.
            git_args.push(format!("--grep={}", search));
            git_args.push("-i".to_string());
        }

        // File path filter — must come after "--" to be treated as a path.
        if !path_str.is_empty() {
            git_args.push("--".to_string());
            git_args.push(path_str.clone());
        }

        let output = std::process::Command::new("git")
            .args(&git_args)
            .current_dir(&ctx.working_dir)
            .output();

        match output {
            Err(e) => Ok(ToolResult {
                output: format!("Failed to run git: {}", e),
                is_error: true,
            }),
            Ok(out) => {
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Ok(ToolResult {
                        output: format!("git log failed: {}", stderr.trim()),
                        is_error: true,
                    });
                }

                let log_text = String::from_utf8_lossy(&out.stdout).trim().to_string();

                if log_text.is_empty() {
                    return Ok(ToolResult {
                        output: "No commits found.".to_string(),
                        is_error: false,
                    });
                }

                // Truncate at 50KB to avoid flooding the context window.
                const MAX_BYTES: usize = 50 * 1024;
                let output = if log_text.len() > MAX_BYTES {
                    let mut p = MAX_BYTES;
                    while p > 0 && !log_text.is_char_boundary(p) {
                        p -= 1;
                    }
                    format!("{}\n\n[... output truncated ...]", &log_text[..p])
                } else {
                    log_text
                };

                Ok(ToolResult {
                    output,
                    is_error: false,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx() -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from("/tmp"),
            sandbox_enabled: false,
            io: std::sync::Arc::new(crate::io::NullIO),
            compact_mode: false,
            lsp_client: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            mcp_client: None,
            nesting_depth: 0,
            llm: std::sync::Arc::new(crate::llm::NullLlmProvider),
            tools: std::sync::Arc::new(crate::tools::ToolRegistry::new()),
            permissions: vec![],
            formatters: std::collections::HashMap::new(),
        }
    }

    #[tokio::test]
    async fn test_git_log_not_a_repo() {
        // /tmp is not a git repo — should fail gracefully with an error message.
        let tool = GitLogTool;
        let args = serde_json::json!({});
        let result = tool.execute(args, &ctx()).await.unwrap();
        // Just confirm it doesn't panic — error is expected.
        let _ = result;
    }

    #[tokio::test]
    async fn test_git_log_default_count() {
        // Confirm default count of 10 is used when no count is provided.
        // We can't easily assert on git output here, just confirm no panic.
        let tool = GitLogTool;
        let args = serde_json::json!({ "format": "oneline" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(!result.output.is_empty());
    }
}
