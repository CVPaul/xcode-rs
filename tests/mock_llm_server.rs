/// Mock LLM server for integration tests.
///
/// Simulates an OpenAI-compatible streaming SSE endpoint. Tests configure
/// the server with a queue of `MockScenario` values; each POST to
/// `/v1/chat/completions` pops the front scenario and streams the response.
use axum::{
    body::Body,
    extract::State,
    http::HeaderMap,
    response::Response,
    routing::post,
    Router,
};
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::net::TcpListener;

/// A pre-configured response the mock server will return.
#[derive(Clone)]
pub enum MockScenario {
    /// Return a plain assistant text message (no tool calls).
    TextResponse(String),
    /// Return a `file_write` tool call followed by a final text message.
    /// `args` must be a JSON string, e.g. `{"path":"hello.txt","content":"Hello"}`.
    ToolCallResponse {
        name: String,
        args: String,
        final_text: String,
    },
    /// Return an HTTP error with the given status code.
    ErrorResponse(u16),
}

/// Shared state for the mock server.
pub struct MockState {
    pub scenarios: Mutex<VecDeque<MockScenario>>,
}

impl MockState {
    pub fn new(scenarios: Vec<MockScenario>) -> Arc<Self> {
        Arc::new(Self {
            scenarios: Mutex::new(scenarios.into_iter().collect()),
        })
    }
}

/// Build a single SSE `data:` line for a text delta chunk.
fn sse_text_chunk(text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}},\"finish_reason\":null}}]}}\n\n",
        serde_json::to_string(text).unwrap()
    )
}

/// Build an SSE `data:` line that opens a tool call (index 0).
fn sse_tool_call_open(call_id: &str, name: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":{id},\"type\":\"function\",\"function\":{{\"name\":{name},\"arguments\":\"\"}}}}]}},\"finish_reason\":null}}]}}\n\n",
        id = serde_json::to_string(call_id).unwrap(),
        name = serde_json::to_string(name).unwrap(),
    )
}

/// Build an SSE `data:` line that appends argument characters to tool call 0.
fn sse_tool_call_args(args: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":{}}}}}]}},\"finish_reason\":null}}]}}\n\n",
        serde_json::to_string(args).unwrap()
    )
}

/// `data: [DONE]` sentinel.
const SSE_DONE: &str = "data: [DONE]\n\n";

/// Handler for `POST /v1/chat/completions`.
async fn chat_completions(
    State(state): State<Arc<MockState>>,
    _headers: HeaderMap,
    _body: axum::body::Bytes,
) -> Response {
    let scenario = {
        let mut q = state.scenarios.lock().unwrap();
        q.pop_front()
    };

    match scenario {
        None => {
            // No scenario queued — return empty success text.
            let body = format!("{}{}", sse_text_chunk("(no scenario)"), SSE_DONE);
            Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .body(Body::from(body))
                .unwrap()
        }
        Some(MockScenario::TextResponse(text)) => {
            let body = format!("{}{}", sse_text_chunk(&text), SSE_DONE);
            Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .body(Body::from(body))
                .unwrap()
        }
        Some(MockScenario::ToolCallResponse {
            name,
            args,
            final_text,
        }) => {
            // First call: emit a tool call.
            // The agent will execute the tool, then call the LLM again.
            // On the second call the scenario queue is empty → returns "(no scenario)".
            // We handle the second call by pre-loading a TextResponse in the queue.
            // But to keep things self-contained, this handler also returns a text
            // response if the queue is empty (see above). So we push the final text
            // back onto the queue for the second request.
            {
                let mut q = state.scenarios.lock().unwrap();
                q.push_front(MockScenario::TextResponse(final_text));
            }
            let body = format!(
                "{}{}{}",
                sse_tool_call_open("call_test_001", &name),
                sse_tool_call_args(&args),
                SSE_DONE
            );
            Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .body(Body::from(body))
                .unwrap()
        }
        Some(MockScenario::ErrorResponse(code)) => Response::builder()
            .status(code)
            .body(Body::empty())
            .unwrap(),
    }
}

/// Start the mock server on a random port. Returns `(address, Arc<MockState>)`.
///
/// The caller must keep the `MockState` alive for the duration of the test.
/// The server runs in a background Tokio task and shuts down when the
/// `TcpListener`-backed handle is dropped — axum servers run until the runtime
/// exits, so tests should simply let the task run.
pub async fn start_mock_server(scenarios: Vec<MockScenario>) -> (SocketAddr, Arc<MockState>) {
    let state = MockState::new(scenarios);
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(state.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (addr, state)
}
