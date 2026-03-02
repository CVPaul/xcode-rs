//! HTTP API server module for xcodeai.
//!
//! This module provides a REST API so that xcodeai can be controlled
//! programmatically — e.g. from 企业微信 (WeChat Work) bots, web UIs, or
//! any HTTP client.
//!
//! Architecture:
//! - `start_server(config, addr)` — builds the axum Router and binds to `addr`
//! - `routes` sub-module — individual handler functions for each endpoint
//! - `AppState` — shared state passed to every handler (session store, etc.)
//!
//! The server intentionally has NO authentication — xcodeai is a single-user
//! local tool.  CORS is enabled so a local web front-end can call the API from
//! a browser without proxy configuration.
//!
//! ## Thread-safety note
//!
//! rusqlite's `Connection` uses `RefCell` internally and is therefore `!Sync`.
//! We wrap `SessionStore` inside `tokio::sync::Mutex` so it can be shared
//! across tokio tasks safely.  Axum's state type must be `Send + Sync + 'static`,
//! so the wrapping is:
//!
//!   `Arc<AppState>` where `AppState { store: Mutex<SessionStore>, ... }`
//!
//! Every handler acquires `.store.lock().await` before touching the DB.
pub mod routes;

use crate::config::Config;
use crate::session::store::SessionStore;
use anyhow::Result;
use axum::Router;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;

/// Shared state cloned into every request handler via axum `State<Arc<AppState>>`.
///
/// We keep it behind `Arc` so multiple requests can hold references
/// simultaneously without copying.
pub struct AppState {
    /// Session storage (SQLite-backed).
    ///
    /// Wrapped in `Mutex` because rusqlite's `Connection` is `!Sync`.
    /// Handlers must call `.store.lock().await` before DB operations.
    pub store: Mutex<SessionStore>,
    /// The loaded xcodeai configuration (model, provider URL, etc.).
    pub config: Config,
    /// Set of session IDs that are currently executing an agent loop.
    ///
    /// Used by `POST /sessions/:id/messages` to prevent two concurrent agent
    /// executions on the same session.  The handler inserts the session ID before
    /// spawning the agent and removes it when the agent finishes (or on error).
    /// Returns HTTP 409 Conflict if the session is already active.
    pub active_sessions: Mutex<HashSet<String>>,
}

impl AppState {
    /// Create a new `AppState` using the same database path that the CLI uses.
    pub fn new(config: Config) -> Result<Self> {
        // Derive the DB path the same way AgentContext does so that `xcodeai
        // serve` shares the same session database as the REPL and `run`
        // subcommands.
        let db_path = crate::session::SessionStore::default_path()?;
        let store = SessionStore::new(&db_path)?;
        Ok(Self {
            store: Mutex::new(store),
            config,
            active_sessions: Mutex::new(HashSet::new()),
        })
    }
}

