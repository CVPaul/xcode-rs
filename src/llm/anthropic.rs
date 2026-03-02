// Anthropic Messages API provider for xcodeai.
//
// Implements the `LlmProvider` trait for Anthropic's claude-* models.
// Uses the native Anthropic Messages API (not an OpenAI-compatible endpoint),
// which means we get direct access to features like:
//  - Claude's native content blocks (text, tool_use, tool_result)
//  - Anthropic streaming events (content_block_delta, message_delta, etc.)
//
// References:
//  - API docs: https://docs.anthropic.com/en/api/messages
//  - Streaming: https://docs.anthropic.com/en/api/messages-streaming
//  - Tool use: https://docs.anthropic.com/en/docs/tool-use
//
// # Key differences from the OpenAI provider
//
//  - Auth: `x-api-key` header instead of `Authorization: Bearer`
//  - System prompt: top-level `"system"` field, NOT a message in the array
//  - Messages: only `user` / `assistant` roles (no `system` or `tool` role messages)
//  - Tool calling: uses content blocks (`tool_use`/`tool_result`), not `tool_calls` field
//  - Tool result messages: sent as a `user` message with a `tool_result` content block
//  - Streaming: different event types (content_block_delta, message_delta, etc.)
//  - Usage: comes in the `message_delta` event as `input_tokens`/`output_tokens`

