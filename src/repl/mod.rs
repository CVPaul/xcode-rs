// src/repl/mod.rs
// REPL loop and related state for xcodeai interactive mode.
// Extracted from main.rs for clarity and modularity.
//
// This module contains:
// - REPL loop (repl_command)
// - REPL state enums and structs
// - Provider presets for /connect
// - Session picker and connect menu
//
// For Rust learners: This file demonstrates how to organize a REPL loop and related state in a separate module. It also shows how to use enums and structs to manage interactive CLI state.

use crate::context::AgentContext;
use crate::llm::openai::COPILOT_API_BASE;
use crate::session::{Session, SessionStore};
use crate::ui::{err, info, ok, print_banner, print_separator, warn};
use anyhow::Result;
use console::style;
use rustyline::DefaultEditor;
use std::path::PathBuf;

pub mod commands;
use commands::{handle_command, ReplState};

#[derive(Clone, Copy, PartialEq)]
pub enum ReplMode {
    Act,
    Plan,
}

/// Built-in provider presets shown in /connect menu
pub struct ProviderPreset {
    pub label: &'static str,
    pub api_base: &'static str,
    pub needs_key: bool,
}

pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        label: "GitHub Copilot       (subscription, no key needed)",
        api_base: "copilot",
        needs_key: false,
    },
    ProviderPreset {
        label: "OpenAI               https://api.openai.com/v1",
        api_base: "https://api.openai.com/v1",
        needs_key: true,
    },
    ProviderPreset {
        label: "Anthropic            (claude-3-5-sonnet-20241022)",
        api_base: "anthropic",
        needs_key: true,
    },
    ProviderPreset {
        label: "Google Gemini        (gemini-2.0-flash)",
        api_base: "gemini",
        needs_key: true,
    },
    ProviderPreset {
        label: "DeepSeek             https://api.deepseek.com/v1",
        api_base: "https://api.deepseek.com/v1",
        needs_key: true,
    },
    ProviderPreset {
        label: "Qwen (Alibaba Cloud) https://dashscope.aliyuncs.com/compatible-mode/v1",
        api_base: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        needs_key: true,
    },
    ProviderPreset {
        label: "GLM (Zhipu AI)       https://open.bigmodel.cn/api/paas/v4",
        api_base: "https://open.bigmodel.cn/api/paas/v4",
        needs_key: true,
    },
    ProviderPreset {
        label: "Ollama (local)       http://localhost:11434/v1",
        api_base: "http://localhost:11434/v1",
        needs_key: false,
    },
    ProviderPreset {
        label: "Custom URL…",
        api_base: "",
        needs_key: true,
    },
];

/// Show a /command picker. Returns the chosen command string (e.g. "/session"), or None if the user pressed Esc / Ctrl-C.
pub fn show_command_menu() -> Option<String> {
    commands::show_command_menu()
}

pub enum SessionPickResult {
    /// User chose to start a brand-new session
    NewSession,
    /// User selected an existing session to resume
    Resume(Session),
    /// User cancelled (Esc)
    Cancelled,
}

/// Show an interactive session picker.
pub fn session_picker(store: &SessionStore) -> SessionPickResult {
    // Implementation copied from main.rs
    use dialoguer::{theme::ColorfulTheme, Select};
    let sessions = match store.list_sessions(30) {
        Ok(s) => s,
        Err(e) => {
            err(&format!("Failed to load sessions: {:#}", e));
            return SessionPickResult::Cancelled;
        }
    };
    let mut labels: Vec<String> = vec![format!("  {} New session", style("+").green().bold())];
    for s in &sessions {
        let date = s.updated_at.format("%Y-%m-%d %H:%M").to_string();
        let title = s.title.as_deref().unwrap_or("(untitled)");
        let short_title = if title.len() > 50 {
            format!("{}…", &title[..49])
        } else {
            title.to_string()
        };
        labels.push(format!("  {}  {}", style(date).dim(), short_title));
    }
    println!();
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Session")
        .items(&labels)
        .default(0)
        .interact_opt();
    println!();
    match selection {
        Ok(Some(0)) => SessionPickResult::NewSession,
        Ok(Some(i)) => SessionPickResult::Resume(sessions[i - 1].clone()),
        _ => SessionPickResult::Cancelled,
    }
}

