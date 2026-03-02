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
        desc: "Toggle compact mode (shorter output, brevity-focused prompt)",
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
}

/// Dispatcher for slash-commands. Returns Some(ReplMode) if mode changes, None otherwise.
pub async fn handle_command(cmd: &str, state: &mut ReplState<'_>) -> Result<Option<ReplMode>> {
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
            Ok(Some(ReplMode::Plan))
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
            Ok(Some(ReplMode::Act))
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
            Ok(None)
        }
        "/tokens" => {
            // Display the cumulative token usage for this REPL session.
            let report = state.session_tracker.detailed_report();
            print!("{}", report);
            Ok(None)
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
            Ok(None)
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
            Ok(None)
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
            Ok(None)
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
            Ok(None)
        }
        "/logout" => {
            match auth::CopilotOAuthToken::delete() {
                Ok(_) => ok("Logged out of GitHub Copilot."),
                Err(e) => err(&format!("Logout failed: {:#}", e)),
            }
            Ok(None)
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
            Ok(None)
        }
        cmd if cmd.starts_with("/model") => {
            let rest = cmd[6..].trim();
            if rest.is_empty() {
                // Interactive model picker based on current provider
                let api_base = &state.ctx.config.provider.api_base;
                let models = model_presets_for_provider(api_base);
                let current = &state.ctx.config.model;

                use dialoguer::{theme::ColorfulTheme, Select};
                use std::io::{self as stdio, BufRead, Write};

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
                    .chain(std::iter::once("Custom…".to_string()))
                    .collect();

                // Default to current model if it's in the list, otherwise 0
                let default_idx = models
                    .iter()
                    .position(|m| *m == current.as_str())
                    .unwrap_or(0);

                println!();
                info(&format!("Current model: {}", style(current).green()));
                println!();
                let selection = Select::with_theme(&ColorfulTheme::default())
                    .with_prompt("Select model")
                    .items(&labels)
                    .default(default_idx)
                    .interact_opt();
                println!();

                match selection {
                    Ok(Some(i)) if i < models.len() => {
                        let new_model = models[i].to_string();
                        if new_model != *current {
                            state.ctx.switch_model(new_model.clone());
                            ok(&format!(
                                "Model set to {} (applies immediately).",
                                style(&new_model).green()
                            ));
                        } else {
                            info(&format!("Already using {}.", style(current).green()));
                        }
                    }
                    Ok(Some(_)) => {
                        // "Custom…" option — prompt for free-form model name
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
                    _ => { /* Esc / Ctrl-C — do nothing */ }
                }
            } else {
                let new_model = rest.to_string();
                state.ctx.switch_model(new_model.clone());
                ok(&format!(
                    "Model set to {} (applies immediately).",
                    style(&new_model).green()
                ));
            }
            Ok(None)
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
            Ok(None)
        }
        // Toggle compact mode on/off for the current session.
        // Compact mode caps file_read output to 50 lines and adds a
        // brevity instruction to the system prompt.
        "/compact" => {
            state.ctx.config.agent.compact_mode = !state.ctx.config.agent.compact_mode;
            state.ctx.tool_ctx.compact_mode = state.ctx.config.agent.compact_mode;
            if state.ctx.config.agent.compact_mode {
                ok("Compact mode ON — concise output, 50-line file reads.");
            } else {
                ok("Compact mode OFF — full output restored.");
            }
            Ok(None)
        }
        // Unknown /xxx command or bare / → show command menu
        cmd if cmd == "/"
            || (cmd.starts_with('/')
                && !cmd.starts_with("/model")
                && !cmd.starts_with("/login")
                && !cmd.starts_with("/compact")
                && !cmd.starts_with("/mcp")
                && !cmd.starts_with("/undo")) =>
        {
            if let Some(chosen) = show_command_menu() {
                state.history.push(&chosen);
                // Buffer the chosen command so the top of the loop processes it exactly like direct user input
                // This will be handled in the main REPL loop.
            }
            Ok(None)
        }
        // Plain-text exit words
        "exit" | "quit" | "q" | "bye" | "bye!" | "exit!" | "quit!" => std::process::exit(0),
        _ => Ok(None), // Not a slash-command
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

/// Return a list of common model names for the given provider api_base.
/// Used by the `/model` interactive picker.
fn model_presets_for_provider(api_base: &str) -> Vec<&'static str> {
    use crate::llm::openai::COPILOT_API_BASE;

    if api_base == COPILOT_API_BASE {
        // GitHub Copilot — models available through Copilot API
        vec![
            "gpt-4o",
            "gpt-4o-mini",
            "o3-mini",
            "claude-3.5-sonnet",
            "claude-3.7-sonnet",
            "claude-sonnet-4",
            "gemini-2.0-flash",
            "gemini-2.5-pro",
        ]
    } else if api_base == "anthropic" || api_base.starts_with("https://api.anthropic.com") {
        vec![
            "claude-sonnet-4-20250514",
            "claude-3-5-sonnet-20241022",
            "claude-3-5-haiku-20241022",
            "claude-opus-4-20250514",
        ]
    } else if api_base == "gemini" || api_base.starts_with("https://generativelanguage.googleapis.com") {
        vec![
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.0-flash",
        ]
    } else if api_base.contains("deepseek") {
        vec!["deepseek-chat", "deepseek-coder", "deepseek-reasoner"]
    } else if api_base.contains("dashscope.aliyuncs.com") {
        vec!["qwen-max", "qwen-plus", "qwen-turbo"]
    } else if api_base.contains("bigmodel.cn") {
        vec!["glm-4-plus", "glm-4", "glm-4-flash"]
    } else if api_base.contains("localhost") || api_base.contains("127.0.0.1") {
        // Ollama / local — common open models
        vec!["llama3", "codellama", "mistral", "deepseek-coder-v2"]
    } else if api_base.contains("openai.com") {
        vec![
            "gpt-4o",
            "gpt-4o-mini",
            "o3-mini",
            "gpt-4-turbo",
        ]
    } else {
        // Unknown provider — show a generic list
        vec!["gpt-4o", "gpt-4o-mini", "claude-3-5-sonnet-20241022", "deepseek-chat"]
    }
}