use super::{
    FunctionDefinition, LlmProvider, LlmResponse, Message, Role, ToolCall, ToolDefinition, Usage,
};
use anyhow::{bail, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use reqwest_eventsource::{Event, RequestBuilderExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The Anthropic Messages API endpoint.
const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";

/// Sentinel value for `api_base` in the xcodeai config that selects this provider.
pub const ANTHROPIC_API_BASE: &str = "anthropic";

/// Anthropic API version header value.
/// This must be a date string — Anthropic requires it for all requests.
const ANTHROPIC_VERSION: &str = "2023-06-01";

// ─── AnthropicProvider ────────────────────────────────────────────────────────

/// LLM provider that talks to the native Anthropic Messages API.
///
/// Create via `AnthropicProvider::new(api_key, model)`, then use it
/// wherever an `Arc<dyn LlmProvider>` is needed.
pub struct AnthropicProvider {
    /// The Anthropic API key (from `XCODE_API_KEY` or config file).
    api_key: String,

    /// Model name, e.g. `"claude-opus-4-5"`, `"claude-sonnet-4-5"`.
    pub model: String,

    /// Shared HTTP client — keep alive across calls for connection reuse.
    client: Client,

    /// When true (the default), stream tokens are printed to stdout in real time.
    /// Set to false in Plan mode or when the output should be captured.
    pub stream_print: AtomicBool,
}

impl AnthropicProvider {
    /// Create a new Anthropic provider.
    ///
    /// # Arguments
    ///
    /// * `api_key` — your Anthropic API key (`sk-ant-...`)
    /// * `model` — Claude model name (e.g. `"claude-sonnet-4-5"`)
    pub fn new(api_key: String, model: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("Failed to create HTTP client");

        AnthropicProvider {
            api_key,
            model,
            client,
            stream_print: AtomicBool::new(true),
        }
    }

    /// Enable or disable real-time streaming output to stdout.
    ///
    /// Called internally by the LlmProvider trait override below.
    pub fn set_stream_print(&self, enabled: bool) {
        self.stream_print.store(enabled, Ordering::Relaxed);
    }

    // ── Request building ──────────────────────────────────────────────────────

    /// Build the JSON request body for the Anthropic Messages API.
    ///
    /// Anthropic's API differs from OpenAI in several important ways:
    ///
    /// 1. **System prompt**: extracted from the first `system` role message
    ///    and placed in a top-level `"system"` field (NOT in the messages array).
    ///
    /// 2. **Message roles**: only `user` and `assistant` are valid.  The
    ///    OpenAI `tool` role doesn't exist in Anthropic — tool results are
    ///    sent as `user` messages with `tool_result` content blocks.
    ///
    /// 3. **Tool calling**: tools are defined with `input_schema` (not
    ///    `parameters`), and tool calls/results use content blocks.
    ///
    /// 4. **ContentPart mapping**:
    ///    - `Text` → `{ "type": "text", "text": "..." }`
    ///    - `ToolUse` → `{ "type": "tool_use", "id": ..., "name": ..., "input": ... }`
    ///    - `ToolResult` → `{ "type": "tool_result", "tool_use_id": ..., "content": ... }`
    fn build_request_body(&self, messages: &[Message], tools: &[ToolDefinition]) -> Value {
        // ── 1. Separate system prompt from conversation messages ──────────────
        //
        // Anthropic's API requires the system prompt as a separate top-level
        // field.  We look for the first `Role::System` message and extract it.
        // All other messages become the conversation array.
        let mut system_prompt: Option<String> = None;
        let mut conversation: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    // Take the system message text and store it separately.
                    // If there are multiple system messages (unusual), we concat them.
                    let text = msg.text_content().unwrap_or_default();
                    match system_prompt.as_mut() {
                        Some(existing) => {
                            existing.push('\n');
                            existing.push_str(&text);
                        }
                        None => system_prompt = Some(text),
                    }
                }
                Role::Tool => {
                    // Anthropic doesn't have a `tool` role.  Tool results are
                    // sent as `user` messages with a `tool_result` content block.
                    let tool_call_id = msg.tool_call_id.as_deref().unwrap_or("unknown");
                    let result_text = msg.text_content().unwrap_or_default();

                    conversation.push(json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": tool_call_id,
                            "content": result_text,
                        }]
                    }));
                }
                Role::User | Role::Assistant => {
                    // Build the Anthropic content blocks from our ContentPart enum.
                    let content = build_anthropic_content(&msg.content, msg.tool_calls.as_deref());

                    let role_str = match msg.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        _ => unreachable!(),
                    };

                    conversation.push(json!({
                        "role": role_str,
                        "content": content,
                    }));
                }
            }
        }

        // ── 2. Build tool definitions in Anthropic format ─────────────────────
        //
        // Anthropic uses `input_schema` where OpenAI uses `parameters`.
        // The actual JSON Schema content is identical.
        let anthropic_tools: Vec<Value> = tools
            .iter()
            .map(|t| build_anthropic_tool(&t.function))
            .collect();

        // ── 3. Assemble the final request body ────────────────────────────────
        let mut body = json!({
            "model": self.model,
            "max_tokens": 8096,
            "messages": conversation,
            "stream": true,
        });

        // Only add system prompt if we have one — empty system is not sent
        if let Some(sys) = system_prompt {
            body["system"] = json!(sys);
        }

        // Only add tools if we have any — empty array would be invalid
        if !anthropic_tools.is_empty() {
            body["tools"] = json!(anthropic_tools);
        }

        body
    }

    // ── Single-attempt execution ──────────────────────────────────────────────

    /// Execute one SSE streaming request to the Anthropic API.
    ///
    /// Returns the assembled `LlmResponse` on success.  On transient errors
    /// (rate limits, 5xx), the caller may retry.
    async fn try_once(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        let body = self.build_request_body(messages, tools);

        // Build the HTTP request with Anthropic-specific headers
        let request = self
            .client
            .post(ANTHROPIC_API_URL)
            // Anthropic uses x-api-key, not Authorization: Bearer
            .header("x-api-key", &self.api_key)
            // Required by Anthropic — specifies the API version
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body);

        // Open the SSE stream
        let mut es = match request.eventsource() {
            Ok(es) => es,
            Err(e) => bail!("Failed to start Anthropic SSE stream: {}", e),
        };

        // ── State accumulators ────────────────────────────────────────────────
        //
        // Anthropic's SSE protocol sends the response in content blocks.
        // Each block starts with `content_block_start`, receives delta events,
        // and ends with `content_block_stop`.  We track the "current block"
        // by index and accumulate the text or input JSON for each block.

        // Text content accumulated from text blocks
        let mut text_content = String::new();

        // Tool use blocks: index → partial builder
        let mut tool_use_blocks: std::collections::HashMap<u32, AnthropicToolUseBuilder> =
            std::collections::HashMap::new();

        // Usage stats (extracted from message_delta event)
        let mut input_tokens: u32 = 0;
        let mut output_tokens: u32 = 0;

        // ── Consume the SSE event stream ──────────────────────────────────────
        while let Some(event) = es.next().await {
            match event {
                Ok(Event::Open) => {}

                Ok(Event::Message(msg)) => {
                    // Each SSE message has an event type field (e.g. "content_block_delta")
                    // We parse the event type to decide how to handle the data.
                    let event_type = msg.event.as_str();

                    match event_type {
                        // ── message_start ─────────────────────────────────────
                        // First event — contains initial usage (input tokens)
                        "message_start" => {
                            if let Ok(v) = serde_json::from_str::<Value>(&msg.data) {
                                // input_tokens is available early in message_start
                                if let Some(n) = v
                                    .pointer("/message/usage/input_tokens")
                                    .and_then(|v| v.as_u64())
                                {
                                    input_tokens = n as u32;
                                }
                            }
                        }

                        // ── content_block_start ───────────────────────────────
                        // A new content block is beginning.  The block type tells us
                        // whether this will be text or a tool_use call.
                        "content_block_start" => {
                            if let Ok(data) =
                                serde_json::from_str::<ContentBlockStartData>(&msg.data)
                            {
                                if data.content_block.block_type == "tool_use" {
                                    // Start a new tool use builder for this block index
                                    tool_use_blocks.insert(
                                        data.index,
                                        AnthropicToolUseBuilder {
                                            id: data.content_block.id.unwrap_or_default(),
                                            name: data.content_block.name.unwrap_or_default(),
                                            input_json: String::new(),
                                        },
                                    );
                                }
                                // For text blocks, we just start accumulating from deltas
                            }
                        }

                        // ── content_block_delta ───────────────────────────────
                        // A chunk of content for the current block.
                        // Delta type can be:
                        //   "text_delta"       → { "text": "..." } (for text blocks)
                        //   "input_json_delta" → { "partial_json": "..." } (for tool_use blocks)
                        "content_block_delta" => {
                            if let Ok(data) =
                                serde_json::from_str::<ContentBlockDeltaData>(&msg.data)
                            {
                                match data.delta.delta_type.as_str() {
                                    "text_delta" => {
                                        if let Some(text) = data.delta.text {
                                            // Stream to stdout if enabled
                                            if self.stream_print.load(Ordering::Relaxed) {
                                                print!("{}", text);
                                                std::io::stdout().flush().ok();
                                            }
                                            text_content.push_str(&text);
                                        }
                                    }
                                    "input_json_delta" => {
                                        // Accumulate the JSON fragments for this tool call
                                        if let Some(partial) = data.delta.partial_json {
                                            if let Some(builder) =
                                                tool_use_blocks.get_mut(&data.index)
                                            {
                                                builder.input_json.push_str(&partial);
                                            }
                                        }
                                    }
                                    _ => {} // Unknown delta type — ignore
                                }
                            }
                        }

                        // ── content_block_stop ────────────────────────────────
                        // The current block is complete.  Nothing to do here since we
                        // accumulated everything in content_block_delta events.
                        "content_block_stop" => {}

                        // ── message_delta ─────────────────────────────────────
                        // Contains the final stop_reason and output token usage.
                        "message_delta" => {
                            if let Ok(data) = serde_json::from_str::<MessageDeltaData>(&msg.data) {
                                // Capture output token count
                                if let Some(n) = data.usage.and_then(|u| u.output_tokens) {
                                    output_tokens = n;
                                }
                            }
                        }

                        // ── message_stop ──────────────────────────────────────
                        // The stream is complete.  Close and break.
                        "message_stop" => {
                            es.close();
                            break;
                        }

                        // Ignore ping events and unknown event types
                        _ => {}
                    }
                }

                Err(e) => {
                    use reqwest_eventsource::Error as EsError;
                    match e {
                        EsError::InvalidStatusCode(status, _resp) => {
                            let code = status.as_u16();
                            bail!("Anthropic API error {}: {}", code, status);
                        }
                        EsError::Transport(e) => {
                            bail!("Anthropic transport error: {}", e);
                        }
                        other => {
                            bail!("Anthropic SSE error: {}", other);
                        }
                    }
                }
            }
        }

        // ── Assemble the final LlmResponse ────────────────────────────────────

        // Convert the accumulated tool_use blocks into our universal ToolCall type
        let tool_calls: Vec<ToolCall> = tool_use_blocks.into_values().map(|b| b.build()).collect();

        Ok(LlmResponse {
            content: if text_content.is_empty() {
                None
            } else {
                Some(text_content)
            },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            usage: Some(Usage {
                prompt_tokens: input_tokens,
                completion_tokens: output_tokens,
                total_tokens: input_tokens + output_tokens,
            }),
        })
    }
}

