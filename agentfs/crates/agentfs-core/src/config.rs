use std::path::{Path, PathBuf};

/// Controls SQLite `PRAGMA synchronous` level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DurabilityLevel {
    /// `synchronous = OFF` — no crash safety. Benchmark only.
    Off,
    /// `synchronous = NORMAL` — safe against process crash. **Default.**
    Normal,
    /// `synchronous = FULL` — safe against process crash + power loss.
    Full,
}

impl Default for DurabilityLevel {
    fn default() -> Self {
        Self::Normal
    }
}

impl std::fmt::Display for DurabilityLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => write!(f, "off"),
            Self::Normal => write!(f, "normal"),
            Self::Full => write!(f, "full"),
        }
    }
}

impl std::str::FromStr for DurabilityLevel {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "normal" => Ok(Self::Normal),
            "full" => Ok(Self::Full),
            other => Err(format!("unknown durability level: {other}")),
        }
    }
}

/// Configuration for an AgentFS instance.
#[derive(Debug, Clone)]
pub struct AgentFSConfig {
    /// Path to the SQLite database file.
    pub db_path: PathBuf,
    /// Durability level (maps to `PRAGMA synchronous`).
    pub durability: DurabilityLevel,
    /// Number of reader connections in the pool.
    pub reader_count: usize,
    /// Chunk size for file data (bytes). Default 64 KiB.
    pub chunk_size: usize,
    /// Whether to verify checksums on every read.
    pub verify_checksums: bool,
    /// Checkpoint interval in seconds. 0 disables background checkpointing.
    pub checkpoint_interval_secs: u64,
    /// WAL page threshold to escalate to TRUNCATE checkpoint.
    pub wal_truncate_threshold: u32,
}

impl AgentFSConfig {
    /// Create a config builder for the given database path.
    pub fn builder(db_path: impl AsRef<Path>) -> AgentFSConfigBuilder {
        AgentFSConfigBuilder {
            db_path: db_path.as_ref().to_path_buf(),
            durability: DurabilityLevel::default(),
            reader_count: 4,
            chunk_size: 64 * 1024,
            verify_checksums: false,
            checkpoint_interval_secs: 30,
            wal_truncate_threshold: 4000,
        }
    }
}

/// Builder for [`AgentFSConfig`].
#[derive(Debug, Clone)]
pub struct AgentFSConfigBuilder {
    db_path: PathBuf,
    durability: DurabilityLevel,
    reader_count: usize,
    chunk_size: usize,
    verify_checksums: bool,
    checkpoint_interval_secs: u64,
    wal_truncate_threshold: u32,
}

impl AgentFSConfigBuilder {
    pub fn durability(mut self, level: DurabilityLevel) -> Self {
        self.durability = level;
        self
    }

    pub fn reader_count(mut self, n: usize) -> Self {
        self.reader_count = n.max(1);
        self
    }

    pub fn chunk_size(mut self, size: usize) -> Self {
        self.chunk_size = size.max(4096);
        self
    }

    pub fn verify_checksums(mut self, yes: bool) -> Self {
        self.verify_checksums = yes;
        self
    }

    pub fn checkpoint_interval_secs(mut self, secs: u64) -> Self {
        self.checkpoint_interval_secs = secs;
        self
    }

    pub fn wal_truncate_threshold(mut self, pages: u32) -> Self {
        self.wal_truncate_threshold = pages;
        self
    }

    pub fn build(self) -> AgentFSConfig {
        AgentFSConfig {
            db_path: self.db_path,
            durability: self.durability,
            reader_count: self.reader_count,
            chunk_size: self.chunk_size,
            verify_checksums: self.verify_checksums,
            checkpoint_interval_secs: self.checkpoint_interval_secs,
            wal_truncate_threshold: self.wal_truncate_threshold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let cfg = AgentFSConfig::builder("/tmp/test.db").build();
        assert_eq!(cfg.durability, DurabilityLevel::Normal);
        assert_eq!(cfg.reader_count, 4);
        assert_eq!(cfg.chunk_size, 64 * 1024);
        assert!(!cfg.verify_checksums);
    }

    #[test]
    fn parse_durability() {
        assert_eq!("off".parse::<DurabilityLevel>().unwrap(), DurabilityLevel::Off);
        assert_eq!("NORMAL".parse::<DurabilityLevel>().unwrap(), DurabilityLevel::Normal);
        assert_eq!("Full".parse::<DurabilityLevel>().unwrap(), DurabilityLevel::Full);
        assert!("bogus".parse::<DurabilityLevel>().is_err());
    }
}
