// The Director is the entry point for Act-mode agent execution.
//
// Design change (v0.7.0): Director no longer creates a fresh message list on
// every call.  Instead the REPL passes in a *mutable reference* to its shared
// `conversation_messages` vector, so multi-turn context is fully preserved.
//
// The OrchestratorAgent indirection has been removed: Director calls
// CoderAgent directly.  There is nothing the Orchestrator was doing that
// isn't better expressed as a plain function call.

use crate::agent::coder::CoderAgent;
use crate::agent::{Agent, AgentResult};
use crate::config::AgentConfig;
use crate::llm::{LlmProvider, Message};
use crate::tools::{ToolContext, ToolRegistry};
use anyhow::Result;

pub struct Director {
    pub config: AgentConfig,
}

impl Director {
    pub fn new(config: AgentConfig) -> Self {
        Director { config }
    }

    /// Execute one user turn, preserving full conversation history.
    ///
    /// `messages` is the *shared* history owned by the REPL loop.  On the
    /// first call it should contain only the CoderAgent system prompt.  On
    /// subsequent calls it will already hold the full conversation so the
    /// model has complete context.
    ///
    /// The caller is responsible for appending the new user `Message` to
    /// `messages` *before* calling this method, so the history is consistent
    /// regardless of whether this method returns Ok or Err.
    pub async fn execute(
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