// ─── LlmProvider impl ────────────────────────────────────────────────────────

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        self.try_once(messages, tools).await
    }

    /// Override so callers can toggle streaming via the trait interface.
    fn set_stream_print(&self, enabled: bool) {
        // Delegate to the concrete struct method.
        AnthropicProvider::set_stream_print(self, enabled);
    }
}

// ─── Helper functions ─────────────────────────────────────────────────────────

/// Convert our `ContentPart` enum and optional `tool_calls` into Anthropic
/// content blocks.
///
/// Anthropic uses a content array for both text and tool interactions:
///
/// ```json
/// [
///   { "type": "text", "text": "I'll call a tool now." },
///   { "type": "tool_use", "id": "toolu_01", "name": "file_write", "input": {...} }
/// ]
/// ```
///
/// For OpenAI-style tool calls (which we store in `message.tool_calls`), we
/// convert each `ToolCall` into a `tool_use` content block.
fn build_anthropic_content(
    parts: &[super::ContentPart],
    tool_calls: Option<&[ToolCall]>,
) -> Vec<Value> {
    let mut blocks: Vec<Value> = Vec::new();

    // Add content parts
    for part in parts {
        match part {
            super::ContentPart::Text { text } => {
                blocks.push(json!({
                    "type": "text",
                    "text": text,
                }));
            }
            super::ContentPart::ImageUrl { image_url } => {
                // Anthropic uses a different image format than OpenAI.
                // We detect base64 data URIs and convert them.
                if image_url.url.starts_with("data:") {
                    // data:<mime>;base64,<data>
                    if let Some((header, data)) = image_url.url.split_once(',') {
                        let mime = header
                            .strip_prefix("data:")
                            .and_then(|s| s.strip_suffix(";base64"))
                            .unwrap_or("image/jpeg");
                        blocks.push(json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": mime,
                                "data": data,
                            }
                        }));
                    }
                } else {
                    // External URL — Anthropic supports URL sources too
                    blocks.push(json!({
                        "type": "image",
                        "source": {
                            "type": "url",
                            "url": image_url.url,
                        }
                    }));
                }
            }
            super::ContentPart::ToolUse { id, name, input } => {
                // Already in Anthropic format — pass through
                blocks.push(json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                }));
            }
            super::ContentPart::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                blocks.push(json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content,
                    "is_error": is_error,
                }));
            }
        }
    }

    // Add OpenAI-style tool calls as tool_use blocks.
    // This handles the case where the provider previously returned tool calls
    // in the OpenAI format (message.tool_calls) and we're now in an Anthropic context.
    if let Some(tcs) = tool_calls {
        for tc in tcs {
            // Parse the arguments string back to a JSON object
            let input: Value = serde_json::from_str(&tc.function.arguments).unwrap_or(json!({}));
            blocks.push(json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.function.name,
                "input": input,
            }));
        }
    }

    // If no blocks, return a single empty text block.
    // Anthropic requires at least one content block.
    if blocks.is_empty() {
        blocks.push(json!({"type": "text", "text": ""}));
    }

    blocks
}

