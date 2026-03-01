mod auth;
mod agent;
mod config;
mod llm;
mod sandbox;
mod session;
mod tools;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use console::{style, Style, Term};
use std::path::PathBuf;

// ─── Styling helpers ──────────────────────────────────────────────────────────

fn print_banner(version: &str, model: &str, project: &str, auth_status: &str) {
    let term = Term::stdout();
    let width = term.size().1 as usize;
    let width = width.max(60).min(100);
    let line = style("─".repeat(width)).dim().to_string();

    println!();
    println!(
        " {} {}  {}  {}",
        style("✦").cyan().bold(),
        style("xcodeai").cyan().bold(),
        style(format!("v{}", version)).dim(),
        style("autonomous AI coding agent").dim(),
    );
    println!(
        "   {} {}    {} {}",
        style("model:").dim(),
        style(model).green(),
        style("project:").dim(),
        style(project).yellow(),
    );
    println!(
        "   {} {}",
        style("auth:").dim(),
        auth_status,
    );
    println!("{}", line);
    println!(
        "   {}",
        style("Type a task and press Enter.  /plan to discuss first.  /help for commands.  Ctrl-D to quit.").dim()
    );
    println!("{}", line);
    println!();
}

fn print_separator(label: &str) {
    let label_str = if label.is_empty() {
        style("─".repeat(44)).dim().to_string()
    } else {
        format!(
            "{} {} {}",
            style("─".repeat(2)).dim(),
            style(label).dim(),
            style("─".repeat(40 - label.len().min(38))).dim()
        )
    };
    println!("{}", label_str);
}

fn ok(msg: &str) {
    println!(" {} {}", style("✓").green().bold(), msg);
}

fn warn(msg: &str) {
    println!(" {} {}", style("!").yellow().bold(), style(msg).yellow());
}

fn err(msg: &str) {
    eprintln!(" {} {}", style("✗").red().bold(), style(msg).red());
}

fn info(msg: &str) {
    println!("   {}", style(msg).dim());
}

// ─── CLI structs ──────────────────────────────────────────────────────────────

/// Top-level CLI. When no subcommand is given, enters interactive REPL mode.
#[derive(Parser)]
#[command(name = "xcodeai", version, about = "Autonomous AI coding agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    // REPL-mode flags (used when no subcommand is given)
    /// Project directory (default: current directory)
    #[arg(long, short, global = false)]
    project: Option<PathBuf>,
    /// Disable sandbox (use direct execution)
    #[arg(long, global = false)]
    no_sandbox: bool,
    /// Override model name
    #[arg(long, global = false)]
    model: Option<String>,
    /// Override API base URL
    #[arg(long, global = false)]
    provider_url: Option<String>,
    /// Override API key
    #[arg(long, global = false)]
    api_key: Option<String>,
    /// Skip confirmation prompts for destructive tool calls (bash rm, file_write overwrites, etc.)
    #[arg(long, short = 'y', global = false)]
    yes: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a single autonomous coding task (non-interactive)
    Run {
        /// The task message for the agent
        message: String,
        /// Project directory (default: current directory)
        #[arg(long, short)]
        project: Option<PathBuf>,
        /// Disable sandbox (use direct execution)
        #[arg(long)]
        no_sandbox: bool,
        /// Override model name
        #[arg(long)]
        model: Option<String>,
        /// Override API base URL
        #[arg(long)]
        provider_url: Option<String>,
        /// Override API key
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Manage sessions
    Session {
        #[command(subcommand)]
        command: SessionCommands,
    },
}

#[derive(Subcommand)]
enum SessionCommands {
    /// List recent sessions
    List {
        #[arg(long, default_value = "20")]
        limit: u32,
    },
    /// Show session details and messages
    Show {
        /// Session ID
        id: String,
    },
}

// ─── Entry point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        None => {
            repl_command(
                cli.project,
                cli.no_sandbox,
                cli.model,
                cli.provider_url,
                cli.api_key,
                cli.yes,
            )
            .await?;
        }
        Some(Commands::Run {
            message,
            project,
            no_sandbox,
            model,
            provider_url,
            api_key,
        }) => {
            run_command(message, project, no_sandbox, model, provider_url, api_key).await?;
        }
        Some(Commands::Session { command }) => match command {
            SessionCommands::List { limit } => session_list_command(limit)?,
            SessionCommands::Show { id } => session_show_command(&id)?,
        },
    }

    Ok(())
}

// ─── Shared context ───────────────────────────────────────────────────────────

struct AgentContext {
    config: config::Config,
    registry: tools::ToolRegistry,
    llm: llm::openai::OpenAiProvider,
    tool_ctx: tools::ToolContext,
    store: session::SessionStore,
    project_dir: PathBuf,
}

