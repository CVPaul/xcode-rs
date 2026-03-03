// src/tools/git_commit.rs
//
// Git commit tool — stages files and creates a commit.
//
// For Rust learners: This tool is marked "destructive" because committing
// is an irreversible operation (without force-pushing or git reset --hard).
// The `is_destructive_call()` function in coder.rs checks for this tool's
// name and will ask the user to confirm before running it in interactive mode.

use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;

pub struct GitCommitTool;

#[async_trait]
impl Tool for GitCommitTool {
    fn name(&self) -> &str {
        "git_commit"
    }

    fn description(&self) -> &str {
        "Stage files and create a git commit. \
        Provide a commit message and either a list of files to stage or use \
        `all: true` to stage all changes. Returns the new commit hash and summary."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Commit message (required)"
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of file paths to stage before committing. \
                        Paths are relative to the project root."
                },
                "all": {
                    "type": "boolean",
                    "description": "Stage ALL changed and new tracked files (git add -A) before committing. \
                        If both `files` and `all` are provided, `all` takes precedence."
                }
            },
            "required": ["message"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        // 1. Validate required `message` argument.
        let message = match args["message"].as_str() {
            Some(m) if !m.trim().is_empty() => m.to_string(),
            _ => {
                return Ok(ToolResult {
                    output: "Missing required argument: message".to_string(),
                    is_error: true,
                });
            }
        };

        let working_dir = &ctx.working_dir;

        // 2. Stage files.
        //    Priority: `all` flag > explicit `files` list > nothing staged.
        let stage_all = args["all"].as_bool().unwrap_or(false);

        if stage_all {
            // git add -A stages everything (new, modified, deleted).
            let add_out = std::process::Command::new("git")
                .args(["add", "-A"])
                .current_dir(working_dir)
                .output();
            if let Err(e) = add_out {
                return Ok(ToolResult {
                    output: format!("Failed to run git add -A: {}", e),
                    is_error: true,
                });
            }
            let add_out = add_out.unwrap();
            if !add_out.status.success() {
                let stderr = String::from_utf8_lossy(&add_out.stderr);
                return Ok(ToolResult {
                    output: format!("git add -A failed: {}", stderr.trim()),
                    is_error: true,
                });
            }
        } else if let Some(files_arr) = args["files"].as_array() {
            // Stage only the specified files.
            let files: Vec<&str> = files_arr.iter().filter_map(|v| v.as_str()).collect();

            if files.is_empty() {
                return Ok(ToolResult {
                    output: "No files to stage — provide `files` or set `all: true`".to_string(),
                    is_error: true,
                });
            }

            // git add -- <file1> <file2> ...
            // The "--" separator ensures file paths aren't interpreted as flags.
            let mut add_args = vec!["add", "--"];
            add_args.extend(files.iter().copied());

            let add_out = std::process::Command::new("git")
                .args(&add_args)
                .current_dir(working_dir)
                .output();
            if let Err(e) = add_out {
                return Ok(ToolResult {
                    output: format!("Failed to run git add: {}", e),
                    is_error: true,
                });
            }
            let add_out = add_out.unwrap();
            if !add_out.status.success() {
                let stderr = String::from_utf8_lossy(&add_out.stderr);
                return Ok(ToolResult {
                    output: format!("git add failed: {}", stderr.trim()),
                    is_error: true,
                });
            }
        }
        // If neither `all` nor `files` is set, we proceed to commit whatever
        // is already staged (useful if the caller staged manually via bash).

        // 3. Create the commit.
        let commit_out = std::process::Command::new("git")
            .args(["commit", "-m", &message])
            .current_dir(working_dir)
            .output();

        match commit_out {
            Err(e) => Ok(ToolResult {
                output: format!("Failed to run git commit: {}", e),
                is_error: true,
            }),
            Ok(out) => {
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    // git commit -m on an empty stage produces a specific message.
                    let combined = format!("{}{}", stdout.trim(), stderr.trim());
                    return Ok(ToolResult {
                        output: format!("git commit failed: {}", combined),
                        is_error: true,
                    });
                }

                // Success — return the stdout which shows the short hash and summary.
                // Example: "[main abc1234] Add error handling\n 2 files changed, …"
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                Ok(ToolResult {
                    output: stdout,
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
    async fn test_git_commit_missing_message() {
        let tool = GitCommitTool;
        let args = serde_json::json!({});
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Missing required argument: message"));
    }

    #[tokio::test]
    async fn test_git_commit_empty_message() {
        let tool = GitCommitTool;
        let args = serde_json::json!({ "message": "   " });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Missing required argument: message"));
    }

    #[tokio::test]
    async fn test_git_commit_no_git_repo() {
        // /tmp is not a git repo — commit should fail gracefully.
        let tool = GitCommitTool;
        let args = serde_json::json!({ "message": "test commit", "all": true });
        let result = tool.execute(args, &ctx()).await.unwrap();
        // Should fail — not panic.
        assert!(result.is_error || result.output.contains("failed"));
    }
}
