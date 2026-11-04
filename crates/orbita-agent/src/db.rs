//! SQLite persistence: dialogue messages, reminders and audit.
//!
//! One process owns the database. WAL and a busy timeout are enough for the
//! single-writer model; the agent does not share the file with other services.

use std::{path::Path, sync::Mutex};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub device_id: Option<String>,
    pub due_at: DateTime<Utc>,
    pub status: String,
    pub attempts: u32,
}

pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_session ON messages(session_id, id);

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    text TEXT NOT NULL,
    device_id TEXT,
    due_at TEXT NOT NULL,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS tasks_due ON tasks(status, due_at);

CREATE TABLE IF NOT EXISTS audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL,
    kind TEXT NOT NULL,
    detail TEXT NOT NULL
);
"#;

impl Store {
    pub fn open(path: &str) -> Result<Self, String> {
        if path != ":memory:"
            && let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("creating {}: {error}", parent.display()))?;
        }
        let conn = Connection::open(path).map_err(|error| format!("opening {path}: {error}"))?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| format!("busy_timeout: {error}"))?;
        conn.execute_batch(SCHEMA)
            .map_err(|error| format!("creating schema: {error}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, role, content, now],
        )
        .map_err(|error| format!("inserting message: {error}"))?;
        Ok(())
    }

    /// Last `limit` messages in chronological order.
    pub fn messages(&self, session_id: &str, limit: usize) -> Result<Vec<Message>, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = conn
            .prepare(
                "SELECT role, content FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2",
            )
            .map_err(|error| format!("preparing messages: {error}"))?;
        let rows = statement
            .query_map(params![session_id, limit as i64], |row| {
                Ok(Message {
                    role: row.get(0)?,
                    content: row.get(1)?,
                })
            })
            .map_err(|error| format!("querying messages: {error}"))?;
        let mut messages = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("reading messages: {error}"))?;
        messages.reverse();
        Ok(messages)
    }

    pub fn insert_task(
        &self,
        kind: &str,
        text: &str,
        device_id: Option<&str>,
        due_at: DateTime<Utc>,
    ) -> Result<String, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO tasks (id, kind, text, device_id, due_at, status, attempts, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6, ?6)",
            params![id, kind, text, device_id, due_at.to_rfc3339(), now],
        )
        .map_err(|error| format!("inserting task: {error}"))?;
        Ok(id)
    }

    pub fn due_tasks(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<Task>, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut statement = conn
            .prepare(
                "SELECT id, kind, text, device_id, due_at, status, attempts FROM tasks \
                 WHERE status = 'pending' AND due_at <= ?1 ORDER BY due_at LIMIT ?2",
            )
            .map_err(|error| format!("preparing due tasks: {error}"))?;
        let rows = statement
            .query_map(params![now.to_rfc3339(), limit as i64], |row| {
                let due_at: String = row.get(4)?;
                Ok(Task {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    text: row.get(2)?,
                    device_id: row.get(3)?,
                    due_at: DateTime::parse_from_rfc3339(&due_at)
                        .map(|value| value.with_timezone(&Utc))
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                4,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                    status: row.get(5)?,
                    attempts: row.get(6)?,
                })
            })
            .map_err(|error| format!("querying due tasks: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("reading due tasks: {error}"))
    }

    pub fn claim_task(&self, id: &str) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let changed = conn
            .execute(
                "UPDATE tasks SET status = 'running', updated_at = ?1 WHERE id = ?2 AND status = 'pending'",
                params![now, id],
            )
            .map_err(|error| format!("claiming task: {error}"))?;
        Ok(changed == 1)
    }

    pub fn finish_task(&self, id: &str, status: &str) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, now, id],
        )
        .map_err(|error| format!("finishing task: {error}"))?;
        Ok(())
    }

    pub fn retry_task(
        &self,
        id: &str,
        next_due: DateTime<Utc>,
        attempts: u32,
    ) -> Result<(), String> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE tasks SET status = 'pending', due_at = ?1, attempts = ?2, updated_at = ?3 WHERE id = ?4",
            params![next_due.to_rfc3339(), attempts, now, id],
        )
        .map_err(|error| format!("retrying task: {error}"))?;
        Ok(())
    }

    /// A crash can leave tasks in `running`; put them back into `pending`.
    pub fn recover_running(&self, now: DateTime<Utc>) -> Result<usize, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "UPDATE tasks SET status = 'pending', due_at = ?1, updated_at = ?1 WHERE status = 'running'",
            params![now.to_rfc3339()],
        )
        .map_err(|error| format!("recovering running tasks: {error}"))
    }

    pub fn audit(&self, kind: &str, detail: &str) -> Result<(), String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO audit (ts, kind, detail) VALUES (?1, ?2, ?3)",
            params![Utc::now().to_rfc3339(), kind, detail],
        )
        .map_err(|error| format!("writing audit: {error}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn store() -> Store {
        Store::open(":memory:").unwrap()
    }

    #[test]
    fn messages_round_trip_newest_last() {
        let store = store();
        store.append_message("voice", "user", "привет").unwrap();
        store
            .append_message("voice", "assistant", "здравствуй")
            .unwrap();
        store.append_message("other", "user", "чужое").unwrap();
        let messages = store.messages("voice", 10).unwrap();
        assert_eq!(
            messages,
            vec![
                Message {
                    role: "user".to_owned(),
                    content: "привет".to_owned()
                },
                Message {
                    role: "assistant".to_owned(),
                    content: "здравствуй".to_owned()
                }
            ]
        );
        assert_eq!(store.messages("voice", 1).unwrap().len(), 1);
    }

    #[test]
    fn due_tasks_are_claimed_once() {
        let store = store();
        let now = Utc::now();
        let id = store
            .insert_task("reminder", "чай", Some("duid"), now - Duration::seconds(1))
            .unwrap();
        let due = store.due_tasks(now, 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, id);
        assert!(store.claim_task(&id).unwrap());
        assert!(!store.claim_task(&id).unwrap());
        assert!(store.due_tasks(now, 10).unwrap().is_empty());
        store.finish_task(&id, "sent").unwrap();
    }

    #[test]
    fn running_tasks_recover() {
        let store = store();
        let now = Utc::now();
        let id = store.insert_task("reminder", "чай", None, now).unwrap();
        store.claim_task(&id).unwrap();
        assert_eq!(store.recover_running(now).unwrap(), 1);
        assert_eq!(store.due_tasks(now, 10).unwrap().len(), 1);
    }

    #[test]
    fn retry_moves_task_into_future() {
        let store = store();
        let now = Utc::now();
        let id = store
            .insert_task("reminder", "чай", None, now - Duration::seconds(1))
            .unwrap();
        store
            .retry_task(&id, now + Duration::seconds(30), 1)
            .unwrap();
        assert!(store.due_tasks(now, 10).unwrap().is_empty());
        assert_eq!(
            store
                .due_tasks(now + Duration::seconds(31), 10)
                .unwrap()
                .len(),
            1
        );
    }
}
