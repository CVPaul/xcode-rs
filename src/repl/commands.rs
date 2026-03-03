// src/repl/commands.rs
// Slash-command handlers and dispatcher for xcodeai REPL.
// Extracted from main.rs for clarity and modularity.
//
// For Rust learners: This file demonstrates how to organize command handling logic in a dedicated module.
// Each slash-command is implemented as a separate function. The dispatcher routes commands to the correct handler.

use crate::auth;
use crate::config;
use crate::llm;
use crate::repl::{ReplMode, SessionPickResult};
use crate::session::Session;
use crate::ui::{err, info, ok, warn};
use anyhow::Result;
use console::{style, Style};
// rustyline is no longer used — connect_menu uses std::io::stdin().

pub struct CommandDef {
    pub cmd: &'static str,
    pub desc: &'static str,
}

pub const COMMANDS: &[CommandDef] = &[
    CommandDef {
        cmd: "/plan",
        desc: "Switch to Plan mode — discuss & clarify your task",
    },
    CommandDef {
        cmd: "/act",
        desc: "Switch to Act mode — execute tools",
    },
    CommandDef {
        cmd: "/undo",
        desc: "Undo the last Act-mode run (git stash pop)",
    },
    CommandDef {
        cmd: "/tokens",
        desc: "Show token usage for this session",
    },
    CommandDef {
        cmd: "/session",
        desc: "Browse history or start a new session",
    },
    CommandDef {
        cmd: "/connect",
        desc: "Pick a provider from a menu",
    },
    CommandDef {
        cmd: "/login",
        desc: "GitHub Copilot device-code OAuth",
    },
    CommandDef {
        cmd: "/logout",
        desc: "Remove saved Copilot credentials",
    },
    CommandDef {
        cmd: "/model",
        desc: "Switch model (interactive picker, or /model <name>)",
    },
    CommandDef {
        cmd: "/clear",
        desc: "Start a fresh session (same as New in /session)",
    },
    CommandDef {
        cmd: "/mcp",
        desc: "List connected MCP servers and their tools",
    },
    CommandDef {
        cmd: "/compact",
        desc: "Summarise context + toggle compact mode",
    },
    CommandDef {
        cmd: "/init",
        desc: "Generate AGENTS.md project rules via LLM analysis",
    },
    CommandDef {
        cmd: "/redo",
        desc: "Re-send the last user message",
    },
    CommandDef {
        cmd: "/help",
        desc: "Show all commands",
    },
    CommandDef {
        cmd: "/exit",
        desc: "Exit xcodeai",
    },
];

pub fn show_command_menu() -> Option<String> {
    use dialoguer::{theme::ColorfulTheme, Select};
    println!();
    let labels: Vec<String> = COMMANDS
        .iter()
        .map(|c| format!("{:<12}  {}", c.cmd, c.desc))
        .collect();
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Command")
        .items(&labels)
        .default(0)
        .interact_opt();
    println!();
    match selection {
        Ok(Some(i)) => Some(COMMANDS[i].cmd.to_string()),
        _ => None,
    }
}

/// Mutable REPL state passed to handle_command to keep the argument count low.
/// For Rust learners: grouping related mutable references into a struct is a
/// common pattern to avoid "too many arguments" and to keep function signatures
/// readable and easy to extend.
pub struct ReplState<'a> {
    pub mode: &'a mut ReplMode,
    pub sess: &'a mut Session,
    pub ctx: &'a mut crate::context::AgentContext,
    /// The input history owned by the REPL main loop.
    /// Passed here so /session etc can call `history.push()` if needed.
    pub history: &'a mut crate::repl::input::InputHistory,
    pub conversation_messages: &'a mut Vec<llm::Message>,
    pub act_messages: &'a mut Vec<llm::Message>,
    pub coder_system_prompt: &'a str,
    /// Used by /undo to persist and pop stash entries across runs.
    pub session_id: &'a str,
    /// Session-level token tracker (accumulates across multiple task runs).
    /// Passed by reference so /tokens can display cumulative stats.
    pub session_tracker: &'a crate::tracking::SessionTracker,
    /// Last user message — used by /redo to re-send.
    pub last_user_message: &'a mut Option<String>,
}

