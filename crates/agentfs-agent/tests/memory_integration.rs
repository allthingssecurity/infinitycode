//! Integration tests for the memory system: tiers, BM25 search, compaction.
//!
//! These tests create a real SQLite database and exercise the full pipeline
//! without needing an API key.

use std::sync::Arc;

use agentfs_core::config::AgentFSConfig;
use agentfs_core::AgentFS;
use tempfile::TempDir;

// Pull in the agent crate's memory module via its binary crate path.
// Since agentfs-agent is a binary crate, we use a helper approach:
// re-test the core logic directly against the database.

/// Create a fresh AgentFS database in a temp directory.
async fn setup_db() -> (Arc<AgentFS>, TempDir) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test-memory.db");
    let cfg = AgentFSConfig::builder(&db_path)
        .checkpoint_interval_secs(0)
        .build();
    let db = AgentFS::create(cfg).await.unwrap();
    (Arc::new(db), dir)
}

// ── Schema v3 ──────────────────────────────────────────────────────

#[tokio::test]
async fn schema_v3_tables_exist() {
    let (db, _dir) = setup_db().await;
    let reader = db.readers().acquire().await.unwrap();

    // memory_metadata exists
    let exists: bool = reader
        .conn()
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='memory_metadata'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(exists, "memory_metadata table should exist");

    // memory_fts exists
    let fts_exists: bool = reader
        .conn()
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='memory_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(fts_exists, "memory_fts table should exist");
}

// ── FTS5 search ────────────────────────────────────────────────────

#[tokio::test]
async fn fts5_index_and_search() {
    let (db, _dir) = setup_db().await;
    let writer = db.writer().clone();
    let readers = db.readers().clone();

    // Insert directly into FTS5
    writer
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO memory_fts (key, provider, content) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    "memory:playbook:str-00001",
                    "playbook",
                    "Always handle errors gracefully in async Rust code"
                ],
            )?;
            conn.execute(
                "INSERT INTO memory_fts (key, provider, content) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    "memory:playbook:str-00002",
                    "playbook",
                    "Use timeout for long-running bash commands"
                ],
            )?;
            conn.execute(
                "INSERT INTO memory_fts (key, provider, content) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    "memory:episode:session-001",
                    "episodes",
                    "Built a REST API with error handling and testing"
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    // Search for "error"
    let reader = readers.acquire().await.unwrap();
    let mut stmt = reader
        .conn()
        .prepare(
            "SELECT key, provider, snippet(memory_fts, 2, '»', '«', '…', 32), -bm25(memory_fts)
             FROM memory_fts WHERE memory_fts MATCH '\"error\"'
             ORDER BY -bm25(memory_fts) DESC",
        )
        .unwrap();

    let results: Vec<(String, String, String, f64)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
            ))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    assert!(
        results.len() >= 2,
        "Should find at least 2 results for 'error', got {}",
        results.len()
    );

    // Both playbook and episode entries should match
    let providers: Vec<&str> = results.iter().map(|r| r.1.as_str()).collect();
    assert!(providers.contains(&"playbook"), "Should find playbook entries");
    assert!(providers.contains(&"episodes"), "Should find episode entries");

    // BM25 scores should be positive
    for r in &results {
        assert!(r.3 > 0.0, "BM25 score should be positive, got {}", r.3);
    }
}

#[tokio::test]
async fn fts5_search_no_results() {
    let (db, _dir) = setup_db().await;
    let writer = db.writer().clone();
    let readers = db.readers().clone();

    writer
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO memory_fts (key, provider, content) VALUES (?1, ?2, ?3)",
                rusqlite::params!["memory:playbook:str-00001", "playbook", "Handle errors well"],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let reader = readers.acquire().await.unwrap();
    let mut stmt = reader
        .conn()
        .prepare(
            "SELECT key FROM memory_fts WHERE memory_fts MATCH '\"zzzznonexistent\"'",
        )
        .unwrap();

    let results: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    assert!(results.is_empty(), "Should find no results for nonsense query");
}

