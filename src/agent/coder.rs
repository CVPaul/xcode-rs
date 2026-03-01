use crate::agent::{truncate_messages, Agent, AgentResult};
use crate::config::AgentConfig;
use crate::llm::{LlmProvider, Message, ToolDefinition};
use crate::tools::{ToolContext, ToolRegistry};
use anyhow::Result;
use async_trait::async_trait;

pub struct CoderAgent {
    pub config: AgentConfig,
}

impl CoderAgent {
    pub fn new(config: AgentConfig) -> Self {
        CoderAgent { config }
    }
}

#[async_trait]
impl Agent for CoderAgent {
    fn name(&self) -> &str {
        "coder"
    }

    fn system_prompt(&self) -> &str {
        // ──────────────────────────────────────────────────────────────────────
        // This prompt is the single most important piece of text in the project.
        // A weak system prompt produces verbose, over-explaining agents that ask
        // unnecessary questions and repeat themselves.  A strong prompt produces
        // concise, professional, tool-focused behavior.
        //
        // IMPORTANT: The prompt includes task-planning and completion-signaling
        // instructions that the auto-continue logic in `run()` depends on.
        // Specifically, the LLM MUST output `[TASK_COMPLETE]` at the very end
        // of its final summary.  The `looks_like_task_complete()` helper checks
        // for that marker to decide whether to stop or inject a "continue"
        // message.  If you change the marker here, update the helper too.
        // ──────────────────────────────────────────────────────────────────────
        concat!(
            "You are xcodeai, an expert autonomous software engineer.\n",
            "\n",
            "## Task execution workflow\n",
            "When you receive a task:\n",
            "1. PLAN — Output a numbered step list (e.g. `## Plan\n1. …\n2. …`).\n",
            "2. EXECUTE — Work through each step. Before each step output a progress\n",
            "   header like `## [Step 1/N] <short description>`.\n",
            "3. VERIFY — After all steps, compile/test/lint to confirm everything works.\n",
            "4. SUMMARIZE — Give ONE short paragraph of what changed and why.\n",
            "5. SIGNAL — End your final message with the EXACT marker `[TASK_COMPLETE]`\n",
            "   on its own line. This tells the harness the entire task is finished.\n",
            "   NEVER output `[TASK_COMPLETE]` until ALL steps are done and verified.\n",
            "\n",
            "## Core behavior\n",
            "- Complete coding tasks fully and autonomously. Never ask for permission to proceed.\n",
            "- Be concise in all responses. No greetings, no affirmations, no apologies.\n",
            "- When you have enough information to act, act. Do not describe what you are about to do — just do it.\n",
            "- Do NOT stop after completing one step. Continue immediately to the next step.\n",
            "\n",
            "## Tool use\n",
            "- Read files before editing them. Never guess at file contents.\n",
            "- Prefer targeted edits (file_edit) over full rewrites (file_write) when only a section needs changing.\n",
            "- Use glob_search and grep_search to understand the codebase before making assumptions.\n",
            "- Run bash commands to verify your work: compile, test, lint.\n",
            "- If a command fails, read the error output carefully and fix the root cause.\n",
            "\n",
            "## Code quality\n",
            "- Match the style, indentation, and conventions already present in the file.\n",
            "- Do not introduce new dependencies unless explicitly requested.\n",
            "- Write idiomatic code for the language in use.\n",
            "- Add comments only where the logic is non-obvious — do not narrate obvious steps.\n",
            "\n",
            "## What NOT to do\n",
            "- Do not produce placeholder code with TODO comments unless instructed.\n",
            "- Do not ask clarifying questions during execution — make a reasonable decision and proceed.\n",
            "- If you truly cannot proceed without critical missing information, use the `question` tool to ask ONE concise question with selectable options. Do not list multiple questions. Wait for the answer before asking anything else.\n",
            "- Do not repeat the user's instructions back to them.\n",
            "- Do not explain what a tool does — just use it.\n",
            "- NEVER output `[TASK_COMPLETE]` prematurely. Only use it after ALL steps are done and verified.\n"
        )
    }

