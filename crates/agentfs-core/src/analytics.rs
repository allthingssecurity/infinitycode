use std::sync::Arc;

use crate::connection::pool::{ReaderPool, WriterHandle};
use crate::error::Result;

/// A token usage record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TokenRecord {
    pub id: Option<i64>,
    pub session_id: Option<String>,
    pub tool_call_id: Option<i64>,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost_microcents: i64,
    pub recorded_at: Option<String>,
}

/// Aggregated usage summary.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageSummary {
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_read: i64,
    pub total_cache_write: i64,
    pub total_cost_microcents: i64,
    pub record_count: i64,
}

/// Per-model breakdown.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelBreakdown {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_microcents: i64,
}

/// Per-session cost.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionCost {
    pub session_id: String,
    pub agent_name: Option<String>,
    pub total_tokens: i64,
    pub cost_microcents: i64,
}

/// Token usage analytics.
pub struct Analytics {
    writer: Arc<WriterHandle>,
    readers: Arc<ReaderPool>,
}

impl Analytics {
    pub fn new(writer: Arc<WriterHandle>, readers: Arc<ReaderPool>) -> Self {
        Self { writer, readers }
    }

    /// Record a token usage entry. Returns the new record ID.
    pub async fn record_usage(&self, record: TokenRecord) -> Result<i64> {
        self.writer
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO token_usage \
                     (session_id, tool_call_id, model, input_tokens, output_tokens, \
                      cache_read_tokens, cache_write_tokens, cost_microcents) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        record.session_id,
                        record.tool_call_id,
                        record.model,
                        record.input_tokens,
                        record.output_tokens,
                        record.cache_read_tokens,
                        record.cache_write_tokens,
                        record.cost_microcents,
                    ],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .await
    }

    /// Get all-time usage summary.
    pub async fn summary(&self) -> Result<UsageSummary> {
        let reader = self.readers.acquire().await?;
        reader.conn().query_row(
            "SELECT \
                COALESCE(SUM(input_tokens), 0), \
                COALESCE(SUM(output_tokens), 0), \
                COALESCE(SUM(cache_read_tokens), 0), \
                COALESCE(SUM(cache_write_tokens), 0), \
                COALESCE(SUM(cost_microcents), 0), \
                COUNT(*) \
             FROM token_usage",
            [],
            |row| {
                Ok(UsageSummary {
                    total_input_tokens: row.get(0)?,
                    total_output_tokens: row.get(1)?,
                    total_cache_read: row.get(2)?,
                    total_cache_write: row.get(3)?,
                    total_cost_microcents: row.get(4)?,
                    record_count: row.get(5)?,
                })
            },
        ).map_err(Into::into)
    }

    /// Get usage summary since a given ISO timestamp.
    pub async fn summary_since(&self, since: &str) -> Result<UsageSummary> {
        let reader = self.readers.acquire().await?;
        let since = since.to_string();
        reader.conn().query_row(
            "SELECT \
                COALESCE(SUM(input_tokens), 0), \
                COALESCE(SUM(output_tokens), 0), \
                COALESCE(SUM(cache_read_tokens), 0), \
                COALESCE(SUM(cache_write_tokens), 0), \
                COALESCE(SUM(cost_microcents), 0), \
                COUNT(*) \
             FROM token_usage WHERE recorded_at >= ?1",
            [&since],
            |row| {
                Ok(UsageSummary {
                    total_input_tokens: row.get(0)?,
                    total_output_tokens: row.get(1)?,
                    total_cache_read: row.get(2)?,
                    total_cache_write: row.get(3)?,
                    total_cost_microcents: row.get(4)?,
                    record_count: row.get(5)?,
                })
            },
        ).map_err(Into::into)
    }

    /// Get usage grouped by model.
    pub async fn by_model(&self) -> Result<Vec<ModelBreakdown>> {
        let reader = self.readers.acquire().await?;
        let mut stmt = reader.conn().prepare(
            "SELECT model, \
                    SUM(input_tokens) as inp, \
                    SUM(output_tokens) as outp, \
                    SUM(cost_microcents) as cost \
             FROM token_usage GROUP BY model ORDER BY cost DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ModelBreakdown {
                    model: row.get(0)?,
                    input_tokens: row.get(1)?,
                    output_tokens: row.get(2)?,
                    cost_microcents: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get cost grouped by session.
    pub async fn by_session(&self) -> Result<Vec<SessionCost>> {
        let reader = self.readers.acquire().await?;
        let mut stmt = reader.conn().prepare(
            "SELECT t.session_id, s.agent_name, \
                    SUM(t.input_tokens + t.output_tokens) as total_tokens, \
                    SUM(t.cost_microcents) as cost \
             FROM token_usage t \
             LEFT JOIN sessions s ON t.session_id = s.session_id \
             WHERE t.session_id IS NOT NULL \
             GROUP BY t.session_id ORDER BY cost DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SessionCost {
                    session_id: row.get(0)?,
                    agent_name: row.get(1)?,
                    total_tokens: row.get(2)?,
                    cost_microcents: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get recent token usage records.
    pub async fn recent_usage(&self, limit: i64) -> Result<Vec<TokenRecord>> {
        let reader = self.readers.acquire().await?;
        let mut stmt = reader.conn().prepare(
            "SELECT id, session_id, tool_call_id, model, input_tokens, output_tokens, \
                    cache_read_tokens, cache_write_tokens, cost_microcents, recorded_at \
             FROM token_usage ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(TokenRecord {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    tool_call_id: row.get(2)?,
                    model: row.get(3)?,
                    input_tokens: row.get(4)?,
                    output_tokens: row.get(5)?,
                    cache_read_tokens: row.get(6)?,
                    cache_write_tokens: row.get(7)?,
                    cost_microcents: row.get(8)?,
                    recorded_at: row.get(9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
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

    async fn setup() -> (Analytics, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let cfg = AgentFSConfig::builder(tmp.path()).reader_count(2).build();

        {
            let conn = Connection::open(tmp.path()).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            init_schema(&conn, cfg.chunk_size).unwrap();
        }

        let writer = Arc::new(WriterHandle::open(&cfg).unwrap());
        let readers = Arc::new(ReaderPool::open(&cfg).unwrap());
        let analytics = Analytics::new(writer, readers);
        (analytics, tmp)
    }

    fn test_record(model: &str, input: i64, output: i64, cost: i64) -> TokenRecord {
        TokenRecord {
            id: None,
            session_id: None,
            tool_call_id: None,
            model: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_microcents: cost,
            recorded_at: None,
        }
    }

    #[tokio::test]
    async fn record_and_summary() {
        let (analytics, _tmp) = setup().await;

        analytics.record_usage(test_record("opus", 100, 50, 500)).await.unwrap();
        analytics.record_usage(test_record("opus", 200, 100, 1000)).await.unwrap();

        let summary = analytics.summary().await.unwrap();
        assert_eq!(summary.total_input_tokens, 300);
        assert_eq!(summary.total_output_tokens, 150);
        assert_eq!(summary.total_cost_microcents, 1500);
        assert_eq!(summary.record_count, 2);
    }

    #[tokio::test]
    async fn by_model() {
        let (analytics, _tmp) = setup().await;

        analytics.record_usage(test_record("opus", 100, 50, 500)).await.unwrap();
        analytics.record_usage(test_record("sonnet", 200, 100, 300)).await.unwrap();

        let models = analytics.by_model().await.unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].model, "opus"); // higher cost first
    }

    #[tokio::test]
    async fn recent_usage() {
        let (analytics, _tmp) = setup().await;

        analytics.record_usage(test_record("opus", 100, 50, 500)).await.unwrap();
        analytics.record_usage(test_record("sonnet", 200, 100, 300)).await.unwrap();

        let recent = analytics.recent_usage(10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].model, "sonnet"); // most recent first
    }
}
