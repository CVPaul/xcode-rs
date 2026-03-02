pub mod anthropic;
pub mod gemini;
pub mod openai;
pub mod registry;
pub mod retry;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ─── ContentPart ─────────────────────────────────────────────────────────────

/// A single part of a message's content.
///
/// Messages can contain multiple parts to support multimodal content (text +
/// images) and structured tool interactions (Anthropic-style APIs).
///
/// # Serde representation
///
/// Each variant is serialized as a JSON object with a `"type"` discriminant:
///
/// ```json
/// // Text variant:
/// { "type": "text", "text": "Hello world" }
///
/// // ImageUrl variant:
/// { "type": "image_url", "image_url": { "url": "data:image/png;base64,..." } }
///
/// // ToolUse (Anthropic):
/// { "type": "tool_use", "id": "call_1", "name": "file_write", "input": {...} }
///
/// // ToolResult (Anthropic):
/// { "type": "tool_result", "tool_use_id": "call_1", "content": "done", "is_error": false }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text content — the most common case.
    Text { text: String },
    /// Image content addressed by URL or base64 data URI.
    /// Used by OpenAI Vision API and Anthropic multimodal.
    ImageUrl { image_url: ImageUrl },
    /// Tool use request (Anthropic-style structured tool calling).
    /// OpenAI tool calls use `Message.tool_calls` instead.
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Tool result response (Anthropic-style).
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

impl ContentPart {
    /// Construct a plain `Text` part (shorthand).
    pub fn text(s: impl Into<String>) -> Self {
        ContentPart::Text { text: s.into() }
    }

    /// If this part is a `Text` variant, return its text. Otherwise `None`.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentPart::Text { text } => Some(text),
            _ => None,
        }
    }
}

/// Image URL/data struct used by `ContentPart::ImageUrl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageUrl {
    /// URL or base64 data URI: `"data:image/png;base64,..."`
    pub url: String,
    /// Optional detail level for OpenAI Vision: `"auto"`, `"low"`, `"high"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

// ─── Custom serde for Message.content ────────────────────────────────────────
//
// Goal: full backwards and forwards compatibility.
//
// Serialization rule (for OpenAI API):
//   - If the Vec has exactly ONE Text part → emit a plain JSON string.
//     This is what OpenAI expects for normal text messages and is what we
//     were sending before this refactor.
//   - Otherwise (multiple parts, or non-Text parts) → emit a JSON array of
//     ContentPart objects.  OpenAI Vision API accepts this format.
//
// Deserialization rule (backwards compat):
//   - If the JSON value is a plain string → wrap it in [Text { text }].
//   - If the JSON value is an array → deserialize as Vec<ContentPart>.
//   - If the JSON value is null / missing → produce an empty Vec.
//
// This means old session DB rows that stored `content: "hello"` still load
// correctly into the new `Vec<ContentPart>` representation.

fn serialize_content_parts<S>(parts: &Vec<ContentPart>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    // Single Text part → plain string (OpenAI API expects this for normal msgs)
    if parts.len() == 1 {
        if let ContentPart::Text { text } = &parts[0] {
            return s.serialize_str(text);
        }
    }
    // Multiple parts or non-text → serialize as array
    parts.serialize(s)
}

fn deserialize_content_parts<'de, D>(d: D) -> Result<Vec<ContentPart>, D::Error>
where
    D: Deserializer<'de>,
{
    // Accept string, array, or null.
    // We use an intermediate `serde_json::Value` to dispatch.
    let value = serde_json::Value::deserialize(d)?;
    match value {
        // null / undefined → empty (tool/assistant messages with only tool_calls)
        serde_json::Value::Null => Ok(vec![]),
        // Old format: plain string → single Text part
        serde_json::Value::String(s) => Ok(vec![ContentPart::Text { text: s }]),
        // New format: array of ContentPart objects
        serde_json::Value::Array(_) => {
            let parts: Vec<ContentPart> =
                serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(parts)
        }
        other => Err(serde::de::Error::custom(format!(
            "Expected string or array for message content, got: {}",
            other
        ))),
    }
}

// ─── Role ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

// ─── Message ─────────────────────────────────────────────────────────────────

/// A single message in the LLM conversation history.
///
/// `content` is now `Vec<ContentPart>` to support multimodal messages
/// (text + images) and Anthropic-style tool content blocks.
///
/// For the common single-text-message case the API still sees a plain string
/// thanks to the custom `serialize_content_parts` serializer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Message {
    pub role: Role,
    /// Message body — one or more content parts.
    /// Use `text_content()` to extract plain text.
    ///
    /// NOTE: serialization is handled by a custom serializer below.
    #[serde(
        default,
        deserialize_with = "deserialize_content_parts",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub content: Vec<ContentPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

