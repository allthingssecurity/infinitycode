use serde_json::{json, Value};

use agentfs_core::analytics::TokenRecord;

use crate::api::{AnthropicClient, Message};
use crate::auth::AuthProvider;
use crate::display;
use crate::error::{AgentError, Result};
use crate::executor::ToolExecutor;
use crate::streaming::{ContentAccumulator, StreamEvent};
use crate::tools;

/// KV key prefix for persisted conversation messages.
const MESSAGES_KEY_PREFIX: &str = "session:messages:";

/// The agentic loop: prompt -> API -> stream -> tool_use -> execute -> loop.
pub struct Agent {
    client: AnthropicClient,
    executor: ToolExecutor,
    messages: Vec<Message>,
    tool_defs: Vec<Value>,
    system: Option<String>,
    session_id: String,
    model: String,
    total_input_tokens: u64,
    total_output_tokens: u64,
}

impl Agent {
    pub fn new(
        client: AnthropicClient,
        executor: ToolExecutor,
        system: Option<String>,
        session_id: String,
        model: String,
    ) -> Self {
        Self {
            client,
            executor,
            messages: Vec::new(),
            tool_defs: tools::tool_definitions(),
            system,
            session_id,
            model,
            total_input_tokens: 0,
            total_output_tokens: 0,
        }
    }

