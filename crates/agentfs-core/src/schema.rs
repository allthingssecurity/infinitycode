use rusqlite::Connection;
use tracing::info;

use crate::error::{AgentFSError, Result};

/// Current schema version.
pub const SCHEMA_VERSION: u32 = 2;

/// Default chunk size in bytes (64 KiB).
pub const DEFAULT_CHUNK_SIZE: usize = 65536;

/// DDL statements for schema v1.
const SCHEMA_V1: &str = r#"
-- Metadata table
CREATE TABLE IF NOT EXISTS agentfs_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Inodes
CREATE TABLE IF NOT EXISTS fs_inode (
    ino       INTEGER PRIMARY KEY AUTOINCREMENT,
    mode      INTEGER NOT NULL,    -- POSIX mode bits (file type + permissions)
    size      INTEGER NOT NULL DEFAULT 0,
    nlink     INTEGER NOT NULL DEFAULT 0,
    ctime     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')),
    mtime     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')),
    atime     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now'))
);

-- Directory entries
CREATE TABLE IF NOT EXISTS fs_dentry (
    parent_ino INTEGER NOT NULL REFERENCES fs_inode(ino),
    name       TEXT NOT NULL,
    ino        INTEGER NOT NULL REFERENCES fs_inode(ino),
    PRIMARY KEY (parent_ino, name)
);

CREATE INDEX IF NOT EXISTS idx_dentry_ino ON fs_dentry(ino);

-- File data chunks with checksums
CREATE TABLE IF NOT EXISTS fs_data (
    ino         INTEGER NOT NULL REFERENCES fs_inode(ino) ON DELETE CASCADE,
    chunk_index INTEGER NOT NULL,
    data        BLOB NOT NULL,
    checksum    INTEGER NOT NULL,  -- XXH3_64
    PRIMARY KEY (ino, chunk_index)
);

-- Symlinks
CREATE TABLE IF NOT EXISTS fs_symlink (
    ino    INTEGER PRIMARY KEY REFERENCES fs_inode(ino) ON DELETE CASCADE,
    target TEXT NOT NULL
);

-- Key-value store
CREATE TABLE IF NOT EXISTS kv_store (
    key       TEXT PRIMARY KEY,
    value     TEXT NOT NULL,
    created   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')),
    updated   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now'))
);

-- Tool calls audit trail
CREATE TABLE IF NOT EXISTS tool_calls (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    tool_name  TEXT NOT NULL,
    status     TEXT NOT NULL DEFAULT 'started',  -- started, success, error
    input      TEXT,           -- JSON
    output     TEXT,           -- JSON
    error_msg  TEXT,
    started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')),
    ended_at   TEXT
);

CREATE INDEX IF NOT EXISTS idx_tool_calls_name ON tool_calls(tool_name);
CREATE INDEX IF NOT EXISTS idx_tool_calls_started ON tool_calls(started_at);
"#;

/// DDL for schema v2 additions (sessions, token_usage, events).
const SCHEMA_V2_ADDITIONS: &str = r#"
-- Agent sessions (agent-agnostic: any agent framework, any provider)
CREATE TABLE IF NOT EXISTS sessions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id    TEXT UNIQUE NOT NULL,
    agent_name    TEXT,
    provider      TEXT,
    status        TEXT NOT NULL DEFAULT 'active',
    metadata      TEXT,
    started_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')),
    ended_at      TEXT
);
CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);

-- Token usage records
CREATE TABLE IF NOT EXISTS token_usage (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id         TEXT REFERENCES sessions(session_id),
    tool_call_id       INTEGER REFERENCES tool_calls(id),
    model              TEXT NOT NULL,
    input_tokens       INTEGER NOT NULL DEFAULT 0,
    output_tokens      INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    cost_microcents    INTEGER NOT NULL DEFAULT 0,
    recorded_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_token_session ON token_usage(session_id);
CREATE INDEX IF NOT EXISTS idx_token_recorded ON token_usage(recorded_at);

-- Unified event log
CREATE TABLE IF NOT EXISTS events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT,
    event_type  TEXT NOT NULL,
    path        TEXT,
    detail      TEXT,
    recorded_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type);
CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
CREATE INDEX IF NOT EXISTS idx_events_recorded ON events(recorded_at);
"#;

/// Initialize the schema on a freshly opened connection.
/// Returns `true` if the schema was newly created, `false` if it already existed.
pub fn init_schema(conn: &Connection, chunk_size: usize) -> Result<bool> {
    // Check if agentfs_meta already exists
    let exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='agentfs_meta'",
        [],
        |row| row.get(0),
    )?;

    if exists {
        // Verify schema version — accept current or migratable versions
        let version = get_schema_version(conn)?;
        if version == SCHEMA_VERSION {
            return Ok(false);
        }
        // Let migrate() handle known upgrades; otherwise error
        return Err(AgentFSError::SchemaMismatch {
            expected: SCHEMA_VERSION,
            found: version,
        });
    }

    // Create schema (v1 base + v2 additions)
    conn.execute_batch(SCHEMA_V1)?;
    conn.execute_batch(SCHEMA_V2_ADDITIONS)?;

    // Insert metadata
    conn.execute(
        "INSERT INTO agentfs_meta (key, value) VALUES ('schema_version', ?1)",
        [SCHEMA_VERSION.to_string()],
    )?;
    conn.execute(
        "INSERT INTO agentfs_meta (key, value) VALUES ('chunk_size', ?1)",
        [chunk_size.to_string()],
    )?;
    conn.execute(
        "INSERT INTO agentfs_meta (key, value) VALUES ('created_at', strftime('%Y-%m-%dT%H:%M:%f', 'now'))",
        [],
    )?;

    // Create root inode (ino=1, directory, mode 040755)
    let root_mode: i64 = 0o040755;
    conn.execute(
        "INSERT INTO fs_inode (ino, mode, nlink) VALUES (1, ?1, 2)",
        [root_mode],
    )?;

    info!("schema v{SCHEMA_VERSION} initialized with chunk_size={chunk_size}");
    Ok(true)
}

