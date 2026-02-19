use serde_json::Value;

use crate::api::Message;
use crate::auth::AuthProvider;
use crate::error::{AgentError, Result};
use crate::memory::{Category, Learning, Reflection, ToolObs};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

const REFLECTION_PROMPT: &str = r#"Analyze this conversation turn and extract learnings. Return a JSON object with:
- "learnings": array of {"category": "strategy"|"mistake"|"pattern", "content": "what was learned", "confidence": 0.0-1.0}
- "helpful_ids": array of playbook entry IDs that were helpful (if any referenced in context)
- "harmful_ids": array of playbook entry IDs that were wrong/misleading
- "tool_observations": array of {"tool": "tool_name", "success": true/false, "pattern": "optional tip", "error": "optional error description"}

Focus on:
1. What strategies worked or failed
2. Common mistakes made
3. Tool usage patterns (success/failure, tips)
4. User corrections (these indicate mistakes to avoid)

Be selective — only extract high-confidence learnings. Prefer 0-3 learnings per turn.
Return ONLY the JSON object, no other text."#;

/// The reflector analyzes turns and extracts learnings.
pub struct Reflector {
    model: String,
    client: reqwest::Client,
}

impl Reflector {
    pub fn new(model: String) -> Self {
        Self {
            model,
            client: reqwest::Client::new(),
        }
    }

    /// Check whether reflection should be triggered for this turn.
    pub fn should_reflect(&self, messages: &[Message], tool_results: &[Value]) -> bool {
        // Trigger 1: A tool call errored
        let has_tool_error = tool_results.iter().any(|r| {
            r.get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        });
        if has_tool_error {
            return true;
        }

        // Trigger 2: The user corrected the agent (heuristic: user message after assistant
        // message that contains correction-like language)
        if let Some(last_user) = messages.iter().rev().find(|m| m.role == "user") {
            if let Some(text) = last_user.content.as_str() {
                let lower = text.to_lowercase();
                let correction_signals = [
                    "no,", "wrong", "incorrect", "that's not", "don't",
                    "instead", "actually,", "fix", "not what I",
                ];
                if correction_signals.iter().any(|s| lower.contains(s)) {
                    return true;
                }
            }
        }

        // Trigger 3: More than 2 tool calls (complex enough to learn from)
        if tool_results.len() > 2 {
            return true;
        }

        false
    }

    /// Analyze a turn and produce a Reflection.
    pub async fn reflect_on_turn(
        &self,
        auth: &mut AuthProvider,
        messages: &[Message],
        tool_results: &[Value],
        session_id: &str,
    ) -> Result<Reflection> {
        // Build a condensed version of the turn for analysis
        let turn_summary = self.summarize_turn(messages, tool_results);

        let reflection_messages = vec![
            Message {
                role: "user".to_string(),
                content: Value::String(format!(
                    "{REFLECTION_PROMPT}\n\n<turn>\n{turn_summary}\n</turn>"
                )),
            },
        ];

        let response = self.call_api(auth, &reflection_messages).await?;
        self.parse_reflection(&response, session_id)
    }

