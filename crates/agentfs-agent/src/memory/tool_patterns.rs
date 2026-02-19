use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use agentfs_core::AgentFS;

use crate::error::Result;
use crate::memory::{MemoryEntry, MemoryProvider, Reflection, ToolPatternsConfig};

use super::compaction::content_hash;
use super::search::MemorySearchEngine;
use super::tiers::TierManager;

const KV_PREFIX: &str = "memory:tool_pattern:";

/// A learned pattern for a specific tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPatternEntry {
    pub pattern: String,
    pub helpful: i32,
}

/// A common error observed with a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommonError {
    pub error: String,
    pub frequency: i32,
}

/// Per-tool learnings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPattern {
    pub tool: String,
    pub patterns: Vec<ToolPatternEntry>,
    pub common_errors: Vec<CommonError>,
}

/// DeepAgent-style tool memory provider.
pub struct ToolPatternProvider {
    db: Arc<AgentFS>,
    config: ToolPatternsConfig,
    /// Per-tool patterns, loaded at session start.
    patterns: RwLock<HashMap<String, ToolPattern>>,
    /// Tools used in the current session.
    session_tools: RwLock<HashSet<String>>,
    /// Optional tier manager for access tracking and metadata.
    tier_manager: Option<Arc<TierManager>>,
    /// Optional search engine for FTS indexing.
    search_engine: Option<Arc<MemorySearchEngine>>,
}