    /// Load persisted messages from a previous session.
    pub async fn load_messages(&mut self) -> Result<usize> {
        let key = format!("{MESSAGES_KEY_PREFIX}{}", self.session_id);
        match self.executor.db.kv.get(&key).await {
            Ok(entry) => {
                let msgs: Vec<Message> = serde_json::from_str(&entry.value)
                    .map_err(|e| AgentError::Other(format!("Failed to parse saved messages: {e}")))?;
                let count = msgs.len();
                self.messages = msgs;
                Ok(count)
            }
            Err(agentfs_core::error::AgentFSError::KeyNotFound { .. }) => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Persist current messages to KV store.
    async fn save_messages(&self) {
        let key = format!("{MESSAGES_KEY_PREFIX}{}", self.session_id);
        if let Ok(json) = serde_json::to_string(&self.messages) {
            let _ = self.executor.db.kv.set(&key, &json).await;
        }
    }

    /// Run a single turn: user message -> (possibly multiple) API calls until end_turn.
    pub async fn run_turn(&mut self, auth: &mut AuthProvider, user_input: &str) -> Result<String> {
        self.messages.push(Message {
            role: "user".to_string(),
            content: Value::String(user_input.to_string()),
        });

        let mut full_response = String::new();

        loop {
            // Show thinking spinner
            let spinner = display::Spinner::thinking();

            // Start streaming — returns a channel
            let rx_result = self
                .client
                .stream_message(
                    auth,
                    &self.messages,
                    &self.tool_defs,
                    self.system.as_deref(),
                )
                .await;

            // If the request itself failed, stop spinner and return error
            let mut rx = match rx_result {
                Ok(rx) => rx,
                Err(e) => {
                    spinner.stop().await;
                    return Err(e);
                }
            };

            // Process events one-by-one from the channel
            let mut accumulator = ContentAccumulator::new();
            let mut input_tokens = 0u64;
            let mut output_tokens = 0u64;
            let mut stop_reason = String::from("end_turn");
            let mut spinner_active = true;
            let mut spinner = Some(spinner);

            while let Some(event) = rx.recv().await {
                // Stop spinner on first content event
                if spinner_active {
                    match &event {
                        StreamEvent::TextDelta { .. }
                        | StreamEvent::ContentBlockStart { .. }
                        | StreamEvent::InputJsonDelta { .. } => {
                            if let Some(s) = spinner.take() {
                                s.stop().await;
                            }
                            spinner_active = false;
                        }
                        _ => {}
                    }
                }

                // Print live
                display::print_stream_event(&event);

                // Accumulate
                accumulator.process(&event);

                match &event {
                    StreamEvent::MessageStart {
                        input_tokens: it, ..
                    } => {
                        input_tokens = *it;
                    }
                    StreamEvent::MessageDelta {
                        stop_reason: sr,
                        output_tokens: ot,
                    } => {
                        stop_reason = sr.clone();
                        output_tokens = *ot;
                    }
                    StreamEvent::Error { message } => {
                        if let Some(s) = spinner.take() {
                            s.stop().await;
                        }
                        return Err(AgentError::Stream(message.clone()));
                    }
                    _ => {}
                }
            }

            // Ensure spinner is stopped
            if let Some(s) = spinner.take() {
                s.stop().await;
            }

            // Track tokens
            self.total_input_tokens += input_tokens;
            self.total_output_tokens += output_tokens;

            // Record token usage
            let _ = self
                .executor
                .db
                .analytics
                .record_usage(TokenRecord {
                    id: None,
                    session_id: Some(self.session_id.clone()),
                    tool_call_id: None,
                    model: self.model.clone(),
                    input_tokens: input_tokens as i64,
                    output_tokens: output_tokens as i64,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    cost_microcents: estimate_cost(&self.model, input_tokens, output_tokens),
                    recorded_at: None,
                })
                .await;

            // Build assistant message content
            let text = accumulator.full_text();
            full_response.push_str(&text);

            let mut content_blocks: Vec<Value> = Vec::new();

            if !text.is_empty() {
                content_blocks.push(json!({
                    "type": "text",
                    "text": text,
                }));
            }

            for tool in &accumulator.tool_use_blocks {
                content_blocks.push(json!({
                    "type": "tool_use",
                    "id": tool.id,
                    "name": tool.name,
                    "input": tool.input,
                }));
            }

            self.messages.push(Message {
                role: "assistant".to_string(),
                content: Value::Array(content_blocks),
            });

            // If tool_use, execute tools and loop
            if stop_reason == "tool_use" && !accumulator.tool_use_blocks.is_empty() {
                let mut tool_results: Vec<Value> = Vec::new();

                for tool in &accumulator.tool_use_blocks {
                    display::print_tool_call(tool);

                    let tool_spinner = display::tool_spinner(&tool.name);
                    let result = self.executor.execute(&tool.name, &tool.input).await;
                    tool_spinner.stop().await;

                    match result {
                        Ok(output) => {
                            display::print_tool_result(&tool.name, &output, false);
                            tool_results.push(json!({
                                "type": "tool_result",
                                "tool_use_id": tool.id,
                                "content": output,
                            }));
                        }
                        Err(e) => {
                            let error_msg = e.to_string();
                            display::print_tool_result(&tool.name, &error_msg, true);
                            tool_results.push(json!({
                                "type": "tool_result",
                                "tool_use_id": tool.id,
                                "content": error_msg,
                                "is_error": true,
                            }));
                        }
                    }
                }

                self.messages.push(Message {
                    role: "user".to_string(),
                    content: Value::Array(tool_results),
                });

                println!();
                continue;
            }

            // End of turn
            display::print_token_usage(input_tokens, output_tokens);
            break;
        }

        // Persist after each turn
        self.save_messages().await;

        Ok(full_response)
    }

    /// Clear conversation history.
    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Get total token counts for the session.
    pub fn token_counts(&self) -> (u64, u64) {
        (self.total_input_tokens, self.total_output_tokens)
    }

    /// Get a reference to the tool executor.
    #[allow(dead_code)]
    pub fn executor(&self) -> &ToolExecutor {
        &self.executor
    }

    /// Consume the agent and return the inner executor (for shutdown).
    pub fn into_executor(self) -> ToolExecutor {
        self.executor
    }
}

/// Rough cost estimation in microcents.
fn estimate_cost(model: &str, input_tokens: u64, output_tokens: u64) -> i64 {
    let (input_price, output_price) = if model.contains("opus") {
        (15_000_000i64, 75_000_000i64)
    } else if model.contains("haiku") {
        (250_000i64, 1_250_000i64)
    } else {
        (3_000_000i64, 15_000_000i64)
    };

    let input_cost = (input_tokens as i64 * input_price) / 1_000_000;
    let output_cost = (output_tokens as i64 * output_price) / 1_000_000;
    input_cost + output_cost
}
