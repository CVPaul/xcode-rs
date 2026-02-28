mod agent;
mod config;
mod llm;
mod sandbox;
mod session;
mod tools;

use clap::{Parser, Subcommand};

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
        /// The task message
        message: String,
        #[arg(long, short)]
        project: Option<std::path::PathBuf>,
        #[arg(long)]
        no_sandbox: bool,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        provider_url: Option<String>,
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
    /// Show a session
    Show { id: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt::init();
    match cli.command {
        Commands::Run { message, .. } => {
            println!("TODO: run agent with message: {message}");
        }
        Commands::Session { command } => match command {
            SessionCommands::List { limit } => {
                println!("TODO: list {limit} sessions");
            }
            SessionCommands::Show { id } => {
                println!("TODO: show session {id}");
            }
        },
    }
    Ok(())
}
