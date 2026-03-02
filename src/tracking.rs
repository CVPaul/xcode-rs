// src/tracking.rs
//
// Token usage and cost tracking for xcodeai sessions.
//
// # Design
//
// Every time CoderAgent calls `llm.chat_completion()` it gets back an
// `LlmResponse` that may contain a `Usage` struct (populated when the
// provider is asked with `stream_options.include_usage = true`).
//
// We accumulate those per-turn usage stats in a `SessionTracker` and
// surface them in two places:
//
//   1. After each LLM turn, a compact inline line:
//      `← tokens: 1,234 in / 567 out`
//
//   2. In the completion banner:
//      `✓ done · 3 iterations · 7 tool calls · 15,678 tokens`
//
//   3. Via the `/tokens` REPL command which shows a full breakdown.
//
// # Cost estimation
//
// We keep a small hard-coded price table (per 1M tokens) and compute a
// rough USD estimate.  The table is intentionally minimal — update it in
// config or just skip it when the model isn't found.  We never block on
// missing cost data.
//
// # Persistence
//
// Token totals are stored in the SQLite sessions table (two new columns:
// `prompt_tokens` and `completion_tokens`).  The store module handles
// the schema migration.

use crate::llm::Usage;
use chrono::{DateTime, Utc};

// ─── Per-turn usage record ────────────────────────────────────────────────────

/// Token counts for a single LLM call (one iteration of the agent loop).
///
/// We record the raw in/out counts rather than just a total so callers can
/// show the breakdown separately (prompt tokens drive caching costs, while
/// completion tokens drive generation costs and are usually more expensive).
#[derive(Debug, Clone)]
#[allow(dead_code)] // Part of public API; will be used in future HTTP/display code
pub struct TurnUsage {
    /// Tokens in the input (system prompt + history + this message).
    pub prompt_tokens: u32,
    /// Tokens the model generated in its reply.
    pub completion_tokens: u32,
    /// Wall-clock timestamp when this turn completed.
    pub timestamp: DateTime<Utc>,
}

// ─── Session-level tracker ────────────────────────────────────────────────────

/// Accumulates token usage across all turns of a single agent session.
///
/// The tracker is owned by CoderAgent::run() for the duration of a task.
/// At the end of the task it is embedded in AgentResult so the REPL can
/// display the final totals and store them in SQLite.
///
/// # Example
///
/// ```ignore
/// let mut tracker = SessionTracker::new("gpt-4o".to_string());
/// tracker.record(Some(&Usage { prompt_tokens: 100, completion_tokens: 50, total_tokens: 150 }));
/// assert_eq!(tracker.total_prompt_tokens(), 100);
/// assert_eq!(tracker.total_completion_tokens(), 50);
/// ```
#[derive(Debug, Clone)]
pub struct SessionTracker {
    /// Name of the model in use (used for cost lookup).
    pub model: String,
    /// Ordered list of per-turn records (earliest first).
    pub turns: Vec<TurnUsage>,
}

impl SessionTracker {
    /// Create a new empty tracker for `model`.
    pub fn new(model: impl Into<String>) -> Self {
        SessionTracker {
            model: model.into(),
            turns: Vec::new(),
        }
    }