impl AgentContext {
    fn new(
        project: Option<PathBuf>,
        no_sandbox: bool,
        model: Option<String>,
        provider_url: Option<String>,
        api_key: Option<String>,
        confirm_destructive: bool,
    ) -> Result<Self> {
        use config::{Config, ConfigOverrides};
        use llm::openai::{OpenAiProvider, COPILOT_API_BASE};
        use session::SessionStore;
        use tools::bash::BashTool;
        use tools::file_edit::FileEditTool;
        use tools::file_read::FileReadTool;
        use tools::file_write::FileWriteTool;
        use tools::glob_search::GlobSearchTool;
        use tools::grep_search::GrepSearchTool;
        use tools::question::QuestionTool;
        use tools::{ToolContext, ToolRegistry};

        // 1. Load config with CLI overrides
        let overrides = ConfigOverrides {
            api_key: api_key.clone(),
            api_base: provider_url.clone(),
            model: model.clone(),
            project_dir: project.clone(),
            no_sandbox,
        };
        let config = Config::load(&overrides)?;

        // 2. API key validated lazily by callers based on provider mode.

        // 3. Project directory
        let project_dir = config
            .project_dir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        // 4. Session store
        let db_path = SessionStore::default_path()?;
        let store = SessionStore::new(&db_path)?;

        // 5. Tools
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FileReadTool));
        registry.register(Box::new(FileWriteTool));
        registry.register(Box::new(FileEditTool));
        registry.register(Box::new(BashTool));
        registry.register(Box::new(GlobSearchTool));
        registry.register(Box::new(GrepSearchTool));
        registry.register(Box::new(QuestionTool));

        // 6. LLM provider — Copilot or standard
        let llm = if config.provider.api_base == COPILOT_API_BASE {
            let oauth_token = auth::CopilotOAuthToken::load()
                .ok()
                .flatten()
                .map(|t| t.access_token)
                .unwrap_or_default();
            OpenAiProvider::new_copilot(oauth_token, config.model.clone())
        } else {
            OpenAiProvider::new(
                config.provider.api_base.clone(),
                config.provider.api_key.clone(),
                config.model.clone(),
            )
        };

        // 7. Tool context
        let tool_ctx = ToolContext {
            working_dir: project_dir.clone(),
            sandbox_enabled: config.sandbox.enabled && !no_sandbox,
            confirm_destructive,
        };

        Ok(Self {
            config,
            registry,
            llm,
            tool_ctx,
            store,
            project_dir,
        })
    }
}

// ─── `xcodeai run "task"` ─────────────────────────────────────────────────────

async fn run_command(
    message: String,
    project: Option<PathBuf>,
    no_sandbox: bool,
    model: Option<String>,
    provider_url: Option<String>,
    api_key: Option<String>,
) -> Result<()> {
    use agent::director::Director;
    use agent::Agent;
    use session::auto_title;

    let ctx = AgentContext::new(project, no_sandbox, model, provider_url, api_key, false)?;

    // Lazy API key validation: only required for non-Copilot providers
    if !ctx.llm.is_copilot() && ctx.config.provider.api_key.is_empty() {
        bail!(
            "API key not configured. Set XCODE_API_KEY environment variable or add to config file at ~/.config/xcode/config.json"
        );
    }

    let title = auto_title(&message);
    let sess = ctx.store.create_session(Some(&title))?;
    tracing::info!("Session created: {}", sess.id);

    ctx.store
        .add_message(&sess.id, &llm::Message::user(&message))?;

    let director = Director::new(ctx.config.agent.clone());
    // Build the initial message list: system prompt + the user task.
    // For `run` (non-interactive) there is only one turn so a fresh vec is correct.
    let coder_system_prompt = agent::coder::CoderAgent::new(ctx.config.agent.clone()).system_prompt().to_string();
    let mut messages = vec![
        llm::Message::system(&coder_system_prompt),
        llm::Message::user(&message),
    ];
    let result = director
        .execute(&mut messages, &ctx.registry, &ctx.llm, &ctx.tool_ctx)
        .await;

    match result {
        Ok(agent_result) => {
            ctx.store.add_message(
                &sess.id,
                &llm::Message::assistant(Some(agent_result.final_message.clone()), None),
            )?;
            ctx.store.update_session_timestamp(&sess.id)?;

            println!();
            print_separator("done");
            ok("Task complete");
            info(&format!("session       {}", sess.id));
            info(&format!("iterations    {}", agent_result.iterations));
            info(&format!("tool calls    {}", agent_result.tool_calls_total));
            if agent_result.auto_continues > 0 {
                info(&format!("auto-continues {}", agent_result.auto_continues));
            }
            print_separator("");
            println!("{}", agent_result.final_message);
        }
        Err(e) => {
            tracing::error!("Agent error: {:#}", e);
            err(&format!("{:#}", e));
            std::process::exit(1);
        }
    }

    Ok(())
}