/// Read the schema version from agentfs_meta.
pub fn get_schema_version(conn: &Connection) -> Result<u32> {
    let version_str: String = conn.query_row(
        "SELECT value FROM agentfs_meta WHERE key = 'schema_version'",
        [],
        |row| row.get(0),
    )?;
    version_str
        .parse::<u32>()
        .map_err(|_| AgentFSError::Other(format!("invalid schema version: {version_str}")))
}

/// Read the chunk size from agentfs_meta.
pub fn get_chunk_size(conn: &Connection) -> Result<usize> {
    let val: String = conn.query_row(
        "SELECT value FROM agentfs_meta WHERE key = 'chunk_size'",
        [],
        |row| row.get(0),
    )?;
    val.parse::<usize>()
        .map_err(|_| AgentFSError::Other(format!("invalid chunk_size: {val}")))
}

/// Migrate the database schema to the latest version.
/// Currently only supports v1 (the initial version).
pub fn migrate(conn: &Connection, chunk_size: usize) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='agentfs_meta'",
        [],
        |row| row.get(0),
    )?;

    if !exists {
        init_schema(conn, chunk_size)?;
        return Ok(());
    }

    let version = get_schema_version(conn)?;
    if version == SCHEMA_VERSION {
        info!("schema already at v{SCHEMA_VERSION}, no migration needed");
        return Ok(());
    }

    if version == 1 {
        migrate_v1_to_v2(conn)?;
        return Ok(());
    }

    Err(AgentFSError::SchemaMismatch {
        expected: SCHEMA_VERSION,
        found: version,
    })
}

/// Migrate from schema v1 to v2: add sessions, token_usage, events tables,
/// and session_id column to tool_calls.
fn migrate_v1_to_v2(conn: &Connection) -> Result<()> {
    info!("migrating schema v1 → v2");

    // Add session_id to tool_calls (nullable for backwards compat)
    conn.execute_batch(
        "ALTER TABLE tool_calls ADD COLUMN session_id TEXT REFERENCES sessions(session_id);",
    )?;

    // Create new v2 tables
    conn.execute_batch(SCHEMA_V2_ADDITIONS)?;

    // Update schema version
    conn.execute(
        "UPDATE agentfs_meta SET value = ?1 WHERE key = 'schema_version'",
        [SCHEMA_VERSION.to_string()],
    )?;

    info!("schema migrated to v{SCHEMA_VERSION}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_and_verify() {
        let conn = Connection::open_in_memory().unwrap();
        let created = init_schema(&conn, 65536).unwrap();
        assert!(created);

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(version, 2);

        let chunk_size = get_chunk_size(&conn).unwrap();
        assert_eq!(chunk_size, 65536);

        // Root inode exists
        let mode: i64 = conn
            .query_row("SELECT mode FROM fs_inode WHERE ino = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, 0o040755);

        // v2 tables exist
        let sessions_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='sessions'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(sessions_exists);

        let events_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='events'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(events_exists);

        let token_usage_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='token_usage'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(token_usage_exists);

        // Second call returns false (already exists)
        let created2 = init_schema(&conn, 65536).unwrap();
        assert!(!created2);
    }

    #[test]
    fn schema_version_mismatch() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 65536).unwrap();

        // Tamper with version
        conn.execute(
            "UPDATE agentfs_meta SET value = '999' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();

        let err = init_schema(&conn, 65536).unwrap_err();
        assert!(matches!(err, AgentFSError::SchemaMismatch { expected: 2, found: 999 }));
    }

    #[test]
    fn migrate_v1_to_v2() {
        let conn = Connection::open_in_memory().unwrap();

        // Create a v1 schema manually
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute(
            "INSERT INTO agentfs_meta (key, value) VALUES ('schema_version', '1')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agentfs_meta (key, value) VALUES ('chunk_size', '65536')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO agentfs_meta (key, value) VALUES ('created_at', strftime('%Y-%m-%dT%H:%M:%f', 'now'))",
            [],
        )
        .unwrap();
        let root_mode: i64 = 0o040755;
        conn.execute(
            "INSERT INTO fs_inode (ino, mode, nlink) VALUES (1, ?1, 2)",
            [root_mode],
        )
        .unwrap();

        assert_eq!(get_schema_version(&conn).unwrap(), 1);

        // Run migration
        migrate(&conn, 65536).unwrap();

        assert_eq!(get_schema_version(&conn).unwrap(), 2);

        // Verify new tables exist
        let sessions_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='sessions'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(sessions_exists);

        // Verify tool_calls has session_id column
        let has_session_id: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('tool_calls') WHERE name='session_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(has_session_id);
    }
}
