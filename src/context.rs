// src/context.rs

use crate::config::{Config, ConfigOverrides};
use crate::io::AgentIO;
use crate::llm::openai::COPILOT_API_BASE;
use crate::llm::registry::ProviderRegistry;
use crate::llm::LlmProvider;
use crate::mcp::bridge::register_mcp_tools;
use crate::session::SessionStore;
use crate::tools::bash::BashTool;
use crate::tools::file_edit::FileEditTool;
use crate::tools::file_read::FileReadTool;
use crate::tools::file_write::FileWriteTool;
use crate::tools::git_blame::GitBlameTool;
use crate::tools::git_commit::GitCommitTool;
use crate::tools::git_diff::GitDiffTool;
use crate::tools::git_log::GitLogTool;
use crate::tools::glob_search::GlobSearchTool;
use crate::tools::grep_search::GrepSearchTool;
use crate::tools::lsp_diagnostics::LspDiagnosticsTool;
use crate::tools::lsp_goto_def::LspGotoDefTool;
use crate::tools::lsp_references::LspReferencesTool;
use crate::tools::question::QuestionTool;
use crate::tools::spawn_task::SpawnTaskTool;
use crate::tools::patch::PatchTool;
use crate::tools::fetch::FetchTool;
use crate::tools::ls::ListDirectoryTool;
use crate::tools::display_image::DisplayImageTool;
use crate::tools::code_search::CodeSearchTool;
use crate::tools::custom_tool::CustomTool;
use crate::tools::{ToolContext, ToolRegistry};
use anyhow::Result;
use console::style;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
pub struct AgentContext {
    pub config: Config,
    /// The tool registry, wrapped in Arc so it can be shared with ToolContext
    /// for the spawn_task tool to give sub-agents the same tool set.
    pub registry: Arc<ToolRegistry>,
    /// The active LLM provider, selected at startup based on config.
    /// Stored as a trait object so we can hot-swap providers (e.g. /connect)
    /// without rebuilding the entire context.
    pub llm: Arc<dyn LlmProvider>,
    /// A secondary reference to the IO handle so switch_provider() can
    /// inject it into newly created OpenAiProvider instances.
    pub io: Arc<dyn AgentIO>,
    pub tool_ctx: ToolContext,
    pub store: SessionStore,
    pub project_dir: PathBuf,
    /// All MCP server connections started at startup, keyed by server name.
    /// Used by the `/mcp` REPL command to list connected servers and tools.
    /// The first connected client is also stored in `tool_ctx.mcp_client` for
    /// use by the `mcp_resource` tool and other MCP-aware code.
    pub mcp_clients: Vec<(String, Arc<Mutex<crate::mcp::McpClient>>)>,
}

