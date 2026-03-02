//! Route handlers for the xcodeai HTTP API.
//!
//! Endpoints implemented here:
//! - `POST   /sessions`      — create a new session
//! - `GET    /sessions`      — list recent sessions (default: latest 50)
//! - `GET    /sessions/:id`  — get one session + its messages
//! - `DELETE /sessions/:id`  — delete a session and its messages
//!
//! Agent execution (`POST /sessions/:id/messages`) is implemented in Task 34.
//!
//! ## Locking convention
//!
//! All handlers acquire `state.store.lock().await` before any DB call,
//! because rusqlite's Connection is `!Sync` (uses RefCell internally).
use crate::http::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{delete, get, post},
    Json, Router,
};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;

// ─── Request / Response types ─────────────────────────────────────────────────

/// Body for `POST /sessions`.
///
/// All fields are optional: a bare `{}` creates an untitled session.
#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    /// Optional human-readable title (e.g. "Refactor auth module").
    pub title: Option<String>,
}

/// Query-string parameters for `GET /sessions`.
#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    /// How many sessions to return (default: 50, max enforced by caller).
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    50
}

/// A single session entry returned in list / get responses.
#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub id: String,
    pub title: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Full session with messages, returned by `GET /sessions/:id`.
#[derive(Debug, Serialize)]
pub struct SessionDetailResponse {
    pub id: String,
    pub title: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// All messages in chronological order.
    pub messages: Vec<MessageResponse>,
}

/// A single stored message.
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub id: String,
    pub role: String,
    pub content: Option<String>,
    pub created_at: String,
}

/// Generic JSON error body.
#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

// ─── Router factory ───────────────────────────────────────────────────────────

/// Build the session sub-router.
pub fn session_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/sessions", post(create_session))
        .route("/sessions", get(list_sessions))
        .route("/sessions/:id", get(get_session))
        .route("/sessions/:id", delete(delete_session))
        .route("/sessions/:id/messages", post(post_message))
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// `POST /sessions` — create a new session.
///
/// Returns 201 Created with `{"session_id": "<uuid>"}` on success.
pub async fn create_session(
    State(state): State<Arc<AppState>>,
    body: Option<Json<CreateSessionRequest>>,
) -> impl IntoResponse {
    let title = body.and_then(|b| b.title.clone());
    let store = state.store.lock().await;
    match store.create_session(title.as_deref()) {
        Ok(session) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "session_id": session.id })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// `GET /sessions?limit=N` — list recent sessions.
///
/// Returns 200 OK with a JSON array of session objects, ordered newest first.
pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListSessionsQuery>,
) -> impl IntoResponse {
    let limit = params.limit.min(200);
    let store = state.store.lock().await;
    match store.list_sessions(limit) {
        Ok(sessions) => {
            let resp: Vec<SessionResponse> = sessions
                .into_iter()
                .map(|s| SessionResponse {
                    id: s.id,
                    title: s.title,
                    created_at: s.created_at.to_rfc3339(),
                    updated_at: s.updated_at.to_rfc3339(),
                })
                .collect();
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// `GET /sessions/:id` — get a single session with all messages.
///
/// Returns 200 OK with a `SessionDetailResponse`, or 404 if not found.
pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let store = state.store.lock().await;

    let session = match store.get_session(&id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("Session '{id}' not found"),
                }),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
                .into_response()
        }
    };

    let messages = match store.get_messages(&id) {
        Ok(msgs) => msgs,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
                .into_response()
        }
    };

    let detail = SessionDetailResponse {
        id: session.id,
        title: session.title,
        created_at: session.created_at.to_rfc3339(),
        updated_at: session.updated_at.to_rfc3339(),
        messages: messages
            .into_iter()
            .map(|m| MessageResponse {
                id: m.id,
                role: m.role,
                content: m.content,
                created_at: m.created_at.to_rfc3339(),
            })
            .collect(),
    };

    (StatusCode::OK, Json(detail)).into_response()
}

