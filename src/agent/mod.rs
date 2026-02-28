use async_trait::async_trait;
use anyhow::Result;
use crate::llm::{Message, LlmProvider, ToolDefinition, Role};
use crate::tools::{ToolContext, ToolRegistry};

pub mod coder;
pub mod orchestrator;
pub mod director;

pub use director::Director;

#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &str;
    fn system_prompt(&self) -> &str;
    async fn run(&self, messages: &mut Vec<Message>, tools: &ToolRegistry, llm: &dyn LlmProvider, ctx: &ToolContext) -> Result<AgentResult>;
}

#[derive(Debug, Clone)]
pub struct AgentResult {
    pub final_message: String,
    pub iterations: u32,
    pub tool_calls_total: u32,
}

/// Truncate messages to fit within approximate token budget.
/// Strategy: keep system prompt (index 0) + first user message (index 1) + last N messages that fit.
/// Rough heuristic: 4 chars ≈ 1 token, budget = 100_000 tokens * 4 = 400_000 chars.
pub fn truncate_messages(messages: &mut Vec<Message>, budget_chars: usize) {
    use tracing::warn;
    if messages.is_empty() { return; }
    let total_chars: usize = messages.iter().filter_map(|m| m.content.as_ref()).map(|c| c.len()).sum();
    if total_chars <= budget_chars { return; }
    // Always keep system prompt and first user message
    let mut keep = vec![];
    if messages.len() > 0 { keep.push(messages[0].clone()); }
    if messages.len() > 1 { keep.push(messages[1].clone()); }
    // Find how many messages from the end fit in the budget
    let mut chars = keep.iter().filter_map(|m| m.content.as_ref()).map(|c| c.len()).sum::<usize>();
    let mut tail = Vec::new();
    for m in messages.iter().skip(2).rev() {
        let mc = m.content.as_ref().map(|c| c.len()).unwrap_or(0);
        if chars + mc > budget_chars { break; }
        tail.push(m.clone());
        chars += mc;
    }
    tail.reverse();
    let truncated_count = messages.len() - (keep.len() + tail.len());
    if truncated_count > 0 {
        warn!("Truncating {} messages for context window", truncated_count);
        keep.push(Message::user(format!("[... {} messages truncated for context window ...]", truncated_count)));
    }
    keep.extend(tail);
    *messages = keep;
}

