// src/io/mod.rs
//
// AgentIO — the I/O abstraction layer for xcodeai.
//
// ── Why this exists ───────────────────────────────────────────────────────────
// The original CoderAgent used `eprintln!` directly for status output and
// `std::io::stdin` for confirmation prompts.  That works fine for a terminal
// REPL, but it makes the agent untestable and un-portable:
//
//   • Tests cannot capture or inject I/O without mocking the whole OS.
//   • A future HTTP/WebSocket interface (Task 32, HttpIO) has no terminal.
//   • Termux has no TUI; we still want pretty-printed tool progress.
//
// The `AgentIO` trait below is the single abstraction that decouples the agent
// loop from the concrete I/O channel.  Two implementations ship here:
//
//   • `TerminalIO`  — the default; writes to stderr/stdout, reads from stdin.
//                     This is "the current behaviour, behind a trait".
//   • `NullIO`      — silently drops all output, always returns "n" for
//                     confirmation.  Used in unit tests where we don't want
//                     any terminal I/O.
//
// A third implementation, `HttpIO`, will be added in Task 32 when we wire up
// the 企业微信 / HTTP API mode.
// ─────────────────────────────────────────────────────────────────────────────

pub mod http;
pub mod terminal;

use anyhow::Result;
use async_trait::async_trait;

/// Abstraction over all input/output the agent needs.
///
/// Each method is `async` so implementations can do async work (e.g. send
/// over a WebSocket) without blocking the executor.
///
/// The `Send + Sync` bounds let us wrap implementations in `Arc<dyn AgentIO>`
/// and share them across the async task boundary.
#[async_trait]
pub trait AgentIO: Send + Sync {
    // ── Output ────────────────────────────────────────────────────────────────

    /// Display a progress / info message.  Used for status banners like
    /// "auto-continuing…" and "checkpoint".
    async fn show_status(&self, msg: &str) -> Result<()>;

    /// Show which tool is about to be called.
    ///
    /// `tool_name` — the function name (e.g. "bash", "file_write")
    /// `args_preview` — a compact single-line summary of the arguments
    async fn show_tool_call(&self, tool_name: &str, args_preview: &str) -> Result<()>;

    /// Show the first line of a tool's output after execution.
    ///
    /// `is_error` — true when the tool returned an error result, causing the
    ///   output to be rendered in red.
    async fn show_tool_result(&self, preview: &str, is_error: bool) -> Result<()>;

    /// Display an error or warning that is NOT part of a tool result.
    /// For example, "Reached auto-continue limit" or a hard-stop warning.
    async fn write_error(&self, msg: &str) -> Result<()>;

    // ── Input ─────────────────────────────────────────────────────────────────

    /// Ask the user to confirm a destructive tool call.
    ///
    /// `tool_name`    — the tool being called
    /// `args_preview` — compact single-line args summary
    ///
    /// Returns `true` if the user approved (typed 'y'/'Y'), `false` otherwise.
    /// Non-interactive implementations (NullIO, HttpIO) should return `false`
    /// so they do NOT silently execute destructive operations.
    async fn confirm_destructive(&self, tool_name: &str, args_preview: &str) -> Result<bool>;
}

// ─── NullIO ──────────────────────────────────────────────────────────────────
//
// Used in unit tests.  All output methods are no-ops; confirm_destructive
// always returns false (deny — the safest default for automated tests).

/// Silent I/O implementation for unit tests.
///
/// • All output methods are no-ops (nothing is printed or logged).
/// • `confirm_destructive` always returns `false` — destructive calls are
///   treated as denied.  Tests that want to test the "approved" path should
///   use a custom `AgentIO` mock instead.
#[allow(dead_code)]
pub struct NullIO;

#[async_trait]
impl AgentIO for NullIO {
    async fn show_status(&self, _msg: &str) -> Result<()> {
        Ok(())
    }

    async fn show_tool_call(&self, _tool_name: &str, _args_preview: &str) -> Result<()> {
        Ok(())
    }

    async fn show_tool_result(&self, _preview: &str, _is_error: bool) -> Result<()> {
        Ok(())
    }

    async fn write_error(&self, _msg: &str) -> Result<()> {
        Ok(())
    }

    async fn confirm_destructive(&self, _tool_name: &str, _args_preview: &str) -> Result<bool> {
        // Always deny — tests must not accidentally execute destructive tools.
        Ok(false)
    }
}

// ─── AutoApproveIO ──────────────────────────────────────────────────────────
//
// Used when `--yes` / `-y` is passed on the CLI.
// All output goes to stderr (same as TerminalIO); confirm_destructive always
// returns true so no human approval is needed.

/// I/O implementation for `--yes` mode.
///
/// Inherits full terminal output (tool calls, results, status lines) from
/// `TerminalIO`, but auto-approves every destructive action without prompting.
/// This is the behaviour of the old `confirm_destructive = false` flag.
pub struct AutoApproveIO;

#[async_trait]
impl AgentIO for AutoApproveIO {
    async fn show_status(&self, msg: &str) -> Result<()> {
        terminal::TerminalIO { no_markdown: false }
            .show_status(msg)
            .await
    }
    async fn show_tool_call(&self, tool_name: &str, args_preview: &str) -> Result<()> {
        terminal::TerminalIO { no_markdown: false }
            .show_tool_call(tool_name, args_preview)
            .await
    }
    async fn show_tool_result(&self, preview: &str, is_error: bool) -> Result<()> {
        terminal::TerminalIO { no_markdown: false }
            .show_tool_result(preview, is_error)
            .await
    }
    async fn write_error(&self, msg: &str) -> Result<()> {
        terminal::TerminalIO { no_markdown: false }
            .write_error(msg)
            .await
    }
    /// Always returns `true` — no prompt, auto-approve.
    async fn confirm_destructive(&self, _tool_name: &str, _args_preview: &str) -> Result<bool> {
        Ok(true)
    }
}
