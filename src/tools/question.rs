// ─── Question Tool ──────────────────────────────────────────────────────────
//
// Implements the "question" tool following the OpenCode pattern.
//
// When the LLM needs to ask the user a question (e.g. to clarify requirements,
// pick between approaches, or gather preferences), it issues a `question` tool
// call with:
//   - `question`: the full question text
//   - `options`: an array of { label, description } objects
//   - `multiple` (optional, default false): allow selecting multiple options
//
// Single-select mode (default):
//   Renders a `dialoguer::Select` menu — arrow keys to move, Enter to pick.
//
// Multi-select mode (`multiple: true`):
//   Renders a `dialoguer::MultiSelect` menu — arrow keys to move, Space to
//   toggle, Enter to confirm.
//
// In both modes a "Type your own answer" escape hatch is appended so the user
// can provide free-form input when none of the LLM's suggestions fit.
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
/// questions.  Supports both single-select and multi-select modes.
/// Available in both Act and Plan modes.
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
         - Answers are returned as arrays of labels; set `multiple: true` to allow \
         selecting more than one.\n\
         - Use `multiple: true` when the user could reasonably want MORE THAN ONE \
         option (e.g. \"which features to add?\", \"which files to modify?\", \
         \"which tests to run?\"). Use single-select (default) for mutually \
         exclusive choices (e.g. \"which approach?\", \"which language?\").\n\
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
                },
                "multiple": {
                    "type": "boolean",
                    "description": "Allow selecting multiple options (default: false). When true, user can toggle options with Space and confirm with Enter."
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

        // `multiple` defaults to false when omitted.
        let multiple = args["multiple"].as_bool().unwrap_or(false);

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
                // Format: "label — description"
                // (dialoguer doesn't support inline styles, so we use an em-dash)
                display_labels.push(format!("{} — {}", label, desc));
            }
        }

        // ── Branch on single-select vs multi-select ──────────────────────────
        if multiple {
            self.execute_multi_select(&question, &display_labels, &raw_labels)
                .await
        } else {
            self.execute_single_select(&question, &display_labels, &raw_labels)
                .await
        }
    }
}

impl QuestionTool {
    // ── Single-select mode ───────────────────────────────────────────────────
    // Arrow keys to navigate, Enter to pick ONE option.
    // A "Type your own answer" option is appended at the end.
    async fn execute_single_select(
        &self,
        question: &str,
        display_labels: &[String],
        raw_labels: &[String],
    ) -> Result<ToolResult> {
        // Clone data for the blocking closure (dialoguer reads stdin synchronously).
        let question_clone = question.to_string();
        let display_clone = display_labels.to_vec();
        let raw_clone = raw_labels.to_vec();
        let custom_index = display_clone.len(); // index of the "Type your own" option

        // Build the items list with the custom-answer escape hatch appended.
        let mut items = display_clone.clone();
        items.push("Type your own answer".to_string());

        // Render the Select menu in a blocking thread so we don't stall the
        // async executor while waiting for user input.
        let selection = tokio::task::spawn_blocking(move || {
            use dialoguer::{theme::ColorfulTheme, Select};

            // Print the question text above the menu.
            // stderr so it doesn't interfere with captured output in tests.
            eprintln!();
            eprintln!(
                "  {} {}",
                console::style("?").cyan().bold(),
                console::style(&question_clone).bold()
            );
            eprintln!();

            Select::with_theme(&ColorfulTheme::default())
                .items(&items)
                .default(0)
                .interact_opt()
        })
        .await
        .unwrap_or(Ok(None));

        // ── Handle the user's choice ─────────────────────────────────────────
        match selection {
            Ok(Some(idx)) if idx == custom_index => {
                // User chose "Type your own answer" — prompt for free-form input.
                let result = prompt_custom_answer().await;
                // Echo the custom answer and show thinking indicator.
                if let Ok(ref r) = result {
                    if !r.is_error {
                        eprintln!(
                            "  {} {}",
                            console::style("→").green().bold(),
                            console::style(&r.output).green(),
                        );
                        eprintln!(
                            "  {}",
                            console::style("thinking…").dim(),
                        );
                    }
                }
                result
            }
            Ok(Some(idx)) => {
                // User picked one of the LLM-provided options.
                let chosen = &raw_clone[idx];
                // Echo the selection so it appears in the conversation flow.
                eprintln!(
                    "  {} {}",
                    console::style("→").green().bold(),
                    console::style(chosen).green(),
                );
                // Show a thinking indicator so the user knows the agent is processing.
                eprintln!(
                    "  {}",
                    console::style("thinking…").dim(),
                );
                Ok(ToolResult {
                    output: format!("User selected: {}", chosen),
                    is_error: false,
                })
            }
            _ => {
                // User pressed Esc or Ctrl-C — treat as cancellation.
                eprintln!(
                    "  {} {}",
                    console::style("→").yellow().bold(),
                    console::style("cancelled").yellow(),
                );
                Ok(ToolResult {
                    output: "User cancelled the selection.".to_string(),
                    is_error: false,
                })
            }
        }
    }

