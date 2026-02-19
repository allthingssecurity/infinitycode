use serde_json::Value;

/// Parsed SSE stream events from the Anthropic Messages API.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum StreamEvent {
    MessageStart {
        id: String,
        input_tokens: u64,
    },
    ContentBlockStart {
        index: u32,
        block_type: ContentBlockType,
    },
    TextDelta {
        index: u32,
        text: String,
    },
    InputJsonDelta {
        index: u32,
        partial_json: String,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        stop_reason: String,
        output_tokens: u64,
    },
    MessageStop,
    Ping,
    Error {
        message: String,
    },
}

/// Type of content block being streamed.
#[derive(Debug, Clone)]
pub enum ContentBlockType {
    Text,
    ToolUse { id: String, name: String },
}

/// Parse a raw SSE event string into a StreamEvent.
pub fn parse_sse_event(raw: &str) -> Option<StreamEvent> {
    let mut event_type = String::new();
    let mut data = String::new();

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("event: ") {
            event_type = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data: ") {
            data = rest.trim().to_string();
        } else if line.starts_with("data:") {
            // "data:" with no space
            data = line.strip_prefix("data:").unwrap_or("").trim().to_string();
        }
    }

    if event_type.is_empty() && data.is_empty() {
        return None;
    }

    match event_type.as_str() {
        "message_start" => {
            let v: Value = serde_json::from_str(&data).ok()?;
            let message = v.get("message")?;
            let id = message.get("id")?.as_str()?.to_string();
            let input_tokens = message
                .get("usage")
                .and_then(|u| u.get("input_tokens"))
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            Some(StreamEvent::MessageStart { id, input_tokens })
        }
        "content_block_start" => {
            let v: Value = serde_json::from_str(&data).ok()?;
            let index = v.get("index")?.as_u64()? as u32;
            let content_block = v.get("content_block")?;
            let block_type_str = content_block.get("type")?.as_str()?;

            let block_type = match block_type_str {
                "text" => ContentBlockType::Text,
                "tool_use" => {
                    let id = content_block.get("id")?.as_str()?.to_string();
                    let name = content_block.get("name")?.as_str()?.to_string();
                    ContentBlockType::ToolUse { id, name }
                }
                _ => return None,
            };

            Some(StreamEvent::ContentBlockStart { index, block_type })
        }
        "content_block_delta" => {
            let v: Value = serde_json::from_str(&data).ok()?;
            let index = v.get("index")?.as_u64()? as u32;
            let delta = v.get("delta")?;
            let delta_type = delta.get("type")?.as_str()?;

            match delta_type {
                "text_delta" => {
                    let text = delta.get("text")?.as_str()?.to_string();
                    Some(StreamEvent::TextDelta { index, text })
                }
                "input_json_delta" => {
                    let partial_json = delta.get("partial_json")?.as_str()?.to_string();
                    Some(StreamEvent::InputJsonDelta {
                        index,
                        partial_json,
                    })
                }
                _ => None,
            }
        }
        "content_block_stop" => {
            let v: Value = serde_json::from_str(&data).ok()?;
            let index = v.get("index")?.as_u64()? as u32;
            Some(StreamEvent::ContentBlockStop { index })
        }
        "message_delta" => {
            let v: Value = serde_json::from_str(&data).ok()?;
            let delta = v.get("delta")?;
            let stop_reason = delta
                .get("stop_reason")
                .and_then(|s| s.as_str())
                .unwrap_or("end_turn")
                .to_string();
            let output_tokens = v
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            Some(StreamEvent::MessageDelta {
                stop_reason,
                output_tokens,
            })
        }
        "message_stop" => Some(StreamEvent::MessageStop),
        "ping" => Some(StreamEvent::Ping),
        "error" => {
            let v: Value = serde_json::from_str(&data).ok()?;
            let message = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            Some(StreamEvent::Error { message })
        }
        _ => None,
    }
}

// ── OpenAI-compatible SSE parser ─────────────────────────────────────
// Used for NVIDIA NIM (Kimi K2.5) and any OpenAI-compatible provider.
// Maps OpenAI streaming chunks → our unified StreamEvent enum.

