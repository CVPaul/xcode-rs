# Changelog

All notable changes to xcodeai are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versions follow [Semantic Versioning](https://semver.org/).

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
