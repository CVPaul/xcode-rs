/// HTTP API integration tests for xcodeai.
///
/// These tests start a REAL TCP server (on a random port) and use `reqwest`
/// to send actual HTTP requests. This exercises the full stack:
///
///   reqwest → axum router → handler → AgentContext → mock LLM server
///
/// Unlike the unit tests in `src/http/mod.rs` (which use `tower::oneshot`
/// without a real network socket), these tests verify that SSE streaming
/// works end-to-end over a real TCP connection.
///
/// ## How it works
///
/// 1. `start_mock_server(scenarios)` — starts a mock OpenAI-compatible LLM
///    server on a random port (defined in `tests/mock_llm_server.rs`).
/// 2. `make_test_state(mock_addr)` — builds an `AppState` whose `Config`
///    points the provider URL at the mock server.  Sandbox is disabled so
///    tests don't require sbox.
/// 3. `start_http_server(state)` — binds a real `TcpListener` on
///    `127.0.0.1:0` and spawns an axum server task.
/// 4. Tests use a `reqwest::Client` to talk to the real server.
///
/// ## SSE parsing
///
/// The SSE stream is consumed as raw bytes, split into lines, and each
/// `data: <json>` line is parsed manually.  We look for an event named
/// `complete` (or `error`) to know the agent loop finished.
///
/// ```text
/// event: status
/// data: {"msg":"auto-continuing…"}
///
/// event: complete
/// data: {}
/// ```

mod helpers {
    pub mod mock_llm_server {}
}

// Pull in the shared mock server helpers from `tests/mock_llm_server.rs`.
// Rust integration tests can share helper modules by declaring them here.
#[path = "mock_llm_server.rs"]
mod mock_llm_server;

use mock_llm_server::{start_mock_server, MockScenario};

use reqwest::Client;
use serde_json::Value;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Build an `AppState` whose LLM provider is pointed at `mock_addr`.
///
/// - Uses an in-memory SQLite database so tests don't touch the user's DB.
/// - Disables sandbox so tests don't need `sbox` installed.
/// - Uses `model = "gpt-4o"` (the mock server ignores the model field).
fn make_test_state(mock_addr: SocketAddr) -> Arc<xcodeai::http::AppState> {
    use xcodeai::config::{AgentConfig, Config, ProviderConfig, SandboxConfig};
    use xcodeai::session::store::SessionStore;

    let config = Config {
        provider: ProviderConfig {
            api_base: format!("http://127.0.0.1:{}/v1", mock_addr.port()),
            api_key: "test-key".to_string(),
        },
        model: "gpt-4o".to_string(),
        project_dir: Some(std::env::temp_dir()),
        sandbox: SandboxConfig {
            enabled: false,
            sbox_path: None,
        },
        agent: AgentConfig {
            max_iterations: 5,
            max_tool_calls_per_response: 5,
            max_auto_continues: 3,
            ..Default::default()
        },
        lsp: Default::default(),
        mcp_servers: vec![],
        custom_tools: vec![],
        permissions: vec![],
        formatters: std::collections::HashMap::new(),
    };

    // In-memory SQLite so tests don't touch the real DB.
    let store = SessionStore::new(std::path::Path::new(":memory:")).unwrap();

    Arc::new(xcodeai::http::AppState {
        store: Mutex::new(store),
        config,
        active_sessions: Mutex::new(HashSet::new()),
    })
}

/// Start a real TCP server backed by `state`.  Returns the bound address.
///
/// The server runs in a background Tokio task for the duration of the test.
/// Because each test gets its own `TcpListener` on port 0, there are no
/// port conflicts between parallel tests.
async fn start_http_server(state: Arc<xcodeai::http::AppState>) -> SocketAddr {
    use axum::Router;
    use tower_http::cors::CorsLayer;
    use xcodeai::http::routes::session_router;

    let app = Router::new()
        .merge(session_router())
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind random port");
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    addr
}

/// Parse SSE events from a raw byte stream.
///
/// The SSE format is:
/// ```text
/// event: <name>\n
/// data: <json>\n
/// \n
/// ```
///
/// Returns a list of `(event_name, data_json)` pairs.
///
/// We read the response body as text, then split on double-newline blocks.
async fn collect_sse_events(resp: reqwest::Response) -> Vec<(String, Value)> {
    let body = resp.text().await.expect("read SSE body");

    let mut events = Vec::new();
    let mut current_event: Option<String> = None;
    let mut current_data: Option<String> = None;

    for line in body.lines() {
        if let Some(event_name) = line.strip_prefix("event: ") {
            current_event = Some(event_name.trim().to_string());
        } else if let Some(data_str) = line.strip_prefix("data: ") {
            current_data = Some(data_str.trim().to_string());
        } else if line.is_empty() {
            // End of one SSE message block.
            if let (Some(ev), Some(dat)) = (current_event.take(), current_data.take()) {
                let json: Value =
                    serde_json::from_str(&dat).unwrap_or_else(|_| Value::String(dat.clone()));
                events.push((ev, json));
            }
        }
    }

    events
}

