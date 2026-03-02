// src/ui.rs
// Styling helpers for xcodeai CLI output.
// Extracted from main.rs for clarity and reuse.
//
// These functions handle colored output, banners, separators, and status messages.
// They are used in both the CLI entrypoint and the REPL loop.
//
// For Rust learners: This module demonstrates how to use the `console` crate for styled terminal output.

use crate::tracking::SessionTracker;
use console::{style, Term};

/// Print the xcodeai banner at startup, showing version, model, project, and auth status.
pub fn print_banner(version: &str, model: &str, project: &str, auth_status: &str) {
    let term = Term::stdout();
    let width = term.size().1 as usize;
    let width = width.clamp(60, 100);
    let line = style("─".repeat(width)).dim().to_string();

    println!();
    println!(
        " {} {}  {}  {}",
        style("✦").cyan().bold(),
        style("xcodeai").cyan().bold(),
        style(format!("v{}", version)).dim(),
        style("autonomous AI coding agent").dim(),
    );
    println!(
        "   {} {}    {} {}",
        style("model:").dim(),
        style(model).green(),
        style("project:").dim(),
        style(project).yellow(),
    );
    println!("   {} {}", style("auth:").dim(), auth_status,);
    println!("{}", line);
    println!(
        "   {}",
        style("Type a task and press Enter.  /plan to discuss first.  /help for commands.  Ctrl-D to quit.").dim()
    );
    println!("{}", line);
    println!();
}

/// Print a separator line, optionally with a label.
pub fn print_separator(label: &str) {
    let label_str = if label.is_empty() {
        style("─".repeat(44)).dim().to_string()
    } else {
        format!(
            "{} {} {}",
            style("─".repeat(2)).dim(),
            style(label).dim(),
            style("─".repeat(40 - label.len().min(38))).dim()
        )
    };
    println!("{}", label_str);
}

/// Print a success message with a green checkmark.
pub fn ok(msg: &str) {
    println!(" {} {}", style("✓").green().bold(), msg);
}

/// Print a warning message with a yellow exclamation mark.
pub fn warn(msg: &str) {
    println!(" {} {}", style("!").yellow().bold(), style(msg).yellow());
}

/// Print an error message with a red cross.
pub fn err(msg: &str) {
    eprintln!(" {} {}", style("✗").red().bold(), style(msg).red());
}

/// Print an informational message in dim text.
pub fn info(msg: &str) {
    println!("   {}", style(msg).dim());
}

/// Print a compact one-line status bar above the REPL prompt.
///
/// Shown before every prompt so the user always has a quick overview of:
///   - cumulative token usage (prompt ↑ / completion ↓) and estimated cost
///   - which MCP servers are active
///   - whether an LSP server is running
///
/// The bar is printed in dim styling so it doesn't compete with the prompt.
/// Nothing is printed when there is no data yet (first prompt, no tokens used).
///
/// # Arguments
///
/// * `tracker`    — session-level token/cost accumulator
/// * `mcp_names`  — display names of connected MCP servers (may be empty)
/// * `lsp_active` — true if an LSP client has been started
/// * `lsp_name`   — command name of the LSP server (e.g. "rust-analyzer"); may be empty
pub fn print_status_bar(
    tracker: &SessionTracker,
    mcp_names: &[String],
    lsp_active: bool,
    lsp_name: &str,
) {
    // ── Collect segments —— only include segments that have data ──

    // Segment 1: token counts and cost.
    // We skip this entire segment when no turns have been recorded yet
    // (i.e. the very first prompt before any agent run) so the status bar
    // doesn't clutter the initial blank state.
    let token_segment: Option<String> = {
        let total = tracker.total_tokens();
        if total == 0 {
            // No LLM calls yet — nothing to show.
            None
        } else {
            // Format large numbers with commas for readability:
            //   12345  →  "12,345"
            let prompt_fmt = format_number_ui(tracker.total_prompt_tokens());
            let completion_fmt = format_number_ui(tracker.total_completion_tokens());

            // Cost estimate: only shown when the model is in the price table.
            let cost_str = match tracker.estimated_cost_usd() {
                Some(cost) => format!("  ~${:.3}", cost),
                None => String::new(),
            };

            // ↑ = prompt tokens (sent to the model)  ↓ = completion tokens (generated)
            Some(format!("tokens {}↑ {}↓{}", prompt_fmt, completion_fmt, cost_str))
        }
    };

    // Segment 2: MCP servers.
    // Only shown when at least one MCP server is connected.
    let mcp_segment: Option<String> = if mcp_names.is_empty() {
        None
    } else {
        // List server names separated by '·'.
        // e.g. "MCP: filesystem · github"
        let names = mcp_names.join(" · ");
        Some(format!("MCP: {}", names))
    };

    // Segment 3: LSP status.
    // ● = active/connected  ○ = configured but not yet connected
    let lsp_segment: Option<String> = if lsp_active {
        // LSP client is running.  Show server name if available.
        let label = if lsp_name.is_empty() {
            "LSP: ● active".to_string()
        } else {
            format!("LSP: ● {}", lsp_name)
        };
        Some(label)
    } else if !lsp_name.is_empty() {
        // LSP is configured but no client has been started yet.
        Some(format!("LSP: ○ {}", lsp_name))
    } else {
        None
    };

    // ── Build the bar ──────────────────────────────────────────────────────
    //
    // Collect all non-None segments and join them with " │ " dividers.
    // If there are no segments at all (no tokens, no MCP, no LSP), print
    // nothing — the very first prompt stays clean.
    let segments: Vec<String> = [token_segment, mcp_segment, lsp_segment]
        .into_iter()
        .flatten()       // discard None values
        .collect();

    if segments.is_empty() {
        // Nothing to show yet.
        return;
    }

    // Join segments with a dim vertical bar separator.
    let bar_content = segments.join(&format!(" {} ", style("│").dim()));

    // Print the full bar, dimmed, with a leading space for alignment.
    println!("   {}", style(bar_content).dim());
}

// ─── Local helper ─────────────────────────────────────────────────────────────

/// Format a `u32` with comma-separated thousands groups.
///
/// Used by `print_status_bar` to make large token counts readable.
///
/// Examples:
///   0        → "0"
///   999      → "999"
///   1_000    → "1,000"
///   123_456  → "123,456"
fn format_number_ui(n: u32) -> String {
    // Convert to string then insert commas every 3 digits from the right.
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    // The digits were inserted in reverse order — flip back.
    result.chars().rev().collect()
}
