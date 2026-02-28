use crate::agent::coder::CoderAgent;
use crate::agent::{Agent, AgentResult};
use crate::config::AgentConfig;
use crate::llm::{LlmProvider, Message};
use crate::tools::{ToolContext, ToolRegistry};
use anyhow::Result;
use async_trait::async_trait;

pub struct OrchestratorAgent {
    pub config: AgentConfig,
}

impl OrchestratorAgent {
    pub fn new(config: AgentConfig) -> Self {
        OrchestratorAgent { config }
    }
}

#[async_trait]
impl Agent for OrchestratorAgent {
    fn name(&self) -> &str {
        "orchestrator"
    }
    fn system_prompt(&self) -> &str {
        "You are an orchestrator that delegates coding tasks to specialized agents. \
        For v1, you directly handle the task yourself."
    }
    async fn run(
        &self,
        messages: &mut Vec<Message>,
        tools: &ToolRegistry,
        llm: &dyn LlmProvider,
        ctx: &ToolContext,
    ) -> Result<AgentResult> {
        let coder = CoderAgent::new(self.config.clone());
        coder.run(messages, tools, llm, ctx).await
    }
}
