mod agent;
mod config;
mod llm;
mod sandbox;
mod session;
mod tools;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "xcode", version, about = "Autonomous AI coding agent")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run an autonomous coding task
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            message,
            project,
            no_sandbox,
            model,
            provider_url,
            api_key,
        } => {
            run_command(message, project, no_sandbox, model, provider_url, api_key).await?;
        }
        Commands::Session { command } => match command {
            SessionCommands::List { limit } => session_list_command(limit)?,
            SessionCommands::Show { id } => session_show_command(&id)?,
        },
    }

    Ok(())
}

async fn run_command(
    message: String,
    project: Option<PathBuf>,
    no_sandbox: bool,
    model: Option<String>,
    provider_url: Option<String>,
    api_key: Option<String>,
) -> Result<()> {
    use agent::director::Director;
    use config::{Config, ConfigOverrides};
    use llm::openai::OpenAiProvider;
    use sandbox::NoSandbox;
    use session::{auto_title, SessionStore};
    use tools::bash::BashTool;
    use tools::file_edit::FileEditTool;
    use tools::file_read::FileReadTool;
    use tools::file_write::FileWriteTool;
    use tools::glob_search::GlobSearchTool;
    use tools::grep_search::GrepSearchTool;
    use tools::ToolContext;
    use tools::ToolRegistry;

    // 1. Load config with CLI overrides
    let overrides = ConfigOverrides {
        api_key: api_key.clone(),
        api_base: provider_url.clone(),
        model: model.clone(),
        project_dir: project.clone(),
        no_sandbox,
    };
    let config = Config::load(&overrides)?;

    // 2. Validate API key
    if config.provider.api_key.is_empty() {
        bail!(
            "API key not configured. Set XCODE_API_KEY environment variable or add to config file at ~/.config/xcode/config.json"
        );
    }

    // 3. Determine project directory
    let project_dir = config
        .project_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // 4. Create session store and new session
    let db_path = SessionStore::default_path()?;
    let store = SessionStore::new(&db_path)?;
    let title = auto_title(&message);
    let session = store.create_session(Some(&title))?;
    tracing::info!("Session created: {}", session.id);

    // 5. Register tools
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FileReadTool));
    registry.register(Box::new(FileWriteTool));
    registry.register(Box::new(FileEditTool));
    registry.register(Box::new(BashTool));
    registry.register(Box::new(GlobSearchTool));
    registry.register(Box::new(GrepSearchTool));

    // 6. Create LLM provider
    let llm = OpenAiProvider::new(
        config.provider.api_base.clone(),
        config.provider.api_key.clone(),
        config.model.clone(),
    );

    // 7. Create tool context (no-sandbox for now; sbox lifecycle added when needed)
    let ctx = ToolContext {
        working_dir: project_dir.clone(),
        sandbox_enabled: config.sandbox.enabled && !no_sandbox,
    };

    // 8. Save initial user message to session
    store.add_message(&session.id, &llm::Message::user(&message))?;

    // 9. Run the director/agent loop
    let director = Director::new(config.agent.clone());
    let result = director.execute(&message, &registry, &llm, &ctx).await;

    match result {
        Ok(agent_result) => {
            // 10. Save final assistant message
            store.add_message(
                &session.id,
                &llm::Message::assistant(Some(agent_result.final_message.clone()), None),
            )?;
            // Update session timestamp
            store.update_session_timestamp(&session.id)?;

            // 11. Print summary
            println!("\n────────────────────────────────────────");
            println!("✓ Task complete");
            println!("  Session ID : {}", session.id);
            println!("  Iterations : {}", agent_result.iterations);
            println!("  Tool calls : {}", agent_result.tool_calls_total);
            println!("────────────────────────────────────────");
            println!("{}", agent_result.final_message);
        }
        Err(e) => {
            tracing::error!("Agent error: {:#}", e);
            eprintln!("Error: {:#}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}

fn session_list_command(limit: u32) -> Result<()> {
    use session::SessionStore;

    let db_path = SessionStore::default_path()?;
    if !db_path.exists() {
        println!("No sessions found.");
        return Ok(());
    }
    let store = SessionStore::new(&db_path)?;
    let sessions = store.list_sessions(limit)?;

    if sessions.is_empty() {
        println!("No sessions found.");
    } else {
        println!("{:<38} {:<20} {}", "ID", "DATE", "TITLE");
        println!("{}", "─".repeat(80));
        for s in &sessions {
            let date = s.updated_at.format("%Y-%m-%d %H:%M").to_string();
            let title = s.title.as_deref().unwrap_or("(untitled)");
            println!("{:<38} {:<20} {}", s.id, date, title);
        }
    }
    Ok(())
}

fn session_show_command(id: &str) -> Result<()> {
    use session::SessionStore;

    let db_path = SessionStore::default_path()?;
    if !db_path.exists() {
        eprintln!("No sessions database found.");
        std::process::exit(1);
    }
    let store = SessionStore::new(&db_path)?;
    let session = store.get_session(id)?;

    match session {
        None => {
            eprintln!("Session not found: {}", id);
            std::process::exit(1);
        }
        Some(s) => {
            println!("Session: {}", s.id);
            println!("Title  : {}", s.title.as_deref().unwrap_or("(untitled)"));
            println!("Created: {}", s.created_at.format("%Y-%m-%d %H:%M:%S UTC"));
            println!("Updated: {}", s.updated_at.format("%Y-%m-%d %H:%M:%S UTC"));
            println!("{}", "─".repeat(60));

            let messages = store.get_messages(id)?;
            for msg in &messages {
                println!("\n[{}]", msg.role.to_uppercase());
                if let Some(content) = &msg.content {
                    // Truncate very long content for display
                    if content.len() > 2000 {
                        println!("{}...(truncated)", &content[..2000]);
                    } else {
                        println!("{}", content);
                    }
                }
                if let Some(tool_calls_json) = &msg.tool_calls {
                    // Show tool call summary
                    if let Ok(tcs) = serde_json::from_str::<Vec<serde_json::Value>>(tool_calls_json)
                    {
                        for tc in &tcs {
                            let name = tc["function"]["name"].as_str().unwrap_or("?");
                            let args = tc["function"]["arguments"]
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .take(100)
                                .collect::<String>();
                            println!("  → tool_call: {}({}...)", name, args);
                        }
                    }
                }
                if let Some(tcid) = &msg.tool_call_id {
                    println!("  [tool_call_id: {}]", tcid);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parses_run() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        cmd.debug_assert();
    }

    #[test]
    fn test_cli_run_subcommand() {
        let cli = Cli::try_parse_from([
            "xcode",
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
            Commands::Run {
                message,
                no_sandbox,
                model,
                api_key,
                ..
            } => {
                assert_eq!(message, "write a hello world");
                assert!(no_sandbox);
                assert_eq!(model.as_deref(), Some("gpt-4o"));
                assert_eq!(api_key.as_deref(), Some("test-key"));
            }
            _ => panic!("expected Run command"),
        }
    }

    #[test]
    fn test_cli_session_list() {
        let cli = Cli::try_parse_from(["xcode", "session", "list", "--limit", "5"]).unwrap();
        match cli.command {
            Commands::Session {
                command: SessionCommands::List { limit },
            } => {
                assert_eq!(limit, 5);
            }
            _ => panic!("expected session list"),
        }
    }

    #[test]
    fn test_cli_session_show() {
        let cli = Cli::try_parse_from(["xcode", "session", "show", "abc-123"]).unwrap();
        match cli.command {
            Commands::Session {
                command: SessionCommands::Show { id },
            } => {
                assert_eq!(id, "abc-123");
            }
            _ => panic!("expected session show"),
        }
    }
}