// Custom Serialize for Message so we can apply the content serialization rule.
impl Serialize for Message {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        // Count how many fields we need.
        // role is always present.
        let mut count = 1; // role
        if !self.content.is_empty() {
            count += 1; // content
        }
        if self.tool_calls.is_some() {
            count += 1;
        }
        if self.tool_call_id.is_some() {
            count += 1;
        }
        if self.name.is_some() {
            count += 1;
        }

        let mut map = s.serialize_map(Some(count))?;
        map.serialize_entry("role", &self.role)?;
        if !self.content.is_empty() {
            // Use the smart serializer: string for single Text, array otherwise
            map.serialize_entry("content", &ContentPartsSerializer(&self.content))?;
        }
        if let Some(tc) = &self.tool_calls {
            map.serialize_entry("tool_calls", tc)?;
        }
        if let Some(id) = &self.tool_call_id {
            map.serialize_entry("tool_call_id", id)?;
        }
        if let Some(n) = &self.name {
            map.serialize_entry("name", n)?;
        }
        map.end()
    }
}

/// A newtype wrapper that applies the "string-if-single-text" serialization
/// rule for `Vec<ContentPart>`.
struct ContentPartsSerializer<'a>(&'a Vec<ContentPart>);

impl<'a> Serialize for ContentPartsSerializer<'a> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        serialize_content_parts(self.0, s)
    }
}

impl Message {
    // ── Constructors ─────────────────────────────────────────────────────────

    /// Create a system message with plain text content.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentPart::text(content)],
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Create a user message with plain text content.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentPart::text(content)],
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Create an assistant message.
    ///
    /// `content` is `Option<String>` here for backwards compatibility with
    /// callers that had `content: Option<String>`.  Pass `None` when the
    /// response contains only tool calls.
    pub fn assistant(content: Option<String>, tool_calls: Option<Vec<ToolCall>>) -> Self {
        Self {
            role: Role::Assistant,
            content: content
                .map(|s| vec![ContentPart::text(s)])
                .unwrap_or_default(),
            tool_calls,
            tool_call_id: None,
            name: None,
        }
    }

    /// Create a tool-result message.
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentPart::text(content)],
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: None,
        }
    }

    // ── Accessors ────────────────────────────────────────────────────────────

    /// Extract all plain text from this message's content parts.
    ///
    /// Concatenates every `ContentPart::Text` in order, joining with `\n`
    /// when there are multiple text parts.  Returns `None` if there are no
    /// text parts (e.g. a message containing only images or tool results).
    pub fn text_content(&self) -> Option<String> {
        let texts: Vec<&str> = self.content.iter().filter_map(|p| p.as_text()).collect();
        if texts.is_empty() {
            None
        } else {
            Some(texts.join("\n"))
        }
    }
}

// ─── ToolCall / FunctionCall ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

// ─── ToolDefinition ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub def_type: String,
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ─── Usage ────────────────────────────────────────────────────────────────────

/// Token usage statistics returned by the LLM API after a completion.
/// Used for cost tracking and context window management.
/// The API populates this only when `stream_options.include_usage` is set to `true`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    /// Number of tokens in the input (prompt + history + system).
    pub prompt_tokens: u32,
    /// Number of tokens generated by the model.
    pub completion_tokens: u32,
    /// Convenience total: prompt_tokens + completion_tokens.
    pub total_tokens: u32,
}

// ─── LlmResponse ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Token counts for this completion turn.
    /// `None` when the API did not return usage data (e.g. provider doesn't support it).
    pub usage: Option<Usage>,
}

// ─── LlmProvider trait ───────────────────────────────────────────────────────

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse>;

    // ── Provider management helpers ───────────────────────────────────────────
    //
    // These have default no-op implementations so that new providers only need
    // to implement `chat_completion`.  OpenAI/Copilot overrides them.

    /// Returns true when this provider is talking to GitHub Copilot.
    /// Used by the REPL to show auth status and guard /login.
    fn is_copilot(&self) -> bool {
        false
    }

    /// Enable / disable real-time streaming output to stdout.
    /// Called by Plan mode to suppress live streaming so the reply can be
    /// post-processed before display.  No-op for providers that don't stream.
    fn set_stream_print(&self, _enabled: bool) {}

    /// Update the long-lived OAuth token after /login completes.
    /// Only meaningful for the Copilot provider; all others do nothing.
    async fn set_copilot_oauth_token(&self, _token: String) {}
}

// ─── NullLlmProvider ─────────────────────────────────────────────────────────
//
// A no-op LLM provider for use in unit tests that construct ToolContext but
// don't actually invoke any LLM calls.  Returns an empty successful response.

