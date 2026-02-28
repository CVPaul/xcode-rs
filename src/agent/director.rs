use crate::agent::Agent;
use crate::agent::{AgentResult};
use crate::config::AgentConfig;
use crate::llm::{Message, LlmProvider};
use crate::tools::{ToolContext, ToolRegistry};
use anyhow::Result;
use crate::agent::orchestrator::OrchestratorAgent;

pub struct Director { pub config: AgentConfig }

impl Director {
    pub fn new(config: AgentConfig) -> Self { Director { config } }
    pub async fn execute(
        &self,
        user_message: &str,
        tools: &ToolRegistry,
        llm: &dyn LlmProvider,
        ctx: &ToolContext,
    ) -> Result<AgentResult> {
        let orchestrator = OrchestratorAgent::new(self.config.clone());
        let mut messages = vec![
            Message::system(orchestrator.system_prompt()),
            Message::user(user_message),
        ];
        orchestrator.run(&mut messages, tools, llm, ctx).await
    }
}