    /// Make a non-streaming API call to the cheap model.
    async fn call_api(
        &self,
        auth: &mut AuthProvider,
        messages: &[Message],
    ) -> Result<String> {
        let auth_headers = auth.get_auth_headers().await?;

        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 1024,
            "messages": messages,
        });

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_static("anthropic-version"),
            reqwest::header::HeaderValue::from_static(API_VERSION),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("content-type"),
            reqwest::header::HeaderValue::from_static("application/json"),
        );

        for (key, value) in &auth_headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(key.as_bytes()),
                reqwest::header::HeaderValue::from_str(value),
            ) {
                headers.insert(name, val);
            }
        }

        let resp = self
            .client
            .post(API_URL)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentError::Memory(format!("Reflection API call failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::Memory(format!(
                "Reflection API error ({status}): {body}"
            )));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| AgentError::Memory(format!("Failed to parse reflection response: {e}")))?;

        // Extract text content from response
        let text = json["content"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|block| block["text"].as_str())
            .unwrap_or("")
            .to_string();

        Ok(text)
    }

    /// Build a condensed summary of the turn for reflection.
    fn summarize_turn(&self, messages: &[Message], tool_results: &[Value]) -> String {
        let mut parts = Vec::new();

        // Get the last few messages (user + assistant)
        let recent: Vec<&Message> = messages.iter().rev().take(4).collect();

        for msg in recent.iter().rev() {
            let role = &msg.role;
            let content = match &msg.content {
                Value::String(s) => {
                    if s.len() > 500 {
                        format!("{}...", &s[..500])
                    } else {
                        s.clone()
                    }
                }
                Value::Array(arr) => {
                    let mut text_parts = Vec::new();
                    for item in arr {
                        if let Some(t) = item.get("type").and_then(|v| v.as_str()) {
                            match t {
                                "text" => {
                                    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                                        let truncated = if text.len() > 300 {
                                            format!("{}...", &text[..300])
                                        } else {
                                            text.to_string()
                                        };
                                        text_parts.push(format!("[text] {truncated}"));
                                    }
                                }
                                "tool_use" => {
                                    let name = item
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?");
                                    text_parts.push(format!("[tool_use: {name}]"));
                                }
                                "tool_result" => {
                                    let is_error = item
                                        .get("is_error")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                    let content = item
                                        .get("content")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    let truncated = if content.len() > 200 {
                                        format!("{}...", &content[..200])
                                    } else {
                                        content.to_string()
                                    };
                                    let label = if is_error { "ERROR" } else { "ok" };
                                    text_parts.push(format!("[tool_result ({label}): {truncated}]"));
                                }
                                _ => {}
                            }
                        }
                    }
                    text_parts.join("\n")
                }
                _ => "[non-text content]".to_string(),
            };
            parts.push(format!("{role}: {content}"));
        }

        // Add tool results summary
        if !tool_results.is_empty() {
            let error_count = tool_results
                .iter()
                .filter(|r| {
                    r.get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .count();
            parts.push(format!(
                "\nTool calls: {} total, {} errors",
                tool_results.len(),
                error_count
            ));
        }

        parts.join("\n\n")
    }

    /// Parse the reflector's JSON response into a Reflection struct.
    fn parse_reflection(&self, response: &str, session_id: &str) -> Result<Reflection> {
        // Try to find JSON in the response (might have markdown fences)
        let json_str = if let Some(start) = response.find('{') {
            if let Some(end) = response.rfind('}') {
                &response[start..=end]
            } else {
                response
            }
        } else {
            response
        };

        let parsed: Value = serde_json::from_str(json_str)
            .map_err(|e| AgentError::Memory(format!("Failed to parse reflection JSON: {e}")))?;

        let learnings = parsed["learnings"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let category = match item["category"].as_str()? {
                            "strategy" => Category::Strategy,
                            "mistake" => Category::Mistake,
                            "pattern" => Category::Pattern,
                            _ => return None,
                        };
                        let content = item["content"].as_str()?.to_string();
                        let confidence = item["confidence"].as_f64().unwrap_or(0.5) as f32;
                        Some(Learning {
                            category,
                            content,
                            confidence,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let helpful_ids = parsed["helpful_ids"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let harmful_ids = parsed["harmful_ids"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let tool_observations = parsed["tool_observations"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let tool = item["tool"].as_str()?.to_string();
                        let success = item["success"].as_bool().unwrap_or(true);
                        let pattern = item["pattern"].as_str().map(String::from);
                        let error = item["error"].as_str().map(String::from);
                        Some(ToolObs {
                            tool,
                            success,
                            pattern,
                            error,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Reflection {
            learnings,
            helpful_ids,
            harmful_ids,
            tool_observations,
            session_id: session_id.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_reflector() -> Reflector {
        Reflector::new("claude-haiku-4-5-20251001".into())
    }

    #[test]
    fn should_reflect_on_tool_error() {
        let r = make_reflector();
        let messages = vec![];
        let tool_results = vec![serde_json::json!({"is_error": true, "content": "fail"})];
        assert!(r.should_reflect(&messages, &tool_results));
    }

    #[test]
    fn should_reflect_on_correction() {
        let r = make_reflector();
        let messages = vec![Message {
            role: "user".into(),
            content: Value::String("No, that's wrong. Try again.".into()),
        }];
        assert!(r.should_reflect(&messages, &[]));
    }

    #[test]
    fn should_reflect_on_many_tools() {
        let r = make_reflector();
        let tool_results = vec![
            serde_json::json!({"content": "ok"}),
            serde_json::json!({"content": "ok"}),
            serde_json::json!({"content": "ok"}),
        ];
        assert!(r.should_reflect(&[], &tool_results));
    }

    #[test]
    fn should_not_reflect_simple_turn() {
        let r = make_reflector();
        let messages = vec![Message {
            role: "user".into(),
            content: Value::String("What is 2+2?".into()),
        }];
        assert!(!r.should_reflect(&messages, &[]));
    }

    #[test]
    fn parse_reflection_json() {
        let r = make_reflector();
        let json = r#"{
            "learnings": [
                {"category": "strategy", "content": "Check file exists first", "confidence": 0.9}
            ],
            "helpful_ids": ["str-00001"],
            "harmful_ids": [],
            "tool_observations": [
                {"tool": "bash", "success": true, "pattern": "Use absolute paths"}
            ]
        }"#;

        let reflection = r.parse_reflection(json, "test-session").unwrap();
        assert_eq!(reflection.learnings.len(), 1);
        assert_eq!(reflection.learnings[0].category, Category::Strategy);
        assert_eq!(reflection.helpful_ids, vec!["str-00001"]);
        assert_eq!(reflection.tool_observations.len(), 1);
        assert_eq!(reflection.session_id, "test-session");
    }

    #[test]
    fn parse_reflection_with_markdown_fences() {
        let r = make_reflector();
        let json = "```json\n{\"learnings\": [], \"helpful_ids\": [], \"harmful_ids\": [], \"tool_observations\": []}\n```";
        let reflection = r.parse_reflection(json, "test").unwrap();
        assert!(reflection.learnings.is_empty());
    }
}
