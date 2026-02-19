pub mod compaction;
pub mod episodes;
pub mod playbook;
pub mod reflector;
pub mod search;
pub mod tiers;
pub mod tool_patterns;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use agentfs_core::AgentFS;
use agentfs_core::kvstore::KvStore;

use crate::error::Result;

use self::compaction::{CompactionConfig, CompactionEngine, CompactionReport};
use self::search::{MemorySearchEngine, SearchResult};
use self::tiers::{MemoryPressure, TierConfig, TierManager};

// ── Data types ──────────────────────────────────────────────────────

/// A generic memory entry for storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct MemoryEntry {
    pub id: String,
    pub provider: String,
    pub content: String,
    pub metadata: serde_json::Value,
    pub created: String,
}

/// Categories for playbook-style learnings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Strategy,
    Mistake,
    Pattern,
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Category::Strategy => write!(f, "strategy"),
            Category::Mistake => write!(f, "mistake"),
            Category::Pattern => write!(f, "pattern"),
        }
    }
}

/// A single learning extracted from reflection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Learning {
    pub category: Category,
    pub content: String,
    pub confidence: f32,
}

/// An observation about tool usage in a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolObs {
    pub tool: String,
    pub success: bool,
    pub pattern: Option<String>,
    pub error: Option<String>,
}

/// The output of the reflector after analyzing a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reflection {
    pub learnings: Vec<Learning>,
    pub helpful_ids: Vec<String>,
    pub harmful_ids: Vec<String>,
    pub tool_observations: Vec<ToolObs>,
    pub session_id: String,
}

// ── Trait ────────────────────────────────────────────────────────────

#[async_trait]
pub trait MemoryProvider: Send + Sync {
    /// Unique name (e.g. "playbook", "episodes", "tool_patterns").
    fn name(&self) -> &str;

    /// Return context to inject into system prompt (called each turn).
    async fn context_for_prompt(&self, query: &str) -> Result<Option<String>>;

    /// Store a new memory entry.
    #[allow(dead_code)]
    async fn store(&self, entry: MemoryEntry) -> Result<()>;

    /// Called after reflection extracts learnings from a turn.
    async fn on_reflection(&self, reflection: &Reflection) -> Result<()>;

    /// Called at session start (load/warm up).
    async fn on_session_start(&self, session_id: &str) -> Result<()>;

    /// Called at session end (compact/summarize).
    async fn on_session_end(&self, session_id: &str) -> Result<()>;
}

// ── Config ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub reflect: bool,
    #[serde(default = "default_reflect_model")]
    pub reflect_model: String,
    #[serde(default = "default_providers")]
    pub providers: Vec<String>,
    #[serde(default)]
    pub playbook: PlaybookConfig,
    #[serde(default)]
    pub episodes: EpisodesConfig,
    #[serde(default)]
    pub tool_patterns: ToolPatternsConfig,
    #[serde(default)]
    pub tiers: TierConfig,
    #[serde(default)]
    pub compaction: CompactionConfig,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            reflect: true,
            reflect_model: default_reflect_model(),
            providers: default_providers(),
            playbook: PlaybookConfig::default(),
            episodes: EpisodesConfig::default(),
            tool_patterns: ToolPatternsConfig::default(),
            tiers: TierConfig::default(),
            compaction: CompactionConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybookConfig {
    #[serde(default = "default_100")]
    pub max_entries: usize,
    #[serde(default = "default_2000")]
    pub prompt_budget_chars: usize,
}

