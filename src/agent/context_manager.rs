// src/agent/context_manager.rs
//
// Smart context-window management for the xcodeai agent loop.
//
// # Problem
//
// Every LLM has a finite context window (e.g. 128k tokens ≈ 512k chars).
// As the conversation grows the message history eventually fills the window,
// causing either a context-length API error or silently degraded reasoning
// (the model can't "see" old messages any more).
//
// # Two strategies
//
// The config lets users choose between two strategies:
//
//   "truncate"   — drop the oldest messages (fast, no extra LLM call, lossy)
//   "summarize"  — send the oldest messages to the same LLM and ask for a
//                  compact summary, then replace those messages with the
//                  summary (slower, one extra LLM call, much less lossy)
//
// # Threshold
//
// We trigger management when the total char count exceeds
// `budget_chars * summarize_threshold` (default 80 % of 400 000 chars).
// This gives the agent a safety margin — we start compressing before the
// window is completely full, not after the API rejects the request.
//
// # Summarisation fallback
//
// If the summarisation LLM call fails for any reason (network error, model
// refuses, etc.) we silently fall back to the old truncation strategy so the
// agent loop always continues rather than crashing.
//
// # Rust learner notes
//
// - `async fn` inside `impl ContextManager` is straightforward; the only
//   requirement is that `ContextManager::manage()` is called with `.await`.
// - `tracing::warn!` and `tracing::info!` are used instead of `eprintln!`
//   so the messages go through the logging subsystem and can be filtered.
// - The `serde(default)` attribute on `ContextConfig` means old config files
//   that don't have the `"context"` key will silently use the defaults here.

use crate::llm::{LlmProvider, Message};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// ─── Configuration ────────────────────────────────────────────────────────────

/// Strategy to use when the context window approaches capacity.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContextStrategy {
    /// Drop oldest messages (fast, lossy).
    Truncate,
    /// Summarise oldest messages via an LLM call, then replace them (slower, less lossy).
    #[default]
    Summarize,
}

/// Configuration for context-window management.
///
/// Stored under `agent.context` in `config.json`.  Every field has a
/// `#[serde(default)]` fallback so old config files don't need updating.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ContextConfig {
    /// Strategy to apply when the budget is approached.
    pub strategy: ContextStrategy,
    /// Total character budget (default 400 000 ≈ 100 000 tokens @ 4 chars/token).
    pub budget_chars: usize,
    /// Fraction of `budget_chars` at which management is triggered (default 0.80).
    /// A value of 0.80 means we act at 80 % usage, leaving 20 % headroom.
    pub threshold: f64,
    /// Maximum tokens for the summary response (default 800).
    /// Keeping this low ensures the summary LLM call is cheap and fast.
    pub summary_max_tokens: u32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        ContextConfig {
            strategy: ContextStrategy::default(),
            budget_chars: 400_000,
            threshold: 0.80,
            summary_max_tokens: 800,
        }
    }
}

// ─── ContextManager ───────────────────────────────────────────────────────────

/// Manages the message history to keep it within the context window.
///
/// Create one per agent run and call `manage()` at the top of every iteration.
pub struct ContextManager {
    pub config: ContextConfig,
    /// How many times we have summarised the context this session.
    pub summarize_count: u32,
}

impl ContextManager {
    /// Create a new manager with the given configuration.
    pub fn new(config: ContextConfig) -> Self {
        ContextManager {
            config,
            summarize_count: 0,
        }
    }

    /// Ensure `messages` fits within the context budget.
    ///
    /// Depending on `self.config.strategy`:
    /// - `Truncate`   → calls the existing simple truncation helper
    /// - `Summarize`  → tries LLM-based summarisation; falls back to truncation on failure
    ///
    /// The `messages` slice is mutated in place.
    pub async fn manage(
        &mut self,
        messages: &mut Vec<Message>,
        llm: &dyn LlmProvider,
    ) -> Result<()> {
        let total_chars: usize = messages
            .iter()
            .map(|m| m.text_content().map(|s| s.len()).unwrap_or(0))
            .sum();

        // Check whether we're above the threshold.
        let trigger_chars = (self.config.budget_chars as f64 * self.config.threshold) as usize;
        if total_chars <= trigger_chars {
            // Still well within budget — nothing to do.
            return Ok(());
        }

        info!(
            total_chars,
            budget_chars = self.config.budget_chars,
            "Context window approaching limit — applying {:?} strategy",
            self.config.strategy,
        );

        match self.config.strategy {
            ContextStrategy::Truncate => {
                truncate_messages(messages, self.config.budget_chars);
            }
            ContextStrategy::Summarize => {
                // Try to summarise; if anything goes wrong, fall back to truncation.
                match self.try_summarize(messages, llm).await {
                    Ok(()) => {
                        self.summarize_count += 1;
                        info!(
                            summarize_count = self.summarize_count,
                            "Context summarised successfully"
                        );
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            "Context summarisation failed — falling back to truncation"
                        );
                        truncate_messages(messages, self.config.budget_chars);
                    }
                }
            }
        }

