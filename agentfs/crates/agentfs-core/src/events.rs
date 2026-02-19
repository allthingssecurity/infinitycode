use std::sync::Arc;

use crate::connection::pool::{ReaderPool, WriterHandle};
use crate::error::Result;

/// A unified event log entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Event {
    pub id: i64,
    pub session_id: Option<String>,
    pub event_type: String,
    pub path: Option<String>,
    pub detail: Option<String>,
    pub recorded_at: String,
}

/// Unified event logging.
pub struct Events {
    writer: Arc<WriterHandle>,
    readers: Arc<ReaderPool>,
}

impl Events {
    pub fn new(writer: Arc<WriterHandle>, readers: Arc<ReaderPool>) -> Self {
        Self { writer, readers }
    }

    /// Log an event. Returns the new event ID.
    pub async fn log(
        &self,
        session_id: Option<&str>,
        event_type: &str,
        path: Option<&str>,
        detail: Option<&str>,
    ) -> Result<i64> {
        let session_id = session_id.map(|s| s.to_string());
        let event_type = event_type.to_string();
        let path = path.map(|s| s.to_string());
        let detail = detail.map(|s| s.to_string());

        self.writer
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO events (session_id, event_type, path, detail) \
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![session_id, event_type, path, detail],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .await
    }

    /// Get recent events.
    pub async fn recent(&self, limit: i64) -> Result<Vec<Event>> {
        let reader = self.readers.acquire().await?;
        let mut stmt = reader.conn().prepare(
            "SELECT id, session_id, event_type, path, detail, recorded_at \
             FROM events ORDER BY id DESC LIMIT ?1",
        )?;
        let events = stmt
            .query_map([limit], |row| {
                Ok(Event {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    event_type: row.get(2)?,
                    path: row.get(3)?,
                    detail: row.get(4)?,
                    recorded_at: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(events)
    }

    /// Get events filtered by type.
    pub async fn by_type(&self, event_type: &str, limit: i64) -> Result<Vec<Event>> {
        let reader = self.readers.acquire().await?;
        let event_type = event_type.to_string();
        let mut stmt = reader.conn().prepare(
            "SELECT id, session_id, event_type, path, detail, recorded_at \
             FROM events WHERE event_type = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let events = stmt
            .query_map(rusqlite::params![event_type, limit], |row| {
                Ok(Event {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    event_type: row.get(2)?,
                    path: row.get(3)?,
                    detail: row.get(4)?,
                    recorded_at: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(events)
    }

    /// Get events filtered by session.
    pub async fn by_session(&self, session_id: &str, limit: i64) -> Result<Vec<Event>> {
        let reader = self.readers.acquire().await?;
        let session_id = session_id.to_string();
        let mut stmt = reader.conn().prepare(
            "SELECT id, session_id, event_type, path, detail, recorded_at \
             FROM events WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let events = stmt
            .query_map(rusqlite::params![session_id, limit], |row| {
                Ok(Event {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    event_type: row.get(2)?,
                    path: row.get(3)?,
                    detail: row.get(4)?,
                    recorded_at: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(events)
    }

    /// Get event counts grouped by type.
    pub async fn count_by_type(&self) -> Result<Vec<(String, i64)>> {
        let reader = self.readers.acquire().await?;
        let mut stmt = reader.conn().prepare(
            "SELECT event_type, COUNT(*) FROM events GROUP BY event_type ORDER BY COUNT(*) DESC",
        )?;
        let counts = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentFSConfig;
    use crate::connection::pool::{ReaderPool, WriterHandle};
    use crate::schema::init_schema;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    async fn setup() -> (Events, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let cfg = AgentFSConfig::builder(tmp.path()).reader_count(2).build();

        {
            let conn = Connection::open(tmp.path()).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            init_schema(&conn, cfg.chunk_size).unwrap();
        }

        let writer = Arc::new(WriterHandle::open(&cfg).unwrap());
        let readers = Arc::new(ReaderPool::open(&cfg).unwrap());
        let events = Events::new(writer, readers);
        (events, tmp)
    }

    #[tokio::test]
    async fn log_and_recent() {
        let (events, _tmp) = setup().await;

        events.log(None, "fs_write", Some("/a.txt"), None).await.unwrap();
        events.log(None, "fs_read", Some("/a.txt"), None).await.unwrap();

        let recent = events.recent(10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].event_type, "fs_read"); // most recent first
    }

    #[tokio::test]
    async fn filter_by_type() {
        let (events, _tmp) = setup().await;

        events.log(None, "fs_write", Some("/a.txt"), None).await.unwrap();
        events.log(None, "fs_read", Some("/b.txt"), None).await.unwrap();
        events.log(None, "fs_write", Some("/c.txt"), None).await.unwrap();

        let writes = events.by_type("fs_write", 10).await.unwrap();
        assert_eq!(writes.len(), 2);
    }

    #[tokio::test]
    async fn filter_by_session() {
        let (events, _tmp) = setup().await;

        events.log(Some("s1"), "fs_write", Some("/a.txt"), None).await.unwrap();
        events.log(Some("s2"), "fs_read", Some("/b.txt"), None).await.unwrap();

        let s1_events = events.by_session("s1", 10).await.unwrap();
        assert_eq!(s1_events.len(), 1);
        assert_eq!(s1_events[0].path.as_deref(), Some("/a.txt"));
    }

    #[tokio::test]
    async fn count_by_type() {
        let (events, _tmp) = setup().await;

        events.log(None, "fs_write", None, None).await.unwrap();
        events.log(None, "fs_write", None, None).await.unwrap();
        events.log(None, "fs_read", None, None).await.unwrap();

        let counts = events.count_by_type().await.unwrap();
        assert_eq!(counts.len(), 2);
        assert_eq!(counts[0], ("fs_write".to_string(), 2));
        assert_eq!(counts[1], ("fs_read".to_string(), 1));
    }
}