    /// Record the usage from one LLM call.
    ///
    /// If the API returned no usage data (`None`) the turn is silently
    /// skipped — we never panic or error on missing usage.
    pub fn record(&mut self, usage: Option<&Usage>) {
        if let Some(u) = usage {
            self.turns.push(TurnUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                timestamp: Utc::now(),
            });
        }
    }

    /// Sum of all prompt tokens across every turn.
    pub fn total_prompt_tokens(&self) -> u32 {
        self.turns.iter().map(|t| t.prompt_tokens).sum()
    }

    /// Sum of all completion tokens across every turn.
    pub fn total_completion_tokens(&self) -> u32 {
        self.turns.iter().map(|t| t.completion_tokens).sum()
    }

    /// Grand total tokens (prompt + completion).
    pub fn total_tokens(&self) -> u32 {
        self.total_prompt_tokens() + self.total_completion_tokens()
    }

    /// Number of turns recorded.
    #[allow(dead_code)] // Part of public API; will be used in future session reporting
    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }

    /// Estimate cost in USD using the built-in price table.
    ///
    /// Returns `None` when the model isn't found in the table.
    /// Prices are per 1,000,000 tokens (as published by the providers).
    pub fn estimated_cost_usd(&self) -> Option<f64> {
        let prices = model_prices(&self.model)?;
        let prompt_cost = (self.total_prompt_tokens() as f64 / 1_000_000.0) * prices.prompt_per_m;
        let completion_cost =
            (self.total_completion_tokens() as f64 / 1_000_000.0) * prices.completion_per_m;
        Some(prompt_cost + completion_cost)
    }

    /// Format a short one-line summary suitable for the completion banner.
    ///
    /// Example: `15,678 tokens (~$0.047)`
    /// If no turns were recorded (provider didn't return usage), returns `""`.
    pub fn summary_line(&self) -> String {
        let total = self.total_tokens();
        if total == 0 {
            return String::new();
        }
        let tokens_fmt = format_number(total);
        match self.estimated_cost_usd() {
            Some(cost) => format!("{} tokens (~${:.3})", tokens_fmt, cost),
            None => format!("{} tokens", tokens_fmt),
        }
    }

    /// Format a detailed multi-line report for the `/tokens` REPL command.
    ///
    /// Shows per-turn breakdown plus session totals and cost estimate.
    pub fn detailed_report(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();

        if self.turns.is_empty() {
            writeln!(
                out,
                "  No token data available (provider did not return usage)."
            )
            .unwrap();
            return out;
        }

        writeln!(out).unwrap();
        writeln!(
            out,
            "  Token usage for this session  (model: {})",
            self.model
        )
        .unwrap();
        writeln!(
            out,
            "  {:<8}  {:>10}  {:>12}  {:>10}",
            "Turn", "Prompt", "Completion", "Total"
        )
        .unwrap();
        writeln!(out, "  {}", "-".repeat(48)).unwrap();

        for (i, t) in self.turns.iter().enumerate() {
            let total = t.prompt_tokens + t.completion_tokens;
            writeln!(
                out,
                "  {:<8}  {:>10}  {:>12}  {:>10}",
                i + 1,
                format_number(t.prompt_tokens),
                format_number(t.completion_tokens),
                format_number(total),
            )
            .unwrap();
        }

        writeln!(out, "  {}", "-".repeat(48)).unwrap();
        writeln!(
            out,
            "  {:<8}  {:>10}  {:>12}  {:>10}",
            "Total",
            format_number(self.total_prompt_tokens()),
            format_number(self.total_completion_tokens()),
            format_number(self.total_tokens()),
        )
        .unwrap();

        if let Some(cost) = self.estimated_cost_usd() {
            writeln!(out, "  Estimated cost: ~${:.4}", cost).unwrap();
        } else {
            writeln!(
                out,
                "  Cost estimate not available for model '{}'.",
                self.model
            )
            .unwrap();
        }

        out
    }
}

// ─── Price table ──────────────────────────────────────────────────────────────

/// Per-million-token prices for a model.
struct ModelPrices {
    /// Cost per 1M input (prompt) tokens in USD.
    prompt_per_m: f64,
    /// Cost per 1M output (completion) tokens in USD.
    completion_per_m: f64,
}

/// Look up prices for a model by name prefix.
///
/// Returns `None` if the model isn't in the table — callers should handle
/// that gracefully (skip cost display rather than showing $0.00).
fn model_prices(model: &str) -> Option<ModelPrices> {
    // Match by prefix so "gpt-4o-2024-08-06" matches "gpt-4o".
    // Prices as of early 2026 — update when providers change rates.
    let m = model.to_lowercase();
    // --- OpenAI ---
    if m.starts_with("gpt-4o-mini") {
        return Some(ModelPrices {
            prompt_per_m: 0.15,
            completion_per_m: 0.60,
        });
    }
    if m.starts_with("gpt-4o") {
        return Some(ModelPrices {
            prompt_per_m: 2.50,
            completion_per_m: 10.00,
        });
    }
    if m.starts_with("gpt-4-turbo") || m.starts_with("gpt-4-0125") || m.starts_with("gpt-4-1106") {
        return Some(ModelPrices {
            prompt_per_m: 10.00,
            completion_per_m: 30.00,
        });
    }
    if m.starts_with("gpt-4") {
        return Some(ModelPrices {
            prompt_per_m: 30.00,
            completion_per_m: 60.00,
        });
    }
    if m.starts_with("gpt-3.5") {
        return Some(ModelPrices {
            prompt_per_m: 0.50,
            completion_per_m: 1.50,
        });
    }
    if m.starts_with("o1-mini") {
        return Some(ModelPrices {
            prompt_per_m: 3.00,
            completion_per_m: 12.00,
        });
    }
    if m.starts_with("o1") {
        return Some(ModelPrices {
            prompt_per_m: 15.00,
            completion_per_m: 60.00,
        });
    }
    if m.starts_with("o3-mini") {
        return Some(ModelPrices {
            prompt_per_m: 1.10,
            completion_per_m: 4.40,
        });
    }
    // --- DeepSeek ---
    if m.starts_with("deepseek-chat") || m.starts_with("deepseek-v3") {
        return Some(ModelPrices {
            prompt_per_m: 0.27,
            completion_per_m: 1.10,
        });
    }
    if m.starts_with("deepseek-reasoner") || m.starts_with("deepseek-r1") {
        return Some(ModelPrices {
            prompt_per_m: 0.55,
            completion_per_m: 2.19,
        });
    }
    // --- Qwen ---
    if m.starts_with("qwen-turbo") {
        return Some(ModelPrices {
            prompt_per_m: 0.06,
            completion_per_m: 0.18,
        });
    }
    if m.starts_with("qwen-plus") {
        return Some(ModelPrices {
            prompt_per_m: 0.40,
            completion_per_m: 1.20,
        });
    }
    if m.starts_with("qwen-max") {
        return Some(ModelPrices {
            prompt_per_m: 2.40,
            completion_per_m: 9.60,
        });
    }
    // --- GitHub Copilot (uses a proxy, pricing opaque — no estimate) ---
    if m.starts_with("copilot") {
        return None;
    }
    // Unknown model
    None
}

