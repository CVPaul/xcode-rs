mod agent;
mod auth;
mod config;
mod llm;
mod lsp;
mod sandbox;
mod session;
mod tools;

mod context;
mod io;
mod mcp;
mod orchestrator;
mod repl;
mod tracking;
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use context::{session_list_command, session_show_command, AgentContext};

use std::path::PathBuf;

mod http;
mod ui;
use ui::{err, info, ok, print_separator};

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
    /// Prompt for confirmation before destructive tool calls (bash rm, file_write overwrites, etc.)
    /// By default xcodeai runs fully autonomously without prompting.
    #[arg(long, global = false)]
    confirm: bool,
    /// Skip loading AGENTS.md project rules
    #[arg(long, global = false)]
    no_agents_md: bool,
    /// Enable compact mode: cap tool output to 50 lines and use a brevity-focused prompt
    #[arg(long, global = false)]
    compact: bool,
    /// Disable markdown rendering of agent output (show raw text instead of styled)
    #[arg(long, global = false)]
    no_markdown: bool,
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
        /// Skip loading AGENTS.md project rules
        #[arg(long)]
        no_agents_md: bool,
        /// Enable compact mode: cap tool output to 50 lines and use a brevity-focused prompt
        #[arg(long)]
        compact: bool,
        /// Disable markdown rendering of agent output (show raw text instead of styled)
        #[arg(long)]
        no_markdown: bool,
    },
    /// Manage sessions
    Session {
        #[command(subcommand)]
        command: SessionCommands,
    },
    /// Start the HTTP API server
    Serve {
        /// Port to listen on (default: 8080)
        #[arg(long, default_value = "8080")]
        port: u16,
        /// Host/interface to bind to (default: 0.0.0.0)
        #[arg(long, default_value = "0.0.0.0")]
        host: String,
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
            repl::repl_command(
                cli.project,
                cli.no_sandbox,
                cli.model,
                cli.provider_url,
                cli.api_key,
                cli.confirm,
                cli.no_agents_md,
                cli.compact,
                cli.no_markdown,
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
            no_agents_md,
            compact,
            no_markdown,
        }) => {
            run_command(
                message,
                project,
                no_sandbox,
                model,
                provider_url,
                api_key,
                no_agents_md,
                compact,
                no_markdown,
            )
            .await?;
        }
        Some(Commands::Session { command }) => match command {
            SessionCommands::List { limit } => session_list_command(limit)?,
            SessionCommands::Show { id } => session_show_command(&id)?,
        },
        Some(Commands::Serve { port, host }) => {
            serve_command(host, port).await?;
        }
    }

    Ok(())
}

// ─── `xcodeai run "task"` ─────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn run_command(
    message: String,
    project: Option<PathBuf>,
    no_sandbox: bool,
    model: Option<String>,
    provider_url: Option<String>,
    api_key: Option<String>,
    no_agents_md: bool,
    // When true, enables compact mode: tool output capped to 50 lines,
    // system prompt gets a brevity instruction.
    compact: bool,
    // When true, disable markdown rendering in terminal output.
    // NOTE: run_command uses AutoApproveIO (non-interactive), so no_markdown
    // doesn't affect output here — it's accepted for CLI symmetry with REPL mode.
    #[allow(unused_variables)] no_markdown: bool,
) -> Result<()> {
    use agent::director::Director;
    use agent::Agent;
    use session::auto_title;

    // `run_command` is the non-interactive batch mode (`xcodeai run "task"`).
    // We use AutoApproveIO rather than TerminalIO so that the agent never blocks
    // waiting for stdin confirmation — there is no human present to answer.
    // All tool output is still printed to stderr (same as TerminalIO).
    let ctx = AgentContext::new(
        project,
        no_sandbox,
        model,
        provider_url,
        api_key,
        compact,
        std::sync::Arc::new(io::AutoApproveIO),
    )
    .await?;

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
    //
    // Load AGENTS.md from the project directory (unless --no-agents-md was passed).
    // This prepends project-specific rules to the system prompt so the agent
    // is immediately aware of the project's conventions.
    let agents_md = if no_agents_md {
        None
    } else {
        agent::agents_md::load_agents_md(&ctx.project_dir)
    };
    if agents_md.is_some() {
        ctx.tool_ctx
            .io
            .show_status("\u{1F4CB} Loaded project rules from AGENTS.md")
            .await
            .ok();
    }
    let coder_system_prompt =
        agent::coder::CoderAgent::new_with_agents_md(ctx.config.agent.clone(), agents_md)
            .system_prompt();
    let mut messages = vec![
        llm::Message::system(&coder_system_prompt),
        llm::Message::user(&message),
    ];
    let result = director
        .execute(
            &mut messages,
            ctx.registry.as_ref(),
            ctx.llm.as_ref(),
            &ctx.tool_ctx,
        )
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

// ─── `xcodeai serve` ─────────────────────────────────────────────────────────

/// Start the HTTP API server.
///
/// Parses `host:port` into a `SocketAddr`, loads the default xcodeai config,
/// then delegates to `http::start_server()` which binds the socket and blocks.
async fn serve_command(host: String, port: u16) -> Result<()> {
    use std::net::SocketAddr;

    // Parse into a SocketAddr.  If the host is a bare hostname (not an IP
    // address), convert it to a numeric address with a simple DNS lookup.
    let addr: SocketAddr = format!("{host}:{port}").parse()?;

    // Load the xcodeai config (env vars + config file).  Provider credentials
    // are NOT required here — the serve command itself doesn't call any LLM.
    // They'll be required when an agent-loop request hits POST /sessions/:id/messages.
    let config = crate::config::Config::load(&crate::config::ConfigOverrides::default())?;

    info(&format!("Starting HTTP server on http://{addr}"));
    http::start_server(config, addr).await?;

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
        let cli = Cli::try_parse_from(["xcodeai", "session", "list", "--limit", "5"]).unwrap();
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