    async fn run(
        &self,
        messages: &mut Vec<Message>,
        tools: &ToolRegistry,
        llm: &dyn LlmProvider,
        ctx: &ToolContext,
    ) -> Result<AgentResult> {
        let tool_defs = build_tool_definitions(tools);
        let mut iterations = 0u32;
        let mut tool_calls_total = 0u32;
        let mut auto_continues = 0u32;
        loop {
            truncate_messages(messages, 400_000);
            let response = llm.chat_completion(messages, &tool_defs).await?;

            // ── No tool calls → the LLM returned text only ──────────────────
            // This is either:
            //   (a) The plan + first step description (before any tools are used)
            //   (b) An intermediate summary between steps
            //   (c) The final summary with `[TASK_COMPLETE]` marker
            //
            // For (a) and (b), we inject a "continue" message and loop back
            // so the LLM keeps working autonomously.
            // For (c), we return the result and stop.
            let has_tool_calls = response
                .tool_calls
                .as_ref()
                .map(|tc| !tc.is_empty())
                .unwrap_or(false);

            if !has_tool_calls {
                let text = response
                    .content
                    .unwrap_or_else(|| "Task completed.".to_string());

                // NOTE: Do NOT println! here — the content was already streamed
                // to stdout in real time by openai.rs (when stream_print=true).
                // Printing again would duplicate the output.

                // Check if the LLM signaled full task completion.
                if looks_like_task_complete(&text)
                    || auto_continues >= self.config.max_auto_continues
                {
                    // If we hit max auto-continues, log a warning so the user
                    // knows why we stopped before seeing [TASK_COMPLETE].
                    if auto_continues >= self.config.max_auto_continues
                        && !looks_like_task_complete(&text)
                    {
                        tracing::warn!(
                            "Reached max auto-continues ({}). Stopping.",
                            self.config.max_auto_continues
                        );
                        eprintln!(
                            "\n  {} {}",
                            console::style("!").yellow().bold(),
                            console::style(format!(
                                "Reached auto-continue limit ({}). The task may not be fully complete.",
                                self.config.max_auto_continues
                            )).yellow(),
                        );
                    }
                    return Ok(AgentResult {
                        final_message: text,
                        iterations,
                        tool_calls_total,
                        auto_continues,
                    });
                }

                // Not complete — push the assistant's text into history and
                // inject a "continue" message so the LLM picks up where it
                // left off.  The separator printed below is visible in the
                // terminal, giving the user a visual cue that the agent is
                // still working autonomously.
                messages.push(Message::assistant(Some(text), None));
                messages.push(Message::user(
                    "Continue with the next step. Do not repeat what you already did.",
                ));

                auto_continues += 1;
                eprintln!(
                    "\n  {} {}",
                    console::style("▶").cyan(),
                    console::style("auto-continuing…").dim(),
                );
                continue;
            }
            messages.push(Message::assistant(
                response.content.clone(),
                response.tool_calls.clone(),
            ));
            let tool_calls = response.tool_calls.unwrap_or_default();
            let to_execute = tool_calls
                .into_iter()
                .take(self.config.max_tool_calls_per_response as usize);
            for tool_call in to_execute {
                tool_calls_total += 1;
                let args = serde_json::from_str(&tool_call.function.arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                // Show the user which tool is being called, with a compact args preview.
                // This is printed to stderr so it doesn't interfere with captured output
                // in tests, while still appearing correctly in a real terminal session.
                let args_preview = format_args_preview(&tool_call.function.arguments);
                eprintln!(
                    "  {} {} {}  {}",
                    console::style("→").cyan().dim(),
                    console::style(&tool_call.function.name).cyan(),
                    console::style("(").dim(),
                    console::style(&args_preview).dim(),
                );

                // ── Destructive tool call confirmation ─────────────────────────────────
                // When `ctx.confirm_destructive` is true (interactive REPL without --yes),
                // check if this call looks potentially dangerous.  If so, prompt the user.
                // On 'n' / Enter, feed a synthetic "denied" tool result back so the LLM
                // can adapt its plan rather than getting confused by a missing result.
                if ctx.confirm_destructive
                    && is_destructive_call(&tool_call.function.name, &args, &ctx.working_dir)
                {
                    if !prompt_confirm(&tool_call.function.name, &args_preview).await {
                        eprintln!(
                            "  {} {}",
                            console::style("✗ skipped").red(),
                            console::style("(denied by user)").dim(),
                        );
                        // Feed a synthetic tool result so the LLM knows this call
                        // was not executed.  It can then adjust its plan.
                        messages.push(Message::tool(
                            tool_call.id.clone(),
                            "Tool call was denied by the user. Do not retry this specific operation. Ask the user how they would like to proceed instead.".to_string(),
                        ));
                        continue;
                    }
                }

                let result = if let Some(tool) = tools.get(&tool_call.function.name) {
                    match tool.execute(args, ctx).await {
                        Ok(r) => r,
                        Err(e) => crate::tools::ToolResult {
                            output: format!("Tool execution error: {}", e),
                            is_error: true,
                        },
                    }
                } else {
                    crate::tools::ToolResult {
                        output: format!("Tool '{}' not found", tool_call.function.name),
                        is_error: true,
                    }
                };

                // Show brief result so the user can see progress.
                let result_preview: String = result.output.lines().next()
                    .unwrap_or("")
                    .chars().take(120).collect();
                if result.is_error {
                    eprintln!(
                        "  {} {}",
                        console::style("← error:").red().dim(),
                        console::style(&result_preview).red().dim(),
                    );
                } else {
                    eprintln!(
                        "  {} {}",
                        console::style("←").dim(),
                        console::style(&result_preview).dim(),
                    );
                }

                messages.push(Message::tool(tool_call.id.clone(), result.output));
            }
            iterations += 1;
            if iterations >= self.config.max_iterations {
                let warning = format!(
                    "Reached maximum iterations ({}). Stopping.",
                    self.config.max_iterations
                );
                tracing::warn!("{}", warning);
                return Ok(AgentResult {
                    final_message: warning,
                    iterations,
                    tool_calls_total,
                    auto_continues,
                });
            }
        }
    }
}

/// Build ToolDefinition list from registry (OpenAI format)
fn build_tool_definitions(tools: &ToolRegistry) -> Vec<ToolDefinition> {
    tools
        .list_definitions()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect()
}

/// Check if the LLM's text response indicates the entire task is complete.
/// The system prompt instructs the LLM to output `[TASK_COMPLETE]` on its own
/// line at the very end of its final summary.  We also accept common variations
/// like casing differences and surrounding whitespace.
///
/// Returns `true` if the text contains the completion marker, meaning the
/// auto-continue loop should stop and return the result to the user.
fn looks_like_task_complete(text: &str) -> bool {
    // Primary check: the exact marker the system prompt requests.
    let lower = text.to_lowercase();
    if lower.contains("[task_complete]") {
        return true;
    }
    // Fallback heuristics for models that rephrase the marker.
    // These are intentionally conservative — we'd rather auto-continue
    // one extra time than stop prematurely.
    false
}

/// Format a compact, single-line preview of JSON tool arguments for display.
/// Shows the most important field values, truncated to keep output readable.
fn format_args_preview(arguments: &str) -> String {
    // Parse the JSON args and pick out key fields for display.
    let v: serde_json::Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(_) => return arguments.chars().take(80).collect(),
    };
    if let Some(obj) = v.as_object() {
        // Priority display fields: command/path/pattern are the most meaningful
        let priority = ["command", "path", "pattern", "old_string", "content"];
        for key in &priority {
            if let Some(val) = obj.get(*key) {
                let s = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                // Take first line only, then truncate
                let first_line: String = s.lines().next().unwrap_or("").chars().take(80).collect();
                return format!("{}: {}", key, first_line);
            }
        }
        // Fallback: show all keys joined
        let keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        return keys.join(", ");
    }
    // Scalar — just show it
    arguments.chars().take(80).collect()
}

