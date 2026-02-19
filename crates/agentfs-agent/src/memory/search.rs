use std::sync::Arc;

use serde::{Deserialize, Serialize};

use agentfs_core::connection::pool::{ReaderPool, WriterHandle};

// ── Types ──────────────────────────────────────────────────────────

/// A single BM25 search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub key: String,
    pub provider: String,
    pub snippet: String,
    pub bm25_score: f64,
    pub combined_score: f64,
}

// ── MemorySearchEngine ─────────────────────────────────────────────

/// FTS5-backed BM25 search engine for memory entries.
pub struct MemorySearchEngine {
    writer: Arc<WriterHandle>,
    readers: Arc<ReaderPool>,
}

impl MemorySearchEngine {
    pub fn new(writer: Arc<WriterHandle>, readers: Arc<ReaderPool>) -> Self {
        Self { writer, readers }
    }

    /// Index a memory entry in the FTS5 table.
    /// Uses INSERT OR REPLACE semantics to handle updates.
    pub async fn index_entry(
        &self,
        key: &str,
        provider: &str,
        content: &str,
    ) -> crate::error::Result<()> {
        let key = key.to_string();
        let provider = provider.to_string();
        let content = content.to_string();

        self.writer
            .with_conn(move |conn| {
                // Delete existing entry if any (FTS5 doesn't support ON CONFLICT)
                conn.execute(
                    "DELETE FROM memory_fts WHERE key = ?1",
                    [&key],
                )?;
                conn.execute(
                    "INSERT INTO memory_fts (key, provider, content) VALUES (?1, ?2, ?3)",
                    rusqlite::params![key, provider, content],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))
    }

    /// Remove an entry from the FTS5 index.
    pub async fn remove_entry(&self, key: &str) -> crate::error::Result<()> {
        let key = key.to_string();
        self.writer
            .with_conn(move |conn| {
                conn.execute("DELETE FROM memory_fts WHERE key = ?1", [&key])?;
                Ok(())
            })
            .await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))
    }

    /// Search using BM25 ranking.
    ///
    /// Returns results ranked by `-bm25(memory_fts)` (higher is more relevant).
    /// Optionally filter by provider.
    pub async fn search_bm25(
        &self,
        query: &str,
        provider_filter: Option<&str>,
        limit: usize,
    ) -> crate::error::Result<Vec<SearchResult>> {
        let reader = self.readers.acquire().await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let query = sanitize_fts_query(query);
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
            if let Some(provider) = provider_filter {
                (
                    "SELECT key, provider, snippet(memory_fts, 2, '»', '«', '…', 32) as snip,
                            -bm25(memory_fts) as rank
                     FROM memory_fts
                     WHERE memory_fts MATCH ?1 AND provider = ?2
                     ORDER BY rank DESC
                     LIMIT ?3".to_string(),
                    vec![
                        Box::new(query) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(provider.to_string()) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(limit as i64) as Box<dyn rusqlite::types::ToSql>,
                    ],
                )
            } else {
                (
                    "SELECT key, provider, snippet(memory_fts, 2, '»', '«', '…', 32) as snip,
                            -bm25(memory_fts) as rank
                     FROM memory_fts
                     WHERE memory_fts MATCH ?1
                     ORDER BY rank DESC
                     LIMIT ?2".to_string(),
                    vec![
                        Box::new(query) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(limit as i64) as Box<dyn rusqlite::types::ToSql>,
                    ],
                )
            };

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

        let mut stmt = reader.conn().prepare(&sql)
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let results = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(SearchResult {
                    key: row.get(0)?,
                    provider: row.get(1)?,
                    snippet: row.get(2)?,
                    bm25_score: row.get(3)?,
                    combined_score: 0.0, // Will be filled in by caller
                })
            })
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(results)
    }

    /// Search with combined scoring: bm25_score × (0.3 + 0.7 × normalized_memory_score).
    ///
    /// `memory_scores` maps key → normalized memory score in [0, 1].
    #[allow(dead_code)]
    pub fn apply_combined_scoring(
        results: &mut [SearchResult],
        memory_scores: &std::collections::HashMap<String, f64>,
    ) {
        for result in results.iter_mut() {
            let mem_score = memory_scores.get(&result.key).copied().unwrap_or(0.5);
            // Clamp to [0, 1]
            let normalized = mem_score.clamp(0.0, 1.0);
            result.combined_score = result.bm25_score * (0.3 + 0.7 * normalized);
        }
        // Re-sort by combined score
        results.sort_by(|a, b| {
            b.combined_score
                .partial_cmp(&a.combined_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Rebuild the entire FTS5 index from kv_store memory entries.
    #[allow(dead_code)]
    pub async fn rebuild_index(&self) -> crate::error::Result<usize> {
        let reader = self.readers.acquire().await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        // Collect all memory KV entries
        let mut stmt = reader.conn().prepare(
            "SELECT key, value FROM kv_store WHERE key LIKE 'memory:%' ORDER BY key",
        ).map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        let entries: Vec<(String, String)> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        drop(stmt);
        drop(reader);

        // Clear existing index
        self.writer
            .with_conn(|conn| {
                conn.execute("DELETE FROM memory_fts", [])?;
                Ok(())
            })
            .await
            .map_err(|e| crate::error::AgentError::Memory(e.to_string()))?;

        // Re-index each entry
        let mut indexed = 0;
        for (key, value) in &entries {
            let provider = extract_provider(key);
            let content = extract_searchable_content(value);
            if !content.is_empty() {
                self.index_entry(key, &provider, &content).await?;
                indexed += 1;
            }
        }

        Ok(indexed)
    }
}

/// Sanitize a query for FTS5 MATCH — escape special characters.
fn sanitize_fts_query(query: &str) -> String {
    // FTS5 uses double-quotes for phrase queries and special operators.
    // Wrap each word in double quotes to treat them as literals.
    query
        .split_whitespace()
        .map(|word| {
            // Strip any FTS5 operators
            let clean: String = word.chars().filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-').collect();
            if clean.is_empty() {
                String::new()
            } else {
                format!("\"{}\"", clean)
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract the provider name from a KV key like "memory:playbook:str-00001".
#[allow(dead_code)]
fn extract_provider(key: &str) -> String {
    if let Some(rest) = key.strip_prefix("memory:") {
        if let Some(idx) = rest.find(':') {
            return rest[..idx].to_string();
        }
    }
    "unknown".to_string()
}

/// Extract searchable text content from a JSON value string.
#[allow(dead_code)]
fn extract_searchable_content(json_value: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_value) {
        let mut parts = Vec::new();

        // Extract common fields
        if let Some(content) = v.get("content").and_then(|v| v.as_str()) {
            parts.push(content.to_string());
        }
        if let Some(summary) = v.get("summary").and_then(|v| v.as_str()) {
            parts.push(summary.to_string());
        }
        if let Some(outcome) = v.get("outcome").and_then(|v| v.as_str()) {
            parts.push(outcome.to_string());
        }
        if let Some(category) = v.get("category").and_then(|v| v.as_str()) {
            parts.push(category.to_string());
        }

        // Tool patterns: extract pattern text
        if let Some(patterns) = v.get("patterns").and_then(|v| v.as_array()) {
            for p in patterns {
                if let Some(text) = p.get("pattern").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
            }
        }

        // Key decisions
        if let Some(decisions) = v.get("key_decisions").and_then(|v| v.as_array()) {
            for d in decisions {
                if let Some(text) = d.as_str() {
                    parts.push(text.to_string());
                }
            }
        }

        // Common errors
        if let Some(errors) = v.get("common_errors").and_then(|v| v.as_array()) {
            for e in errors {
                if let Some(text) = e.get("error").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
            }
        }

        parts.join(" ")
    } else {
        // Not JSON, use raw text
        json_value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_query_basic() {
        let result = sanitize_fts_query("error handling");
        assert_eq!(result, "\"error\" \"handling\"");
    }

    #[test]
    fn sanitize_query_strips_special() {
        let result = sanitize_fts_query("NOT foo OR bar*");
        assert_eq!(result, "\"NOT\" \"foo\" \"OR\" \"bar\"");
    }

    #[test]
    fn sanitize_empty_query() {
        let result = sanitize_fts_query("   ");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_provider_playbook() {
        assert_eq!(extract_provider("memory:playbook:str-00001"), "playbook");
    }

    #[test]
    fn extract_provider_episode() {
        assert_eq!(extract_provider("memory:episode:abc-123"), "episode");
    }

    #[test]
    fn extract_provider_tool() {
        assert_eq!(extract_provider("memory:tool_pattern:bash"), "tool_pattern");
    }

    #[test]
    fn extract_content_from_playbook_json() {
        let json = r#"{"content":"Always check file exists","category":"strategy","helpful":5}"#;
        let content = extract_searchable_content(json);
        assert!(content.contains("Always check file exists"));
        assert!(content.contains("strategy"));
    }

    #[test]
    fn extract_content_from_episode_json() {
        let json = r#"{"summary":"Built a REST API","outcome":"success","key_decisions":["chose tower"]}"#;
        let content = extract_searchable_content(json);
        assert!(content.contains("Built a REST API"));
        assert!(content.contains("success"));
        assert!(content.contains("chose tower"));
    }

    #[test]
    fn combined_scoring() {
        let mut results = vec![
            SearchResult {
                key: "a".into(),
                provider: "playbook".into(),
                snippet: "test".into(),
                bm25_score: 1.0,
                combined_score: 0.0,
            },
            SearchResult {
                key: "b".into(),
                provider: "playbook".into(),
                snippet: "test".into(),
                bm25_score: 0.5,
                combined_score: 0.0,
            },
        ];

        let mut scores = std::collections::HashMap::new();
        scores.insert("a".into(), 0.2); // Low memory score
        scores.insert("b".into(), 1.0); // High memory score

        MemorySearchEngine::apply_combined_scoring(&mut results, &scores);

        // 'b' with high memory score should have a competitive combined score
        // a: 1.0 * (0.3 + 0.7 * 0.2) = 1.0 * 0.44 = 0.44
        // b: 0.5 * (0.3 + 0.7 * 1.0) = 0.5 * 1.0 = 0.50
        assert!(results[0].key == "b", "Entry with higher memory score should rank first");
    }
}