impl ToolPatternProvider {
    pub fn new(db: Arc<AgentFS>, config: ToolPatternsConfig) -> Self {
        Self {
            db,
            config,
            patterns: RwLock::new(HashMap::new()),
            session_tools: RwLock::new(HashSet::new()),
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

    /// Load all tool patterns from KV.
    async fn load_patterns(&self) -> Result<HashMap<String, ToolPattern>> {
        let kv_entries = self
            .db
            .kv
            .list_prefix(KV_PREFIX)
            .await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let mut patterns = HashMap::new();
        for kv in kv_entries {
            match serde_json::from_str::<ToolPattern>(&kv.value) {
                Ok(tp) => {
                    patterns.insert(tp.tool.clone(), tp);
                }
                Err(e) => {
                    tracing::warn!("Failed to parse tool pattern {}: {e}", kv.key);
                }
            }
        }
        Ok(patterns)
    }

    /// Save a tool pattern to KV, and update metadata + FTS index.
    async fn save_pattern(&self, pattern: &ToolPattern) -> Result<()> {
        let key = format!("{KV_PREFIX}{}", pattern.tool);
        let value = serde_json::to_string(pattern)
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        self.db
            .kv
            .set(&key, &value)
            .await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        // Update tier metadata
        if let Some(ref tm) = self.tier_manager {
            let hash = content_hash(&value);
            let _ = tm.ensure_metadata(&key, "tool_patterns", Some(&hash), value.len() as i64).await;
        }

        // Index in FTS
        if let Some(ref se) = self.search_engine {
            let searchable_parts: Vec<String> = pattern
                .patterns
                .iter()
                .map(|p| p.pattern.clone())
                .chain(pattern.common_errors.iter().map(|e| e.error.clone()))
                .collect();
            let searchable = format!("{} {}", pattern.tool, searchable_parts.join(" "));
            let _ = se.index_entry(&key, "tool_patterns", &searchable).await;
        }

        Ok(())
    }

    /// Format tool tips for system prompt.
    fn format_for_prompt(
        patterns: &HashMap<String, ToolPattern>,
        session_tools: &HashSet<String>,
        budget: usize,
    ) -> String {
        if patterns.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        let mut total_len = 0;

        // Show tips for tools used in this session, plus top general tips
        let relevant_tools: Vec<&String> = if session_tools.is_empty() {
            patterns.keys().collect()
        } else {
            // Prioritize session tools, then include others
            let mut tools: Vec<&String> = session_tools
                .iter()
                .filter(|t| patterns.contains_key(*t))
                .collect();
            for k in patterns.keys() {
                if !session_tools.contains(k) {
                    tools.push(k);
                }
            }
            tools
        };

        for tool_name in relevant_tools {
            if let Some(tp) = patterns.get(tool_name) {
                let mut tool_tips = Vec::new();

                // Top patterns by helpful score
                let mut sorted_patterns: Vec<&ToolPatternEntry> = tp.patterns.iter().collect();
                sorted_patterns.sort_by(|a, b| b.helpful.cmp(&a.helpful));

                for p in sorted_patterns.iter().take(3) {
                    tool_tips.push(format!("  tip: {}", p.pattern));
                }

                // Top errors by frequency
                let mut sorted_errors: Vec<&CommonError> = tp.common_errors.iter().collect();
                sorted_errors.sort_by(|a, b| b.frequency.cmp(&a.frequency));

                for e in sorted_errors.iter().take(2) {
                    tool_tips.push(format!("  watch: {}", e.error));
                }

                if !tool_tips.is_empty() {
                    let section = format!("{}:\n{}", tool_name, tool_tips.join("\n"));
                    if total_len + section.len() > budget {
                        break;
                    }
                    total_len += section.len();
                    lines.push(section);
                }
            }
        }

        if lines.is_empty() {
            return String::new();
        }

        format!("<tool_tips>\n{}\n</tool_tips>", lines.join("\n"))
    }

    /// Track that a tool was used in this session.
    #[allow(dead_code)]
    pub async fn track_tool_use(&self, tool: &str) {
        self.session_tools.write().await.insert(tool.to_string());
    }

    /// Get total pattern count across all tools.
    #[allow(dead_code)]
    pub async fn pattern_count(&self) -> usize {
        let patterns = self.patterns.read().await;
        patterns.values().map(|tp| tp.patterns.len()).sum()
    }
}

#[async_trait]
impl MemoryProvider for ToolPatternProvider {
    fn name(&self) -> &str {
        "tool_patterns"
    }

    async fn context_for_prompt(&self, _query: &str) -> Result<Option<String>> {
        let patterns = self.patterns.read().await;
        let session_tools = self.session_tools.read().await;

        // Record access for tool patterns included in prompt
        if let Some(ref tm) = self.tier_manager {
            for tool_name in session_tools.iter() {
                if patterns.contains_key(tool_name) {
                    let key = format!("{KV_PREFIX}{}", tool_name);
                    let _ = tm.record_access(&key).await;
                }
            }
        }

        let formatted =
            Self::format_for_prompt(&patterns, &session_tools, self.config.prompt_budget_chars);
        if formatted.is_empty() {
            Ok(None)
        } else {
            Ok(Some(formatted))
        }
    }

    async fn store(&self, _entry: MemoryEntry) -> Result<()> {
        // Tool patterns are updated via on_reflection, not direct store.
        Ok(())
    }

    async fn on_reflection(&self, reflection: &Reflection) -> Result<()> {
        let mut patterns = self.patterns.write().await;

        for obs in &reflection.tool_observations {
            let tp = patterns
                .entry(obs.tool.clone())
                .or_insert_with(|| ToolPattern {
                    tool: obs.tool.clone(),
                    patterns: Vec::new(),
                    common_errors: Vec::new(),
                });

            // If there's a pattern observation, update or add it
            if let Some(pattern_text) = &obs.pattern {
                if let Some(existing) = tp
                    .patterns
                    .iter_mut()
                    .find(|p| p.pattern == *pattern_text)
                {
                    existing.helpful += 1;
                } else {
                    tp.patterns.push(ToolPatternEntry {
                        pattern: pattern_text.clone(),
                        helpful: 1,
                    });
                }
            }

            // If there's an error, track it
            if let Some(error_text) = &obs.error {
                if let Some(existing) = tp
                    .common_errors
                    .iter_mut()
                    .find(|e| e.error == *error_text)
                {
                    existing.frequency += 1;
                } else {
                    tp.common_errors.push(CommonError {
                        error: error_text.clone(),
                        frequency: 1,
                    });
                }
            }

            // Save updated pattern
            if let Err(e) = self.save_pattern(tp).await {
                tracing::warn!("Failed to save tool pattern for {}: {e}", obs.tool);
            }

            // Track tool use
            self.session_tools.write().await.insert(obs.tool.clone());
        }

        Ok(())
    }

    async fn on_session_start(&self, _session_id: &str) -> Result<()> {
        let loaded = self.load_patterns().await?;

        // Ensure metadata and FTS index for all loaded patterns
        if self.tier_manager.is_some() || self.search_engine.is_some() {
            for (tool_name, pattern) in &loaded {
                let key = format!("{KV_PREFIX}{}", tool_name);
                if let Ok(val) = serde_json::to_string(pattern) {
                    if let Some(ref tm) = self.tier_manager {
                        let hash = content_hash(&val);
                        let _ = tm.ensure_metadata(&key, "tool_patterns", Some(&hash), val.len() as i64).await;
                    }
                    if let Some(ref se) = self.search_engine {
                        let searchable_parts: Vec<String> = pattern
                            .patterns
                            .iter()
                            .map(|p| p.pattern.clone())
                            .chain(pattern.common_errors.iter().map(|e| e.error.clone()))
                            .collect();
                        let searchable = format!("{} {}", pattern.tool, searchable_parts.join(" "));
                        let _ = se.index_entry(&key, "tool_patterns", &searchable).await;
                    }
                }
            }
        }

        *self.patterns.write().await = loaded;
        *self.session_tools.write().await = HashSet::new();
        Ok(())
    }

    async fn on_session_end(&self, _session_id: &str) -> Result<()> {
        // Patterns are saved incrementally; nothing extra needed.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_empty() {
        let result = ToolPatternProvider::format_for_prompt(
            &HashMap::new(),
            &HashSet::new(),
            500,
        );
        assert!(result.is_empty());
    }

    #[test]
    fn format_with_patterns() {
        let mut patterns = HashMap::new();
        patterns.insert(
            "bash".into(),
            ToolPattern {
                tool: "bash".into(),
                patterns: vec![
                    ToolPatternEntry {
                        pattern: "Use timeout for long commands".into(),
                        helpful: 5,
                    },
                    ToolPatternEntry {
                        pattern: "Quote paths with spaces".into(),
                        helpful: 3,
                    },
                ],
                common_errors: vec![CommonError {
                    error: "Command not found".into(),
                    frequency: 2,
                }],
            },
        );

        let mut session_tools = HashSet::new();
        session_tools.insert("bash".into());

        let result = ToolPatternProvider::format_for_prompt(&patterns, &session_tools, 500);
        assert!(result.contains("<tool_tips>"));
        assert!(result.contains("bash:"));
        assert!(result.contains("Use timeout"));
        assert!(result.contains("Command not found"));
    }
}
