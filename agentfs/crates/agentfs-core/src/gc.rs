use rusqlite::Connection;

use crate::error::Result;

/// Report from a garbage collection run.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GcReport {
    /// Number of orphan inodes deleted (nlink=0, no dentry refs).
    pub orphan_inodes: u64,
    /// Number of stale data chunks deleted (ino not in fs_inode).
    pub stale_chunks: u64,
    /// Number of stale symlinks deleted (ino not in fs_inode).
    pub stale_symlinks: u64,
}

/// Run garbage collection in a single transaction.
///
/// Cleans up:
/// 1. Orphan inodes: nlink=0 and no dentry references
/// 2. Stale data chunks: ino references a non-existent inode
/// 3. Stale symlinks: ino references a non-existent inode
pub fn collect_garbage(conn: &Connection) -> Result<GcReport> {
    let tx = conn.unchecked_transaction()?;

    // 1. Find and delete orphan inodes (nlink <= 0 and no dentry refs, excluding root)
    let orphan_inodes = tx.execute(
        "DELETE FROM fs_inode WHERE ino != 1 AND nlink <= 0 \
         AND ino NOT IN (SELECT DISTINCT ino FROM fs_dentry)",
        [],
    )? as u64;

    // 2. Delete data chunks whose inode no longer exists
    let stale_chunks = tx.execute(
        "DELETE FROM fs_data WHERE ino NOT IN (SELECT ino FROM fs_inode)",
        [],
    )? as u64;

    // 3. Delete symlinks whose inode no longer exists
    let stale_symlinks = tx.execute(
        "DELETE FROM fs_symlink WHERE ino NOT IN (SELECT ino FROM fs_inode)",
        [],
    )? as u64;

    tx.commit()?;

    Ok(GcReport {
        orphan_inodes,
        stale_chunks,
        stale_symlinks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::init_schema;

    #[test]
    fn gc_cleans_orphans() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 65536).unwrap();

        // Create an orphan inode (nlink=0, no dentry)
        conn.execute(
            "INSERT INTO fs_inode (mode, nlink) VALUES (?1, 0)",
            [0o100644i64],
        )
        .unwrap();
        let orphan_ino = conn.last_insert_rowid();

        // Temporarily disable FK checks to insert stale data
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();

        // Create stale data for a non-existent inode
        conn.execute(
            "INSERT INTO fs_data (ino, chunk_index, data, checksum) VALUES (9999, 0, X'FF', 0)",
            [],
        )
        .unwrap();

        // Create stale symlink for a non-existent inode
        conn.execute(
            "INSERT INTO fs_symlink (ino, target) VALUES (9999, '/foo')",
            [],
        )
        .unwrap();

        conn.pragma_update(None, "foreign_keys", "ON").unwrap();

        let report = collect_garbage(&conn).unwrap();
        assert_eq!(report.orphan_inodes, 1);
        assert_eq!(report.stale_chunks, 1);
        assert_eq!(report.stale_symlinks, 1);

        // Verify orphan is gone
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fs_inode WHERE ino = ?1",
                [orphan_ino],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);

        // Root inode still exists
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fs_inode WHERE ino = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn gc_noop_on_clean_db() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 65536).unwrap();

        let report = collect_garbage(&conn).unwrap();
        assert_eq!(report.orphan_inodes, 0);
        assert_eq!(report.stale_chunks, 0);
        assert_eq!(report.stale_symlinks, 0);
    }
}
