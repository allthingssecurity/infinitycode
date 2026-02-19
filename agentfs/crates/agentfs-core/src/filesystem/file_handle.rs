use rusqlite::Connection;

use crate::error::Result;
use crate::integrity::{compute_checksum, verify_checksum};

/// Write file data to chunks with checksums.
///
/// Replaces all existing data for the inode.
pub fn write_file_data(
    conn: &Connection,
    ino: i64,
    data: &[u8],
    chunk_size: usize,
) -> Result<()> {
    // Delete existing chunks
    conn.execute("DELETE FROM fs_data WHERE ino = ?1", [ino])?;

    if data.is_empty() {
        conn.execute(
            "UPDATE fs_inode SET size = 0, mtime = strftime('%Y-%m-%dT%H:%M:%f', 'now') WHERE ino = ?1",
            [ino],
        )?;
        return Ok(());
    }

    let mut stmt = conn.prepare_cached(
        "INSERT INTO fs_data (ino, chunk_index, data, checksum) VALUES (?1, ?2, ?3, ?4)",
    )?;

    for (i, chunk) in data.chunks(chunk_size).enumerate() {
        let checksum = compute_checksum(chunk);
        stmt.execute(rusqlite::params![ino, i as i64, chunk, checksum as i64])?;
    }

    conn.execute(
        "UPDATE fs_inode SET size = ?1, mtime = strftime('%Y-%m-%dT%H:%M:%f', 'now') WHERE ino = ?2",
        rusqlite::params![data.len() as i64, ino],
    )?;

    Ok(())
}

/// Read all file data, reassembling chunks in order.
///
/// If `verify` is true, checks each chunk's XXH3 checksum.
pub fn read_file_data(conn: &Connection, ino: i64, verify: bool) -> Result<Vec<u8>> {
    let mut stmt = conn.prepare_cached(
        "SELECT chunk_index, data, checksum FROM fs_data WHERE ino = ?1 ORDER BY chunk_index",
    )?;

    let mut result = Vec::new();
    let rows = stmt.query_map([ino], |row| {
        let chunk_index: i64 = row.get(0)?;
        let data: Vec<u8> = row.get(1)?;
        let checksum: i64 = row.get(2)?;
        Ok((chunk_index, data, checksum as u64))
    })?;

    for row in rows {
        let (chunk_index, data, checksum) = row?;
        if verify {
            verify_checksum(&data, checksum, ino, chunk_index)?;
        }
        result.extend_from_slice(&data);
    }

    // Update atime
    let _ = conn.execute(
        "UPDATE fs_inode SET atime = strftime('%Y-%m-%dT%H:%M:%f', 'now') WHERE ino = ?1",
        [ino],
    );

    Ok(result)
}

/// Perform fsync semantics based on durability level.
///
/// - `Full`: every commit already fsyncs; this is a no-op.
/// - `Normal`: triggers a PASSIVE WAL checkpoint.
/// - `Off`: no-op.
pub fn fsync(conn: &Connection, durability: crate::config::DurabilityLevel) -> Result<()> {
    use crate::config::DurabilityLevel;
    match durability {
        DurabilityLevel::Normal => {
            crate::connection::checkpoint::passive_checkpoint(conn)?;
            Ok(())
        }
        DurabilityLevel::Full | DurabilityLevel::Off => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AgentFSError;
    use crate::schema::init_schema;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 64).unwrap(); // tiny chunks for testing
        // Create a file inode
        conn.execute(
            "INSERT INTO fs_inode (ino, mode, nlink) VALUES (2, ?1, 1)",
            [0o100644i64],
        )
        .unwrap();
        conn
    }

    #[test]
    fn write_and_read_empty() {
        let conn = setup();
        write_file_data(&conn, 2, b"", 64).unwrap();
        let data = read_file_data(&conn, 2, true).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn write_and_read_single_chunk() {
        let conn = setup();
        write_file_data(&conn, 2, b"hello", 64).unwrap();
        let data = read_file_data(&conn, 2, true).unwrap();
        assert_eq!(data, b"hello");
    }

    #[test]
    fn write_and_read_multi_chunk() {
        let conn = setup();
        let big = vec![0xABu8; 200]; // 200 bytes, chunk_size=64 => 4 chunks
        write_file_data(&conn, 2, &big, 64).unwrap();
        let data = read_file_data(&conn, 2, true).unwrap();
        assert_eq!(data, big);

        // Verify chunk count
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM fs_data WHERE ino = 2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 4); // ceil(200/64) = 4
    }

    #[test]
    fn checksum_verified_on_read() {
        let conn = setup();
        write_file_data(&conn, 2, b"test data", 64).unwrap();

        // Corrupt the checksum
        conn.execute(
            "UPDATE fs_data SET checksum = 12345 WHERE ino = 2",
            [],
        )
        .unwrap();

        let err = read_file_data(&conn, 2, true).unwrap_err();
        assert!(matches!(err, AgentFSError::ChecksumMismatch { .. }));

        // Without verification, it should succeed
        let data = read_file_data(&conn, 2, false).unwrap();
        assert_eq!(data, b"test data");
    }
}
