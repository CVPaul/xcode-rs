// src/tools/git_blame.rs
//
// Git blame tool — shows which commit and author last modified each line of a
// file, optionally restricted to a specific line range.
//
// For Rust learners: This is the simplest possible Tool implementation —
// a single subprocess call with a small parameter set.  Notice how we handle
// the optional line range by conditionally pushing "-L start,end" into the
// argument list.  We never build a shell string; we pass Vec<String> directly
// to Command::args() for safety.

use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;

pub struct GitBlameTool;

#[async_trait]
impl Tool for GitBlameTool {
    fn name(&self) -> &str {
        "git_blame"
    }

    fn description(&self) -> &str {
        "Show which commit and author last modified each line of a file. \
        Supports restricting output to a specific line range."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to blame (relative to project root or absolute)"
                },
                "start_line": {
                    "type": "integer",
                    "description": "First line of the range to blame (1-indexed, inclusive)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "Last line of the range to blame (1-indexed, inclusive)"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        // 1. Require `path` parameter.
        let path_str = match args["path"].as_str() {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => {
                return Ok(ToolResult {
                    output: "Missing required argument: path".to_string(),
                    is_error: true,
                });
            }
        };

        let mut git_args: Vec<String> = vec!["blame".to_string()];

        // 2. Optional line range: -L start,end
        //    Both start_line and end_line must be provided for the range filter
        //    to apply; if only one is given we ignore both to avoid bad syntax.
        let start = args["start_line"].as_u64();
        let end = args["end_line"].as_u64();

        if let (Some(s), Some(e)) = (start, end) {
            // git blame -L start,end
            git_args.push(format!("-L{},{}", s, e));
        }

        // 3. The file path comes last (no "--" needed for blame since it takes
        //    a single file argument, not a path list).
        git_args.push(path_str.clone());

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
                        output: format!("git blame failed: {}", stderr.trim()),
                        is_error: true,
                    });
                }

                let blame_text = String::from_utf8_lossy(&out.stdout).trim().to_string();

                if blame_text.is_empty() {
                    return Ok(ToolResult {
                        output: "No blame output (file may be empty or untracked).".to_string(),
                        is_error: false,
                    });
                }

                // Truncate to 50KB to avoid overwhelming the LLM context.
                const MAX_BYTES: usize = 50 * 1024;
                let output = if blame_text.len() > MAX_BYTES {
                    let mut p = MAX_BYTES;
                    while p > 0 && !blame_text.is_char_boundary(p) {
                        p -= 1;
                    }
                    format!(
                        "{}\n\n[... output truncated — use start_line/end_line to narrow the range ...]",
                        &blame_text[..p]
                    )
                } else {
                    blame_text
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
        }
    }

    #[tokio::test]
    async fn test_git_blame_missing_path() {
        let tool = GitBlameTool;
        let args = serde_json::json!({});
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Missing required argument: path"));
    }

    #[tokio::test]
    async fn test_git_blame_nonexistent_file() {
        let tool = GitBlameTool;
        let args = serde_json::json!({ "path": "nonexistent_file_xcode_test.rs" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        // git blame on a nonexistent file should error, not panic.
        let _ = result;
    }
}
