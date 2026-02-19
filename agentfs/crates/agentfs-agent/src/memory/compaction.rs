use std::sync::Arc;

use serde::{Deserialize, Serialize};

use agentfs_core::connection::pool::{ReaderPool, WriterHandle};
use agentfs_core::kvstore::KvStore;

use super::episodes::Episode;
use super::search::MemorySearchEngine;
use super::tiers::{MemoryPressure, TierManager};

// ── Config ─────────────────────────────────────────────────────────

/// Configuration for the compaction engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    /// Number of cold episodes to batch into a meta-episode.
    #[serde(default = "default_5")]
    pub cold_batch_size: usize,
    /// Whether to enable content-hash dedup.
    #[serde(default = "default_true")]
    pub dedup_enabled: bool,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            cold_batch_size: 5,
            dedup_enabled: true,
        }
    }
}

fn default_5() -> usize { 5 }
fn default_true() -> bool { true }

// ── Report ─────────────────────────────────────────────────────────

/// Report from a compaction cycle.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CompactionReport {
    pub duplicates_removed: usize,
    pub episodes_compressed: usize,
    pub tiers_rebalanced: usize,
}

impl std::fmt::Display for CompactionReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Compaction: {} duplicates removed, {} episodes compressed, {} tier changes",
            self.duplicates_removed, self.episodes_compressed, self.tiers_rebalanced,
        )
    }
}

// ── CompactionEngine ───────────────────────────────────────────────

/// Engine for deduplication, compression, and tier management.
pub struct CompactionEngine {
    #[allow(dead_code)]
    writer: Arc<WriterHandle>,
    readers: Arc<ReaderPool>,
    kv: KvStore,
    tier_manager: Arc<TierManager>,
    search_engine: Arc<MemorySearchEngine>,
    config: CompactionConfig,
}

impl CompactionEngine {
    pub fn new(
        writer: Arc<WriterHandle>,
        readers: Arc<ReaderPool>,
        kv: KvStore,
        tier_manager: Arc<TierManager>,
        search_engine: Arc<MemorySearchEngine>,
        config: CompactionConfig,
    ) -> Self {
        Self {
            writer,
            readers,
            kv,
            tier_manager,
            search_engine,
            config,
        }
    }

    /// Run a full compaction cycle:
    /// 1. Content-hash dedup scan
    /// 2. Compress cold episodes (if pressure is High)
    /// 3. Rebalance tiers
    pub async fn run_cycle(&self) -> crate::error::Result<CompactionReport> {
        let mut report = CompactionReport::default();

        // Step 1: Dedup scan
        if self.config.dedup_enabled {
            report.duplicates_removed = self.dedup_scan().await?;
        }

        // Step 2: Compress cold episodes if pressure is high
        let pressure = self.tier_manager.memory_pressure().await?;
        if pressure == MemoryPressure::High {
            report.episodes_compressed = self.compress_cold_episodes().await?;
        }

        // Step 3: Rebalance tiers
        report.tiers_rebalanced = self.tier_manager.rebalance().await?;

        Ok(report)
    }

    /// Scan for duplicate content using content hashes.
    /// Removes newer duplicates, keeping the original.
    async fn dedup_scan(&self) -> crate::error::Result<usize> {
        let reader = self.readers.acquire().await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        // Find all entries grouped by content_hash where there are duplicates
        let mut stmt = reader.conn().prepare(
            "SELECT content_hash, GROUP_CONCAT(key, '|') as keys
             FROM memory_metadata
             WHERE content_hash IS NOT NULL
             GROUP BY content_hash
             HAVING COUNT(*) > 1",
        ).map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let dups: Vec<(String, String)> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        drop(stmt);
        drop(reader);

        let mut removed = 0;
        for (_hash, keys_str) in dups {
            let keys: Vec<&str> = keys_str.split('|').collect();
            if keys.len() < 2 {
                continue;
            }

            // Keep the first key (oldest), remove the rest
            for key in &keys[1..] {
                // Remove from KV
                let _ = self.kv.delete(key).await;
                // Remove from metadata
                self.tier_manager.remove_metadata(key).await?;
                // Remove from FTS
                self.search_engine.remove_entry(key).await?;
                removed += 1;
            }
        }

        Ok(removed)
    }