// ─── `xcodeai` (no args) — interactive REPL ──────────────────────────────────

// ─── REPL mode ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum ReplMode {
    Act,
    Plan,
}

/// Built-in provider presets shown in /connect menu
struct ProviderPreset {
    label: &'static str,
    api_base: &'static str,
    needs_key: bool,
}

const PROVIDER_PRESETS: &[ProviderPreset] = &[
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

// ─── Command definitions (used for / autocomplete) ───────────────────────────

struct CommandDef {
    cmd: &'static str,
    desc: &'static str,
}

const COMMANDS: &[CommandDef] = &[
    CommandDef { cmd: "/plan",    desc: "Switch to Plan mode — discuss & clarify your task" },
    CommandDef { cmd: "/act",     desc: "Switch to Act mode — execute tools" },
    CommandDef { cmd: "/undo",    desc: "Undo the last Act-mode run (git stash pop)" },
    CommandDef { cmd: "/session", desc: "Browse history or start a new session" },
    CommandDef { cmd: "/connect", desc: "Pick a provider from a menu" },
    CommandDef { cmd: "/login",   desc: "GitHub Copilot device-code OAuth" },
    CommandDef { cmd: "/logout",  desc: "Remove saved Copilot credentials" },
    CommandDef { cmd: "/model",   desc: "Show or change current model (/model gpt-4o)" },
    CommandDef { cmd: "/help",    desc: "Show all commands" },
    CommandDef { cmd: "/exit",    desc: "Exit xcodeai" },
];

/// Show a /command picker. Returns the chosen command string (e.g. "/session"),
/// or None if the user pressed Esc / Ctrl-C.
fn show_command_menu() -> Option<String> {
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

// ─── Session picker ───────────────────────────────────────────────────────────

enum SessionPickResult {
    /// User chose to start a brand-new session
    NewSession,
    /// User selected an existing session to resume
    Resume(session::Session),
    /// User cancelled (Esc)
    Cancelled,
}

/// Show an interactive session picker.
/// Returns NewSession, Resume(session), or Cancelled.
fn session_picker(store: &session::SessionStore) -> SessionPickResult {
    use dialoguer::{theme::ColorfulTheme, Select};

    let sessions = match store.list_sessions(30) {
        Ok(s) => s,
        Err(e) => {
            err(&format!("Failed to load sessions: {:#}", e));
            return SessionPickResult::Cancelled;
        }
    };

    // Build label list: first entry is always "New session"
    let mut labels: Vec<String> = vec![
        format!("  {} New session", style("+").green().bold()),
    ];
    for s in &sessions {
        let date = s.updated_at.format("%Y-%m-%d %H:%M").to_string();
        let title = s.title.as_deref().unwrap_or("(untitled)");
        // Truncate long titles so the list stays readable
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
/// Returns the selected (api_base, api_key) or None if cancelled.
fn connect_menu(rl: &mut rustyline::DefaultEditor) -> Option<(String, String)> {
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
        // Custom URL — ask inline
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

    // For Copilot — signal to the async caller to run device-code flow
    if api_base == llm::openai::COPILOT_API_BASE {
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

async fn repl_command(
    project: Option<PathBuf>,
    no_sandbox: bool,
    model: Option<String>,
    provider_url: Option<String>,
    api_key: Option<String>,
    yes: bool,  // if true, skip confirmation prompts for destructive tool calls
) -> Result<()> {
    use agent::coder::run_plan_turn;
    use agent::director::Director;
    use agent::Agent;
    use rustyline::error::ReadlineError;
    use rustyline::DefaultEditor;
    use session::auto_title;

    let mut ctx = AgentContext::new(project, no_sandbox, model, provider_url, api_key, !yes)?;

    let mut sess = ctx.store.create_session(Some("REPL session"))?;

    // Auth status string with colour
    let auth_status: String = if ctx.llm.is_copilot() {
        match auth::CopilotOAuthToken::load() {
            Ok(Some(_)) => style("GitHub Copilot  ✓ authenticated").green().to_string(),
            _ => style("GitHub Copilot  ✗ not authenticated — run /login").yellow().to_string(),
        }
    } else if ctx.config.provider.api_key.is_empty() {
        style("no API key — run /connect to configure").yellow().to_string()
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
        use agent::coder::CoderAgent;
        CoderAgent::new(ctx.config.agent.clone()).system_prompt().to_string()
    };
    let mut act_messages: Vec<llm::Message> = vec![
        llm::Message::system(&coder_system_prompt),
    ];

    // ── Undo stack ────────────────────────────────────────────────────────────
    // Tracks whether a git stash was created before the most recent Act-mode run.
    // `true`  → a stash exists and /undo can pop it.
    // `false` → nothing to undo (no run yet, or undo already used).
    //
    // Implementation strategy:
    //   Before each Act-mode run we attempt `git stash push --include-untracked`.
    //   If the working tree is already clean git prints nothing and exits 0, but
    //   no stash entry is created — `git stash show` will tell us whether it
    //   actually saved anything.  We store that fact in `undo_stash_available`.
    //   On `/undo` we run `git stash pop` and clear the flag.
    //
    //   If the project directory is not inside a git repo (or git is not installed)
    //   the commands fail silently and undo is simply unavailable — we warn the
    //   user gracefully.
    let mut undo_stash_available: bool = false;
    loop {
        let prompt = match mode {
            ReplMode::Act  => format!("{} ", style("xcodeai›").cyan().bold()),
            ReplMode::Plan => format!("{} ", style("[plan] xcodeai›").yellow().bold()),
        };

        // Drain a pending command from the menu, or read a new line from the user.
        // Using a dedicated variable here avoids shadowing `line` inside the
        // Interrupted / Eof error arms and keeps all readline error handling in
        // one place.
        let line = if let Some(p) = pending_line.take() {
            // A menu selection was buffered — use it as if the user typed it.
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
                // ── REPL commands ──────────────────────────────────────────
                match line.as_str() {
                    "/exit" | "/quit" | "/q" => break,

                    "/plan" => {
                        mode = ReplMode::Plan;
                        println!();
                        println!(
                            "  {} {}",
                            style("⟳").yellow().bold(),
                            style("Switched to Plan mode — discuss your task freely. /act to execute.").yellow(),
                        );
                        println!();
                        continue;
                    }

                    "/act" => {
                        mode = ReplMode::Act;
                        println!();
                        println!(
                            "  {} {}",
                            style("⟳").cyan().bold(),
                            style("Switched to Act mode — ready to execute.").cyan(),
                        );
                        println!();
                        continue;
                    }

                    "/undo" => {
                        // ── /undo ──────────────────────────────────────
                        // Restore the working tree to its state immediately before
                        // the last Act-mode run by popping the git stash that was
                        // pushed automatically before that run.
                        if !undo_stash_available {
                            warn("Nothing to undo — no Act-mode run since last /undo (or project is not a git repo).");
                        } else {
                            // `git stash pop` restores the index and working tree from
                            // the most recent stash entry and removes it from the stash.
                            let pop_result = std::process::Command::new("git")
                                .args(["stash", "pop"])
                                .current_dir(&ctx.project_dir)
                                .output();
                            match pop_result {
                                Ok(out) if out.status.success() => {
                                    // Clear the flag — undo is now spent.
                                    undo_stash_available = false;
                                    ok("Undo successful — working tree restored to pre-run state.");
                                    // Show a compact diff so the user sees what changed.
                                    let stat = std::process::Command::new("git")
                                        .args(["diff", "--stat", "HEAD"])
                                        .current_dir(&ctx.project_dir)
                                        .output();
                                    if let Ok(s) = stat {
                                        let text = String::from_utf8_lossy(&s.stdout);
                                        let trimmed = text.trim();
                                        if !trimmed.is_empty() {
                                            println!();
                                            for l in trimmed.lines() {
                                                println!("   {}", style(l).dim());
                                            }
                                            println!();
                                        }
                                    }
                                }
                                Ok(out) => {
                                    // git printed an error (e.g. merge conflict)
                                    let msg = String::from_utf8_lossy(&out.stderr);
                                    err(&format!("git stash pop failed: {}", msg.trim()));
                                }
                                Err(e) => {
                                    err(&format!("Could not run git: {:#}", e));
                                }
                            }
                        }
                        continue;
                    }

                    "/session" => {
                        match session_picker(&ctx.store) {
                            SessionPickResult::NewSession => {
                                // Save the old session, start a new one
                                ok(&format!("Session saved: {}", style(&sess.id).dim()));
                                sess = ctx.store.create_session(Some("REPL session"))?;
                                conversation_messages.clear();
                                // Reset Act mode history too — new session = fresh context
                                act_messages.clear();
                                act_messages.push(llm::Message::system(&coder_system_prompt));
                                info(&format!("New session started: {}", style(&sess.id).cyan()));
                            }
                            SessionPickResult::Resume(old_sess) => {
                                // Load existing session messages into conversation history
                                match ctx.store.get_messages(&old_sess.id) {
                                    Ok(stored_msgs) => {
                                        conversation_messages.clear();
                                        act_messages.clear();
                                        act_messages.push(llm::Message::system(&coder_system_prompt));
                                        for m in &stored_msgs {
                                            let role = match m.role.as_str() {
                                                "user" => {
                                                    let msg = llm::Message::user(m.content.as_deref().unwrap_or(""));
                                                    conversation_messages.push(msg.clone());
                                                    act_messages.push(msg);
                                                }
                                                "assistant" => {
                                                    let msg = llm::Message::assistant(m.content.clone(), None);
                                                    conversation_messages.push(msg.clone());
                                                    act_messages.push(msg);
                                                }
                                                _ => {}
                                            };
                                            let _ = role;
                                        }
                                        let title = old_sess.title.clone().unwrap_or_else(|| "(untitled)".to_string());
                                        sess = old_sess;
                                        ok(&format!("Resumed: {}", style(title).cyan()));
                                        info(&format!("Session: {}", style(&sess.id).dim()));
                                    }  // Ok(stored_msgs)
                                    Err(e) => err(&format!("Failed to load session: {:#}", e)),
                                }  // match get_messages
                            }  // Resume
                            SessionPickResult::Cancelled => {}
                        }  // match session_picker
                        continue;
                    }

                    "/clear" => {
                        // Same as "New session" — create a fresh session without restarting
                        ok(&format!("Session saved: {}", style(&sess.id).dim()));
                        sess = ctx.store.create_session(Some("REPL session"))?;
                        conversation_messages.clear();
                        act_messages.clear();
                        act_messages.push(llm::Message::system(&coder_system_prompt));
                        info(&format!("New session started: {}", style(&sess.id).cyan()));
                        continue;
                    }

                    "/help" => {
                        println!();
                        let cyan = Style::new().cyan();
                        let dim = Style::new().dim();
                        let mode_str = match mode {
                            ReplMode::Act  => style("Act  (full tools)").green().to_string(),
                            ReplMode::Plan => style("Plan (discuss only)").yellow().to_string(),
                        };
                        println!("  {}  {}", style("Mode:").dim(), mode_str);
                        println!();
                        println!("  {}  {}", cyan.apply_to("/plan          "), dim.apply_to("Switch to Plan mode — discuss & clarify your task"));
                        println!("  {}  {}", cyan.apply_to("/act           "), dim.apply_to("Switch to Act mode — execute tools"));
                        println!("  {}  {}", cyan.apply_to("/undo          "), dim.apply_to("Undo the last Act-mode run (requires git)"));
                        println!("  {}  {}", cyan.apply_to("/login         "), dim.apply_to("GitHub Copilot device-code OAuth"));
                        println!("  {}  {}", cyan.apply_to("/logout        "), dim.apply_to("Remove saved Copilot credentials"));
                        println!("  {}  {}", cyan.apply_to("/connect       "), dim.apply_to("Pick a provider from a menu"));
                        println!("  {}  {}", cyan.apply_to("/model [name]  "), dim.apply_to("Show current model, or switch to a new one immediately"));
                        println!("  {}  {}", cyan.apply_to("/session       "), dim.apply_to("Browse history or start a new session"));
                        println!("  {}  {}", cyan.apply_to("/clear         "), dim.apply_to("Start a fresh session (same as New in /session)"));
                        println!("  {}  {}", cyan.apply_to("/exit  /quit   "), dim.apply_to("Exit xcodeai"));
                        println!("  {}  {}", cyan.apply_to("Ctrl-C         "), dim.apply_to("Cancel current input line"));
                        println!("  {}  {}", cyan.apply_to("Ctrl-D         "), dim.apply_to("Exit xcodeai"));
                        println!("  {}  {}", cyan.apply_to("(anything else)"), dim.apply_to("Run as an agent task (Act) or discuss (Plan)"));
                        println!();
                        continue;
                    }

                    cmd if cmd.starts_with("/login") => {
                        info("Starting GitHub Copilot device authorization…");
                        match auth::device_code_flow(&reqwest::Client::new()).await {
                            Ok(oauth_token) => match oauth_token.save() {
                                Ok(_) => {
                                    let token_str = oauth_token.access_token.clone();
                                    ctx.llm.set_copilot_oauth_token(token_str).await;
                                    ok("Logged in to GitHub Copilot.");
                                    // Persist provider=copilot to config so next startup
                                    // auto-loads the saved OAuth token without re-login.
                                    if let Err(e) = config::Config::save_provider(
                                        llm::openai::COPILOT_API_BASE, ""
                                    ) {
                                        warn(&format!("Could not save provider to config: {:#}", e));
                                    } else {
                                        info("Provider saved to ~/.config/xcode/config.json — no re-login needed next time.");
                                    }
                                }
                                Err(e) => err(&format!("Failed to save token: {:#}", e)),
                            },
                            Err(e) => err(&format!("Login failed: {:#}", e)),
                        }
                        continue;
                    }

                    "/logout" => {
                        match auth::CopilotOAuthToken::delete() {
                            Ok(_) => ok("Logged out of GitHub Copilot."),
                            Err(e) => err(&format!("Logout failed: {:#}", e)),
                        }
                        continue;
                    }

                    "/connect" => {
                        if let Some((new_base, new_key)) = connect_menu(&mut rl) {
                            if new_base == "copilot_do_login" {
                                // User chose GitHub Copilot — run device-code flow now
                                info("Starting GitHub Copilot device authorization…");
                                match auth::device_code_flow(&reqwest::Client::new()).await {
                                    Ok(oauth_token) => match oauth_token.save() {
                                        Ok(_) => {
                                            let token_str = oauth_token.access_token.clone();
                                            ctx.llm.set_copilot_oauth_token(token_str).await;
                                            ok("Logged in to GitHub Copilot.");
                                            info("Provider set to Copilot. You can start a task now.");
                                            // Persist provider=copilot to config file.
                                            if let Err(e) = config::Config::save_provider(
                                                llm::openai::COPILOT_API_BASE, ""
                                            ) {
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
                                // Non-Copilot provider: apply the new base URL and API key
                                // in-memory right now so the next agent call uses them.
                                ctx.llm.api_base = new_base.clone();
                                ctx.llm.api_key = new_key.clone();
                                ctx.config.provider.api_base = new_base.clone();
                                ctx.config.provider.api_key = new_key.clone();
                                ok(&format!("Provider set to {} (applies immediately).", style(&new_base).green()));
                                // Also persist to disk so the next restart picks it up.
                                match config::Config::save_provider(&new_base, &new_key) {
                                    Ok(_) => info("Saved to ~/.config/xcode/config.json."),
                                    Err(e) => warn(&format!("Could not save to config: {:#}", e)),
                                }
                            }
                        }
                        continue;
                    }

                    cmd if cmd.starts_with("/model") => {
                        let rest = cmd[6..].trim();
                        if rest.is_empty() {
                            // No argument — just show the current model
                            info(&format!("Current model: {}", style(&ctx.config.model).green()));
                        } else {
                            // Apply the new model name immediately — no restart needed.
                            // We update both the in-memory LLM provider and the config
                            // so every subsequent agent call uses the new model.
                            let new_model = rest.to_string();
                            ctx.llm.model = new_model.clone();
                            ctx.config.model = new_model.clone();
                            ok(&format!("Model set to {} (applies immediately).", style(&new_model).green()));
                        }
                        continue;
                    }

                    // Plain-text exit words — no one types these as a coding task
                    "exit" | "quit" | "q" | "bye" | "bye!" | "exit!" | "quit!" => break,

                    // Unknown /xxx command or bare / → show command menu
                    cmd if cmd == "/" || (cmd.starts_with('/') && !cmd.starts_with("/model") && !cmd.starts_with("/login")) => {
                        if let Some(chosen) = show_command_menu() {
                            // Buffer the chosen command so the top of the loop
                            // processes it exactly like direct user input —
                            // zero code duplication.
                            let _ = rl.add_history_entry(&chosen);
                            pending_line = Some(chosen);
                        }
                        continue;
                    }  // end unknown-command arm

                    _ => {} // fall through to agent execution

                }  // end match line.as_str()

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
                        // This is done here (not inside Director) so that the message is
                        // recorded even if the director returns an Err.

                        // ── Pre-run git stash (enables /undo) ────────────────
                        // We fire git stash in a background OS thread BEFORE the agent
                        // starts executing, then collect the result AFTER the run completes.
                        // This way the git stash races the LLM round-trip for free —
                        // the user sees zero extra latency even on large repos.
                        //
                        // We snapshot only tracked changes (no --include-untracked) to
                        // avoid walking large build directories.
                        use std::sync::mpsc;
                        use std::time::Duration;
                        let (stash_tx, stash_rx) =
                            mpsc::channel::<std::io::Result<std::process::Output>>();
                        {
                            let project_dir_clone = ctx.project_dir.clone();
                            std::thread::spawn(move || {
                                let out = std::process::Command::new("git")
                                    .args(["stash", "push", "-m", "xcodeai-undo"])
                                    .current_dir(&project_dir_clone)
                                    .output();
                                let _ = stash_tx.send(out);
                            });
                        }
                        // Stash is now running concurrently.  Start the agent immediately.

                        act_messages.push(llm::Message::user(&line));

                        let result = director
                            .execute(&mut act_messages, &ctx.registry, &ctx.llm, &ctx.tool_ctx)
                            .await;

                        // Agent finished.  Collect the stash result (with a 30-second
                        // grace period for very slow git operations).
                        undo_stash_available = match stash_rx.recv_timeout(Duration::from_secs(30)) {
                            Ok(Ok(out)) if out.status.success() => {
                                let stdout = String::from_utf8_lossy(&out.stdout);
                                !stdout.contains("No local changes to save")
                            }
                            _ => false,
                        };

                        println!();

                        match result {
                            Ok(agent_result) => {
                                ctx.store.add_message(
                                    &sess.id,
                                    &llm::Message::assistant(
                                        Some(agent_result.final_message.clone()),
                                        None,
                                    ),
                                )?;
                                ctx.store.update_session_timestamp(&sess.id)?;

                                print_separator("done");

                                // Build a stats line showing iterations, tool calls,
                                // and auto-continues (if any occurred).
                                let mut stats_parts: Vec<String> = vec![
                                    format!("{} iterations", agent_result.iterations),
                                    format!("{} tool calls", agent_result.tool_calls_total),
                                ];
                                if agent_result.auto_continues > 0 {
                                    stats_parts.push(format!(
                                        "{} auto-continues",
                                        agent_result.auto_continues
                                    ));
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
                                // After every successful Act-mode run, show a compact
                                // `git diff --stat HEAD` if we are inside a git repo.
                                // Errors (not a git repo, git not installed) are silently
                                // ignored — this is cosmetic output only.
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
                                            style("git diff --stat HEAD").dim(),
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
                                // Remove the failed user message so we don't corrupt context
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
                        // before displaying.  The question tool (if called by the
                        // LLM) handles its own interactive display via dialoguer.
                        ctx.llm.set_stream_print(false);
                        let plan_result = run_plan_turn(
                            &conversation_messages,
                            &ctx.llm,
                            &ctx.registry,
                            &ctx.tool_ctx,
                        ).await;
                        // Always re-enable streaming for Act-mode tool calls.
                        ctx.llm.set_stream_print(true);

                        match plan_result {
                            Ok(reply) => {
                                // Display the LLM's final response text.
                                if !reply.trim().is_empty() {
                                    println!("{}", reply.trim_end());
                                    println!();
                                }

                                // Save to conversation history and session store.
                                conversation_messages.push(llm::Message::assistant(
                                    Some(reply.clone()),
                                    None,
                                ));
                                ctx.store.add_message(
                                    &sess.id,
                                    &llm::Message::assistant(Some(reply), None),
                                )?;
                                ctx.store.update_session_timestamp(&sess.id)?;
                            }
                            Err(e) => {
                                err(&format!("{:#}", e));
                                info("Plan mode error. Try again or type /act to switch back.");
                                // Remove the failed user message from history
                                conversation_messages.pop();
                            }
                        }  // match plan_result
                    }  // ReplMode::Plan arm
                }  // match mode
    }  // loop

    let _ = rl.save_history(&history_path);
    println!();
    ok(&format!("Session saved: {}", style(&sess.id).dim()));
    info(&format!("xcodeai session show {}", sess.id));
    println!();

    Ok(())
}

// ─── Session title helper ─────────────────────────────────────────────────────

fn update_session_title(
    store: &session::SessionStore,
    session_id: &str,
    title: &str,
) -> Result<()> {
    store.update_session_title(session_id, title)
}

// ─── Session display commands ─────────────────────────────────────────────────

fn session_list_command(limit: u32) -> Result<()> {
    use session::SessionStore;

    let db_path = SessionStore::default_path()?;
    if !db_path.exists() {
        info("No sessions found.");
        return Ok(());
    }
    let store = SessionStore::new(&db_path)?;
    let sessions = store.list_sessions(limit)?;

    if sessions.is_empty() {
        info("No sessions found.");
    } else {
        println!();
        println!(
            "  {}  {}  {}",
            style(format!("{:<38}", "ID")).dim().bold(),
            style(format!("{:<16}", "DATE")).dim().bold(),
            style("TITLE").dim().bold()
        );
        println!("  {}", style("─".repeat(76)).dim());
        for s in &sessions {
            let date = s.updated_at.format("%Y-%m-%d %H:%M").to_string();
            let title = s.title.as_deref().unwrap_or("(untitled)");
            println!(
                "  {}  {}  {}",
                style(format!("{:<38}", s.id)).cyan(),
                style(format!("{:<16}", date)).dim(),
                title
            );
        }
        println!();
    }
    Ok(())
}

fn session_show_command(id: &str) -> Result<()> {
    use session::SessionStore;

    let db_path = SessionStore::default_path()?;
    if !db_path.exists() {
        err("No sessions database found.");
        std::process::exit(1);
    }
    let store = SessionStore::new(&db_path)?;
    let session = store.get_session(id)?;

    match session {
        None => {
            err(&format!("Session not found: {}", id));
            std::process::exit(1);
        }
        Some(s) => {
            println!();
            println!("  {}  {}", style("Session:").dim(), style(&s.id).cyan());
            println!(
                "  {}  {}",
                style("Title:  ").dim(),
                s.title.as_deref().unwrap_or("(untitled)")
            );
            println!(
                "  {}  {}",
                style("Created:").dim(),
                s.created_at.format("%Y-%m-%d %H:%M:%S UTC")
            );
            println!(
                "  {}  {}",
                style("Updated:").dim(),
                s.updated_at.format("%Y-%m-%d %H:%M:%S UTC")
            );
            println!("  {}", style("─".repeat(60)).dim());

            let messages = store.get_messages(id)?;
            for msg in &messages {
                println!();
                println!("  {}", style(format!("[{}]", msg.role.to_uppercase())).bold());
                if let Some(content) = &msg.content {
                    let display = if content.len() > 2000 {
                        format!("{}…(truncated)", &content[..2000])
                    } else {
                        content.clone()
                    };
                    for line in display.lines() {
                        println!("  {}", line);
                    }
                }
                if let Some(tool_calls_json) = &msg.tool_calls {
                    if let Ok(tcs) =
                        serde_json::from_str::<Vec<serde_json::Value>>(tool_calls_json)
                    {
                        for tc in &tcs {
                            let name = tc["function"]["name"].as_str().unwrap_or("?");
                            let args = tc["function"]["arguments"]
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(100)
                                .collect::<String>();
                            println!(
                                "  {} {}({}…)",
                                style("→").dim(),
                                style(name).yellow(),
                                args
                            );
                        }
                    }
                }
                if let Some(tcid) = &msg.tool_call_id {
                    println!("  {}", style(format!("[tool_call_id: {}]", tcid)).dim());
                }
            }
            println!();
        }
    }
    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_debug_assert() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        cmd.debug_assert();
    }

    #[test]
    fn test_cli_run_subcommand() {
        let cli = Cli::try_parse_from([
            "xcodeai",
            "run",
            "write a hello world",
            "--no-sandbox",
            "--model",
            "gpt-4o",
            "--api-key",
            "test-key",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Run {
                message,
                no_sandbox,
                model,
                api_key,
                ..
            }) => {
                assert_eq!(message, "write a hello world");
                assert!(no_sandbox);
                assert_eq!(model.as_deref(), Some("gpt-4o"));
                assert_eq!(api_key.as_deref(), Some("test-key"));
            }
            _ => panic!("expected Run command"),
        }
    }

    #[test]
    fn test_cli_no_subcommand_is_repl() {
        let cli = Cli::try_parse_from(["xcodeai"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_cli_no_subcommand_with_flags() {
        let cli =
            Cli::try_parse_from(["xcodeai", "--model", "deepseek-chat", "--no-sandbox"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.model.as_deref(), Some("deepseek-chat"));
        assert!(cli.no_sandbox);
    }

    #[test]
    fn test_cli_session_list() {
        let cli =
            Cli::try_parse_from(["xcodeai", "session", "list", "--limit", "5"]).unwrap();
        match cli.command {
            Some(Commands::Session {
                command: SessionCommands::List { limit },
            }) => {
                assert_eq!(limit, 5);
            }
            _ => panic!("expected session list"),
        }
    }

    #[test]
    fn test_cli_session_show() {
        let cli = Cli::try_parse_from(["xcodeai", "session", "show", "abc-123"]).unwrap();
        match cli.command {
            Some(Commands::Session {
                command: SessionCommands::Show { id },
            }) => {
                assert_eq!(id, "abc-123");
            }
            _ => panic!("expected session show"),
        }
    }

    #[test]
    fn test_cli_copilot_provider_url() {
        let cli = Cli::try_parse_from(["xcodeai", "--provider-url", "copilot"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.provider_url.as_deref(), Some("copilot"));
    }
}