/// Convert an xcodeai `FunctionDefinition` to the Anthropic tool format.
///
/// The key difference from OpenAI: `input_schema` vs `parameters`.
fn build_anthropic_tool(func: &FunctionDefinition) -> Value {
    json!({
        "name": func.name,
        "description": func.description,
        // Anthropic uses "input_schema" where OpenAI uses "parameters"
        // The actual JSON Schema value is the same.
        "input_schema": func.parameters,
    })
}

// ─── Tool use builder ─────────────────────────────────────────────────────────

/// Accumulates the streaming fragments of a single tool_use content block.
///
/// Anthropic streams the JSON input of a tool call as a sequence of
/// `input_json_delta` events.  We concatenate them here and then parse
/// the final JSON string when the block completes.
struct AnthropicToolUseBuilder {
    /// The tool call id (from content_block_start)
    id: String,
    /// The tool name (from content_block_start)
    name: String,
    /// Accumulated partial JSON string (from input_json_delta events)
    input_json: String,
}

impl AnthropicToolUseBuilder {
    /// Convert the accumulated data into a `ToolCall`.
    ///
    /// We keep `arguments` as a JSON string (not parsed) to match the
    /// OpenAI format that the rest of xcodeai expects.
    fn build(self) -> ToolCall {
        ToolCall {
            id: self.id,
            call_type: "function".to_string(),
            function: super::FunctionCall {
                name: self.name,
                // The arguments JSON string — parse errors just fall back to "{}"
                arguments: if self.input_json.is_empty() {
                    "{}".to_string()
                } else {
                    self.input_json
                },
            },
        }
    }
}