// ─── Destructive tool call detection + user confirmation ─────────────────────

/// Returns true if this tool call should be considered "potentially destructive"
/// and therefore require user confirmation when `ctx.confirm_destructive` is set.
///
/// Heuristics:
/// - `bash` with commands containing deletion/overwrite keywords
/// - `file_write` when the target file already exists on disk (overwrite)
///
/// Conservative on false-negatives: it is better to ask one extra time than to
/// silently delete something important.  The user can always pass --yes to skip.
fn is_destructive_call(tool_name: &str, args: &serde_json::Value, working_dir: &std::path::Path) -> bool {
    match tool_name {
        "bash" => {
            // Inspect the command string for patterns that typically destroy data.
            let cmd = args["command"].as_str().unwrap_or("");
            // Use word-boundary-style checks: keyword followed by space/tab/end,
            // or preceded by space/newline/semicolon/pipe/ampersand, to avoid
            // false-positives on words like "remove_prefix" or filenames.
            let dangerous_patterns: &[&str] = &[
                "rm ", "rm\t", "rm\n",      // rm with args
                "rmdir ", "rmdir\t",        // rmdir
                "dd ", "dd\t",              // dd (disk dump — destroys devices)
                "shred ", "shred\t",        // secure delete
                "wipefs ", "wipefs\t",      // wipe filesystem
                "mkfs",                     // format filesystem
                "truncate ", "truncate\t",  // truncate file
                ":> ",                      // shell truncation idiom
                "git reset --hard",         // destructive git operations
                "git clean -f",             // remove untracked files
                "git push --force",         // force-push
                "drop table", "DROP TABLE", // SQL drops
                "drop database", "DROP DATABASE",
            ];
            dangerous_patterns.iter().any(|p| cmd.contains(p))
        }
        "file_write" => {
            // Ask when overwriting an existing file (not creating a new one).
            if let Some(path_str) = args["path"].as_str() {
                // Resolve relative to working dir
                let full = if std::path::Path::new(path_str).is_absolute() {
                    std::path::PathBuf::from(path_str)
                } else {
                    working_dir.join(path_str)
                };
                full.exists()
            } else {
                false
            }
        }
        // file_edit has an old_string guard so it is self-confirming.
        // file_read, glob_search, grep_search are read-only.
        _ => false,
    }
}

