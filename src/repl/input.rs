// src/repl/input.rs
//
// Crossterm-based line editor with real-time slash-command suggestions.
//
// This replaces rustyline's `rl.readline()` for the REPL main loop.
// The key difference: we process keypresses one at a time (raw mode),
// so we can update the suggestion list on *every* character — no Tab needed.
//
// ┌─────────────────────────────────────────────────────────┐
// │  xcodeai›  /he_                                         │
// │  /help      Show all commands                           │  ← suggestions
// │  /hooks     Manage hook configurations                  │    rendered live
// └─────────────────────────────────────────────────────────┘
//
// For Rust learners:
//   - `crossterm::terminal::enable_raw_mode()` puts the terminal into a mode
//     where keypresses are delivered immediately (no line buffering) and the
//     terminal doesn't echo characters by itself — we do it manually.
//   - `crossterm::event::read()` blocks until a key is pressed, then returns
//     a `KeyEvent` describing exactly what was pressed.
//   - We manage a `Vec<char>` buffer and a cursor position ourselves, just
//     like a text editor would — insert/delete characters, move left/right.
//   - After each keypress we erase the previous suggestion area and redraw it
//     using ANSI escape sequences via crossterm's `queue!` macro.

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    style::{Color, Print, SetForegroundColor, ResetColor},
    terminal::{self, ClearType},
    ExecutableCommand, QueueableCommand,
};
use std::io::{self, Write};

use super::commands::COMMANDS;

// ── Public result type ────────────────────────────────────────────────────────

/// What `readline_with_suggestions` returns to the caller.
pub enum ReadResult {
    /// User typed a line and pressed Enter.
    Line(String),
    /// Ctrl-C was pressed (maps to "Interrupted" in rustyline).
    Interrupted,
    /// Ctrl-D on an empty line (maps to "Eof" in rustyline).
    Eof,
}

// ── History ───────────────────────────────────────────────────────────────────

/// Simple in-process history list.
/// The REPL keeps one `InputHistory` alive for the session.
/// For persistence across sessions we still use the file-based approach
/// (load at start, save at end) but the in-memory list drives navigation.
pub struct InputHistory {
    entries: Vec<String>,
    /// When navigating, which index are we at?  `None` = not navigating.
    cursor: Option<usize>,
    /// Stash the live line while the user is browsing history.
    stash: String,
}

impl InputHistory {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            cursor: None,
            stash: String::new(),
        }
    }

    /// Load from a text file (one entry per line, oldest first).
    pub fn load_from_file(&mut self, path: &std::path::Path) {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines() {
                let s = line.trim().to_string();
                if !s.is_empty() {
                    self.entries.push(s);
                }
            }
        }
    }

    /// Save to a text file, keeping the last 1000 entries.
    pub fn save_to_file(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let start = self.entries.len().saturating_sub(1000);
        let content = self.entries[start..].join("\n");
        let _ = std::fs::write(path, content);
    }

    /// Push a new entry (skip duplicates of the most recent).
    pub fn push(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        // Don't add exact duplicate of the last entry.
        if self.entries.last().map(|s| s.as_str()) != Some(line) {
            self.entries.push(line.to_string());
        }
        self.cursor = None;
        self.stash = String::new();
    }

    /// Move up (older) in history.  Returns the entry to display.
    pub fn up(&mut self, current_line: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        match self.cursor {
            None => {
                // First press: stash current line, go to most recent entry.
                self.stash = current_line.to_string();
                self.cursor = Some(self.entries.len() - 1);
            }
            Some(0) => {
                // Already at oldest; stay there.
            }
            Some(i) => {
                self.cursor = Some(i - 1);
            }
        }
        self.cursor.map(|i| self.entries[i].clone())
    }

    /// Move down (newer) in history.  Returns the entry, or None if back at live line.
    pub fn down(&mut self) -> Option<String> {
        match self.cursor {
            None => None,
            Some(i) if i + 1 >= self.entries.len() => {
                // Back to live line.
                self.cursor = None;
                Some(self.stash.clone())
            }
            Some(i) => {
                self.cursor = Some(i + 1);
                Some(self.entries[i + 1].clone())
            }
        }
    }

    /// Reset navigation (called when Enter is pressed).
    pub fn reset_nav(&mut self) {
        self.cursor = None;
        self.stash = String::new();
    }
}

// ── Main readline function ────────────────────────────────────────────────────

