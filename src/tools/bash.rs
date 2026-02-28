use crate::sandbox::NoSandbox;
use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;

/// BashTool executes shell commands via the NoSandbox executor.
/// Commands run as `sh -c <command>` in the session's working directory.
pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a bash/shell command in the working directory. \
        Returns stdout, stderr, and exit code. \
        Output is truncated to 50KB (first 25KB + last 25KB) if too large."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 120)"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        // Extract required `command` argument
        let command = match args["command"].as_str() {
            Some(c) => c.to_string(),
            None => {
                return Ok(ToolResult {
                    output: "Missing required argument: command".to_string(),
                    is_error: true,
                });
            }
        };

        // Extract optional `timeout` argument, default 120 seconds
        let timeout_secs = args["timeout"].as_u64().unwrap_or(120);

        // Create a NoSandbox executor bound to the session working directory
        let sandbox = NoSandbox::new(ctx.working_dir.clone());

        // Execute the command
        let exec_result = match sandbox.exec(&command, timeout_secs).await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult {
                    output: format!("Failed to execute command: {}", e),
                    is_error: true,
                });
            }
        };

        // Handle timeout
        if exec_result.timed_out {
            return Ok(ToolResult {
                output: format!("Command timed out after {}s", timeout_secs),
                is_error: true,
            });
        }

        // Truncate output if combined stdout+stderr exceeds 50KB
        const MAX_BYTES: usize = 50 * 1024;
        const HALF_MAX: usize = MAX_BYTES / 2;

        let stdout = truncate_output(&exec_result.stdout, HALF_MAX);
        let stderr = truncate_output(&exec_result.stderr, HALF_MAX);

        let output = format!(
            "Exit code: {}\nStdout:\n{}\nStderr:\n{}",
            exec_result.exit_code, stdout, stderr
        );

        Ok(ToolResult {
            output,
            is_error: exec_result.exit_code != 0,
        })
    }
}

/// Truncate a string to `max_bytes`, keeping first half and last half
/// with a notice in the middle if truncation is needed.
fn truncate_output(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Split at character boundaries
    let half = max_bytes / 2;
    // Find safe character boundary for first half
    let first_end = find_char_boundary(s, half);
    // Find safe character boundary for last half
    let last_start = s.len() - find_char_boundary_from_end(s, half);
    format!(
        "{}\n\n[... {} bytes truncated ...]\n\n{}",
        &s[..first_end],
        s.len() - max_bytes,
        &s[last_start..]
    )
}

/// Find the largest valid UTF-8 char boundary <= `pos`.
fn find_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos.min(s.len());
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Find the largest valid UTF-8 char boundary <= `len` counting from the END.
/// Returns offset from start where last `len` bytes begin (adjusted to char boundary).
fn find_char_boundary_from_end(s: &str, len: usize) -> usize {
    let total = s.len();
    if len >= total {
        return 0;
    }
    let mut start = total - len;
    while start < total && !s.is_char_boundary(start) {
        start += 1;
    }
    total - start
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx() -> ToolContext {
        ToolContext {
            working_dir: PathBuf::from("/tmp"),
            sandbox_enabled: false,
        }
    }

    #[tokio::test]
    async fn test_bash_execute() {
        let tool = BashTool;
        let args = serde_json::json!({ "command": "echo hello" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("hello"));
        assert!(result.output.contains("Exit code: 0"));
    }

    #[tokio::test]
    async fn test_bash_exit_code() {
        let tool = BashTool;
        let args = serde_json::json!({ "command": "exit 1" });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Exit code: 1"));
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let tool = BashTool;
        let args = serde_json::json!({ "command": "sleep 10", "timeout": 1 });
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("timed out"));
    }

    #[tokio::test]
    async fn test_bash_missing_command() {
        let tool = BashTool;
        let args = serde_json::json!({});
        let result = tool.execute(args, &ctx()).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Missing required argument"));
    }

    #[test]
    fn test_truncate_output_short() {
        let s = "hello world";
        assert_eq!(truncate_output(s, 100), "hello world");
    }

    #[test]
    fn test_truncate_output_long() {
        let s = "a".repeat(200);
        let out = truncate_output(&s, 100);
        assert!(out.contains("truncated"));
    }
}