/// Prompt the user for confirmation before a destructive tool call.
/// Returns true if the user confirmed (typed 'y' or 'Y'), false otherwise.
///
/// Uses tokio's blocking task so the async runtime is not blocked while
/// waiting for stdin.
async fn prompt_confirm(tool_name: &str, args_preview: &str) -> bool {
    use std::io::Write;
    // Print the warning prompt to stderr (same stream as tool-call output)
    eprint!(
        "  {} {} {}  {}  {} ",
        console::style("⚠").yellow().bold(),
        console::style(tool_name).yellow(),
        console::style("(").dim(),
        console::style(args_preview).yellow(),
        console::style("[y/N]:").dim(),
    );
    let _ = std::io::stderr().flush();

    // Read one line from stdin in a blocking thread (avoids blocking the async executor).
    let answer = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap_or(0);
        line.trim().to_lowercase()
    })
    .await
    .unwrap_or_default();

    answer == "y"
}

// ─── Plan mode system prompt ──────────────────────────────────────────────────

pub const PLAN_SYSTEM_PROMPT: &str = "\
You are a planning assistant. Help the user think through their task carefully. \
Ask clarifying questions, read files to understand the codebase, and produce a \
clear step-by-step plan. Do NOT write, edit, or delete any files. Do NOT run \
shell commands that modify state. When the plan is ready, tell the user to \
type /act to switch to Act mode and execute the plan. \
IMPORTANT: When you need to ask the user a question, use the `question` tool. \
Provide 2-5 concise options. Ask ONE question at a time — wait for the answer \
before asking anything else. Never list multiple questions in your response.";