/// Read one line from the terminal with live slash-command suggestions.
///
/// `prompt` is the coloured prompt string (already formatted with ANSI codes).
/// `history` is the mutable history list owned by the REPL.
///
/// The function:
///   1. Enables raw mode so we get keypresses immediately.
///   2. Prints the prompt.
///   3. Processes keys one by one, updating the buffer and suggestion list.
///   4. Returns when Enter / Ctrl-D / Ctrl-C is pressed.
///   5. Restores cooked mode before returning.
pub fn readline_with_suggestions(
    prompt: &str,
    history: &mut InputHistory,
) -> io::Result<ReadResult> {
    // ── Enter raw mode ────────────────────────────────────────────────────────
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();

    // Print the prompt (it contains ANSI colour codes from `console::style`).
    // We use `print!` + `flush` rather than crossterm's Print so that the
    // `console` crate's escape sequences go through unmolested.
    print!("{}", prompt);
    stdout.flush()?;

    // ── State ─────────────────────────────────────────────────────────────────
    let mut buf: Vec<char> = Vec::new(); // the characters the user has typed
    let mut cursor_pos: usize = 0; // insertion point (0 = before first char)
    let mut suggestions: Vec<&'static str> = Vec::new(); // current /command matches
    let mut selected: usize = 0; // which suggestion is highlighted

    let result = loop {
        // ── Wait for a keypress ───────────────────────────────────────────────
        let evt = event::read()?;

        // On Windows and some terminals, key-release events are also sent.
        // We only want key-press events.
        let key = match evt {
            Event::Key(k) if k.kind == KeyEventKind::Press => k,
            // Ignore resize, mouse, key-release, etc.
            _ => continue,
        };

        match (key.code, key.modifiers) {
            // ── Enter: submit ─────────────────────────────────────────────────
            (KeyCode::Enter, _) => {
                // If a suggestion is highlighted, complete it first.
                if !suggestions.is_empty() {
                    let chosen = suggestions[selected];
                    // Replace buffer with completed command + trailing space.
                    buf = format!("{} ", chosen).chars().collect();
                    cursor_pos = buf.len();
                    redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;

                    // If the command takes no arguments, submit immediately.
                    // Commands that take args (e.g. /model, /undo N) get a
                    // trailing space so the user can type the argument.
                    let no_args = !matches!(chosen, "/model" | "/undo");
                    if no_args {
                        suggestions.clear();
                        erase_suggestions(&mut stdout, 0)?;
                        // Move to a new line before returning.
                        stdout.execute(Print("\r\n"))?;
                        history.reset_nav();
                        let line: String = buf.iter().collect();
                        let line = line.trim().to_string();
                        break ReadResult::Line(line);
                    } else {
                        // Keep suggestions visible so user sees the description,
                        // but clear the selection — they can now type the arg.
                        suggestions.clear();
                        erase_suggestions(&mut stdout, 0)?;
                        continue;
                    }
                }

                // No active suggestion — normal Enter.
                erase_suggestions(&mut stdout, suggestions.len())?;
                stdout.execute(Print("\r\n"))?;
                history.reset_nav();
                let line: String = buf.iter().collect();
                let line = line.trim().to_string();
                break ReadResult::Line(line);
            }

            // ── Ctrl-D: EOF (only on empty buffer) ───────────────────────────
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                if buf.is_empty() {
                    erase_suggestions(&mut stdout, suggestions.len())?;
                    stdout.execute(Print("\r\n"))?;
                    break ReadResult::Eof;
                }
                // Non-empty: delete char under cursor (like normal Ctrl-D).
                if cursor_pos < buf.len() {
                    buf.remove(cursor_pos);
                    update_suggestions(&buf, &mut suggestions, &mut selected);
                    redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
                }
            }

            // ── Ctrl-C: interrupted ───────────────────────────────────────────
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                erase_suggestions(&mut stdout, suggestions.len())?;
                stdout.execute(Print("\r\n"))?;
                history.reset_nav();
                buf.clear();
                // cursor_pos already 0 from initialisation; nothing to update.
                break ReadResult::Interrupted;
            }

            // ── Ctrl-U: clear line ────────────────────────────────────────────
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                buf.clear();
                cursor_pos = 0;
                update_suggestions(&buf, &mut suggestions, &mut selected);
                redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
            }

            // ── Ctrl-W: delete word before cursor ─────────────────────────────
            (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                // Delete backwards to the previous word boundary.
                while cursor_pos > 0 && buf[cursor_pos - 1] == ' ' {
                    cursor_pos -= 1;
                    buf.remove(cursor_pos);
                }
                while cursor_pos > 0 && buf[cursor_pos - 1] != ' ' {
                    cursor_pos -= 1;
                    buf.remove(cursor_pos);
                }
                update_suggestions(&buf, &mut suggestions, &mut selected);
                redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
            }

            // ── Ctrl-A / Home: go to start of line ───────────────────────────
            (KeyCode::Char('a'), KeyModifiers::CONTROL) | (KeyCode::Home, _) => {
                cursor_pos = 0;
                redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
            }

            // ── Ctrl-E / End: go to end of line ──────────────────────────────
            (KeyCode::Char('e'), KeyModifiers::CONTROL) | (KeyCode::End, _) => {
                cursor_pos = buf.len();
                redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
            }

            // ── Left arrow: move cursor left ──────────────────────────────────
            (KeyCode::Left, _) => {
                if cursor_pos > 0 {
                    cursor_pos -= 1;
                    redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
                }
            }

            // ── Right arrow: move cursor right ────────────────────────────────
            (KeyCode::Right, _) => {
                if cursor_pos < buf.len() {
                    cursor_pos += 1;
                    redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
                }
            }

            // ── Up arrow: navigate suggestions UP or history UP ───────────────
            (KeyCode::Up, _) => {
                if !suggestions.is_empty() {
                    // Cycle through suggestions upward.
                    selected = if selected == 0 {
                        suggestions.len() - 1
                    } else {
                        selected - 1
                    };
                    redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
                } else {
                    // History navigation.
                    let current: String = buf.iter().collect();
                    if let Some(entry) = history.up(&current) {
                        buf = entry.chars().collect();
                        cursor_pos = buf.len();
                        update_suggestions(&buf, &mut suggestions, &mut selected);
                        redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
                    }
                }
            }

            // ── Down arrow: navigate suggestions DOWN or history DOWN ─────────
            (KeyCode::Down, _) => {
                if !suggestions.is_empty() {
                    // Cycle through suggestions downward.
                    selected = if selected + 1 >= suggestions.len() {
                        0
                    } else {
                        selected + 1
                    };
                    redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
                } else {
                    // History navigation.
                    if let Some(entry) = history.down() {
                        buf = entry.chars().collect();
                        cursor_pos = buf.len();
                        update_suggestions(&buf, &mut suggestions, &mut selected);
                        redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
                    }
                }
            }

            // ── Tab: complete the selected suggestion ─────────────────────────
            (KeyCode::Tab, _) => {
                if !suggestions.is_empty() {
                    let chosen = suggestions[selected];
                    buf = format!("{} ", chosen).chars().collect();
                    cursor_pos = buf.len();
                    suggestions.clear();
                    erase_suggestions(&mut stdout, 0)?;
                    redraw_line(&mut stdout, prompt, &buf, cursor_pos, &[], 0)?;
                }
            }

            // ── Escape: clear suggestions / clear line ────────────────────────
            (KeyCode::Esc, _) => {
                if !suggestions.is_empty() {
                    suggestions.clear();
                    erase_suggestions(&mut stdout, 0)?;
                    redraw_line(&mut stdout, prompt, &buf, cursor_pos, &[], 0)?;
                } else {
                    // Clear buffer.
                    buf.clear();
                    cursor_pos = 0;
                    redraw_line(&mut stdout, prompt, &buf, cursor_pos, &[], 0)?;
                }
            }

            // ── Backspace: delete char before cursor ──────────────────────────
            (KeyCode::Backspace, _) => {
                if cursor_pos > 0 {
                    cursor_pos -= 1;
                    buf.remove(cursor_pos);
                    update_suggestions(&buf, &mut suggestions, &mut selected);
                    redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
                }
            }

            // ── Delete: delete char under cursor ──────────────────────────────
            (KeyCode::Delete, _) => {
                if cursor_pos < buf.len() {
                    buf.remove(cursor_pos);
                    update_suggestions(&buf, &mut suggestions, &mut selected);
                    redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
                }
            }

            // ── Regular character: insert at cursor ───────────────────────────
            (KeyCode::Char(ch), mods)
                if mods.is_empty() || mods == KeyModifiers::SHIFT =>
            {
                buf.insert(cursor_pos, ch);
                cursor_pos += 1;
                update_suggestions(&buf, &mut suggestions, &mut selected);
                redraw_line(&mut stdout, prompt, &buf, cursor_pos, &suggestions, selected)?;
            }

            // Ignore everything else (F-keys, Ctrl+other, Alt+…, etc.)
            _ => {}
        }
    }; // end loop

    // ── Restore cooked mode ───────────────────────────────────────────────────
    terminal::disable_raw_mode()?;
    Ok(result)
}