// ─── SSE deserialization structs ──────────────────────────────────────────────
//
// These private structs are used only for parsing the SSE event payloads.
// They don't need to be part of the public API.

/// Data for the `content_block_start` event.
#[derive(Deserialize)]
struct ContentBlockStartData {
    /// The index of this content block in the response (0-based).
    index: u32,
    /// The starting state of the content block.
    content_block: ContentBlockStart,
}

#[derive(Deserialize)]
struct ContentBlockStart {
    #[serde(rename = "type")]
    block_type: String,
    /// Present for `tool_use` blocks — the call id
    id: Option<String>,
    /// Present for `tool_use` blocks — the tool name
    name: Option<String>,
}

/// Data for the `content_block_delta` event.
#[derive(Deserialize)]
struct ContentBlockDeltaData {
    /// The index of the content block this delta belongs to.
    index: u32,
    /// The delta content.
    delta: ContentBlockDelta,
}

#[derive(Deserialize)]
struct ContentBlockDelta {
    #[serde(rename = "type")]
    delta_type: String,
    /// Present for `text_delta` — the text fragment
    text: Option<String>,
    /// Present for `input_json_delta` — a partial JSON string for a tool call
    partial_json: Option<String>,
}

/// Data for the `message_delta` event — contains final usage stats.
#[derive(Deserialize)]
struct MessageDeltaData {
    /// Present in `message_delta` events — contains output token count.
    usage: Option<MessageDeltaUsage>,
}

