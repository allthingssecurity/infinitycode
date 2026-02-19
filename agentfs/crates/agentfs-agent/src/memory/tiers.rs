use std::sync::Arc;

use serde::{Deserialize, Serialize};

use agentfs_core::connection::pool::{ReaderPool, WriterHandle};

// ── Types ──────────────────────────────────────────────────────────

/// Memory tier — determines eviction priority and prompt inclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    Hot,
    Warm,
    Cold,
}

impl MemoryTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Warm => "warm",
            Self::Cold => "cold",
        }
    }

    #[allow(dead_code)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "hot" => Self::Hot,
            "cold" => Self::Cold,
            _ => Self::Warm,
        }
    }
}

impl std::fmt::Display for MemoryTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Memory pressure level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressure {
    Low,
    Medium,
    High,
}

/// Configuration for the tier system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierConfig {
    /// Max entries in the hot tier.
    #[serde(default = "default_30")]
    pub hot_budget: usize,
    /// Max total entries across all tiers.
    #[serde(default = "default_200")]
    pub total_budget: usize,
    /// Half-life for time decay in days.
    #[serde(default = "default_14f64")]
    pub half_life_days: f64,
    /// Score threshold below which entries move to cold tier.
    #[serde(default = "default_cold_threshold")]
    pub cold_threshold: f64,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            hot_budget: 30,
            total_budget: 200,
            half_life_days: 14.0,
            cold_threshold: 0.1,
        }
    }
}

fn default_30() -> usize { 30 }
fn default_200() -> usize { 200 }
fn default_14f64() -> f64 { 14.0 }
fn default_cold_threshold() -> f64 { 0.1 }

/// Metadata row from memory_metadata table.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MemoryMeta {
    pub key: String,
    pub provider: String,
    pub tier: MemoryTier,
    pub access_count: i64,
    pub last_accessed: String,
    pub content_hash: Option<String>,
    pub byte_size: i64,
    pub created: String,
}

/// A scored entry used during rebalancing.
#[derive(Debug, Clone)]
pub struct ScoredEntry {
    pub key: String,
    pub score: f64,
    pub tier: MemoryTier,
}

// ── TierManager ────────────────────────────────────────────────────

/// Manages tiered memory storage with time-decay scoring.
pub struct TierManager {
    writer: Arc<WriterHandle>,
    readers: Arc<ReaderPool>,
    config: TierConfig,
}

impl TierManager {
    pub fn new(
        writer: Arc<WriterHandle>,
        readers: Arc<ReaderPool>,
        config: TierConfig,
    ) -> Self {
        Self { writer, readers, config }
    }

    /// Compute the composite score for a memory entry.
    ///
    /// score = (helpful - harmful) × 0.5^(age_days / half_life)
    ///       + recency_weight × 0.5^(days_since_access / half_life)
    ///       + frequency_weight × ln(1 + access_count)
    pub fn compute_score(
        &self,
        helpful: i32,
        harmful: i32,
        age_days: f64,
        days_since_access: f64,
        access_count: i64,
    ) -> f64 {
        let hl = self.config.half_life_days;
        let base_score = (helpful - harmful) as f64;

        let time_decay = 0.5_f64.powf(age_days / hl);
        let relevance = base_score * time_decay;

        let recency_weight = 0.3;
        let recency = recency_weight * 0.5_f64.powf(days_since_access / hl);

        let frequency_weight = 0.2;
        let frequency = frequency_weight * (1.0 + access_count as f64).ln();

        relevance + recency + frequency
    }