/// What the REPL loop should do after a command finishes.
pub enum CommandAction {
    /// Nothing special — continue the REPL loop.
    Continue,
    /// Re-inject this text as if the user typed it (used by /redo).
    InjectLine(String),
}

/// Dispatcher for slash-commands.
pub async fn handle_command(cmd: &str, state: &mut ReplState<'_>) -> Result<CommandAction> {

    // Access state fields directly via state.field (avoids double-mut-ref issues from destructuring)
    match cmd {
        "/exit" | "/quit" | "/q" => std::process::exit(0),
        "/plan" => {
            *state.mode = ReplMode::Plan;
            println!();
            println!(
                "  {} {}",
                style("⟳").yellow().bold(),
                style("Switched to Plan mode — discuss your task freely. /act to execute.")
                    .yellow(),
            );
            println!();
            Ok(CommandAction::Continue)
        }
        "/act" => {
            *state.mode = ReplMode::Act;
            println!();
            println!(
                "  {} {}",
                style("⟳").cyan().bold(),
                style("Switched to Act mode — ready to execute.").cyan(),
            );
            println!();
            Ok(CommandAction::Continue)
        }
        // /undo              — pop one entry and restore git working tree
        // /undo list        — show the full undo history
        // /undo N           — pop N entries (confirms if N > 1)
        //
        // For Rust learners: `cmd if cmd.starts_with(...)` is a match guard —
        // a boolean condition attached to a pattern arm.  The arm only fires
        // when BOTH the pattern and the guard are true.
        cmd if cmd == "/undo" || cmd.starts_with("/undo ") => {
            // Parse the sub-command that follows /undo (if any).
            let rest = if cmd == "/undo" { "" } else { cmd[5..].trim() };
            match rest {
                // ── /undo list ───────────────────────────────────────────────
                "list" => match state.ctx.store.list_undo(state.session_id) {
                    Ok(entries) if entries.is_empty() => {
                        info("No undo history for this session.");
                    }
                    Ok(entries) => {
                        println!();
                        println!("{}", console::style("  Undo history (newest first):").dim());
                        for (i, e) in entries.iter().enumerate() {
                            let desc = e.description.as_deref().unwrap_or("(no description)");
                            let ts = e.created_at.format("%H:%M:%S");
                            println!(
                                "  {}  {}  {}",
                                console::style(format!("[{}]", i + 1)).cyan(),
                                console::style(ts.to_string()).dim(),
                                desc,
                            );
                        }
                        println!();
                        info("Use /undo or /undo N to restore the N-th most recent run.");
                    }
                    Err(e) => err(&format!("Failed to read undo history: {:#}", e)),
                },

                // ── /undo N ──────────────────────────────────────────────────
                n if n.parse::<usize>().is_ok() => {
                    let count = n.parse::<usize>().unwrap();
                    if count == 0 {
                        warn("Usage: /undo, /undo list, /undo N  (N ≥ 1)");
                    } else {
                        // Confirm before undoing more than one run.
                        if count > 1 {
                            use dialoguer::{theme::ColorfulTheme, Confirm};
                            let confirmed = Confirm::with_theme(&ColorfulTheme::default())
                                .with_prompt(format!("Undo {} runs? This cannot be undone", count))
                                .default(false)
                                .interact_opt()
                                .unwrap_or(None)
                                .unwrap_or(false);
                            if !confirmed {
                                info("Cancelled.");
                            } else {
                                pop_n_undo(state, count).await;
                            }
                        } else {
                            pop_n_undo(state, count).await;
                        }
                    }
                }

                // ── /undo (bare) ─────────────────────────────────────────────
                "" => {
                    pop_n_undo(state, 1).await;
                }

                _ => {
                    warn("Usage: /undo              — undo last run");
                    warn("       /undo list         — show undo history");
                    warn("       /undo N            — undo last N runs");
                }
            }
            Ok(CommandAction::Continue)
        }
        "/tokens" => {
            // Display the cumulative token usage for this REPL session.
            let report = state.session_tracker.detailed_report();
            print!("{}", report);
            Ok(CommandAction::Continue)
        }
        "/session" => {
            match crate::repl::session_picker(&state.ctx.store) {
                SessionPickResult::NewSession => {
                    ok(&format!("Session saved: {}", style(&state.sess.id).dim()));
                    *state.sess = state.ctx.store.create_session(Some("REPL session"))?;
                    state.conversation_messages.clear();
                    state.act_messages.clear();
                    state
                        .act_messages
                        .push(llm::Message::system(state.coder_system_prompt));
                    info(&format!(
                        "New session started: {}",
                        style(&state.sess.id).cyan()
                    ));
                }
                SessionPickResult::Resume(old_sess) => {
                    match state.ctx.store.get_messages(&old_sess.id) {
                        Ok(stored_msgs) => {
                            state.conversation_messages.clear();
                            state.act_messages.clear();
                            state
                                .act_messages
                                .push(llm::Message::system(state.coder_system_prompt));
                            for m in &stored_msgs {
                                match m.role.as_str() {
                                    "user" => {
                                        let msg =
                                            llm::Message::user(m.content.as_deref().unwrap_or(""));
                                        state.conversation_messages.push(msg.clone());
                                        state.act_messages.push(msg);
                                    }
                                    "assistant" => {
                                        let msg = llm::Message::assistant(m.content.clone(), None);
                                        state.conversation_messages.push(msg.clone());
                                        state.act_messages.push(msg);
                                    }
                                    _ => {}
                                };
                            }
                            let title = old_sess
                                .title
                                .clone()
                                .unwrap_or_else(|| "(untitled)".to_string());
                            *state.sess = old_sess;
                            ok(&format!("Resumed: {}", style(title).cyan()));
                            info(&format!("Session: {}", style(&state.sess.id).dim()));
                        }
                        Err(e) => err(&format!("Failed to load session: {:#}", e)),
                    }
                }
                SessionPickResult::Cancelled => {}
            }
            Ok(CommandAction::Continue)
        }
        "/clear" => {
            ok(&format!("Session saved: {}", style(&state.sess.id).dim()));
            *state.sess = state.ctx.store.create_session(Some("REPL session"))?;
            state.conversation_messages.clear();
            state.act_messages.clear();
            state
                .act_messages
                .push(llm::Message::system(state.coder_system_prompt));
            info(&format!(
                "New session started: {}",
                style(&state.sess.id).cyan()
            ));
            Ok(CommandAction::Continue)
        }
        "/help" => {
            println!();
            let cyan = Style::new().cyan();
            let dim = Style::new().dim();
            let mode_str = match state.mode {
                ReplMode::Act => style("Act  (full tools)").green().to_string(),
                ReplMode::Plan => style("Plan (discuss only)").yellow().to_string(),
            };
            println!("  {}  {}", style("Mode:").dim(), mode_str);
            println!();
            for cmd in COMMANDS {
                println!("  {}  {}", cyan.apply_to(cmd.cmd), dim.apply_to(cmd.desc));
            }
            println!();
            Ok(CommandAction::Continue)
        }
        cmd if cmd.starts_with("/login") => {
            info("Starting GitHub Copilot device authorization…");
            match auth::device_code_flow(&reqwest::Client::new()).await {
                Ok(oauth_token) => match oauth_token.save() {
                    Ok(_) => {
                        let token_str = oauth_token.access_token.clone();
                        state.ctx.llm.set_copilot_oauth_token(token_str).await;
                        ok("Logged in to GitHub Copilot.");
                        if let Err(e) =
                            config::Config::save_provider(llm::openai::COPILOT_API_BASE, "")
                        {
                            warn(&format!("Could not save provider to config: {:#}", e));
                        } else {
                            info("Provider saved to ~/.config/xcode/config.json — no re-login needed next time.");
                        }
                    }
                    Err(e) => err(&format!("Failed to save token: {:#}", e)),
                },
                Err(e) => err(&format!("Login failed: {:#}", e)),
            }
            Ok(CommandAction::Continue)
        }
        "/logout" => {
            match auth::CopilotOAuthToken::delete() {
                Ok(_) => ok("Logged out of GitHub Copilot."),
                Err(e) => err(&format!("Logout failed: {:#}", e)),
            }
            Ok(CommandAction::Continue)
        }
        "/connect" => {
            if let Some((new_base, new_key)) = crate::repl::connect_menu() {
                if new_base == "copilot_do_login" {
                    info("Starting GitHub Copilot device authorization…");
                    match auth::device_code_flow(&reqwest::Client::new()).await {
                        Ok(oauth_token) => match oauth_token.save() {
                            Ok(_) => {
                                let token_str = oauth_token.access_token.clone();
                                state.ctx.llm.set_copilot_oauth_token(token_str).await;
                                ok("Logged in to GitHub Copilot.");
                                info("Provider set to Copilot. You can start a task now.");
                                if let Err(e) =
                                    config::Config::save_provider(llm::openai::COPILOT_API_BASE, "")
                                {
                                    warn(&format!("Could not save provider to config: {:#}", e));
                                } else {
                                    info("Provider saved — no re-login needed next time.");
                                }
                            }
                            Err(e) => err(&format!("Failed to save token: {:#}", e)),
                        },
                        Err(e) => err(&format!("Login failed: {:#}", e)),
                    }
                } else {
                    // switch_provider() creates a new provider instance and updates config.
                    state.ctx.switch_provider(new_base.clone(), new_key.clone());
                    ok(&format!(
                        "Provider set to {} (applies immediately).",
                        style(&new_base).green()
                    ));
                    match config::Config::save_provider(&new_base, &new_key) {
                        Ok(_) => info("Saved to ~/.config/xcode/config.json."),
                        Err(e) => warn(&format!("Could not save to config: {:#}", e)),
                    }
                }
            }
            Ok(CommandAction::Continue)
        }
        cmd if cmd.starts_with("/model") => {
            let rest = cmd[6..].trim();
            if rest.is_empty() {
                // Interactive model picker — fetch models dynamically from the provider API.
                let api_base = state.ctx.config.provider.api_base.clone();
                let api_key = state.ctx.config.provider.api_key.clone();
                let current = state.ctx.config.model.clone();

                use dialoguer::{theme::ColorfulTheme, Select};
                use std::io::{self as stdio, BufRead, Write};

                println!();
                info(&format!("Current model: {}", style(&current).green()));

                // Try to fetch models from the provider API.
                info("Fetching available models from provider...");
                let models_result = fetch_models(&api_base, &api_key).await;

                match models_result {
                    Ok(models) if !models.is_empty() => {
                        println!();
                        // Build labels: mark the currently active model
                        let labels: Vec<String> = models
                            .iter()
                            .map(|m| {
                                if *m == current.as_str() {
                                    format!("{} (current)", m)
                                } else {
                                    m.to_string()
                                }
                            })
                            .chain(std::iter::once("Custom\u{2026}".to_string()))
                            .collect();

                        // Default to current model if it's in the list, otherwise 0
                        let default_idx = models
                            .iter()
                            .position(|m| *m == current.as_str())
                            .unwrap_or(0);

                        let selection = Select::with_theme(&ColorfulTheme::default())
                            .with_prompt("Select model")
                            .items(&labels)
                            .default(default_idx)
                            .interact_opt();
                        println!();

                        match selection {
                            Ok(Some(i)) if i < models.len() => {
                                let new_model = models[i].clone();
                                if new_model != current {
                                    state.ctx.switch_model(new_model.clone());
                                    ok(&format!(
                                        "Model set to {} (applies immediately).",
                                        style(&new_model).green()
                                    ));
                                } else {
                                    info(&format!("Already using {}.", style(&current).green()));
                                }
                            }
                            Ok(Some(_)) => {
                                // "Custom\u{2026}" option \u{2014} prompt for free-form model name
                                print!("   Enter model name: ");
                                stdio::stdout().flush().ok();
                                let mut input = String::new();
                                if stdio::stdin().lock().read_line(&mut input).is_ok() {
                                    let name = input.trim();
                                    if !name.is_empty() {
                                        state.ctx.switch_model(name.to_string());
                                        ok(&format!(
                                            "Model set to {} (applies immediately).",
                                            style(name).green()
                                        ));
                                    }
                                }
                            }
                            _ => { /* Esc / Ctrl-C \u{2014} do nothing */ }
                        }
                    }
                    Ok(_) | Err(_) => {
                        // API call failed or returned no models \u{2014} fall back to manual input.
                        if let Err(ref e) = models_result {
                            warn(&format!("Could not fetch models: {:#}", e));
                        } else {
                            warn("Provider returned no models.");
                        }
                        println!();
                        print!("   Enter model name: ");
                        stdio::stdout().flush().ok();
                        let mut input = String::new();
                        if stdio::stdin().lock().read_line(&mut input).is_ok() {
                            let name = input.trim();
                            if !name.is_empty() {
                                state.ctx.switch_model(name.to_string());
                                ok(&format!(
                                    "Model set to {} (applies immediately).",
                                    style(name).green()
                                ));
                            }
                        }
                    }
                }
            } else {
                let new_model = rest.to_string();
                state.ctx.switch_model(new_model.clone());
                ok(&format!(
                    "Model set to {} (applies immediately).",
                    style(&new_model).green()
                ));
            }
            Ok(CommandAction::Continue)
        }
        // Show all connected MCP servers and the tools each one provides.
        // This is purely informational — it doesn’t change any state.
        "/mcp" => {
            if state.ctx.mcp_clients.is_empty() {
                info("No MCP servers connected. Add entries under \"mcp_servers\" in config.json.");
            } else {
                println!();
                for (name, client) in &state.ctx.mcp_clients {
                    // Lock the client briefly to list its tools.
                    // list_tools() is a cached call (the manifest was fetched at startup)
                    // so this should return immediately without a network round-trip.
                    let mut locked = client.lock().await;
                    match locked.list_tools().await {
                        Ok(tools) => {
                            println!(
                                "  {} {} ({} tools)",
                                style("●").green(),
                                style(name).bold(),
                                tools.len()
                            );
                            for t in &tools {
                                // Each tool is advertised under the "mcp_<name>" prefix
                                // that register_mcp_tools assigns when it registers them.
                                let desc = t
                                    .description
                                    .as_deref()
                                    .map(|d| format!(" — {}", d))
                                    .unwrap_or_default();
                                println!("    {} mcp_{}{}", style("·").dim(), t.name, desc);
                            }
                        }
                        Err(e) => {
                            println!(
                                "  {} {} (error listing tools: {:#})",
                                style("✗").red(),
                                style(name).bold(),
                                e
                            );
                        }
                    }
                }
                println!();
            }
            Ok(CommandAction::Continue)
        }
        "/compact" => {
            // Toggle compact mode
            state.ctx.config.agent.compact_mode = !state.ctx.config.agent.compact_mode;
            state.ctx.tool_ctx.compact_mode = state.ctx.config.agent.compact_mode;
            if state.ctx.config.agent.compact_mode {
                ok("Compact mode ON — concise output, 50-line file reads.");
            } else {
                ok("Compact mode OFF — full output restored.");
            }
            // Also run context summarization on current act messages
            if state.act_messages.len() > 4 {
                info("Summarising conversation context...");
                let ctx_mgr = crate::agent::context_manager::ContextManager::new(
                    state.ctx.config.agent.context.clone(),
                );
                match ctx_mgr.try_summarize(state.act_messages, state.ctx.llm.as_ref()).await {
                    Ok(()) => ok("Context summarised."),
                    Err(e) => warn(&format!("Summarisation failed: {:#}", e)),
                }
            }
            Ok(CommandAction::Continue)
        }
        // /init — generate AGENTS.md by sending the project to the LLM for analysis
        "/init" => {
            let agents_md_path = state.ctx.project_dir.join("AGENTS.md");
            if agents_md_path.exists() {
                use dialoguer::{theme::ColorfulTheme, Confirm};
                let overwrite = Confirm::with_theme(&ColorfulTheme::default())
                    .with_prompt("AGENTS.md already exists. Overwrite?")
                    .default(false)
                    .interact_opt()
                    .unwrap_or(None)
                    .unwrap_or(false);
                if !overwrite {
                    info("Cancelled.");
                    return Ok(CommandAction::Continue);
                }
            }
            info("Analyzing project to generate AGENTS.md...");
            let init_prompt = format!(
                "Analyze the codebase in the current working directory and create an AGENTS.md file. \
                 The file should contain:\n\
                 1. A brief project overview (1-2 sentences)\n\
                 2. Build commands (build, test, lint, format, single-test)\n\
                 3. Code style guidelines observed in the codebase\n\
                 4. Key architectural patterns\n\
                 5. Important conventions and rules\n\n\
                 Keep it under 150 lines. Be specific to THIS project — not generic advice. \
                 Write the file to: {}\n\
                 If you find existing Cursor rules (.cursorrules) or Copilot instructions (.github/copilot-instructions.md), incorporate them.",
                agents_md_path.display()
            );
            // Run this as a one-shot agent task via Director
            let mut task_messages = vec![
                crate::llm::Message::system(state.coder_system_prompt),
                crate::llm::Message::user(&init_prompt),
            ];
            let director = crate::agent::director::Director::new(state.ctx.config.agent.clone());
            println!();
            match director.execute(
                &mut task_messages,
                state.ctx.registry.as_ref(),
                state.ctx.llm.as_ref(),
                &state.ctx.tool_ctx,
            ).await {
                Ok(_) => ok("AGENTS.md generated. It will be loaded on next startup."),
                Err(e) => err(&format!("Failed to generate AGENTS.md: {:#}", e)),
            }
            crate::ui::print_separator("");
            Ok(CommandAction::Continue)
        }
        // /redo — re-send the last user message
        "/redo" => {
            match state.last_user_message.clone() {
                Some(msg) => {
                    info(&format!("Re-sending: {}", &msg.chars().take(80).collect::<String>()));
                    Ok(CommandAction::InjectLine(msg))
                }
                None => {
                    warn("No previous message to re-send.");
                    Ok(CommandAction::Continue)
                }
            }
        }
        // Unknown /xxx command or bare / → show command menu
        cmd if cmd == "/"
            || (cmd.starts_with('/')
                && !cmd.starts_with("/model")
                && !cmd.starts_with("/login")
                && !cmd.starts_with("/compact")
                && !cmd.starts_with("/mcp")
                && !cmd.starts_with("/undo")
                && !cmd.starts_with("/init")
                && !cmd.starts_with("/redo")) =>
        {
            if let Some(chosen) = show_command_menu() {
                state.history.push(&chosen);
                // Buffer the chosen command so the top of the loop processes it exactly like direct user input
                // This will be handled in the main REPL loop.
            }
            Ok(CommandAction::Continue)
        }
        // Plain-text exit words
        "exit" | "quit" | "q" | "bye" | "bye!" | "exit!" | "quit!" => std::process::exit(0),
        _ => Ok(CommandAction::Continue), // Not a slash-command
    }
}

