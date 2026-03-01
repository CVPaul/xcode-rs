# Changelog

All notable changes to xcodeai are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versions follow [Semantic Versioning](https://semver.org/).

---


## [0.8.0] — 2026-03-01

### Added

- **Auto-continue loop** — the agent no longer stops after a single LLM turn. When the LLM returns text without tool calls and without the `[TASK_COMPLETE]` marker, the harness automatically injects a "Continue with the next step" message and loops back. This lets the agent execute complex multi-step tasks fully autonomously.
  - Safety limit: `max_auto_continues` (default 20) prevents infinite loops.
  - Visual indicator: `▶ auto-continuing…` printed to stderr so the user sees the agent is still working.
  - If the limit is reached without `[TASK_COMPLETE]`, a yellow warning is shown.
- **`[TASK_COMPLETE]` marker** — the system prompt now instructs the LLM to output `[TASK_COMPLETE]` on its own line at the very end of its final summary. The `looks_like_task_complete()` helper checks for this marker to decide when the entire task is truly done.
- **Task planning workflow in system prompt** — the LLM is now instructed to: PLAN (numbered steps) → EXECUTE (with `## [Step N/M]` headers) → VERIFY → SUMMARIZE → SIGNAL. This gives the user clear progress visibility.
- **`max_auto_continues` config field** — new field in `AgentConfig` (default 20). Configurable in `~/.config/xcode/config.json` under `agent.max_auto_continues`.
- **`auto_continues` in AgentResult** — tracks how many auto-continue injections occurred. Displayed in the completion banner when > 0.

### Fixed

- **Content duplication** — in Act mode, LLM text was printed twice: once during SSE streaming and once after the loop ended. Removed the redundant `println!` in `CoderAgent::run()` so streamed output is no longer duplicated.

### Changed

- **Completion banner** — now shows `✓ task complete` (instead of generic `✓ complete`) with auto-continues count when applicable. REPL and `run` modes both updated.
- **System prompt rewritten** — stronger instructions for autonomous multi-step execution: "Do NOT stop after completing one step", explicit workflow stages, and premature `[TASK_COMPLETE]` guard.

---
## [0.7.0] — 2026-02-28

### Added

- **Confirmation before destructive tool calls** (`--yes` / `-y` flag) — in interactive REPL mode, xcodeai now pauses and asks `⚠ tool_name ( args )  [y/N]:` before executing any tool call that looks potentially dangerous:
  - `bash` commands containing `rm`, `rmdir`, `dd`, `shred`, `mkfs`, `truncate`, `git reset --hard`, `git clean -f`, `git push --force`, SQL `DROP TABLE` / `DROP DATABASE`
  - `file_write` when the target file already exists on disk (overwrite)
  - Answering `n` (or just pressing Enter) injects a synthetic `"Tool call was denied by the user."` result so the LLM can adapt its plan rather than stalling.
  - Pass `--yes` / `-y` to skip all prompts (e.g. in scripts or when you trust the agent).
  - `xcodeai run` is always non-interactive and never prompts.

- **`/undo` command** — type `/undo` in the REPL after an Act-mode run to restore the working tree to its pre-run state:
  - Before each Act-mode run xcodeai automatically runs `git stash push --include-untracked -m xcodeai-undo` to snapshot the current state.
  - `/undo` runs `git stash pop` to roll back all changes the agent made.
  - If the tree was already clean before the run (nothing to stash), `/undo` reports nothing to undo.
  - Gracefully no-ops if the project directory is not a git repository.

### Changed

- **Zero compiler warnings** — all `unused import` and `dead_code` warnings eliminated from both the main binary and test binaries.

---

## [0.6.0] — 2026-02-28

### Added

- **`/` command autocomplete** — typing `/` (or any unrecognised `/xxx` command) now opens an arrow-key menu listing all available commands with their descriptions. Select with ↑/↓, confirm with Enter, cancel with Esc.
- **`/session` session browser** — `/session` now opens an interactive picker showing all past sessions (title + date). Choose a session to resume the conversation, or select **“+ New session”** to start fresh without restarting xcodeai.
- **`/clear` starts a new session** — instead of just printing a warning, `/clear` now immediately creates a new session (equivalent to picking “New session” in `/session`).
- **Session title persistence** — the session title is now actually written to the database when the first message arrives (the stub was a no-op before this release).
- **Fixed `tests/helpers.rs` binary path** — integration tests now reference the correct `xcodeai` binary instead of the old `xcode` name.

---

## [0.5.1] — 2026-02-28

### Fixed

- **Auth persistence**: after a successful `/login` or `/connect → GitHub Copilot`, xcodeai now writes `"api_base": "copilot"` to `~/.config/xcode/config.json`. On the next startup, the program automatically enters Copilot mode and loads the saved OAuth token — no need to re-authenticate every session.

---

## [0.5.0] — 2026-02-28

### Added

- **Plan mode** — type `/plan` to enter a discussion-only mode where the LLM helps you think through your task without writing any files or running commands. Type `/act` to switch back to full execution mode.
- **Mode-aware prompt** — the prompt changes to `[plan] xcodeai›` (yellow) in Plan mode and `xcodeai›` (cyan) in Act mode so you always know which mode you are in.
- **Shared session history** — Plan mode and Act mode share the same session, so context from your discussion carries over when you switch to Act and say "go ahead".
- **`/help` shows current mode** — the help output now displays whether you are in Act or Plan mode.

---

## [0.4.3] — 2026-02-28

### Fixed

- **`/connect` Copilot auth state**: after selecting GitHub Copilot and completing device-code login via `/connect`, subsequent tasks no longer fail with "No API key configured" — the provider is now correctly switched to Copilot mode in-memory without requiring a restart

---


## [0.4.2] — 2026-02-28

### Fixed

- Typing `exit`, `quit`, `q`, or `bye` at the prompt now exits immediately instead of being sent to the agent as a task

---


## [0.4.1] — 2026-02-28

### Fixed

- **`/connect` Copilot flow**: selecting GitHub Copilot from the `/connect` menu now immediately starts the device-code OAuth flow (shows code + opens browser), instead of just printing a hint to run `/login` separately

---


## [0.4.0] — 2026-02-28

### Added

- **Beautiful terminal UI**: styled welcome banner, colored output, and interactive menus using `console` + `dialoguer`
- **Interactive `/connect` menu**: select a provider from a numbered list — no manual URL/key entry required (GitHub Copilot, OpenAI, DeepSeek, Qwen, GLM, Ollama, Custom)
- **Styled prompt**: REPL prompt is now `xcodeai›` in cyan bold
- **Styled helpers**: `✓ ok`, `! warn`, `✗ err`, `  info` — consistent colored output throughout the REPL

### Changed

- **All REPL commands now use `/` prefix** instead of `:` (`/login`, `/logout`, `/connect`, `/model`, `/session`, `/clear`, `/help`, `/exit`)
- Default log level changed from `info` to `warn` — less noise in the styled REPL
- Task completion summary now shows iterations and tool calls inline with dim separators

---


## [0.3.0] — 2026-02-28

### Added

- **GitHub Copilot support**: use `xcodeai` with your Copilot subscription — no OpenAI key needed
- **Device-code OAuth flow**: `:login` opens GitHub authorization in the browser, persists the token to `~/.config/xcode/copilot_auth.json`
- **Auto token refresh**: Copilot API tokens (~25 min TTL) are refreshed transparently before every LLM call
- **REPL command `:login`**: start Copilot device-code authorization from within the REPL
- **REPL command `:logout`**: remove stored Copilot credentials
- **REPL command `:connect <url> [key]`**: switch provider inline (use `copilot` as the URL for Copilot)
- **REPL command `:model <name>`**: display current model / note model to use on next restart
- **Lazy API key check**: `xcodeai` starts without requiring any API key configured; auth errors surface only when a task is actually run
- **Auth status banner**: welcome screen now shows authentication status (Copilot auth state or provider URL)

### Changed

- `AgentContext::new` no longer bails on missing API key — callers validate lazily after checking provider mode
- `run` subcommand now skips key validation when provider is Copilot
- REPL `:help` updated to list all new commands

---

## [0.2.0] — 2026-02-28

### Added

- **Interactive REPL mode**: running `xcodeai` with no arguments now enters a persistent conversation loop
- Same session maintained across all tasks within one REPL session — full history preserved
- Command history with arrow-key navigation, stored at `~/.local/share/xcode/repl_history.txt`
- Special REPL commands: `:exit`, `:quit`, `:q`, `:session`, `:clear`, `:help`
- Top-level flags (`--project`, `--no-sandbox`, `--model`, `--provider-url`, `--api-key`) for REPL mode
- `xcodeai run <task>` continues to work exactly as before (non-interactive single-shot)

---

## [0.1.0] — 2026-02-28

Initial release.

### Added

**Core agent loop**
- Director → Orchestrator → Coder agent architecture
- Autonomous LLM ↔ tool loop — runs until task complete or `max_iterations` reached
- Context window management: keeps system prompt + most recent messages when near token limit
- Hard limits: `max_iterations` (default 25), `max_tool_calls_per_response` (default 10)
- Tool error recovery: errors passed back to LLM as tool results, agent decides how to continue

**LLM client**
- OpenAI-compatible streaming SSE client (`/v1/chat/completions`)
- Works with OpenAI, DeepSeek, Qwen, GLM, Ollama, and any OpenAI-compatible endpoint
- Streaming text printed to stdout as it arrives
- Exponential backoff retry for transient errors (429, 5xx), max 3 retries
- Tool call assembly from streaming SSE chunks

**Built-in tools**
- `file_read` — read file with line numbers, `offset`/`limit` for large files
- `file_write` — write or create files, creates parent directories automatically
- `file_edit` — string replacement in files, errors on missing or ambiguous matches
- `bash` — execute shell commands, 120s default timeout, 50KB output limit with head/tail truncation
- `glob_search` — find files by glob pattern, max 100 results sorted by modification time
- `grep_search` — search file contents by regex with file path, line number, and content, max 200 matches

**Configuration**
- JSON config file at `~/.config/xcode/config.json`, created with defaults on first run
- Environment variable overrides: `XCODE_API_KEY`, `XCODE_API_BASE`, `XCODE_MODEL`
- CLI flag overrides: `--api-key`, `--provider-url`, `--model`, `--project`, `--no-sandbox`
- Precedence: defaults → config file → env vars → CLI flags

**Session persistence**
- SQLite database at `~/.local/share/xcode/sessions.db`
- Stores full conversation history (all roles: system, user, assistant, tool)
- `xcodeai session list` — list recent sessions
- `xcodeai session show <id>` — show full conversation for a session
- Auto-generated session titles from first user message

**Sandboxing**
- Optional sbox integration for rootless user-space session isolation
- `--no-sandbox` flag and `sandbox.enabled: false` config for direct execution
- Graceful fallback when sbox is not installed

**CLI**
- `xcodeai run <message>` with `--project`, `--no-sandbox`, `--model`, `--provider-url`, `--api-key`
- `xcodeai session list [--limit N]`
- `xcodeai session show <id>`
- Clear error message when API key is missing

**Test suite**
- 58 unit tests covering config, LLM types, all 6 tools, agent loop, session persistence
- 4 integration tests using an axum mock SSE server (no real LLM required)
- Tests for: simple text response, tool call creating a file, LLM error handling, session persistence

[0.1.0]: https://crates.io/crates/xcodeai/0.1.0
