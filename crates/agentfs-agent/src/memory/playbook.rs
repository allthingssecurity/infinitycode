use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use agentfs_core::AgentFS;

use crate::error::Result;
use crate::memory::{
    Category, MemoryEntry, MemoryProvider, PlaybookConfig, Reflection,
};

use super::compaction::content_hash;
use super::search::MemorySearchEngine;
use super::tiers::TierManager;

const KV_PREFIX: &str = "memory:playbook:";

/// A single playbook entry — a learned strategy, mistake, or pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybookEntry {
    pub id: String,
    pub category: Category,
    pub content: String,
    pub helpful: i32,
    pub harmful: i32,
    pub source_session: String,
    pub created: String,
    pub updated: String,
}

impl PlaybookEntry {
    /// Net score: helpful - harmful.
    pub fn score(&self) -> i32 {
        self.helpful - self.harmful
    }
}

/// ACE-style playbook memory provider.
pub struct PlaybookProvider {
    db: Arc<AgentFS>,
    config: PlaybookConfig,
    /// In-memory cache of entries, loaded at session start.
    entries: RwLock<Vec<PlaybookEntry>>,
    /// Optional tier manager for access tracking and metadata.
    tier_manager: Option<Arc<TierManager>>,
    /// Optional search engine for FTS indexing.
    search_engine: Option<Arc<MemorySearchEngine>>,
}

impl PlaybookProvider {
    pub fn new(db: Arc<AgentFS>, config: PlaybookConfig) -> Self {
        Self {
            db,
            config,
            entries: RwLock::new(Vec::new()),
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

    /// Load all playbook entries from KV.
    async fn load_entries(&self) -> Result<Vec<PlaybookEntry>> {
        let kv_entries = self
            .db
            .kv
            .list_prefix(KV_PREFIX)
            .await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let mut entries = Vec::new();
        for kv in kv_entries {
            match serde_json::from_str::<PlaybookEntry>(&kv.value) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    tracing::warn!("Failed to parse playbook entry {}: {e}", kv.key);
                }
            }
        }
        Ok(entries)
    }

    /// Save a single entry to KV, and update metadata + FTS index.
    async fn save_entry(&self, entry: &PlaybookEntry) -> Result<()> {
        let key = format!("{KV_PREFIX}{}", entry.id);
        let value = serde_json::to_string(entry)
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        self.db
            .kv
            .set(&key, &value)
            .await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        // Update tier metadata
        if let Some(ref tm) = self.tier_manager {
            let hash = content_hash(&value);
            let _ = tm.ensure_metadata(&key, "playbook", Some(&hash), value.len() as i64).await;
        }

        // Index in FTS
        if let Some(ref se) = self.search_engine {
            let _ = se.index_entry(&key, "playbook", &entry.content).await;
        }

        Ok(())
    }

    /// Generate the next entry ID.
    fn next_id(entries: &[PlaybookEntry]) -> String {
        let max_num = entries
            .iter()
            .filter_map(|e| e.id.strip_prefix("str-").and_then(|n| n.parse::<u32>().ok()))
            .max()
            .unwrap_or(0);
        format!("str-{:05}", max_num + 1)
    }

    /// Format entries for system prompt injection.
    pub(crate) fn format_for_prompt(entries: &[PlaybookEntry], budget: usize) -> String {
        let mut sorted: Vec<&PlaybookEntry> = entries.iter().filter(|e| e.score() > 0).collect();
        sorted.sort_by(|a, b| b.score().cmp(&a.score()));

        let mut sections: Vec<String> = Vec::new();
        let mut total_len = 0;

        // Group by category
        for (label, cat) in [
            ("STRATEGIES", Category::Strategy),
            ("MISTAKES TO AVOID", Category::Mistake),
            ("PATTERNS & CONVENTIONS", Category::Pattern),
        ] {
            let items: Vec<String> = sorted
                .iter()
                .filter(|e| e.category == cat)
                .map(|e| format!("- {} [score: {}]", e.content, e.score()))
                .collect();

            if !items.is_empty() {
                let section = format!("### {label}\n{}", items.join("\n"));
                if total_len + section.len() > budget {
                    break;
                }
                total_len += section.len();
                sections.push(section);
            }
        }

        if sections.is_empty() {
            return String::new();
        }

        format!("<playbook>\n{}\n</playbook>", sections.join("\n\n"))
    }