// ── Suggestion filtering ──────────────────────────────────────────────────────

/// Recompute which commands match the current buffer.
/// Called after every character insertion/deletion.
fn update_suggestions(buf: &[char], suggestions: &mut Vec<&'static str>, selected: &mut usize) {
    let line: String = buf.iter().collect();
    if line.starts_with('/') {
        // Keep the '/' prefix in the match so "/he" matches "/help".
        let new_sug: Vec<&'static str> = COMMANDS
            .iter()
            .filter(|c| c.cmd.starts_with(line.as_str()))
            .map(|c| c.cmd)
            .collect();
        // Try to keep the currently-selected item selected.
        if !new_sug.is_empty() {
            if *selected >= new_sug.len() {
                *selected = 0;
            }
        } else {
            *selected = 0;
        }
        *suggestions = new_sug;
    } else {
        suggestions.clear();
        *selected = 0;
    }
}

// ── Terminal rendering ────────────────────────────────────────────────────────

/// Erase `count` suggestion lines below the input line, then move the cursor
/// back up to the input line.
///
/// We call this before every redraw so we never leave ghost lines behind.
fn erase_suggestions(stdout: &mut impl Write, count: usize) -> io::Result<()> {
    if count == 0 {
        return Ok(());
    }
    for _ in 0..count {
        // Move down one line, clear it.
        stdout
            .queue(cursor::MoveDown(1))?
            .queue(terminal::Clear(ClearType::CurrentLine))?;
    }
    // Move back up to the input line.
    stdout.queue(cursor::MoveUp(count as u16))?;
    stdout.flush()?;
    Ok(())
}

