use super::*;
use crate::io::{AgentIO, NullIO};
use crate::llm::retry::{retry_with_backoff, RetryConfig, RetryableError};
use anyhow::{bail, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use reqwest_eventsource::{Event, RequestBuilderExt};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Mutex;

/// Sentinel value for `api_base` that signals GitHub Copilot mode.
pub const COPILOT_API_BASE: &str = "copilot";
const COPILOT_CHAT_URL: &str = "https://api.githubcopilot.com/chat/completions";

// ─── Provider ────────────────────────────────────────────────────────────────

/// Shared Copilot token state managed across async calls.
#[derive(Default)]
struct CopilotState {
    /// The long-lived GitHub OAuth token (persisted to disk).
    oauth_token: Option<String>,
    /// Short-lived Copilot API token, refreshed automatically.
    api_token: Option<crate::auth::CopilotApiToken>,
}

pub struct OpenAiProvider {
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    client: Client,
    /// Only populated when `api_base == COPILOT_API_BASE`.
    copilot: Arc<Mutex<CopilotState>>,
    /// Set to true when Copilot mode is activated (either at init or via set_copilot_oauth_token).
    copilot_mode: AtomicBool,
    /// When true (the default), stream tokens are printed to stdout in real time.
    /// Set to false in Plan mode so xcodeai can post-process the reply before displaying.
    pub stream_print: AtomicBool,
    /// I/O handle for reporting retry status messages to the user.
    /// Defaults to `NullIO` (silent) — call `with_io()` to inject a real terminal I/O.
    io: Arc<dyn AgentIO>,
    /// Retry / back-off configuration for this provider.
    /// Defaults to `RetryConfig::default()` — call `with_retry()` to override.
    retry: RetryConfig,
}

impl OpenAiProvider {
    /// Create a standard (non-Copilot) provider.
    ///
    /// The provider is created with `NullIO` (silent retries) and the default
    /// `RetryConfig`. Use the builder methods `with_io()` and `with_retry()` to
    /// configure them before use.
    pub fn new(api_base: String, api_key: String, model: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("Failed to create HTTP client");
        let is_copilot = api_base == COPILOT_API_BASE;
        Self {
            api_base,
            api_key,
            model,
            client,
            copilot: Arc::new(Mutex::new(CopilotState::default())),
            copilot_mode: AtomicBool::new(is_copilot),
            stream_print: AtomicBool::new(true),
            io: Arc::new(NullIO),
            retry: RetryConfig::default(),
        }
    }

    /// Create a Copilot-mode provider from a persisted OAuth token.
    pub fn new_copilot(oauth_token: String, model: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("Failed to create HTTP client");
        let copilot = Arc::new(Mutex::new(CopilotState {
            oauth_token: Some(oauth_token),
            api_token: None,
        }));
        Self {
            api_base: COPILOT_API_BASE.to_string(),
            api_key: String::new(),
            model,
            client,
            copilot,
            copilot_mode: AtomicBool::new(true),
            stream_print: AtomicBool::new(true),
            io: Arc::new(NullIO),
            retry: RetryConfig::default(),
        }
    }

    /// Builder method: inject an I/O handle for retry status reporting.
    ///
    /// # Example
    /// ```rust,ignore
    /// let provider = OpenAiProvider::new(base, key, model)
    ///     .with_io(io.clone())
    ///     .with_retry(config.agent.retry.clone());
    /// ```
    pub fn with_io(mut self, io: Arc<dyn AgentIO>) -> Self {
        self.io = io;
        self
    }

    /// Builder method: override the retry configuration.
    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    /// Exchange the OAuth token for a short-lived Copilot API bearer token.
    ///
    /// Checks the cached `CopilotState.api_token` first; if it is expired or
    /// missing, calls `auth::refresh_copilot_token` to get a fresh one, caches
    /// it, and returns the bearer string.
    async fn copilot_bearer_token(&self) -> anyhow::Result<String> {
        let mut state = self.copilot.lock().await;
        // Refresh if we have no token or it's about to expire.
        let needs_refresh = state
            .api_token
            .as_ref()
            .map(|t| t.is_expired())
            .unwrap_or(true);
        if needs_refresh {
            let oauth = state
                .oauth_token
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("No Copilot OAuth token — run /login first"))?;
            let fresh = crate::auth::refresh_copilot_token(&self.client, oauth).await?;
            state.api_token = Some(fresh);
        }
        // Safe to unwrap: we just ensured api_token is Some above.
        Ok(state.api_token.as_ref().unwrap().token.clone())
    }

    /// Execute a single SSE streaming request and return the assembled response.
    ///
    /// This is the "one attempt" that `retry_with_backoff` will call repeatedly.
    /// On transient HTTP errors (429, 500, 502, 503, 504), it returns a
    /// `RetryableError` wrapped in `anyhow::Error` so the retry logic can
    /// recognise it and schedule another attempt.
    ///
    /// On permanent errors (400, 422, parse errors) it returns a plain error
    /// that propagates immediately without retrying.
    async fn try_once(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        // ── Build the request body ────────────────────────────────────────────
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            // Ask the API to include token usage in the final SSE chunk.
            // OpenAI sends a trailing chunk with `choices: []` and a `usage` field.
            // Not all providers support this — we silently ignore missing usage.
            "stream_options": { "include_usage": true },
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools)?;
            body["tool_choice"] = json!("auto");
        }

        // ── Build request — Copilot needs different URL + auth headers ────────
        let request = if self.is_copilot() {
            let bearer = self.copilot_bearer_token().await?;
            self.client
                .post(COPILOT_CHAT_URL)
                .header("Authorization", format!("Bearer {}", bearer))
                .header("Copilot-Integration-Id", "vscode-chat")
                .header("Content-Type", "application/json")
                .header("User-Agent", "GithubCopilot/1.155.0")
                .json(&body)
        } else {
            self.client
                .post(format!("{}/chat/completions", self.api_base))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&body)
        };

        // ── Open the SSE stream ───────────────────────────────────────────────
        let mut es = match request.eventsource() {
            Ok(es) => es,
            Err(e) => bail!("Failed to start eventsource: {}", e),
        };

        let mut content = String::new();
        let mut tool_calls: HashMap<usize, ToolCallBuilder> = HashMap::new();
        // Accumulates usage data from the final SSE chunk (sent after [DONE] by OpenAI
        // when stream_options.include_usage is true).
        let mut usage_acc: Option<StreamChunkUsage> = None;

        // ── Consume the SSE event stream ──────────────────────────────────────
        while let Some(event) = es.next().await {
            match event {
                Ok(Event::Open) => {}
                Ok(Event::Message(msg)) => {
                    if msg.data == "[DONE]" {
                        es.close();
                        break;
                    }
                    let chunk: StreamChunk = match serde_json::from_str(&msg.data) {
                        Ok(c) => c,
                        Err(e) => {
                            // Parse errors are permanent — no point retrying the same message.
                            return Err(LlmError::ParseError(format!("{}: {}", msg.data, e)).into());
                        }
                    };
                    for choice in chunk.choices {
                        let delta = choice.delta;
                        if let Some(text) = delta.content {
                            // Only stream to stdout if stream_print is enabled.
                            // Plan mode disables this so replies can be post-processed
                            // (e.g. parsed for CHOICES: blocks) before display.
                            if self.stream_print.load(Ordering::Relaxed) {
                                print!("{}", text);
                                std::io::stdout().flush().ok();
                            }
                            content.push_str(&text);
                        }
                        if let Some(partials) = delta.tool_calls {
                            for partial in partials {
                                let entry = tool_calls.entry(partial.index).or_default();
                                if let Some(id) = partial.id {
                                    entry.id = Some(id);
                                }
                                if let Some(call_type) = partial.call_type {
                                    entry.call_type = Some(call_type);
                                }
                                if let Some(function) = partial.function {
                                    if let Some(name) = function.name {
                                        entry.name.push_str(&name);
                                    }
                                    if let Some(args) = function.arguments {
                                        entry.arguments.push_str(&args);
                                    }
                                }
                            }
                        }
                    }
                    // Capture usage if the API sent it (final chunk from OpenAI).
                    // OpenAI sends a trailing chunk with `choices: []` and `usage: {...}`.
                    if let Some(u) = chunk.usage {
                        usage_acc = Some(u);
                    }
                }
                Err(e) => {
                    use reqwest_eventsource::Error as EsError;
                    match e {
                        // HTTP errors with a status code: translate to RetryableError
                        // so the retry wrapper can decide whether to retry.
                        EsError::InvalidStatusCode(status, ref resp) => {
                            let code = status.as_u16();
                            // Try to extract the Retry-After header value (in seconds).
                            // reqwest_eventsource gives us a `Response` reference here.
                            let retry_after_secs: Option<u64> = resp
                                .headers()
                                .get("retry-after")
                                .and_then(|v| v.to_str().ok())
                                .and_then(|s| s.trim().parse::<u64>().ok());

                            // Return a RetryableError for transient codes.
                            // For other codes, return a plain LlmError — no retry.
                            return Err(match code {
                                429 | 500 | 502 | 503 | 504 => {
                                    anyhow::Error::new(RetryableError::Http {
                                        status: code,
                                        retry_after: retry_after_secs,
                                    })
                                }
                                _ => LlmError::HttpError {
                                    status: code,
                                    body: status.to_string(),
                                }
                                .into(),
                            });
                        }
                        // reqwest timeout error
                        EsError::Transport(ref transport_err) if transport_err.is_timeout() => {
                            return Err(anyhow::Error::new(RetryableError::Timeout));
                        }
                        // Other network/transport errors (connection refused, DNS, etc.)
                        EsError::Transport(transport_err) => {
                            return Err(anyhow::Error::new(RetryableError::Network(
                                transport_err.to_string(),
                            )));
                        }
                        // Everything else is a parse or stream error — permanent
                        other => {
                            return Err(LlmError::ParseError(other.to_string()).into());
                        }
                    }
                }
            }
        }

        // ── Assemble and return the final response ────────────────────────────
        let tc = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls.into_values().map(|b| b.build()).collect())
        };

        Ok(LlmResponse {
            content: if content.is_empty() {
                None
            } else {
                Some(content)
            },
            tool_calls: tc,
            // Convert the raw SSE usage into our public Usage type.
            // If the provider didn't return usage, this is None — callers must handle that.
            usage: usage_acc.map(|u| super::Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
        })
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Error, Debug)]
pub enum LlmError {
    /// Permanent HTTP error (e.g. 400 Bad Request, 401 Unauthorized).
    /// Transient errors (429, 5xx) are handled by `RetryableError` in retry.rs.
    #[error("HTTP error {status}: {body}")]
    HttpError { status: u16, body: String },
    #[error("Stream parse error: {0}")]
    ParseError(String),
}

