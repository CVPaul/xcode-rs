use crate::llm::{Message, Role};
use crate::session::{Session, StoredMessage};
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub struct SessionStore {
    pub(crate) conn: Connection,
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
            CREATE TABLE IF NOT EXISTS undo_history (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                stash_ref TEXT NOT NULL,
                description TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );
            CREATE INDEX IF NOT EXISTS idx_undo_session ON undo_history(session_id, created_at);
            ",
        )?;
        // Idempotent migrations: add token columns if they don't exist yet.
        // SQLite doesn't support ALTER TABLE ADD COLUMN IF NOT EXISTS, so we ignore
        // any error (which means the column already exists).
        let _ = self.conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN prompt_tokens INTEGER NOT NULL DEFAULT 0;",
        );
        let _ = self.conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN completion_tokens INTEGER NOT NULL DEFAULT 0;",
        );
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
        let mut stmt = self
            .conn
            .prepare("SELECT id, title, created_at, updated_at FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            let created_str: String = row.get(2)?;
            let updated_str: String = row.get(3)?;
            Ok(Some(Session {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&created_str)?.with_timezone(&Utc),
                updated_at: chrono::DateTime::parse_from_rfc3339(&updated_str)?.with_timezone(&Utc),
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
                created_at: chrono::DateTime::parse_from_rfc3339(&created_str)?.with_timezone(&Utc),
                updated_at: chrono::DateTime::parse_from_rfc3339(&updated_str)?.with_timezone(&Utc),
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
            .map(serde_json::to_string)
            .transpose()?;
        // Serialize content for storage.
        // - Empty Vec (tool-call-only messages) → NULL
        // - Single Text part → plain string (backwards-compatible with old rows)
        // - Multiple parts or non-Text parts → JSON array string
        let content_str: Option<String> = if msg.content.is_empty() {
            None
        } else if msg.content.len() == 1 {
            if let crate::llm::ContentPart::Text { text } = &msg.content[0] {
                Some(text.clone())
            } else {
                Some(serde_json::to_string(&msg.content)?)
            }
        } else {
            Some(serde_json::to_string(&msg.content)?)
        };
        self.conn.execute(
            "INSERT INTO messages (id, session_id, role, content, tool_calls, tool_call_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, session_id, role, content_str, tool_calls_json, msg.tool_call_id, now_str],
        )?;
        Ok(StoredMessage {
            id,
            session_id: session_id.to_string(),
            role: role.to_string(),
            content: content_str,
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
                created_at: chrono::DateTime::parse_from_rfc3339(&created_str)?.with_timezone(&Utc),
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

    /// Add `prompt` and `completion` tokens to the running totals for a session.
    ///
    /// This is called at the end of each Act-mode task run in the REPL so that
    /// cumulative usage is stored even after the process restarts.
    /// The UPDATE is additive (`+= delta`) rather than absolute so multiple runs
    /// accumulate correctly without needing to read the current total first.
    pub fn update_session_tokens(&self, id: &str, prompt: u32, completion: u32) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET prompt_tokens = prompt_tokens + ?1, \
             completion_tokens = completion_tokens + ?2 WHERE id = ?3",
            params![prompt, completion, id],
        )?;
        Ok(())
    }

    /// Delete a session and all its messages.
    ///
    /// Messages are deleted first to satisfy the foreign-key constraint
    /// (SQLite doesn't enforce FK by default, but we do it explicitly for
    /// correctness in case FK enforcement is ever enabled).
    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM messages WHERE session_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Return the default database path: ~/.local/share/xcode/sessions.db
    pub fn default_path() -> Result<PathBuf> {
        let base = dirs::data_local_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine local data directory"))?;
        Ok(base.join("xcode").join("sessions.db"))
    }

    // ─── Undo History Methods ──────────────────────────────────────────────────
    //
    // Each Act-mode run creates an entry in `undo_history`.  The entry holds a
    // reference to the git stash created just before the run (stash_ref is the
    // unique message label used in `git stash push -m <stash_ref>`).
    //
    // We keep at most MAX_UNDO_HISTORY entries per session.

    /// Push a new undo entry onto the history for the given session.
    ///
    /// `stash_ref` is the unique label passed to `git stash push -m <stash_ref>` so
    /// we can later identify the right stash entry with `git stash list`.
    /// `description` is a short human-readable description (the user's task prompt
    /// truncated to 80 chars) shown in `/undo list`.
    pub fn push_undo(
        &self,
        session_id: &str,
        stash_ref: &str,
        description: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO undo_history (id, session_id, stash_ref, description, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, session_id, stash_ref, description, now],
        )?;
        Ok(id)
    }

    /// Pop the most recent undo entry for the given session.
    ///
    /// Returns `Ok(Some(entry))` if there was an entry, `Ok(None)` if the history
    /// is empty.  The entry is REMOVED from the DB so a subsequent `/undo` will
    /// pop the next-older entry.
    pub fn pop_undo(&self, session_id: &str) -> Result<Option<UndoEntry>> {
        // Find the most recent entry.
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, stash_ref, description, created_at \
             FROM undo_history WHERE session_id = ?1 ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![session_id])?;
        let entry = if let Some(row) = rows.next()? {
            let created_str: String = row.get(4)?;
            Some(UndoEntry {
                id: row.get(0)?,
                session_id: row.get(1)?,
                stash_ref: row.get(2)?,
                description: row.get(3)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&created_str)?.with_timezone(&Utc),
            })
        } else {
            None
        };
        // Remove the entry AFTER we've read it so we return it to the caller.
        if let Some(ref e) = entry {
            self.conn
                .execute("DELETE FROM undo_history WHERE id = ?1", params![e.id])?;
        }
        Ok(entry)
    }

    /// Return all undo entries for the given session, newest first.
    ///
    /// Does NOT remove them — this is a read-only peek for `/undo list`.
    pub fn list_undo(&self, session_id: &str) -> Result<Vec<UndoEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, stash_ref, description, created_at \
             FROM undo_history WHERE session_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (id, session_id, stash_ref, description, created_str) = row?;
            entries.push(UndoEntry {
                id,
                session_id,
                stash_ref,
                description,
                created_at: chrono::DateTime::parse_from_rfc3339(&created_str)?.with_timezone(&Utc),
            });
        }
        Ok(entries)
    }

    /// Trim undo history for a session to at most `max` entries.
    ///
    /// Deletes the oldest entries beyond the limit.  Called after every `push_undo`
    /// to keep the table from growing unboundedly.
    pub fn trim_undo_history(&self, session_id: &str, max: usize) -> Result<()> {
        // We select the IDs to keep (newest `max` entries) and delete everything else.
        self.conn.execute(
            "DELETE FROM undo_history \
             WHERE session_id = ?1 \
             AND id NOT IN (\
               SELECT id FROM undo_history \
               WHERE session_id = ?1 \
               ORDER BY created_at DESC LIMIT ?2\
             )",
            params![session_id, max as i64],
        )?;
        Ok(())
    }
}