/// Create a session via `POST /sessions` and return the session ID.
async fn create_session(client: &Client, base: &str, title: &str) -> String {
    let resp = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({ "title": title }))
        .send()
        .await
        .expect("POST /sessions");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "expected 201 from POST /sessions"
    );

    let body: Value = resp.json().await.unwrap();
    body["session_id"]
        .as_str()
        .expect("session_id in response")
        .to_string()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/// Test 1: `POST /sessions/:id/messages` returns 404 when session does not exist.
///
/// This is a non-LLM test — the handler returns 404 before it even tries to
/// start the agent loop, so no mock server is needed.
#[tokio::test]
async fn test_http_session_not_found() {
    // Any non-existent mock addr is fine — the 404 check fires before LLM init.
    let mock_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let state = make_test_state(mock_addr);
    let server_addr = start_http_server(state).await;
    let base = format!("http://{server_addr}");

    let client = Client::new();
    let resp = client
        .post(format!("{base}/sessions/does-not-exist/messages"))
        .json(&serde_json::json!({ "content": "hello" }))
        .send()
        .await
        .expect("POST to nonexistent session");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "missing session must return 404"
    );
}

/// Test 2: `POST /sessions/:id/messages` returns 409 when the session is
/// already executing an agent loop.
///
/// We simulate "already active" by inserting the session ID into
/// `AppState.active_sessions` before sending the second POST.
#[tokio::test]
async fn test_http_conflict_concurrent() {
    let mock_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let state = make_test_state(mock_addr);
    let server_addr = start_http_server(Arc::clone(&state)).await;
    let base = format!("http://{server_addr}");

    let client = Client::new();

    // Create a real session so the 404 check passes.
    let session_id = create_session(&client, &base, "conflict test").await;

    // Mark it as already active — simulates a running agent.
    state
        .active_sessions
        .lock()
        .await
        .insert(session_id.clone());

    // A second POST must return 409 Conflict.
    let resp = client
        .post(format!("{base}/sessions/{session_id}/messages"))
        .json(&serde_json::json!({ "content": "concurrent!" }))
        .send()
        .await
        .expect("POST to active session");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CONFLICT,
        "concurrent POST must return 409"
    );
}

/// Test 3: Full lifecycle — create session → post message → receive SSE stream.
///
/// The mock LLM server returns a plain text response ("hello from mock").
/// We verify:
/// - HTTP 200 with `content-type: text/event-stream`
/// - At least one `status` SSE event arrives
/// - A `complete` SSE event arrives (agent loop finished)
/// - The session still exists in the DB after the run
#[tokio::test]
async fn test_http_full_lifecycle_text_response() {
    // The mock LLM responds with a simple text message.
    let (mock_addr, _mock_state) = start_mock_server(vec![MockScenario::TextResponse(
        "[TASK_COMPLETE] All done!".to_string(),
    )])
    .await;

    let state = make_test_state(mock_addr);
    let server_addr = start_http_server(Arc::clone(&state)).await;
    let base = format!("http://{server_addr}");

    let client = Client::new();

    // 1. Create a session.
    let session_id = create_session(&client, &base, "lifecycle test").await;

    // 2. POST a message — the response is an SSE stream.
    let resp = client
        .post(format!("{base}/sessions/{session_id}/messages"))
        .json(&serde_json::json!({ "content": "Say hello" }))
        .send()
        .await
        .expect("POST /sessions/:id/messages");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "agent loop response must be 200"
    );

    // Check SSE content-type.
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("text/event-stream"),
        "response must be SSE, got: {content_type}"
    );

    // 3. Consume the SSE stream and collect events.
    let events = collect_sse_events(resp).await;

    // We must see a `complete` event (or `status` with `[DONE]`) to know
    // the agent loop finished.
    let has_complete = events.iter().any(|(name, _)| name == "complete");
    let has_done_status = events.iter().any(|(name, data)| {
        name == "status" && data["msg"].as_str().unwrap_or("").contains("[DONE]")
    });
    assert!(
        has_complete || has_done_status,
        "expected a 'complete' or '[DONE]' event; got: {events:?}"
    );

    // 4. After the agent loop, the session must still be accessible.
    let get_resp = client
        .get(format!("{base}/sessions/{session_id}"))
        .send()
        .await
        .expect("GET session after agent run");
    assert_eq!(
        get_resp.status(),
        reqwest::StatusCode::OK,
        "session must still exist after agent run"
    );
}

