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
