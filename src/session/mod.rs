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
    if trimmed.len() <= 50 {
        return trimmed.to_string();
    }
    let cut = &trimmed[..50];
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
}