impl Default for PlaybookConfig {
    fn default() -> Self {
        Self {
            max_entries: 100,
            prompt_budget_chars: 2000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodesConfig {
    #[serde(default = "default_20")]
    pub max_episodes: usize,
    #[serde(default = "default_1000")]
    pub prompt_budget_chars: usize,
}

impl Default for EpisodesConfig {
    fn default() -> Self {
        Self {
            max_episodes: 20,
            prompt_budget_chars: 1000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPatternsConfig {
    #[serde(default = "default_500")]
    pub prompt_budget_chars: usize,
}

impl Default for ToolPatternsConfig {
    fn default() -> Self {
        Self {
            prompt_budget_chars: 500,
        }
    }
}

fn default_true() -> bool { true }
fn default_reflect_model() -> String { "claude-haiku-4-5-20251001".to_string() }
fn default_providers() -> Vec<String> {
    vec!["playbook".into(), "episodes".into(), "tool_patterns".into()]
}
fn default_100() -> usize { 100 }
fn default_2000() -> usize { 2000 }
fn default_20() -> usize { 20 }
fn default_1000() -> usize { 1000 }
fn default_500() -> usize { 500 }

/// Load memory config from ~/.infinity/memory.json (creates default if missing).
pub fn load_memory_config() -> MemoryConfig {
    let path = memory_config_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => MemoryConfig::default(),
    }
}

fn memory_config_path() -> PathBuf {
    let mut path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push(".infinity");
    path.push("memory.json");
    path
}

// ── MemoryManager ───────────────────────────────────────────────────

/// Orchestrates all memory providers, tier management, search, and compaction.
pub struct MemoryManager {
    providers: Vec<Box<dyn MemoryProvider>>,
    reflector: Option<reflector::Reflector>,
    tier_manager: Arc<TierManager>,
    search_engine: Arc<MemorySearchEngine>,
    compaction: Arc<CompactionEngine>,
    #[allow(dead_code)]
    config: MemoryConfig,
}

impl MemoryManager {
    /// Create a new memory manager from config, using the given DB for KV storage.
    pub async fn from_config(config: MemoryConfig, db: Arc<AgentFS>) -> Result<Self> {
        // Create tier manager and search engine from DB's writer/reader pool
        let writer = db.writer().clone();
        let readers = db.readers().clone();

        let tier_manager = Arc::new(TierManager::new(
            writer.clone(),
            readers.clone(),
            config.tiers.clone(),
        ));

        let search_engine = Arc::new(MemorySearchEngine::new(
            writer.clone(),
            readers.clone(),
        ));

        // Create KV store for compaction engine
        let kv = KvStore::new(writer.clone(), readers.clone());

        let compaction = Arc::new(CompactionEngine::new(
            writer,
            readers,
            kv,
            Arc::clone(&tier_manager),
            Arc::clone(&search_engine),
            config.compaction.clone(),
        ));

        let mut providers: Vec<Box<dyn MemoryProvider>> = Vec::new();

        for name in &config.providers {
            match name.as_str() {
                "playbook" => {
                    let provider = playbook::PlaybookProvider::new(
                        Arc::clone(&db),
                        config.playbook.clone(),
                    ).with_tier_and_search(
                        Arc::clone(&tier_manager),
                        Arc::clone(&search_engine),
                    );
                    providers.push(Box::new(provider));
                }
                "episodes" => {
                    let provider = episodes::EpisodeProvider::new(
                        Arc::clone(&db),
                        config.episodes.clone(),
                    ).with_tier_and_search(
                        Arc::clone(&tier_manager),
                        Arc::clone(&search_engine),
                    );
                    providers.push(Box::new(provider));
                }
                "tool_patterns" => {
                    let provider = tool_patterns::ToolPatternProvider::new(
                        Arc::clone(&db),
                        config.tool_patterns.clone(),
                    ).with_tier_and_search(
                        Arc::clone(&tier_manager),
                        Arc::clone(&search_engine),
                    );
                    providers.push(Box::new(provider));
                }
                other => {
                    tracing::warn!("Unknown memory provider: {other}, skipping");
                }
            }
        }

        let reflector = if config.reflect {
            Some(reflector::Reflector::new(config.reflect_model.clone()))
        } else {
            None
        };

        Ok(Self {
            providers,
            reflector,
            tier_manager,
            search_engine,
            compaction,
            config,
        })
    }

    /// Get combined context from all providers to inject into system prompt.
    pub async fn context_for_prompt(&self, query: &str) -> String {
        let mut sections = Vec::new();

        for provider in &self.providers {
            match provider.context_for_prompt(query).await {
                Ok(Some(ctx)) if !ctx.is_empty() => sections.push(ctx),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Memory provider {} error: {e}", provider.name());
                }
            }
        }

        if sections.is_empty() {
            return String::new();
        }

        format!("\n\n<memory>\n{}\n</memory>", sections.join("\n\n"))
    }

    /// Notify all providers of session start, then rebalance tiers.
    pub async fn on_session_start(&self, session_id: &str) {
        for provider in &self.providers {
            if let Err(e) = provider.on_session_start(session_id).await {
                tracing::warn!("Memory provider {} session_start error: {e}", provider.name());
            }
        }

        // Rebalance tiers at session start
        if let Err(e) = self.tier_manager.rebalance().await {
            tracing::warn!("Tier rebalance error at session start: {e}");
        }
    }

    /// Notify all providers of session end, then run compaction cycle.
    pub async fn on_session_end(&self, session_id: &str) {
        for provider in &self.providers {
            if let Err(e) = provider.on_session_end(session_id).await {
                tracing::warn!("Memory provider {} session_end error: {e}", provider.name());
            }
        }

        // Run compaction cycle at session end
        match self.compaction.run_cycle().await {
            Ok(report) => {
                if report.duplicates_removed > 0 || report.episodes_compressed > 0 {
                    tracing::info!("{report}");
                }
            }
            Err(e) => {
                tracing::warn!("Compaction error at session end: {e}");
            }
        }
    }

    /// Run reflection on a turn and feed results to providers.
    /// Returns true if reflection was triggered.
    pub async fn reflect(
        &self,
        auth: &mut crate::auth::AuthProvider,
        messages: &[crate::api::Message],
        tool_results: &[serde_json::Value],
        session_id: &str,
    ) -> bool {
        let reflector = match &self.reflector {
            Some(r) => r,
            None => return false,
        };

        // Check triggers: only reflect when worthwhile
        if !reflector.should_reflect(messages, tool_results) {
            return false;
        }

        match reflector.reflect_on_turn(auth, messages, tool_results, session_id).await {
            Ok(reflection) => {
                for provider in &self.providers {
                    if let Err(e) = provider.on_reflection(&reflection).await {
                        tracing::warn!(
                            "Memory provider {} reflection error: {e}",
                            provider.name()
                        );
                    }
                }
                true
            }
            Err(e) => {
                tracing::warn!("Reflection failed: {e}");
                false
            }
        }
    }

    /// Search memory using BM25.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.search_engine.search_bm25(query, None, limit).await
    }

    /// Run a manual compaction cycle.
    pub async fn compact(&self) -> Result<CompactionReport> {
        self.compaction.run_cycle().await
    }

    /// Get tier distribution counts (hot, warm, cold).
    pub async fn tier_counts(&self) -> Result<(usize, usize, usize)> {
        self.tier_manager.tier_counts().await
    }

    /// Get memory pressure level.
    pub async fn memory_pressure(&self) -> Result<MemoryPressure> {
        self.tier_manager.memory_pressure().await
    }

    /// Rebuild the FTS index from scratch.
    #[allow(dead_code)]
    pub async fn rebuild_search_index(&self) -> Result<usize> {
        self.search_engine.rebuild_index().await
    }

    /// Get stats for display.
    #[allow(dead_code)]
    pub async fn stats(&self) -> Vec<(String, usize)> {
        let mut stats = Vec::new();
        for provider in &self.providers {
            stats.push((provider.name().to_string(), 0));
        }
        stats
    }

    /// Whether memory is enabled.
    #[allow(dead_code)]
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Provider count.
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    /// Get a reference to providers for stats queries.
    pub fn providers(&self) -> &[Box<dyn MemoryProvider>] {
        &self.providers
    }

    /// Check if reflection is configured.
    pub fn has_reflector(&self) -> bool {
        self.reflector.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let cfg = MemoryConfig::default();
        assert!(cfg.enabled);
        assert!(cfg.reflect);
        assert_eq!(cfg.providers.len(), 3);
        assert_eq!(cfg.playbook.max_entries, 100);
        assert_eq!(cfg.episodes.max_episodes, 20);
        assert_eq!(cfg.tiers.hot_budget, 30);
        assert_eq!(cfg.tiers.total_budget, 200);
        assert_eq!(cfg.compaction.cold_batch_size, 5);
    }

    #[test]
    fn deserialize_config() {
        let json = r#"{"enabled": true, "reflect": false, "providers": ["playbook"]}"#;
        let cfg: MemoryConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.enabled);
        assert!(!cfg.reflect);
        assert_eq!(cfg.providers, vec!["playbook"]);
        // Defaults for tiers and compaction
        assert_eq!(cfg.tiers.hot_budget, 30);
        assert!(cfg.compaction.dedup_enabled);
    }

    #[test]
    fn deserialize_config_with_tiers() {
        let json = r#"{"enabled": true, "tiers": {"hot_budget": 50, "total_budget": 300}}"#;
        let cfg: MemoryConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.tiers.hot_budget, 50);
        assert_eq!(cfg.tiers.total_budget, 300);
    }

    #[test]
    fn category_display() {
        assert_eq!(Category::Strategy.to_string(), "strategy");
        assert_eq!(Category::Mistake.to_string(), "mistake");
        assert_eq!(Category::Pattern.to_string(), "pattern");
    }
}
