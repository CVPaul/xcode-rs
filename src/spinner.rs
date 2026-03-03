// src/spinner.rs
//
// A lightweight terminal spinner for showing activity during long-running
// operations (LLM calls, tool execution).
//
// The spinner writes to stderr so it never interferes with stdout (where LLM
// streaming output goes).  It is TTY-safe: if stderr is not a terminal, the
// spinner silently does nothing.
//
// Usage:
//   let spinner = Spinner::start("thinking…");
//   // … long operation …
//   spinner.stop();
//
// The spinner renders braille-style frames (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏) at ~80ms intervals,
// producing a smooth rotation effect.  On stop(), the spinner line is erased
// so it doesn't clutter the terminal.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Braille spinner frames — smooth rotation pattern.
const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Frame interval in milliseconds.
const FRAME_MS: u64 = 80;

/// A terminal spinner that runs in a background tokio task.
///
/// Create with `Spinner::start(msg)`.  The spinner writes frames to stderr
/// using carriage-return overwriting.  Call `stop()` to cancel and erase.
///
/// If stderr is not a TTY (piped, redirected), the spinner is a no-op —
/// `start()` returns a Spinner whose `stop()` does nothing.
pub struct Spinner {
    /// Signal to the background task to stop.
    running: Arc<AtomicBool>,
    /// Handle to the background task (joined on stop).
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Spinner {
    /// Start a spinner with the given message.
    ///
    /// The message is displayed after the spinner frame:
    ///   ⠋ thinking…
    ///
    /// Returns immediately.  The spinner runs in a background tokio task.
    pub fn start(msg: impl Into<String>) -> Self {
        let msg = msg.into();

        // Don't spin if stderr is not a TTY.
        if !console::Term::stderr().is_term() {
            return Spinner {
                running: Arc::new(AtomicBool::new(false)),
                handle: None,
            };
        }

        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        let handle = tokio::spawn(async move {
            use std::io::Write;
            let mut frame_idx = 0usize;
            let styled_msg = format!("{}", console::style(&msg).dim());

            while running_clone.load(Ordering::Relaxed) {
                let frame = FRAMES[frame_idx % FRAMES.len()];
                // \r moves cursor to start of line.
                // We write the frame + message, then pad with spaces to clear
                // any previous longer content.
                let line = format!(
                    "\r  {} {}",
                    console::style(frame).cyan(),
                    styled_msg,
                );
                eprint!("{}", line);
                // Pad to clear residual chars from previous longer lines
                eprint!("   ");
                std::io::stderr().flush().ok();

                frame_idx += 1;
                tokio::time::sleep(std::time::Duration::from_millis(FRAME_MS)).await;
            }

            // Erase the spinner line: \r + spaces + \r
            eprint!("\r{}\r", " ".repeat(60));
            std::io::stderr().flush().ok();
        });

        Spinner {
            running,
            handle: Some(handle),
        }
    }

    /// Stop the spinner and erase its line from stderr.
    ///
    /// This is idempotent — calling stop() multiple times is safe.
    pub fn stop(mut self) {
        self.cancel();
    }

    /// Internal: signal stop and wait for the background task.
    fn cancel(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            handle.abort();
            // The abort may kill the task before it can erase the spinner line,
            // so we erase directly here to guarantee cleanup.
            use std::io::Write;
            if console::Term::stderr().is_term() {
                eprint!("\r{}\r", " ".repeat(60));
                std::io::stderr().flush().ok();
            }
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        // Ensure we always signal stop, even if the caller forgot.
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spinner::start should not panic even in a non-TTY test environment.
    #[tokio::test]
    async fn test_spinner_non_tty_noop() {
        // In the test harness, stderr is not a TTY, so the spinner is a no-op.
        let spinner = Spinner::start("testing…");
        // Give it a moment to confirm no panic
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        spinner.stop();
    }

    /// Spinner::drop should not panic.
    #[tokio::test]
    async fn test_spinner_drop_safety() {
        let spinner = Spinner::start("drop test");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(spinner);
        // No panic = pass
    }
}
