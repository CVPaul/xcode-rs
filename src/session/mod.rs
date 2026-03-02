pub mod store;
pub use store::SessionStore;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: Option<String>,
    pub tool_calls: Option<String>, // JSON-serialized Vec<ToolCall>
    pub tool_call_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Generate a short title from the first user message (≤50 chars, truncated at word boundary)
pub fn auto_title(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.chars().count() <= 50 {
        return trimmed.to_string();
    }
    // Find the byte index of the 50th character (safe for multi-byte CJK chars)
    let byte_end = trimmed.char_indices().nth(50).map(|(i, _)| i).unwrap_or(trimmed.len());
    let cut = &trimmed[..byte_end];
    if let Some(pos) = cut.rfind(' ') {
        trimmed[..pos].to_string()
    } else {
        cut.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_title_short() {
        assert_eq!(auto_title("hello"), "hello");
    }

    #[test]
    fn test_auto_title_truncates_at_word() {
        let msg = "implement a function to read files from the filesystem efficiently";
        let title = auto_title(msg);
        assert!(title.len() <= 50);
        assert!(!title.ends_with(' '));
    }

    #[test]
    fn test_auto_title_no_spaces() {
        let msg = "a".repeat(60);
        let title = auto_title(&msg);
        assert_eq!(title.len(), 50);
    }

    #[test]
    fn test_auto_title_cjk() {
        // This input caused a panic: byte index 50 is not a char boundary
        let msg = "我要在老家建房，需要一个设计软件，最好是多agent 可以出设计效果的那种";
        let title = auto_title(msg); // must not panic
        assert!(title.chars().count() <= 50);
        // Verify it doesn't slice mid-character
        assert!(title.is_char_boundary(title.len()));
    }

    #[test]
    fn test_auto_title_cjk_short() {
        let msg = "你好世界";
        assert_eq!(auto_title(msg), "你好世界");
    }
}
