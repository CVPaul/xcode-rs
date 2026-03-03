//! xcodeai library crate.
//!
//! This file exists so that integration tests in `tests/` can access internal
//! types via `xcodeai::http::AppState`, `xcodeai::config::Config`, etc.
//!
//! A Rust binary crate only exposes its modules to integration tests if a
//! `lib.rs` exists.  The binary entry point (`main.rs`) still works normally;
//! `lib.rs` just provides the public API surface for the test harness.
//!
//! All modules declared here are the same modules declared in `main.rs` via
//! `mod` statements.  Rust compiles them once and shares the code between the
//! binary and the library crate.
pub mod agent;
pub mod auth;
pub mod config;
pub mod context;
pub mod http;
pub mod io;
pub mod llm;
pub mod lsp;
pub mod mcp;
pub mod orchestrator;
pub mod repl;
pub mod sandbox;
pub mod session;
pub mod tools;
pub mod tracking;
pub mod ui;
pub mod spinner;
