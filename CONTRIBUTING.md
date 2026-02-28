# Contributing to xcodeai

Thanks for your interest in contributing. This document covers how to set up a development environment, run the tests, and submit changes.

---

## Development Setup

You need Rust 1.75 or later. Install via [rustup](https://rustup.rs):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Clone the repo and build:

```bash
git clone <repo>
cd xcode
cargo build
```

Run the tests to confirm everything works:

```bash
cargo test
```

Expected: 62 tests pass, 0 failures.

---

## Running Tests

```bash
# All tests
cargo test

# A specific module
cargo test config
cargo test tools::bash
cargo test agent

# Integration tests with output
cargo test e2e_run -- --nocapture

# With debug logging
RUST_LOG=debug cargo test
```

---

## Code Style

```bash
# Format (required before committing)
cargo fmt

# Lint (no warnings allowed)
cargo clippy
```

All pull requests must pass `cargo fmt --check` and `cargo clippy` with zero errors.

---

## Adding a New Tool

All tools implement the `Tool` trait in `src/tools/mod.rs`:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;  // JSON Schema
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolResult>;
}
```

Steps to add a new tool:

1. Create `src/tools/your_tool.rs` implementing the `Tool` trait
2. Export it in `src/tools/mod.rs`: `pub mod your_tool;`
3. Register it in `src/main.rs` inside `run_command()`:
   ```rust
   registry.register(Box::new(YourTool));
   ```
4. Write unit tests in the same file under `#[cfg(test)]`
5. Run `cargo test tools::your_tool`

The `parameters_schema()` method must return a valid [JSON Schema](https://json-schema.org/) object. The LLM uses this schema to know what arguments to pass.

---

## Adding a New LLM Provider

For providers with non-standard APIs (not OpenAI-compatible), implement the `LlmProvider` trait in `src/llm/mod.rs`:

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

Most providers (DeepSeek, Qwen, GLM) are already OpenAI-compatible and work without changes — just set the `api_base` in config.

---

## Submitting Changes

1. Fork the repository and create a branch
2. Make your changes with tests
3. Ensure `cargo test`, `cargo clippy`, and `cargo fmt --check` all pass
4. Open a pull request with a clear description of what changed and why

For significant changes (new agent types, new sandboxing backends, protocol changes), open an issue first to discuss the approach.

---

## Reporting Bugs

Open a GitHub issue with:
- xcodeai version (`xcodeai --version`)
- OS and Rust version (`rustc --version`)
- The command you ran
- Full output including any error messages
- `RUST_LOG=debug xcodeai ...` output if relevant
