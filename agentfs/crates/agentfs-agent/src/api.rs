use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::auth::AuthProvider;
use crate::error::{AgentError, Result};
use crate::streaming::{self, StreamEvent};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

/// A message in the conversation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Value,
}

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
            HeaderValue::from_static(API_VERSION),
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
            .post(API_URL)
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
