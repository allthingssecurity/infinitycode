use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::auth::AuthProvider;
use crate::error::{AgentError, Result};
use crate::streaming::{self, StreamEvent};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const NVIDIA_API_URL: &str = "https://integrate.api.nvidia.com/v1/chat/completions";

/// A message in the conversation (Anthropic format internally).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Value,
}

// ── LlmClient — unified enum for all providers ─────────────────────

/// Unified LLM client that dispatches to the right provider.
pub enum LlmClient {
    Anthropic(AnthropicClient),
    Nvidia(NvidiaClient),
}

impl LlmClient {
    pub async fn stream_message(
        &self,
        auth: &mut AuthProvider,
        messages: &[Message],
        tools: &[Value],
        system: Option<&str>,
    ) -> Result<mpsc::Receiver<StreamEvent>> {
        match self {
            LlmClient::Anthropic(c) => c.stream_message(auth, messages, tools, system).await,
            LlmClient::Nvidia(c) => c.stream_message(messages, tools, system).await,
        }
    }
}

// ── Anthropic Client ────────────────────────────────────────────────

/// Anthropic API client with streaming support.
pub struct AnthropicClient {
    client: reqwest::Client,
    model: String,
    max_tokens: u32,
}

impl AnthropicClient {
    pub fn new(model: String, max_tokens: u32) -> Self {
        Self {
            client: reqwest::Client::new(),
            model,
            max_tokens,
        }
    }

    /// Send a streaming message request and return a channel of events.
    pub async fn stream_message(
        &self,
        auth: &mut AuthProvider,
        messages: &[Message],
        tools: &[Value],
        system: Option<&str>,
    ) -> Result<mpsc::Receiver<StreamEvent>> {
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "stream": true,
            "messages": messages,
        });

        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.to_vec());
        }
        if let Some(sys) = system {
            body["system"] = Value::String(sys.to_string());
        }

        // Try request, retry once on 401
        match self.do_stream_request(auth, &body).await {
            Ok(rx) => Ok(rx),
            Err(AgentError::Api { status: 401, .. }) => {
                tracing::info!("Got 401, attempting to re-authenticate");
                self.do_stream_request(auth, &body).await
            }
            Err(e) => Err(e),
        }
    }

    async fn do_stream_request(
        &self,
        auth: &mut AuthProvider,
        body: &Value,
    ) -> Result<mpsc::Receiver<StreamEvent>> {
        let auth_headers = auth.get_auth_headers().await?;

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static(ANTHROPIC_API_VERSION),
        );
        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );

        for (key, value) in &auth_headers {
            if let (Ok(name), Ok(val)) = (
                HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                headers.insert(name, val);
            }
        }

        let resp = self
            .client
            .post(ANTHROPIC_API_URL)
            .headers(headers)
            .json(body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::Api {
                status: status_code,
                message: body,
            });
        }

        // Spawn a task to parse SSE stream and send events through channel
        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let mut stream = resp.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(_) => break,
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = buffer.find("\n\n") {
                    let event_text = buffer[..pos].to_string();
                    buffer = buffer[pos + 2..].to_string();

                    if let Some(event) = streaming::parse_sse_event(&event_text) {
                        if tx.send(event).await.is_err() {
                            return; // receiver dropped
                        }
                    }
                }
            }

            // Process remaining
            if !buffer.trim().is_empty() {
                if let Some(event) = streaming::parse_sse_event(buffer.trim()) {
                    let _ = tx.send(event).await;
                }
            }
        });

        Ok(rx)
    }
}

// ── NVIDIA / OpenAI-compatible Client ───────────────────────────────

/// Client for NVIDIA NIM (Kimi K2.5) and any OpenAI-compatible API.
pub struct NvidiaClient {
    client: reqwest::Client,
    api_key: String,
    model: String,
    max_tokens: u32,
    base_url: String,
}