// ─── Formatting helpers ───────────────────────────────────────────────────────

/// Format an integer with thousands separators.
///
/// `format_number(1_234_567)` → `"1,234,567"`
pub fn format_number(n: u32) -> String {
    // Build digits right-to-left then reverse.
    let s = n.to_string();
    let chars: Vec<char> = s.chars().rev().collect();
    let grouped: Vec<String> = chars
        .chunks(3)
        .map(|c| c.iter().collect::<String>())
        .collect();
    grouped.join(",").chars().rev().collect()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::Usage;

    fn make_usage(prompt: u32, completion: u32) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
        }
    }

    #[test]
    fn test_empty_tracker() {
        let t = SessionTracker::new("gpt-4o");
        assert_eq!(t.total_tokens(), 0);
        assert_eq!(t.turn_count(), 0);
        assert!(t.summary_line().is_empty());
    }

    #[test]
    fn test_record_some_usage() {
        let mut t = SessionTracker::new("gpt-4o");
        t.record(Some(&make_usage(100, 50)));
        t.record(Some(&make_usage(200, 80)));
        assert_eq!(t.total_prompt_tokens(), 300);
        assert_eq!(t.total_completion_tokens(), 130);
        assert_eq!(t.total_tokens(), 430);
        assert_eq!(t.turn_count(), 2);
    }

    #[test]
    fn test_record_none_is_silently_skipped() {
        let mut t = SessionTracker::new("gpt-4o");
        t.record(None); // provider returned no usage
        t.record(Some(&make_usage(50, 20)));
        // Only one turn recorded; the None is a no-op.
        assert_eq!(t.turn_count(), 1);
        assert_eq!(t.total_tokens(), 70);
    }

    #[test]
    fn test_summary_line_with_known_model() {
        let mut t = SessionTracker::new("gpt-4o-mini");
        t.record(Some(&make_usage(1_000_000, 500_000)));
        let line = t.summary_line();
        // Should contain the token count and a cost estimate.
        assert!(line.contains("1,500,000 tokens"), "line was: {line}");
        assert!(line.contains("~$"), "expected cost estimate in: {line}");
    }

    #[test]
    fn test_summary_line_with_unknown_model() {
        let mut t = SessionTracker::new("my-custom-model");
        t.record(Some(&make_usage(5000, 2000)));
        let line = t.summary_line();
        assert!(line.contains("7,000 tokens"), "line was: {line}");
        // No cost estimate for unknown model.
        assert!(
            !line.contains("~$"),
            "should not have cost estimate: {line}"
        );
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1_000), "1,000");
        assert_eq!(format_number(1_234_567), "1,234,567");
        assert_eq!(format_number(1_000_000), "1,000,000");
    }

    #[test]
    fn test_detailed_report_empty() {
        let t = SessionTracker::new("gpt-4o");
        let report = t.detailed_report();
        assert!(report.contains("No token data"));
    }

    #[test]
    fn test_detailed_report_with_turns() {
        let mut t = SessionTracker::new("gpt-4o");
        t.record(Some(&make_usage(1000, 400)));
        t.record(Some(&make_usage(800, 200)));
        let report = t.detailed_report();
        assert!(report.contains("Turn"));
        assert!(report.contains("Total"));
        assert!(report.contains("gpt-4o"));
    }

    #[test]
    fn test_cost_estimation_gpt4o() {
        let mut t = SessionTracker::new("gpt-4o");
        // 1M prompt tokens at $2.50/M + 1M completion at $10.00/M = $12.50
        t.record(Some(&make_usage(1_000_000, 1_000_000)));
        let cost = t.estimated_cost_usd().unwrap();
        assert!(
            (cost - 12.50).abs() < 0.001,
            "expected ~$12.50, got ${cost}"
        );
    }

    #[test]
    fn test_cost_estimation_unknown_model() {
        let mut t = SessionTracker::new("some-local-llm");
        t.record(Some(&make_usage(1000, 500)));
        assert!(t.estimated_cost_usd().is_none());
    }
}
