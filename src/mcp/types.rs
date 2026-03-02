// MCP (Model Context Protocol) type definitions.
//
// These types represent the protocol messages exchanged between xcodeai
// (as MCP *client*) and external MCP *servers* that provide additional
// tools and resources to the agent.
//
// MCP Reference: https://spec.modelcontextprotocol.io/specification/

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─── Tool types ───────────────────────────────────────────────────────────────

/// A tool advertised by an MCP server.
///
/// The MCP server lists its tools in response to a `tools/list` request.
/// The agent then decides which tools to call based on their names and
/// descriptions.
///
/// # JSON shape (from server)
/// ```json
/// {
///   "name": "read_file",
///   "description": "Read the contents of a file.",
///   "inputSchema": {
///     "type": "object",
///     "properties": { "path": { "type": "string" } },
///     "required": ["path"]
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDefinition {
    /// The tool's unique name within this MCP server.
    pub name: String,

    /// Human-readable description of what the tool does.
    /// The LLM uses this to decide whether to call the tool.
    pub description: Option<String>,

    /// JSON Schema describing the tool's input parameters.
    /// Follows the same JSON Schema format as OpenAI function parameters.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// The result returned by an MCP server after a `tools/call` request.
///
/// # JSON shape (from server)
/// ```json
/// {
///   "content": [
///     { "type": "text", "text": "Hello from MCP tool!" }
///   ],
///   "isError": false
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCallResult {
    /// One or more content blocks returned by the tool.
    /// Usually just a single Text block, but can include images or
    /// embedded resources.
    pub content: Vec<McpContent>,

    /// If true, the content describes an error that occurred during
    /// tool execution. The agent should treat this as a tool error,
    /// not a successful result.
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

/// A single content block within a tool result or resource read response.
///
/// MCP uses a tagged union: the `type` field determines which variant this is.
///
/// # JSON variants
/// - `{ "type": "text", "text": "..." }`
/// - `{ "type": "image", "data": "<base64>", "mimeType": "image/png" }`
/// - `{ "type": "resource", "resource": { ... } }`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpContent {
    /// Plain text content — the most common case.
    Text {
        /// The text content returned by the tool.
        text: String,
    },

    /// A base64-encoded image.
    Image {
        /// Base64-encoded image bytes.
        data: String,
        /// MIME type, e.g. `"image/png"`, `"image/jpeg"`.
        #[serde(rename = "mimeType")]
        mime_type: String,
    },

    /// An embedded MCP resource (the server is returning a resource
    /// inline rather than requiring a separate resources/read call).
    Resource {
        /// The embedded resource.
        resource: McpResource,
    },
}

// ─── Resource types ───────────────────────────────────────────────────────────

/// A resource exposed by an MCP server.
///
/// Resources are server-managed data items (files, database rows, API
/// responses, etc.) that the agent can read via `resources/read`.
///
/// # JSON shape (from server's resources/list response)
/// ```json
/// {
///   "uri": "file:///home/user/notes.txt",
///   "name": "notes.txt",
///   "description": "User's personal notes",
///   "mimeType": "text/plain"
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResource {
    /// Unique identifier for this resource, typically a URI.
    /// Used as the parameter to `resources/read`.
    pub uri: String,

    /// Human-readable display name.
    pub name: String,

    /// Optional description of the resource's contents.
    pub description: Option<String>,

    /// Optional MIME type hint (e.g. `"text/plain"`, `"application/json"`).
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
}

/// The content of a resource returned by `resources/read`.
///
/// # JSON shape (from server)
/// ```json
/// {
///   "uri": "file:///home/user/notes.txt",
///   "text": "Today I learned about MCP...",
///   "mimeType": "text/plain"
/// }
/// ```
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceContent {
    /// The URI of the resource (matches the request URI).
    pub uri: String,

    /// Text content of the resource.
    /// Present when mimeType is text-based.
    pub text: Option<String>,

    /// Base64-encoded binary content.
    /// Present when mimeType is binary (e.g. images).
    pub blob: Option<String>,

    /// MIME type of the content.
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
}

// ─── Protocol request/response wrappers ──────────────────────────────────────

/// Wrapper for the `tools/list` response body.
#[derive(Debug, Deserialize)]
pub struct McpToolsListResult {
    /// The list of tools this server provides.
    pub tools: Vec<McpToolDefinition>,
}

/// Wrapper for the `resources/list` response body.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct McpResourcesListResult {
    /// The list of resources this server exposes.
    pub resources: Vec<McpResource>,
}

