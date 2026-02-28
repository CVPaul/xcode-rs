# xcode-v1 Learnings

## Project Context
- Rust binary at /volume/pt-data/xqli/xcode
- Git initialized on main branch (2026-02-28)
- Single crate (not workspace)
- Target: autonomous AI coding agent, no human intervention
- sbox for sandboxing (rootless, env-var based)

## Crate Stack (Mandatory)
tokio (full), reqwest (json+stream), reqwest-eventsource, serde/serde_json (derive),
clap (derive), tracing+tracing-subscriber, anyhow, thiserror, rusqlite (bundled),
globset, regex, uuid (v4), chrono (serde), tempfile, async-trait, walkdir, dirs

## Architecture Decisions
- Director → Orchestrator → Coder (2 agents v1)
- No permission system — sbox IS the security
- OpenAI-compatible LLM only for v1
- Hard limits: max 25 iterations, max 10 tool calls per response
- --no-sandbox flag for dev/testing
- Context truncation: keep system prompt + first user msg + last N msgs

## Module Structure
src/main.rs, src/config.rs, src/llm/mod.rs, src/llm/openai.rs,
src/tools/mod.rs, src/tools/file_read.rs, src/tools/file_write.rs,
src/tools/file_edit.rs, src/tools/bash.rs, src/tools/glob_search.rs,
src/tools/grep_search.rs, src/agent/mod.rs, src/agent/coder.rs,
src/agent/orchestrator.rs, src/agent/director.rs,
src/session/mod.rs, src/session/store.rs, src/sandbox/mod.rs

## Task 1: Project Scaffold - COMPLETED ✓

### Key Implementation Details
1. Used `cargo init` in existing directory (not `cargo new`)
2. Edition set to 2021 (not 2024 which doesn't exist)
3. All 23 main dependencies + 1 dev-dependency added as specified
4. Module structure: 6 stub modules + 1 config module with sample structs
5. Main.rs uses clap derive macros with Parser/Subcommand enums
6. CLI hierarchy: xcode → run/session, session → list/show
7. Eliminated unused code warnings with #[allow(dead_code)]
8. Code formatted with rustfmt before commit

### Build Verification
- cargo build: ✓ (17.98s first, 0.67s rebuild)
- cargo test: ✓ (0 tests, all pass)
- cargo clippy: ✓ (0 errors/warnings)
- cargo fmt --check: ✓ (code compliant)
- cargo run -- --help: ✓ (CLI shows run and session commands)

### Rust Environment Setup
- Installed rustup via curl (stable-x86_64-unknown-linux-gnu)
- Rust version: 1.93.1 (2026-02-11)
- Cargo version: 1.93.1
- Build times reasonable for first compile with full tokio/reqwest stack

### Notes for Task 2
- All module stubs use comments like "// Implementation in Task N"
- Config module has sample structs (Config, ProviderConfig) for reference
- Main.rs implementation uses anyhow::Result for error handling
- Tokio runtime initialized with #[tokio::main] macro
- tracing_subscriber initialized at startup for logging

## Task 2: Config Module - COMPLETED ✓

### Implementation Summary
Replaced stub `src/config.rs` with full config loading implementation supporting:
- JSON config file persistence
- Environment variable overrides (XCODE_API_KEY, XCODE_API_BASE, XCODE_MODEL)
- CLI parameter precedence (highest priority)
- Automatic default config creation in XDG-compliant location (~/.config/xcode/config.json)

### Key Structs Implemented
1. **Config**: Main struct with provider, model, project_dir, sandbox, agent fields
2. **ProviderConfig**: API base URL and key
3. **SandboxConfig**: Sandbox enablement and path
4. **AgentConfig**: Agent limits (max_iterations=25, max_tool_calls_per_response=10)
5. **ConfigOverrides**: CLI override container struct

### Config Loading Precedence
1. Start with Config::default()
2. Load from JSON file (creates if missing)
3. Apply environment variable overrides
4. Apply CLI parameter overrides (highest priority)

### Test Implementation (5 tests, all passing)
- **test_default_config**: Verifies defaults (25 iterations, 10 calls/response, sandbox enabled)
- **test_load_from_file**: Tests JSON file parsing and value propagation
- **test_env_override**: Tests XCODE_* env var precedence
- **test_cli_override_takes_precedence**: Verifies CLI wins over env vars
- **test_sandbox_disable_override**: Tests no_sandbox flag functionality

### Test Isolation Strategy
Used `static TEST_LOCK: Mutex<()>` to serialize environment variable access across tests.
Each test acquires lock before modifying environment, preventing race conditions.
All env vars cleared before and after each test for proper isolation.

### Error Handling
- anyhow::Context for rich error messages in file I/O
- Graceful directory creation if config dir doesn't exist
- JSON parse errors clearly reported with context
- Missing API keys allowed (empty string in defaults, will fail at runtime if needed)

### Type Traits
All main types derive:
- Debug, Clone (required for agent context in future T7)
- Deserialize, Serialize (for JSON persistence)

### Notes for Future Tasks
- Config::load() wrapper exists for convenience (marked #[allow(dead_code)])
- ConfigOverrides is simple struct (not serializable by design)
- Default config is immediately persisted to disk if file missing
- Tests use tempfile::TempDir for isolation (no real ~/.config writes)
- Config suitable for passing to agent in Task 7 with Clone trait