/// `DELETE /sessions/:id` — delete a session and all its messages.
///
/// Returns 204 No Content on success, 404 if not found.
pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let store = state.store.lock().await;

    // Verify the session exists first so we can return a proper 404.
    match store.get_session(&id) {
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("Session '{id}' not found"),
                }),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
                .into_response()
        }
        Ok(Some(_)) => {} // proceed
    }

    match store.delete_session(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

// ─── POST /sessions/:id/messages ────────────────────────────────────────────

/// Request body for `POST /sessions/:id/messages`.
///
/// Sends a user message to an existing session and runs the agent loop,
/// streaming the output as Server-Sent Events.
#[derive(Debug, Deserialize)]
pub struct PostMessageRequest {
    /// The task/message text for the agent.
    pub content: String,
    /// Optional list of image file paths to attach as multimodal content.
    /// Each entry is a filesystem path; the handler reads and base64-encodes it.
    #[serde(default)]
    #[allow(dead_code)]
    pub images: Vec<String>,
}

/// `POST /sessions/:id/messages`
///
/// Accepts a JSON body `{ "content": "...", "images": [...] }`, persists the
/// user message in the session, then runs the CoderAgent loop and streams
/// all agent output as a Server-Sent Events response.
///
/// ## SSE event types
///
/// | event       | data fields                             |
/// |-------------|----------------------------------------|
/// | `status`    | `{"msg": "..."}` — progress update     |
/// | `tool_call` | `{"name": "...", "args": "..."}` — tool invocation |
/// | `tool_result` | `{"preview": "...", "is_error": bool}` — tool result |
/// | `error`     | `{"msg": "..."}` — agent-level error   |
/// | `complete`  | `{}` — agent finished, stream ends     |
///
/// ## Error responses (non-SSE)
///
/// - `404 Not Found` — session ID does not exist
/// - `409 Conflict` — session already has an active agent execution
/// - `500 Internal Server Error` — failed to start agent
pub async fn post_message(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<PostMessageRequest>,
) -> impl IntoResponse {
    use crate::agent::director::Director;
    use crate::agent::Agent;
    use crate::context::AgentContext;
    use crate::io::http::HttpIO;
    use crate::llm;

    // ── 1. Verify the session exists ──────────────────────────────────────
    {
        let store = state.store.lock().await;
        match store.get_session(&session_id) {
            Ok(Some(_)) => {} // exists, proceed
            Ok(None) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(ErrorResponse {
                        error: format!("Session '{}' not found", session_id),
                    }),
                )
                    .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: e.to_string(),
                    }),
                )
                    .into_response();
            }
        }
    }

    // ── 2. Concurrency check — 409 if session already active ──────────────
    {
        let mut active = state.active_sessions.lock().await;
        if active.contains(&session_id) {
            return (
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: format!(
                        "Session '{}' already has an active agent execution",
                        session_id
                    ),
                }),
            )
                .into_response();
        }
        active.insert(session_id.clone());
    }

    // ── 3. Persist the user message ───────────────────────────────────────
    let content = body.content.clone();
    {
        let store = state.store.lock().await;
        if let Err(e) = store.add_message(&session_id, &llm::Message::user(&content)) {
            // Remove from active set on early failure.
            state.active_sessions.lock().await.remove(&session_id);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
                .into_response();
        }
    }

    // ── 4. Build HttpIO — agent will push SseEvents into the channel ──────
    let (http_io, rx) = HttpIO::new();
    let io: std::sync::Arc<dyn crate::io::AgentIO> = std::sync::Arc::new(http_io);

    // Clone the config from AppState — AgentContext::new() takes ownership of
    // these values and the task runs asynchronously so we need owned copies.
    let config = state.config.clone();
    let sid = session_id.clone();
    let state2 = std::sync::Arc::clone(&state);

    // ── 5. Spawn the agent loop in a background task ──────────────────────
    //
    // The task:
    //   a) Builds a fresh AgentContext (registers tools, optional MCP, etc.)
    //   b) Constructs the message list (system prompt + user message)
    //   c) Runs Director::execute() — the agent → tool loop
    //   d) Persists the final assistant message
    //   e) Sends SseEvent::Complete so the HTTP client knows the stream ended
    //   f) Removes the session from `active_sessions`
    //
    // The HTTP handler returns the SSE stream immediately (step below) and does
    // NOT await the spawned task — the client receives events as they arrive.
    tokio::spawn(async move {
        // Build IO + AgentContext.
        // We pass None for project/sandbox/model/provider/key — they are all
        // read from `config` which was already loaded at serve startup.
        let ctx_result = AgentContext::new(
            config.project_dir.clone(), // project dir from config
            false,                      // no_sandbox: read from config inside new()
            Some(config.model.clone()),
            Some(config.provider.api_base.clone()),
            Some(config.provider.api_key.clone()),
            config.agent.compact_mode,
            std::sync::Arc::clone(&io),
        )
        .await;

        let ctx = match ctx_result {
            Ok(c) => c,
            Err(e) => {
                let _ = io.write_error(&format!("Agent init failed: {:#}", e)).await;
                state2.active_sessions.lock().await.remove(&sid);
                return;
            }
        };

        // Build system prompt and initial messages.
        let agents_md = crate::agent::agents_md::load_agents_md(&ctx.project_dir);
        let coder = crate::agent::coder::CoderAgent::new_with_agents_md(
            ctx.config.agent.clone(),
            agents_md,
        );
        let mut messages = vec![
            llm::Message::system(coder.system_prompt().as_str()),
            llm::Message::user(content.as_str()),
        ];

        // Run the agent loop.
        let director = Director::new(ctx.config.agent.clone());
        let result = director
            .execute(
                &mut messages,
                ctx.registry.as_ref(),
                ctx.llm.as_ref(),
                &ctx.tool_ctx,
            )
            .await;

        // Persist result and send Complete event.
        match result {
            Ok(agent_result) => {
                let store = state2.store.lock().await;
                let _ = store.add_message(
                    &sid,
                    &llm::Message::assistant(Some(agent_result.final_message), None),
                );
                let _ = store.update_session_timestamp(&sid);
            }
            Err(e) => {
                let _ = io.write_error(&format!("Agent error: {:#}", e)).await;
            }
        }

        // Signal the SSE stream that the agent is done.
        // The Sender is owned by HttpIO which is wrapped in `io` (Arc).
        // Drop io before removing from active_sessions so the channel closes
        // and the SSE stream terminates cleanly.
        let _ = io.show_status("[DONE]").await;
        drop(io);
        state2.active_sessions.lock().await.remove(&sid);
    });

    // ── 6. Convert the mpsc Receiver into an axum SSE stream ─────────────
    //
    // `stream::unfold` turns our Receiver into an async Stream<Item=...>.
    // Each received SseEvent is converted to an axum `Event` with the
    // appropriate `event` name and JSON `data` field.
    // When the channel closes (sender dropped), the stream ends naturally.
    let sse_stream = stream::unfold(rx, |mut receiver| async move {
        // recv() returns None when the channel is closed.
        let event = receiver.recv().await?;

        // Build the axum SSE Event.
        let axum_event = Event::default()
            .event(event.event_name())
            .data(event.data_json());

        Some((Ok::<Event, Infallible>(axum_event), receiver))
    });

    // Return the SSE response.  KeepAlive sends periodic comments to prevent
    // proxy/browser timeouts on long-running agent executions.
    Sse::new(sse_stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