#[derive(Deserialize)]
struct MessageDeltaUsage {
    output_tokens: Option<u32>,
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ContentPart, FunctionDefinition, ImageUrl, Message, ToolDefinition};
    use serde_json::json;

    // ── Request building tests ──────────────────────────────────────────────

    /// System message is extracted into the top-level "system" field.
    #[test]
    fn test_system_message_extracted() {
        let provider = AnthropicProvider::new("key".into(), "claude-opus-4-5".into());

        let messages = vec![
            Message::system("You are a helpful assistant."),
            Message::user("Hello!"),
        ];

        let body = provider.build_request_body(&messages, &[]);

        // System prompt should be a top-level string field
        assert_eq!(
            body["system"], "You are a helpful assistant.",
            "System prompt should be a top-level field"
        );

        // The messages array should only contain user/assistant messages
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(
            msgs.len(),
            1,
            "Only non-system messages should be in messages array"
        );
        assert_eq!(msgs[0]["role"], "user");
    }

    /// User message is serialized correctly with text content block.
    #[test]
    fn test_user_message_content_block() {
        let provider = AnthropicProvider::new("key".into(), "claude-opus-4-5".into());
        let messages = vec![Message::user("Hello!")];
        let body = provider.build_request_body(&messages, &[]);

        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        // Content should be an array of blocks
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Hello!");
    }

    /// Tool role message becomes a user message with tool_result content block.
    #[test]
    fn test_tool_result_message_conversion() {
        let provider = AnthropicProvider::new("key".into(), "claude-opus-4-5".into());

        let tool_msg = Message::tool("call_123", "File written successfully.");
        let messages = vec![tool_msg];
        let body = provider.build_request_body(&messages, &[]);

        let msgs = body["messages"].as_array().unwrap();
        // Tool result becomes a user message in Anthropic format
        assert_eq!(msgs[0]["role"], "user");
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "call_123");
        assert_eq!(content[0]["content"], "File written successfully.");
    }

    /// Tool definitions are converted to Anthropic format (input_schema not parameters).
    #[test]
    fn test_tool_definition_anthropic_format() {
        let provider = AnthropicProvider::new("key".into(), "claude-opus-4-5".into());

        let tools = vec![ToolDefinition {
            def_type: "function".to_string(),
            function: FunctionDefinition {
                name: "file_write".to_string(),
                description: "Write a file".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    }
                }),
            },
        }];

        let body = provider.build_request_body(&[], &tools);
        let tools_json = body["tools"].as_array().unwrap();

        assert_eq!(tools_json[0]["name"], "file_write");
        assert_eq!(tools_json[0]["description"], "Write a file");
        // Anthropic uses "input_schema" not "parameters"
        assert!(
            tools_json[0].get("input_schema").is_some(),
            "Should use input_schema, not parameters"
        );
        assert!(
            tools_json[0].get("parameters").is_none(),
            "Should NOT have parameters field"
        );
    }

    /// No tools → tools field is not included in the request body.
    #[test]
    fn test_no_tools_omitted_from_body() {
        let provider = AnthropicProvider::new("key".into(), "claude-opus-4-5".into());
        let messages = vec![Message::user("Hello!")];
        let body = provider.build_request_body(&messages, &[]);

        assert!(
            body.get("tools").is_none(),
            "tools field should be absent when there are no tools"
        );
    }

    /// No system message → "system" field absent from body.
    #[test]
    fn test_no_system_message_omitted_from_body() {
        let provider = AnthropicProvider::new("key".into(), "claude-opus-4-5".into());
        let messages = vec![Message::user("Hello!")];
        let body = provider.build_request_body(&messages, &[]);

        assert!(
            body.get("system").is_none(),
            "system field should be absent when there is no system message"
        );
    }

    // ── Content block conversion tests ──────────────────────────────────────

    /// Text ContentPart → Anthropic text block.
    #[test]
    fn test_build_anthropic_content_text() {
        let parts = vec![ContentPart::text("Hello!")];
        let blocks = build_anthropic_content(&parts, None);

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "Hello!");
    }

    /// ToolUse ContentPart → Anthropic tool_use block (passed through as-is).
    #[test]
    fn test_build_anthropic_content_tool_use() {
        let parts = vec![ContentPart::ToolUse {
            id: "toolu_01".to_string(),
            name: "file_write".to_string(),
            input: json!({ "path": "test.txt" }),
        }];
        let blocks = build_anthropic_content(&parts, None);

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_use");
        assert_eq!(blocks[0]["id"], "toolu_01");
        assert_eq!(blocks[0]["name"], "file_write");
    }

    /// ToolResult ContentPart → Anthropic tool_result block.
    #[test]
    fn test_build_anthropic_content_tool_result() {
        let parts = vec![ContentPart::ToolResult {
            tool_use_id: "toolu_01".to_string(),
            content: "done".to_string(),
            is_error: false,
        }];
        let blocks = build_anthropic_content(&parts, None);

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "toolu_01");
        assert_eq!(blocks[0]["content"], "done");
        assert_eq!(blocks[0]["is_error"], false);
    }

    /// Base64 data URI image → Anthropic base64 image block.
    #[test]
    fn test_build_anthropic_content_image_base64() {
        let parts = vec![ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: "data:image/png;base64,abc123".to_string(),
                detail: None,
            },
        }];
        let blocks = build_anthropic_content(&parts, None);

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["source"]["type"], "base64");
        assert_eq!(blocks[0]["source"]["media_type"], "image/png");
        assert_eq!(blocks[0]["source"]["data"], "abc123");
    }

    /// External URL image → Anthropic URL image block.
    #[test]
    fn test_build_anthropic_content_image_url() {
        let parts = vec![ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: "https://example.com/image.jpg".to_string(),
                detail: None,
            },
        }];
        let blocks = build_anthropic_content(&parts, None);

        assert_eq!(blocks[0]["source"]["type"], "url");
        assert_eq!(blocks[0]["source"]["url"], "https://example.com/image.jpg");
    }

    /// Empty content → single empty text block (Anthropic requires non-empty content).
    #[test]
    fn test_empty_content_produces_empty_text_block() {
        let blocks = build_anthropic_content(&[], None);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "");
    }

    // ── Tool definition format tests ────────────────────────────────────────

    /// build_anthropic_tool converts parameters → input_schema.
    #[test]
    fn test_build_anthropic_tool_format() {
        let func = FunctionDefinition {
            name: "bash".to_string(),
            description: "Run a bash command".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                }
            }),
        };

        let tool = build_anthropic_tool(&func);

        assert_eq!(tool["name"], "bash");
        assert_eq!(tool["description"], "Run a bash command");
        // The schema is under "input_schema", not "parameters"
        assert_eq!(tool["input_schema"]["type"], "object");
        assert!(tool.get("parameters").is_none());
    }

    // ── AnthropicToolUseBuilder tests ───────────────────────────────────────

    /// Builder correctly assembles a ToolCall from accumulated streaming fragments.
    #[test]
    fn test_tool_use_builder() {
        let builder = AnthropicToolUseBuilder {
            id: "toolu_01".to_string(),
            name: "file_write".to_string(),
            input_json: r#"{"path":"test.txt","content":"hello"}"#.to_string(),
        };

        let tc = builder.build();
        assert_eq!(tc.id, "toolu_01");
        assert_eq!(tc.call_type, "function");
        assert_eq!(tc.function.name, "file_write");
        assert_eq!(
            tc.function.arguments,
            r#"{"path":"test.txt","content":"hello"}"#
        );
    }

    /// Empty input_json falls back to "{}" (valid empty JSON object).
    #[test]
    fn test_tool_use_builder_empty_input() {
        let builder = AnthropicToolUseBuilder {
            id: "toolu_02".to_string(),
            name: "list_files".to_string(),
            input_json: String::new(),
        };

        let tc = builder.build();
        assert_eq!(tc.function.arguments, "{}");
    }
}