/// Parse an OpenAI-format SSE data line into StreamEvent(s).
/// OpenAI sends lines like:
///   data: {"id":"...","choices":[{"delta":{"content":"hi"},"index":0,"finish_reason":null}]}
///   data: [DONE]
pub fn parse_openai_sse_event(raw: &str) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    for line in raw.lines() {
        let data = if let Some(rest) = line.strip_prefix("data: ") {
            rest.trim()
        } else if let Some(rest) = line.strip_prefix("data:") {
            rest.trim()
        } else {
            continue;
        };

        if data == "[DONE]" {
            events.push(StreamEvent::MessageStop);
            continue;
        }

        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();

        // Extract usage if present (some providers send it in the final chunk)
        if let Some(usage) = v.get("usage") {
            let input_tokens = usage
                .get("prompt_tokens")
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            let output_tokens = usage
                .get("completion_tokens")
                .and_then(|t| t.as_u64())
                .unwrap_or(0);

            // Emit MessageStart with input tokens if we see usage
            if input_tokens > 0 {
                events.push(StreamEvent::MessageStart {
                    id: id.clone(),
                    input_tokens,
                });
            }
            if output_tokens > 0 {
                events.push(StreamEvent::MessageDelta {
                    stop_reason: "end_turn".to_string(),
                    output_tokens,
                });
            }
        }

        let choices = match v.get("choices").and_then(|c| c.as_array()) {
            Some(c) => c,
            None => continue,
        };

        for choice in choices {
            let finish_reason = choice
                .get("finish_reason")
                .and_then(|f| f.as_str())
                .unwrap_or("");

            if let Some(delta) = choice.get("delta") {
                // Text content
                if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                    if !content.is_empty() {
                        events.push(StreamEvent::TextDelta {
                            index: 0,
                            text: content.to_string(),
                        });
                    }
                }

                // Tool calls
                if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                    for tc in tool_calls {
                        let tc_index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;

                        // If we get id + function.name, it's the start of a new tool call
                        if let Some(tc_id) = tc.get("id").and_then(|i| i.as_str()) {
                            let func = tc.get("function").unwrap_or(&Value::Null);
                            let name = func
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string();
                            events.push(StreamEvent::ContentBlockStart {
                                index: tc_index + 1, // offset by 1 since 0 is text
                                block_type: ContentBlockType::ToolUse {
                                    id: tc_id.to_string(),
                                    name,
                                },
                            });
                            // Also emit any initial arguments
                            if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                                if !args.is_empty() {
                                    events.push(StreamEvent::InputJsonDelta {
                                        index: tc_index + 1,
                                        partial_json: args.to_string(),
                                    });
                                }
                            }
                        } else if let Some(func) = tc.get("function") {
                            // Continuation: just arguments delta
                            if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                                if !args.is_empty() {
                                    events.push(StreamEvent::InputJsonDelta {
                                        index: tc_index + 1,
                                        partial_json: args.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // Handle finish_reason
            match finish_reason {
                "stop" => {
                    events.push(StreamEvent::ContentBlockStop { index: 0 });
                    events.push(StreamEvent::MessageDelta {
                        stop_reason: "end_turn".to_string(),
                        output_tokens: 0,
                    });
                }
                "tool_calls" => {
                    // Close any open tool call blocks
                    events.push(StreamEvent::MessageDelta {
                        stop_reason: "tool_use".to_string(),
                        output_tokens: 0,
                    });
                }
                _ => {}
            }
        }
    }

    events
}

/// Accumulates streaming content blocks into complete content.
pub struct ContentAccumulator {
    /// Accumulated text content.
    pub text_blocks: Vec<String>,
    /// Accumulated tool use blocks.
    pub tool_use_blocks: Vec<ToolUseBlock>,
    /// Currently accumulating text.
    current_text: Option<(u32, String)>,
    /// Currently accumulating tool use JSON.
    current_tool: Option<(u32, String, String, String)>, // (index, id, name, json_parts)
}

/// A complete tool use block.
#[derive(Debug, Clone)]
pub struct ToolUseBlock {
    pub id: String,
    pub name: String,
    pub input: Value,
}

impl ContentAccumulator {
    pub fn new() -> Self {
        Self {
            text_blocks: Vec::new(),
            tool_use_blocks: Vec::new(),
            current_text: None,
            current_tool: None,
        }
    }

    /// Process a stream event and accumulate content.
    pub fn process(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::ContentBlockStart { index, block_type } => match block_type {
                ContentBlockType::Text => {
                    self.current_text = Some((*index, String::new()));
                }
                ContentBlockType::ToolUse { id, name } => {
                    self.current_tool =
                        Some((*index, id.clone(), name.clone(), String::new()));
                }
            },
            StreamEvent::TextDelta { text, .. } => {
                if let Some((_, ref mut buf)) = self.current_text {
                    buf.push_str(text);
                }
            }
            StreamEvent::InputJsonDelta { partial_json, .. } => {
                if let Some((_, _, _, ref mut json_buf)) = self.current_tool {
                    json_buf.push_str(partial_json);
                }
            }
            StreamEvent::ContentBlockStop { .. } => {
                if let Some((_, text)) = self.current_text.take() {
                    self.text_blocks.push(text);
                }
                if let Some((_, id, name, json_str)) = self.current_tool.take() {
                    let input = serde_json::from_str(&json_str).unwrap_or(Value::Object(
                        serde_json::Map::new(),
                    ));
                    self.tool_use_blocks.push(ToolUseBlock { id, name, input });
                }
            }
            _ => {}
        }
    }

    /// Get all accumulated text joined together.
    pub fn full_text(&self) -> String {
        self.text_blocks.join("")
    }
}
