use std::collections::HashMap;
use std::path::PathBuf;

use agentfs_core::config::AgentFSConfig;
use agentfs_core::AgentFS;

/// Manages database connections — one per database path.
pub struct DbManager {
    dbs: HashMap<PathBuf, AgentFS>,
}

impl DbManager {
    pub fn new() -> Self {
        Self {
            dbs: HashMap::new(),
        }
    }

    /// Get or open a database at the given path.
    pub async fn get_or_open(&mut self, path: &str) -> Result<&AgentFS, String> {
        let canonical = std::fs::canonicalize(path).map_err(|e| format!("invalid path {path}: {e}"))?;

        if !self.dbs.contains_key(&canonical) {
            let config = AgentFSConfig::builder(&canonical)
                .checkpoint_interval_secs(0)
                .build();
            let afs = AgentFS::open(config)
                .await
                .map_err(|e| format!("failed to open {path}: {e}"))?;
            self.dbs.insert(canonical.clone(), afs);
        }

        Ok(self.dbs.get(&canonical).unwrap())
    }

    /// Create a new database at the given path.
    pub async fn create(&mut self, path: &str) -> Result<&AgentFS, String> {
        let path_buf = PathBuf::from(path);
        let config = AgentFSConfig::builder(&path_buf)
            .checkpoint_interval_secs(0)
            .build();
        let afs = AgentFS::create(config)
            .await
            .map_err(|e| format!("failed to create {path}: {e}"))?;

        let canonical = std::fs::canonicalize(path).map_err(|e| format!("canonicalize failed: {e}"))?;
        self.dbs.insert(canonical.clone(), afs);
        Ok(self.dbs.get(&canonical).unwrap())
    }

    /// Gracefully close all database connections.
    pub async fn close_all(self) {
        for (_, afs) in self.dbs {
            let _ = afs.close().await;
        }
    }
}