    // ── Multi-select mode ────────────────────────────────────────────────────
    // Arrow keys to navigate, Space to toggle, Enter to confirm.
    // A "Type your own answer" option is appended at the end.
    async fn execute_multi_select(
        &self,
        question: &str,
        display_labels: &[String],
        raw_labels: &[String],
    ) -> Result<ToolResult> {
        let question_clone = question.to_string();
        let display_clone = display_labels.to_vec();
        let raw_clone = raw_labels.to_vec();
        let custom_index = display_clone.len();

        let mut items = display_clone.clone();
        items.push("Type your own answer".to_string());

        let selection = tokio::task::spawn_blocking(move || {
            use dialoguer::{theme::ColorfulTheme, MultiSelect};

            eprintln!();
            eprintln!(
                "  {} {} {}",
                console::style("?").cyan().bold(),
                console::style(&question_clone).bold(),
                console::style("(Space to toggle, Enter to confirm)").dim()
            );
            eprintln!();

            MultiSelect::with_theme(&ColorfulTheme::default())
                .items(&items)
                .interact_opt()
        })
        .await
        .unwrap_or(Ok(None));

        match selection {
            Ok(Some(indices)) if indices.is_empty() => {
                // User pressed Enter without selecting anything.
                eprintln!(
                    "  {} {}",
                    console::style("→").yellow().bold(),
                    console::style("(nothing selected)").yellow(),
                );
                Ok(ToolResult {
                    output: "(User selected nothing)".to_string(),
                    is_error: false,
                })
            }
            Ok(Some(indices)) => {
                // Check if the user toggled "Type your own answer".
                let chose_custom = indices.contains(&custom_index);

                // Collect the selected LLM-provided labels (skip the custom option).
                let selected: Vec<String> = indices
                    .iter()
                    .filter(|&&i| i < raw_clone.len())
                    .map(|&i| raw_clone[i].clone())
                    .collect();

                let result = if chose_custom && selected.is_empty() {
                    // Only custom was picked — prompt for free-form input.
                    prompt_custom_answer().await
                } else if chose_custom {
                    // Custom + some real options — get the custom text and merge.
                    let custom = prompt_custom_answer_text().await;
                    let mut all = selected;
                    if !custom.is_empty() {
                        all.push(custom);
                    }
                    Ok(ToolResult {
                        output: format!("User selected: {}", all.join(", ")),
                        is_error: false,
                    })
                } else {
                    // Normal multi-select — return comma-separated labels.
                    Ok(ToolResult {
                        output: format!("User selected: {}", selected.join(", ")),
                        is_error: false,
                    })
                };

                // Echo the selection and show thinking indicator.
                if let Ok(ref r) = result {
                    if !r.is_error {
                        eprintln!(
                            "  {} {}",
                            console::style("→").green().bold(),
                            console::style(&r.output).green(),
                        );
                        eprintln!(
                            "  {}",
                            console::style("thinking…").dim(),
                        );
                    }
                }

                result
            }
            _ => {
                // User pressed Esc or Ctrl-C.
                eprintln!(
                    "  {} {}",
                    console::style("→").yellow().bold(),
                    console::style("cancelled").yellow(),
                );
                Ok(ToolResult {
                    output: "User cancelled the selection.".to_string(),
                    is_error: false,
                })
            }
        }
    }
}

