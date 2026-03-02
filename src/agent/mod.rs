use crate::llm::{LlmProvider, Message};
use crate::tools::{ToolContext, ToolRegistry};
use crate::tracking::SessionTracker;
use anyhow::Result;
use async_trait::async_trait;

pub mod agents_md;
pub mod coder;
pub mod context_manager;
pub mod director;
pub mod orchestrator;

#[async_trait]
#[allow(dead_code)] // `name()` is part of the trait API; used by implementations
pub trait Agent: Send + Sync {
    fn name(&self) -> &str;
    fn system_prompt(&self) -> String;
    async fn run(
        &self,
        messages: &mut Vec<Message>,
        tools: &ToolRegistry,
        llm: &dyn LlmProvider,
        ctx: &ToolContext,
    ) -> Result<AgentResult>;
}

#[derive(Debug, Clone)]
pub struct AgentResult {
    pub final_message: String,
    pub iterations: u32,
    pub tool_calls_total: u32,
    /// Number of times the harness injected a "continue" message because the
    /// LLM stopped mid-task (returned text without `[TASK_COMPLETE]` and without
    /// tool calls).  Displayed in the completion banner so the user knows the
    /// agent ran autonomously beyond a single LLM turn.
    pub auto_continues: u32,
    /// Accumulated token usage across all LLM calls in this task.
    /// Used for the completion banner and stored in session SQLite.
    pub tracker: SessionTracker,
}
