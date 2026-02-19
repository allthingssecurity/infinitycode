use std::sync::Arc;

use crate::connection::pool::{ReaderPool, WriterHandle};
use crate::error::{AgentFSError, Result};

/// Key-value entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KvEntry {
    pub key: String,
    pub value: String,
    pub created: String,
    pub updated: String,
}

/// Key-value store backed by SQLite.
pub struct KvStore {
    writer: Arc<WriterHandle>,
    readers: Arc<ReaderPool>,
}

impl KvStore {
    pub fn new(writer: Arc<WriterHandle>, readers: Arc<ReaderPool>) -> Self {
        Self { writer, readers }
    }

    /// Get a value by key.
    pub async fn get(&self, key: &str) -> Result<KvEntry> {
        let reader = self.readers.acquire().await?;
        let key = key.to_string();
        reader
            .conn()
            .query_row(
                "SELECT key, value, created, updated FROM kv_store WHERE key = ?1",
                [&key],
                |row| {
                    Ok(KvEntry {
                        key: row.get(0)?,
                        value: row.get(1)?,
                        created: row.get(2)?,
                        updated: row.get(3)?,
                    })
                },
            )
            .map_err(|_| AgentFSError::KeyNotFound { key })
    }

    /// Set a key-value pair (upsert).
    pub async fn set(&self, key: &str, value: &str) -> Result<()> {
        let key = key.to_string();
        let value = value.to_string();
        self.writer
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO kv_store (key, value) VALUES (?1, ?2) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value, \
                     updated = strftime('%Y-%m-%dT%H:%M:%f', 'now')",
                    rusqlite::params![key, value],
                )?;
                Ok(())
            })
            .await
    }

    /// Delete a key.
    pub async fn delete(&self, key: &str) -> Result<()> {
        let key = key.to_string();
        self.writer
            .with_conn(move |conn| {
                let changed = conn.execute("DELETE FROM kv_store WHERE key = ?1", [&key])?;
                if changed == 0 {
                    return Err(AgentFSError::KeyNotFound { key });
                }
                Ok(())
            })
            .await
    }

    /// List all keys.
    pub async fn keys(&self) -> Result<Vec<String>> {
        let reader = self.readers.acquire().await?;
        let mut stmt = reader.conn().prepare("SELECT key FROM kv_store ORDER BY key")?;
        let keys = stmt
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(keys)
    }

    /// List keys with a given prefix.
    pub async fn list_prefix(&self, prefix: &str) -> Result<Vec<KvEntry>> {
        let reader = self.readers.acquire().await?;
        let pattern = format!("{prefix}%");
        let mut stmt = reader.conn().prepare(
            "SELECT key, value, created, updated FROM kv_store WHERE key LIKE ?1 ORDER BY key",
        )?;
        let entries = stmt
            .query_map([&pattern], |row| {
                Ok(KvEntry {
                    key: row.get(0)?,
                    value: row.get(1)?,
                    created: row.get(2)?,
                    updated: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(entries)
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

    async fn setup() -> (KvStore, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let cfg = AgentFSConfig::builder(tmp.path()).reader_count(2).build();

        {
            let conn = Connection::open(tmp.path()).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            init_schema(&conn, cfg.chunk_size).unwrap();
        }

        let writer = Arc::new(WriterHandle::open(&cfg).unwrap());
        let readers = Arc::new(ReaderPool::open(&cfg).unwrap());
        let kv = KvStore::new(writer, readers);
        (kv, tmp)
    }

    #[tokio::test]
    async fn set_get_delete() {
        let (kv, _tmp) = setup().await;

        kv.set("foo", "bar").await.unwrap();
        let entry = kv.get("foo").await.unwrap();
        assert_eq!(entry.value, "bar");

        kv.delete("foo").await.unwrap();
        let err = kv.get("foo").await.unwrap_err();
        assert!(matches!(err, AgentFSError::KeyNotFound { .. }));
    }

    #[tokio::test]
    async fn upsert() {
        let (kv, _tmp) = setup().await;
        kv.set("k", "v1").await.unwrap();
        kv.set("k", "v2").await.unwrap();
        let entry = kv.get("k").await.unwrap();
        assert_eq!(entry.value, "v2");
    }

    #[tokio::test]
    async fn keys_and_prefix() {
        let (kv, _tmp) = setup().await;
        kv.set("agent:1", "a").await.unwrap();
        kv.set("agent:2", "b").await.unwrap();
        kv.set("config:x", "c").await.unwrap();

        let all = kv.keys().await.unwrap();
        assert_eq!(all, vec!["agent:1", "agent:2", "config:x"]);

        let agents = kv.list_prefix("agent:").await.unwrap();
        assert_eq!(agents.len(), 2);
    }
}
