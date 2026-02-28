use crate::llm::{Message, Role, ToolCall};
use crate::session::{Session, StoredMessage};
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub struct SessionStore {
    conn: Connection,
}

impl SessionStore {
    /// Open (or create) the SQLite database at db_path and run schema migrations.
    pub fn new(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open database: {}", db_path.display()))?;
        let store = SessionStore { conn };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_calls TEXT,
                tool_call_id TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
            ",
        )?;
        Ok(())
    }

    pub fn create_session(&self, title: Option<&str>) -> Result<Session> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.conn.execute(
            "INSERT INTO sessions (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, title, now_str, now_str],
        )?;
        Ok(Session {
            id,
            title: title.map(|s| s.to_string()),
            created_at: now,
            updated_at: now,
        })
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, created_at, updated_at FROM sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            let created_str: String = row.get(2)?;
            let updated_str: String = row.get(3)?;
            Ok(Some(Session {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&created_str)?
                    .with_timezone(&Utc),
                updated_at: chrono::DateTime::parse_from_rfc3339(&updated_str)?
                    .with_timezone(&Utc),
            }))
        } else {
            Ok(None)
        }
    }

    pub fn list_sessions(&self, limit: u32) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, created_at, updated_at FROM sessions ORDER BY updated_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut sessions = Vec::new();
        for row in rows {
            let (id, title, created_str, updated_str) = row?;
            sessions.push(Session {
                id,
                title,
                created_at: chrono::DateTime::parse_from_rfc3339(&created_str)?
                    .with_timezone(&Utc),
                updated_at: chrono::DateTime::parse_from_rfc3339(&updated_str)?
                    .with_timezone(&Utc),
            });
        }
        Ok(sessions)
    }

    pub fn add_message(&self, session_id: &str, msg: &Message) -> Result<StoredMessage> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let role = match msg.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let tool_calls_json: Option<String> = msg
            .tool_calls
            .as_ref()
            .map(|tc| serde_json::to_string(tc))
            .transpose()?;
        self.conn.execute(
            "INSERT INTO messages (id, session_id, role, content, tool_calls, tool_call_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, session_id, role, msg.content, tool_calls_json, msg.tool_call_id, now_str],
        )?;
        Ok(StoredMessage {
            id,
            session_id: session_id.to_string(),
            role: role.to_string(),
            content: msg.content.clone(),
            tool_calls: tool_calls_json,
            tool_call_id: msg.tool_call_id.clone(),
            created_at: now,
        })
    }

    pub fn get_messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, role, content, tool_calls, tool_call_id, created_at FROM messages WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        let mut messages = Vec::new();
        for row in rows {
            let (id, session_id, role, content, tool_calls, tool_call_id, created_str) = row?;
            messages.push(StoredMessage {
                id,
                session_id,
                role,
                content,
                tool_calls,
                tool_call_id,
                created_at: chrono::DateTime::parse_from_rfc3339(&created_str)?
                    .with_timezone(&Utc),
            });
        }
        Ok(messages)
    }

    pub fn update_session_title(&self, id: &str, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?1 WHERE id = ?2",
            params![title, id],
        )?;
        Ok(())
    }

    pub fn update_session_timestamp(&self, id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    /// Return the default database path: ~/.local/share/xcode/sessions.db
    pub fn default_path() -> Result<PathBuf> {
        let base = dirs::data_local_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine local data directory"))?;
        Ok(base.join("xcode").join("sessions.db"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, Message, ToolCall};
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, SessionStore) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let store = SessionStore::new(&db_path).unwrap();
        (tmp, store)
    }

    #[test]
    fn test_session_create_and_get() {
        let (_tmp, store) = temp_store();
        let session = store.create_session(Some("My Task")).unwrap();
        assert!(!session.id.is_empty());
        assert_eq!(session.title.as_deref(), Some("My Task"));

        let retrieved = store.get_session(&session.id).unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.id, session.id);
        assert_eq!(retrieved.title.as_deref(), Some("My Task"));
    }

    #[test]
    fn test_session_not_found() {
        let (_tmp, store) = temp_store();
        let result = store.get_session("nonexistent-id").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_session_list_ordering() {
        let (_tmp, store) = temp_store();
        let s1 = store.create_session(Some("First")).unwrap();
        // Small sleep to ensure different timestamps
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _s2 = store.create_session(Some("Second")).unwrap();
        // Update first session to make it most recent
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.update_session_timestamp(&s1.id).unwrap();

        let sessions = store.list_sessions(10).unwrap();
        assert_eq!(sessions.len(), 2);
        // s1 updated last, should be first
        assert_eq!(sessions[0].id, s1.id);
    }

    #[test]
    fn test_message_all_roles() {
        let (_tmp, store) = temp_store();
        let session = store.create_session(None).unwrap();

        let msgs = vec![
            Message::system("You are a helpful assistant."),
            Message::user("Write hello.txt"),
            Message::assistant(Some("I'll write it now.".to_string()), None),
            Message::tool("call-1", "File written successfully"),
        ];

        for msg in &msgs {
            store.add_message(&session.id, msg).unwrap();
        }

        let stored = store.get_messages(&session.id).unwrap();
        assert_eq!(stored.len(), 4);
        assert_eq!(stored[0].role, "system");
        assert_eq!(stored[1].role, "user");
        assert_eq!(stored[2].role, "assistant");
        assert_eq!(stored[3].role, "tool");
        assert_eq!(stored[3].tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn test_tool_calls_roundtrip() {
        let (_tmp, store) = temp_store();
        let session = store.create_session(None).unwrap();

        let tool_calls = vec![ToolCall {
            id: "tc-123".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "file_write".to_string(),
                arguments: r#"{"path":"hello.txt","content":"Hello World"}"#.to_string(),
            },
        }];

        let msg = Message::assistant(None, Some(tool_calls.clone()));
        store.add_message(&session.id, &msg).unwrap();

        let stored = store.get_messages(&session.id).unwrap();
        assert_eq!(stored.len(), 1);
        let json = stored[0].tool_calls.as_ref().unwrap();
        let parsed: Vec<ToolCall> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "tc-123");
        assert_eq!(parsed[0].function.name, "file_write");
    }

    #[test]
    fn test_update_session_title() {
        let (_tmp, store) = temp_store();
        let session = store.create_session(None).unwrap();
        assert!(session.title.is_none());

        store.update_session_title(&session.id, "New Title").unwrap();
        let updated = store.get_session(&session.id).unwrap().unwrap();
        assert_eq!(updated.title.as_deref(), Some("New Title"));
    }

    #[test]
    fn test_session_create_no_title() {
        let (_tmp, store) = temp_store();
        let session = store.create_session(None).unwrap();
        assert!(session.title.is_none());
        let retrieved = store.get_session(&session.id).unwrap().unwrap();
        assert!(retrieved.title.is_none());
    }
}
