use rusqlite::Connection;

use crate::config::DurabilityLevel;
use crate::error::Result;

/// Role of a connection — determines which PRAGMAs to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionRole {
    Writer,
    Reader,
}

/// Apply all PRAGMAs for a freshly opened connection.
pub fn apply_pragmas(
    conn: &Connection,
    role: ConnectionRole,
    durability: DurabilityLevel,
) -> Result<()> {
    // WAL mode — must be set before other pragmas
    conn.pragma_update(None, "journal_mode", "WAL")?;

    // Foreign keys
    conn.pragma_update(None, "foreign_keys", "ON")?;

    // Synchronous level
    let sync_val = match durability {
        DurabilityLevel::Off => "OFF",
        DurabilityLevel::Normal => "NORMAL",
        DurabilityLevel::Full => "FULL",
    };
    conn.pragma_update(None, "synchronous", sync_val)?;

    // Disable auto-checkpoint — we manage checkpoints ourselves
    conn.pragma_update(None, "wal_autocheckpoint", "0")?;

    // Busy timeout — 5 seconds
    conn.pragma_update(None, "busy_timeout", "5000")?;

    // Memory-mapped I/O — 64 MiB
    conn.pragma_update(None, "mmap_size", "67108864")?;

    // Page cache — 2000 pages (~8 MiB)
    conn.pragma_update(None, "cache_size", "-8000")?;

    // Readers get query_only for defense-in-depth
    if role == ConnectionRole::Reader {
        conn.pragma_update(None, "query_only", "ON")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_pragmas() {
        let conn = Connection::open_in_memory().unwrap();
        apply_pragmas(&conn, ConnectionRole::Writer, DurabilityLevel::Normal).unwrap();

        let sync: i64 = conn.pragma_query_value(None, "synchronous", |r| r.get(0)).unwrap();
        // 1 = NORMAL
        assert_eq!(sync, 1);

        // Writer should NOT be query_only
        let qo: i64 = conn.pragma_query_value(None, "query_only", |r| r.get(0)).unwrap();
        assert_eq!(qo, 0);
    }

    #[test]
    fn reader_pragmas() {
        let conn = Connection::open_in_memory().unwrap();
        apply_pragmas(&conn, ConnectionRole::Reader, DurabilityLevel::Full).unwrap();

        let sync: i64 = conn.pragma_query_value(None, "synchronous", |r| r.get(0)).unwrap();
        // 2 = FULL
        assert_eq!(sync, 2);

        let qo: i64 = conn.pragma_query_value(None, "query_only", |r| r.get(0)).unwrap();
        assert_eq!(qo, 1);
    }
}
