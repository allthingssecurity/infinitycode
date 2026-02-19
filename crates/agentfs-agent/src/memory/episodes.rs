use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use agentfs_core::AgentFS;

use crate::error::Result;
use crate::memory::{EpisodesConfig, MemoryEntry, MemoryProvider, Reflection};

use super::compaction::content_hash;
use super::search::MemorySearchEngine;
use super::tiers::TierManager;

const KV_PREFIX: &str = "memory:episode:";

/// A compressed session summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub session_id: String,
    pub summary: String,
    pub key_decisions: Vec<String>,
    pub tools_used: Vec<String>,
    pub outcome: String,
    pub created: String,
}

/// DeepAgent-style episodic memory provider.
pub struct EpisodeProvider {
    db: Arc<AgentFS>,
    config: EpisodesConfig,
    /// In-memory cache of episodes, loaded at session start.
    episodes: RwLock<Vec<Episode>>,
    /// Tools used in the current session (tracked for summary).
    session_tools: RwLock<Vec<String>>,
    /// Optional tier manager for access tracking and metadata.
    tier_manager: Option<Arc<TierManager>>,
    /// Optional search engine for FTS indexing.
    search_engine: Option<Arc<MemorySearchEngine>>,
}

impl EpisodeProvider {
    pub fn new(db: Arc<AgentFS>, config: EpisodesConfig) -> Self {
        Self {
            db,
            config,
            episodes: RwLock::new(Vec::new()),
            session_tools: RwLock::new(Vec::new()),
            tier_manager: None,
            search_engine: None,
        }
    }

    /// Attach a tier manager and search engine.
    pub fn with_tier_and_search(
        mut self,
        tier_manager: Arc<TierManager>,
        search_engine: Arc<MemorySearchEngine>,
    ) -> Self {
        self.tier_manager = Some(tier_manager);
        self.search_engine = Some(search_engine);
        self
    }

    /// Load all episodes from KV.
    async fn load_episodes(&self) -> Result<Vec<Episode>> {
        let kv_entries = self
            .db
            .kv
            .list_prefix(KV_PREFIX)
            .await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let mut episodes = Vec::new();
        for kv in kv_entries {
            match serde_json::from_str::<Episode>(&kv.value) {
                Ok(ep) => episodes.push(ep),
                Err(e) => {
                    tracing::warn!("Failed to parse episode {}: {e}", kv.key);
                }
            }
        }

        // Sort by created date descending
        episodes.sort_by(|a, b| b.created.cmp(&a.created));
        Ok(episodes)
    }

    /// Save an episode to KV, and update metadata + FTS index.
    async fn save_episode(&self, episode: &Episode) -> Result<()> {
        let key = format!("{KV_PREFIX}{}", episode.session_id);
        let value = serde_json::to_string(episode)
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        self.db
            .kv
            .set(&key, &value)
            .await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        // Update tier metadata
        if let Some(ref tm) = self.tier_manager {
            let hash = content_hash(&value);
            let _ = tm.ensure_metadata(&key, "episodes", Some(&hash), value.len() as i64).await;
        }

        // Index in FTS
        if let Some(ref se) = self.search_engine {
            let searchable = format!(
                "{} {} {}",
                episode.summary,
                episode.key_decisions.join(" "),
                episode.outcome,
            );
            let _ = se.index_entry(&key, "episodes", &searchable).await;
        }

        Ok(())
    }

    /// Prune old episodes beyond max_episodes.
    async fn prune(&self) -> Result<()> {
        let mut episodes = self.episodes.write().await;
        while episodes.len() > self.config.max_episodes {
            if let Some(oldest) = episodes.pop() {
                let key = format!("{KV_PREFIX}{}", oldest.session_id);
                let _ = self.db.kv.delete(&key).await;
                // Clean up metadata and FTS
                if let Some(ref tm) = self.tier_manager {
                    let _ = tm.remove_metadata(&key).await;
                }
                if let Some(ref se) = self.search_engine {
                    let _ = se.remove_entry(&key).await;
                }
            }
        }
        Ok(())
    }

    /// Format episodes for system prompt.
    fn format_for_prompt(episodes: &[Episode], budget: usize) -> String {
        if episodes.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        let mut total_len = 0;

        for ep in episodes.iter().take(5) {
            let line = format!(
                "- [{}] {} (tools: {}, outcome: {})",
                &ep.created[..10.min(ep.created.len())],
                ep.summary,
                ep.tools_used.join(", "),
                ep.outcome,
            );
            if total_len + line.len() > budget {
                break;
            }
            total_len += line.len();
            lines.push(line);
        }

        if lines.is_empty() {
            return String::new();
        }

        format!("<past_sessions>\n{}\n</past_sessions>", lines.join("\n"))
    }

