use std::sync::Arc;

use crate::connection::pool::{ReaderPool, WriterHandle};
use crate::error::Result;

/// An agent session record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: i64,
    pub session_id: String,
    pub agent_name: Option<String>,
    pub provider: Option<String>,
    pub status: String,
    pub metadata: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
}

/// Session lifecycle management — agent-agnostic.
pub struct Sessions {
    writer: Arc<WriterHandle>,
    readers: Arc<ReaderPool>,
}

impl Sessions {
    pub fn new(writer: Arc<WriterHandle>, readers: Arc<ReaderPool>) -> Self {
        Self { writer, readers }
    }

    /// Start a new session.
    pub async fn start(
        &self,
        session_id: &str,
        agent_name: Option<&str>,
        provider: Option<&str>,
        metadata: Option<&str>,
    ) -> Result<Session> {
        let session_id = session_id.to_string();
        let agent_name = agent_name.map(|s| s.to_string());
        let provider = provider.map(|s| s.to_string());
        let metadata = metadata.map(|s| s.to_string());

        self.writer
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO sessions (session_id, agent_name, provider, metadata) \
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![session_id, agent_name, provider, metadata],
                )?;
                let id = conn.last_insert_rowid();
                let session = conn.query_row(
                    "SELECT id, session_id, agent_name, provider, status, metadata, started_at, ended_at \
                     FROM sessions WHERE id = ?1",
                    [id],
                    |row| {
                        Ok(Session {
                            id: row.get(0)?,
                            session_id: row.get(1)?,
                            agent_name: row.get(2)?,
                            provider: row.get(3)?,
                            status: row.get(4)?,
                            metadata: row.get(5)?,
                            started_at: row.get(6)?,
                            ended_at: row.get(7)?,
                        })
                    },
                )?;
                Ok(session)
            })
            .await
    }

    /// End a session with a given status (completed, failed).
    pub async fn end(&self, session_id: &str, status: &str) -> Result<()> {
        let session_id = session_id.to_string();
        let status = status.to_string();
        self.writer
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE sessions SET status = ?1, ended_at = strftime('%Y-%m-%dT%H:%M:%f', 'now') \
                     WHERE session_id = ?2",
                    rusqlite::params![status, session_id],
                )?;
                Ok(())
            })
            .await
    }

    /// Get a session by ID.
    pub async fn get(&self, session_id: &str) -> Result<Session> {
        let reader = self.readers.acquire().await?;
        let session_id = session_id.to_string();
        reader
            .conn()
            .query_row(
                "SELECT id, session_id, agent_name, provider, status, metadata, started_at, ended_at \
                 FROM sessions WHERE session_id = ?1",
                [&session_id],
                |row| {
                    Ok(Session {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        agent_name: row.get(2)?,
                        provider: row.get(3)?,
                        status: row.get(4)?,
                        metadata: row.get(5)?,
                        started_at: row.get(6)?,
                        ended_at: row.get(7)?,
                    })
                },
            )
            .map_err(|_| crate::error::AgentFSError::Other(format!("session not found: {session_id}")))
    }

    /// List all active sessions.
    pub async fn list_active(&self) -> Result<Vec<Session>> {
        let reader = self.readers.acquire().await?;
        let mut stmt = reader.conn().prepare(
            "SELECT id, session_id, agent_name, provider, status, metadata, started_at, ended_at \
             FROM sessions WHERE status = 'active' ORDER BY id DESC",
        )?;
        let sessions = stmt
            .query_map([], |row| {
                Ok(Session {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    agent_name: row.get(2)?,
                    provider: row.get(3)?,
                    status: row.get(4)?,
                    metadata: row.get(5)?,
                    started_at: row.get(6)?,
                    ended_at: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    /// List recent sessions (any status).
    pub async fn list_recent(&self, limit: i64) -> Result<Vec<Session>> {
        let reader = self.readers.acquire().await?;
        let mut stmt = reader.conn().prepare(
            "SELECT id, session_id, agent_name, provider, status, metadata, started_at, ended_at \
             FROM sessions ORDER BY id DESC LIMIT ?1",
        )?;
        let sessions = stmt
            .query_map([limit], |row| {
                Ok(Session {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    agent_name: row.get(2)?,
                    provider: row.get(3)?,
                    status: row.get(4)?,
                    metadata: row.get(5)?,
                    started_at: row.get(6)?,
                    ended_at: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sessions)
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

    async fn setup() -> (Sessions, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let cfg = AgentFSConfig::builder(tmp.path()).reader_count(2).build();

        {
            let conn = Connection::open(tmp.path()).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            init_schema(&conn, cfg.chunk_size).unwrap();
        }

        let writer = Arc::new(WriterHandle::open(&cfg).unwrap());
        let readers = Arc::new(ReaderPool::open(&cfg).unwrap());
        let sessions = Sessions::new(writer, readers);
        (sessions, tmp)
    }

    #[tokio::test]
    async fn start_and_get() {
        let (sessions, _tmp) = setup().await;
        let s = sessions
            .start("sess-1", Some("coder"), Some("anthropic"), None)
            .await
            .unwrap();
        assert_eq!(s.session_id, "sess-1");
        assert_eq!(s.status, "active");
        assert_eq!(s.agent_name.as_deref(), Some("coder"));

        let fetched = sessions.get("sess-1").await.unwrap();
        assert_eq!(fetched.session_id, "sess-1");
    }

    #[tokio::test]
    async fn start_and_end() {
        let (sessions, _tmp) = setup().await;
        sessions
            .start("sess-2", None, None, None)
            .await
            .unwrap();

        sessions.end("sess-2", "completed").await.unwrap();
        let s = sessions.get("sess-2").await.unwrap();
        assert_eq!(s.status, "completed");
        assert!(s.ended_at.is_some());
    }

    #[tokio::test]
    async fn list_active_and_recent() {
        let (sessions, _tmp) = setup().await;
        sessions.start("a", None, None, None).await.unwrap();
        sessions.start("b", None, None, None).await.unwrap();
        sessions.end("a", "completed").await.unwrap();

        let active = sessions.list_active().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].session_id, "b");

        let recent = sessions.list_recent(10).await.unwrap();
        assert_eq!(recent.len(), 2);
    }
}