/// Interactive /connect menu — lets user pick a provider from a numbered list.
pub fn connect_menu(rl: &mut DefaultEditor) -> Option<(String, String)> {
    use dialoguer::{theme::ColorfulTheme, Select};
    println!();
    info("Select a provider:");
    println!();
    let labels: Vec<&str> = PROVIDER_PRESETS.iter().map(|p| p.label).collect();
    let selection = Select::with_theme(&ColorfulTheme::default())
        .items(&labels)
        .default(0)
        .interact_opt();
    let idx = match selection {
        Ok(Some(i)) => i,
        _ => {
            info("Cancelled.");
            return None;
        }
    };
    let preset = &PROVIDER_PRESETS[idx];
    println!();
    let api_base = if preset.api_base.is_empty() {
        match rl.readline(&format!("{} ", style("  API base URL:").cyan())) {
            Ok(line) => {
                let url = line.trim().to_string();
                if url.is_empty() {
                    info("Cancelled.");
                    return None;
                }
                url
            }
            Err(_) => return None,
        }
    } else {
        preset.api_base.to_string()
    };
    if api_base == COPILOT_API_BASE {
        return Some(("copilot_do_login".to_string(), String::new()));
    }
    let api_key = if preset.needs_key {
        match rl.readline(&format!("{} ", style("  API key:").cyan())) {
            Ok(line) => {
                let k = line.trim().to_string();
                if k.is_empty() {
                    warn("No key entered — provider set, but API calls will fail without a key.");
                }
                k
            }
            Err(_) => return None,
        }
    } else {
        String::new()
    };
    Some((api_base, api_key))
}

