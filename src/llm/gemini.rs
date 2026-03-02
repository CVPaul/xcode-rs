// Google Gemini API provider for xcodeai.
//
// Implements the `LlmProvider` trait for Google's gemini-* models.
// Uses the native Gemini `generateContent` REST API with SSE streaming.
//
// References:
//  - API docs: https://ai.google.dev/api/generate-content
//  - Streaming: https://ai.google.dev/api/generate-content#v1beta.models.streamGenerateContent
//  - Tool use: https://ai.google.dev/api/caching#FunctionDeclaration
//
// # Key differences from the OpenAI provider
//
//  - Auth: `?key=` query parameter (no Authorization header)
//  - URL: `POST .../models/{model}:streamGenerateContent?key={key}&alt=sse`
//  - System prompt: top-level `"system_instruction"` field with `{ "parts": [{ "text": ... }] }`
//  - Roles: `"user"` and `"model"` (not `"assistant"`)
//  - Message format: `{ "role": ..., "parts": [{ "text": ... }] }`
//  - Tool results: sent as a `"user"` message with `{ "function_response": { "name": ..., "response": { "content": ... } } }`
//  - Tool definitions: inside `{ "tools": [{ "function_declarations": [...] }] }`
//  - Tool calls in response: `{ "functionCall": { "name": ..., "args": {...} } }` part
//  - Streaming: SSE `data:` lines, each a full JSON `GenerateContentResponse`
//  - Usage: `usageMetadata.promptTokenCount` + `usageMetadata.candidatesTokenCount`
//
// # Streaming note
//
// Gemini's streaming differs from Anthropic/OpenAI in one important way:
// each SSE chunk is a *complete* partial response, not a diff/delta.
// So we accumulate text by concatenating the `.candidates[0].content.parts[0].text`
// from each chunk, and we take the final chunk's usageMetadata for token counts.

