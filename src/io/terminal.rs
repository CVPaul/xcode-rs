// src/io/terminal.rs
//
// TerminalIO — concrete AgentIO implementation for interactive terminals.
//
// This is the "current behaviour, behind the AgentIO trait".  All the
// eprintln! calls that previously lived directly in CoderAgent::run() have
// been moved here.  The styling is unchanged — same `console` crate, same
// colours, same format.
//
// Markdown rendering:
//   When running in a real TTY and `no_markdown` is false, the agent's final
//   response text (passed through `show_status`) is rendered with `termimad`
//   so that **bold**, *italic*, `code`, bullet lists, headings, etc. are
//   displayed with ANSI styling instead of raw markdown syntax.
//
//   Pass `--no-markdown` on the CLI (or set `no_markdown: true` when
//   constructing TerminalIO directly) to disable this and show plain text.
//
// Reading confirmation answers still uses tokio::task::spawn_blocking so we
// don't block the async executor while waiting for the user to type.

use crate::io::AgentIO;
use anyhow::Result;
use async_trait::async_trait;

/// Terminal-based I/O: writes status/tool output to stderr, reads from stdin.
///
/// # Fields
///
/// * `no_markdown` – when `true`, the agent's textual output is written
///   verbatim (no ANSI markdown rendering).  When `false` (the default) AND
///   stderr is a real TTY, `termimad` is used to render markdown syntax.
///
/// This is the implementation used in both the `xcodeai run` subcommand and
/// the interactive REPL.
#[derive(Default)]
pub struct TerminalIO {
    /// Disable markdown rendering even when stderr is a TTY.
    /// Controlled by the `--no-markdown` CLI flag.
    pub no_markdown: bool,
}

impl TerminalIO {
    /// Create a new TerminalIO with the given markdown setting.
    ///
    /// # Arguments
    /// * `no_markdown` – pass `true` to disable markdown rendering.
    #[allow(dead_code)]
    pub fn new(no_markdown: bool) -> Self {
        Self { no_markdown }
    }
}

// ── Markdown rendering helper ────────────────────────────────────────────────

/// Render `text` using `termimad` if we are on a real TTY and `no_markdown`
/// is false; otherwise return the text unchanged.
///
/// `termimad` understands CommonMark-style markdown:
///   - `**bold**` / `*italic*`
///   - `` `inline code` `` and fenced code blocks
///   - `# headings` at various levels
///   - `- bullet` lists and `1. numbered` lists
///   - `> blockquotes`
///   - `---` horizontal rules
///
/// The rendered output contains ANSI escape codes that are interpreted by
/// the terminal emulator to apply colour and bold/italic styling.
///
/// We check `console::Term::stderr().is_term()` so that piped output (e.g.
/// `xcodeai run "task" 2>log.txt`) never contains stray ANSI codes.
fn render_markdown(text: &str, no_markdown: bool) -> String {
    // Condition 1: caller explicitly requested plain text.
    if no_markdown {
        return text.to_owned();
    }
    // Condition 2: stderr is not a real TTY — ANSI codes would be noise.
    if !console::Term::stderr().is_term() {
        return text.to_owned();
    }
    // Use termimad's default skin.  MadSkin::default() picks a skin that
    // works on both light and dark terminal backgrounds by using bold/italic
    // ANSI attributes rather than colour-specific themes.
    let skin = termimad::MadSkin::default();
    // `skin.term_text(text)` returns a `FmtText` that implements `Display`.
    // `format!` drives the Display impl, which writes the ANSI-decorated
    // lines into a String.
    format!("{}", skin.term_text(text))
}

// ── AgentIO implementation ───────────────────────────────────────────────────

#[async_trait]
impl AgentIO for TerminalIO {
    // ── Status banner / final response ────────────────────────────────────────

    /// Print a status line or the agent's final response.
    ///
    /// When markdown rendering is enabled (TTY + `!no_markdown`), the text is
    /// run through `termimad` so that markdown syntax is displayed with ANSI
    /// styling.  Otherwise the text is printed as-is.
    ///
    /// Example output (plain):
    ///   "  ▶ auto-continuing…"
    ///   "  ◆ checkpoint (25 iterations) — verifying task progress…"
    ///
    /// Example output (rendered):
    ///   The string "**bold** and `code`" appears with bold ANSI styling.
    async fn show_status(&self, msg: &str) -> Result<()> {
        let rendered = render_markdown(msg, self.no_markdown);
        eprintln!("{}", rendered);
        Ok(())
    }