// ─── Undo History types ───────────────────────────────────────────────────────

/// Maximum number of undo entries stored per session.
pub const MAX_UNDO_HISTORY: usize = 10;

/// A single entry in the undo history for a session.
#[derive(Debug, Clone)]
pub struct UndoEntry {
    /// Unique entry ID (UUID).
    pub id: String,
    /// The session this entry belongs to.
    #[allow(dead_code)]
    pub session_id: String,
    /// The `-m` label used in `git stash push -m <stash_ref>`.
    /// We construct it as `xcodeai-undo-<UUID>` so it's unique and searchable.
    pub stash_ref: String,
    /// Human-readable description of what the agent was doing (truncated user message).
    pub description: Option<String>,
    /// When this entry was created.
    pub created_at: chrono::DateTime<chrono::Utc>,
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

        store
            .update_session_title(&session.id, "New Title")
            .unwrap();
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

    #[test]
    fn test_update_session_tokens() {
        let (_tmp, store) = temp_store();
        let session = store.create_session(Some("Token Test")).unwrap();

        // First run: 1000 prompt + 500 completion.
        store.update_session_tokens(&session.id, 1000, 500).unwrap();
        // Second run: 200 prompt + 100 completion — should accumulate.
        store.update_session_tokens(&session.id, 200, 100).unwrap();

        // Verify via a direct SQL query (conn is pub(crate)).
        let (p, c): (u32, u32) = store
            .conn
            .query_row(
                "SELECT prompt_tokens, completion_tokens FROM sessions WHERE id = ?1",
                params![session.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(p, 1200, "prompt_tokens should accumulate to 1200");
        assert_eq!(c, 600, "completion_tokens should accumulate to 600");
    }

    // ─── Undo History Tests ────────────────────────────────────────────────────

    #[test]
    fn test_push_and_pop_undo() {
        let (_tmp, store) = temp_store();
        let session = store.create_session(Some("Undo Test")).unwrap();

        // Nothing to pop initially.
        let empty = store.pop_undo(&session.id).unwrap();
        assert!(empty.is_none());

        // Push two entries.
        store
            .push_undo(&session.id, "xcodeai-undo-aaa", "First task")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store
            .push_undo(&session.id, "xcodeai-undo-bbb", "Second task")
            .unwrap();

        // Pop gives newest first.
        let e = store.pop_undo(&session.id).unwrap().unwrap();
        assert_eq!(e.stash_ref, "xcodeai-undo-bbb");
        assert_eq!(e.description.as_deref(), Some("Second task"));

        let e2 = store.pop_undo(&session.id).unwrap().unwrap();
        assert_eq!(e2.stash_ref, "xcodeai-undo-aaa");

        // Now empty again.
        assert!(store.pop_undo(&session.id).unwrap().is_none());
    }

    #[test]
    fn test_list_undo_does_not_remove() {
        let (_tmp, store) = temp_store();
        let session = store.create_session(None).unwrap();

        store.push_undo(&session.id, "ref-1", "task 1").unwrap();
        store.push_undo(&session.id, "ref-2", "task 2").unwrap();

        // list_undo should not consume entries.
        let list = store.list_undo(&session.id).unwrap();
        assert_eq!(list.len(), 2);
        // Newest first.
        assert_eq!(list[0].stash_ref, "ref-2");
        assert_eq!(list[1].stash_ref, "ref-1");

        // Pop still works after list.
        let e = store.pop_undo(&session.id).unwrap().unwrap();
        assert_eq!(e.stash_ref, "ref-2");
    }

    #[test]
    fn test_trim_undo_history() {
        let (_tmp, store) = temp_store();
        let session = store.create_session(None).unwrap();

        // Push 5 entries.
        for i in 0..5 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            store
                .push_undo(&session.id, &format!("ref-{}", i), &format!("task {}", i))
                .unwrap();
        }

        // Trim to max 3 — should keep the 3 newest.
        store.trim_undo_history(&session.id, 3).unwrap();
        let list = store.list_undo(&session.id).unwrap();
        assert_eq!(list.len(), 3);
        // Newest first → ref-4, ref-3, ref-2.
        assert_eq!(list[0].stash_ref, "ref-4");
        assert_eq!(list[1].stash_ref, "ref-3");
        assert_eq!(list[2].stash_ref, "ref-2");
    }
}
