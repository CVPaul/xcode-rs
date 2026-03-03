use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;

/// A user-defined custom tool that executes a shell command.
/// Configured via `custom_tools` in config.json.
///
/// Example config:
/// ```json
/// {
///   "custom_tools": [
///     {
///       "name": "deploy",
///       "description": "Deploy the application to staging",
///       "command": "make deploy-staging",
///       "parameters": {}
///     }
///   ]
/// }
/// ```
pub struct CustomTool {
    pub tool_name: String,
    pub tool_description: String,
    pub command_template: String,
    pub tool_parameters: serde_json::Value,
}

#[async_trait]
impl Tool for CustomTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        if self.tool_parameters.is_object() && !self.tool_parameters.as_object().unwrap().is_empty()
        {
            serde_json::json!({
                "type": "object",
                "properties": self.tool_parameters,
                "required": []
            })
        } else {
            // No parameters — tool just runs the command as-is
            serde_json::json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "string",
                        "description": "Optional arguments appended to the command"
                    }
                }
            })
        }
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult> {
        // Build the final command by substituting any {{param}} placeholders
        let mut command = self.command_template.clone();

        if let Some(obj) = args.as_object() {
            for (key, value) in obj {
                let placeholder = format!("{{{{{}}}}}", key);
                let val_str = match value.as_str() {
                    Some(s) => s.to_string(),
                    None => value.to_string(),
                };
                command = command.replace(&placeholder, &val_str);
            }
            // If there's a plain "args" field and no placeholder, append it
            if let Some(extra) = obj.get("args").and_then(|v| v.as_str()) {
                if !self.command_template.contains("{{args}}") {
                    command = format!("{} {}", command, extra);
                }
            }
        }

        // Execute the command
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(&ctx.working_dir)
            .output()
            .await;

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let mut result = String::new();
                if !stdout.is_empty() {
                    result.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str("STDERR:\n");
                    result.push_str(&stderr);
                }
                if result.is_empty() {
                    result = format!("Command completed with exit code {}", out.status.code().unwrap_or(-1));
                }
                Ok(ToolResult {
                    output: result,
                    is_error: !out.status.success(),
                })
            }
            Err(e) => Ok(ToolResult {
                output: format!("Failed to execute custom tool command: {}", e),
                is_error: true,
            }),
        }
    }
}