    // ── Tool call progress ────────────────────────────────────────────────────

    /// Print a one-line "→ tool_name ( args_preview" line.
    ///
    /// Example output:
    ///   "  → bash (  command: cargo test"
    async fn show_tool_call(&self, tool_name: &str, args_preview: &str) -> Result<()> {
        eprintln!(
            "  {} {} {}  {}",
            console::style("→").cyan().dim(),
            console::style(tool_name).cyan(),
            console::style("(").dim(),
            console::style(args_preview).dim(),
        );
        Ok(())
    }

    // ── Tool result preview ───────────────────────────────────────────────────

    /// Print the first line of a tool result.
    ///
    /// Success:  "  ← first line of output"   (dim)
    /// Error:    "  ← error: first line"       (red)
    async fn show_tool_result(&self, preview: &str, is_error: bool) -> Result<()> {
        if is_error {
            eprintln!(
                "  {} {}",
                console::style("← error:").red().dim(),
                console::style(preview).red().dim(),
            );
        } else {
            eprintln!(
                "  {} {}",
                console::style("←").dim(),
                console::style(preview).dim(),
            );
        }
        Ok(())
    }

    // ── Error / warning ───────────────────────────────────────────────────────

    /// Print a warning / error line, e.g. "  ! Reached auto-continue limit".
    async fn write_error(&self, msg: &str) -> Result<()> {
        eprintln!("{}", msg);
        Ok(())
    }

    // ── Destructive confirmation ───────────────────────────────────────────────

    /// Prompt the user before a destructive tool call.
    ///
    /// Prints a yellow warning to stderr, reads one line from stdin
    /// via `tokio::task::spawn_blocking` (so the async executor is not blocked),
    /// and returns `true` only if the user typed 'y' or 'Y'.
    async fn confirm_destructive(&self, tool_name: &str, args_preview: &str) -> Result<bool> {
        use std::io::Write;

        // Print the prompt to stderr — same stream as all tool output.
        eprint!(
            "  {} {} {}  {}  {} ",
            console::style("⚠").yellow().bold(),
            console::style(tool_name).yellow(),
            console::style("(").dim(),
            console::style(args_preview).yellow(),
            console::style("[y/N]:").dim(),
        );
        let _ = std::io::stderr().flush();

        // Read one line from stdin in a blocking thread.
        // `spawn_blocking` moves the work off the async executor so we don't
        // starve other tasks while the user is thinking.
        let answer = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).unwrap_or(0);
            line.trim().to_lowercase()
        })
        .await
        .unwrap_or_default();

        Ok(answer == "y")
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// When `no_markdown` is true, text is returned unchanged regardless of
    /// whether we're in a TTY or not.
    #[test]
    fn test_render_markdown_no_markdown_flag() {
        let input = "**bold** and *italic* and `code`";
        // no_markdown=true → identity
        let output = render_markdown(input, true);
        assert_eq!(
            output, input,
            "should return text unchanged when no_markdown=true"
        );
    }

    /// The `render_markdown` function must never panic on empty input.
    #[test]
    fn test_render_markdown_empty_string() {
        // Should not panic — just return empty or whitespace
        let output = render_markdown("", true);
        assert_eq!(output, "");
    }

    /// Verify TerminalIO::default() has no_markdown = false.
    #[test]
    fn test_terminal_io_default_no_markdown_false() {
        let io = TerminalIO::default();
        assert!(!io.no_markdown, "default should have markdown enabled");
    }

    /// Verify TerminalIO::new(true) stores the flag correctly.
    #[test]
    fn test_terminal_io_new_no_markdown_true() {
        let io = TerminalIO::new(true);
        assert!(io.no_markdown);
    }

    /// When not in a TTY (which is always the case in test harness),
    /// render_markdown with no_markdown=false should return text unchanged
    /// (because `console::Term::stderr().is_term()` is false in tests).
    #[test]
    fn test_render_markdown_non_tty_returns_plain() {
        // In the cargo test harness stderr is not a TTY, so even with
        // no_markdown=false the function should return the original text.
        let input = "# Heading\n**bold** text";
        let output = render_markdown(input, false);
        // In CI / test harness (non-TTY), we get the plain text back.
        // This test verifies the non-TTY code path doesn't corrupt the text.
        assert_eq!(output, input);
    }
}