// ─── Shared helpers ──────────────────────────────────────────────────────────

/// Prompt the user for free-form text input and return a ToolResult.
async fn prompt_custom_answer() -> Result<ToolResult> {
    let text = prompt_custom_answer_text().await;
    if text.is_empty() {
        Ok(ToolResult {
            output: "(User provided no answer)".to_string(),
            is_error: false,
        })
    } else {
        Ok(ToolResult {
            output: format!("User answered: {}", text),
            is_error: false,
        })
    }
}

/// Read one line of free-form text from stdin (blocking, spawned on a thread).
async fn prompt_custom_answer_text() -> String {
    tokio::task::spawn_blocking(|| {
        use std::io::Write;
        eprint!("  {} ", console::style("Your answer:").cyan());
        let _ = std::io::stderr().flush();

        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap_or(0);
        line.trim().to_string()
    })
    .await
    .unwrap_or_default()
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> crate::tools::ToolContext {
        crate::tools::ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
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
        assert!(schema["properties"]["multiple"].is_object());

        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("question")));
        assert!(required.contains(&serde_json::json!("options")));
        // `multiple` is optional — should NOT be in required
        assert!(!required.contains(&serde_json::json!("multiple")));
    }

    #[test]
    fn test_question_schema_multiple_field() {
        // Verify the `multiple` field has the correct type and description.
        let tool = QuestionTool;
        let schema = tool.parameters_schema();
        let multiple = &schema["properties"]["multiple"];
        assert_eq!(multiple["type"], "boolean");
        assert!(multiple["description"]
            .as_str()
            .unwrap()
            .contains("multiple"));
    }

    #[tokio::test]
    async fn test_question_missing_question_arg() {
        let tool = QuestionTool;
        let ctx = test_ctx();
        let args = serde_json::json!({
            "options": [{"label": "A", "description": "option a"}]
        });
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result
            .output
            .contains("Missing required argument: question"));
    }

    #[tokio::test]
    async fn test_question_missing_options_arg() {
        let tool = QuestionTool;
        let ctx = test_ctx();
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
        let ctx = test_ctx();
        let args = serde_json::json!({
            "question": "Pick something",
            "options": []
        });
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("empty"));
    }

    #[tokio::test]
    async fn test_question_multiple_defaults_false() {
        // When `multiple` is omitted, it should default to false.
        // We can't fully test the interactive menu without a TTY, but we can
        // verify the parameter parsing by checking that the function reaches
        // the single-select branch (which calls dialoguer::Select).
        // Here we just verify the args parsing doesn't error on missing `multiple`.
        let tool = QuestionTool;
        let ctx = test_ctx();
        let args = serde_json::json!({
            "question": "Pick one",
            "options": [
                {"label": "A", "description": "first"},
                {"label": "B", "description": "second"}
            ]
            // `multiple` deliberately omitted — should default to false
        });
        // This will try to render a Select menu and fail in non-TTY,
        // but it should NOT return a parameter-error ToolResult.
        let result = tool.execute(args, &ctx).await.unwrap();
        // In a non-TTY test environment, dialoguer returns None → "User cancelled"
        // The important thing is it's NOT an is_error about missing params.
        assert!(!result.is_error || !result.output.contains("Missing"));
    }
}