impl NvidiaClient {
    pub fn new(api_key: String, model: String, max_tokens: u32) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            model,
            max_tokens,
            base_url: NVIDIA_API_URL.to_string(),
        }
    }

    /// Override the base URL (for other OpenAI-compatible providers).
    #[allow(dead_code)]
    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }

    /// Send a streaming chat completion request.
    /// Converts Anthropic-format messages/tools to OpenAI format internally.
    pub async fn stream_message(
        &self,
        messages: &[Message],
        tools: &[Value],
        system: Option<&str>,
    ) -> Result<mpsc::Receiver<StreamEvent>> {
        // Build OpenAI-format messages
        let openai_messages = convert_messages_to_openai(messages, system);
        let openai_tools = convert_tools_to_openai(tools);

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "stream": true,
            "messages": openai_messages,
            "temperature": 0.6,
        });

        if !openai_tools.is_empty() {
            body["tools"] = Value::Array(openai_tools);
            body["tool_choice"] = json!("auto");
        }

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );
        let auth_val = format!("Bearer {}", self.api_key);
        headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&auth_val).map_err(|e| {
                AgentError::Config(format!("Invalid API key format: {e}"))
            })?,
        );

        let resp = self
            .client
            .post(&self.base_url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let body_text = resp.text().await.unwrap_or_default();
            return Err(AgentError::Api {
                status: status_code,
                message: body_text,
            });
        }

        // Emit a synthetic MessageStart (OpenAI doesn't send one)
        let (tx, rx) = mpsc::channel(64);
        let _ = tx
            .send(StreamEvent::MessageStart {
                id: "nvidia".to_string(),
                input_tokens: 0,
            })
            .await;

        // Also emit a ContentBlockStart for text (so accumulator works)
        let _ = tx
            .send(StreamEvent::ContentBlockStart {
                index: 0,
                block_type: streaming::ContentBlockType::Text,
            })
            .await;

        tokio::spawn(async move {
            let mut stream = resp.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(_) => break,
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // OpenAI SSE: each event is "data: ...\n\n"
                while let Some(pos) = buffer.find("\n\n") {
                    let event_text = buffer[..pos].to_string();
                    buffer = buffer[pos + 2..].to_string();

                    let events = streaming::parse_openai_sse_event(&event_text);
                    for event in events {
                        if tx.send(event).await.is_err() {
                            return;
                        }
                    }
                }

                // Also try single newline separation (some providers)
                while !buffer.contains("\n\n") {
                    if let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].to_string();
                        buffer = buffer[pos + 1..].to_string();
                        if line.trim().is_empty() {
                            continue;
                        }
                        let events = streaming::parse_openai_sse_event(&line);
                        for event in events {
                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                    } else {
                        break;
                    }
                }
            }

            // Process remaining
            if !buffer.trim().is_empty() {
                let events = streaming::parse_openai_sse_event(buffer.trim());
                for event in events {
                    let _ = tx.send(event).await;
                }
            }
        });

        Ok(rx)
    }
}

// ── Format conversion helpers ───────────────────────────────────────

/// Convert Anthropic-format messages to OpenAI chat messages.
fn convert_messages_to_openai(messages: &[Message], system: Option<&str>) -> Vec<Value> {
    let mut openai_msgs = Vec::new();

    // System message goes first as a message (not a top-level param)
    if let Some(sys) = system {
        openai_msgs.push(json!({
            "role": "system",
            "content": sys,
        }));
    }

    for msg in messages {
        match msg.role.as_str() {
            "user" => {
                // Check if content is an array of tool_results (Anthropic format)
                if let Some(arr) = msg.content.as_array() {
                    let has_tool_results = arr
                        .iter()
                        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"));

                    if has_tool_results {
                        // Convert each tool_result to a separate "tool" message
                        for block in arr {
                            if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                                let tool_call_id = block
                                    .get("tool_use_id")
                                    .and_then(|i| i.as_str())
                                    .unwrap_or("");
                                let content = block
                                    .get("content")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("");
                                openai_msgs.push(json!({
                                    "role": "tool",
                                    "tool_call_id": tool_call_id,
                                    "content": content,
                                }));
                            }
                        }
                        continue;
                    }
                }

                // Regular user message
                let content = if let Some(s) = msg.content.as_str() {
                    s.to_string()
                } else {
                    msg.content.to_string()
                };
                openai_msgs.push(json!({
                    "role": "user",
                    "content": content,
                }));
            }
            "assistant" => {
                // Check if content has tool_use blocks (Anthropic format)
                if let Some(arr) = msg.content.as_array() {
                    let mut text_parts = String::new();
                    let mut tool_calls: Vec<Value> = Vec::new();

                    for block in arr {
                        match block.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                    text_parts.push_str(t);
                                }
                            }
                            Some("tool_use") => {
                                let id = block
                                    .get("id")
                                    .and_then(|i| i.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let name = block
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let input = block.get("input").cloned().unwrap_or(json!({}));
                                tool_calls.push(json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": serde_json::to_string(&input).unwrap_or_default(),
                                    }
                                }));
                            }
                            _ => {}
                        }
                    }

                    let mut assistant_msg = json!({ "role": "assistant" });
                    if !text_parts.is_empty() {
                        assistant_msg["content"] = json!(text_parts);
                    }
                    if !tool_calls.is_empty() {
                        assistant_msg["tool_calls"] = Value::Array(tool_calls);
                    }
                    openai_msgs.push(assistant_msg);
                } else {
                    // Simple string content
                    openai_msgs.push(json!({
                        "role": "assistant",
                        "content": msg.content,
                    }));
                }
            }
            _ => {
                openai_msgs.push(json!({
                    "role": msg.role,
                    "content": msg.content,
                }));
            }
        }
    }

    openai_msgs
}

/// Convert Anthropic tool definitions to OpenAI function-calling format.
fn convert_tools_to_openai(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name")?.as_str()?;
            let description = tool.get("description").and_then(|d| d.as_str()).unwrap_or("");
            let input_schema = tool
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));

            Some(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": input_schema,
                }
            }))
        })
        .collect()
}
