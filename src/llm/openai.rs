use super::*;
use anyhow::{bail, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use reqwest_eventsource::{Event, RequestBuilderExt};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
}

impl OpenAiProvider {
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
        }
    }

    /// Returns true if this provider is in GitHub Copilot mode.
    pub fn is_copilot(&self) -> bool {
        self.copilot_mode.load(Ordering::Relaxed)
    }

    /// Enable or disable real-time streaming output to stdout.
    /// Call with `false` before a Plan-mode LLM call so xcodeai can
    /// post-process the reply (e.g. render choices via dialoguer) before
    /// displaying anything.  Always restore to `true` afterwards.
    pub fn set_stream_print(&self, enabled: bool) {
        self.stream_print.store(enabled, Ordering::Relaxed);
    }

    /// Get a fresh Copilot API token, refreshing if expired.
    /// Returns error if no OAuth token is set (user hasn't run :login).
    async fn copilot_bearer_token(&self) -> Result<String> {
        let mut state = self.copilot.lock().await;

        let oauth_token = state
            .oauth_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!(
                "Not authenticated with GitHub Copilot. Run :login in the REPL."
            ))?;

        // Refresh if expired or not yet fetched
        let needs_refresh = state
            .api_token
            .as_ref()
            .map(|t: &crate::auth::CopilotApiToken| t.is_expired())
            .unwrap_or(true);

        if needs_refresh {
            let new_token =
                crate::auth::refresh_copilot_token(&self.client, &oauth_token).await?;
            state.api_token = Some(new_token);
        }

        Ok(state.api_token.as_ref().unwrap().token.clone())
    }

    /// Update the OAuth token (called after :login completes).
    pub async fn set_copilot_oauth_token(&self, token: String) {
        let mut state = self.copilot.lock().await;
        state.oauth_token = Some(token);
        state.api_token = None; // force refresh on next call
        self.copilot_mode.store(true, Ordering::Relaxed);
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Error, Debug)]
pub enum LlmError {
    #[error("HTTP error {status}: {body}")]
    HttpError { status: u16, body: String },
    #[error("Rate limited, retry after {retry_after}s")]
    RateLimited { retry_after: u64 },
    #[error("Stream parse error: {0}")]
    ParseError(String),
}

// ─── SSE deserialization ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<ChunkChoice>,
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

// ─── LlmProvider impl ────────────────────────────────────────────────────────

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        let mut retries = 0u32;
        let max_retries = 3u32;
        let mut delay = 1u64;
        loop {
            let mut body = json!({
                "model": self.model,
                "messages": messages,
                "stream": true,
            });
            if !tools.is_empty() {
                body["tools"] = serde_json::to_value(tools)?;
                body["tool_choice"] = json!("auto");
            }

            // Build request — Copilot needs different URL + auth headers
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

            let mut es = match request.eventsource() {
                Ok(es) => es,
                Err(e) => bail!("Failed to start eventsource: {}", e),
            };
            let mut content = String::new();
            let mut tool_calls: HashMap<usize, ToolCallBuilder> = HashMap::new();
            let mut retry_request = false;
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
                                return Err(
                                    LlmError::ParseError(format!("{}: {}", msg.data, e)).into()
                                )
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
                    Err(e) => {
                        use reqwest_eventsource::Error as EsError;
                        match e {
                            EsError::InvalidStatusCode(status, _) => {
                                let code = status.as_u16();
                                if (code == 429 || code == 503) && retries < max_retries {
                                    tokio::time::sleep(Duration::from_secs(delay)).await;
                                    retries += 1;
                                    delay *= 2;
                                    retry_request = true;
                                    break;
                                } else if code == 429 || code == 503 {
                                    return Err(LlmError::RateLimited { retry_after: delay }.into());
                                } else {
                                    return Err(LlmError::HttpError {
                                        status: code,
                                        body: status.to_string(),
                                    }
                                    .into());
                                }
                            }
                            other => {
                                return Err(LlmError::ParseError(other.to_string()).into());
                            }
                        }
                    }
                }
            }
            if retry_request {
                continue;
            }
            let tc = if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls.into_iter().map(|(_, b)| b.build()).collect())
            };
            return Ok(LlmResponse {
                content: if content.is_empty() {
                    None
                } else {
                    Some(content)
                },
                tool_calls: tc,
            });
        }
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
        let p = OpenAiProvider::new(COPILOT_API_BASE.to_string(), String::new(), "gpt-4o".to_string());
        assert!(p.is_copilot());
        let p2 = OpenAiProvider::new("https://api.openai.com/v1".to_string(), "key".to_string(), "gpt-4o".to_string());
        assert!(!p2.is_copilot());
    }
}