    /// Get episode count.
    #[allow(dead_code)]
    pub async fn episode_count(&self) -> usize {
        self.episodes.read().await.len()
    }
}

#[async_trait]
impl MemoryProvider for EpisodeProvider {
    fn name(&self) -> &str {
        "episodes"
    }

    async fn context_for_prompt(&self, _query: &str) -> Result<Option<String>> {
        let episodes = self.episodes.read().await;

        // Record access for episodes included in prompt
        if let Some(ref tm) = self.tier_manager {
            for ep in episodes.iter().take(5) {
                let key = format!("{KV_PREFIX}{}", ep.session_id);
                let _ = tm.record_access(&key).await;
            }
        }

        let formatted = Self::format_for_prompt(&episodes, self.config.prompt_budget_chars);
        if formatted.is_empty() {
            Ok(None)
        } else {
            Ok(Some(formatted))
        }
    }

    async fn store(&self, entry: MemoryEntry) -> Result<()> {
        let episode: Episode = serde_json::from_value(entry.metadata)
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;
        self.save_episode(&episode).await?;
        self.episodes.write().await.insert(0, episode);
        self.prune().await?;
        Ok(())
    }

    async fn on_reflection(&self, reflection: &Reflection) -> Result<()> {
        // Track tools used in this session
        let mut tools = self.session_tools.write().await;
        for obs in &reflection.tool_observations {
            if !tools.contains(&obs.tool) {
                tools.push(obs.tool.clone());
            }
        }
        Ok(())
    }

    async fn on_session_start(&self, _session_id: &str) -> Result<()> {
        let loaded = self.load_episodes().await?;

        // Ensure metadata and FTS index for all loaded episodes
        if self.tier_manager.is_some() || self.search_engine.is_some() {
            for ep in &loaded {
                let key = format!("{KV_PREFIX}{}", ep.session_id);
                if let Ok(val) = serde_json::to_string(ep) {
                    if let Some(ref tm) = self.tier_manager {
                        let hash = content_hash(&val);
                        let _ = tm.ensure_metadata(&key, "episodes", Some(&hash), val.len() as i64).await;
                    }
                    if let Some(ref se) = self.search_engine {
                        let searchable = format!("{} {} {}", ep.summary, ep.key_decisions.join(" "), ep.outcome);
                        let _ = se.index_entry(&key, "episodes", &searchable).await;
                    }
                }
            }
        }

        *self.episodes.write().await = loaded;
        *self.session_tools.write().await = Vec::new();
        Ok(())
    }

    async fn on_session_end(&self, session_id: &str) -> Result<()> {
        // Create a summary episode from the session.
        let tools = self.session_tools.read().await;

        // Only create episode if we actually used tools
        if tools.is_empty() {
            return Ok(());
        }

        let episode = Episode {
            session_id: session_id.to_string(),
            summary: format!("Session used {} tools", tools.len()),
            key_decisions: Vec::new(),
            tools_used: tools.clone(),
            outcome: "completed".to_string(),
            created: chrono::Utc::now().to_rfc3339(),
        };

        self.save_episode(&episode).await?;
        self.episodes.write().await.insert(0, episode);
        self.prune().await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_empty() {
        let result = EpisodeProvider::format_for_prompt(&[], 1000);
        assert!(result.is_empty());
    }

    #[test]
    fn format_with_episodes() {
        let episodes = vec![Episode {
            session_id: "abc-123".into(),
            summary: "Built a REST API".into(),
            key_decisions: vec!["chose tower".into()],
            tools_used: vec!["bash".into(), "write_file".into()],
            outcome: "success".into(),
            created: "2026-02-19T12:00:00Z".into(),
        }];

        let result = EpisodeProvider::format_for_prompt(&episodes, 1000);
        assert!(result.contains("<past_sessions>"));
        assert!(result.contains("Built a REST API"));
        assert!(result.contains("bash, write_file"));
    }

    #[test]
    fn budget_limits_output() {
        let episodes: Vec<Episode> = (0..20)
            .map(|i| Episode {
                session_id: format!("session-{i}"),
                summary: format!("Did something important #{i} that takes up space in the prompt"),
                key_decisions: vec![],
                tools_used: vec!["bash".into()],
                outcome: "success".into(),
                created: format!("2026-02-{:02}T12:00:00Z", (i % 28) + 1),
            })
            .collect();

        let result = EpisodeProvider::format_for_prompt(&episodes, 200);
        // Should be truncated to fit budget
        assert!(result.len() <= 300); // some overhead for tags
    }
}