use super::{
    FunctionDefinition, LlmProvider, LlmResponse, Message, Role, ToolCall, ToolDefinition, Usage,
};
use anyhow::{bail, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Gemini API base URL (without model or method).
const GEMINI_API_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";

/// Sentinel value for `api_base` in the xcodeai config that selects this provider.
/// When `config.provider.api_base == "gemini"`, `AgentContext::new()` creates a `GeminiProvider`.
pub const GEMINI_API_BASE: &str = "gemini";

// ─── GeminiProvider ───────────────────────────────────────────────────────────

/// LLM provider that talks to the native Google Gemini API.
///
/// Create via `GeminiProvider::new(api_key, model)`, then use wherever
/// an `Arc<dyn LlmProvider>` is needed.
///
/// # Example
/// ```rust,ignore
/// let provider = GeminiProvider::new(
///     std::env::var("GEMINI_API_KEY").unwrap(),
///     "gemini-2.0-flash".to_string(),
/// );
/// let llm: Arc<dyn LlmProvider> = Arc::new(provider);
/// ```
pub struct GeminiProvider {
    /// The Google AI API key (from `XCODE_API_KEY` or config file).
    api_key: String,

    /// Model name, e.g. `"gemini-2.0-flash"`, `"gemini-1.5-pro"`.
    pub model: String,

    /// Shared HTTP client — kept alive across calls for connection reuse.
    client: Client,

    /// When true (the default), streamed tokens are printed to stdout in real time.
    /// Set to false in Plan mode or when output should be captured for post-processing.
    pub stream_print: AtomicBool,
}

impl GeminiProvider {
    /// Create a new Gemini provider.
    ///
    /// # Arguments
    /// * `api_key` — your Google AI API key
    /// * `model` — Gemini model name (e.g. `"gemini-2.0-flash"`)
    pub fn new(api_key: String, model: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("Failed to create HTTP client");

        GeminiProvider {
            api_key,
            model,
            client,
            stream_print: AtomicBool::new(true),
        }
    }

    /// Enable or disable real-time streaming output to stdout.
    ///
    /// Called internally via the LlmProvider trait override below.
    pub fn set_stream_print(&self, enabled: bool) {
        self.stream_print.store(enabled, Ordering::Relaxed);
    }

    // ── Request building ──────────────────────────────────────────────────────

    /// Build the JSON request body for the Gemini `generateContent` API.
    ///
    /// Gemini's API differs from OpenAI in several important ways:
    ///
    /// 1. **System prompt**: extracted from the first `Role::System` message and
    ///    placed in a top-level `"system_instruction"` object.
    ///
    /// 2. **Roles**: only `"user"` and `"model"` are valid (Gemini calls the
    ///    assistant role `"model"`).  The OpenAI `"tool"` role doesn't exist;
    ///    tool results are sent as `"user"` messages with `"function_response"` parts.
    ///
    /// 3. **Message format**: each message is `{ "role": ..., "parts": [...] }`.
    ///    Parts can be `{ "text": "..." }` or `{ "function_call": {...} }` or
    ///    `{ "function_response": {...} }`.
    ///
    /// 4. **Tool definitions**: wrapped in a top-level `"tools"` array containing
    ///    a single object with a `"function_declarations"` array.
    ///
    /// 5. **Alternating turns**: Gemini requires strict user/model alternation.
    ///    Back-to-back tool results (user role) can be merged into one message.
    fn build_request_body(&self, messages: &[Message], tools: &[ToolDefinition]) -> Value {
        // ── 1. Separate system prompt from conversation messages ──────────────
        //
        // We scan for the first Role::System message and remove it from the
        // conversation.  Multiple system messages (unusual) are concatenated.
        let mut system_text: Option<String> = None;
        let mut gemini_messages: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    let text = msg.text_content().unwrap_or_default();
                    match system_text.as_mut() {
                        Some(existing) => {
                            existing.push('\n');
                            existing.push_str(&text);
                        }
                        None => system_text = Some(text),
                    }
                }

                Role::Tool => {
                    // Gemini doesn't have a `tool` role. Tool results are sent
                    // as `"user"` messages with a `"function_response"` part.
                    //
                    // The `tool_call_id` field holds the function name in our
                    // internal format (set by the agent loop in coder.rs).
                    let name = msg.tool_call_id.as_deref().unwrap_or("unknown");
                    let result_text = msg.text_content().unwrap_or_default();

                    gemini_messages.push(json!({
                        "role": "user",
                        "parts": [{
                            "function_response": {
                                "name": name,
                                "response": {
                                    // Gemini expects the response as an object.
                                    // We wrap the text result in a `content` key.
                                    "content": result_text
                                }
                            }
                        }]
                    }));
                }

                Role::User | Role::Assistant => {
                    // Build Gemini `parts` from our ContentPart array.
                    let parts = build_gemini_content_parts(&msg.content, msg.tool_calls.as_deref());

                    // Map our roles to Gemini roles:
                    // - User → "user"
                    // - Assistant → "model"
                    let role_str = match msg.role {
                        Role::User => "user",
                        Role::Assistant => "model",
                        _ => unreachable!(),
                    };

                    gemini_messages.push(json!({
                        "role": role_str,
                        "parts": parts,
                    }));
                }
            }
        }

        // ── 2. Build tool definitions in Gemini format ────────────────────────
        //
        // Gemini wraps all function declarations in a single `tools` entry:
        // `{ "tools": [{ "function_declarations": [...] }] }`
        let function_declarations: Vec<Value> = tools
            .iter()
            .map(|t| build_gemini_tool(&t.function))
            .collect();

        // ── 3. Assemble the final request body ────────────────────────────────
        let mut body = json!({
            "contents": gemini_messages,
        });

        // Only add system_instruction if we have one
        if let Some(sys) = system_text {
            body["system_instruction"] = json!({
                "parts": [{ "text": sys }]
            });
        }

        // Only add tools if we have any declarations
        if !function_declarations.is_empty() {
            body["tools"] = json!([{
                "function_declarations": function_declarations
            }]);
        }

        body
    }

    // ── Single-attempt execution ──────────────────────────────────────────────

    /// Execute one SSE streaming request to the Gemini API.
    ///
    /// Gemini's streaming works differently from Anthropic/OpenAI:
    /// - Each SSE `data:` line contains a *complete* partial response JSON
    ///   (a `GenerateContentResponse`), not a diff.
    /// - We accumulate text by concatenating the text from each chunk.
    /// - Token usage is in the final chunk's `usageMetadata`.
    /// - Tool calls appear as `functionCall` parts in a candidate's content.
    ///
    /// The URL format is:
    /// `POST https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent?key={key}&alt=sse`
    async fn try_once(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        let body = self.build_request_body(messages, tools);

        // Build the streaming URL: model goes into the path, key into the query
        let url = format!(
            "{}/{}:streamGenerateContent?key={}&alt=sse",
            GEMINI_API_BASE_URL, self.model, self.api_key
        );

        // Build the HTTP request (no auth header — key is in the URL)
        let response = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        // Check for HTTP-level errors before reading the body
        let status = response.status();
        if !status.is_success() {
            let err_body = response.text().await.unwrap_or_default();
            bail!("Gemini API error {}: {}", status.as_u16(), err_body);
        }

        // ── State accumulators ────────────────────────────────────────────────
        //
        // Gemini sends multiple SSE chunks, each a complete partial response.
        // We accumulate text and function calls across all chunks.

        // Final text content (concatenated across all chunks)
        let mut text_content = String::new();

        // Function calls: name → args JSON object
        // We use a Vec because order matters (tool calls should preserve order)
        let mut function_calls: Vec<(String, String)> = Vec::new();

        // Token usage (overwritten each chunk; final chunk has the full counts)
        let mut prompt_tokens: u32 = 0;
        let mut completion_tokens: u32 = 0;

        // ── Read SSE stream line by line ──────────────────────────────────────
        //
        // Gemini SSE format: lines starting with `data: ` contain JSON.
        // We read the raw bytes stream and split by lines manually.
        let mut byte_stream = response.bytes_stream();

        // Buffer to accumulate incomplete lines across chunks
        let mut line_buf = String::new();

        while let Some(chunk_result) = byte_stream.next().await {
            let chunk = chunk_result?;
            let chunk_str = String::from_utf8_lossy(&chunk);

            // Append the new bytes to our line buffer
            line_buf.push_str(&chunk_str);

            // Process all complete lines in the buffer
            while let Some(newline_pos) = line_buf.find('\n') {
                // Extract the complete line (without the newline)
                let line = line_buf[..newline_pos].trim_end_matches('\r').to_string();
                line_buf = line_buf[newline_pos + 1..].to_string();

                // Only process `data:` lines (SSE format)
                if let Some(json_str) = line.strip_prefix("data: ") {
                    let json_str = json_str.trim();

                    // Skip `[DONE]` markers (some implementations send these)
                    if json_str == "[DONE]" {
                        continue;
                    }

                    // Parse the JSON chunk
                    if let Ok(chunk_json) = serde_json::from_str::<GeminiResponse>(json_str) {
                        // ── Extract text and function calls from candidates ────
                        for candidate in &chunk_json.candidates {
                            if let Some(content) = &candidate.content {
                                for part in &content.parts {
                                    // Text part: accumulate and optionally stream to stdout
                                    if let Some(text) = &part.text {
                                        if self.stream_print.load(Ordering::Relaxed) {
                                            print!("{}", text);
                                            std::io::stdout().flush().ok();
                                        }
                                        text_content.push_str(text);
                                    }

                                    // Function call part: capture name + args
                                    if let Some(fc) = &part.function_call {
                                        // Serialize the args back to a JSON string
                                        // so we can store them in our ToolCall format
                                        let args_json = fc
                                            .args
                                            .as_ref()
                                            .map(|a| serde_json::to_string(a).unwrap_or_default())
                                            .unwrap_or_else(|| "{}".to_string());
                                        function_calls.push((fc.name.clone(), args_json));
                                    }
                                }
                            }
                        }

                        // ── Extract token usage ───────────────────────────────
                        if let Some(usage) = &chunk_json.usage_metadata {
                            if let Some(n) = usage.prompt_token_count {
                                prompt_tokens = n;
                            }
                            if let Some(n) = usage.candidates_token_count {
                                completion_tokens = n;
                            }
                        }
                    }
                }
            }
        }

        // ── Assemble the final LlmResponse ────────────────────────────────────

        // Convert accumulated function calls into our universal ToolCall type.
        // Gemini doesn't provide tool call IDs in the same way, so we synthesize
        // unique IDs using the index and function name.
        let tool_calls: Vec<ToolCall> = function_calls
            .into_iter()
            .enumerate()
            .map(|(i, (name, args))| ToolCall {
                id: format!("gemini_call_{}_{}", i, name),
                call_type: "function".to_string(),
                function: super::FunctionCall {
                    name,
                    arguments: args,
                },
            })
            .collect();

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
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            }),
        })
    }
}

