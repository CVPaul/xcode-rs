# xcode v1 — Autonomous AI Coding Agent in Rust

## TL;DR

> **Quick Summary**: Build xcode, a fully autonomous AI coding agent in Rust that replaces opencode. Uses sbox (rootless user-space sandbox) for session-level isolation instead of permission prompts — zero human intervention required. CLI-first (headless), with Director+Workers agent architecture.
> 
> **Deliverables**:
> - `xcode` Rust binary with `run` subcommand for autonomous coding
> - OpenAI-compatible LLM provider (covers DeepSeek, Qwen, GLM, iQuest, OpenAI)
> - 2 agent types: Orchestrator (director) + Coder (worker)
> - 6 tools: bash, file_read, file_write, file_edit, glob_search, grep_search
> - sbox session-level sandboxing for all tool execution
> - SQLite-backed session persistence with history
> - JSON config file for provider/model/project settings
> - Unit + integration test suite
> 
> **Estimated Effort**: Large (10-12 tasks, ~8-12k lines of Rust)
> **Parallel Execution**: YES - 4 waves
> **Critical Path**: Task 1 → Task 2 → Task 3 → Task 5 → Task 7 → Task 8 → Task 9 → Task 10

---

## Context

### Original Request
User wants to build "xcode" — a fully autonomous AI coding agent to replace opencode. Key motivations:
- opencode requires human confirmation for tool execution (permissions system)
- opencode run has bugs in non-interactive mode (PR #14607)
- User wants zero human intervention during agent execution
- Traditional sandboxing (Docker/Podman/VMs) is too heavy — user built sbox for lightweight rootless isolation

### Interview Summary
**Key Discussions**:
- **Language**: Rust chosen (user is a Rust learner — plan must be extra detailed about crates and patterns)
- **Interface**: Headless CLI first (`xcode run "message"`), TUI deferred to v2
- **Isolation**: sbox session-level sandboxing — each session spawns in its own sbox environment
- **LLM Providers**: User wants broad support (OpenAI, GitHub Copilot, Gemini, GLM, Qwen, DeepSeek). For v1, OpenAI-compatible API covers most of these
- **Agent Architecture**: Director + Workers, user selected all 7 agent types but Metis recommended scaling down to 2 for v1 (Orchestrator + Coder), extensible to more
- **Config**: JSON format (familiar from opencode)
- **Tests**: Unit + integration tests included

**Research Findings**:
- opencode architecture: native Go/TS binary + JSON config + plugin system (oh-my-opencode) + permission system (ask/allow/deny)
- opencode eliminates permission system entirely — xcode replaces it with sbox isolation
- opencode's client-server architecture (REST + SSE) is NOT needed — xcode is CLI-direct
- sbox: rootless, user-space, env-var-based isolation; supports copy, mount, build, run, pack/unpack

### Metis Review
**Key Recommendations** (addressed):
- Scale v1 agents from 7 → 2 (Orchestrator + Coder). Design trait-based so more agents can be added in v2 without rewriting core
- v1 LLM providers: OpenAI-compatible only (covers DeepSeek, Qwen, GLM, iQuest). GitHub Copilot gateway and Gemini native API deferred to v2
- LSP integration deferred to v2 (massive complexity for a Rust learner)
- No client-server architecture — CLI-direct is dramatically simpler
- Implement hard limits: max 25 agent iterations, max 10 tool calls per response
- Provide fallback "no-sandbox" mode for development/testing without sbox installed
- Each task must produce compiling code with at least one `cargo test`

**Identified Gaps** (addressed):
- Missing error handling strategy → Use `anyhow` for application errors, `thiserror` for library errors
- Missing context window management → Simple truncation: keep system prompt + last N messages when near token limit
- Missing streaming strategy → Use `reqwest-eventsource` for SSE streaming of LLM responses
- User is Rust learner → Every task includes specific crate additions, type definitions, and detailed implementation guidance
- sbox binary path not configurable → Added to config
- No fallback for missing sbox → Added `--no-sandbox` flag for development

---

## Work Objectives

### Core Objective
Build a Rust binary (`xcode`) that takes a user message, spawns an sbox-sandboxed session, runs an autonomous LLM agent loop (Orchestrator dispatches to Coder, Coder uses tools to implement), and produces working code — all without human intervention.

### Concrete Deliverables
- `xcode` binary: `cargo build --release` → `target/release/xcode`
- `xcode run "message"` — autonomous coding execution
- `xcode session list` — list past sessions
- `xcode session show <id>` — show session history
- `~/.config/xcode/config.json` — configuration file
- SQLite database at `~/.local/share/xcode/sessions.db` — session storage
- Test suite: `cargo test` — all passing

### Definition of Done
- [ ] `cargo build --release` succeeds with 0 errors
- [ ] `cargo test` passes all tests (unit + integration)
- [ ] `cargo clippy` passes with 0 errors
- [ ] `xcode run --help` shows usage
- [ ] `xcode run "Create a file called hello.txt with 'Hello World'" --project /tmp/test` creates the file inside sbox
- [ ] Session is persisted and visible via `xcode session list`

### Must Have
- OpenAI-compatible LLM client with streaming SSE support
- Tool execution inside sbox (file ops + bash)
- Agent loop: LLM → tool calls → tool results → LLM (repeat until done)
- Session persistence (SQLite)
- JSON config file
- Graceful error handling (no panics in normal operation)
- Hard limits on iterations and tool calls (prevent infinite loops)
- `--no-sandbox` flag for development without sbox

### Must NOT Have (Guardrails)
- No permission prompts or confirmation dialogs — this is the core design principle
- No TUI (ratatui/crossterm) — CLI output only
- No plugin system — agents and tools are compiled in
- No MCP support — v2
- No Web UI — v2
- No GitHub/GitLab integration — v2
- No session sharing — v2
- No LSP integration — v2 (too complex for a Rust learner's first project)
- No ast-grep tools — v2
- No sub-agent spawning via tool calls — Orchestrator delegates sequentially in v1
- No `unsafe` code or complex lifetime annotations
- No client-server architecture — xcode is CLI-direct
- No over-abstraction — prefer concrete implementations over generic frameworks
- No excessive comments or documentation bloat

---

## Verification Strategy

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed. No exceptions.

### Test Decision
- **Infrastructure exists**: YES (created as part of project setup)
- **Automated tests**: Tests-after (build module, then add tests)
- **Framework**: `cargo test` (built-in Rust test framework)
- **Every task**: Must end with `cargo build` succeeding and relevant `cargo test` passing

### QA Policy
Every task MUST include agent-executed QA scenarios.
Evidence saved to `.sisyphus/evidence/task-{N}-{scenario-slug}.{ext}`.

- **CLI**: Use Bash — Run `cargo build`, `cargo test`, `cargo clippy`, and `xcode` commands
- **Library/Module**: Use Bash — Import/call in Rust test files, compare output
- **Integration**: Use Bash — Full end-to-end `xcode run` with mock or real LLM

### Rust Crate Stack (Mandatory)
All tasks MUST use these crates where relevant:
| Crate | Purpose | Version |
|---|---|---|
| `tokio` | Async runtime | latest, features: full |
| `reqwest` | HTTP client | latest, features: json, stream |
| `reqwest-eventsource` | SSE streaming | latest |
| `serde` + `serde_json` | JSON serialization | latest, features: derive |
| `clap` | CLI argument parsing | latest, features: derive |
| `tracing` + `tracing-subscriber` | Structured logging | latest |
| `anyhow` | Application error handling | latest |
| `thiserror` | Library error types | latest |
| `rusqlite` | SQLite session storage | latest, features: bundled |
| `globset` | Glob pattern matching | latest |
| `regex` | Regular expressions | latest |
| `uuid` | Session/message IDs | latest, features: v4 |
| `chrono` | Timestamps | latest, features: serde |
| `tempfile` | Temporary directories for tests | latest |

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Start Immediately — foundation):
├── Task 1: Rust project setup + skeleton [quick]
├── Task 2: Config module (JSON config loading) [quick]
└── Task 3: LLM client (OpenAI-compatible streaming) [deep]

Wave 2 (After Wave 1 — tools):
├── Task 4: Tool trait + file tools (read/write/edit) [unspecified-high]
├── Task 5: Bash tool + sbox integration [deep]
└── Task 6: Search tools (glob + grep) [quick]

Wave 3 (After Wave 2 — agent + persistence):
├── Task 7: Agent loop (Orchestrator + Coder) [deep]
└── Task 8: Session persistence (SQLite) [unspecified-high]

Wave 4 (After Wave 3 — integration):
├── Task 9: CLI integration (clap + full run flow) [unspecified-high]
└── Task 10: Integration tests + end-to-end QA [deep]

Wave FINAL (After ALL tasks — verification):
├── Task F1: Plan compliance audit (oracle)
├── Task F2: Code quality review (unspecified-high)
├── Task F3: Real manual QA (unspecified-high)
└── Task F4: Scope fidelity check (deep)

Critical Path: T1 → T2 → T3 → T5 → T7 → T8 → T9 → T10 → F1-F4
Parallel Speedup: ~40% faster than sequential
Max Concurrent: 3 (Waves 1 & 2)
```

### Dependency Matrix

| Task | Depends On | Blocks | Wave |
|---|---|---|---|
| T1 | — | T2, T3, T4, T5, T6 | 1 |
| T2 | T1 | T3, T5, T7, T9 | 1 |
| T3 | T1, T2 | T7 | 1 |
| T4 | T1 | T7 | 2 |
| T5 | T1, T2 | T7, T10 | 2 |
| T6 | T1 | T7 | 2 |
| T7 | T3, T4, T5, T6 | T8, T9, T10 | 3 |
| T8 | T7 | T9, T10 | 3 |
| T9 | T7, T8 | T10 | 4 |
| T10 | T9 | F1-F4 | 4 |
| F1-F4 | T10 | — | FINAL |

### Agent Dispatch Summary

- **Wave 1 (3)**: T1 → `quick`, T2 → `quick`, T3 → `deep`
- **Wave 2 (3)**: T4 → `unspecified-high`, T5 → `deep`, T6 → `quick`
- **Wave 3 (2)**: T7 → `deep`, T8 → `unspecified-high`
- **Wave 4 (2)**: T9 → `unspecified-high`, T10 → `deep`
- **FINAL (4)**: F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

> Implementation + Tests = ONE Task. Never separate.
> EVERY task MUST produce compiling code with `cargo test` passing.

---

- [x] 1. Rust Project Setup + Skeleton

  **What to do**:
  - Install Rust toolchain if not present (`rustup` check)
  - Run `cargo init --name xcode` in the project directory `/volume/pt-data/xqli/xcode`
  - Set up the module structure in `src/`:
    ```
    src/
    ├── main.rs          # Entry point, clap CLI skeleton
    ├── config.rs         # Config types (stub)
    ├── llm/
    │   └── mod.rs        # LLM client trait + types (stub)
    ├── tools/
    │   └── mod.rs        # Tool trait + registry (stub)
    ├── agent/
    │   └── mod.rs        # Agent trait + types (stub)
    ├── session/
    │   └── mod.rs        # Session types (stub)
    └── sandbox/
    │   └── mod.rs        # sbox integration (stub)
    ```
  - Add ALL required dependencies to `Cargo.toml` (see crate stack in Verification Strategy)
  - Create stub modules with `todo!()` or empty trait definitions
  - Set up `tracing-subscriber` initialization in `main.rs`
  - Add a basic `clap` CLI with `run` and `session` subcommands (arg parsing only, no logic)
  - Ensure `cargo build` and `cargo test` pass
  - Run `cargo fmt` and `cargo clippy`

  **Must NOT do**:
  - Do not implement any actual logic — stubs only
  - Do not add crates not in the approved crate stack
  - Do not create a workspace (single crate only)

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Scaffolding task, mostly boilerplate
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - None needed for scaffolding

  **Parallelization**:
  - **Can Run In Parallel**: NO (all other tasks depend on this)
  - **Parallel Group**: Wave 1 — but must complete before T2/T3
  - **Blocks**: T2, T3, T4, T5, T6
  - **Blocked By**: None (starts immediately)

  **References**:

  **Pattern References**:
  - `Cargo.toml` best practices: Use exact versions in dependencies for reproducibility
  - Module structure: Follow Rust convention of `mod.rs` for directories, `file.rs` for leaf modules

  **External References**:
  - Clap derive API: https://docs.rs/clap/latest/clap/_derive/index.html
  - Tokio runtime setup: https://docs.rs/tokio/latest/tokio/attr.main.html
  - Tracing subscriber setup: https://docs.rs/tracing-subscriber/latest/tracing_subscriber/

  **Acceptance Criteria**:
  - [ ] `cargo build` succeeds with 0 errors
  - [ ] `cargo test` passes (even if 0 tests initially)
  - [ ] `cargo clippy` has 0 errors
  - [ ] `cargo fmt --check` passes
  - [ ] All modules listed above exist as files
  - [ ] `cargo run -- --help` shows CLI with `run` and `session` subcommands

  **QA Scenarios:**

  ```
  Scenario: Project compiles and CLI shows help
    Tool: Bash
    Preconditions: Rust toolchain installed, project initialized
    Steps:
      1. Run `cargo build 2>&1`
      2. Assert exit code 0 and no "error" in output
      3. Run `cargo run -- --help 2>&1`
      4. Assert output contains "run" and "session" and "xcode"
      5. Run `cargo clippy 2>&1`
      6. Assert no "error" in output
    Expected Result: Build succeeds, CLI shows help with subcommands, clippy clean
    Failure Indicators: Compilation errors, missing subcommands in help output
    Evidence: .sisyphus/evidence/task-1-compile-and-help.txt

  Scenario: Module structure is correct
    Tool: Bash
    Preconditions: Project built successfully
    Steps:
      1. Run `find src -name '*.rs' | sort`
      2. Assert output contains: main.rs, config.rs, llm/mod.rs, tools/mod.rs, agent/mod.rs, session/mod.rs, sandbox/mod.rs
    Expected Result: All 7+ source files exist in expected locations
    Failure Indicators: Missing module files
    Evidence: .sisyphus/evidence/task-1-module-structure.txt
  ```

  **Commit**: YES
  - Message: `feat(init): scaffold xcode Rust project with module structure`
  - Files: `Cargo.toml`, `src/**/*.rs`
  - Pre-commit: `cargo build && cargo test && cargo clippy`

- [x] 2. Config Module (JSON Config Loading)

  **What to do**:
  - Define config types in `src/config.rs`:
    ```rust
    #[derive(Debug, Deserialize, Serialize)]
    pub struct Config {
        pub provider: ProviderConfig,
        pub model: String,           // e.g., "gpt-4.1"
        pub project_dir: Option<PathBuf>,
        pub sandbox: SandboxConfig,
        pub agent: AgentConfig,
    }

    #[derive(Debug, Deserialize, Serialize)]
    pub struct ProviderConfig {
        pub api_base: String,        // e.g., "https://api.openai.com/v1"
        pub api_key: String,
    }

    #[derive(Debug, Deserialize, Serialize)]
    pub struct SandboxConfig {
        pub enabled: bool,           // default: true
        pub sbox_path: Option<String>, // path to sbox binary
    }

    #[derive(Debug, Deserialize, Serialize)]
    pub struct AgentConfig {
        pub max_iterations: u32,     // default: 25
        pub max_tool_calls_per_response: u32, // default: 10
    }
    ```
  - Implement config loading:
    - Default path: `~/.config/xcode/config.json`
    - `XDG_CONFIG_HOME` support
    - CLI flag overrides (--provider-url, --api-key, --model, --project, --no-sandbox)
    - Environment variable overrides: `XCODE_API_KEY`, `XCODE_API_BASE`, `XCODE_MODEL`
    - Default values for all optional fields
  - Implement `Config::load()` that merges: defaults ← config file ← env vars ← CLI flags
  - Create a default config template and write it on first run if missing
  - Add unit tests:
    - Test default config creation
    - Test loading from JSON file
    - Test env var overrides
    - Test CLI flag overrides take precedence

  **Must NOT do**:
  - Do not implement TOML or YAML support (JSON only)
  - Do not add complex validation beyond basic type checking
  - Do not implement provider-specific config (just OpenAI-compatible for v1)

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Config loading is straightforward serde work
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T3, after T1)
  - **Parallel Group**: Wave 1 (with Tasks 1, 3)
  - **Blocks**: T3, T5, T7, T9
  - **Blocked By**: T1

  **References**:

  **Pattern References**:
  - opencode config: `/volume/pt-data/xqli/.config/opencode/opencode.json` — JSON config structure with providers, models, plugins. xcode config should be MUCH simpler.
  - XDG convention: `dirs::config_dir()` from `dirs` crate (add to Cargo.toml if needed), or manual `XDG_CONFIG_HOME` / `~/.config` fallback

  **External References**:
  - serde_json derive: https://serde.rs/derive.html
  - clap derive for CLI overrides: https://docs.rs/clap/latest/clap/_derive/index.html

  **Acceptance Criteria**:
  - [ ] `cargo test config` passes all config tests
  - [ ] Default config JSON is valid and parseable
  - [ ] Config loads from file → env → CLI in correct precedence
  - [ ] Missing config file creates default template

  **QA Scenarios:**

  ```
  Scenario: Config loads defaults when no file exists
    Tool: Bash
    Preconditions: No config file at test path
    Steps:
      1. Run `cargo test config::tests::test_default_config -- --nocapture 2>&1`
      2. Assert test passes
    Expected Result: Default config created with sensible defaults
    Failure Indicators: Test failure, panic, missing fields
    Evidence: .sisyphus/evidence/task-2-default-config.txt

  Scenario: Environment variables override config file
    Tool: Bash
    Preconditions: Config file exists with base values
    Steps:
      1. Run `cargo test config::tests::test_env_override -- --nocapture 2>&1`
      2. Assert test passes
    Expected Result: Env vars take precedence over file values
    Failure Indicators: Test failure showing wrong precedence
    Evidence: .sisyphus/evidence/task-2-env-override.txt
  ```

  **Commit**: YES
  - Message: `feat(config): add JSON config loading and validation`
  - Files: `src/config.rs`, `Cargo.toml`
  - Pre-commit: `cargo test`

- [x] 3. LLM Client (OpenAI-Compatible Streaming)

  **What to do**:
  - Create `src/llm/mod.rs` and `src/llm/openai.rs`:
  - Define the LLM provider trait:
    ```rust
    #[async_trait]
    pub trait LlmProvider: Send + Sync {
        async fn chat_completion(
            &self,
            messages: &[Message],
            tools: &[ToolDefinition],
        ) -> Result<LlmResponse>;
    }
    ```
  - Define message types:
    ```rust
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Message {
        pub role: Role,  // system, user, assistant, tool
        pub content: Option<String>,
        pub tool_calls: Option<Vec<ToolCall>>,
        pub tool_call_id: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ToolCall {
        pub id: String,
        pub function: FunctionCall,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct FunctionCall {
        pub name: String,
        pub arguments: String, // JSON string
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct ToolDefinition {
        pub name: String,
        pub description: String,
        pub parameters: serde_json::Value, // JSON Schema
    }
    ```
  - Implement `OpenAiProvider` struct:
    - POST to `{api_base}/chat/completions` with streaming enabled
    - Parse SSE stream using `reqwest-eventsource`
    - Accumulate streaming chunks into complete `LlmResponse`
    - Handle tool_calls in response (parse function name + JSON arguments)
    - Support `model`, `temperature`, `max_tokens` parameters
    - Print streaming text to stdout as it arrives (for user feedback)
    - Handle errors gracefully: network errors, rate limits (429), invalid responses
    - Implement retry with exponential backoff for transient errors (max 3 retries)
  - Add unit tests:
    - Test message serialization/deserialization
    - Test tool call parsing from mock SSE response
    - Test error handling for invalid responses

  **Must NOT do**:
  - Do not implement Anthropic-native or Gemini-native APIs (OpenAI-compatible only)
  - Do not implement conversation history management here (that's the agent's job)
  - Do not implement token counting (use simple character/message count heuristic)

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: SSE streaming + async HTTP + complex JSON parsing requires careful implementation
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T2, after T1)
  - **Parallel Group**: Wave 1 (with Tasks 1, 2)
  - **Blocks**: T7
  - **Blocked By**: T1, T2 (needs config for api_base/api_key)

  **References**:

  **Pattern References**:
  - OpenAI chat completions API: The standard `/v1/chat/completions` endpoint with `stream: true`
  - SSE format: `data: {"id":"...","choices":[{"delta":{"content":"...","tool_calls":[...]}}]}`
  - Tool call accumulation: Multiple SSE chunks may contain partial tool_calls that need to be assembled

  **External References**:
  - reqwest-eventsource: https://docs.rs/reqwest-eventsource/latest/reqwest_eventsource/
  - OpenAI API reference: https://platform.openai.com/docs/api-reference/chat/create
  - async-trait crate: https://docs.rs/async-trait/latest/async_trait/ (add to Cargo.toml)

  **WHY Each Reference Matters**:
  - reqwest-eventsource handles SSE reconnection and parsing — don't reinvent this
  - OpenAI API reference defines the exact JSON schema for requests/responses including tool_calls
  - async-trait is needed because Rust traits can't have async methods natively yet

  **Acceptance Criteria**:
  - [ ] `cargo test llm` passes all LLM client tests
  - [ ] Message and ToolCall types serialize/deserialize correctly
  - [ ] OpenAiProvider can be constructed from Config
  - [ ] SSE streaming logic handles complete + partial tool calls
  - [ ] Error types are defined for network/parse/rate-limit errors

  **QA Scenarios:**

  ```
  Scenario: LLM types serialize correctly
    Tool: Bash
    Preconditions: LLM module compiled
    Steps:
      1. Run `cargo test llm::tests -- --nocapture 2>&1`
      2. Assert all tests pass
      3. Verify test output shows JSON round-trip for Message, ToolCall, ToolDefinition
    Expected Result: All serialization tests pass
    Failure Indicators: Serde errors, missing fields in JSON output
    Evidence: .sisyphus/evidence/task-3-llm-types.txt

  Scenario: SSE chunk parsing assembles complete response
    Tool: Bash
    Preconditions: Mock SSE data defined in tests
    Steps:
      1. Run `cargo test llm::openai::tests::test_sse_parsing -- --nocapture 2>&1`
      2. Assert test passes
      3. Verify assembled response contains correct content and tool_calls
    Expected Result: Multiple SSE chunks correctly assembled into LlmResponse
    Failure Indicators: Incomplete tool calls, missing content, panic
    Evidence: .sisyphus/evidence/task-3-sse-parsing.txt
  ```

  **Commit**: YES
  - Message: `feat(llm): implement OpenAI-compatible streaming LLM client`
  - Files: `src/llm/mod.rs`, `src/llm/openai.rs`, `Cargo.toml`
  - Pre-commit: `cargo test`

- [x] 4. Tool Trait + File Tools (Read/Write/Edit)

  **What to do**:
  - Define the tool execution trait in `src/tools/mod.rs`:
    ```rust
    #[async_trait]
    pub trait Tool: Send + Sync {
        fn name(&self) -> &str;
        fn description(&self) -> &str;
        fn parameters_schema(&self) -> serde_json::Value;
        async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult>;
    }

    pub struct ToolContext {
        pub working_dir: PathBuf,  // project directory (inside sbox or direct)
        pub sandbox_enabled: bool,
    }

    pub struct ToolResult {
        pub output: String,
        pub is_error: bool,
    }

    pub struct ToolRegistry {
        tools: HashMap<String, Box<dyn Tool>>,
    }
    ```
  - Implement `ToolRegistry` with `register()`, `get()`, `list_definitions()` methods
  - Create `src/tools/file_read.rs`:
    - `file_read` tool: reads file content, returns with line numbers
    - Parameters: `{ path: string, offset?: number, limit?: number }`
    - Returns file content with line numbers (e.g., `1: line content`)
    - Handles: file not found, permission denied, binary files
  - Create `src/tools/file_write.rs`:
    - `file_write` tool: writes content to a file (creates parent dirs if needed)
    - Parameters: `{ path: string, content: string }`
    - Creates parent directories with `fs::create_dir_all`
    - Returns success message with file path
  - Create `src/tools/file_edit.rs`:
    - `file_edit` tool: replaces a range of lines in a file
    - Parameters: `{ path: string, old_string: string, new_string: string }`
    - Finds `old_string` in file content and replaces with `new_string`
    - Returns success with line range affected
    - Handles: string not found, multiple matches (error)
  - Add unit tests for each tool:
    - Test file_read with existing/missing files
    - Test file_write creates files and parent dirs
    - Test file_edit replaces correctly, errors on missing/ambiguous matches

  **Must NOT do**:
  - Do not execute file operations through sbox yet (direct fs for now, sbox integration in T5)
  - Do not implement complex diff-based editing (simple string replacement is fine)
  - Do not add tools beyond read/write/edit

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Multiple tools with error handling, trait design requires care
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T5, T6 after T1)
  - **Parallel Group**: Wave 2 (with Tasks 5, 6)
  - **Blocks**: T7
  - **Blocked By**: T1

  **References**:

  **Pattern References**:
  - Tool definition pattern: Each tool is a struct implementing the `Tool` trait. The `parameters_schema()` method returns a JSON Schema object that matches the OpenAI function calling format.
  - OpenAI function calling format: `{ "type": "function", "function": { "name": "...", "description": "...", "parameters": { "type": "object", "properties": {...} } } }`

  **External References**:
  - OpenAI function calling: https://platform.openai.com/docs/guides/function-calling
  - serde_json::Value for dynamic JSON: https://docs.rs/serde_json/latest/serde_json/enum.Value.html

  **Acceptance Criteria**:
  - [ ] `cargo test tools` passes all tool tests
  - [ ] ToolRegistry registers and retrieves tools by name
  - [ ] `list_definitions()` returns OpenAI-compatible tool definitions
  - [ ] file_read reads files with line numbers
  - [ ] file_write creates files and directories
  - [ ] file_edit replaces strings correctly

  **QA Scenarios:**

  ```
  Scenario: File tools CRUD operations
    Tool: Bash
    Preconditions: Project compiled
    Steps:
      1. Run `cargo test tools::file_write::tests -- --nocapture 2>&1`
      2. Assert test creates a file in temp directory
      3. Run `cargo test tools::file_read::tests -- --nocapture 2>&1`
      4. Assert test reads file with line numbers
      5. Run `cargo test tools::file_edit::tests -- --nocapture 2>&1`
      6. Assert test replaces string correctly
    Expected Result: All file tool tests pass, files created/read/edited correctly
    Failure Indicators: IO errors, wrong line numbers, edit not applied
    Evidence: .sisyphus/evidence/task-4-file-tools.txt

  Scenario: Tool registry returns OpenAI-compatible definitions
    Tool: Bash
    Preconditions: Tools registered
    Steps:
      1. Run `cargo test tools::tests::test_registry_definitions -- --nocapture 2>&1`
      2. Assert output contains JSON with "type": "function" and "parameters"
    Expected Result: Tool definitions match OpenAI function calling format
    Failure Indicators: Missing fields, wrong JSON structure
    Evidence: .sisyphus/evidence/task-4-tool-registry.txt
  ```

  **Commit**: YES
  - Message: `feat(tools): add file read/write/edit tools with trait abstraction`
  - Files: `src/tools/mod.rs`, `src/tools/file_read.rs`, `src/tools/file_write.rs`, `src/tools/file_edit.rs`
  - Pre-commit: `cargo test`

- [x] 5. Bash Tool + sbox Integration

  **What to do**:
  - Create `src/sandbox/mod.rs` with sbox session management:
    ```rust
    pub struct SboxSession {
        pub session_name: String,
        pub project_dir: PathBuf,
        pub sbox_path: String,
        pub is_initialized: bool,
    }

    impl SboxSession {
        pub fn new(project_dir: PathBuf, config: &SandboxConfig) -> Result<Self>;
        pub fn init(&mut self) -> Result<()>;  // sbox create + mount project
        pub async fn exec(&self, command: &str, timeout_secs: u64) -> Result<ExecResult>;
        pub fn destroy(&mut self) -> Result<()>;  // sbox cleanup
    }

    pub struct ExecResult {
        pub stdout: String,
        pub stderr: String,
        pub exit_code: i32,
        pub timed_out: bool,
    }
    ```
  - Implement sbox lifecycle:
    - `init()`: Run `sbox create <session-name>` + `sbox mount <project-dir> <mount-point>`
    - `exec()`: Run `sbox exec <session-name> -- <command>` with timeout via `tokio::time::timeout`
    - `destroy()`: Run `sbox destroy <session-name>` for cleanup
    - Handle sbox binary not found gracefully
  - Implement `NoSandbox` mode:
    - When `--no-sandbox` or `config.sandbox.enabled = false`
    - Execute commands directly via `tokio::process::Command` in the project directory
    - Same `ExecResult` interface
  - Create `src/tools/bash.rs`:
    - `bash` tool: executes a shell command
    - Parameters: `{ command: string, timeout?: number }`
    - Default timeout: 120 seconds
    - Returns stdout + stderr + exit code
    - Truncates output if > 50KB (return first 25KB + last 25KB with truncation notice)
  - Modify file tools (from T4) to use sandbox when enabled:
    - `ToolContext` gets a reference to `SboxSession` or `NoSandbox`
    - File tools use sbox exec for file operations when sandbox is enabled
    - OR: file tools operate directly on the mounted project directory (simpler — preferred approach)
  - Add tests:
    - Test SboxSession lifecycle (with `#[cfg(feature = "sbox-tests")]` guard for CI)
    - Test NoSandbox mode (always runnable)
    - Test bash tool execution with timeout
    - Test bash tool output truncation

  **Must NOT do**:
  - Do not implement complex sbox image/layer management
  - Do not implement network isolation (sbox doesn't do this)
  - Do not implement persistent sbox environments across sessions (create/destroy per session)

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: sbox integration + process management + timeout handling is complex
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T4, T6 after T1)
  - **Parallel Group**: Wave 2 (with Tasks 4, 6)
  - **Blocks**: T7, T10
  - **Blocked By**: T1, T2 (needs SandboxConfig)

  **References**:

  **Pattern References**:
  - sbox CLI: `sbox create`, `sbox mount`, `sbox exec`, `sbox destroy` — the key commands for session lifecycle
  - sbox source: `https://github.com/CVPaul/sbox` — README documents the env-var-based isolation model
  - `/volume/pt-data/xqli/sbox/` — local sbox installation for reference

  **External References**:
  - tokio::process::Command: https://docs.rs/tokio/latest/tokio/process/struct.Command.html
  - tokio::time::timeout: https://docs.rs/tokio/latest/tokio/time/fn.timeout.html

  **WHY Each Reference Matters**:
  - sbox CLI commands are the interface xcode uses to create/destroy sandboxed environments
  - tokio process is needed for async command execution with proper timeout handling
  - The sbox README explains the isolation model (HOME/PATH/TMPDIR redirection) which affects how file tools interact with sandboxed files

  **Acceptance Criteria**:
  - [ ] `cargo test sandbox` passes sandbox tests
  - [ ] `cargo test tools::bash` passes bash tool tests
  - [ ] NoSandbox mode works without sbox installed
  - [ ] Bash tool executes commands and returns stdout/stderr/exit_code
  - [ ] Bash tool respects timeout
  - [ ] Output truncation works for large outputs

  **QA Scenarios:**

  ```
  Scenario: Bash tool executes command in no-sandbox mode
    Tool: Bash
    Preconditions: Project compiled
    Steps:
      1. Run `cargo test tools::bash::tests::test_bash_execute -- --nocapture 2>&1`
      2. Assert test passes, output shows command stdout/stderr
    Expected Result: Command executes, exit code captured correctly
    Failure Indicators: Timeout, wrong exit code, missing output
    Evidence: .sisyphus/evidence/task-5-bash-nosandbox.txt

  Scenario: Bash tool handles timeout
    Tool: Bash
    Preconditions: Project compiled
    Steps:
      1. Run `cargo test tools::bash::tests::test_bash_timeout -- --nocapture 2>&1`
      2. Assert test passes, timed_out flag is true for slow command
    Expected Result: Command killed after timeout, timed_out=true in result
    Failure Indicators: Test hangs, timeout not enforced
    Evidence: .sisyphus/evidence/task-5-bash-timeout.txt

  Scenario: sbox session lifecycle (requires sbox installed)
    Tool: Bash
    Preconditions: sbox installed, `cargo build`
    Steps:
      1. Run `cargo test sandbox::tests::test_sbox_lifecycle --features sbox-tests -- --nocapture 2>&1`
      2. If sbox not installed, assert test is skipped (feature gate)
      3. If sbox installed, assert session create/exec/destroy works
    Expected Result: sbox session created, command executed inside, session destroyed
    Failure Indicators: sbox binary not found (should skip), session leak (not destroyed)
    Evidence: .sisyphus/evidence/task-5-sbox-lifecycle.txt
  ```

  **Commit**: YES
  - Message: `feat(sandbox): implement bash tool with sbox session-level isolation`
  - Files: `src/sandbox/mod.rs`, `src/tools/bash.rs`, `Cargo.toml`
  - Pre-commit: `cargo test`

- [x] 6. Search Tools (Glob + Grep)

  **What to do**:
  - Create `src/tools/glob_search.rs`:
    - `glob_search` tool: finds files matching a glob pattern
    - Parameters: `{ pattern: string, path?: string }`
    - Uses `globset` crate for pattern matching
    - Walks the directory tree and returns matching file paths (sorted by modification time)
    - Limit: max 100 results (prevent huge outputs)
    - Default path: project root
  - Create `src/tools/grep_search.rs`:
    - `grep_search` tool: searches file contents using regex
    - Parameters: `{ pattern: string, path?: string, include?: string }`
    - Uses `regex` crate for pattern matching
    - `include` parameter filters by file glob (e.g., `"*.rs"`)
    - Returns matching lines with file path, line number, and content
    - Limit: max 200 matching lines
  - Register both tools in `ToolRegistry`
  - Add tests:
    - Test glob_search finds files by pattern
    - Test glob_search respects max results limit
    - Test grep_search finds content by regex
    - Test grep_search filters by include pattern
    - Test grep_search handles invalid regex gracefully

  **Must NOT do**:
  - Do not implement ast-grep or structural search
  - Do not implement full ripgrep functionality (basic regex search is fine)
  - Do not add binary file detection (just skip files that fail UTF-8 read)

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Straightforward use of globset and regex crates
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T4, T5 after T1)
  - **Parallel Group**: Wave 2 (with Tasks 4, 5)
  - **Blocks**: T7
  - **Blocked By**: T1

  **References**:

  **Pattern References**:
  - opencode's glob/grep tools: Similar to opencode's glob and grep tools but simplified

  **External References**:
  - globset: https://docs.rs/globset/latest/globset/
  - regex: https://docs.rs/regex/latest/regex/
  - walkdir crate: https://docs.rs/walkdir/latest/walkdir/ (add to Cargo.toml for directory traversal)

  **Acceptance Criteria**:
  - [ ] `cargo test tools::glob_search` passes
  - [ ] `cargo test tools::grep_search` passes
  - [ ] Glob search finds files by pattern with max 100 limit
  - [ ] Grep search finds content by regex with line numbers
  - [ ] Invalid regex returns error, not panic

  **QA Scenarios:**

  ```
  Scenario: Glob search finds Rust source files
    Tool: Bash
    Preconditions: Project compiled with src/ directory containing .rs files
    Steps:
      1. Run `cargo test tools::glob_search::tests::test_glob_find_rs_files -- --nocapture 2>&1`
      2. Assert test passes, found files include main.rs
    Expected Result: Glob pattern `**/*.rs` finds all source files
    Failure Indicators: No files found, incorrect paths
    Evidence: .sisyphus/evidence/task-6-glob-search.txt

  Scenario: Grep search finds function definitions
    Tool: Bash
    Preconditions: Project compiled with source files
    Steps:
      1. Run `cargo test tools::grep_search::tests::test_grep_find_functions -- --nocapture 2>&1`
      2. Assert test passes, results include file path + line number + content
    Expected Result: Regex `fn \w+` finds function definitions with context
    Failure Indicators: No results, missing line numbers
    Evidence: .sisyphus/evidence/task-6-grep-search.txt
  ```

  **Commit**: YES
  - Message: `feat(tools): add glob and grep search tools`
  - Files: `src/tools/glob_search.rs`, `src/tools/grep_search.rs`, `src/tools/mod.rs`, `Cargo.toml`
  - Pre-commit: `cargo test`

- [x] 7. Agent Loop (Orchestrator + Coder)

  **What to do**:
  - Create `src/agent/mod.rs` with the core agent trait and types:
    ```rust
    #[async_trait]
    pub trait Agent: Send + Sync {
        fn name(&self) -> &str;
        fn system_prompt(&self) -> &str;
        async fn run(
            &self,
            messages: &mut Vec<Message>,
            tools: &ToolRegistry,
            llm: &dyn LlmProvider,
            ctx: &ToolContext,
        ) -> Result<AgentResult>;
    }

    pub struct AgentResult {
        pub final_message: String,
        pub iterations: u32,
        pub tool_calls_total: u32,
    }
    ```
  - Create `src/agent/coder.rs` — the Coder agent:
    - System prompt focused on coding tasks: file manipulation, bash commands, code generation
    - Prompt includes: available tools list, project context, coding best practices, instruction to be autonomous
    - The agent loop:
      1. Send messages + tools to LLM via `LlmProvider::chat_completion()`
      2. Parse response — if no tool_calls, agent is done (return final message)
      3. If tool_calls present:
         a. For each tool_call (up to `max_tool_calls_per_response`):
            - Look up tool in `ToolRegistry` by name
            - Deserialize arguments from JSON string
            - Execute tool via `tool.execute(args, ctx)`
            - Build a `Message { role: Tool, tool_call_id, content: result }` for each
         b. Append assistant message + all tool result messages to conversation
      4. Check iteration count — if >= `max_iterations`, return with warning message
      5. Loop back to step 1
    - Streaming: print LLM text content to stdout as it arrives (via the streaming in T3)
    - Handle LLM errors: retry transient errors (already in T3), abort on persistent errors
    - Handle tool errors: tool returns `ToolResult { is_error: true }` — pass error back to LLM as tool result, let LLM decide how to recover
  - Create `src/agent/orchestrator.rs` — the Orchestrator agent:
    - For v1: thin wrapper that creates a Coder and delegates the user's message to it
    - System prompt focused on task decomposition and delegation (but in v1, just passes through)
    - In v1 the Orchestrator IS the loop runner: it receives user message, creates Coder, runs Coder's loop
    - This design makes it trivial to add more worker agents in v2 (Orchestrator dispatches to multiple workers)
  - Create `src/agent/director.rs` — the Director (top-level entry point):
    ```rust
    pub struct Director {
        pub config: AgentConfig,
    }

    impl Director {
        pub async fn execute(
            &self,
            user_message: &str,
            tools: &ToolRegistry,
            llm: &dyn LlmProvider,
            ctx: &ToolContext,
        ) -> Result<AgentResult> {
            let orchestrator = Orchestrator::new(&self.config);
            let mut messages = vec![
                Message::system(orchestrator.system_prompt()),
                Message::user(user_message),
            ];
            orchestrator.run(&mut messages, tools, llm, ctx).await
        }
    }
    ```
  - Implement context window management:
    - Track approximate token count (rough: 4 chars ≈ 1 token)
    - When messages exceed ~80% of model context (default 128k tokens → ~100k threshold):
      - Keep system prompt (always)
      - Keep first user message (always)
      - Keep last N messages that fit within budget
      - Drop middle messages with a `[... N messages truncated ...]` placeholder
    - Log a tracing::warn when truncation happens
  - Add unit tests:
    - Test Coder agent loop with mock LlmProvider (returns canned responses)
    - Test tool_call parsing and execution flow
    - Test max_iterations limit is enforced
    - Test max_tool_calls_per_response limit is enforced
    - Test context truncation logic
    - Test error recovery (tool returns error, LLM gets error message)

  **Must NOT do**:
  - Do not implement sub-agent spawning (Orchestrator delegates sequentially only)
  - Do not implement parallel tool execution (sequential for v1)
  - Do not implement conversation branching or rollback
  - Do not implement sophisticated token counting (rough heuristic is fine)
  - Do not add a planning/reasoning step — Coder receives message and acts directly

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Core agent loop is the most complex piece — async iteration, tool dispatch, context management, error handling all interleave
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - None — this is pure Rust logic, no external skills needed

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T8, after Wave 2)
  - **Parallel Group**: Wave 3 (with Task 8)
  - **Blocks**: T8, T9, T10
  - **Blocked By**: T3 (LlmProvider), T4 (Tool trait + file tools), T5 (bash tool + sbox), T6 (search tools)

  **References**:

  **Pattern References** (existing code to follow):
  - `src/llm/mod.rs:LlmProvider` (from T3) — The trait this agent calls. Understand the `chat_completion()` return type (`LlmResponse`) and how `tool_calls` are structured in the response.
  - `src/tools/mod.rs:Tool` (from T4) — The tool execution interface. Understand `execute(args, ctx) -> ToolResult` and how `ToolRegistry::get(name)` works.
  - `src/tools/mod.rs:ToolRegistry::list_definitions()` (from T4) — Returns `Vec<ToolDefinition>` in OpenAI function format. Pass this to `chat_completion()` as the `tools` parameter.
  - `src/sandbox/mod.rs:SboxSession` (from T5) — The sandbox context. The `ToolContext` holds a reference to either `SboxSession` or `NoSandbox` for tool execution.
  - `src/config.rs:AgentConfig` (from T2) — Provides `max_iterations` and `max_tool_calls_per_response` limits.
  - OpenClaw agent loop: `/volume/pt-data/xqli/sbox/openclaw/src/node-host/runner.ts` — Reference for how a similar tool-calling agent loop works (LLM call → tool calls → results → repeat). Adapt the pattern to Rust.

  **API/Type References** (contracts to implement against):
  - `src/llm/mod.rs:Message` (from T3) — The message type with `role`, `content`, `tool_calls`, `tool_call_id`. Use `Message::system()`, `Message::user()`, `Message::assistant()`, `Message::tool()` constructors.
  - `src/llm/mod.rs:ToolCall` (from T3) — Contains `id` and `function: FunctionCall { name, arguments }`. The `arguments` field is a JSON string that needs `serde_json::from_str()` to parse.
  - `src/llm/mod.rs:LlmResponse` (from T3) — The response from `chat_completion()`. Contains `content: Option<String>` and `tool_calls: Option<Vec<ToolCall>>`.

  **External References**:
  - OpenAI chat completions tool calling flow: https://platform.openai.com/docs/guides/function-calling — Describes the exact back-and-forth protocol between assistant messages with tool_calls and tool result messages
  - async-trait crate: https://docs.rs/async-trait/latest/async_trait/ — Required for the `Agent` trait since Rust traits can't have async methods natively

  **WHY Each Reference Matters**:
  - The LlmProvider trait (T3) defines the exact interface the agent calls — you need to understand what `LlmResponse` looks like to parse tool_calls
  - The Tool/ToolRegistry (T4) defines how tools are discovered and executed — the agent loop's inner body is basically `registry.get(name).execute(args, ctx)`
  - The OpenClaw runner.ts shows a working implementation of the same pattern in TypeScript — useful as a mental model for the Rust implementation
  - The OpenAI function calling docs explain the message protocol (assistant sends tool_calls → you send tool results → assistant continues)

  **Acceptance Criteria**:
  - [ ] `cargo test agent` passes all agent tests
  - [ ] Mock LlmProvider test: agent calls LLM → receives tool_calls → executes tools → calls LLM again → receives final message → returns
  - [ ] Max iterations enforced: agent stops after `max_iterations` and returns warning
  - [ ] Max tool calls enforced: only first N tool_calls executed per response
  - [ ] Context truncation test: large conversation truncated correctly, system prompt preserved
  - [ ] Error recovery test: tool returns error → error passed to LLM → LLM continues

  **QA Scenarios:**

  ```
  Scenario: Agent completes a simple task with tool calls
    Tool: Bash
    Preconditions: Project compiled, mock LlmProvider implemented in test
    Steps:
      1. Run `cargo test agent::tests::test_coder_simple_task -- --nocapture 2>&1`
      2. Assert test passes
      3. Verify test output shows: LLM called → tool_call parsed → tool executed → result sent back → LLM called again → final message returned
    Expected Result: Agent completes in 2-3 iterations, final message contains task summary
    Failure Indicators: Infinite loop, tool_call parsing error, panic on tool execution
    Evidence: .sisyphus/evidence/task-7-agent-simple-task.txt

  Scenario: Agent respects max_iterations limit
    Tool: Bash
    Preconditions: Mock LlmProvider that always returns tool_calls (never finishes)
    Steps:
      1. Run `cargo test agent::tests::test_max_iterations -- --nocapture 2>&1`
      2. Assert test passes
      3. Verify iteration count equals max_iterations config value
    Expected Result: Agent stops after max_iterations, returns result with warning about hitting limit
    Failure Indicators: Agent loops forever, panic, iteration count exceeds limit
    Evidence: .sisyphus/evidence/task-7-max-iterations.txt

  Scenario: Agent handles tool execution errors gracefully
    Tool: Bash
    Preconditions: Mock tool that returns is_error=true
    Steps:
      1. Run `cargo test agent::tests::test_tool_error_recovery -- --nocapture 2>&1`
      2. Assert test passes
      3. Verify error message was sent to LLM as tool result, LLM continued
    Expected Result: Tool error sent as tool result message, LLM receives it and decides next action
    Failure Indicators: Panic on tool error, error not passed to LLM, agent aborts
    Evidence: .sisyphus/evidence/task-7-tool-error-recovery.txt

  Scenario: Context window truncation preserves system prompt
    Tool: Bash
    Preconditions: Test with >100 messages to trigger truncation
    Steps:
      1. Run `cargo test agent::tests::test_context_truncation -- --nocapture 2>&1`
      2. Assert test passes
      3. Verify first message (system) and last N messages preserved, middle truncated
    Expected Result: System prompt always present, recent messages preserved, total within budget
    Failure Indicators: System prompt dropped, truncation too aggressive or not triggered
    Evidence: .sisyphus/evidence/task-7-context-truncation.txt
  ```

  **Commit**: YES
  - Message: `feat(agent): implement Orchestrator+Coder agent loop`
  - Files: `src/agent/mod.rs`, `src/agent/coder.rs`, `src/agent/orchestrator.rs`, `src/agent/director.rs`
  - Pre-commit: `cargo test`

---


- [x] 8. Session Persistence (SQLite)

  **What to do**:
  - Create `src/session/mod.rs` with session types and SQLite storage:
    ```rust
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Session {
        pub id: String,           // UUID v4
        pub title: Option<String>, // auto-generated from first user message
        pub created_at: DateTime<Utc>,
        pub updated_at: DateTime<Utc>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct StoredMessage {
        pub id: String,           // UUID v4
        pub session_id: String,
        pub role: String,         // system, user, assistant, tool
        pub content: Option<String>,
        pub tool_calls: Option<String>,  // JSON serialized Vec<ToolCall>
        pub tool_call_id: Option<String>,
        pub created_at: DateTime<Utc>,
    }
    ```
  - Create `src/session/store.rs` with `SessionStore`:
    ```rust
    pub struct SessionStore {
        conn: Connection,  // rusqlite::Connection
    }

    impl SessionStore {
        pub fn new(db_path: &Path) -> Result<Self>;  // open/create DB, run migrations
        pub fn create_session(&self, title: Option<&str>) -> Result<Session>;
        pub fn get_session(&self, id: &str) -> Result<Option<Session>>;
        pub fn list_sessions(&self, limit: u32) -> Result<Vec<Session>>;
        pub fn add_message(&self, session_id: &str, msg: &Message) -> Result<StoredMessage>;
        pub fn get_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>>;
        pub fn update_session_title(&self, id: &str, title: &str) -> Result<()>;
        pub fn update_session_timestamp(&self, id: &str) -> Result<()>;
    }
    ```
  - Implement SQLite schema initialization (run on `SessionStore::new()`):
    ```sql
    CREATE TABLE IF NOT EXISTS sessions (
        id TEXT PRIMARY KEY,
        title TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS messages (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        role TEXT NOT NULL,
        content TEXT,
        tool_calls TEXT,
        tool_call_id TEXT,
        created_at TEXT NOT NULL,
        FOREIGN KEY (session_id) REFERENCES sessions(id)
    );
    CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
    ```
  - Database location: `~/.local/share/xcode/sessions.db`
    - Use `dirs::data_local_dir()` or manual `XDG_DATA_HOME` / `~/.local/share` fallback
    - Create parent directories if they don't exist
  - Implement `Message` ↔ `StoredMessage` conversion:
    - `Message::tool_calls` (Vec<ToolCall>) → serialize to JSON string for storage
    - `StoredMessage::tool_calls` (JSON string) → deserialize back to Vec<ToolCall> for retrieval
  - Auto-generate session title: take first 50 chars of first user message, truncate at word boundary
  - Add unit tests:
    - Test session CRUD (create, get, list)
    - Test message storage and retrieval (all roles: system, user, assistant, tool)
    - Test tool_calls serialization round-trip
    - Test session title auto-generation
    - Test list_sessions ordering (most recent first)
    - All tests use `tempfile` for temporary database paths

  **Must NOT do**:
  - Do not implement session sharing or export
  - Do not implement message editing or deletion
  - Do not implement database migrations beyond initial schema (v1 schema is final for v1)
  - Do not use an ORM — raw rusqlite SQL is preferred for simplicity and learning
  - Do not implement connection pooling (single connection is fine for CLI)

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: SQLite integration with proper schema, serialization, and test coverage needs careful implementation
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - None — pure Rust + rusqlite, no external skills needed

  **Parallelization**:
  - **Can Run In Parallel**: YES (with T7, after Wave 2)
  - **Parallel Group**: Wave 3 (with Task 7)
  - **Blocks**: T9, T10
  - **Blocked By**: T7 (needs Message types and agent result types)

  **References**:

  **Pattern References** (existing code to follow):
  - `src/llm/mod.rs:Message` (from T3) — The Message type that gets stored. Understand all fields: `role`, `content`, `tool_calls`, `tool_call_id`. The `StoredMessage` mirrors this but with `tool_calls` as a JSON string instead of `Vec<ToolCall>`.
  - `src/config.rs` (from T2) — Follow the same pattern for XDG directory resolution. Config uses `~/.config/xcode/`, sessions use `~/.local/share/xcode/`.
  - `src/agent/mod.rs:AgentResult` (from T7) — The result type from agent execution. The `final_message` and `iterations` fields can be logged to the session.

  **External References**:
  - rusqlite: https://docs.rs/rusqlite/latest/rusqlite/ — Connection, Statement, params! macro, types
  - rusqlite bundled feature: Ensures SQLite is compiled in, no system dependency needed
  - chrono serde: https://docs.rs/chrono/latest/chrono/serde/index.html — For DateTime<Utc> serialization to/from SQLite TEXT columns
  - dirs crate: https://docs.rs/dirs/latest/dirs/ — For `data_local_dir()` to find `~/.local/share`

  **WHY Each Reference Matters**:
  - The Message type (T3) is what gets persisted — you need to convert between the in-memory `Vec<ToolCall>` and stored JSON string
  - rusqlite docs show the exact API for prepared statements, parameter binding, and row mapping
  - The config module (T2) already solved XDG directory resolution — reuse the same pattern for data directories

  **Acceptance Criteria**:
  - [ ] `cargo test session` passes all session tests
  - [ ] Sessions can be created, retrieved, and listed
  - [ ] Messages with all roles (system, user, assistant, tool) can be stored and retrieved
  - [ ] tool_calls JSON round-trip preserves all data
  - [ ] Session title auto-generated from first user message
  - [ ] Database created automatically at correct XDG path
  - [ ] Tests use tempfile for isolation (no shared state)

  **QA Scenarios:**

  ```
  Scenario: Session CRUD operations work correctly
    Tool: Bash
    Preconditions: Project compiled
    Steps:
      1. Run `cargo test session::tests::test_session_crud -- --nocapture 2>&1`
      2. Assert test passes
      3. Verify output shows: session created with UUID → session retrieved by ID → session appears in list
    Expected Result: Session created, retrievable, and listed with correct timestamps
    Failure Indicators: SQLite error, UUID collision, missing session in list
    Evidence: .sisyphus/evidence/task-8-session-crud.txt

  Scenario: Messages with tool_calls round-trip through SQLite
    Tool: Bash
    Preconditions: Project compiled
    Steps:
      1. Run `cargo test session::tests::test_message_tool_calls_roundtrip -- --nocapture 2>&1`
      2. Assert test passes
      3. Verify stored and retrieved tool_calls are identical (same id, function name, arguments)
    Expected Result: Vec<ToolCall> → JSON string → Vec<ToolCall> preserves all fields
    Failure Indicators: Deserialization error, missing fields, wrong tool_call_id
    Evidence: .sisyphus/evidence/task-8-message-roundtrip.txt

  Scenario: List sessions returns most recent first
    Tool: Bash
    Preconditions: Multiple sessions created with different timestamps
    Steps:
      1. Run `cargo test session::tests::test_list_sessions_ordering -- --nocapture 2>&1`
      2. Assert test passes
      3. Verify sessions returned in descending order by updated_at
    Expected Result: Most recently updated session appears first
    Failure Indicators: Wrong ordering, missing sessions
    Evidence: .sisyphus/evidence/task-8-session-ordering.txt
  ```

  **Commit**: YES
  - Message: `feat(session): add SQLite session persistence`
  - Files: `src/session/mod.rs`, `src/session/store.rs`, `Cargo.toml`
  - Pre-commit: `cargo test`

---


- [x] 9. CLI Integration (clap + Full Run Flow)

  **What to do**:
  - Rewrite `src/main.rs` to wire everything together using clap derive:
    ```rust
    #[derive(Parser)]
    #[command(name = "xcode", version, about = "Autonomous AI coding agent")]
    struct Cli {
        #[command(subcommand)]
        command: Commands,
    }

    #[derive(Subcommand)]
    enum Commands {
        /// Run an autonomous coding task
        Run {
            /// The task message for the agent
            message: String,
            /// Project directory (default: current directory)
            #[arg(long, short)]
            project: Option<PathBuf>,
            /// Disable sandbox (use direct execution)
            #[arg(long)]
            no_sandbox: bool,
            /// Override model name
            #[arg(long)]
            model: Option<String>,
            /// Override API base URL
            #[arg(long)]
            provider_url: Option<String>,
            /// Override API key
            #[arg(long)]
            api_key: Option<String>,
        },
        /// Manage sessions
        Session {
            #[command(subcommand)]
            command: SessionCommands,
        },
    }

    #[derive(Subcommand)]
    enum SessionCommands {
        /// List recent sessions
        List {
            #[arg(long, default_value = "20")]
            limit: u32,
        },
        /// Show session details and messages
        Show {
            /// Session ID
            id: String,
        },
    }
    ```
  - Implement the `run` command flow (this is the main entry point):
    1. Load config: `Config::load()` with CLI overrides applied
    2. Validate: ensure api_key is set (from config, env, or CLI flag)
    3. Initialize tracing subscriber (info level by default, debug with RUST_LOG)
    4. Create SessionStore → create new Session
    5. Create ToolRegistry → register all 6 tools (file_read, file_write, file_edit, bash, glob_search, grep_search)
    6. Create LlmProvider (OpenAiProvider from config)
    7. Create ToolContext:
       - If sandbox enabled: create SboxSession, init it, set working_dir to mounted project
       - If --no-sandbox: set working_dir to project directory directly
    8. Create Director → call `director.execute(message, tools, llm, ctx)`
    9. Save all messages to session via SessionStore
    10. Print final summary: session ID, iterations used, tools called, final message
    11. Cleanup: destroy SboxSession if used
    12. Handle all errors gracefully — print user-friendly error messages, no panics
  - Implement the `session list` command:
    - Open SessionStore at default DB path
    - List sessions with formatted output: ID, title, date, message count
  - Implement the `session show` command:
    - Open SessionStore, get session by ID
    - Print all messages in conversation format (role: content)
    - For tool_calls, show function name + truncated arguments
  - Add integration test:
    - Test CLI argument parsing for all subcommands and flags
    - Test that `--help` output contains all expected fields
    - Test error case: missing api_key produces clear error message

  **Must NOT do**:
  - Do not implement TUI (text UI with ratatui) — CLI output only
  - Do not implement `--watch` or live-reload functionality
  - Do not implement `xcode init` (config template creation is handled in T2's Config::load)
  - Do not add color output (plain text for v1; color can be added later)
  - Do not implement piped input (stdin reading) — message must be a CLI argument

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Full integration of all modules — config, LLM, tools, agent, session, sandbox — with proper error handling and user-facing output
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - None — integration task, all components already built

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on everything)
  - **Parallel Group**: Wave 4 (sequential after Wave 3)
  - **Blocks**: T10
  - **Blocked By**: T7 (agent loop), T8 (session persistence)

  **References**:

  **Pattern References** (existing code to follow):
  - `src/config.rs:Config::load()` (from T2) — Config loading with defaults ← file ← env ← CLI. The CLI flags in the `Run` struct must override config values using the same merge pattern.
  - `src/agent/director.rs:Director` (from T7) — The entry point for agent execution. Call `director.execute()` with the user message.
  - `src/session/store.rs:SessionStore` (from T8) — Session creation and message persistence. Create session before agent runs, save messages after.
  - `src/sandbox/mod.rs:SboxSession` (from T5) — Sandbox lifecycle: `new()` → `init()` → (agent runs) → `destroy()`. Must destroy even on error (use `Drop` trait or explicit cleanup).
  - `src/tools/mod.rs:ToolRegistry` (from T4) — Register all 6 tools. Follow the pattern: `registry.register(Box::new(FileRead::new()))` for each tool.
  - `src/llm/openai.rs:OpenAiProvider` (from T3) — Construct with api_base, api_key, model from config.

  **External References**:
  - Clap derive subcommands: https://docs.rs/clap/latest/clap/_derive/index.html#subcommands
  - Rust error handling patterns with anyhow: https://docs.rs/anyhow/latest/anyhow/
  - tracing-subscriber EnvFilter: https://docs.rs/tracing-subscriber/latest/tracing_subscriber/struct.EnvFilter.html

  **WHY Each Reference Matters**:
  - This task is pure integration — every module from T1-T8 is wired together here. The references point to the exact types and methods to call.
  - Clap derive docs show how to define nested subcommands (Session { List, Show }) which is the exact pattern needed.
  - The sandbox lifecycle (create → use → destroy) MUST be reliable even on error — check SboxSession for Drop impl or add explicit cleanup with a guard.

  **Acceptance Criteria**:
  - [ ] `cargo build --release` succeeds
  - [ ] `cargo test` passes all existing + new tests
  - [ ] `xcode --help` shows all subcommands and options
  - [ ] `xcode run --help` shows message, --project, --no-sandbox, --model, --provider-url, --api-key
  - [ ] `xcode session list` prints sessions (or empty list)
  - [ ] `xcode session show <id>` prints messages for a session (or error for missing)
  - [ ] Missing api_key produces clear error: "API key not configured. Set XCODE_API_KEY or add to config."
  - [ ] Sandbox cleanup runs even when agent errors

  **QA Scenarios:**

  ```
  Scenario: CLI help shows all subcommands and options
    Tool: Bash
    Preconditions: `cargo build --release` succeeds
    Steps:
      1. Run `./target/release/xcode --help 2>&1`
      2. Assert output contains "run" and "session"
      3. Run `./target/release/xcode run --help 2>&1`
      4. Assert output contains "message", "--project", "--no-sandbox", "--model", "--provider-url", "--api-key"
      5. Run `./target/release/xcode session --help 2>&1`
      6. Assert output contains "list" and "show"
    Expected Result: All subcommands and flags documented in help output
    Failure Indicators: Missing subcommands, missing flags, clap derive error
    Evidence: .sisyphus/evidence/task-9-cli-help.txt

  Scenario: Missing API key produces clear error
    Tool: Bash
    Preconditions: No XCODE_API_KEY env var, no config file
    Steps:
      1. Run `unset XCODE_API_KEY && ./target/release/xcode run "test" --no-sandbox 2>&1`
      2. Assert exit code is non-zero
      3. Assert output contains "API key" or "api_key" (case insensitive)
    Expected Result: Clear error message about missing API key, non-zero exit code
    Failure Indicators: Panic, cryptic error, zero exit code on error
    Evidence: .sisyphus/evidence/task-9-missing-api-key.txt

  Scenario: Session list works on empty database
    Tool: Bash
    Preconditions: Fresh database (no previous sessions)
    Steps:
      1. Run `./target/release/xcode session list 2>&1`
      2. Assert exit code is 0
      3. Assert output indicates no sessions or shows empty list
    Expected Result: Clean output showing no sessions, no errors
    Failure Indicators: Database error, panic, non-zero exit code
    Evidence: .sisyphus/evidence/task-9-session-list-empty.txt
  ```

  **Commit**: YES
  - Message: `feat(cli): integrate full xcode run flow with clap CLI`
  - Files: `src/main.rs`
  - Pre-commit: `cargo test`

---


- [x] 10. Integration Tests + End-to-End QA

  **What to do**:
  - Create `tests/` directory for integration tests (Rust convention: `tests/` at crate root)
  - Create `tests/mock_llm_server.rs` — a mock HTTP server:
    - Simple HTTP server using `tokio` + `hyper` (or `axum` — add as dev dependency)
    - Listens on localhost with random available port
    - Endpoint: `POST /v1/chat/completions`
    - Response scenarios (configurable per test):
      a. **Simple text response**: Returns assistant message with text only (no tool_calls)
      b. **Tool call response**: Returns assistant message with tool_calls (e.g., file_write)
      c. **Multi-turn**: First response has tool_calls, second response is final text
      d. **Error responses**: 429 rate limit, 500 server error, invalid JSON
    - Returns SSE-formatted streaming responses (same format as OpenAI)
    - Must be reusable across multiple test functions
  - Create `tests/e2e_run.rs` — end-to-end integration tests:
    ```rust
    #[tokio::test]
    async fn test_run_simple_text_response() {
        // Start mock server → configure to return simple text
        // Run: xcode run "hello" --no-sandbox --provider-url <mock> --api-key test --project /tmp/test-dir
        // Assert: exit code 0, output contains response text
    }

    #[tokio::test]
    async fn test_run_creates_file_via_tool_call() {
        // Start mock server → configure to return file_write tool_call, then final message
        // Run: xcode run "create hello.txt" --no-sandbox --provider-url <mock> --api-key test --project /tmp/test-dir
        // Assert: exit code 0, file /tmp/test-dir/hello.txt exists with expected content
    }

    #[tokio::test]
    async fn test_run_handles_llm_error() {
        // Start mock server → configure to return 500
        // Run: xcode run "test" --no-sandbox --provider-url <mock> --api-key test
        // Assert: exit code non-zero, error message printed (not panic)
    }

    #[tokio::test]
    async fn test_session_persisted_after_run() {
        // Run a successful xcode run
        // Then: xcode session list → assert new session appears
        // Then: xcode session show <id> → assert messages present
    }
    ```
  - Create `tests/e2e_tools.rs` — tool-specific integration tests:
    - Test each tool through the full agent loop (not unit level — actual LLM→tool→LLM flow)
    - file_read, file_write, file_edit, bash, glob_search, grep_search
    - Each test uses the mock LLM server configured to call that specific tool
  - Add a `test-utils` module or helper file for shared test setup:
    - `start_mock_server(scenario)` → returns server + port
    - `run_xcode(args)` → runs the binary with given args, returns output + exit code
    - `create_temp_project()` → creates temp dir with some files for testing
  - Run the full test suite and ensure everything passes:
    - `cargo test` — all unit tests (from T1-T8) + integration tests
    - `cargo test -- --ignored` — for sbox-dependent tests (if sbox is installed)
    - `cargo clippy` — no warnings
    - `cargo fmt --check` — formatting clean

  **Must NOT do**:
  - Do not test against real LLM providers (mock server only for deterministic tests)
  - Do not add complex test frameworks beyond what Rust provides (no cucumber, proptest, etc.)
  - Do not implement benchmarks or performance tests
  - Do not test TUI (doesn't exist)
  - Do not add `axum` to non-dev dependencies (it's a dev-dependency only for the mock server)

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Mock server implementation + multi-scenario e2e tests + full flow validation is complex and must be thorough
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - None — Rust testing with mock server, no external skills needed

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on all prior tasks)
  - **Parallel Group**: Wave 4 (after T9)
  - **Blocks**: F1-F4 (Final Verification)
  - **Blocked By**: T9 (needs full binary to test), T5 (sbox tests)

  **References**:

  **Pattern References** (existing code to follow):
  - All `src/**/*.rs` modules (T1-T9) — The entire codebase is exercised by these integration tests. The mock server must mimic exactly what `OpenAiProvider` expects to receive.
  - `src/llm/openai.rs` (from T3) — The SSE parsing logic determines exactly what format the mock server must return. Study the stream parsing code to know what SSE events to emit.
  - `src/tools/mod.rs:ToolDefinition` (from T4) — The tool definitions sent to the LLM. The mock server's responses must reference tools by these exact names and parameter schemas.

  **External References**:
  - axum minimal server: https://docs.rs/axum/latest/axum/ — Use for the mock HTTP server (lightweight, async, easy to configure per-route responses)
  - tokio test runtime: https://docs.rs/tokio/latest/tokio/attr.test.html — `#[tokio::test]` for async integration tests
  - Rust integration test conventions: https://doc.rust-lang.org/book/ch11-03-test-organization.html — `tests/` directory at crate root
  - OpenAI SSE format: `data: {"id":"...","choices":[{"delta":{...}}]}\n\ndata: [DONE]\n\n` — The exact format the mock must emit

  **WHY Each Reference Matters**:
  - The mock server MUST return exactly the SSE format that `OpenAiProvider` parses. Study `src/llm/openai.rs` to know the exact `data:` line format, chunk structure, and `[DONE]` sentinel.
  - axum is ideal for the mock because it's async, lightweight, and can configure different routes/responses per test scenario
  - The integration tests validate the ENTIRE pipeline: CLI → config → LLM → tools → session — they are the ultimate proof that xcode works

  **Acceptance Criteria**:
  - [ ] `cargo test` passes ALL tests (unit from T1-T8 + integration from T10)
  - [ ] Mock LLM server correctly mimics OpenAI SSE streaming format
  - [ ] E2E test: `xcode run` with mock server creates a file via tool_call
  - [ ] E2E test: `xcode run` with mock server handles LLM error gracefully
  - [ ] E2E test: session is persisted and visible via `xcode session list/show`
  - [ ] `cargo clippy` passes with 0 warnings
  - [ ] `cargo fmt --check` passes

  **QA Scenarios:**

  ```
  Scenario: Full end-to-end file creation through agent loop
    Tool: Bash
    Preconditions: xcode binary built, mock LLM server ready
    Steps:
      1. Run `cargo test e2e_run::test_run_creates_file_via_tool_call -- --nocapture 2>&1`
      2. Assert test passes
      3. Verify test output shows: mock server started → xcode run invoked → tool_call received → file created → agent completed → session saved
    Expected Result: File exists at expected path with expected content, session persisted in SQLite
    Failure Indicators: File not created, wrong content, session not saved, test timeout
    Evidence: .sisyphus/evidence/task-10-e2e-file-creation.txt

  Scenario: All unit tests pass alongside integration tests
    Tool: Bash
    Preconditions: All tasks T1-T9 complete
    Steps:
      1. Run `cargo test 2>&1`
      2. Capture test summary line
      3. Assert 0 failures
      4. Run `cargo clippy 2>&1`
      5. Assert 0 errors
      6. Run `cargo fmt --check 2>&1`
      7. Assert no output (all formatted)
    Expected Result: All tests pass, clippy clean, formatting clean
    Failure Indicators: Any test failure, clippy warning, formatting diff
    Evidence: .sisyphus/evidence/task-10-full-test-suite.txt

  Scenario: LLM error produces graceful failure (not panic)
    Tool: Bash
    Preconditions: Mock server configured to return 500
    Steps:
      1. Run `cargo test e2e_run::test_run_handles_llm_error -- --nocapture 2>&1`
      2. Assert test passes
      3. Verify output contains error message (not stack trace or panic)
    Expected Result: Non-zero exit code, user-friendly error message
    Failure Indicators: Panic, stack trace in output, test hangs
    Evidence: .sisyphus/evidence/task-10-llm-error-handling.txt

  Scenario: Session persisted and retrievable after run
    Tool: Bash
    Preconditions: Successful xcode run completed
    Steps:
      1. Run `cargo test e2e_run::test_session_persisted_after_run -- --nocapture 2>&1`
      2. Assert test passes
      3. Verify session list shows new session, session show displays messages
    Expected Result: Session appears in list with correct title, messages show full conversation
    Failure Indicators: Empty session list, missing messages, wrong ordering
    Evidence: .sisyphus/evidence/task-10-session-persistence.txt
  ```

  **Commit**: YES
  - Message: `test(e2e): add integration tests and end-to-end validation`
  - Files: `tests/mock_llm_server.rs`, `tests/e2e_run.rs`, `tests/e2e_tools.rs`, `Cargo.toml` (dev-dependencies)
  - Pre-commit: `cargo test`

---


## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Rejection → fix → re-run.

- [x] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists (`cargo test`, run binary, read source). For each "Must NOT Have": search codebase for forbidden patterns (TUI imports, unsafe code, permission prompts) — reject with file:line if found. Check evidence files exist in `.sisyphus/evidence/`. Compare deliverables against plan.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [x] F2. **Code Quality Review** — `unspecified-high`
  Run `cargo build --release`, `cargo test`, `cargo clippy`, `cargo fmt --check`. Review all source files for: `unwrap()` in non-test code (prefer `?`), `unsafe` blocks, dead code, unused imports, overly complex lifetimes. Check for Rust anti-patterns: unnecessary clones, `String` where `&str` suffices, missing `Display` impls for error types.
  Output: `Build [PASS/FAIL] | Tests [N pass/N fail] | Clippy [PASS/FAIL] | Fmt [PASS/FAIL] | Files [N clean/N issues] | VERDICT`

- [x] F3. **Real Manual QA** — `unspecified-high`
  Start from clean state (`cargo build --release`). Execute EVERY QA scenario from EVERY task — follow exact steps, capture evidence. Test cross-task integration: full `xcode run` flow from CLI through LLM through tools to output. Test edge cases: empty project, invalid config, unreachable LLM, sbox not installed (`--no-sandbox`). Save to `.sisyphus/evidence/final-qa/`.
  Output: `Scenarios [N/N pass] | Integration [N/N] | Edge Cases [N tested] | VERDICT`

- [x] F4. **Scope Fidelity Check** — `deep`
  For each task: read "What to do", read actual source. Verify 1:1 — everything in spec was built (no missing), nothing beyond spec was built (no creep). Check "Must NOT do" compliance. Detect scope creep: any file that shouldn't exist, any dependency not in the approved crate list, any feature not in the plan. Flag unaccounted changes.
  Output: `Tasks [N/N compliant] | Creep [CLEAN/N issues] | Unaccounted [CLEAN/N files] | VERDICT`

---

## Commit Strategy

| After Task | Message | Pre-commit |
|---|---|---|
| T1 | `feat(init): scaffold xcode Rust project with module structure` | `cargo build` |
| T2 | `feat(config): add JSON config loading and validation` | `cargo test` |
| T3 | `feat(llm): implement OpenAI-compatible streaming LLM client` | `cargo test` |
| T4 | `feat(tools): add file read/write/edit tools with trait abstraction` | `cargo test` |
| T5 | `feat(sandbox): implement bash tool with sbox session-level isolation` | `cargo test` |
| T6 | `feat(tools): add glob and grep search tools` | `cargo test` |
| T7 | `feat(agent): implement Orchestrator+Coder agent loop` | `cargo test` |
| T8 | `feat(session): add SQLite session persistence` | `cargo test` |
| T9 | `feat(cli): integrate full xcode run flow with clap CLI` | `cargo test` |
| T10 | `test(e2e): add integration tests and end-to-end validation` | `cargo test` |

---

## Success Criteria

### Verification Commands
```bash
# Build succeeds
cargo build --release 2>&1 | grep -c "error"
# Expected: 0

# All tests pass
cargo test 2>&1 | grep "test result"
# Expected: test result: ok. N passed; 0 failed

# Clippy clean
cargo clippy 2>&1 | grep -c "error"
# Expected: 0

# Formatting clean
cargo fmt --check
# Expected: no output (all formatted)

# CLI works
./target/release/xcode --help
# Expected: Shows CLI with "run", "session" subcommands

# Version
./target/release/xcode --version
# Expected: xcode 0.1.0

# Config created on first run
./target/release/xcode run --help
# Expected: Shows run subcommand options

# End-to-end (with --no-sandbox for CI)
./target/release/xcode run --no-sandbox --provider-url http://localhost:8080/v1 --api-key test "Create hello.txt" --project /tmp/xcode-test
# Expected: Executes agent loop, creates file, prints session summary
```

### Final Checklist
- [ ] All "Must Have" present
- [ ] All "Must NOT Have" absent
- [ ] All tests pass
- [ ] `cargo clippy` clean
- [ ] `cargo fmt --check` clean
- [ ] Binary runs and shows help
- [ ] Config loading works
- [ ] LLM client connects and streams
- [ ] Tools execute inside sbox (or without when `--no-sandbox`)
- [ ] Agent loop completes a simple coding task
- [ ] Sessions persist across runs