    /// Get entry count.
    #[allow(dead_code)]
    pub async fn entry_count(&self) -> usize {
        self.entries.read().await.len()
    }

    /// Get top entries for display.
    #[allow(dead_code)]
    pub async fn top_entries(&self, limit: usize) -> Vec<PlaybookEntry> {
        let entries = self.entries.read().await;
        let mut sorted: Vec<PlaybookEntry> = entries.clone();
        sorted.sort_by(|a, b| b.score().cmp(&a.score()));
        sorted.truncate(limit);
        sorted
    }
}

#[async_trait]
impl MemoryProvider for PlaybookProvider {
    fn name(&self) -> &str {
        "playbook"
    }

    async fn context_for_prompt(&self, _query: &str) -> Result<Option<String>> {
        let entries = self.entries.read().await;

        // Record access for entries included in prompt
        if let Some(ref tm) = self.tier_manager {
            for entry in entries.iter().filter(|e| e.score() > 0) {
                let key = format!("{KV_PREFIX}{}", entry.id);
                let _ = tm.record_access(&key).await;
            }
        }

        let formatted = Self::format_for_prompt(&entries, self.config.prompt_budget_chars);
        if formatted.is_empty() {
            Ok(None)
        } else {
            Ok(Some(formatted))
        }
    }

    async fn store(&self, entry: MemoryEntry) -> Result<()> {
        let pb_entry = PlaybookEntry {
            id: entry.id,
            category: serde_json::from_value(
                entry.metadata.get("category").cloned().unwrap_or_default(),
            )
            .unwrap_or(Category::Pattern),
            content: entry.content,
            helpful: 1,
            harmful: 0,
            source_session: entry
                .metadata
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            created: entry.created.clone(),
            updated: entry.created,
        };

        self.save_entry(&pb_entry).await?;
        self.entries.write().await.push(pb_entry);
        Ok(())
    }