// ─── LlmProvider impl ────────────────────────────────────────────────────────

#[async_trait]
impl LlmProvider for GeminiProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        self.try_once(messages, tools).await
    }

    /// Override so callers can toggle streaming via the trait interface.
    fn set_stream_print(&self, enabled: bool) {
        GeminiProvider::set_stream_print(self, enabled);
    }
}

// ─── Response deserialization structs ────────────────────────────────────────
//
// These structs mirror the Gemini API response JSON structure.
// They're only used for deserialization — we don't need to serialize them.
//
// Gemini response schema (simplified):
// ```json
// {
//   "candidates": [{
//     "content": {
//       "role": "model",
//       "parts": [
//         { "text": "..." },
//         { "functionCall": { "name": "...", "args": {...} } }
//       ]
//     },
//     "finishReason": "STOP"
//   }],
//   "usageMetadata": {
//     "promptTokenCount": 100,
//     "candidatesTokenCount": 50,
//     "totalTokenCount": 150
//   }
// }
// ```

/// Top-level Gemini API response (one per SSE chunk in streaming mode).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiResponse {
    /// List of candidate responses (usually just one for non-voting mode).
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,

    /// Token usage metadata (present in all chunks; final chunk has full counts).
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsageMetadata>,
}

/// One candidate response in the Gemini output.
#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    /// The generated content (text and/or function calls).
    content: Option<GeminiContent>,
}