/// No-op LLM provider for unit tests.
///
/// Use this when you need to build a `ToolContext` (which holds an
/// `Arc<dyn LlmProvider>`) in test code, but the test itself does not exercise
/// any LLM call path.  Any call to `chat_completion` returns an empty success.
#[allow(dead_code)]
pub struct NullLlmProvider;

#[async_trait]
impl LlmProvider for NullLlmProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        Ok(LlmResponse {
            content: Some(String::new()),
            tool_calls: None,
            usage: None,
        })
    }
}

// ─── Image loading helper ────────────────────────────────────────────────────

/// Load an image file from disk and return a [`ContentPart::ImageUrl`] carrying
/// a base64 data URI (`data:<mime>;base64,<encoded>`).
///
/// This is the primary way to add images to a multimodal message. The resulting
/// `ContentPart` can be appended to any `Message.content` vec and will be
/// serialised correctly by all three providers (OpenAI, Anthropic, Gemini).
///
/// # Supported formats
/// JPEG (`.jpg` / `.jpeg`), PNG (`.png`), GIF (`.gif`), WebP (`.webp`).
///
/// # Errors
/// - File not found or not readable.
/// - Extension absent or not one of the supported formats.
#[allow(dead_code)]
pub fn image_to_content_part(path: &std::path::Path) -> anyhow::Result<ContentPart> {
    use anyhow::Context as _;
    use std::io::Read as _;

    // Determine MIME type from file extension.
    // We do this before reading the file so we can fail fast on unsupported formats.
    let mime = match path.extension().and_then(|e| e.to_str()) {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some(ext) => anyhow::bail!("Unsupported image format: .{}", ext),
        None => anyhow::bail!("Cannot determine image format: no file extension"),
    };

    // Read the entire file into memory.
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("Cannot open image file: {}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("Cannot read image file: {}", path.display()))?;

    // Encode as a base64 data URI so it can be embedded directly in a JSON request.
    // All three providers (OpenAI, Anthropic, Gemini) accept this format.
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let data_uri = format!("data:{};base64,{}", mime, b64);

    Ok(ContentPart::ImageUrl {
        image_url: ImageUrl {
            url: data_uri,
            detail: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Usage tests ────────────────────────────────────────────────────────

    /// `Usage::default()` should produce all-zero counts.
    /// This verifies our `#[derive(Default)]` works correctly and
    /// that callers can safely use `Usage::default()` as a zero-value.
    #[test]
    fn test_usage_default_is_zero() {
        let u = Usage::default();
        assert_eq!(u.prompt_tokens, 0, "prompt_tokens should start at 0");
        assert_eq!(
            u.completion_tokens, 0,
            "completion_tokens should start at 0"
        );
        assert_eq!(u.total_tokens, 0, "total_tokens should start at 0");
    }

    /// Two `Usage` values with the same fields should be equal.
    /// This verifies `#[derive(PartialEq)]` works (needed for assert_eq! in tests).
    #[test]
    fn test_usage_equality() {
        let a = Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        };
        let b = Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        };
        assert_eq!(a, b);
    }

    /// `LlmResponse.usage` should be `None` when constructed without usage data.
    /// The openai.rs test helper (`parse_sse_chunks`) also sets `usage: None`
    /// to represent a provider that doesn't return usage stats.
    #[test]
    fn test_llm_response_usage_is_none_by_default() {
        let resp = LlmResponse {
            content: Some("hello".to_string()),
            tool_calls: None,
            usage: None,
        };
        assert!(
            resp.usage.is_none(),
            "usage should be None when not provided by API"
        );
    }

    /// `LlmResponse.usage` can hold a populated `Usage` struct.
    /// This verifies the field wiring is correct end-to-end.
    #[test]
    fn test_llm_response_usage_can_be_some() {
        let resp = LlmResponse {
            content: Some("answer".to_string()),
            tool_calls: None,
            usage: Some(Usage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
            }),
        };
        let u = resp.usage.unwrap();
        assert_eq!(u.prompt_tokens, 100);
        assert_eq!(u.completion_tokens, 50);
        assert_eq!(u.total_tokens, 150);
    }

    // ── ContentPart tests ──────────────────────────────────────────────────

    /// A `Text` ContentPart serializes to `{ "type": "text", "text": "..." }`.
    #[test]
    fn test_content_part_text_serde() {
        let part = ContentPart::text("hello world");
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"hello world\""));

        let back: ContentPart = serde_json::from_str(&json).unwrap();
        assert_eq!(part, back);
    }

    /// An `ImageUrl` ContentPart roundtrips correctly.
    #[test]
    fn test_content_part_image_url_serde() {
        let part = ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: "https://example.com/img.png".to_string(),
                detail: Some("auto".to_string()),
            },
        };
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("\"type\":\"image_url\""));
        let back: ContentPart = serde_json::from_str(&json).unwrap();
        assert_eq!(part, back);
    }

    // ── Message serialization tests ─────────────────────────────────────────

    /// A Message with a single Text part serializes content as a plain string.
    /// This is what the OpenAI API expects for normal text messages.
    #[test]
    fn test_message_single_text_serializes_as_string() {
        let msg = Message::user("Hello!");
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        // content should be a plain string, not an array
        assert_eq!(
            v["content"],
            serde_json::Value::String("Hello!".to_string())
        );
    }

    /// A Message with multiple parts serializes content as an array.
    #[test]
    fn test_message_multipart_serializes_as_array() {
        let msg = Message {
            role: Role::User,
            content: vec![
                ContentPart::text("describe this image"),
                ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "data:image/png;base64,abc".to_string(),
                        detail: None,
                    },
                },
            ],
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert!(
            v["content"].is_array(),
            "Expected array for multi-part content"
        );
        assert_eq!(v["content"].as_array().unwrap().len(), 2);
    }

    // ── Backwards compatibility tests ───────────────────────────────────────

    /// Old session DB rows that stored `content: "string"` still deserialize
    /// correctly into the new `Vec<ContentPart>` representation.
    #[test]
    fn test_backwards_compat_string_content() {
        let old_json = r#"{"role":"user","content":"Write a hello world program"}"#;
        let msg: Message = serde_json::from_str(old_json).unwrap();
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content.len(), 1);
        assert_eq!(
            msg.text_content(),
            Some("Write a hello world program".to_string())
        );
    }

    /// A message with `content: null` deserializes to an empty Vec.
    #[test]
    fn test_null_content_deserializes_to_empty_vec() {
        let json = r#"{"role":"assistant","content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"file_write","arguments":"{}"}}]}"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        assert!(msg.content.is_empty());
        assert!(msg.tool_calls.is_some());
    }

    /// Assistant message with text content roundtrips.
    #[test]
    fn test_assistant_message_roundtrip() {
        let msg = Message::assistant(Some("Here is the code.".to_string()), None);
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
        assert_eq!(back.text_content(), Some("Here is the code.".to_string()));
    }

    /// Tool message roundtrips.
    #[test]
    fn test_tool_message_roundtrip() {
        let msg = Message::tool("call_1", "File written successfully");
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    /// `text_content()` returns None for a message with no Text parts.
    #[test]
    fn test_text_content_no_text_parts() {
        let msg = Message {
            role: Role::User,
            content: vec![ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/png;base64,abc".to_string(),
                    detail: None,
                },
            }],
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        assert_eq!(msg.text_content(), None);
    }

    /// `text_content()` joins multiple Text parts with newline.
    #[test]
    fn test_text_content_multiple_parts() {
        let msg = Message {
            role: Role::User,
            content: vec![ContentPart::text("part one"), ContentPart::text("part two")],
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        assert_eq!(msg.text_content(), Some("part one\npart two".to_string()));
    }
    // ── image_to_content_part tests ─────────────────────────────────────────────

    /// Unsupported extension returns an error with a helpful message.
    #[test]
    fn test_image_to_content_part_unsupported_ext() {
        let result = image_to_content_part(std::path::Path::new("file.bmp"));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unsupported image format"));
    }

    /// No extension returns an error.
    #[test]
    fn test_image_to_content_part_no_ext() {
        let result = image_to_content_part(std::path::Path::new("noextension"));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Cannot determine image format"));
    }

    /// Missing file returns an error (even with valid extension).
    #[test]
    fn test_image_to_content_part_missing_file() {
        let result = image_to_content_part(std::path::Path::new("/nonexistent/path/img.png"));
        assert!(result.is_err());
    }

    /// Real PNG bytes on disk produce a correct `data:image/png;base64,...` ContentPart.
    #[test]
    fn test_image_to_content_part_from_disk() {
        // Write a minimal PNG header (8 bytes) to a temp file with .png extension.
        // Write a minimal PNG header (8 bytes) to a temp file with .png extension.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        // Minimal PNG magic bytes
        let png_bytes: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        std::fs::write(&path, png_bytes).unwrap();

        let part = image_to_content_part(&path).unwrap();
        match part {
            ContentPart::ImageUrl { image_url } => {
                assert!(
                    image_url.url.starts_with("data:image/png;base64,"),
                    "Expected data URI prefix, got: {}",
                    &image_url.url[..50.min(image_url.url.len())]
                );
                assert!(image_url.detail.is_none());
            }
            other => panic!("Expected ImageUrl variant, got {:?}", other),
        }
    }
}
