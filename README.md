# xcodeai

[![Crates.io](https://img.shields.io/crates/v/xcodeai.svg)](https://crates.io/crates/xcodeai)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Fully autonomous AI coding agent in Rust. Give it a task — it writes the code, runs the tools, and finishes without asking for permission.

Built as a lightweight alternative to opencode: no permission prompts, no heavy containers, optional rootless sandboxing via [sbox](https://github.com/CVPaul/sbox).

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

# All flags
xcodeai run --help
```

### Session management

```bash
# List recent sessions
xcodeai session list

# Show full conversation for a session
xcodeai session show <session-id>
```

---

## Supported Providers

Any OpenAI-compatible API endpoint works:

| Provider | api_base |
|---|---|
| OpenAI | `https://api.openai.com/v1` |
| DeepSeek | `https://api.deepseek.com/v1` |
| Qwen (Alibaba Cloud) | `https://dashscope.aliyuncs.com/compatible-mode/v1` |
| GLM (Zhipu AI) | `https://open.bigmodel.cn/api/paas/v4` |
| Local (Ollama) | `http://localhost:11434/v1` |

---

## Tools

The agent has access to six built-in tools:

| Tool | Description | Key Parameters |
|---|---|---|
| `file_read` | Read file content with line numbers | `path`, `offset`, `limit` |
| `file_write` | Write or create a file | `path`, `content` |
| `file_edit` | Replace a string in a file | `path`, `old_string`, `new_string` |
| `bash` | Execute a shell command | `command`, `timeout` (default 120s) |
| `glob_search` | Find files by glob pattern | `pattern`, `path` (max 100 results) |
| `grep_search` | Search file contents by regex | `pattern`, `path`, `include` (max 200 matches) |

---

## Agent Architecture

```
xcodeai run "task"
      │
      ▼
   Director
      │
      ▼
  Orchestrator
      │
      ▼
    Coder ◄──────────────────────┐
      │                          │
      ▼                          │
  LLM call (streaming SSE)       │
      │                          │
      ▼                          │
  tool_calls?                    │
  ├── yes → execute tools ───────┘
  └── no  → task complete
```

- **Director** — entry point, initialises the Orchestrator
- **Orchestrator** — sets context, delegates to Coder
- **Coder** — runs the LLM ↔ tool loop until no more tool calls or `max_iterations` reached
- **Context management** — keeps system prompt + last N messages when approaching the context window limit
- **Session persistence** — every run is stored in SQLite at `~/.local/share/xcode/sessions.db`

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

# Run tests (62 unit + integration)
cargo test

# Release binary
cargo build --release
./target/release/xcodeai --help

# Lint
cargo clippy
cargo fmt --check
```

### Project Structure

```
src/
├── main.rs           CLI entry point (clap)
├── config.rs         Config loading with env/CLI overrides
├── llm/              LlmProvider trait + OpenAI SSE streaming client
├── tools/            Tool trait, ToolRegistry, 6 built-in tools
├── agent/            Director → Orchestrator → Coder loop
├── session/          Session types + SQLite store
└── sandbox/          SboxSession + NoSandbox implementations
tests/
├── mock_llm_server.rs  axum mock SSE server for integration tests
├── helpers.rs          Shared test utilities
└── e2e_run.rs          End-to-end integration tests
```

---

## License

MIT — see [LICENSE](LICENSE).
