pub mod analytics;
pub mod config;
pub mod connection;
pub mod error;
pub mod events;
pub mod filesystem;
pub mod gc;
pub mod integrity;
pub mod kvstore;
pub mod schema;
pub mod sessions;
pub mod toolcalls;

use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;
use tokio_util::sync::CancellationToken;
use tracing::info;

use analytics::Analytics;
use config::AgentFSConfig;
use connection::checkpoint::spawn_checkpoint_task;
use connection::pool::{ReaderPool, WriterHandle};
use error::{AgentFSError, Result};
use events::Events;
use filesystem::AgentFSFileSystem;
use kvstore::KvStore;
use sessions::Sessions;
use toolcalls::ToolCalls;

/// Top-level AgentFS instance.
///
/// Manages the writer + reader pool, background checkpoint task,
/// and provides access to the filesystem, kv store, and tool calls.
pub struct AgentFS {
    pub fs: AgentFSFileSystem,
    pub kv: KvStore,
    pub tools: ToolCalls,
    pub sessions: Sessions,
    pub analytics: Analytics,
    pub events: Events,
    writer: Arc<WriterHandle>,
    readers: Arc<ReaderPool>,
    checkpoint_task: Option<tokio::task::JoinHandle<()>>,
    shutdown: CancellationToken,
    config: AgentFSConfig,
}

impl AgentFS {
    /// Create a new AgentFS database at `path`.
    pub async fn create(config: AgentFSConfig) -> Result<Self> {
        if config.db_path.exists() {
            return Err(AgentFSError::DatabaseExists {
                path: config.db_path.clone(),
            });
        }

        // Create and initialize the database
        {
            let conn = Connection::open(&config.db_path)?;
            conn.pragma_update(None, "journal_mode", "WAL")?;
            schema::init_schema(&conn, config.chunk_size)?;
        }

        Self::open_internal(config).await
    }

    /// Open an existing AgentFS database.
    /// Automatically migrates from v1 to v2 if needed.
    pub async fn open(config: AgentFSConfig) -> Result<Self> {
        if !config.db_path.exists() {
            return Err(AgentFSError::DatabaseNotFound {
                path: config.db_path.clone(),
            });
        }

        // Verify schema — auto-migrate if needed
        {
            let conn = Connection::open(&config.db_path)?;
            let version = schema::get_schema_version(&conn)?;
            if version != schema::SCHEMA_VERSION {
                // Try to migrate
                schema::migrate(&conn, config.chunk_size)?;
            }
        }

        Self::open_internal(config).await
    }

    /// Open without checking existence — used by both create and open.
    async fn open_internal(config: AgentFSConfig) -> Result<Self> {
        let writer = Arc::new(WriterHandle::open(&config)?);
        let readers = Arc::new(ReaderPool::open(&config)?);

        let fs = AgentFSFileSystem::new(writer.clone(), readers.clone(), &config)?;
        let kv = KvStore::new(writer.clone(), readers.clone());
        let tools = ToolCalls::new(writer.clone(), readers.clone());
        let sessions = Sessions::new(writer.clone(), readers.clone());
        let analytics = Analytics::new(writer.clone(), readers.clone());
        let events = Events::new(writer.clone(), readers.clone());

        let shutdown = CancellationToken::new();

        // Start background checkpoint task if configured
        let checkpoint_task = if config.checkpoint_interval_secs > 0 {
            let handle = spawn_checkpoint_task(
                writer.conn_arc(),
                config.checkpoint_interval_secs,
                config.wal_truncate_threshold,
                shutdown.clone(),
            );
            Some(handle)
        } else {
            None
        };

        info!(
            path = %config.db_path.display(),
            durability = %config.durability,
            readers = config.reader_count,
            "AgentFS opened"
        );

        Ok(Self {
            fs,
            kv,
            tools,
            sessions,
            analytics,
            events,
            writer,
            readers,
            checkpoint_task,
            shutdown,
            config,
        })
    }

    /// Get the database config.
    pub fn config(&self) -> &AgentFSConfig {
        &self.config
    }

    /// Force a WAL checkpoint (PASSIVE, escalating to TRUNCATE if needed).
    pub async fn checkpoint(&self) -> Result<()> {
        let conn = self.writer.conn_arc();
        let guard = conn.lock().await;
        let (wal_size, _) = connection::checkpoint::passive_checkpoint(&guard)?;
        if wal_size > self.config.wal_truncate_threshold as i32 {
            connection::checkpoint::truncate_checkpoint(&guard)?;
        }
        Ok(())
    }

    /// Run garbage collection.
    pub async fn gc(&self) -> Result<gc::GcReport> {
        self.writer
            .with_conn(|conn| gc::collect_garbage(conn))
            .await
    }

    /// Run a full integrity scrub.
    pub async fn integrity_check(&self) -> Result<integrity::IntegrityReport> {
        let reader = self.readers.acquire().await?;
        integrity::scrub(reader.conn())
    }

    /// Create a snapshot using SQLite's backup API.
    pub async fn snapshot(&self, dest: &Path) -> Result<()> {
        let dest = dest.to_path_buf();
        let reader = self.readers.acquire().await?;
        let mut dest_conn = Connection::open(&dest)?;
        let backup = rusqlite::backup::Backup::new(reader.conn(), &mut dest_conn)?;
        backup.run_to_completion(100, std::time::Duration::from_millis(50), None)?;
        info!(dest = %dest.display(), "snapshot complete");
        Ok(())
    }

