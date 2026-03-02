// src/tools/git_diff.rs
//
// Git diff tool — shows changes in the working tree, staged changes, or
// differences against a specific commit.
//
// For Rust learners: This follows the exact same pattern as BashTool but with
// typed parameters and a specialized output format.  Notice that we execute a
// subprocess just like BashTool does, but we build the argument list ourselves
// instead of passing a raw shell string.  This avoids shell-injection risk.

use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;

pub struct GitDiffTool;

#[async_trait]
impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }

    fn description(&self) -> &str {
        "Show git diff output for the working tree, staged changes, or against a commit. \
        Returns formatted diff text. Output is truncated to 50KB if too large."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Specific file or directory to diff (optional — omit for entire repo)"
                },
                "staged": {
                    "type": "boolean",
                    "description": "Show staged (index) diff instead of working-tree diff (default: false)"
                },
                "commit": {
                    "type": "string",
                    "description": "Compare working tree against this commit hash or ref (e.g. 'HEAD', 'main', 'abc1234')"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        // Build the git diff argument list.
        // We use std::process::Command directly (no shell) to avoid injection.
        let mut git_args: Vec<String> = vec!["diff".to_string()];

        // --staged shows index vs HEAD; without it we get working-tree vs index.
        let staged = args["staged"].as_bool().unwrap_or(false);
        if staged {
            git_args.push("--staged".to_string());
        }

        // Optional commit ref to diff against.
        if let Some(commit) = args["commit"].as_str() {
            if !commit.is_empty() {
                git_args.push(commit.to_string());
            }
        }

        // Optional path filter — separated by "--" so git treats it as a path,
        // not a ref, even if it looks like one.
        let path_str = args["path"].as_str().unwrap_or("").to_string();
        if !path_str.is_empty() {
            git_args.push("--".to_string());
            git_args.push(path_str.clone());
        }

        // Run git diff in the project working directory.
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
                        output: format!("git diff failed: {}", stderr.trim()),
                        is_error: true,
                    });
                }

                let diff_text = String::from_utf8_lossy(&out.stdout).into_owned();

                if diff_text.is_empty() {
                    return Ok(ToolResult {
                        output: "No differences found.".to_string(),
                        is_error: false,
                    });
                }

                // Truncate to 50KB to prevent overwhelming the LLM context.
                const MAX_BYTES: usize = 50 * 1024;
                let output = if diff_text.len() > MAX_BYTES {
                    // Keep the first 50KB and add a truncation notice.
                    let truncated = &diff_text[..find_char_boundary(&diff_text, MAX_BYTES)];
                    format!(
                        "{}\n\n[... {} bytes truncated — use the `path` parameter to narrow the diff ...]",
                        truncated,
                        diff_text.len() - MAX_BYTES
                    )
                } else {
                    diff_text
                };

                Ok(ToolResult {
                    output,
                    is_error: false,
                })
            }
        }
    }
}

/// Find the largest valid UTF-8 char boundary <= `pos`.
fn find_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos.min(s.len());
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
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
    async fn test_git_diff_missing_git_repo() {
        // /tmp is unlikely to be a git repo, so git diff should fail.
        let tool = GitDiffTool;
        let args = serde_json::json!({});
        let result = tool.execute(args, &ctx()).await.unwrap();
        // Either fails (not a git repo) or returns "No differences found."
        // Both are acceptable — we just check the tool doesn't panic.
        let _ = result;
    }

    #[tokio::test]
    async fn test_git_diff_staged_flag_builds_correctly() {
        // We can't assert git output in isolation, but we can confirm the tool
        // runs without a crash when the staged flag is set.
        let tool = GitDiffTool;
        let args = serde_json::json!({ "staged": true });
        let result = tool.execute(args, &ctx()).await.unwrap();
        // Just assert it returned *some* output (even an error is fine for /tmp).
        assert!(!result.output.is_empty());
    }

    #[test]
    fn test_find_char_boundary_ascii() {
        let s = "hello world";
        assert_eq!(find_char_boundary(s, 5), 5);
        assert_eq!(find_char_boundary(s, 100), s.len());
    }
}
