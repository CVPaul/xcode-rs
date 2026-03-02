# xcodeai

[![Crates.io](https://img.shields.io/crates/v/xcodeai.svg)](https://crates.io/crates/xcodeai)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Fully autonomous AI coding agent in Rust. Give it a task — it writes the code, runs the tools, and finishes without asking for permission.

Built as a complete, lightweight replacement for [opencode](https://opencode.ai): no TUI dependency, works on Termux, controllable via HTTP API for chat-interface integrations (e.g. 企业微信 vibe coding).

---

## Install

```bash
cargo install xcodeai
```

Requires Rust 1.75+. The binary is placed at `~/.cargo/bin/xcodeai`.

---

## Quick Start

```bash
export XCODE_API_KEY=sk-your-key

xcodeai run "Create a hello world HTTP server in main.rs" --project ./myproject --no-sandbox
```

That's it. The agent loops until the task is done.

---

## Configuration

xcodeai looks for a config file at `~/.config/xcode/config.json`. On first run, a default template is created automatically.

```json
{
  "provider": {
    "api_base": "https://api.openai.com/v1",
    "api_key": "sk-..."
  },
  "model": "gpt-4o",
  "sandbox": {
    "enabled": false
  },
  "agent": {
    "max_iterations": 25,
    "max_tool_calls_per_response": 10
  }
}
```

### Environment Variables

| Variable | Description | Example |
|---|---|---|
| `XCODE_API_KEY` | API key | `sk-abc123` |
| `XCODE_API_BASE` | Provider base URL | `https://api.deepseek.com/v1` |
| `XCODE_MODEL` | Model name | `deepseek-chat` |

### Precedence (low → high)

```
defaults → config file → env vars → CLI flags
```

CLI flags always win.

---

## Usage

### Interactive REPL (default)

Just run `xcodeai` with no arguments to enter the interactive loop:

```bash
xcodeai
```

```
  ✦ xcodeai v2.0.0  ·  gpt-4o  ·  /home/user/myproject  ·  no auth
  Type your task. /help for commands. Ctrl-D to exit.
──────────────────────────────────────────────────────────────────
xcodeai› Add error handling to all functions in lib.rs
(agent runs autonomously...)
──────────────────────────────────────────────────────────────────
  ✓ done · 3 iterations · 7 tool calls
──────────────────────────────────────────────────────────────────

xcodeai› Now write tests for the new error handling
(agent runs...)
```

Each session is saved automatically — use `/session` to see the session ID.

You can pass the same flags as `run`:

```bash
xcodeai --project ./mylib --model deepseek-chat --no-sandbox
```

Pass `--no-markdown` to disable terminal markdown rendering:

```bash
xcodeai --no-markdown
```

#### REPL special commands

| Command | Effect |
|---|---|
| `/plan` | Switch to **Plan mode** — discuss & clarify your task with the LLM (no file writes) |
| `/act` | Switch back to **Act mode** — full tool execution |
| `/undo` | Undo the last Act-mode run (restores git state via `git stash pop`) |
| `/undo N` | Undo the last N runs |
| `/undo list` | Show the undo history for this session |
| `/login` | GitHub Copilot device-code OAuth (browser + code) |
| `/logout` | Remove saved Copilot credentials |
| `/connect` | Interactive provider selector — pick from built-in presets |
| `/model [name]` | Show current model or switch immediately (`/model gpt-4o`) |
| `/session` | Browse history or start a new session |
| `/clear` | Start a fresh session (same as "New session" in `/session`) |
| `/compact` | Summarise conversation history to reduce token usage |
| `/help` | Show all commands + current mode |
| `/exit` / `/quit` / `/q` | Exit xcodeai |
| `Ctrl+C` | Clear current input line |
| `Ctrl+D` | Exit xcodeai |

#### Plan Mode

Plan mode lets you have a free-form discussion with the LLM to clarify your task before executing anything.

```
xcodeai› /plan
  ⟳ Switched to Plan mode — discuss your task freely. /act to execute.

[plan] xcodeai› I want to refactor the database module but I'm not sure whether to
              use the repository pattern or keep it procedural.

(LLM discusses tradeoffs, asks clarifying questions, produces a plan…)

[plan] xcodeai› Let's go with the repository pattern. Generate the plan.

(LLM outlines exact steps…)

[plan] xcodeai› /act
  ⟳ Switched to Act mode — ready to execute.

xcodeai› Go ahead and implement the plan.

(agent executes autonomously, with context from the discussion above)
```

#### Multi-Step Undo

xcodeai records a git stash entry for every Act-mode agent run. You can rewind multiple steps:

```
xcodeai› /undo          # undo the most recent run
xcodeai› /undo 3        # undo the last 3 runs
xcodeai› /undo list     # see undo history (up to 10 entries)
```

Undo requires the project directory to be a git repository.

### Run a coding task

```bash
# Basic — uses config file for provider settings
xcodeai run "Add error handling to all functions in lib.rs" --project ./mylib

# Override provider inline
xcodeai run "Write tests for src/parser.rs" \
  --project . \
  --provider-url https://api.deepseek.com/v1 \
  --api-key sk-xxx \
  --model deepseek-chat

# Skip sandbox (direct execution)
xcodeai run "Refactor the database module" --project . --no-sandbox

# Disable markdown rendering in output
xcodeai run "Summarise all TODOs" --project . --no-markdown

# All flags
xcodeai run --help
```

### HTTP API Server

Start xcodeai as an HTTP server — useful for chat-interface integrations (企业微信, web UIs, scripts):

```bash
xcodeai serve                    # listens on 0.0.0.0:8080 (default)
xcodeai serve --addr 127.0.0.1:9090
```

#### Endpoints

| Method | Path | Description |
|---|---|---|
| `POST` | `/sessions` | Create a new session |
| `GET` | `/sessions` | List recent sessions (latest 50) |
| `GET` | `/sessions/:id` | Get one session with its message history |
| `DELETE` | `/sessions/:id` | Delete a session |
| `POST` | `/sessions/:id/messages` | Send a message and stream agent output via SSE |

#### SSE Event Types

`POST /sessions/:id/messages` returns a `text/event-stream` response. Each event has a named type:

| Event name | Data fields | Meaning |
|---|---|---|
| `status` | `{"msg": "..."}` | Agent progress update or final response text |
| `tool_call` | `{"name": "...", "args": "..."}` | Agent is about to call a tool |
| `tool_result` | `{"preview": "...", "is_error": bool}` | Result of a tool call |
| `error` | `{"msg": "..."}` | Agent-level error (not a tool error) |
| `complete` | `{}` | Agent finished; stream ends |

#### Example curl session

```bash
# Create a session
SESSION=$(curl -s -X POST http://localhost:8080/sessions \
  -H 'Content-type: application/json' \
  -d '{"title":"my task"}' | jq -r .session_id)

# Run an agent task, streaming output
curl -N http://localhost:8080/sessions/$SESSION/messages \
  -X POST \
  -H 'Content-type: application/json' \
  -d '{"content":"Create a Fibonacci function in fib.rs"}'

# List all sessions
curl http://localhost:8080/sessions

# Get session with history
curl http://localhost:8080/sessions/$SESSION

# Delete it
curl -X DELETE http://localhost:8080/sessions/$SESSION
```

#### Image Attachments

Send image files alongside a message using the `images` field:

```bash
curl -X POST http://localhost:8080/sessions/$SESSION/messages \
  -H 'Content-type: application/json' \
  -d '{"content":"Implement the UI shown in this screenshot","images":["/path/to/screenshot.png"]}'
```

Images are read from disk and base64-encoded before being sent to the LLM. All three built-in providers (OpenAI, Anthropic, Gemini) support multimodal image input.

### Session management

```bash
# List recent sessions
xcodeai session list

# Show full conversation for a session
xcodeai session show <session-id>
```

---

## Supported Providers

Any OpenAI-compatible API endpoint works, plus native Anthropic and Gemini support:

| Provider | api_base | Notes |
|---|---|---|
| OpenAI | `https://api.openai.com/v1` | GPT-4o, o1, etc. |
| Anthropic | `https://api.anthropic.com` | Claude 3.x — native, not OpenAI-compat |
| Gemini | `https://generativelanguage.googleapis.com` | Gemini 1.5+ — native |
| DeepSeek | `https://api.deepseek.com/v1` | OpenAI-compat |
| Qwen (Alibaba Cloud) | `https://dashscope.aliyuncs.com/compatible-mode/v1` | OpenAI-compat |
| GLM (Zhipu AI) | `https://open.bigmodel.cn/api/paas/v4` | OpenAI-compat |
| Local (Ollama) | `http://localhost:11434/v1` | OpenAI-compat |
| **GitHub Copilot** | `copilot` (special sentinel) | Device-code OAuth, no API key needed |

### GitHub Copilot Authentication

xcodeai supports your GitHub Copilot subscription via device-code OAuth — no separate API key needed.

```bash
# 1. Start xcodeai
xcodeai --provider-url copilot

# 2. In the REPL, authenticate:
xcodeai› /login

# GitHub shows a code and URL:
#   Visit: https://github.com/login/device
#   Enter code:  XXXX-XXXX
#
# After you approve in the browser:
#   ✓ Logged in to GitHub Copilot.

# 3. Now run tasks normally
xcodeai> Write a Fibonacci function in main.rs
```

The OAuth token is saved to `~/.config/xcode/copilot_auth.json`. Future sessions authenticate automatically. The short-lived Copilot API token (~25 min TTL) is refreshed transparently.

```bash
# Remove saved credentials
xcodeai› /logout
```

---

## Tools

The agent has access to built-in tools plus optional Git, LSP, MCP, and orchestration tools:

### Built-in Tools

| Tool | Description | Key Parameters |
|---|---|---|
| `file_read` | Read file content with line numbers | `path`, `offset`, `limit` |
| `file_write` | Write or create a file | `path`, `content` |
| `file_edit` | Replace a string in a file | `path`, `old_string`, `new_string` |
| `bash` | Execute a shell command | `command`, `timeout` (default 120s) |
| `glob_search` | Find files by glob pattern | `pattern`, `path` (max 100 results) |
| `grep_search` | Search file contents by regex | `pattern`, `path`, `include` (max 200 matches) |
| `question` | Ask the user a clarifying question | `question` |

### Git Tools

| Tool | Description |
|---|---|
| `git_status` | Show working tree status |
| `git_diff` | Show staged/unstaged changes |
| `git_log` | Show recent commit history |
| `git_commit` | Stage all changes and create a commit |
| `git_checkout` | Switch branches or restore files |

### LSP Tools

| Tool | Description |
|---|---|
| `lsp_hover` | Get type info / docs at a position |
| `lsp_definition` | Jump to symbol definition |
| `lsp_references` | Find all usages of a symbol |
| `lsp_diagnostics` | Get errors and warnings from the language server |
| `lsp_rename` | Rename a symbol across the whole project |

### Orchestration Tools (spawn_task)

The `spawn_task` tool lets the agent delegate sub-tasks to child agents — enabling multi-agent workflows with up to 3 levels of nesting.

```
Parent agent
  └── spawn_task("Write all unit tests")
        └── Child agent (full tool access)
```

### MCP Tools

xcodeai can connect to any [Model Context Protocol](https://modelcontextprotocol.io) server, automatically registering all tools the server exposes.

Configure in `~/.config/xcode/config.json`:
```json
{
  "mcp": {
    "servers": [
      { "name": "my-server", "command": "npx", "args": ["-y", "@my/mcp-server"] }
    ]
  }
}
```

---

## Agent Architecture

```
xcodeai run "task"
      │
      ▼
   Director
      │
      ▼
    Coder ◄───────────────────────────┤
      │                          │
      ▼                          │
  LLM call (streaming SSE)       │
      │                          │
      ▼                          │
  tool_calls?                    │
  ├── yes → execute tools ───────┘
  └── no  → task complete
```

- **Director** — entry point, creates the CoderAgent and executes the task
- **Coder** — runs the LLM ↔ tool loop until no more tool calls or `max_iterations` reached
- **Context management** — keeps system prompt + last N messages when approaching the context window limit
- **Compact mode** — `/compact` summarises conversation history to reduce token usage
- **Session persistence** — every run is stored in SQLite at `~/.local/share/xcode/sessions.db`
- **Token tracking** — prompt/completion/total tokens displayed after each run
- **AGENTS.md** — place an `AGENTS.md` file in your project root to inject project-specific instructions into the system prompt

---

## Sandboxing

By default, xcodeai runs tools directly in the project directory. Optionally, install [sbox](https://github.com/CVPaul/sbox) for rootless user-space session isolation:

```json
{ "sandbox": { "enabled": true, "sbox_path": "/usr/local/bin/sbox" } }
```

Or disable per-run:

```bash
xcodeai run "task" --no-sandbox
```

---

## Development

```bash
git clone <repo>
cd xcode

# Build
export PATH="$HOME/.cargo/bin:$PATH"
cargo build

# Run tests (671 total)
cargo test

# Release binary
cargo build --release
./target/release/xcodeai --help

# Lint (zero warnings enforced)
cargo clippy -- -D warnings
cargo fmt --check
```

### Project Structure

```
src/
├── main.rs            CLI entry point (clap) + serve_command()
├── lib.rs             Public API surface for integration tests
├── config.rs          Config loading with env/CLI overrides
├── context.rs         AgentContext — shared agent state
├── agent/             Director + CoderAgent loop + AGENTS.md loader
├── auth/              GitHub Copilot device-code OAuth
├── http/              HTTP API server (axum) — serve subcommand
│   ├── mod.rs         AppState + start_server()
│   └── routes.rs      REST + SSE route handlers
├── io/                AgentIO trait for pluggable output
│   ├── mod.rs         AgentIO trait + NullIO + AutoApproveIO
│   ├── terminal.rs    TerminalIO — REPL/run output with markdown rendering
│   └── http.rs        HttpIO — SSE event channel for HTTP API
├── llm/               LLM providers + streaming
│   ├── mod.rs         LlmProvider trait, Message, ContentPart (multimodal)
│   ├── openai.rs      OpenAI / OpenAI-compat SSE client
│   ├── anthropic.rs   Anthropic native SSE client
│   ├── gemini.rs      Gemini native SSE client
│   ├── registry.rs    ProviderRegistry — select provider by URL
│   └── retry.rs       RetryingLlmProvider — exponential backoff
├── tools/             Tool trait + registry + all tools
│   ├── mod.rs         ToolRegistry, ToolContext
│   ├── bash.rs, file_*.rs, glob_search.rs, grep_search.rs, question.rs
│   ├── git/           Git tools (status, diff, log, commit, checkout)
│   ├── lsp/           LSP tools (hover, definition, references, diagnostics, rename)
│   ├── mcp_resource.rs MCP resource-read tool
│   └── spawn_task.rs  Multi-agent orchestration tool
├── session/           Session types + SQLite store + undo history
├── sandbox/           SboxSession + NoSandbox implementations
├── repl/              Interactive REPL loop + slash command dispatch
├── lsp/               LSP client (JSON-RPC 2.0 over stdio)
├── mcp/               MCP client (JSON-RPC 2.0 over stdio)
├── orchestrator/      Multi-step task graph executor
├── tracking.rs        Token usage tracking
└── ui.rs              Console styling helpers
tests/
├── mock_llm_server.rs   axum mock SSE server for integration tests
├── helpers.rs           Shared test utilities
├── e2e_run.rs           End-to-end integration tests
└── http_integration.rs  HTTP API integration tests
```

---

## License

MIT — see [LICENSE](LICENSE).