/// Start the HTTP API server and block until it exits.
///
/// # Arguments
/// * `config` — loaded xcodeai config
/// * `addr`   — socket address to listen on (e.g. `0.0.0.0:8080`)
pub async fn start_server(config: Config, addr: SocketAddr) -> Result<()> {
    // Build shared application state.
    let state = Arc::new(AppState::new(config)?);

    // Build the router.
    //   - CORS: wide-open (Access-Control-Allow-Origin: *) so a local browser
    //     UI doesn't need a proxy.  This is fine for a single-user local tool.
    //   - All routes are defined in the `routes` sub-module.
    let app = Router::new()
        .merge(routes::session_router())
        .layer(CorsLayer::permissive())
        .with_state(state);

    // Bind and serve.
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("xcodeai HTTP server listening on {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for `.oneshot()`

    /// Build an in-memory `AppState` pointing at a temp SQLite database.
    /// This lets tests exercise the full handler stack without a real TCP socket.
    pub fn test_state() -> Arc<AppState> {
        let config = Config::default();
        // Use an in-memory DB so tests don't collide with the user's real sessions.
        let store = SessionStore::new(std::path::Path::new(":memory:")).unwrap();
        Arc::new(AppState {
            store: Mutex::new(store),
            config,
            active_sessions: Mutex::new(HashSet::new()),
        })
    }

    /// Helper that builds the full axum `Router` wired to `state`.
    pub fn test_app(state: Arc<AppState>) -> Router {
        Router::new()
            .merge(routes::session_router())
            .layer(CorsLayer::permissive())
            .with_state(state)
    }

    // ── GET /sessions ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_sessions_empty() {
        let app = test_app(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    // ── POST /sessions ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_create_session_returns_id() {
        let app = test_app(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"title":"my task"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // Response must contain a non-empty "session_id" string.
        assert!(json["session_id"].is_string());
        assert!(!json["session_id"].as_str().unwrap().is_empty());
    }

    // ── GET /sessions/:id ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_session_not_found() {
        let app = test_app(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/sessions/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_create_then_get_session() {
        let state = test_state();
        let app = test_app(Arc::clone(&state));

        // Create a session.
        let create_resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"title":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);
        let create_body = axum::body::to_bytes(create_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let create_json: serde_json::Value = serde_json::from_slice(&create_body).unwrap();
        let session_id = create_json["session_id"].as_str().unwrap().to_string();

        // Fetch it back — must use a fresh router instance on the SAME state.
        let app2 = test_app(Arc::clone(&state));
        let get_resp = app2
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let get_body = axum::body::to_bytes(get_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let get_json: serde_json::Value = serde_json::from_slice(&get_body).unwrap();
        assert_eq!(get_json["id"].as_str().unwrap(), session_id);
    }

    // ── DELETE /sessions/:id ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_delete_session() {
        let state = test_state();
        let app = test_app(Arc::clone(&state));

        // Create a session first.
        let create_resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let create_body = axum::body::to_bytes(create_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let session_id = serde_json::from_slice::<serde_json::Value>(&create_body).unwrap()
            ["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Delete it.
        let app2 = test_app(Arc::clone(&state));
        let del_resp = app2
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);

        // Should be gone now.
        let app3 = test_app(Arc::clone(&state));
        let get_resp = app3
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/sessions/{session_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::NOT_FOUND);
    }

    // ── CORS header ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_cors_header_present() {
        let app = test_app(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/sessions")
                    // Simulate a browser cross-origin request.
                    .header("origin", "http://localhost:3000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        // tower-http's CorsLayer::permissive() adds this header.
        assert!(resp.headers().contains_key("access-control-allow-origin"));
    }
    // ── POST /sessions/:id/messages — Task 34 ────────────────────────────────

    /// POST /sessions/:id/messages returns 404 when the session does not exist.
    /// The agent loop should never be started for a non-existent session.
    #[tokio::test]
    async fn test_post_message_session_not_found() {
        let app = test_app(test_state());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/nonexistent-session-id/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"content":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// POST /sessions/:id/messages returns 409 Conflict when the same session
    /// already has an active agent execution running.
    ///
    /// We simulate "already active" by manually inserting the session ID into
    /// `active_sessions` before sending the request.
    #[tokio::test]
    async fn test_post_message_conflict_when_active() {
        let state = test_state();

        // 1. Create a real session so the 404 check passes.
        let app = test_app(Arc::clone(&state));
        let create_resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);
        let create_body = axum::body::to_bytes(create_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let session_id = serde_json::from_slice::<serde_json::Value>(&create_body).unwrap()
            ["session_id"]
            .as_str()
            .unwrap()
            .to_string();

        // 2. Mark the session as already active — simulating a running agent.
        state
            .active_sessions
            .lock()
            .await
            .insert(session_id.clone());

        // 3. A second POST to the same session must return 409.
        let app2 = test_app(Arc::clone(&state));
        let resp = app2
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{session_id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"content":"duplicate task"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }
}