/// Redraw the input line + suggestion list from scratch.
///
/// Steps:
///   1. Move to column 0, clear the line.
///   2. Print prompt + buffer.
///   3. Erase old suggestion lines (if any were rendered before).
///   4. Print new suggestion lines below.
///   5. Move cursor back to the input line at the correct column.
fn redraw_line(
    stdout: &mut impl Write,
    prompt: &str,
    buf: &[char],
    cursor_pos: usize,
    suggestions: &[&str],
    selected: usize,
) -> io::Result<()> {
    let line: String = buf.iter().collect();

    // ── 1. Redraw input line ──────────────────────────────────────────────────
    // Move to start of line and clear it.
    stdout
        .queue(cursor::MoveToColumn(0))?
        .queue(terminal::Clear(ClearType::CurrentLine))?
        // Print prompt (contains ANSI from `console` crate — use Print so they
        // pass through verbatim).
        .queue(Print(prompt))?
        .queue(Print(&line))?;

    // ── 2. Print suggestion lines ─────────────────────────────────────────────
    // We need to know how many *previous* suggestion lines we rendered so we
    // can erase them.  We always re-render all of them, so just erase the
    // current count below the input line first.
    //
    // Approach: move down/up around the suggestion area.
    if !suggestions.is_empty() {
        // Compute the longest command name for alignment.
        let max_cmd_len = suggestions.iter().map(|s| s.len()).max().unwrap_or(0);

        for (i, &cmd) in suggestions.iter().enumerate() {
            // Move to the next line, go to column 0, clear it.
            stdout
                .queue(Print("\r\n"))?
                .queue(terminal::Clear(ClearType::CurrentLine))?;

            // Find the description for this command.
            let desc = COMMANDS
                .iter()
                .find(|c| c.cmd == cmd)
                .map(|c| c.desc)
                .unwrap_or("");

            if i == selected {
                // Highlighted row: white text.
                stdout
                    .queue(SetForegroundColor(Color::White))?
                    .queue(Print(format!("  {:<width$}  {}", cmd, desc, width = max_cmd_len)))?
                    .queue(ResetColor)?;
            } else {
                // Dimmed row: dark grey.
                stdout
                    .queue(SetForegroundColor(Color::DarkGrey))?
                    .queue(Print(format!("  {:<width$}  {}", cmd, desc, width = max_cmd_len)))?
                    .queue(ResetColor)?;
            }
        }

        // Move back up to the input line.
        stdout.queue(cursor::MoveUp(suggestions.len() as u16))?;
    }

    // ── 3. Position the cursor correctly on the input line ───────────────────
    // `prompt` may contain ANSI escape sequences (from `console::style`).
    // We need the *visible* character width of the prompt, not its byte length.
    // Use `console::measure_text_width` for this.
    let prompt_visible_width = console::measure_text_width(prompt);
    // Cursor column = prompt width + cursor position in the buffer.
    let col = (prompt_visible_width + cursor_pos) as u16;
    stdout.queue(cursor::MoveToColumn(col))?;

    stdout.flush()?;
    Ok(())
}