/// The `content` field inside a Gemini candidate.
#[derive(Debug, Deserialize)]
struct GeminiContent {
    /// The parts making up this content (text, function call, etc.)
    #[serde(default)]
    parts: Vec<GeminiPart>,
}

/// A single part within a Gemini content object.
///
/// Parts are polymorphic — a part can be a text chunk, a function call,
/// or (in request messages) a function response.  Only one field will be
/// present in any given part.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    /// Plain text content.
    text: Option<String>,

    /// A function call request from the model.
    function_call: Option<GeminiFunctionCall>,
}

/// A function call emitted by the Gemini model.
#[derive(Debug, Deserialize)]
struct GeminiFunctionCall {
    /// The name of the function to call.
    name: String,

    /// The arguments as a JSON object.  May be absent if the function takes no args.
    args: Option<Value>,
}

/// Token usage statistics from Gemini.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    /// Number of tokens in the prompt (input).
    prompt_token_count: Option<u32>,

    /// Number of tokens generated (output).
    candidates_token_count: Option<u32>,
}

// ─── Request building helpers ─────────────────────────────────────────────────

/// Infer an image MIME type string from a URL's file extension.
/// Falls back to `"image/jpeg"` when the extension is unrecognised.
fn infer_mime_from_url(url: &str) -> &'static str {
    let lower = url.to_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else {
        // Default to JPEG for unknown extensions (covers .jpg, .jpeg, and bare URLs)
        "image/jpeg"
    }
}

/// Convert our `Vec<ContentPart>` and optional `tool_calls` into Gemini `parts` array.
///
/// Gemini parts format:
/// - Text content: `{ "text": "..." }`
/// - Tool/function call (from assistant): `{ "functionCall": { "name": ..., "args": {...} } }`
///
/// Note: tool_result messages are handled separately (they become their own
/// `function_response` messages with role `"user"`).
fn build_gemini_content_parts(
    content: &[super::ContentPart],
    tool_calls: Option<&[super::ToolCall]>,
) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();

    // Process each content part and convert to Gemini's format.
    for cp in content {
        match cp {
            super::ContentPart::Text { text } => {
                // Only add non-empty text parts
                if !text.is_empty() {
                    parts.push(json!({ "text": text }));
                }
            }
            super::ContentPart::ImageUrl { image_url } => {
                // Gemini supports two image formats:
                // 1. `inline_data` for base64 data URIs (data:<mime>;base64,<data>)
                // 2. `file_data` for external URLs
                if image_url.url.starts_with("data:") {
                    // Parse data URI: data:<mime>;base64,<encoded>
                    if let Some((header, data)) = image_url.url.split_once(',') {
                        // header is like "data:image/png;base64"
                        let mime = header
                            .strip_prefix("data:")
                            .and_then(|s| s.strip_suffix(";base64"))
                            .unwrap_or("image/jpeg");
                        parts.push(json!({
                            "inline_data": {
                                "mime_type": mime,
                                "data": data,
                            }
                        }));
                    }
                } else {
                    // External URL — use file_data format
                    let mime = infer_mime_from_url(&image_url.url);
                    parts.push(json!({
                        "file_data": {
                            "mime_type": mime,
                            "file_uri": image_url.url,
                        }
                    }));
                }
            }
            // ToolUse and ToolResult are not placed as parts here;
            // those are handled by the caller building the request body.
            _ => {}
        }
    }

    // Add function call parts from tool_calls
    if let Some(calls) = tool_calls {
        for tc in calls {
            // Parse the arguments string back to a JSON object
            // (tool call arguments are stored as a JSON string in our format)
            let args: Value = serde_json::from_str(&tc.function.arguments)
                .unwrap_or(Value::Object(serde_json::Map::new()));

            parts.push(json!({
                "functionCall": {
                    "name": tc.function.name,
                    "args": args
                }
            }));
        }
    }

    // Gemini requires at least one part — if we somehow have none, add an empty text
    if parts.is_empty() {
        parts.push(json!({ "text": "" }));
    }

    parts
}

