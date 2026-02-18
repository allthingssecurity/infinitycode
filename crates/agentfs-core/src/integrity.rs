use rusqlite::Connection;
use xxhash_rust::xxh3::xxh3_64;

use crate::error::{AgentFSError, Result};

/// Compute an XXH3_64 checksum of a data chunk.
pub fn compute_checksum(data: &[u8]) -> u64 {
    xxh3_64(data)
}

/// Verify a chunk's checksum. Returns `Ok(())` or a `ChecksumMismatch` error.
pub fn verify_checksum(data: &[u8], expected: u64, ino: i64, chunk_index: i64) -> Result<()> {
    let actual = compute_checksum(data);
    if actual != expected {
        return Err(AgentFSError::ChecksumMismatch {
            ino,
            chunk_index,
            expected,
            actual,
        });
    }
    Ok(())
}

/// Result of a full-database integrity scrub.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrityReport {
    pub total_chunks: u64,
    pub verified_chunks: u64,
    pub corrupt_chunks: Vec<CorruptChunk>,
    pub sqlite_integrity_ok: bool,
}

impl IntegrityReport {
    pub fn is_clean(&self) -> bool {
        self.corrupt_chunks.is_empty() && self.sqlite_integrity_ok
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CorruptChunk {
    pub ino: i64,
    pub chunk_index: i64,
    pub expected: u64,
    pub actual: u64,
}

/// Run a full integrity scrub over all chunks in the database.
pub fn scrub(conn: &Connection) -> Result<IntegrityReport> {
    // SQLite built-in integrity check
    let sqlite_ok: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    let sqlite_integrity_ok = sqlite_ok == "ok";

    // Scan all chunks
    let mut stmt = conn.prepare("SELECT ino, chunk_index, data, checksum FROM fs_data ORDER BY ino, chunk_index")?;
    let mut total: u64 = 0;
    let mut verified: u64 = 0;
    let mut corrupt = Vec::new();

    let rows = stmt.query_map([], |row| {
        let ino: i64 = row.get(0)?;
        let chunk_index: i64 = row.get(1)?;
        let data: Vec<u8> = row.get(2)?;
        let checksum: i64 = row.get(3)?;
        Ok((ino, chunk_index, data, checksum as u64))
    })?;

    for row in rows {
        let (ino, chunk_index, data, expected) = row?;
        total += 1;
        let actual = compute_checksum(&data);
        if actual == expected {
            verified += 1;
        } else {
            corrupt.push(CorruptChunk {
                ino,
                chunk_index,
                expected,
                actual,
            });
        }
    }

    Ok(IntegrityReport {
        total_chunks: total,
        verified_chunks: verified,
        corrupt_chunks: corrupt,
        sqlite_integrity_ok,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_roundtrip() {
        let data = b"hello, agentfs!";
        let cs = compute_checksum(data);
        assert!(cs != 0);
        verify_checksum(data, cs, 1, 0).unwrap();
    }

    #[test]
    fn checksum_mismatch() {
        let data = b"hello";
        let err = verify_checksum(data, 0xDEADBEEF, 1, 0).unwrap_err();
        assert!(matches!(err, AgentFSError::ChecksumMismatch { .. }));
    }
}