// ─── SSE deserialization ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<ChunkChoice>,
    /// Usage statistics. OpenAI sends this in a final chunk with `choices: []`
    /// when `stream_options.include_usage` is `true`.
    usage: Option<StreamChunkUsage>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    delta: Delta,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Delta {
    content: Option<String>,
    tool_calls: Option<Vec<PartialToolCall>>,
}

#[derive(Deserialize)]
struct PartialToolCall {
    index: usize,
    id: Option<String>,
    #[serde(rename = "type")]
    call_type: Option<String>,
    function: Option<PartialFunction>,
}

#[derive(Deserialize, Default)]
struct PartialFunction {
    name: Option<String>,
    arguments: Option<String>,
}

/// Usage data sent by OpenAI in the final SSE chunk when `stream_options.include_usage` is set.
#[derive(Deserialize, Default)]
struct StreamChunkUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

// ─── LlmProvider impl ────────────────────────────────────────────────────────

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        // Delegate to retry_with_backoff, which will call try_once() repeatedly
        // on transient failures (429, 5xx, timeouts, network drops).
        //
        // The closure captures `messages` and `tools` by reference. Rust requires
        // the closure to be `Fn` (not `FnOnce`) because it may be called multiple
        // times, so we use shared references inside.
        retry_with_backoff(&self.retry, self.io.as_ref(), || async {
            self.try_once(messages, tools).await
        })
        .await
    }

    fn is_copilot(&self) -> bool {
        self.copilot_mode.load(Ordering::Relaxed)
    }

    fn set_stream_print(&self, enabled: bool) {
        self.stream_print.store(enabled, Ordering::Relaxed);
    }

    async fn set_copilot_oauth_token(&self, token: String) {
        let mut state = self.copilot.lock().await;
        state.oauth_token = Some(token);
        state.api_token = None; // force refresh on next call
        self.copilot_mode.store(true, Ordering::Relaxed);
    }
}