/// Main REPL loop for interactive mode.
#[allow(clippy::too_many_arguments)]
pub async fn repl_command(
    project: Option<PathBuf>,
    no_sandbox: bool,
    model: Option<String>,
    provider_url: Option<String>,
    api_key: Option<String>,
    yes: bool,
    no_agents_md: bool,
    // When true, enables compact mode for this REPL session.
    compact: bool,
    // When true, disable markdown rendering of agent output in the terminal.
    no_markdown: bool,
) -> Result<()> {
    // ─── REPL loop implementation ───────────────────────────────────────────────
    use crate::agent::coder::run_plan_turn;
    use crate::agent::director::Director;
    use crate::agent::Agent;
    use crate::auth;
    use crate::context::update_session_title;
    use crate::llm;
    use crate::session::auto_title;
    use rustyline::error::ReadlineError;
    use std::sync::mpsc;
    use std::time::Duration;

    let io: std::sync::Arc<dyn crate::io::AgentIO> = if yes {
        // --yes flag: auto-approve all destructive calls, no prompts.
        std::sync::Arc::new(crate::io::AutoApproveIO)
    } else {
        // Interactive mode: prompt before destructive operations.
        std::sync::Arc::new(crate::io::terminal::TerminalIO { no_markdown })
    };
    let mut ctx = AgentContext::new(
        project,
        no_sandbox,
        model,
        provider_url,
        api_key,
        compact,
        io,
    )
    .await?;
    let mut sess = ctx.store.create_session(Some("REPL session"))?;

    // Auth status string with colour
    let auth_status: String = if ctx.llm.is_copilot() {
        match auth::CopilotOAuthToken::load() {
            Ok(Some(_)) => style("GitHub Copilot  ✓ authenticated").green().to_string(),
            _ => style("GitHub Copilot  ✗ not authenticated — run /login")
                .yellow()
                .to_string(),
        }
    } else if ctx.config.provider.api_key.is_empty() {
        style("no API key — run /connect to configure")
            .yellow()
            .to_string()
    } else {
        format!(
            "{} {}",
            style("provider:").dim(),
            style(&ctx.config.provider.api_base).cyan()
        )
    };

    print_banner(
        env!("CARGO_PKG_VERSION"),
        &ctx.config.model,
        &ctx.project_dir.display().to_string(),
        &auth_status,
    );

    // Readline editor with persistent history
    let mut rl = DefaultEditor::new()?;
    let history_path = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("xcode")
        .join("repl_history.txt");

    if let Some(parent) = history_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = rl.load_history(&history_path);

    let director = Director::new(ctx.config.agent.clone());
    let mut mode = ReplMode::Act;

    // When the command-menu returns a choice, we store it here instead of
    // calling rl.readline() again.  At the top of every loop iteration we
    // drain this slot first; only if it is empty do we ask readline for
    // actual user input.  This eliminates the ~100-line duplicate dispatch
    // block that previously lived inside the menu handler.
    let mut pending_line: Option<String> = None;

    // Plan mode conversation history (discuss, no tools)
    let mut conversation_messages: Vec<llm::Message> = Vec::new();

    // Act mode conversation history — seeded with CoderAgent system prompt.
    // Persists across turns so the agent has full context of previous work.
    let coder_system_prompt = {
        use crate::agent::agents_md::load_agents_md;
        use crate::agent::coder::CoderAgent;
        // Load AGENTS.md unless --no-agents-md was passed.
        // The content is prepended to the system prompt for the entire REPL session.
        let agents_md = if no_agents_md {
            None
        } else {
            load_agents_md(&ctx.project_dir)
        };
        if agents_md.is_some() {
            // We can't easily .await here inside a sync block, so we print directly.
            // show_status uses eprintln internally; this is the REPL init path.
            eprintln!("  \u{1F4CB} Loaded project rules from AGENTS.md");
        }
        CoderAgent::new_with_agents_md(ctx.config.agent.clone(), agents_md).system_prompt()
    };
    let mut act_messages: Vec<llm::Message> = vec![llm::Message::system(&coder_system_prompt)];

    // ── Undo stack ────────────────────────────────────────────────────────────
    // The undo history is persisted in the SQLite DB (undo_history table).
    // Before each Act-mode run we push a unique stash label; after each run we
    // record the label in the DB if git actually created a stash entry.
    // /undo pops entries from the DB and calls `git stash pop stash@{N}` to
    // restore the matching stash.
    //
    // If the project is not inside a git repo (or git is not installed) the
    // commands fail silently and undo is simply unavailable.
    //
    // Session-level token tracker — accumulates usage across all task runs this REPL session.
    let mut session_tracker = crate::tracking::SessionTracker::new(ctx.config.model.clone());
    loop {
        let prompt = match mode {
            ReplMode::Act => format!("{} ", style("xcodeai›").cyan().bold()),
            ReplMode::Plan => format!("{} ", style("[plan] xcodeai›").yellow().bold()),
        };

        // Drain a pending command from the menu, or read a new line from the user.
        let line = if let Some(p) = pending_line.take() {
            p
        } else {
            match rl.readline(&prompt) {
                Ok(raw) => {
                    let trimmed = raw.trim().to_string();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let _ = rl.add_history_entry(&trimmed);
                    trimmed
                }
                Err(ReadlineError::Interrupted) => {
                    info("Ctrl-C — type /exit or press Ctrl-D to quit.");
                    continue;
                }
                Err(ReadlineError::Eof) => break,
                Err(e) => return Err(e.into()),
            }
        };

        // ── REPL commands ──────────────────────────────────────────────────
        // If the line starts with '/' or is a plain exit word, route it to
        // handle_command().  The function returns:
        //   Ok(None)          → command handled, continue the loop
        //   Ok(Some(mode))    → mode changed (not used currently, mode is mutated in place)
        //   Err(e)            → propagate fatal error
        // Special case: '/' with a menu selection needs to buffer the chosen
        // command into `pending_line` so the loop processes it next iteration.
        if line.starts_with('/')
            || matches!(
                line.as_str(),
                "exit" | "quit" | "q" | "bye" | "bye!" | "exit!" | "quit!"
            )
        {
            // handle_command mutates mode/sess/ctx/messages in place.
            // For the '/' bare-slash case, commands.rs returns the chosen label
            // via rl history — but we need pending_line.  We special-case that
            // here: if show_command_menu() returns something, buffer it.
            if line == "/"
                || (line.starts_with('/')
                    && !line.starts_with("/model")
                    && !line.starts_with("/login")
                    && !line.starts_with("/undo")
                    && !matches!(
                        line.as_str(),
                        "/plan"
                            | "/act"
                            | "/tokens"
                            | "/session"
                            | "/connect"
                            | "/clear"
                            | "/help"
                            | "/logout"
                            | "/exit"
                            | "/quit"
                            | "/q"
                    ))
            {
                // Unknown /xxx command — show the interactive picker.
                if let Some(chosen) = show_command_menu() {
                    let _ = rl.add_history_entry(&chosen);
                    pending_line = Some(chosen);
                }
                continue;
            }
            // Clone the session ID before constructing ReplState so that
            // Rust doesn't see simultaneous mutable (&mut sess) and
            // immutable (&sess.id) borrows of the same binding.
            let sess_id = sess.id.clone();
            handle_command(
                &line,
                &mut ReplState {
                    mode: &mut mode,
                    sess: &mut sess,
                    ctx: &mut ctx,
                    rl: &mut rl,
                    conversation_messages: &mut conversation_messages,
                    act_messages: &mut act_messages,
                    coder_system_prompt: &coder_system_prompt,
                    session_id: &sess_id,
                    session_tracker: &session_tracker,
                },
            )
            .await?;
            continue;
        }
        // ── Lazy auth guard ────────────────────────────────────────
        if !ctx.llm.is_copilot() && ctx.config.provider.api_key.is_empty() {
            err("No API key configured. Run /connect to pick a provider.");
            continue;
        }
        if ctx.llm.is_copilot() {
            if let Ok(None) | Err(_) = auth::CopilotOAuthToken::load() {
                err("Not authenticated with GitHub Copilot. Run /login first.");
                continue;
            }
        }

        // ── Save message & run agent ───────────────────────────────
        ctx.store
            .add_message(&sess.id, &llm::Message::user(&line))?;
        let title = auto_title(&line);
        let _ = ctx.store.update_session_timestamp(&sess.id);
        let _ = update_session_title(&ctx.store, &sess.id, &title);

        println!();
        match mode {
            ReplMode::Act => {
                // Append the user message to Act-mode history before calling the agent.
                act_messages.push(llm::Message::user(&line));

                // ── Pre-run git stash (enables /undo) ────────────────
                // Use a unique UUID-based label so we can identify this
                // specific stash later, even if the user created their own
                // stashes in the meantime.
                let stash_ref = format!("xcodeai-undo-{}", uuid::Uuid::new_v4());
                // Capture the first 80 chars of the user's message for the
                // undo history description shown in `/undo list`.
                let short_desc: String = line.chars().take(80).collect();
                let (stash_tx, stash_rx) = mpsc::channel::<std::io::Result<std::process::Output>>();
                {
                    let project_dir_clone = ctx.project_dir.clone();
                    // Clone stash_ref into the thread so we can move it.
                    let stash_ref_clone = stash_ref.clone();
                    std::thread::spawn(move || {
                        let out = std::process::Command::new("git")
                            .args(["stash", "push", "-m", &stash_ref_clone])
                            .current_dir(&project_dir_clone)
                            .output();
                        let _ = stash_tx.send(out);
                    });
                }

                // Stash is now running concurrently.  Start the agent immediately.
                let result = director
                    .execute(
                        &mut act_messages,
                        ctx.registry.as_ref(),
                        ctx.llm.as_ref(),
                        &ctx.tool_ctx,
                    )
                    .await;

                // Agent finished.  Collect the stash result (with a 30-second grace period).
                let stash_was_created = match stash_rx.recv_timeout(Duration::from_secs(30)) {
                    Ok(Ok(out)) if out.status.success() => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        !stdout.contains("No local changes to save")
                    }
                    _ => false,
                };
                // Persist the undo entry in the DB so the user can restore
                // this state later with /undo (or /undo N).
                if stash_was_created {
                    let _ = ctx.store.push_undo(&sess.id, &stash_ref, &short_desc);
                    let _ = ctx
                        .store
                        .trim_undo_history(&sess.id, crate::session::store::MAX_UNDO_HISTORY);
                }

                println!();

                match result {
                    Ok(mut agent_result) => {
                        ctx.store.add_message(
                            &sess.id,
                            &llm::Message::assistant(
                                Some(agent_result.final_message.clone()),
                                None,
                            ),
                        )?;
                        ctx.store.update_session_timestamp(&sess.id)?;

                        print_separator("done");

                        // Fill in model name now (CoderAgent left it empty) so
                        // cost estimation in summary_line() can look up the price.
                        agent_result.tracker.model = ctx.config.model.clone();

                        // Merge this task's turns into the session-level tracker so
                        // /tokens shows cumulative stats across all runs this session.
                        for turn in &agent_result.tracker.turns {
                            session_tracker.record(Some(&crate::llm::Usage {
                                prompt_tokens: turn.prompt_tokens,
                                completion_tokens: turn.completion_tokens,
                                total_tokens: turn.prompt_tokens + turn.completion_tokens,
                            }));
                        }
                        // Keep the session tracker's model current (user may have used /model).
                        session_tracker.model = ctx.config.model.clone();

                        // Persist the accumulated token counts for this task run to SQLite.
                        // Non-fatal — don't crash on DB write failure, just silently ignore.
                        let _ = ctx.store.update_session_tokens(
                            &sess.id,
                            agent_result.tracker.total_prompt_tokens(),
                            agent_result.tracker.total_completion_tokens(),
                        );

                        // Build a stats line showing iterations, tool calls, and auto-continues.
                        let mut stats_parts: Vec<String> = vec![
                            format!("{} iterations", agent_result.iterations),
                            format!("{} tool calls", agent_result.tool_calls_total),
                        ];
                        if agent_result.auto_continues > 0 {
                            stats_parts
                                .push(format!("{} auto-continues", agent_result.auto_continues));
                        }
                        // Append token summary when the provider returned usage data.
                        let token_summary = agent_result.tracker.summary_line();
                        if !token_summary.is_empty() {
                            stats_parts.push(token_summary);
                        }
                        let stats_str = stats_parts
                            .iter()
                            .map(|s| format!("{}", style(s).dim()))
                            .collect::<Vec<_>>()
                            .join(&format!("  {}  ", style("·").dim()));

                        println!(
                            "   {} {}  {}  {}",
                            style("✓").green().bold(),
                            style("task complete").green(),
                            style("·").dim(),
                            stats_str,
                        );
                        print_separator("");

                        // ── Git diff summary ──────────────────────
                        let diff_output = std::process::Command::new("git")
                            .args(["diff", "--stat", "HEAD"])
                            .current_dir(&ctx.project_dir)
                            .output();
                        if let Ok(out) = diff_output {
                            let text = String::from_utf8_lossy(&out.stdout);
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                println!(
                                    "  {} {}",
                                    style("▸").dim(),
                                    style("git diff --stat HEAD").dim()
                                );
                                for line in trimmed.lines() {
                                    println!("   {}", style(line).dim());
                                }
                                println!();
                            }
                        }

                        println!();
                    }
                    Err(e) => {
                        act_messages.pop();
                        err(&format!("{:#}", e));
                        info("Try a different task, or type /exit to quit.");
                    }
                }
            }
            ReplMode::Plan => {
                // Add user message to plan conversation history
                conversation_messages.push(llm::Message::user(&line));

                // Disable streaming stdout so we can post-process the reply
                ctx.llm.set_stream_print(false);
                let plan_result = run_plan_turn(
                    &conversation_messages,
                    ctx.llm.as_ref(),
                    ctx.registry.as_ref(),
                    &ctx.tool_ctx,
                )
                .await;
                ctx.llm.set_stream_print(true);

                match plan_result {
                    Ok(reply) => {
                        if !reply.trim().is_empty() {
                            println!("{}", reply.trim_end());
                            println!();
                        }

                        // Save to conversation history and session store.
                        conversation_messages
                            .push(llm::Message::assistant(Some(reply.clone()), None));
                        ctx.store
                            .add_message(&sess.id, &llm::Message::assistant(Some(reply), None))?;
                        ctx.store.update_session_timestamp(&sess.id)?;
                    }
                    Err(e) => {
                        err(&format!("{:#}", e));
                        info("Plan mode error. Try again or type /act to switch back.");
                        conversation_messages.pop();
                    }
                }
            }
        }
    }

    let _ = rl.save_history(&history_path);
    println!();
    ok(&format!("Session saved: {}", style(&sess.id).dim()));
    info(&format!("xcodeai session show {}", sess.id));
    println!();

    Ok(())
}