// ─── /undo helper ───────────────────────────────────────────────────────────
//
// Pop `count` undo entries from the DB and restore each via `git stash pop`.
// Separated into its own function to keep handle_command() readable.
//
// For Rust learners: `async fn` outside of `impl` blocks is perfectly fine in
// Rust.  We pass `state` by mutable reference so we can access the store and
// project directory without copying them.
async fn pop_n_undo(state: &mut ReplState<'_>, count: usize) {
    let mut popped = 0;
    for _ in 0..count {
        // Fetch and remove the newest undo entry from the DB.
        let entry = match state.ctx.store.pop_undo(state.session_id) {
            Ok(Some(e)) => e,
            Ok(None) => {
                if popped == 0 {
                    warn("Nothing to undo — no Act-mode runs recorded for this session.");
                } else {
                    ok(&format!("Undid {} run(s) — no more entries.", popped));
                }
                return;
            }
            Err(e) => {
                err(&format!("Failed to read undo history: {:#}", e));
                return;
            }
        };

        // ── Find the stash index that matches our unique label ──
        //
        // `git stash list --format="%gd %s"` prints lines like:
        //   stash@{0} On main: xcodeai-undo-<uuid>
        //   stash@{1} On main: xcodeai-undo-<another-uuid>
        //
        // We search for the line that contains our `stash_ref` label,
        // extract the `stash@{N}` part, and pop that specific entry.
        // This is more reliable than always popping stash@{0} because
        // the user may have created their own stashes in between.
        let list_out = std::process::Command::new("git")
            .args(["stash", "list", "--format=%gd %s"])
            .current_dir(&state.ctx.project_dir)
            .output();

        let stash_index: Option<String> = match list_out {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                // Find the line that contains our unique stash_ref label.
                text.lines()
                    .find(|l| l.contains(&entry.stash_ref))
                    .and_then(|l| l.split_whitespace().next())
                    .map(|s| s.to_string())
            }
            Err(_) => None,
        };

        match stash_index {
            Some(ref idx) => {
                // Pop the specific stash entry by its index (e.g. stash@{2}).
                let pop = std::process::Command::new("git")
                    .args(["stash", "pop", idx])
                    .current_dir(&state.ctx.project_dir)
                    .output();
                match pop {
                    Ok(o) if o.status.success() => {
                        popped += 1;
                        let desc = entry.description.as_deref().unwrap_or("(no description)");
                        ok(&format!("Undid: {}", desc));
                        // Show a compact diff of what was restored.
                        let stat = std::process::Command::new("git")
                            .args(["diff", "--stat", "HEAD"])
                            .current_dir(&state.ctx.project_dir)
                            .output();
                        if let Ok(s) = stat {
                            let text = String::from_utf8_lossy(&s.stdout);
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                println!();
                                for l in trimmed.lines() {
                                    println!("   {}", console::style(l).dim());
                                }
                            }
                        }
                    }
                    Ok(o) => {
                        let msg = String::from_utf8_lossy(&o.stderr);
                        err(&format!("git stash pop {} failed: {}", idx, msg.trim()));
                        return;
                    }
                    Err(e) => {
                        err(&format!("Could not run git: {:#}", e));
                        return;
                    }
                }
            }
            None => {
                // The stash entry was recorded in the DB but the git stash is gone
                // (user may have run `git stash drop` manually).
                let desc = entry.description.as_deref().unwrap_or("(no description)");
                warn(&format!(
                    "Undo entry '{}' has no matching git stash — stash may have been dropped manually.",
                    desc
                ));
                // Keep looping — the DB entry is already consumed.
            }
        }
    }
    if popped > 1 {
        println!();
        ok(&format!("Undid {} run(s) total.", popped));
    } else if popped == 1 && count == 1 {
        // Single undo message already printed inside the loop.
    }
    if popped > 0 {
        println!();
    }
}

