use std::sync::Arc;

use crate::connection::pool::{ReaderPool, WriterHandle};
use crate::error::Result;

/// A tool call record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub id: i64,
    pub tool_name: String,
    pub status: String,
    pub input: Option<String>,
    pub output: Option<String>,
    pub error_msg: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
}

/// Tool call statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolStats {
    pub tool_name: String,
    pub total: i64,
    pub successes: i64,
    pub errors: i64,
    pub in_progress: i64,
}

/// Tool call audit trail backed by SQLite.
pub struct ToolCalls {
    writer: Arc<WriterHandle>,
    readers: Arc<ReaderPool>,
}

impl ToolCalls {
    pub fn new(writer: Arc<WriterHandle>, readers: Arc<ReaderPool>) -> Self {
        Self { writer, readers }
    }

    /// Record the start of a tool call. Returns the new record ID.
    pub async fn start(&self, tool_name: &str, input: Option<&str>) -> Result<i64> {
        let tool_name = tool_name.to_string();
        let input = input.map(|s| s.to_string());
        self.writer
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO tool_calls (tool_name, status, input) VALUES (?1, 'started', ?2)",
                    rusqlite::params![tool_name, input],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .await
    }

    /// Record a successful tool call completion.
    pub async fn success(&self, id: i64, output: Option<&str>) -> Result<()> {
        let output = output.map(|s| s.to_string());
        self.writer
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE tool_calls SET status = 'success', output = ?1, \
                     ended_at = strftime('%Y-%m-%dT%H:%M:%f', 'now') WHERE id = ?2",
                    rusqlite::params![output, id],
                )?;
                Ok(())
            })
            .await
    }

    /// Record a failed tool call.
    pub async fn error(&self, id: i64, error_msg: &str) -> Result<()> {
        let error_msg = error_msg.to_string();
        self.writer
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE tool_calls SET status = 'error', error_msg = ?1, \
                     ended_at = strftime('%Y-%m-%dT%H:%M:%f', 'now') WHERE id = ?2",
                    rusqlite::params![error_msg, id],
                )?;
                Ok(())
            })
            .await
    }

    /// Record a complete tool call in one shot.
    pub async fn record(
        &self,
        tool_name: &str,
        input: Option<&str>,
        output: Option<&str>,
        error_msg: Option<&str>,
    ) -> Result<i64> {
        let tool_name = tool_name.to_string();
        let input = input.map(|s| s.to_string());
        let output = output.map(|s| s.to_string());
        let error_msg = error_msg.map(|s| s.to_string());
        let status = if error_msg.is_some() { "error" } else { "success" };

        self.writer
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO tool_calls (tool_name, status, input, output, error_msg, ended_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%f', 'now'))",
                    rusqlite::params![tool_name, status, input, output, error_msg],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .await
    }

    /// Get the most recent tool calls.
    pub async fn recent(&self, limit: i64) -> Result<Vec<ToolCall>> {
        let reader = self.readers.acquire().await?;
        let mut stmt = reader.conn().prepare(
            "SELECT id, tool_name, status, input, output, error_msg, started_at, ended_at \
             FROM tool_calls ORDER BY id DESC LIMIT ?1",
        )?;
        let calls = stmt
            .query_map([limit], |row| {
                Ok(ToolCall {
                    id: row.get(0)?,
                    tool_name: row.get(1)?,
                    status: row.get(2)?,
                    input: row.get(3)?,
                    output: row.get(4)?,
                    error_msg: row.get(5)?,
                    started_at: row.get(6)?,
                    ended_at: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(calls)
    }

    /// Get statistics grouped by tool name.
    pub async fn stats(&self) -> Result<Vec<ToolStats>> {
        let reader = self.readers.acquire().await?;
        let mut stmt = reader.conn().prepare(
            "SELECT tool_name, \
                    COUNT(*) as total, \
                    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) as successes, \
                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) as errors, \
                    SUM(CASE WHEN status = 'started' THEN 1 ELSE 0 END) as in_progress \
             FROM tool_calls GROUP BY tool_name ORDER BY total DESC",
        )?;
        let stats = stmt
            .query_map([], |row| {
                Ok(ToolStats {
                    tool_name: row.get(0)?,
                    total: row.get(1)?,
                    successes: row.get(2)?,
                    errors: row.get(3)?,
                    in_progress: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(stats)
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

    async fn setup() -> (ToolCalls, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let cfg = AgentFSConfig::builder(tmp.path()).reader_count(2).build();

        {
            let conn = Connection::open(tmp.path()).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            init_schema(&conn, cfg.chunk_size).unwrap();
        }

        let writer = Arc::new(WriterHandle::open(&cfg).unwrap());
        let readers = Arc::new(ReaderPool::open(&cfg).unwrap());
        let tc = ToolCalls::new(writer, readers);
        (tc, tmp)
    }

    #[tokio::test]
    async fn start_success_flow() {
        let (tc, _tmp) = setup().await;

        let id = tc.start("read_file", Some(r#"{"path":"/foo"}"#)).await.unwrap();
        tc.success(id, Some(r#"{"content":"hello"}"#)).await.unwrap();

        let recent = tc.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].status, "success");
    }

    #[tokio::test]
    async fn start_error_flow() {
        let (tc, _tmp) = setup().await;

        let id = tc.start("write_file", None).await.unwrap();
        tc.error(id, "permission denied").await.unwrap();

        let recent = tc.recent(10).await.unwrap();
        assert_eq!(recent[0].status, "error");
        assert_eq!(recent[0].error_msg.as_deref(), Some("permission denied"));
    }

    #[tokio::test]
    async fn record_one_shot() {
        let (tc, _tmp) = setup().await;
        tc.record("ls", None, Some("file.txt"), None).await.unwrap();
        tc.record("rm", None, None, Some("not found")).await.unwrap();

        let stats = tc.stats().await.unwrap();
        assert_eq!(stats.len(), 2);
    }
}
