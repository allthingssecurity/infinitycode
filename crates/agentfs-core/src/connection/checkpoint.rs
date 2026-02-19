use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::error::Result;

/// Run a PASSIVE WAL checkpoint. Returns (wal_size, checkpointed) in pages.
pub fn passive_checkpoint(conn: &Connection) -> Result<(i32, i32)> {
    let mut wal_size: i32 = 0;
    let mut checkpointed: i32 = 0;
    conn.query_row(
        "PRAGMA wal_checkpoint(PASSIVE)",
        [],
        |row| {
            let _busy: i32 = row.get(0)?;
            wal_size = row.get(1)?;
            checkpointed = row.get(2)?;
            Ok(())
        },
    )?;
    debug!(wal_size, checkpointed, "PASSIVE checkpoint");
    Ok((wal_size, checkpointed))
}

/// Run a TRUNCATE checkpoint — blocks writers, resets WAL to zero.
pub fn truncate_checkpoint(conn: &Connection) -> Result<()> {
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        let busy: i32 = row.get(0)?;
        let wal_size: i32 = row.get(1)?;
        let checkpointed: i32 = row.get(2)?;
        if busy != 0 {
            tracing::warn!(wal_size, checkpointed, "TRUNCATE checkpoint was busy");
        } else {
            tracing::info!(wal_size, checkpointed, "TRUNCATE checkpoint complete");
        }
        Ok(())
    })?;
    Ok(())
}

/// Spawn a background checkpoint task that runs periodically.
///
/// - Runs `PASSIVE` every `interval_secs` seconds.
/// - Escalates to `TRUNCATE` when WAL exceeds `truncate_threshold` pages.
/// - Stops when the `shutdown` token is cancelled.
pub fn spawn_checkpoint_task(
    writer_conn: Arc<Mutex<Connection>>,
    interval_secs: u64,
    truncate_threshold: u32,
    shutdown: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval = tokio::time::Duration::from_secs(interval_secs);
        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {},
                _ = shutdown.cancelled() => {
                    info!("checkpoint task shutting down — final TRUNCATE");
                    let conn = writer_conn.lock().await;
                    if let Err(e) = truncate_checkpoint(&conn) {
                        warn!("final TRUNCATE checkpoint failed: {e}");
                    }
                    return;
                }
            }

            let conn = writer_conn.lock().await;
            match passive_checkpoint(&conn) {
                Ok((wal_size, _checkpointed)) => {
                    if wal_size > truncate_threshold as i32 {
                        info!(wal_size, threshold = truncate_threshold, "WAL exceeds threshold, escalating to TRUNCATE");
                        if let Err(e) = truncate_checkpoint(&conn) {
                            warn!("TRUNCATE checkpoint failed: {e}");
                        }
                    }
                }
                Err(e) => {
                    warn!("PASSIVE checkpoint failed: {e}");
                }
            }
        }
    })
}