/// Plan conversation turn: one LLM call with the `question` tool available.
/// If the LLM calls the `question` tool, we execute it (which renders a
/// dialoguer::Select in the terminal), feed the result back, and call the LLM
/// again so it can continue based on the user's answer.
///
/// Returns the final assistant reply text (after all question-tool loops).
pub async fn run_plan_turn(
    messages: &[Message],
    llm: &dyn LlmProvider,
    tools: &crate::tools::ToolRegistry,
    tool_ctx: &crate::tools::ToolContext,
) -> Result<String> {
    // Build the message list with plan system prompt prepended.
    // Keep full history so the LLM has context.
    let mut plan_messages: Vec<Message> = Vec::new();
    plan_messages.push(Message::system(PLAN_SYSTEM_PROMPT));
    // Append conversation history (skip any existing system message at index 0)
    for msg in messages {
        if msg.role == crate::llm::Role::System {
            continue; // replace with plan system prompt
        }
        plan_messages.push(msg.clone());
    }
    truncate_messages(&mut plan_messages, 400_000);

    // Build tool definitions — in Plan mode we only expose the `question` tool
    // so the LLM can ask the user structured questions but cannot modify files.
    let question_tool_defs: Vec<ToolDefinition> = tools
        .list_definitions()
        .into_iter()
        .filter_map(|v| {
            // Only include the "question" tool in Plan mode
            if v["function"]["name"].as_str() == Some("question") {
                serde_json::from_value(v).ok()
            } else {
                None
            }
        })
        .collect();

    // Maximum rounds of question-tool interaction to prevent infinite loops.
    // Each round = one LLM call that results in a question tool call.
    const MAX_QUESTION_ROUNDS: u32 = 5;
    let mut rounds = 0u32;

    loop {
        let response = llm.chat_completion(&plan_messages, &question_tool_defs).await?;

        // Check if the LLM made any tool calls (i.e. wants to ask a question).
        let has_tool_calls = response
            .tool_calls
            .as_ref()
            .map(|tc| !tc.is_empty())
            .unwrap_or(false);

        if !has_tool_calls {
            // No tool calls — this is a plain text response. Return it.
            let reply = response
                .content
                .unwrap_or_else(|| "(no response)".to_string());
            return Ok(reply);
        }

        // The LLM wants to ask a question. Push the assistant message with
        // tool_calls so the conversation stays well-formed for the next round.
        plan_messages.push(Message::assistant(
            response.content.clone(),
            response.tool_calls.clone(),
        ));

        // Execute each tool call (should only be `question`, but we handle all).
        let tool_calls = response.tool_calls.unwrap_or_default();
        for tool_call in &tool_calls {
            let args: serde_json::Value =
                serde_json::from_str(&tool_call.function.arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

            let result = if let Some(tool) = tools.get(&tool_call.function.name) {
                match tool.execute(args, tool_ctx).await {
                    Ok(r) => r,
                    Err(e) => crate::tools::ToolResult {
                        output: format!("Tool execution error: {}", e),
                        is_error: true,
                    },
                }
            } else {
                crate::tools::ToolResult {
                    output: format!("Tool '{}' not found", tool_call.function.name),
                    is_error: true,
                }
            };

            // Push the tool result as a message so the LLM sees the user's answer.
            plan_messages.push(Message::tool(
                tool_call.id.clone(),
                result.output,
            ));
        }

        rounds += 1;
        if rounds >= MAX_QUESTION_ROUNDS {
            return Ok("(Reached maximum question rounds. Please type your response directly.)".to_string());
        }

        // Loop back to call the LLM again — it now has the user's answer(s)
        // and can either ask another question or produce its final response.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use crate::llm::{FunctionCall, LlmResponse, Message, Role, ToolCall, ToolDefinition};
    use crate::tools::{Tool, ToolContext, ToolRegistry, ToolResult};
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    // MockLlmProvider: returns a sequence of canned responses
    struct MockLlm {
        responses: Arc<Mutex<Vec<LlmResponse>>>,
    }
    impl MockLlm {
        fn new(responses: Vec<LlmResponse>) -> Self {
            MockLlm {
                responses: Arc::new(Mutex::new(responses)),
            }
        }
    }
    #[async_trait]
    impl LlmProvider for MockLlm {
        async fn chat_completion(
            &self,
            _msgs: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<LlmResponse> {
            let mut r = self.responses.lock().unwrap();
            if r.is_empty() {
                return Ok(LlmResponse {
                    content: Some("done [TASK_COMPLETE]".to_string()),
                    tool_calls: None,
                });
            }
            Ok(r.remove(0))
        }
    }

    // MockTool: just echoes back the args
    struct EchoTool;
    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echo the input"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"object","properties":{"text":{"type":"string"}}})
        }
        async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext) -> Result<ToolResult> {
            Ok(ToolResult {
                output: args["text"].as_str().unwrap_or("").to_string(),
                is_error: false,
            })
        }
    }

    fn make_tool_call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        }
    }

    fn test_ctx() -> ToolContext {
        ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            sandbox_enabled: false,
            confirm_destructive: false,
        }
    }

    fn test_config() -> AgentConfig {
        AgentConfig {
            max_iterations: 5,
            max_tool_calls_per_response: 3,
            max_auto_continues: 5,
        }
    }

    #[tokio::test]
    async fn test_coder_simple_task() {
        let llm = MockLlm::new(vec![
            LlmResponse {
                content: None,
                tool_calls: Some(vec![make_tool_call("call1", "echo", r#"{"text":"hello"}"#)]),
            },
            LlmResponse {
                content: Some("Task complete! [TASK_COMPLETE]".to_string()),
                tool_calls: None,
            },
        ]);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let agent = CoderAgent::new(test_config());
        let mut messages = vec![
            Message::system(agent.system_prompt()),
            Message::user("say hello"),
        ];
        let result = agent
            .run(&mut messages, &registry, &llm, &test_ctx())
            .await
            .unwrap();
        assert_eq!(result.iterations, 1);
        assert_eq!(result.tool_calls_total, 1);
        assert!(result.final_message.contains("Task complete"));
    }

    #[tokio::test]
    async fn test_coder_max_iterations() {
        let responses: Vec<LlmResponse> = (0..20)
            .map(|_| LlmResponse {
                content: None,
                tool_calls: Some(vec![make_tool_call("call1", "echo", r#"{"text":"loop"}"#)]),
            })
            .collect();
        let llm = MockLlm::new(responses);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let config = AgentConfig {
            max_iterations: 3,
            max_tool_calls_per_response: 1,
            max_auto_continues: 20,
        };
        let agent = CoderAgent::new(config);
        let mut messages = vec![
            Message::system(agent.system_prompt()),
            Message::user("loop forever"),
        ];
        let result = agent
            .run(&mut messages, &registry, &llm, &test_ctx())
            .await
            .unwrap();
        assert_eq!(result.iterations, 3);
        assert!(result.final_message.contains("maximum iterations"));
    }

    #[tokio::test]
    async fn test_coder_max_tool_calls_per_response() {
        let tool_calls: Vec<ToolCall> = (0..5)
            .map(|i| make_tool_call(&format!("call{}", i), "echo", r#"{"text":"x"}"#))
            .collect();
        let llm = MockLlm::new(vec![
            LlmResponse {
                content: None,
                tool_calls: Some(tool_calls),
            },
            LlmResponse {
                content: Some("done [TASK_COMPLETE]".to_string()),
                tool_calls: None,
            },
        ]);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let config = AgentConfig {
            max_iterations: 5,
            max_tool_calls_per_response: 2,
            max_auto_continues: 20,
        };
        let agent = CoderAgent::new(config);
        let mut messages = vec![
            Message::system(agent.system_prompt()),
            Message::user("many tools"),
        ];
        let result = agent
            .run(&mut messages, &registry, &llm, &test_ctx())
            .await
            .unwrap();
        assert_eq!(result.tool_calls_total, 2);
    }

    #[tokio::test]
    async fn test_coder_tool_error_recovery() {
        struct ErrorTool;
        #[async_trait::async_trait]
        impl Tool for ErrorTool {
            fn name(&self) -> &str {
                "fail"
            }
            fn description(&self) -> &str {
                "Always fails"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({"type":"object"})
            }
            async fn execute(&self, _: serde_json::Value, _: &ToolContext) -> Result<ToolResult> {
                Ok(ToolResult {
                    output: "something went wrong".to_string(),
                    is_error: true,
                })
            }
        }
        let llm = MockLlm::new(vec![
            LlmResponse {
                content: None,
                tool_calls: Some(vec![make_tool_call("e1", "fail", "{}")]),
            },
            LlmResponse {
                content: Some("recovered [TASK_COMPLETE]".to_string()),
                tool_calls: None,
            },
        ]);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(ErrorTool));
        let agent = CoderAgent::new(test_config());
        let mut messages = vec![
            Message::system(agent.system_prompt()),
            Message::user("test error"),
        ];
        let result = agent
            .run(&mut messages, &registry, &llm, &test_ctx())
            .await
            .unwrap();
        assert!(result.final_message.contains("recovered"));
        let tool_result_msg = messages.iter().find(|m| m.role == Role::Tool);
        assert!(tool_result_msg.is_some());
        assert!(tool_result_msg
            .unwrap()
            .content
            .as_ref()
            .unwrap()
            .contains("something went wrong"));
    }

    #[tokio::test]
    async fn test_context_truncation() {
        let long_content = "a".repeat(10_000);
        let mut messages: Vec<Message> = vec![Message::system("system")];
        messages.push(Message::user("first user message"));
        for _ in 0..50 {
            messages.push(Message::user(long_content.clone()));
            messages.push(Message::assistant(Some(long_content.clone()), None));
        }
        let original_len = messages.len();
        truncate_messages(&mut messages, 50_000);
        assert!(
            messages.len() < original_len,
            "Expected truncation but got {} msgs",
            messages.len()
        );
        assert_eq!(messages[0].role, Role::System);
    }

    // ── Tests for run_plan_turn ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_plan_turn_plain_text() {
        // When the LLM returns plain text (no tool calls), run_plan_turn
        // should return that text directly.
        let llm = MockLlm::new(vec![
            LlmResponse {
                content: Some("Here is your plan: step 1, step 2.".to_string()),
                tool_calls: None,
            },
        ]);
        let registry = ToolRegistry::new(); // empty — no tools needed
        let ctx = test_ctx();
        let messages = vec![Message::user("Help me plan")];

        let result = run_plan_turn(&messages, &llm, &registry, &ctx)
            .await
            .unwrap();

        assert!(result.contains("plan"));
        assert!(result.contains("step 1"));
    }

    #[tokio::test]
    async fn test_plan_turn_tool_call_loop() {
        // Simulates the LLM calling a mock "question" tool, receiving the
        // result, and then producing a final text response.
        //
        // We use a simple mock tool named "question" that returns a canned
        // answer (avoiding the real dialoguer::Select which needs a TTY).
        struct MockQuestionTool;
        #[async_trait]
        impl Tool for MockQuestionTool {
            fn name(&self) -> &str { "question" }
            fn description(&self) -> &str { "Mock question" }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(
                &self,
                _args: serde_json::Value,
                _ctx: &ToolContext,
            ) -> Result<ToolResult> {
                Ok(ToolResult {
                    output: "User selected: Option A".to_string(),
                    is_error: false,
                })
            }
        }

        let llm = MockLlm::new(vec![
            // Round 1: LLM calls the question tool
            LlmResponse {
                content: Some("Let me ask you something.".to_string()),
                tool_calls: Some(vec![make_tool_call(
                    "q1",
                    "question",
                    r#"{"question":"Which approach?","options":[{"label":"A","description":"fast"},{"label":"B","description":"safe"}]}"#,
                )]),
            },
            // Round 2: LLM sees the answer and produces final text
            LlmResponse {
                content: Some("Great, you chose Option A. Here is the plan.".to_string()),
                tool_calls: None,
            },
        ]);

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockQuestionTool));
        let ctx = test_ctx();
        let messages = vec![Message::user("Plan my refactor")];

        let result = run_plan_turn(&messages, &llm, &registry, &ctx)
            .await
            .unwrap();

        // The final response should be from round 2
        assert!(result.contains("Option A"));
        assert!(result.contains("plan"));
    }

    #[tokio::test]
    async fn test_plan_turn_max_question_rounds() {
        // Verify that run_plan_turn stops after MAX_QUESTION_ROUNDS even if
        // the LLM keeps calling the question tool indefinitely.
        struct MockQuestionTool;
        #[async_trait]
        impl Tool for MockQuestionTool {
            fn name(&self) -> &str { "question" }
            fn description(&self) -> &str { "Mock question" }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(
                &self,
                _args: serde_json::Value,
                _ctx: &ToolContext,
            ) -> Result<ToolResult> {
                Ok(ToolResult {
                    output: "User selected: something".to_string(),
                    is_error: false,
                })
            }
        }

        // Return 10 responses that ALL contain tool calls — the function
        // should bail after MAX_QUESTION_ROUNDS (5).
        let responses: Vec<LlmResponse> = (0..10)
            .map(|i| LlmResponse {
                content: Some(format!("Question {}", i)),
                tool_calls: Some(vec![make_tool_call(
                    &format!("q{}", i),
                    "question",
                    r#"{"question":"Again?","options":[{"label":"Y","description":"yes"}]}"#,
                )]),
            })
            .collect();

        let llm = MockLlm::new(responses);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockQuestionTool));
        let ctx = test_ctx();
        let messages = vec![Message::user("infinite questions")];

        let result = run_plan_turn(&messages, &llm, &registry, &ctx)
            .await
            .unwrap();

        // Should contain the safety message about max rounds
        assert!(result.contains("maximum question rounds"));
    }
}
