// ─── Question Tool ──────────────────────────────────────────────────────────
//
// Implements the "question" tool following the OpenCode pattern.
//
// When the LLM needs to ask the user a question (e.g. to clarify requirements,
// pick between approaches, or gather preferences), it issues a `question` tool
// call with:
//   - `question`: the full question text
//   - `options`: an array of { label, description } objects
//
// This tool renders a `dialoguer::Select` menu in the terminal so the user
// can navigate options with arrow keys and press Enter.  A "Type your own
// answer" option is always appended, allowing free-form input when none of
// the LLM's suggestions fit.
//
// The user's selection (label text, or their typed answer) is returned as the
// tool result string, which the LLM sees on the next turn.
//
// This approach is deterministic and reliable — unlike prompt-based approaches
// that ask the LLM to output a special format (which it often ignores).
// ────────────────────────────────────────────────────────────────────────────

use crate::tools::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;

/// The question tool — renders interactive selection menus for LLM-initiated
/// questions.  Available in both Act and Plan modes.
pub struct QuestionTool;

#[async_trait]
impl Tool for QuestionTool {
    fn name(&self) -> &str {
        "question"
    }

    fn description(&self) -> &str {
        "Ask the user a question with selectable options. Use this when you need to:\n\
         1. Gather user preferences or requirements\n\
         2. Clarify ambiguous instructions\n\
         3. Get decisions on implementation choices\n\
         4. Offer choices about what direction to take\n\n\
         Usage notes:\n\
         - A \"Type your own answer\" option is added automatically; do NOT include \
         \"Other\" or catch-all options.\n\
         - If you recommend a specific option, make it the first option in the list \
         and add \"(Recommended)\" at the end of the label.\n\
         - Ask ONE question at a time. Never batch multiple questions into a single call."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The complete question to ask the user"
                },
                "options": {
                    "type": "array",
                    "description": "Available choices (2-6 options recommended)",
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": {
                                "type": "string",
                                "description": "Display text for this option (1-5 words, concise)"
                            },
                            "description": {
                                "type": "string",
                                "description": "Brief explanation of what this option means"
                            }
                        },
                        "required": ["label", "description"]
                    }
                }
            },
            "required": ["question", "options"]
        })
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolResult> {
        // ── Extract parameters ───────────────────────────────────────────────
        let question = match args["question"].as_str() {
            Some(q) => q.to_string(),
            None => {
                return Ok(ToolResult {
                    output: "Missing required argument: question".to_string(),
                    is_error: true,
                });
            }
        };

        let options = match args["options"].as_array() {
            Some(arr) => arr.clone(),
            None => {
                return Ok(ToolResult {
                    output: "Missing required argument: options (must be an array)".to_string(),
                    is_error: true,
                });
            }
        };

        if options.is_empty() {
            return Ok(ToolResult {
                output: "Options array is empty — provide at least 2 options.".to_string(),
                is_error: true,
            });
        }

        // ── Build display labels ─────────────────────────────────────────────
        // Each option has a `label` and `description`.  We format them as:
        //   "label — description"
        // so the user sees both in the selection menu.
        let mut display_labels: Vec<String> = Vec::new();
        let mut raw_labels: Vec<String> = Vec::new();

        for opt in &options {
            let label = opt["label"].as_str().unwrap_or("(unnamed)");
            let desc = opt["description"].as_str().unwrap_or("");
            raw_labels.push(label.to_string());

            if desc.is_empty() {
                display_labels.push(label.to_string());
            } else {
                // Format: "label — description" with dim description
                // (dialoguer doesn't support inline styles, so we use a plain dash)
                display_labels.push(format!("{} — {}", label, desc));
            }
        }

        // Always add a "Type your own answer" escape hatch at the end.
        let custom_index = display_labels.len();
        display_labels.push("Type your own answer".to_string());

        // ── Render the selection menu ────────────────────────────────────────
        // We use tokio::task::spawn_blocking because dialoguer::Select reads
        // from stdin synchronously, and we must not block the async executor.
        let question_clone = question.clone();
        let display_clone = display_labels.clone();

        let selection = tokio::task::spawn_blocking(move || {
            use dialoguer::{theme::ColorfulTheme, Select};

            // Print the question text above the menu.
            // Use eprintln so it doesn't interfere with captured output in tests.
            eprintln!();
            eprintln!("  {} {}", console::style("?").cyan().bold(), console::style(&question_clone).bold());
            eprintln!();

            Select::with_theme(&ColorfulTheme::default())
                .items(&display_clone)
                .default(0)
                .interact_opt()
        })
        .await
        .unwrap_or(Ok(None));

        // ── Handle the user's choice ─────────────────────────────────────────
        match selection {
            Ok(Some(idx)) if idx == custom_index => {
                // User chose "Type your own answer" — prompt for free-form input.
                let custom_answer = tokio::task::spawn_blocking(|| {
                    use std::io::Write;
                    eprint!("  {} ", console::style("Your answer:").cyan());
                    let _ = std::io::stderr().flush();

                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line).unwrap_or(0);
                    line.trim().to_string()
                })
                .await
                .unwrap_or_default();

                if custom_answer.is_empty() {
                    Ok(ToolResult {
                        output: "(User provided no answer)".to_string(),
                        is_error: false,
                    })
                } else {
                    Ok(ToolResult {
                        output: format!("User answered: {}", custom_answer),
                        is_error: false,
                    })
                }
            }
            Ok(Some(idx)) => {
                // User picked one of the LLM-provided options.
                let chosen_label = &raw_labels[idx];
                Ok(ToolResult {
                    output: format!("User selected: {}", chosen_label),
                    is_error: false,
                })
            }
            _ => {
                // User pressed Esc or Ctrl-C — treat as cancellation.
                Ok(ToolResult {
                    output: "User cancelled the selection.".to_string(),
                    is_error: false,
                })
            }
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_question_tool_metadata() {
        let tool = QuestionTool;
        assert_eq!(tool.name(), "question");
        assert!(tool.description().contains("Ask the user"));

        // Verify the schema has the expected structure
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["question"].is_object());
        assert!(schema["properties"]["options"].is_object());

        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("question")));
        assert!(required.contains(&serde_json::json!("options")));
    }

    #[tokio::test]
    async fn test_question_missing_question_arg() {
        let tool = QuestionTool;
        let ctx = crate::tools::ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            sandbox_enabled: false,
            confirm_destructive: false,
        };
        // Missing "question" key entirely
        let args = serde_json::json!({
            "options": [{"label": "A", "description": "option a"}]
        });
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Missing required argument: question"));
    }

    #[tokio::test]
    async fn test_question_missing_options_arg() {
        let tool = QuestionTool;
        let ctx = crate::tools::ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            sandbox_enabled: false,
            confirm_destructive: false,
        };
        let args = serde_json::json!({
            "question": "Pick a color"
        });
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("Missing required argument: options"));
    }

    #[tokio::test]
    async fn test_question_empty_options() {
        let tool = QuestionTool;
        let ctx = crate::tools::ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            sandbox_enabled: false,
            confirm_destructive: false,
        };
        let args = serde_json::json!({
            "question": "Pick something",
            "options": []
        });
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("empty"));
    }
}