    async fn on_reflection(&self, reflection: &Reflection) -> Result<()> {
        let mut entries = self.entries.write().await;
        let now = chrono::Utc::now().to_rfc3339();

        // Bump helpful scores
        for id in &reflection.helpful_ids {
            if let Some(entry) = entries.iter_mut().find(|e| &e.id == id) {
                entry.helpful += 1;
                entry.updated = now.clone();
                let key = format!("{KV_PREFIX}{}", entry.id);
                if let Ok(val) = serde_json::to_string(entry) {
                    let _ = self.db.kv.set(&key, &val).await;
                    // Update FTS
                    if let Some(ref se) = self.search_engine {
                        let _ = se.index_entry(&key, "playbook", &entry.content).await;
                    }
                }
            }
        }

        // Bump harmful scores
        for id in &reflection.harmful_ids {
            if let Some(entry) = entries.iter_mut().find(|e| &e.id == id) {
                entry.harmful += 1;
                entry.updated = now.clone();
                let key = format!("{KV_PREFIX}{}", entry.id);
                if let Ok(val) = serde_json::to_string(entry) {
                    let _ = self.db.kv.set(&key, &val).await;
                }
            }
        }

        // Store new learnings
        for learning in &reflection.learnings {
            if learning.confidence < 0.5 {
                continue;
            }

            // Content-hash dedup check
            if let Some(ref tm) = self.tier_manager {
                let hash = content_hash(&learning.content);
                if let Ok(Some(_existing)) = tm.has_content_hash(&hash).await {
                    tracing::debug!("Skipping duplicate playbook entry (content hash match)");
                    continue;
                }
            }

            // Check for duplicate content (case-insensitive fallback)
            let already_exists = entries
                .iter()
                .any(|e| e.content.to_lowercase() == learning.content.to_lowercase());
            if already_exists {
                continue;
            }

            // Enforce max entries
            if entries.len() >= self.config.max_entries {
                // Remove lowest-scoring entry
                if let Some(min_idx) = entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| e.score())
                    .map(|(i, _)| i)
                {
                    let removed = entries.remove(min_idx);
                    let key = format!("{KV_PREFIX}{}", removed.id);
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

            let id = Self::next_id(&entries);
            let entry = PlaybookEntry {
                id,
                category: learning.category.clone(),
                content: learning.content.clone(),
                helpful: 1,
                harmful: 0,
                source_session: reflection.session_id.clone(),
                created: now.clone(),
                updated: now.clone(),
            };

            if let Err(e) = self.save_entry(&entry).await {
                tracing::warn!("Failed to save playbook entry: {e}");
                continue;
            }
            entries.push(entry);
        }

        Ok(())
    }

    async fn on_session_start(&self, _session_id: &str) -> Result<()> {
        let loaded = self.load_entries().await?;

        // Ensure metadata and FTS index for all loaded entries
        if self.tier_manager.is_some() || self.search_engine.is_some() {
            for entry in &loaded {
                let key = format!("{KV_PREFIX}{}", entry.id);
                if let Ok(val) = serde_json::to_string(entry) {
                    if let Some(ref tm) = self.tier_manager {
                        let hash = content_hash(&val);
                        let _ = tm.ensure_metadata(&key, "playbook", Some(&hash), val.len() as i64).await;
                    }
                    if let Some(ref se) = self.search_engine {
                        let _ = se.index_entry(&key, "playbook", &entry.content).await;
                    }
                }
            }
        }

        *self.entries.write().await = loaded;
        Ok(())
    }

    async fn on_session_end(&self, _session_id: &str) -> Result<()> {
        // Entries are saved incrementally; nothing extra needed.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_score() {
        let entry = PlaybookEntry {
            id: "str-00001".into(),
            category: Category::Strategy,
            content: "test".into(),
            helpful: 5,
            harmful: 2,
            source_session: "s1".into(),
            created: String::new(),
            updated: String::new(),
        };
        assert_eq!(entry.score(), 3);
    }

    #[test]
    fn format_empty() {
        let result = PlaybookProvider::format_for_prompt(&[], 2000);
        assert!(result.is_empty());
    }

    #[test]
    fn format_with_entries() {
        let entries = vec![
            PlaybookEntry {
                id: "str-00001".into(),
                category: Category::Strategy,
                content: "Always check file exists".into(),
                helpful: 5,
                harmful: 0,
                source_session: "s1".into(),
                created: String::new(),
                updated: String::new(),
            },
            PlaybookEntry {
                id: "str-00002".into(),
                category: Category::Mistake,
                content: "Don't use rm -rf without checking".into(),
                helpful: 3,
                harmful: 0,
                source_session: "s1".into(),
                created: String::new(),
                updated: String::new(),
            },
        ];

        let result = PlaybookProvider::format_for_prompt(&entries, 2000);
        assert!(result.contains("<playbook>"));
        assert!(result.contains("STRATEGIES"));
        assert!(result.contains("MISTAKES TO AVOID"));
        assert!(result.contains("Always check file exists"));
    }

    #[test]
    fn next_id_from_empty() {
        assert_eq!(PlaybookProvider::next_id(&[]), "str-00001");
    }

    #[test]
    fn next_id_increments() {
        let entries = vec![PlaybookEntry {
            id: "str-00003".into(),
            category: Category::Strategy,
            content: "test".into(),
            helpful: 1,
            harmful: 0,
            source_session: "s1".into(),
            created: String::new(),
            updated: String::new(),
        }];
        assert_eq!(PlaybookProvider::next_id(&entries), "str-00004");
    }
}