#[tokio::test]
async fn fts5_provider_filter() {
    let (db, _dir) = setup_db().await;
    let writer = db.writer().clone();
    let readers = db.readers().clone();

    writer
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO memory_fts (key, provider, content) VALUES (?1, ?2, ?3)",
                rusqlite::params!["memory:playbook:str-00001", "playbook", "test pattern matching"],
            )?;
            conn.execute(
                "INSERT INTO memory_fts (key, provider, content) VALUES (?1, ?2, ?3)",
                rusqlite::params!["memory:episode:s-001", "episodes", "test pattern matching in session"],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let reader = readers.acquire().await.unwrap();
    let mut stmt = reader
        .conn()
        .prepare(
            "SELECT key FROM memory_fts
             WHERE memory_fts MATCH '\"pattern\"' AND provider = 'playbook'",
        )
        .unwrap();

    let results: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    assert_eq!(results.len(), 1, "Should find exactly 1 playbook result");
    assert!(results[0].contains("playbook"));
}

// ── Tier metadata ──────────────────────────────────────────────────

#[tokio::test]
async fn tier_metadata_crud() {
    let (db, _dir) = setup_db().await;
    let writer = db.writer().clone();
    let readers = db.readers().clone();

    // Insert metadata
    writer
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO memory_metadata (key, provider, tier, byte_size, content_hash)
                 VALUES ('memory:playbook:str-00001', 'playbook', 'warm', 100, 'abc123')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    // Read back
    let reader = readers.acquire().await.unwrap();
    let (tier, access_count, byte_size): (String, i64, i64) = reader
        .conn()
        .query_row(
            "SELECT tier, access_count, byte_size FROM memory_metadata WHERE key = ?1",
            ["memory:playbook:str-00001"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();

    assert_eq!(tier, "warm");
    assert_eq!(access_count, 0);
    assert_eq!(byte_size, 100);
    drop(reader);

    // Update access
    writer
        .with_conn(|conn| {
            conn.execute(
                "UPDATE memory_metadata SET access_count = access_count + 1,
                 last_accessed = strftime('%Y-%m-%dT%H:%M:%f', 'now')
                 WHERE key = 'memory:playbook:str-00001'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let reader2 = readers.acquire().await.unwrap();
    let new_count: i64 = reader2
        .conn()
        .query_row(
            "SELECT access_count FROM memory_metadata WHERE key = ?1",
            ["memory:playbook:str-00001"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(new_count, 1);
}

#[tokio::test]
async fn tier_distribution_query() {
    let (db, _dir) = setup_db().await;
    let writer = db.writer().clone();
    let readers = db.readers().clone();

    // Insert entries in different tiers
    writer
        .with_conn(|conn| {
            for i in 0..5 {
                conn.execute(
                    "INSERT INTO memory_metadata (key, provider, tier, byte_size)
                     VALUES (?1, 'playbook', 'hot', 50)",
                    [format!("memory:playbook:hot-{i}")],
                )?;
            }
            for i in 0..10 {
                conn.execute(
                    "INSERT INTO memory_metadata (key, provider, tier, byte_size)
                     VALUES (?1, 'playbook', 'warm', 50)",
                    [format!("memory:playbook:warm-{i}")],
                )?;
            }
            for i in 0..3 {
                conn.execute(
                    "INSERT INTO memory_metadata (key, provider, tier, byte_size)
                     VALUES (?1, 'episodes', 'cold', 50)",
                    [format!("memory:episode:cold-{i}")],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();

    let reader = readers.acquire().await.unwrap();

    let hot: i64 = reader
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM memory_metadata WHERE tier = 'hot'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let warm: i64 = reader
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM memory_metadata WHERE tier = 'warm'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let cold: i64 = reader
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM memory_metadata WHERE tier = 'cold'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    assert_eq!(hot, 5);
    assert_eq!(warm, 10);
    assert_eq!(cold, 3);
}

// ── Content-hash dedup ─────────────────────────────────────────────

#[tokio::test]
async fn content_hash_dedup_detection() {
    let (db, _dir) = setup_db().await;
    let writer = db.writer().clone();
    let readers = db.readers().clone();

    let hash = format!(
        "{:016x}",
        xxhash_rust::xxh3::xxh3_64(b"Always check file exists before reading")
    );

    // Insert first entry with hash
    writer
        .with_conn({
            let hash = hash.clone();
            move |conn| {
                conn.execute(
                    "INSERT INTO memory_metadata (key, provider, tier, byte_size, content_hash)
                     VALUES ('memory:playbook:str-00001', 'playbook', 'warm', 50, ?1)",
                    [&hash],
                )?;
                Ok(())
            }
        })
        .await
        .unwrap();

    // Check if hash exists (simulates dedup check)
    let reader = readers.acquire().await.unwrap();
    let existing: Option<String> = reader
        .conn()
        .query_row(
            "SELECT key FROM memory_metadata WHERE content_hash = ?1 LIMIT 1",
            [&hash],
            |r| r.get(0),
        )
        .ok();

    assert!(
        existing.is_some(),
        "Should detect duplicate via content hash"
    );
    assert_eq!(existing.unwrap(), "memory:playbook:str-00001");
}

// ── KV + FTS integration ──────────────────────────────────────────

#[tokio::test]
async fn kv_and_fts_roundtrip() {
    let (db, _dir) = setup_db().await;

    // Store a playbook entry via KV
    let entry_json = serde_json::json!({
        "id": "str-00001",
        "category": "strategy",
        "content": "Always validate input before processing database queries",
        "helpful": 5,
        "harmful": 0,
        "source_session": "test-session",
        "created": "2026-02-19T12:00:00Z",
        "updated": "2026-02-19T12:00:00Z"
    });
    let value = serde_json::to_string(&entry_json).unwrap();
    db.kv
        .set("memory:playbook:str-00001", &value)
        .await
        .unwrap();

    // Index in FTS
    db.writer()
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO memory_fts (key, provider, content) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    "memory:playbook:str-00001",
                    "playbook",
                    "Always validate input before processing database queries"
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    // Track in metadata
    let hash = format!(
        "{:016x}",
        xxhash_rust::xxh3::xxh3_64(value.as_bytes())
    );
    db.writer()
        .with_conn({
            let hash = hash.clone();
            move |conn| {
                conn.execute(
                    "INSERT INTO memory_metadata (key, provider, tier, byte_size, content_hash)
                     VALUES ('memory:playbook:str-00001', 'playbook', 'warm', ?1, ?2)",
                    rusqlite::params![value.len() as i64, hash],
                )?;
                Ok(())
            }
        })
        .await
        .unwrap();

    // Search via FTS
    let reader = db.readers().acquire().await.unwrap();
    let mut stmt = reader
        .conn()
        .prepare(
            "SELECT f.key, snippet(memory_fts, 2, '»', '«', '…', 32), -bm25(memory_fts) as rank
             FROM memory_fts f
             WHERE memory_fts MATCH '\"database\"'",
        )
        .unwrap();

    let results: Vec<(String, String, f64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    assert_eq!(results.len(), 1);
    assert!(results[0].1.contains("database"));

    // Verify KV value is retrievable
    let kv_entry = db.kv.get("memory:playbook:str-00001").await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&kv_entry.value).unwrap();
    assert_eq!(parsed["content"], "Always validate input before processing database queries");

    // Verify metadata is retrievable
    let (tier, access_count): (String, i64) = reader
        .conn()
        .query_row(
            "SELECT tier, access_count FROM memory_metadata WHERE key = ?1",
            ["memory:playbook:str-00001"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(tier, "warm");
    assert_eq!(access_count, 0);
}

// ── Schema migration ───────────────────────────────────────────────

#[tokio::test]
async fn v2_db_migrates_to_v3_on_open() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("migrate-test.db");

    // Create a v2 database manually
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();

        // v1 base tables
        conn.execute_batch(
            "CREATE TABLE agentfs_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE fs_inode (ino INTEGER PRIMARY KEY AUTOINCREMENT, mode INTEGER NOT NULL, size INTEGER NOT NULL DEFAULT 0, nlink INTEGER NOT NULL DEFAULT 0, ctime TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')), mtime TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')), atime TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')));
             CREATE TABLE fs_dentry (parent_ino INTEGER NOT NULL, name TEXT NOT NULL, ino INTEGER NOT NULL, PRIMARY KEY (parent_ino, name));
             CREATE TABLE fs_data (ino INTEGER NOT NULL, chunk_index INTEGER NOT NULL, data BLOB NOT NULL, checksum INTEGER NOT NULL, PRIMARY KEY (ino, chunk_index));
             CREATE TABLE fs_symlink (ino INTEGER PRIMARY KEY, target TEXT NOT NULL);
             CREATE TABLE kv_store (key TEXT PRIMARY KEY, value TEXT NOT NULL, created TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')), updated TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')));
             CREATE TABLE tool_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, tool_name TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'started', input TEXT, output TEXT, error_msg TEXT, started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')), ended_at TEXT);",
        )
        .unwrap();

        // v2 tables
        conn.execute_batch(
            "ALTER TABLE tool_calls ADD COLUMN session_id TEXT;
             CREATE TABLE sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT UNIQUE NOT NULL, agent_name TEXT, provider TEXT, status TEXT NOT NULL DEFAULT 'active', metadata TEXT, started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')), ended_at TEXT);
             CREATE TABLE token_usage (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, tool_call_id INTEGER, model TEXT NOT NULL, input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0, cache_read_tokens INTEGER NOT NULL DEFAULT 0, cache_write_tokens INTEGER NOT NULL DEFAULT 0, cost_microcents INTEGER NOT NULL DEFAULT 0, recorded_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')));
             CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, event_type TEXT NOT NULL, path TEXT, detail TEXT, recorded_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%f', 'now')));",
        )
        .unwrap();

        conn.execute(
            "INSERT INTO agentfs_meta (key, value) VALUES ('schema_version', '2')",
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
        conn.execute(
            "INSERT INTO fs_inode (ino, mode, nlink) VALUES (1, 16877, 2)",
            [],
        )
        .unwrap();

        // Add some KV memory entries (pre-existing data that should survive migration)
        conn.execute(
            "INSERT INTO kv_store (key, value) VALUES ('memory:playbook:str-00001', '{\"id\":\"str-00001\",\"category\":\"strategy\",\"content\":\"Test entry\",\"helpful\":3,\"harmful\":0,\"source_session\":\"s1\",\"created\":\"2026-01-01\",\"updated\":\"2026-01-01\"}')",
            [],
        )
        .unwrap();
    }

    // Open with auto-migration
    let cfg = AgentFSConfig::builder(&db_path)
        .checkpoint_interval_secs(0)
        .build();
    let db = AgentFS::open(cfg).await.unwrap();

    // Verify schema version is 3
    let info = db.info().await.unwrap();
    assert_eq!(info.schema_version, 3);

    // Verify v3 tables exist
    let reader = db.readers().acquire().await.unwrap();
    let metadata_exists: bool = reader
        .conn()
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='memory_metadata'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(metadata_exists, "memory_metadata should exist after migration");

    let fts_exists: bool = reader
        .conn()
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='memory_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(fts_exists, "memory_fts should exist after migration");

    // Verify pre-existing data survived
    let kv_entry = db.kv.get("memory:playbook:str-00001").await.unwrap();
    assert!(kv_entry.value.contains("Test entry"));

    db.close().await.unwrap();
}