    /// Ensure a metadata row exists for a memory entry.
    pub async fn ensure_metadata(
        &self,
        key: &str,
        provider: &str,
        content_hash: Option<&str>,
        byte_size: i64,
    ) -> crate::error::Result<()> {
        let key = key.to_string();
        let provider = provider.to_string();
        let content_hash = content_hash.map(|s| s.to_string());
        self.writer
            .with_conn(move |conn| {
                conn.execute(
                    "INSERT INTO memory_metadata (key, provider, content_hash, byte_size)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(key) DO UPDATE SET
                       content_hash = excluded.content_hash,
                       byte_size = excluded.byte_size",
                    rusqlite::params![key, provider, content_hash, byte_size],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))
    }

    /// Record an access to a memory entry — bumps access_count and last_accessed.
    pub async fn record_access(&self, key: &str) -> crate::error::Result<()> {
        let key = key.to_string();
        self.writer
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE memory_metadata
                     SET access_count = access_count + 1,
                         last_accessed = strftime('%Y-%m-%dT%H:%M:%f', 'now')
                     WHERE key = ?1",
                    [&key],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))
    }

    /// Remove metadata for a key.
    pub async fn remove_metadata(&self, key: &str) -> crate::error::Result<()> {
        let key = key.to_string();
        self.writer
            .with_conn(move |conn| {
                conn.execute("DELETE FROM memory_metadata WHERE key = ?1", [&key])?;
                Ok(())
            })
            .await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))
    }

    /// Rebalance tiers based on scores.
    /// Returns the number of entries that changed tiers.
    pub async fn rebalance(&self) -> crate::error::Result<usize> {
        let reader = self.readers.acquire().await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let mut stmt = reader.conn().prepare(
            "SELECT m.key, m.provider, m.tier, m.access_count, m.last_accessed, m.created,
                    COALESCE(
                        (SELECT CAST(json_extract(kv.value, '$.helpful') AS INTEGER) FROM kv_store kv WHERE kv.key = 'memory:playbook:' || SUBSTR(m.key, LENGTH('memory:playbook:') + 1)),
                        0
                    ) as helpful,
                    COALESCE(
                        (SELECT CAST(json_extract(kv.value, '$.harmful') AS INTEGER) FROM kv_store kv WHERE kv.key = 'memory:playbook:' || SUBSTR(m.key, LENGTH('memory:playbook:') + 1)),
                        0
                    ) as harmful
             FROM memory_metadata m
             ORDER BY m.key",
        ).map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let now = chrono::Utc::now();
        let mut scored: Vec<ScoredEntry> = Vec::new();

        let rows = stmt.query_map([], |row| {
            let key: String = row.get(0)?;
            let _provider: String = row.get(1)?;
            let tier_str: String = row.get(2)?;
            let access_count: i64 = row.get(3)?;
            let last_accessed: String = row.get(4)?;
            let created: String = row.get(5)?;
            let helpful: i32 = row.get(6)?;
            let harmful: i32 = row.get(7)?;
            Ok((key, tier_str, access_count, last_accessed, created, helpful, harmful))
        }).map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        for row in rows {
            let (key, _tier_str, access_count, last_accessed, created, helpful, harmful) =
                row.map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

            let age_days = parse_age_days(&created, &now);
            let access_days = parse_age_days(&last_accessed, &now);
            let score = self.compute_score(helpful, harmful, age_days, access_days, access_count);

            scored.push(ScoredEntry {
                key,
                score,
                tier: MemoryTier::Warm, // Will be reassigned below
            });
        }

        drop(stmt);
        drop(reader);

        // Sort by score descending
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // Assign tiers: top hot_budget → Hot, rest above cold_threshold → Warm, below → Cold
        let mut changed = 0usize;
        for (i, entry) in scored.iter_mut().enumerate() {
            let new_tier = if i < self.config.hot_budget {
                MemoryTier::Hot
            } else if entry.score >= self.config.cold_threshold {
                MemoryTier::Warm
            } else {
                MemoryTier::Cold
            };
            entry.tier = new_tier;
        }

        // Batch update tiers
        for entry in &scored {
            let key = entry.key.clone();
            let tier = entry.tier.as_str().to_string();
            let did_change = self.writer
                .with_conn(move |conn| {
                    let updated = conn.execute(
                        "UPDATE memory_metadata SET tier = ?1 WHERE key = ?2 AND tier != ?1",
                        rusqlite::params![tier, key],
                    )?;
                    Ok(updated)
                })
                .await
                .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;
            changed += did_change;
        }

        Ok(changed)
    }

    /// Get the current memory pressure level.
    pub async fn memory_pressure(&self) -> crate::error::Result<MemoryPressure> {
        let reader = self.readers.acquire().await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let total: i64 = reader.conn().query_row(
            "SELECT COUNT(*) FROM memory_metadata",
            [],
            |r| r.get(0),
        ).map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let total = total as usize;
        let budget = self.config.total_budget;

        Ok(if total >= budget {
            MemoryPressure::High
        } else if total >= budget * 3 / 4 {
            MemoryPressure::Medium
        } else {
            MemoryPressure::Low
        })
    }

    /// Get tier distribution counts.
    pub async fn tier_counts(&self) -> crate::error::Result<(usize, usize, usize)> {
        let reader = self.readers.acquire().await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let hot: i64 = reader.conn().query_row(
            "SELECT COUNT(*) FROM memory_metadata WHERE tier = 'hot'",
            [],
            |r| r.get(0),
        ).map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let warm: i64 = reader.conn().query_row(
            "SELECT COUNT(*) FROM memory_metadata WHERE tier = 'warm'",
            [],
            |r| r.get(0),
        ).map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let cold: i64 = reader.conn().query_row(
            "SELECT COUNT(*) FROM memory_metadata WHERE tier = 'cold'",
            [],
            |r| r.get(0),
        ).map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        Ok((hot as usize, warm as usize, cold as usize))
    }

    /// Check if a content hash already exists.
    pub async fn has_content_hash(&self, hash: &str) -> crate::error::Result<Option<String>> {
        let reader = self.readers.acquire().await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let hash = hash.to_string();
        let result: Option<String> = reader.conn().query_row(
            "SELECT key FROM memory_metadata WHERE content_hash = ?1 LIMIT 1",
            [&hash],
            |r| r.get(0),
        ).ok();

        Ok(result)
    }

    /// Get the tier config.
    #[allow(dead_code)]
    pub fn config(&self) -> &TierConfig {
        &self.config
    }
}

