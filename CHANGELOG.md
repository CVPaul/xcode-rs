# Changelog

All notable changes to xcodeai are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versions follow [Semantic Versioning](https://semver.org/).

---


## [2.0.0] — 2026-03-01

### Phase 4: 企业微信 Ready — HTTP API, HttpIO, Image Support, Undo History, Integration Tests

All tests pass (671 passing + 10 ignored doctests). `cargo clippy` reports 0 warnings.

#### Added

- **Task 31 — HTTP API Server** (`src/http/mod.rs`, `src/http/routes.rs`): axum-based REST API server. `xcodeai serve --port 8080` starts it. Session CRUD endpoints (`POST/GET/DELETE /sessions`, `GET /sessions/:id`, `GET /sessions/:id/messages`). CORS enabled via `tower-http`. `AppState` wraps `SessionStore` in `tokio::sync::Mutex` for `!Sync` safety.

- **Task 32 — `HttpIO`** (`src/io/http.rs`): `AgentIO` implementation for HTTP mode. Uses `tokio::sync::mpsc` channel to stream `SseEvent`s (`Status`, `ToolCall`, `ToolResult`, `Error`, `Complete`) from the agent loop back to the SSE handler. `confirm_destructive` auto-approves in HTTP mode.

- **Task 33 — Image / Multimodal Support** (`src/llm/mod.rs`): `image_to_content_part()` helper reads image files from disk, infers MIME type, base64-encodes them, and returns `ContentPart::ImageUrl` with a data URI. OpenAI, Anthropic, and Gemini provider encoders updated to handle `ImageUrl` parts.

- **Task 34 — HTTP Agent Loop Endpoint** (`src/http/routes.rs`): `POST /sessions/:id/messages` spawns an async agent loop and streams events back as Server-Sent Events. Enforces 409 Conflict for concurrent executions via `AppState.active_sessions`. Persists assistant response to SQLite after completion.

- **Task 35 — Markdown Rendering** (`src/io/terminal.rs`): `TerminalIO` now renders agent output through `termimad` for styled markdown in the terminal. `--no-markdown` flag disables rendering. `TerminalIO { no_markdown: bool }` struct.

- **Task 36 — Multi-Step Undo History** (`src/session/store.rs`, `src/repl/commands.rs`): `undo_history` SQLite table (max 10 entries per session). `/undo` REPL command supports `bare` (undo last), `list`, and `/undo N` (undo last N steps). Each undo stash uses a unique UUID label.

- **Task 37 — HTTP API Integration Tests** (`tests/http_integration.rs`, `src/lib.rs`): 6 real-TCP integration tests using `reqwest` against a live axum server backed by an in-memory SQLite store. Tests cover: 404 for missing session, 409 for concurrent execution, full SSE lifecycle, CRUD over TCP, CORS headers, SSE event termination. Added `src/lib.rs` to expose crate modules to integration tests.

---


## [1.2.0] — 2026-03-01

### Phase 3: Extensibility — MCP, Multi-Provider, Task Orchestration

All 303 tests pass (299 unit + 4 integration). `cargo clippy` reports 0 warnings.

#### Added

- **Task 20 — MCP Client** (`src/mcp/mod.rs`, `src/mcp/transport.rs`, `src/mcp/types.rs`): Full JSON-RPC 2.0 client over stdio transport with Content-Length framing (same protocol as LSP). Handles `initialize`/`initialized` handshake, `tools/list`, `tools/call`, `resources/list`, `resources/read`. Async request/response with sequential ID tracking. 27 unit tests.

- **Task 21 — Anthropic Native Provider** (`src/llm/anthropic.rs`): Implements `LlmProvider` against the Anthropic Messages API. SSE streaming with `content_block_delta` events, native tool-use content blocks (`tool_use` / `tool_result`), `x-api-key` auth header, `anthropic-version` header, token usage from `usage` field in response.

- **Task 22 — Task Graph** (`src/orchestrator/graph.rs`): `TaskGraph` DAG with `TaskNode`, `TaskStatus`. Kahn's algorithm topological sort, `compute_waves()` for parallel grouping, `next_ready()` for dependency-satisfied tasks, cycle detection. JSON serialisation. 20 unit tests.

- **Task 23 — Gemini Native Provider** (`src/llm/gemini.rs`): Implements `LlmProvider` against the Google Generative Language API. SSE streaming (each `data:` chunk is a partial JSON), `function_declarations` tool format, `function_response` parts for tool results, `?key=` query param auth, token usage from `usageMetadata`.

- **Task 24 — MCP Tool Discovery & Registration** (`src/mcp/bridge.rs`): `register_mcp_tools(client, registry)` calls `tools/list` on a connected MCP server and registers each discovered tool as a `McpTool` in the `ToolRegistry`. Discovered tools execute via `tools/call` and return `ToolResult`.

- **Task 25 — MCP Resource Access** (`src/tools/mcp_resource.rs`): `mcp_resource` tool lets the agent read MCP server resources by URI (`resources/read`). Returns text or base64-decoded content. Gracefully handles missing MCP client with a clear error.

- **Task 26 — Task Graph Executor** (`src/orchestrator/executor.rs`): `TaskExecutor` drives parallel wave execution. Each wave's ready tasks spawn concurrent `tokio` tasks. Completed/failed results flow back via `JoinSet`. Executor updates graph state between waves. 4 e2e-style tests.

- **Task 27 — Provider Registry** (`src/llm/registry.rs`): `ProviderRegistry::create_provider(base, key)` dispatches on sentinel strings (`copilot`, `anthropic`, `gemini`) or URL prefix to instantiate the correct `LlmProvider`. Used by `AgentContext` and `/connect`.

- **Task 28 — `spawn_task` Tool** (`src/tools/spawn_task.rs`): Lets the agent create a sub-agent task on the fly. Accepts `description` and optional `agent_config` overrides. Builds a `TaskGraph` with one node, runs it through `TaskExecutor`, and returns the `AgentResult` as a tool result. Depth-limited to prevent infinite nesting.

- **Task 29 — MCP Config + REPL Integration** (`src/config.rs`, `src/context.rs`, `src/repl/commands.rs`): `McpServerConfig` struct added to `Config` with `#[serde(default)]` — old configs without `mcp_servers` continue to work. `AgentContext::new()` is now `async` and spawns all configured MCP servers at startup, registering their tools before `Arc::new(registry)`. `/mcp` REPL command lists connected servers and their tools.

---

## [1.1.0] — 2026-03-01

### Phase 2: Project Understanding

All 168 tests pass (164 unit + 4 integration). `cargo clippy` reports 0 warnings.

#### Added

- **Task 12 — LSP Client** (`src/lsp/mod.rs`, `src/lsp/transport.rs`): JSON-RPC 2.0 over stdio. Handles Content-Length framing, `initialize`/`initialized` handshake, `shutdown`/`exit` teardown, and request/response ID tracking. Auto-detects server (`rust-analyzer`, `typescript-language-server`, `pylsp`) from project files. Config option `lsp.server_command` overrides auto-detection.

- **Task 13 — `git_diff` tool** (`src/tools/git_diff.rs`): Shows unstaged diffs, staged diffs (`staged: true`), or diffs against a commit (`commit: "HEAD~1"`). Optional `path` filter. Output truncated at 50 KB.

- **Task 14 — `git_commit` tool** (`src/tools/git_commit.rs`): Stages a list of files (or all changes) then commits with the provided message. Marked destructive — requires user confirmation in interactive REPL mode. Returns the new commit hash.

- **Task 15 — `git_log` & `git_blame` tools** (`src/tools/git_log.rs`, `src/tools/git_blame.rs`): `git_log` supports count, path filter, oneline/full format, and commit-message grep. `git_blame` supports optional line range (`start_line` / `end_line`).

- **Task 16 — Compact mode** (`src/config.rs`, `src/agent/coder.rs`, `src/tools/file_read.rs`, `src/repl/commands.rs`): `/compact` REPL command and `--compact` CLI flag toggle compact mode. In compact mode, tool results are capped at 50 lines, file reads return focused windows, and the system prompt is augmented with a concise-output instruction.

- **Task 17 — `lsp_diagnostics` tool** (`src/tools/lsp_diagnostics.rs`): Opens a file via `textDocument/didOpen`, collects `textDocument/publishDiagnostics` notifications, and returns formatted `file:line:col: severity: message` output. Optional `severity` filter (error / warning / all). LSP client started lazily on first use.

- **Task 18 — `lsp_goto_definition` & `lsp_find_references` tools** (`src/tools/lsp_goto_def.rs`, `src/tools/lsp_references.rs`): `lsp_goto_definition` sends `textDocument/definition` and returns `file:line:col` locations. `lsp_find_references` sends `textDocument/references` and returns all reference locations. Both reuse the shared `ensure_lsp_started`, `path_to_uri`, `detect_language_id`, and `parse_locations` helpers.

---

## [1.0.0] — 2026-03-01

### Phase 1: Autonomous Reliability (Production-Ready)

All 129 tests pass (125 unit + 4 integration). `cargo clippy` reports 0 warnings.

#### Added

- **Task 7 — Retry & Error Recovery** (`src/llm/retry.rs`): Transparent retry layer wraps every LLM HTTP call. Retries on 429, 500, 502, 503, timeout, and network errors with exponential backoff (1 s → 2 s → 4 s, capped at 60 s). Respects `Retry-After` response header. Permanent errors (400, 401, 403, 404) propagate immediately. Configurable `max_retries` (default 5).

- **Task 8 — AGENTS.md Auto-Loading** (`src/agent/agents_md.rs`): On every run, xcodeai walks up from the project root looking for `AGENTS.md`, `.agents.md`, `xcodeai.agents.md` (or lowercase variants). The first file found is prepended verbatim to the system prompt as project-specific rules. Lets teams encode coding style, tool restrictions, and workflow conventions directly in the repo.

- **Task 9 — Token & Cost Tracking** (`src/tracking.rs`): Every LLM turn records `prompt_tokens`, `completion_tokens`, and a cost estimate (based on known per-million-token rates for GPT-4o, GPT-4o-mini, DeepSeek, Copilot). Per-turn usage is printed in the agent footer. The REPL `/tokens` command shows a full session breakdown. Totals are persisted to SQLite in the sessions table.

- **Task 10 — Smart Context Window Management** (`src/agent/context_manager.rs`): `ContextManager` monitors accumulated message size against a configurable budget (default 400 000 chars, 80 % threshold). When the threshold is crossed it first tries LLM-based summarisation (sends the oldest messages to the LLM and replaces them with a concise summary); if summarisation fails or `strategy = truncate` is set, it falls back to hard truncation, always preserving the system prompt and the most recent messages. A `[context compacted]` marker is injected so the model knows history was condensed.

---

## [0.9.0] — 2026-03-01

### Phase 0: Architectural Refactor

This release completes a full architectural cleanup preparing the codebase for Phase 1 (autonomous reliability) and beyond. No user-visible behaviour changes; all 90 tests pass.

#### Refactors

- **Extract REPL module** (`src/repl/mod.rs`, `src/repl/commands.rs`): The interactive REPL loop and all `/command` handlers are now in their own module. `main.rs` is reduced to CLI declarations and dispatch only.

- **Extract AgentContext** (`src/context.rs`): `AgentContext` struct and its builder are now in a dedicated module. Both REPL mode and `run` subcommand share the same context construction path with no duplication.

- **Usage struct** (`src/llm/mod.rs`): `LlmResponse` now carries an `Option<Usage>` field (`prompt_tokens`, `completion_tokens`, `total_tokens`). The OpenAI client requests `stream_options: { include_usage: true }` and parses usage from the final SSE chunk.

- **ContentPart enum** (`src/llm/mod.rs`): `Message.content` is now `Vec<ContentPart>` instead of `Option<String>`. Supports `Text`, `ImageUrl`, `ToolUse`, and `ToolResult` variants. Custom `Serialize`/`Deserialize` maintains full backwards compatibility with existing session databases — old string-content sessions still load correctly.

- **AgentIO trait** (`src/io/`): A new `AgentIO` trait decouples the agent loop from the terminal. Three implementations ship: `TerminalIO` (interactive REPL, reads stdin for confirmations), `AutoApproveIO` (batch `run` mode — outputs to terminal but never blocks stdin), and `NullIO` (silent, for unit tests). `ToolContext` now holds `Arc<dyn AgentIO>` instead of a bare `confirm_destructive: bool` flag.

#### Bug fixes

- `xcodeai run` (non-interactive batch mode) now uses `AutoApproveIO` instead of `TerminalIO`, matching the old `confirm_destructive: false` behaviour and preventing potential stdin hangs in non-interactive environments.

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
