mod helpers;
/// End-to-end integration tests for the `xcode` binary.
///
/// Each test:
///   1. Starts a mock OpenAI-compatible SSE server (see `mock_llm_server`).
///   2. Invokes the `xcode` binary with `--no-sandbox` and `--provider-url`
///      pointing at the mock server.
///   3. Asserts the expected outcome (exit code, files created, session recorded).
mod mock_llm_server;

use helpers::{assert_file_contains, output_to_string, run_xcode_with_env};
use mock_llm_server::{start_mock_server, MockScenario};
use tempfile::TempDir;

/// Helper: common provider args for a mock server at `addr`.
#[allow(dead_code)] // useful test helper, not all tests use it
fn provider_args(addr: std::net::SocketAddr) -> [String; 4] {
    [
        "--no-sandbox".to_string(),
        "--provider-url".to_string(),
        format!("http://127.0.0.1:{}/v1", addr.port()),
        "--api-key".to_string(),
    ]
}

// ─── test_run_simple_text_response ─────────────────────────────────────────

/// `xcode run` with a mock that returns plain text should exit 0.
#[tokio::test]
async fn test_run_simple_text_response() {
    let (addr, _state) = start_mock_server(vec![MockScenario::TextResponse(
        "Task completed successfully.".to_string(),
    )])
    .await;

    let project_dir = TempDir::new().unwrap();
    let home_dir = TempDir::new().unwrap();

    let port = addr.port().to_string();
    let project_path = project_dir.path().to_str().unwrap().to_string();
    let home_path = home_dir.path().to_str().unwrap().to_string();

    let output = run_xcode_with_env(
        &[
            "run",
            "--no-sandbox",
            "--provider-url",
            &format!("http://127.0.0.1:{}/v1", port),
            "--api-key",
            "testkey",
            "--project",
            &project_path,
            "hello",
        ],
        &[("HOME", &home_path)],
    )
    .await;

    let text = output_to_string(&output);
    assert!(
        output.status.success(),
        "xcode run failed (exit {:?}).\nOutput:\n{}",
        output.status.code(),
        text
    );
}

// ─── test_run_creates_file_via_tool_call ────────────────────────────────────

/// `xcode run` with a mock that returns a `file_write` tool call should
/// create the specified file in the project directory.
#[tokio::test]
async fn test_run_creates_file_via_tool_call() {
    let (addr, _state) = start_mock_server(vec![MockScenario::ToolCallResponse {
        name: "file_write".to_string(),
        args: r#"{"path":"hello.txt","content":"Hello World"}"#.to_string(),
        final_text: "Done".to_string(),
    }])
    .await;

    let project_dir = TempDir::new().unwrap();
    let home_dir = TempDir::new().unwrap();

    let port = addr.port().to_string();
    let project_path = project_dir.path().to_str().unwrap().to_string();
    let home_path = home_dir.path().to_str().unwrap().to_string();

    let output = run_xcode_with_env(
        &[
            "run",
            "--no-sandbox",
            "--provider-url",
            &format!("http://127.0.0.1:{}/v1", port),
            "--api-key",
            "testkey",
            "--project",
            &project_path,
            "create hello.txt",
        ],
        &[("HOME", &home_path)],
    )
    .await;

    let text = output_to_string(&output);
    assert!(
        output.status.success(),
        "xcode run failed (exit {:?}).\nOutput:\n{}",
        output.status.code(),
        text
    );

    let expected_file = project_dir.path().join("hello.txt");
    assert!(
        expected_file.exists(),
        "Expected file {:?} was not created.\nxcode output:\n{}",
        expected_file,
        text
    );
    assert_file_contains(&expected_file, "Hello World");
}

// ─── test_run_handles_llm_error ─────────────────────────────────────────────

/// When the LLM returns HTTP 500 the `xcode` binary should exit non-zero
/// with a user-friendly error message (not a panic/stack trace).
#[tokio::test]
async fn test_run_handles_llm_error() {
    // One 500 error is enough — we set XCODEAI_RETRY_MAX=0 so retries are
    // disabled and the error propagates immediately.
    let (addr, _state) = start_mock_server(vec![MockScenario::ErrorResponse(500)]).await;

    let project_dir = TempDir::new().unwrap();
    let home_dir = TempDir::new().unwrap();

    let port = addr.port().to_string();
    let project_path = project_dir.path().to_str().unwrap().to_string();
    let home_path = home_dir.path().to_str().unwrap().to_string();

    let output = run_xcode_with_env(
        &[
            "run",
            "--no-sandbox",
            "--provider-url",
            &format!("http://127.0.0.1:{}/v1", port),
            "--api-key",
            "testkey",
            "--project",
            &project_path,
            "test error handling",
        ],
        // Disable retries so the 500 error propagates immediately (no sleeps).
        &[("HOME", &home_path), ("XCODEAI_RETRY_MAX", "0")],
    )
    .await;

    assert!(
        !output.status.success(),
        "Expected non-zero exit code on LLM error, but got success.\nOutput:\n{}",
        output_to_string(&output)
    );

    // Must NOT be a Rust panic.
    let combined = output_to_string(&output);
    assert!(
        !combined.contains("thread 'main' panicked"),
        "Binary panicked instead of returning a graceful error.\nOutput:\n{}",
        combined
    );
}

// ─── test_session_persisted_after_run ───────────────────────────────────────

/// After a successful `xcode run`, `xcode session list` should show the
/// newly created session (not "No sessions found").
#[tokio::test]
async fn test_session_persisted_after_run() {
    let (addr, _state) = start_mock_server(vec![MockScenario::TextResponse(
        "Persisted successfully.".to_string(),
    )])
    .await;

    // Isolate the SQLite database by redirecting HOME.
    let home_dir = TempDir::new().unwrap();
    let project_dir = TempDir::new().unwrap();

    let port = addr.port().to_string();
    let project_path = project_dir.path().to_str().unwrap().to_string();
    let home_path = home_dir.path().to_str().unwrap().to_string();

    // Run xcode — this should create a session in the DB.
    let run_output = run_xcode_with_env(
        &[
            "run",
            "--no-sandbox",
            "--provider-url",
            &format!("http://127.0.0.1:{}/v1", port),
            "--api-key",
            "testkey",
            "--project",
            &project_path,
            "persist this session",
        ],
        &[("HOME", &home_path)],
    )
    .await;

    assert!(
        run_output.status.success(),
        "xcode run failed.\nOutput:\n{}",
        output_to_string(&run_output)
    );

    // Now list sessions — should NOT be empty.
    let list_output = run_xcode_with_env(&["session", "list"], &[("HOME", &home_path)]).await;

    assert!(
        list_output.status.success(),
        "xcode session list failed.\nOutput:\n{}",
        output_to_string(&list_output)
    );

    let list_text = output_to_string(&list_output);
    assert!(
        !list_text.contains("No sessions found"),
        "Expected at least one session after run, but got:\n{}",
        list_text
    );
}