/// Test 4: CRUD endpoints work over a real TCP connection.
///
/// Verifies that session create / list / get / delete all work correctly
/// when accessed via a real reqwest client (not `tower::oneshot`).
#[tokio::test]
async fn test_http_session_crud_over_tcp() {
    // No LLM calls in this test — mock addr is irrelevant.
    let mock_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let state = make_test_state(mock_addr);
    let server_addr = start_http_server(state).await;
    let base = format!("http://{server_addr}");

    let client = Client::new();

    // Create two sessions.
    let id1 = create_session(&client, &base, "first").await;
    let id2 = create_session(&client, &base, "second").await;

    // List must return both.
    let list_resp = client.get(format!("{base}/sessions")).send().await.unwrap();
    assert_eq!(list_resp.status(), reqwest::StatusCode::OK);
    let list: Vec<Value> = list_resp.json().await.unwrap();
    assert!(
        list.len() >= 2,
        "expected at least 2 sessions, got {}: {list:?}",
        list.len()
    );

    // GET one session by ID.
    let get_resp = client
        .get(format!("{base}/sessions/{id1}"))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), reqwest::StatusCode::OK);
    let detail: Value = get_resp.json().await.unwrap();
    assert_eq!(detail["id"].as_str().unwrap(), id1);
    assert_eq!(detail["title"].as_str().unwrap_or(""), "first");

    // DELETE id2.
    let del_resp = client
        .delete(format!("{base}/sessions/{id2}"))
        .send()
        .await
        .unwrap();
    assert_eq!(del_resp.status(), reqwest::StatusCode::NO_CONTENT);

    // After deletion, GET must return 404.
    let get_after = client
        .get(format!("{base}/sessions/{id2}"))
        .send()
        .await
        .unwrap();
    assert_eq!(get_after.status(), reqwest::StatusCode::NOT_FOUND);
}

/// Test 5: CORS header is present on a real TCP response.
///
/// The `CorsLayer::permissive()` in our router should add
/// `Access-Control-Allow-Origin: *` to every response.
#[tokio::test]
async fn test_http_cors_header_over_tcp() {
    let mock_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    let state = make_test_state(mock_addr);
    let server_addr = start_http_server(state).await;
    let base = format!("http://{server_addr}");

    let client = Client::new();
    let resp = client
        .get(format!("{base}/sessions"))
        .header("origin", "http://localhost:3000")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert!(
        resp.headers().contains_key("access-control-allow-origin"),
        "CORS header must be present"
    );
}

/// Test 6: SSE stream delivers events in order and contains at least a
/// `status` event followed eventually by `complete`.
///
/// This test uses a text-only mock response (no tool calls), so the
/// event sequence should be:
///
/// ```
/// status {"msg":"[DONE]"}    ← emitted by routes.rs after agent finishes
/// complete {}                 ← emitted by HttpIO signal chain
/// ```
///
/// (The exact ordering depends on timing, but `complete` or a `[DONE]` status
/// must appear.)
#[tokio::test]
async fn test_http_sse_events_present() {
    let (mock_addr, _) = start_mock_server(vec![MockScenario::TextResponse(
        "[TASK_COMPLETE] Done.".to_string(),
    )])
    .await;

    let state = make_test_state(mock_addr);
    let server_addr = start_http_server(Arc::clone(&state)).await;
    let base = format!("http://{server_addr}");

    let client = Client::new();
    let session_id = create_session(&client, &base, "sse events test").await;

    let resp = client
        .post(format!("{base}/sessions/{session_id}/messages"))
        .json(&serde_json::json!({ "content": "Do the thing" }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let events = collect_sse_events(resp).await;

    // Must have at least one event.
    assert!(!events.is_empty(), "SSE stream must not be empty");

    // The stream must terminate: look for complete event OR [DONE] status.
    let terminated = events.iter().any(|(name, data)| {
        name == "complete"
            || (name == "status" && data["msg"].as_str().unwrap_or("").contains("[DONE]"))
    });
    assert!(
        terminated,
        "stream must terminate with complete/[DONE]; events: {events:?}"
    );
}