// ─── Builder ─────────────────────────────────────────────────────────────────

#[derive(Default)]
struct ToolCallBuilder {
    id: Option<String>,
    call_type: Option<String>,
    name: String,
    arguments: String,
}

impl ToolCallBuilder {
    fn build(self) -> ToolCall {
        ToolCall {
            id: self.id.unwrap_or_default(),
            call_type: self.call_type.unwrap_or_else(|| "function".to_string()),
            function: FunctionCall {
                name: self.name,
                arguments: self.arguments,
            },
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn test_message_serialization() {
        let sys = Message::system("hello");
        let user = Message::user("hi");
        let tc = ToolCall {
            id: "abc123".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "file_write".to_string(),
                arguments: "{\"path\":\"foo.txt\"}".to_string(),
            },
        };
        let assistant = Message::assistant(Some("ok".to_string()), Some(vec![tc.clone()]));
        let tool = Message::tool("abc123", "done");
        let msgs = vec![sys, user, assistant, tool];
        for msg in msgs {
            let json_str = serde_json::to_string(&msg).unwrap();
            let back: Message = serde_json::from_str(&json_str).unwrap();
            assert_eq!(msg, back);
            let v: Value = serde_json::from_str(&json_str).unwrap();
            assert!(matches!(
                v["role"].as_str(),
                Some("system") | Some("user") | Some("assistant") | Some("tool")
            ));
        }
    }

    #[test]
    fn test_tool_definition_format() {
        let def = ToolDefinition {
            def_type: "function".to_string(),
            function: FunctionDefinition {
                name: "file_write".to_string(),
                description: "Write a file".to_string(),
                parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            },
        };
        let v: Value = serde_json::to_value(&def).unwrap();
        assert_eq!(v["type"], "function");
        assert!(v["function"].is_object());
    }

    fn parse_sse_chunks(chunks: &[&str]) -> LlmResponse {
        let mut content = String::new();
        let mut tool_calls: HashMap<usize, ToolCallBuilder> = HashMap::new();
        for chunk in chunks {
            if *chunk == "[DONE]" {
                break;
            }
            let chunk: StreamChunk = serde_json::from_str(chunk).unwrap();
            for choice in chunk.choices {
                let delta = choice.delta;
                if let Some(text) = delta.content {
                    content.push_str(&text);
                }
                if let Some(partials) = delta.tool_calls {
                    for partial in partials {
                        let entry = tool_calls
                            .entry(partial.index)
                            .or_insert_with(ToolCallBuilder::default);
                        if let Some(id) = partial.id {
                            entry.id = Some(id);
                        }
                        if let Some(call_type) = partial.call_type {
                            entry.call_type = Some(call_type);
                        }
                        if let Some(function) = partial.function {
                            if let Some(name) = function.name {
                                entry.name.push_str(&name);
                            }
                            if let Some(args) = function.arguments {
                                entry.arguments.push_str(&args);
                            }
                        }
                    }
                }
            }
        }
        let tc = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls.into_iter().map(|(_, b)| b.build()).collect())
        };
        LlmResponse {
            content: if content.is_empty() {
                None
            } else {
                Some(content)
            },
            tool_calls: tc,
            usage: None, // test helper — no real API call
        }
    }

    #[test]
    fn test_sse_parsing_text_only() {
        let chunks = vec![
            r#"{"choices":[{"delta":{"content":"Hello "}}]}"#,
            r#"{"choices":[{"delta":{"content":"world!"}}]}"#,
            "[DONE]",
        ];
        let resp = parse_sse_chunks(&chunks);
        assert_eq!(resp.content, Some("Hello world!".to_string()));
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn test_sse_parsing_tool_call() {
        let chunks = vec![
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"abc","type":"function","function":{"name":"file_","arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"write","arguments":"\"foo.txt\"}"}}]}}]}"#,
            "[DONE]",
        ];
        let resp = parse_sse_chunks(&chunks);
        assert!(resp.content.is_none());
        let tc = resp.tool_calls.unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "abc");
        assert_eq!(tc[0].function.name, "file_write");
        assert_eq!(tc[0].function.arguments, "{\"path\":\"foo.txt\"}");
    }

    #[test]
    fn test_partial_tool_call_assembly() {
        let chunks = vec![
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"file_","arguments":"{"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"write","arguments":"}"}}]}}]}"#,
            "[DONE]",
        ];
        let resp = parse_sse_chunks(&chunks);
        let tc = resp.tool_calls.unwrap();
        assert_eq!(tc[0].function.name, "file_write");
        assert_eq!(tc[0].function.arguments, "{}".to_string());
    }

    #[test]
    fn test_is_copilot() {
        let p = OpenAiProvider::new(
            COPILOT_API_BASE.to_string(),
            String::new(),
            "gpt-4o".to_string(),
        );
        assert!(p.is_copilot());
        let p2 = OpenAiProvider::new(
            "https://api.openai.com/v1".to_string(),
            "key".to_string(),
            "gpt-4o".to_string(),
        );
        assert!(!p2.is_copilot());
    }
}