/// Parse a datetime string into days-ago from `now`.
fn parse_age_days(datetime_str: &str, now: &chrono::DateTime<chrono::Utc>) -> f64 {
    chrono::DateTime::parse_from_rfc3339(datetime_str)
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(datetime_str, "%Y-%m-%dT%H:%M:%S%.f")
            .map(|naive| naive.and_utc().fixed_offset()))
        .map(|dt| {
            let duration = *now - dt.with_timezone(&chrono::Utc);
            duration.num_seconds() as f64 / 86400.0
        })
        .unwrap_or(0.0)
        .max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_display() {
        assert_eq!(MemoryTier::Hot.as_str(), "hot");
        assert_eq!(MemoryTier::Warm.as_str(), "warm");
        assert_eq!(MemoryTier::Cold.as_str(), "cold");
    }

    #[test]
    fn tier_from_str() {
        assert_eq!(MemoryTier::from_str("hot"), MemoryTier::Hot);
        assert_eq!(MemoryTier::from_str("warm"), MemoryTier::Warm);
        assert_eq!(MemoryTier::from_str("cold"), MemoryTier::Cold);
        assert_eq!(MemoryTier::from_str("unknown"), MemoryTier::Warm);
    }

    #[test]
    fn default_config() {
        let cfg = TierConfig::default();
        assert_eq!(cfg.hot_budget, 30);
        assert_eq!(cfg.total_budget, 200);
        assert_eq!(cfg.half_life_days, 14.0);
        assert_eq!(cfg.cold_threshold, 0.1);
    }

    #[test]
    fn score_computation() {
        let writer = test_writer();
        let readers = test_readers();
        let mgr = TierManager::new(writer, readers, TierConfig::default());

        // Fresh, highly-helpful entry
        let score = mgr.compute_score(5, 0, 0.0, 0.0, 0);
        assert!(score > 4.0, "Fresh helpful entry should score high: {score}");

        // Old entry decays
        let score_old = mgr.compute_score(5, 0, 28.0, 28.0, 0);
        assert!(score_old < score, "Old entry should score lower: {score_old} vs {score}");

        // Frequently accessed entry gets a boost
        let score_freq = mgr.compute_score(1, 0, 14.0, 1.0, 20);
        let score_nofreq = mgr.compute_score(1, 0, 14.0, 1.0, 0);
        assert!(score_freq > score_nofreq, "Frequent access should boost score");

        // Harmful entries score lower
        let score_harmful = mgr.compute_score(2, 5, 0.0, 0.0, 0);
        assert!(score_harmful < 0.0, "Harmful entry should be negative: {score_harmful}");
    }

    #[test]
    fn parse_age_days_works() {
        let now = chrono::Utc::now();
        let yesterday = (now - chrono::Duration::days(1)).to_rfc3339();
        let age = parse_age_days(&yesterday, &now);
        assert!((age - 1.0).abs() < 0.1, "Should be ~1 day: {age}");
    }

    // Helpers — these don't actually connect to a DB, just satisfy the type system
    // for unit tests that only test compute_score.
    fn test_writer() -> Arc<WriterHandle> {
        use agentfs_core::config::AgentFSConfig;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let cfg = AgentFSConfig::builder(tmp.path()).build();
        Arc::new(WriterHandle::open(&cfg).unwrap())
    }

    fn test_readers() -> Arc<ReaderPool> {
        use agentfs_core::config::AgentFSConfig;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let cfg = AgentFSConfig::builder(tmp.path()).build();
        Arc::new(ReaderPool::open(&cfg).unwrap())
    }
}