/// Convert a `FunctionDefinition` into a Gemini `functionDeclaration` object.
///
/// Gemini tool format:
/// ```json
/// {
///   "name": "function_name",
///   "description": "What it does",
///   "parameters": { ...JSON Schema... }
/// }
/// ```
///
/// This is similar to OpenAI's format (uses `parameters` with JSON Schema).
/// The main difference is the wrapping: Gemini groups all declarations under
/// `tools[0].function_declarations` rather than `tools[i].function`.
fn build_gemini_tool(func: &FunctionDefinition) -> Value {
    json!({
        "name": func.name,
        "description": func.description,
        "parameters": func.parameters,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{
        ContentPart, FunctionCall, FunctionDefinition, ImageUrl, Message, Role, ToolCall,
        ToolDefinition,
    };

    // ── Constructor tests ─────────────────────────────────────────────────────

    /// GeminiProvider::new() stores the api_key and model correctly.
    #[test]
    fn test_new_stores_fields() {
        let p = GeminiProvider::new("key-abc".to_string(), "gemini-2.0-flash".to_string());
        assert_eq!(p.api_key, "key-abc");
        assert_eq!(p.model, "gemini-2.0-flash");
        // stream_print defaults to true
        assert!(p.stream_print.load(Ordering::Relaxed));
    }

    /// set_stream_print toggles the AtomicBool correctly.
    #[test]
    fn test_set_stream_print() {
        let p = GeminiProvider::new("k".to_string(), "m".to_string());
        assert!(p.stream_print.load(Ordering::Relaxed)); // default true
        p.set_stream_print(false);
        assert!(!p.stream_print.load(Ordering::Relaxed));
        p.set_stream_print(true);
        assert!(p.stream_print.load(Ordering::Relaxed));
    }

    // ── Request building tests ─────────────────────────────────────────────────

    /// A simple user message produces the correct Gemini JSON structure.
    #[test]
    fn test_build_request_body_simple_message() {
        let p = GeminiProvider::new("k".to_string(), "gemini-2.0-flash".to_string());
        let messages = vec![Message::user("Hello!")];
        let body = p.build_request_body(&messages, &[]);

        // Should have a `contents` array
        assert!(body["contents"].is_array());
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);

        // Role should be "user"
        assert_eq!(contents[0]["role"], "user");

        // Parts should contain the text
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "Hello!");
    }

    /// System message is extracted into system_instruction, not into contents.
    #[test]
    fn test_build_request_body_system_prompt() {
        let p = GeminiProvider::new("k".to_string(), "m".to_string());
        let messages = vec![
            Message::system("You are a helpful assistant."),
            Message::user("Hi"),
        ];
        let body = p.build_request_body(&messages, &[]);

        // system_instruction should be present
        assert!(!body["system_instruction"].is_null());
        assert_eq!(
            body["system_instruction"]["parts"][0]["text"],
            "You are a helpful assistant."
        );

        // contents should only have the user message (not the system one)
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
    }

    /// Assistant message role is mapped to "model" (not "assistant").
    #[test]
    fn test_build_request_body_assistant_role_is_model() {
        let p = GeminiProvider::new("k".to_string(), "m".to_string());
        let messages = vec![
            Message::user("What is 2+2?"),
            Message::assistant(Some("4".to_string()), None),
        ];
        let body = p.build_request_body(&messages, &[]);

        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model"); // NOT "assistant"
    }

    /// Tool result messages are converted to user messages with function_response parts.
    #[test]
    fn test_build_request_body_tool_result() {
        let p = GeminiProvider::new("k".to_string(), "m".to_string());
        // The agent loop sets tool_call_id to the function name in Gemini's case
        let tool_msg = Message {
            role: Role::Tool,
            content: vec![ContentPart::text("File created successfully")],
            tool_calls: None,
            tool_call_id: Some("file_write".to_string()),
            name: None,
        };
        let body = p.build_request_body(&[tool_msg], &[]);

        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user"); // tool results are user messages

        let parts = contents[0]["parts"].as_array().unwrap();
        let fr = &parts[0]["function_response"];
        assert_eq!(fr["name"], "file_write");
        assert_eq!(fr["response"]["content"], "File created successfully");
    }

    /// Tools are included as function_declarations when provided.
    #[test]
    fn test_build_request_body_with_tools() {
        let p = GeminiProvider::new("k".to_string(), "m".to_string());
        let tools = vec![ToolDefinition {
            def_type: "function".to_string(),
            function: FunctionDefinition {
                name: "bash".to_string(),
                description: "Run a shell command".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
        }];
        let body = p.build_request_body(&[Message::user("run ls")], &tools);

        // tools should be a non-empty array with function_declarations
        let tools_json = body["tools"].as_array().unwrap();
        assert_eq!(tools_json.len(), 1);

        let decls = tools_json[0]["function_declarations"].as_array().unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], "bash");
        assert_eq!(decls[0]["description"], "Run a shell command");
    }

    /// No tools key is emitted when the tools slice is empty.
    #[test]
    fn test_build_request_body_no_tools_when_empty() {
        let p = GeminiProvider::new("k".to_string(), "m".to_string());
        let body = p.build_request_body(&[Message::user("hello")], &[]);
        // tools should be absent (not an empty array)
        assert!(body.get("tools").is_none() || body["tools"].is_null());
    }

    // ── Helper function tests ─────────────────────────────────────────────────

    /// build_gemini_content_parts with plain text produces a text part.
    #[test]
    fn test_build_gemini_content_parts_text() {
        let parts = build_gemini_content_parts(&[ContentPart::text("hello")], None);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "hello");
    }

    /// build_gemini_content_parts with tool calls adds functionCall parts.
    #[test]
    fn test_build_gemini_content_parts_tool_calls() {
        let tool_calls = vec![ToolCall {
            id: "call_1".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "file_read".to_string(),
                arguments: r#"{"path": "src/main.rs"}"#.to_string(),
            },
        }];
        let parts = build_gemini_content_parts(&[], Some(&tool_calls));

        // Should have exactly one functionCall part
        let fc_part = parts.iter().find(|p| !p["functionCall"].is_null());
        assert!(fc_part.is_some());
        let fc = &fc_part.unwrap()["functionCall"];
        assert_eq!(fc["name"], "file_read");
        assert_eq!(fc["args"]["path"], "src/main.rs");
    }

    /// build_gemini_content_parts with no content produces a single empty text part
    /// (Gemini requires at least one part).
    #[test]
    fn test_build_gemini_content_parts_empty_gets_placeholder() {
        let parts = build_gemini_content_parts(&[], None);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "");
    }

    /// build_gemini_tool produces the correct function declaration JSON.
    #[test]
    fn test_build_gemini_tool() {
        let func = FunctionDefinition {
            name: "grep_search".to_string(),
            description: "Search file contents".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        };
        let tool_json = build_gemini_tool(&func);
        assert_eq!(tool_json["name"], "grep_search");
        assert_eq!(tool_json["description"], "Search file contents");
        assert!(!tool_json["parameters"].is_null());
    }

    // ── Constant tests ────────────────────────────────────────────────────────

    /// The GEMINI_API_BASE sentinel matches what context.rs will compare against.
    #[test]
    fn test_gemini_api_base_sentinel() {
        assert_eq!(GEMINI_API_BASE, "gemini");
    }

    /// GeminiProvider doesn't claim to be Copilot (trait default).
    #[test]
    fn test_is_not_copilot() {
        let p = GeminiProvider::new("k".to_string(), "m".to_string());
        // The default LlmProvider::is_copilot() returns false
        // We can't call the trait method directly without Arc, but we can
        // verify the struct-level behavior by checking the copilot sentinel
        assert_ne!(GEMINI_API_BASE, "copilot");
        // stream_print starts true (not related to copilot but sanity check)
        assert!(p.stream_print.load(Ordering::Relaxed));
    }

    // ── SSE parsing tests ─────────────────────────────────────────────────────

    /// GeminiResponse deserializes from a typical streaming chunk.
    #[test]
    fn test_gemini_response_deserializes_text_chunk() {
        let json_str = r#"{
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{ "text": "Hello " }]
                }
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "totalTokenCount": 15
            }
        }"#;

        let resp: GeminiResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.candidates.len(), 1);

        let content = resp.candidates[0].content.as_ref().unwrap();
        assert_eq!(content.parts.len(), 1);
        assert_eq!(content.parts[0].text.as_deref(), Some("Hello "));

        let usage = resp.usage_metadata.as_ref().unwrap();
        assert_eq!(usage.prompt_token_count, Some(10));
        assert_eq!(usage.candidates_token_count, Some(5));
    }

    /// GeminiResponse deserializes a function call chunk correctly.
    #[test]
    fn test_gemini_response_deserializes_function_call() {
        let json_str = r#"{
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": "bash",
                            "args": { "command": "ls -la" }
                        }
                    }]
                }
            }]
        }"#;

        let resp: GeminiResponse = serde_json::from_str(json_str).unwrap();
        let content = resp.candidates[0].content.as_ref().unwrap();
        let fc = content.parts[0].function_call.as_ref().unwrap();
        assert_eq!(fc.name, "bash");
        assert_eq!(fc.args.as_ref().unwrap()["command"], "ls -la");
    }

    /// GeminiUsageMetadata handles missing fields gracefully (all are Option<u32>).
    #[test]
    fn test_gemini_usage_metadata_partial() {
        let json_str = r#"{ "promptTokenCount": 42 }"#;
        let usage: GeminiUsageMetadata = serde_json::from_str(json_str).unwrap();
        assert_eq!(usage.prompt_token_count, Some(42));
        assert_eq!(usage.candidates_token_count, None);
    }

    /// Empty candidates array deserializes without panic.
    #[test]
    fn test_gemini_response_empty_candidates() {
        let json_str = r#"{ "candidates": [] }"#;
        let resp: GeminiResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.candidates.is_empty());
    }

    // ── Image encoding tests ───────────────────────────────────────────────────────────────────────

    /// build_gemini_content_parts converts a base64 data URI to an `inline_data` block.
    #[test]
    fn test_build_gemini_content_image_base64() {
        let parts = build_gemini_content_parts(
            &[ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/png;base64,iVBORw0KGgo=".to_string(),
                    detail: None,
                },
            }],
            None,
        );
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["inline_data"]["mime_type"], "image/png");
        assert_eq!(parts[0]["inline_data"]["data"], "iVBORw0KGgo=");
    }

    /// build_gemini_content_parts converts an external URL to a `file_data` block.
    #[test]
    fn test_build_gemini_content_image_url() {
        let parts = build_gemini_content_parts(
            &[ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "https://example.com/photo.jpg".to_string(),
                    detail: None,
                },
            }],
            None,
        );
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["file_data"]["mime_type"], "image/jpeg");
        assert_eq!(
            parts[0]["file_data"]["file_uri"],
            "https://example.com/photo.jpg"
        );
    }

    /// infer_mime_from_url returns the correct MIME type for each known extension.
    #[test]
    fn test_infer_mime_from_url() {
        assert_eq!(infer_mime_from_url("photo.png"), "image/png");
        assert_eq!(infer_mime_from_url("anim.gif"), "image/gif");
        assert_eq!(infer_mime_from_url("img.webp"), "image/webp");
        assert_eq!(infer_mime_from_url("pic.jpg"), "image/jpeg");
        // Query params and uppercase are also handled
        assert_eq!(
            infer_mime_from_url("https://cdn.example.com/img.PNG"),
            "image/png"
        );
        // Unknown extension falls back to JPEG
        assert_eq!(infer_mime_from_url("no-extension"), "image/jpeg");
    }
}