    /// Compress cold-tier episodes: batch them into meta-episodes.
    async fn compress_cold_episodes(&self) -> crate::error::Result<usize> {
        let reader = self.readers.acquire().await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        // Find cold episodes
        let mut stmt = reader.conn().prepare(
            "SELECT m.key FROM memory_metadata m
             WHERE m.tier = 'cold' AND m.provider = 'episodes'
             ORDER BY m.created ASC",
        ).map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let cold_keys: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        drop(stmt);
        drop(reader);

        if cold_keys.len() < self.config.cold_batch_size {
            return Ok(0);
        }

        let mut compressed = 0;

        // Process in batches
        for batch in cold_keys.chunks(self.config.cold_batch_size) {
            let mut summaries = Vec::new();
            let mut tools: Vec<String> = Vec::new();

            for key in batch {
                if let Ok(entry) = self.kv.get(key).await {
                    if let Ok(episode) = serde_json::from_str::<Episode>(&entry.value) {
                        summaries.push(episode.summary.clone());
                        for t in &episode.tools_used {
                            if !tools.contains(t) {
                                tools.push(t.clone());
                            }
                        }
                    }
                }
            }

            if summaries.is_empty() {
                continue;
            }

            // Create meta-episode
            let meta_episode = Episode {
                session_id: format!("meta-{}", uuid_v4_stub()),
                summary: format!(
                    "Compressed {} sessions: {}",
                    summaries.len(),
                    summaries.join("; ")
                ),
                key_decisions: Vec::new(),
                tools_used: tools,
                outcome: "compressed".to_string(),
                created: chrono::Utc::now().to_rfc3339(),
            };

            let meta_key = format!("memory:episode:{}", meta_episode.session_id);
            let meta_value = serde_json::to_string(&meta_episode)
                .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

            // Store meta-episode
            self.kv.set(&meta_key, &meta_value).await
                .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;
            let hash = content_hash(&meta_value);
            self.tier_manager.ensure_metadata(
                &meta_key,
                "episodes",
                Some(&hash),
                meta_value.len() as i64,
            ).await?;
            self.search_engine.index_entry(&meta_key, "episodes", &meta_episode.summary).await?;

            // Delete originals
            for key in batch {
                let _ = self.kv.delete(key).await;
                self.tier_manager.remove_metadata(key).await?;
                self.search_engine.remove_entry(key).await?;
                compressed += 1;
            }
        }

        Ok(compressed)
    }
}

/// Compute a content hash using xxh3_64.
pub fn content_hash(content: &str) -> String {
    let hash = xxhash_rust::xxh3::xxh3_64(content.as_bytes());
    format!("{:016x}", hash)
}

/// Simple UUID v4 stub (avoids adding uuid dep to compaction).
fn uuid_v4_stub() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:032x}", ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_deterministic() {
        let h1 = content_hash("hello world");
        let h2 = content_hash("hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn content_hash_differs() {
        let h1 = content_hash("hello");
        let h2 = content_hash("world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn default_config() {
        let cfg = CompactionConfig::default();
        assert_eq!(cfg.cold_batch_size, 5);
        assert!(cfg.dedup_enabled);
    }

    #[test]
    fn report_display() {
        let report = CompactionReport {
            duplicates_removed: 3,
            episodes_compressed: 10,
            tiers_rebalanced: 5,
        };
        let s = format!("{report}");
        assert!(s.contains("3 duplicates"));
        assert!(s.contains("10 episodes"));
        assert!(s.contains("5 tier changes"));
    }
}