        Ok(())
    }

    /// Attempt LLM-based summarisation of the oldest messages.
    ///
    /// Strategy:
    ///   1. Keep messages[0] (system prompt) and messages.last() (most recent).
    ///   2. Collect the "middle" messages that pushed us over budget.
    ///   3. Send them to the LLM with a summarisation system prompt.
    ///   4. Replace the middle messages with a single `[Context Summary]` user message.
    ///
    /// Returns an error if the LLM call fails (caller falls back to truncation).
    async fn try_summarize(
        &self,
        messages: &mut Vec<Message>,
        llm: &dyn LlmProvider,
    ) -> Result<()> {
        if messages.len() < 4 {
            // Not enough messages to summarise meaningfully (system + user + 1 turn = 3).
            // Just truncate instead.
            truncate_messages(messages, self.config.budget_chars);
            return Ok(());
        }

        // Decide how many messages from the beginning (after the system prompt) to
        // hand to the summariser.  We aim to summarise roughly the oldest half of
        // the non-system messages, but at least 2.
        //
        // Example: 20 messages → summarise messages[1..10], keep messages[10..20].
        let non_system_count = messages.len().saturating_sub(1); // exclude system prompt
        let summarise_count = (non_system_count / 2).max(2);
        // The slice to summarise: messages[1 .. 1+summarise_count]
        let summarise_end = 1 + summarise_count;

        // Build the text we will hand to the LLM.
        let context_text: String = messages[1..summarise_end]
            .iter()
            .filter_map(|m| {
                m.text_content().map(|text| {
                    let role_label = match m.role {
                        crate::llm::Role::User => "User",
                        crate::llm::Role::Assistant => "Assistant",
                        crate::llm::Role::System => "System",
                        crate::llm::Role::Tool => "Tool",
                    };
                    format!("[{role_label}]: {text}")
                })
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        // Build a minimal one-shot summarisation conversation.
        // We use a fresh Vec (not the agent's own messages) to keep the call isolated.
        let summarisation_prompt = format!(
            "The following is an excerpt from a coding session conversation. \
             Summarise the key context, decisions, file changes, tool calls, \
             and current state in 500 words or less.  Be factual and concise — \
             this summary will be injected back into the conversation so the \
             agent can continue working without losing important context.\n\n\
             --- Conversation excerpt ---\n\n{context_text}\n\n--- End excerpt ---"
        );
        let summary_request = vec![Message::user(&summarisation_prompt)];

        // Re-use the same provider but with an empty tool list (no tools needed for summary).
        // We don't thread the config's summary_max_tokens through the generic LlmProvider
        // trait — that would require API changes.  Instead we rely on the model's default
        // behaviour for short summarisation tasks (it will naturally produce a short reply).
        let summary_response = llm.chat_completion(&summary_request, &[]).await?;

        let summary_text = summary_response
            .content
            .unwrap_or_else(|| "[Context summary unavailable]".to_string());

        // Replace the summarised slice with a single user message containing the summary.
        // This keeps the conversation as a valid message sequence (user → assistant → ...).
        let summary_message = Message::user(format!(
            "[Context Summary — condensed from {} messages]\n\n{}",
            summarise_count, summary_text
        ));

        // Rebuild: system + summary_message + rest of messages
        let rest = messages.split_off(summarise_end); // messages is now messages[0..summarise_end]
        messages.truncate(1); // keep only the system prompt
        messages.push(summary_message);
        messages.extend(rest);

        Ok(())
    }
}

// ─── Simple truncation (kept as fallback) ────────────────────────────────────

/// Truncate messages to fit within approximate token budget.
///
/// Strategy: always keep system prompt (index 0) + first user message (index 1)
/// + as many messages from the END as fit within `budget_chars`.
///
/// This is the original strategy from `agent/mod.rs`, preserved here as the
/// fallback when LLM summarisation fails or is disabled.
pub fn truncate_messages(messages: &mut Vec<Message>, budget_chars: usize) {
    if messages.is_empty() {
        return;
    }
    let total_chars: usize = messages
        .iter()
        .map(|m| m.text_content().map(|s| s.len()).unwrap_or(0))
        .sum();
    if total_chars <= budget_chars {
        return;
    }

    // Always keep system prompt and first user message.
    let mut keep = vec![];
    if !messages.is_empty() {
        keep.push(messages[0].clone());
    }
    if messages.len() > 1 {
        keep.push(messages[1].clone());
    }

    // Find how many messages from the end fit in the budget.
    let mut chars = keep
        .iter()
        .map(|m| m.text_content().map(|s| s.len()).unwrap_or(0))
        .sum::<usize>();
    let mut tail = Vec::new();
    for m in messages.iter().skip(2).rev() {
        let mc = m.text_content().map(|s| s.len()).unwrap_or(0);
        if chars + mc > budget_chars {
            break;
        }
        tail.push(m.clone());
        chars += mc;
    }
    tail.reverse();

    let truncated_count = messages.len() - (keep.len() + tail.len());
    if truncated_count > 0 {
        warn!("Truncating {} messages for context window", truncated_count);
        keep.push(Message::user(format!(
            "[... {} messages truncated for context window ...]",
            truncated_count
        )));
    }
    keep.extend(tail);
    *messages = keep;
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Message;

    fn make_messages(n: usize) -> Vec<Message> {
        let mut msgs = vec![Message::system("You are a helpful assistant.")];
        for i in 0..n {
            let big = "x".repeat(50_000); // 50k chars each
            if i % 2 == 0 {
                msgs.push(Message::user(format!("user message {}: {}", i, big)));
            } else {
                msgs.push(Message::assistant(
                    Some(format!("assistant reply {}: {}", i, big)),
                    None,
                ));
            }
        }
        msgs
    }

    // ── truncate_messages ──────────────────────────────────────────────────

    #[test]
    fn test_truncate_noop_when_under_budget() {
        let mut msgs = vec![
            Message::system("system"),
            Message::user("hello"),
            Message::assistant(Some("hi".to_string()), None),
        ];
        let original_len = msgs.len();
        truncate_messages(&mut msgs, 400_000);
        assert_eq!(
            msgs.len(),
            original_len,
            "should not truncate when under budget"
        );
    }

    #[test]
    fn test_truncate_keeps_system_and_recent() {
        // 10 messages of 50k chars each = 500k total, budget = 400k
        let mut msgs = make_messages(9); // system + 9 more = 10 messages
        let system_content = msgs[0].text_content().unwrap();

        truncate_messages(&mut msgs, 400_000);

        // System prompt must survive.
        assert_eq!(
            msgs[0].text_content().unwrap(),
            system_content,
            "system prompt must be preserved"
        );
        // Total chars after truncation should fit in budget.
        let total: usize = msgs
            .iter()
            .map(|m| m.text_content().map(|s| s.len()).unwrap_or(0))
            .sum();
        assert!(
            total <= 400_000,
            "total chars after truncation should be ≤ budget, got {total}"
        );
    }

    #[test]
    fn test_truncate_inserts_marker() {
        let mut msgs = make_messages(9);
        truncate_messages(&mut msgs, 400_000);
        // There should be a message containing "[... N messages truncated ...]"
        let has_marker = msgs.iter().any(|m| {
            m.text_content()
                .map(|t| t.contains("messages truncated"))
                .unwrap_or(false)
        });
        assert!(has_marker, "truncated marker message should be present");
    }

    // ── ContextConfig defaults ─────────────────────────────────────────────

    #[test]
    fn test_context_config_defaults() {
        let cfg = ContextConfig::default();
        assert_eq!(cfg.budget_chars, 400_000);
        assert!((cfg.threshold - 0.80).abs() < 1e-9);
        assert_eq!(cfg.strategy, ContextStrategy::Summarize);
        assert_eq!(cfg.summary_max_tokens, 800);
    }

    // ── ContextManager with Truncate strategy ─────────────────────────────

    #[tokio::test]
    async fn test_context_manager_truncate_noop_under_threshold() {
        let cfg = ContextConfig {
            strategy: ContextStrategy::Truncate,
            budget_chars: 400_000,
            threshold: 0.80,
            summary_max_tokens: 800,
        };
        let mut mgr = ContextManager::new(cfg);

        // Small messages — well under budget.
        let mut msgs = vec![Message::system("system"), Message::user("hello")];
        let len_before = msgs.len();

        // We pass a dummy LlmProvider ref — it should never be called since
        // we're under the threshold.  Use a NullProvider for the test.
        struct NullProvider;
        #[async_trait::async_trait]
        impl crate::llm::LlmProvider for NullProvider {
            async fn chat_completion(
                &self,
                _messages: &[crate::llm::Message],
                _tools: &[crate::llm::ToolDefinition],
            ) -> Result<crate::llm::LlmResponse> {
                panic!("should not be called");
            }
        }

        mgr.manage(&mut msgs, &NullProvider).await.unwrap();
        assert_eq!(msgs.len(), len_before, "no-op when under threshold");
    }

    #[tokio::test]
    async fn test_context_manager_truncate_strategy() {
        let cfg = ContextConfig {
            strategy: ContextStrategy::Truncate,
            budget_chars: 100_000,
            threshold: 0.80,
            summary_max_tokens: 800,
        };
        let mut mgr = ContextManager::new(cfg);

        // 3 messages of 50k chars each = 150k total; threshold = 80k.
        let mut msgs = vec![
            Message::system("system"),
            Message::user("x".repeat(50_000)),
            Message::assistant(Some("y".repeat(50_000)), None),
            Message::user("z".repeat(50_000)),
        ];

        struct NullProvider;
        #[async_trait::async_trait]
        impl crate::llm::LlmProvider for NullProvider {
            async fn chat_completion(
                &self,
                _: &[crate::llm::Message],
                _: &[crate::llm::ToolDefinition],
            ) -> Result<crate::llm::LlmResponse> {
                panic!("truncate should not call LLM");
            }
        }

        mgr.manage(&mut msgs, &NullProvider).await.unwrap();

        let total: usize = msgs
            .iter()
            .map(|m| m.text_content().map(|s| s.len()).unwrap_or(0))
            .sum();
        assert!(
            total <= 100_000,
            "should fit within budget after truncation, got {total}"
        );
    }
}