/// Fetch available model names from the provider's list-models API.
///
/// Each provider has a different endpoint / auth scheme:
///  - OpenAI / OpenAI-compat: `GET {api_base}/models`
///  - Anthropic:              `GET https://api.anthropic.com/v1/models`
///  - Gemini:                 `GET https://generativelanguage.googleapis.com/v1beta/models?key=…`
///  - GitHub Copilot:         `GET https://api.githubcopilot.com/models`  (Copilot bearer token)
///
/// Returns a sorted list of model IDs on success, or an error if the HTTP
/// call fails.  The caller should fall back to manual input on error.
async fn fetch_models(api_base: &str, api_key: &str) -> Result<Vec<String>> {
    use crate::llm::anthropic::ANTHROPIC_API_BASE;
    use crate::llm::gemini::GEMINI_API_BASE;
    use crate::llm::openai::COPILOT_API_BASE;
    use serde::Deserialize;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    // ── Helper structs for JSON deserialization ──────────────────────────────

    #[derive(Deserialize)]
    struct OpenAiModelsResponse {
        data: Vec<OpenAiModelEntry>,
    }
    #[derive(Deserialize)]
    struct OpenAiModelEntry {
        id: String,
    }

    #[derive(Deserialize)]
    struct AnthropicModelsResponse {
        data: Vec<AnthropicModelEntry>,
    }
    #[derive(Deserialize)]
    struct AnthropicModelEntry {
        id: String,
    }

    #[derive(Deserialize)]
    struct GeminiModelsResponse {
        models: Vec<GeminiModelEntry>,
    }
    #[derive(Deserialize)]
    struct GeminiModelEntry {
        name: String,
    }

    #[derive(Deserialize)]
    struct CopilotModelsResponse {
        data: Vec<CopilotModelEntry>,
    }
    #[derive(Deserialize)]
    struct CopilotModelEntry {
        id: String,
    }

    // ── Determine provider type and call the right endpoint ─────────────────

    let is_anthropic = api_base == ANTHROPIC_API_BASE
        || api_base.starts_with("https://api.anthropic.com");
    let is_gemini = api_base == GEMINI_API_BASE
        || api_base.starts_with("https://generativelanguage.googleapis.com");
    let is_copilot = api_base == COPILOT_API_BASE;

    if is_copilot {
        // ── GitHub Copilot ─────────────────────────────────────────────────
        // Load the persisted OAuth token, exchange it for a short-lived API
        // bearer token, then query the Copilot models endpoint.
        let oauth = crate::auth::CopilotOAuthToken::load()?
            .ok_or_else(|| anyhow::anyhow!("No Copilot credentials — run /login first"))?;
        let api_token = crate::auth::refresh_copilot_token(&client, &oauth.access_token).await?;
        let resp = client
            .get("https://api.githubcopilot.com/models")
            .header("Authorization", format!("Bearer {}", api_token.token))
            .header("Editor-Version", "vscode/1.80.1")
            .header("Copilot-Integration-Id", "vscode-chat")
            .header("User-Agent", "GithubCopilot/1.155.0")
            .header("Content-Type", "application/json")
            .send()
            .await?
            .error_for_status()?;
        let body: CopilotModelsResponse = resp.json().await?;
        let mut models: Vec<String> = body.data.into_iter().map(|e| e.id).collect();
        models.sort();
        Ok(models)
    } else if is_anthropic {
        // ── Anthropic ─────────────────────────────────────────────────────
        let resp = client
            .get("https://api.anthropic.com/v1/models?limit=100")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await?
            .error_for_status()?;
        let body: AnthropicModelsResponse = resp.json().await?;
        let mut models: Vec<String> = body.data.into_iter().map(|e| e.id).collect();
        models.sort();
        Ok(models)
    } else if is_gemini {
        // ── Google Gemini ──────────────────────────────────────────────────
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models?key={}&pageSize=100",
            api_key
        );
        let resp = client.get(&url).send().await?.error_for_status()?;
        let body: GeminiModelsResponse = resp.json().await?;
        // Gemini returns names like "models/gemini-2.5-pro"; strip the prefix.
        let mut models: Vec<String> = body
            .models
            .into_iter()
            .map(|e| e.name.strip_prefix("models/").unwrap_or(&e.name).to_string())
            .collect();
        models.sort();
        Ok(models)
    } else {
        // ── OpenAI / OpenAI-compatible ────────────────────────────────────
        // api_base is the full URL like "https://api.openai.com/v1".
        // Append "/models" to it.
        let url = format!("{}/models", api_base.trim_end_matches('/'));
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .send()
            .await?
            .error_for_status()?;
        let body: OpenAiModelsResponse = resp.json().await?;
        let mut models: Vec<String> = body.data.into_iter().map(|e| e.id).collect();
        models.sort();
        Ok(models)
    }
}