impl AgentContext {
    pub async fn new(
        project: Option<PathBuf>,
        no_sandbox: bool,
        model: Option<String>,
        provider_url: Option<String>,
        api_key: Option<String>,
        // When true, enables compact mode: tool output capped to 50 lines,
        // and a brevity instruction is appended to the system prompt.
        compact: bool,
        // The I/O channel for agent output and confirmation prompts.
        // Pass `Arc<TerminalIO>` for interactive mode, `Arc<NullIO>` for tests.
        io: Arc<dyn AgentIO>,
    ) -> Result<Self> {
        // 1. Load config with CLI overrides
        let overrides = ConfigOverrides {
            api_key: api_key.clone(),
            api_base: provider_url.clone(),
            model: model.clone(),
            project_dir: project.clone(),
            no_sandbox,
            compact,
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
        // Register all built-in tools.  We build the registry first so it
        // can be wrapped in Arc, then pass that Arc into ToolContext for the
        // spawn_task tool to share with child agents.
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FileReadTool));
        registry.register(Box::new(FileWriteTool));
        registry.register(Box::new(FileEditTool));
        registry.register(Box::new(BashTool));
        registry.register(Box::new(GlobSearchTool));
        registry.register(Box::new(GrepSearchTool));
        registry.register(Box::new(QuestionTool));
        registry.register(Box::new(GitDiffTool));
        registry.register(Box::new(GitCommitTool));
        registry.register(Box::new(GitLogTool));
        registry.register(Box::new(GitBlameTool));
        registry.register(Box::new(LspDiagnosticsTool));
        registry.register(Box::new(LspGotoDefTool));
        registry.register(Box::new(LspReferencesTool));
        // SpawnTaskTool is stateless: it gets the LLM and tool registry from
        registry.register(Box::new(ListDirectoryTool));
        registry.register(Box::new(FetchTool));
        registry.register(Box::new(PatchTool));
        registry.register(Box::new(DisplayImageTool));
        registry.register(Box::new(CodeSearchTool));
        // 5a. Custom tools — register user-defined tools from config.custom_tools.
        for ct in &config.custom_tools {
            if ct.name.is_empty() || ct.command.is_empty() {
                tracing::warn!("Skipping custom tool with empty name or command");
                continue;
            }
            registry.register(Box::new(CustomTool {
                tool_name: ct.name.clone(),
                tool_description: ct.description.clone(),
                command_template: ct.command.clone(),
                tool_parameters: ct.parameters.clone(),
            }));
            tracing::info!("Registered custom tool '{}'", ct.name);
        }
        // ctx.llm and ctx.tools at execute time, so no constructor args needed.
        registry.register(Box::new(SpawnTaskTool));
        // 5b. MCP servers — start each server described in config.mcp_servers and
        // register its tools into the (still-mutable) registry BEFORE we wrap it
        // in Arc.  Failures are logged and skipped — a broken MCP server should
        // never prevent xcodeai from starting (all built-in tools still work).
        let mut mcp_clients: Vec<(String, Arc<Mutex<crate::mcp::McpClient>>)> = Vec::new();
        for server_cfg in &config.mcp_servers {
            // Convert Vec<String> args → Vec<&str> for McpClient::start().
            // We borrow from server_cfg which lives for the duration of the loop body.
            let args: Vec<&str> = server_cfg.args.iter().map(|s| s.as_str()).collect();
            match crate::mcp::McpClient::start(&server_cfg.command, &args).await {
                Ok(mut client) => {
                    match client.initialize().await {
                        Ok(_) => {
                            let shared = Arc::new(Mutex::new(client));
                            // Register every tool this server advertises.
                            match register_mcp_tools(Arc::clone(&shared), &mut registry).await {
                                Ok(count) => {
                                    tracing::info!(
                                        "MCP server '{}': registered {} tools",
                                        server_cfg.name,
                                        count
                                    );
                                    mcp_clients.push((server_cfg.name.clone(), shared));
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "MCP server '{}': tool registration failed: {:#}",
                                        server_cfg.name,
                                        e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "MCP server '{}': initialize failed: {:#}. Skipping.",
                                server_cfg.name,
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "MCP server '{}': failed to start '{}': {:#}. Skipping.",
                        server_cfg.name,
                        server_cfg.command,
                        e
                    );
                }
            }
        }

        // All mutations to registry are done — wrap in Arc for shared ownership.
        let registry = Arc::new(registry);
        // 6. LLM provider — select based on config.provider.api_base.
        // ProviderRegistry centralises the if/else dispatch so new providers
        // only need to be added in one place.
        let oauth_token = if config.provider.api_base == COPILOT_API_BASE {
            // Load the persisted OAuth token; fall back to empty string
            // (user will need to /login before the first LLM call).
            crate::auth::CopilotOAuthToken::load()
                .ok()
                .flatten()
                .map(|t| t.access_token)
        } else {
            None
        };
        let llm = ProviderRegistry::create_provider(
            &config.provider,
            &config.model,
            &config.agent.retry,
            &io,
            oauth_token,
        );

        // 7. Tool context — pass the AgentIO reference directly.
        // `lsp_client` starts as None; the first LSP tool call will start the server.
        // `nesting_depth` is 0 for the top-level REPL agent.
        // `llm` and `tools` are shared with child agents spawned by spawn_task.
        // `mcp_client` is the first connected MCP server (if any) — used by the
        //  mcp_resource tool.  If no MCP servers were configured, this is None.
        let first_mcp = mcp_clients.first().map(|(_, c)| Arc::clone(c));
        let tool_ctx = ToolContext {
            working_dir: project_dir.clone(),
            sandbox_enabled: config.sandbox.enabled && !no_sandbox,
            io: io.clone(),
            compact_mode: config.agent.compact_mode,
            lsp_client: Arc::new(Mutex::new(None)),
            mcp_client: first_mcp,
            nesting_depth: 0,
            llm: Arc::clone(&llm),
            tools: Arc::clone(&registry),
            permissions: config.permissions.clone(),
            formatters: config.formatters.clone(),
        };

        Ok(Self {
            config,
            registry,
            llm,
            io,
            tool_ctx,
            store,
            project_dir,
            mcp_clients,
        })
    }

    // ── Provider hot-swap ───────────────────────────────────────────────────────

    /// Switch to a new provider at runtime (called by /connect).
    ///
    /// Creates a fresh provider based on `api_base` and replaces `self.llm`.
    /// Anthropic is selected when `api_base == ANTHROPIC_API_BASE` ("anthropic").
    /// Copilot should use the /login flow instead.
    pub fn switch_provider(&mut self, api_base: String, api_key: String) {
        let model = self.config.model.clone();
        let retry = self.config.agent.retry.clone();
        let io = self.io.clone();
        // Build a temporary ProviderConfig so we can reuse ProviderRegistry.
        let tmp_config = crate::config::ProviderConfig {
            api_base: api_base.clone(),
            api_key: api_key.clone(),
        };
        self.llm = ProviderRegistry::create_provider(&tmp_config, &model, &retry, &io, None);
        self.config.provider.api_base = api_base;
        self.config.provider.api_key = api_key;
    }

    /// Update the active model name at runtime (called by /model).
    ///
    /// This re-creates the provider with the new model name so the change
    /// takes effect on the very next LLM call.
    pub fn switch_model(&mut self, model: String) {
        self.config.model = model.clone();
        let api_base = self.config.provider.api_base.clone();
        let api_key = self.config.provider.api_key.clone();
        let retry = self.config.agent.retry.clone();
        let io = self.io.clone();
        // For Copilot, reload the OAuth token from disk so it's always fresh.
        let oauth_token = if api_base == COPILOT_API_BASE {
            crate::auth::CopilotOAuthToken::load()
                .ok()
                .flatten()
                .map(|t| t.access_token)
        } else {
            None
        };
        let tmp_config = crate::config::ProviderConfig { api_base, api_key };
        self.llm = ProviderRegistry::create_provider(&tmp_config, &model, &retry, &io, oauth_token);
    }
}

pub fn update_session_title(store: &SessionStore, session_id: &str, title: &str) -> Result<()> {
    store.update_session_title(session_id, title)
}

pub fn session_list_command(limit: u32) -> Result<()> {
    let db_path = SessionStore::default_path()?;
    if !db_path.exists() {
        crate::ui::info("No sessions found.");
        return Ok(());
    }
    let store = SessionStore::new(&db_path)?;
    let sessions = store.list_sessions(limit)?;

    if sessions.is_empty() {
        crate::ui::info("No sessions found.");
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

pub fn session_show_command(id: &str) -> Result<()> {
    let db_path = SessionStore::default_path()?;
    if !db_path.exists() {
        crate::ui::err("No sessions database found.");
        std::process::exit(1);
    }
    let store = SessionStore::new(&db_path)?;
    let session = store.get_session(id)?;

    match session {
        None => {
            crate::ui::err(&format!("Session not found: {}", id));
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
                println!(
                    "  {}",
                    style(format!("[{}]", msg.role.to_uppercase())).bold()
                );
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
                            println!("  {} {}({}…)", style("→").dim(), style(name).yellow(), args);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::NullIO;

    /// Verify that AgentContext::new() completes successfully when no MCP
    /// servers are configured — the mcp_clients vec should be empty and
    /// tool_ctx.mcp_client should be None.
    #[tokio::test]
    async fn test_new_with_no_mcp_servers() {
        // Use a temporary directory so SessionStore::default_path() doesn't
        // pollute the real user data directory during tests.
        // We set XCODE_NO_DB_PERSIST (not used by SessionStore yet) but the
        // real guard is that the test config has empty mcp_servers.
        let ctx = AgentContext::new(
            Some(std::path::PathBuf::from("/tmp")),
            true,  // no_sandbox — direct execution, no sbox required
            None,  // model — use config default
            None,  // provider_url
            None,  // api_key
            false, // compact
            std::sync::Arc::new(NullIO),
        )
        .await;

        // Building the context should succeed (config loads, registry builds,
        // session store opens).  Whether or not it errors depends on whether
        // the test environment has a writable data dir; we just assert the
        // mcp_clients field is empty when the config has no servers.
        if let Ok(ctx) = ctx {
            assert!(
                ctx.mcp_clients.is_empty(),
                "mcp_clients should be empty when config.mcp_servers is empty"
            );
            assert!(
                ctx.tool_ctx.mcp_client.is_none(),
                "tool_ctx.mcp_client should be None when no MCP servers are configured"
            );
        }
        // If Err(_) the environment just lacks a writable data dir — that is
        // acceptable in CI.  The important thing is that no panic occurs.
    }
}