    /// Get database info/stats.
    pub async fn info(&self) -> Result<DbInfo> {
        let reader = self.readers.acquire().await?;
        let conn = reader.conn();

        let schema_version = schema::get_schema_version(conn)?;
        let chunk_size = schema::get_chunk_size(conn)?;

        let created_at: String = conn.query_row(
            "SELECT value FROM agentfs_meta WHERE key = 'created_at'",
            [],
            |r| r.get(0),
        )?;

        let inode_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM fs_inode", [], |r| r.get(0))?;
        let file_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM fs_inode WHERE (mode & 61440) = 32768",
            [],
            |r| r.get(0),
        )?;
        let dir_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM fs_inode WHERE (mode & 61440) = 16384",
            [],
            |r| r.get(0),
        )?;
        let total_data_bytes: i64 = conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(data)), 0) FROM fs_data",
            [],
            |r| r.get(0),
        )?;
        let kv_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM kv_store", [], |r| r.get(0))?;
        let tool_call_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM tool_calls", [], |r| r.get(0))?;

        let session_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
        let active_sessions: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE status = 'active'",
            [],
            |r| r.get(0),
        )?;
        let total_tokens: i64 = conn.query_row(
            "SELECT COALESCE(SUM(input_tokens + output_tokens), 0) FROM token_usage",
            [],
            |r| r.get(0),
        )?;
        let total_cost_microcents: i64 = conn.query_row(
            "SELECT COALESCE(SUM(cost_microcents), 0) FROM token_usage",
            [],
            |r| r.get(0),
        )?;
        let event_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;

        // WAL size in pages
        let wal_pages: i32 = conn
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| row.get(1))
            .unwrap_or(0);

        // DB file size
        let page_count: i64 =
            conn.pragma_query_value(None, "page_count", |r| r.get(0))?;
        let page_size: i64 =
            conn.pragma_query_value(None, "page_size", |r| r.get(0))?;

        Ok(DbInfo {
            schema_version,
            chunk_size,
            created_at,
            durability: self.config.durability,
            inode_count,
            file_count,
            dir_count,
            total_data_bytes,
            kv_count,
            tool_call_count,
            session_count,
            active_sessions,
            total_tokens,
            total_cost_microcents,
            event_count,
            wal_pages,
            db_size_bytes: page_count * page_size,
        })
    }

    /// Run schema migration.
    pub async fn migrate(&self) -> Result<()> {
        let chunk_size = self.config.chunk_size;
        self.writer
            .with_conn(move |conn| schema::migrate(conn, chunk_size))
            .await
    }

    /// Graceful shutdown: signal checkpoint task and wait for it.
    pub async fn close(self) -> Result<()> {
        self.shutdown.cancel();
        if let Some(task) = self.checkpoint_task {
            let _ = task.await;
        }
        info!("AgentFS closed");
        Ok(())
    }
}

/// Database information summary.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DbInfo {
    pub schema_version: u32,
    pub chunk_size: usize,
    pub created_at: String,
    pub durability: config::DurabilityLevel,
    pub inode_count: i64,
    pub file_count: i64,
    pub dir_count: i64,
    pub total_data_bytes: i64,
    pub kv_count: i64,
    pub tool_call_count: i64,
    pub session_count: i64,
    pub active_sessions: i64,
    pub total_tokens: i64,
    pub total_cost_microcents: i64,
    pub event_count: i64,
    pub wal_pages: i32,
    pub db_size_bytes: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn create_open_close() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let cfg = AgentFSConfig::builder(&db_path)
            .checkpoint_interval_secs(0)
            .build();

        // Create
        let afs = AgentFS::create(cfg).await.unwrap();

        // Write + read
        afs.fs.write_file("/test.txt", b"hello").await.unwrap();
        let data = afs.fs.read_file("/test.txt").await.unwrap();
        assert_eq!(data, b"hello");

        // Info
        let info = afs.info().await.unwrap();
        assert_eq!(info.schema_version, 2);
        assert_eq!(info.file_count, 1);

        // Close
        afs.close().await.unwrap();

        // Reopen
        let cfg2 = AgentFSConfig::builder(&db_path)
            .checkpoint_interval_secs(0)
            .build();
        let afs2 = AgentFS::open(cfg2).await.unwrap();
        let data = afs2.fs.read_file("/test.txt").await.unwrap();
        assert_eq!(data, b"hello");
        afs2.close().await.unwrap();
    }

    #[tokio::test]
    async fn gc_and_integrity() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let cfg = AgentFSConfig::builder(&db_path)
            .checkpoint_interval_secs(0)
            .verify_checksums(true)
            .build();

        let afs = AgentFS::create(cfg).await.unwrap();
        afs.fs.write_file("/x.txt", b"data").await.unwrap();

        // GC on clean DB
        let report = afs.gc().await.unwrap();
        assert_eq!(report.orphan_inodes, 0);

        // Integrity check
        let integrity = afs.integrity_check().await.unwrap();
        assert!(integrity.is_clean());
        assert!(integrity.total_chunks > 0);

        afs.close().await.unwrap();
    }

    #[tokio::test]
    async fn snapshot() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("src.db");
        let snap_path = dir.path().join("snap.db");
        let cfg = AgentFSConfig::builder(&db_path)
            .checkpoint_interval_secs(0)
            .build();

        let afs = AgentFS::create(cfg).await.unwrap();
        afs.fs.write_file("/file.txt", b"snapshot test").await.unwrap();
        afs.snapshot(&snap_path).await.unwrap();
        afs.close().await.unwrap();

        // Open snapshot and verify
        let cfg2 = AgentFSConfig::builder(&snap_path)
            .checkpoint_interval_secs(0)
            .build();
        let afs2 = AgentFS::open(cfg2).await.unwrap();
        let data = afs2.fs.read_file("/file.txt").await.unwrap();
        assert_eq!(data, b"snapshot test");
        afs2.close().await.unwrap();
    }
}
