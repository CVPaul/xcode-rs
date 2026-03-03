use crate::io::AgentIO;
use crate::llm::LlmProvider;
use crate::lsp::LspClient;
use crate::mcp::McpClient;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod bash;
pub mod file_edit;
pub mod file_read;
pub mod file_write;
pub mod fetch;
pub mod git_blame;
pub mod git_commit;
pub mod git_diff;
pub mod git_log;
pub mod glob_search;
pub mod grep_search;
pub mod lsp_diagnostics;
pub mod lsp_goto_def;
pub mod lsp_references;
pub mod ls;
pub mod mcp_resource;
pub mod question;
pub mod spawn_task;
pub mod patch;
pub mod display_image;
pub mod code_search;
pub mod custom_tool;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult>;
}

#[derive(Clone)]
pub struct ToolContext {
    pub working_dir: PathBuf,
    /// Whether to route tool execution through the sandbox process.
    /// Currently set but not yet acted upon — sandbox integration is future work.
    #[allow(dead_code)]
    pub sandbox_enabled: bool,
    /// I/O channel used by the agent for status output and confirmation prompts.
    /// In terminal mode this is `Arc<TerminalIO>`; in tests it is `Arc<NullIO>`.
    /// A future HTTP mode will pass `Arc<HttpIO>` here without touching the agent.
    pub io: Arc<dyn AgentIO>,
    /// When true, tool output is capped to 50 lines to save tokens.
    /// Set via `--compact` CLI flag or `/compact` REPL command.
    pub compact_mode: bool,
    /// Lazily-started LSP client shared across all LSP tools.
    /// Wrapped in `Arc<Mutex<Option<...>>>` so multiple tools can share it.
    /// The first LSP tool call to run will start and initialize the server.
    /// `None` if LSP is disabled in config or the server failed to start.
    pub lsp_client: Arc<Mutex<Option<LspClient>>>,
    /// Optional MCP client connection, shared across MCP-related tools.
    /// `None` when no MCP server is connected.
    /// Wrapped in `Option<Arc<Mutex<...>>>` so it can be absent and shared.
    #[allow(dead_code)]
    pub mcp_client: Option<Arc<Mutex<McpClient>>>,
    /// How many levels deep the current agent is in a spawn_task nesting chain.
    /// spawn_task increments this for child agents; at depth >= 3 it refuses
    /// to spawn further so the stack cannot grow unboundedly.
    /// The top-level REPL agent starts at depth 0.
    pub nesting_depth: u32,
    /// The active LLM provider for this agent context.
    /// spawn_task needs access to the provider to give it to child agents.
    /// We store it here as `Arc<dyn LlmProvider>` so it can be cloned cheaply.
    pub llm: Arc<dyn LlmProvider>,
    /// The full tool registry, shared via Arc so spawn_task can give child
    /// agents the same set of tools without re-constructing the registry.
    pub tools: Arc<ToolRegistry>,
    /// Permission rules from config — used by the agent loop to check whether a
    /// tool call needs explicit user confirmation.
    pub permissions: Vec<crate::config::PermissionRule>,
    /// Code formatters keyed by file extension.  After file_write/file_edit
    /// succeeds, the tool checks this map and runs the formatter if one matches.
    pub formatters: std::collections::HashMap<String, String>,
}

pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn list_definitions(&self) -> Vec<serde_json::Value> {
        self.tools
            .values()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters_schema()
                    }
                })
            })
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::file_edit::FileEditTool;
    use crate::tools::file_read::FileReadTool;
    use crate::tools::file_write::FileWriteTool;

    #[test]
    fn test_registry_register_and_get() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FileReadTool));
        registry.register(Box::new(FileWriteTool));
        registry.register(Box::new(FileEditTool));
        assert!(registry.get("file_read").is_some());
        assert!(registry.get("file_write").is_some());
        assert!(registry.get("file_edit").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_registry_definitions() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FileReadTool));
        registry.register(Box::new(FileWriteTool));
        registry.register(Box::new(FileEditTool));
        let defs = registry.list_definitions();
        assert_eq!(defs.len(), 3);
        for def in &defs {
            assert_eq!(def["type"], "function");
            assert!(def["function"]["name"].is_string());
            assert!(def["function"]["description"].is_string());
            assert!(def["function"]["parameters"].is_object());
        }
    }
}