/// Wrapper for the `resources/read` response body.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct McpResourceReadResult {
    /// One or more content blocks for the requested resource.
    /// A single resource can have multiple content blocks (e.g. a file
    /// split into pages).
    pub contents: Vec<McpResourceContent>,
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// McpToolDefinition round-trips through JSON correctly.
    #[test]
    fn test_tool_definition_roundtrip() {
        let json = json!({
            "name": "read_file",
            "description": "Read a file's contents",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }
        });

        let def: McpToolDefinition = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(def.name, "read_file");
        assert_eq!(def.description.as_deref(), Some("Read a file's contents"));

        // Re-serialize and check the name survives the round-trip
        let re = serde_json::to_value(&def).unwrap();
        assert_eq!(re["name"], "read_file");
    }

    /// McpToolDefinition with no description parses correctly.
    #[test]
    fn test_tool_definition_no_description() {
        let json = json!({
            "name": "ping",
            "inputSchema": { "type": "object" }
        });

        let def: McpToolDefinition = serde_json::from_value(json).unwrap();
        assert_eq!(def.name, "ping");
        assert!(def.description.is_none());
    }

    /// McpContent::Text deserializes from `{ "type": "text", "text": "..." }`.
    #[test]
    fn test_content_text_deserialize() {
        let json = json!({"type": "text", "text": "hello world"});
        let content: McpContent = serde_json::from_value(json).unwrap();
        match content {
            McpContent::Text { text } => assert_eq!(text, "hello world"),
            other => panic!("Expected Text, got {:?}", other),
        }
    }

    /// McpContent::Image deserializes correctly.
    #[test]
    fn test_content_image_deserialize() {
        let json = json!({
            "type": "image",
            "data": "aGVsbG8=",
            "mimeType": "image/png"
        });
        let content: McpContent = serde_json::from_value(json).unwrap();
        match content {
            McpContent::Image { data, mime_type } => {
                assert_eq!(data, "aGVsbG8=");
                assert_eq!(mime_type, "image/png");
            }
            other => panic!("Expected Image, got {:?}", other),
        }
    }

    /// McpToolCallResult with isError=false parses correctly.
    #[test]
    fn test_tool_call_result_success() {
        let json = json!({
            "content": [{"type": "text", "text": "done"}],
            "isError": false
        });
        let result: McpToolCallResult = serde_json::from_value(json).unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content.len(), 1);
    }

    /// McpToolCallResult with isError=true marks the error flag.
    #[test]
    fn test_tool_call_result_error() {
        let json = json!({
            "content": [{"type": "text", "text": "file not found"}],
            "isError": true
        });
        let result: McpToolCallResult = serde_json::from_value(json).unwrap();
        assert!(result.is_error);
    }

    /// McpToolCallResult with missing isError defaults to false.
    #[test]
    fn test_tool_call_result_default_not_error() {
        let json = json!({
            "content": [{"type": "text", "text": "ok"}]
        });
        let result: McpToolCallResult = serde_json::from_value(json).unwrap();
        assert!(!result.is_error, "isError should default to false");
    }

    /// McpResource round-trips through JSON.
    #[test]
    fn test_resource_roundtrip() {
        let json = json!({
            "uri": "file:///notes.txt",
            "name": "notes.txt",
            "description": "My notes",
            "mimeType": "text/plain"
        });

        let resource: McpResource = serde_json::from_value(json).unwrap();
        assert_eq!(resource.uri, "file:///notes.txt");
        assert_eq!(resource.name, "notes.txt");
        assert_eq!(resource.description.as_deref(), Some("My notes"));
        assert_eq!(resource.mime_type.as_deref(), Some("text/plain"));
    }

    /// McpResource with no optional fields parses correctly.
    #[test]
    fn test_resource_minimal() {
        let json = json!({
            "uri": "db://table/users",
            "name": "users"
        });

        let resource: McpResource = serde_json::from_value(json).unwrap();
        assert_eq!(resource.uri, "db://table/users");
        assert!(resource.description.is_none());
        assert!(resource.mime_type.is_none());
    }

    /// McpToolsListResult parses a real tools/list response structure.
    #[test]
    fn test_tools_list_result() {
        let json = json!({
            "tools": [
                {
                    "name": "tool_a",
                    "description": "Does A",
                    "inputSchema": { "type": "object" }
                },
                {
                    "name": "tool_b",
                    "inputSchema": { "type": "object" }
                }
            ]
        });

        let result: McpToolsListResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.tools.len(), 2);
        assert_eq!(result.tools[0].name, "tool_a");
        assert_eq!(result.tools[1].name, "tool_b");
    }

    /// McpResourcesListResult parses a real resources/list response.
    #[test]
    fn test_resources_list_result() {
        let json = json!({
            "resources": [
                { "uri": "file:///a.txt", "name": "a.txt" },
                { "uri": "file:///b.txt", "name": "b.txt" }
            ]
        });

        let result: McpResourcesListResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.resources.len(), 2);
    }

    /// McpResourceReadResult parses a resources/read response.
    #[test]
    fn test_resource_read_result() {
        let json = json!({
            "contents": [
                {
                    "uri": "file:///a.txt",
                    "text": "hello",
                    "mimeType": "text/plain"
                }
            ]
        });

        let result: McpResourceReadResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.contents.len(), 1);
        assert_eq!(result.contents[0].uri, "file:///a.txt");
        assert_eq!(result.contents[0].text.as_deref(), Some("hello"));
    }
}
