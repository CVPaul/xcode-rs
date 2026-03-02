// src/ui.rs
// Styling helpers for xcodeai CLI output.
// Extracted from main.rs for clarity and reuse.
//
// These functions handle colored output, banners, separators, and status messages.
// They are used in both the CLI entrypoint and the REPL loop.
//
// For Rust learners: This module demonstrates how to use the `console` crate for styled terminal output.

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
