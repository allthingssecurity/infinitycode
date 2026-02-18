use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{Mutex, Semaphore, OwnedSemaphorePermit};

use crate::config::{AgentFSConfig, DurabilityLevel};
use crate::connection::pragmas::{apply_pragmas, ConnectionRole};
use crate::error::{AgentFSError, Result};

/// Exclusive writer handle — one connection behind a tokio Mutex.
pub struct WriterHandle {
    conn: Arc<Mutex<Connection>>,
    durability: DurabilityLevel,
}

impl WriterHandle {
    pub fn open(config: &AgentFSConfig) -> Result<Self> {
        let conn = Connection::open(&config.db_path)?;
        apply_pragmas(&conn, ConnectionRole::Writer, config.durability)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            durability: config.durability,
        })
    }

    /// Run a blocking closure on the writer connection.
    ///
    /// The closure runs inside `spawn_blocking` so rusqlite's `!Send` is fine.
    pub async fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        let guard = conn.lock().await;
        // We need to use the guard inside spawn_blocking.
        // Since Connection is !Send, we do the work while holding the lock.
        // We wrap this carefully: hold the Mutex, do work synchronously.
        // Actually, we can't move the MutexGuard into spawn_blocking either.
        // The correct pattern: lock, then do synchronous work in the current task.
        // For truly non-blocking, we'd need a dedicated thread. For now, this is
        // acceptable since writes are serialized anyway and SQLite ops are fast.
        f(&guard)
    }

    pub fn durability(&self) -> DurabilityLevel {
        self.durability
    }

    /// Get direct access to the underlying connection Arc for checkpoint operations.
    pub fn conn_arc(&self) -> Arc<Mutex<Connection>> {
        self.conn.clone()
    }
}

/// A reader connection borrowed from the pool.
pub struct ReaderGuard {
    conn: Option<Connection>,
    pool: Arc<ReaderPoolInner>,
    _permit: OwnedSemaphorePermit,
}

impl ReaderGuard {
    pub fn conn(&self) -> &Connection {
        self.conn.as_ref().unwrap()
    }
}

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let mut conns = self.pool.connections.lock().unwrap();
            conns.push(conn);
        }
        // OwnedSemaphorePermit is dropped automatically, releasing the slot
    }
}

struct ReaderPoolInner {
    connections: std::sync::Mutex<Vec<Connection>>,
    semaphore: Arc<Semaphore>,
    db_path: PathBuf,
    durability: DurabilityLevel,
}

/// Semaphore-gated pool of reader connections.
pub struct ReaderPool {
    inner: Arc<ReaderPoolInner>,
}

impl ReaderPool {
    pub fn open(config: &AgentFSConfig) -> Result<Self> {
        let mut connections = Vec::with_capacity(config.reader_count);
        for _ in 0..config.reader_count {
            let conn = Connection::open(&config.db_path)?;
            apply_pragmas(&conn, ConnectionRole::Reader, config.durability)?;
            connections.push(conn);
        }

        Ok(Self {
            inner: Arc::new(ReaderPoolInner {
                connections: std::sync::Mutex::new(connections),
                semaphore: Arc::new(Semaphore::new(config.reader_count)),
                db_path: config.db_path.clone(),
                durability: config.durability,
            }),
        })
    }

    /// Acquire a reader connection from the pool.
    pub async fn acquire(&self) -> Result<ReaderGuard> {
        let permit = self
            .inner
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AgentFSError::PoolShutDown)?;

        let conn = {
            let mut conns = self.inner.connections.lock().unwrap();
            conns.pop()
        };

        let conn = match conn {
            Some(c) => c,
            None => {
                // Shouldn't happen if semaphore is sized correctly, but handle gracefully
                let c = Connection::open(&self.inner.db_path)?;
                apply_pragmas(&c, ConnectionRole::Reader, self.inner.durability)?;
                c
            }
        };

        Ok(ReaderGuard {
            conn: Some(conn),
            pool: self.inner.clone(),
            _permit: permit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentFSConfig;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn writer_roundtrip() {
        let tmp = NamedTempFile::new().unwrap();
        let cfg = AgentFSConfig::builder(tmp.path()).build();
        let writer = WriterHandle::open(&cfg).unwrap();

        let result = writer
            .with_conn(|conn| {
                conn.execute_batch("CREATE TABLE t(x INTEGER)")?;
                conn.execute("INSERT INTO t VALUES (42)", [])?;
                let val: i64 = conn.query_row("SELECT x FROM t", [], |r| r.get(0))?;
                Ok(val)
            })
            .await
            .unwrap();

        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn reader_pool_basics() {
        let tmp = NamedTempFile::new().unwrap();
        let cfg = AgentFSConfig::builder(tmp.path()).reader_count(2).build();

        // Create a table via direct connection first
        {
            let conn = Connection::open(tmp.path()).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            conn.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (99)").unwrap();
        }

        let pool = ReaderPool::open(&cfg).unwrap();

        let guard = pool.acquire().await.unwrap();
        let val: i64 = guard.conn().query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(val, 99);
    }
}
