use std::sync::Arc;

use serde_json::{json, Value};

use agentfs_core::analytics::TokenRecord;

use crate::api::{LlmClient, Message};
use crate::auth::AuthProvider;
use crate::display;
use crate::error::{AgentError, Result};
use crate::executor::ToolExecutor;
use crate::memory::MemoryManager;
use crate::streaming::{ContentAccumulator, StreamEvent};
use crate::tools;

/// KV key prefix for persisted conversation messages.
const MESSAGES_KEY_PREFIX: &str = "session:messages:";

/// The agentic loop: prompt -> API -> stream -> tool_use -> execute -> loop.
pub struct Agent {
    client: LlmClient,
    executor: ToolExecutor,
    messages: Vec<Message>,
    tool_defs: Vec<Value>,
    system: Option<String>,
    session_id: String,
    model: String,
    total_input_tokens: u64,
    total_output_tokens: u64,
    memory: Option<Arc<MemoryManager>>,
}

impl Agent {
    pub fn new(
        client: LlmClient,
        executor: ToolExecutor,
        system: Option<String>,
        session_id: String,
        model: String,
        extra_tools: Vec<Value>,
    ) -> Self {
        let tool_defs = tools::merge_tools(tools::tool_definitions(), extra_tools);
        Self {
            client,
            executor,
            messages: Vec::new(),
            tool_defs,
            system,
            session_id,
            model,
            total_input_tokens: 0,
            total_output_tokens: 0,
            memory: None,
        }
    }

    /// Attach a memory manager to this agent.
    pub fn with_memory(mut self, memory: Arc<MemoryManager>) -> Self {
        self.memory = Some(memory);
        self
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

    /// Build the effective system prompt with memory context injected.
    async fn effective_system_prompt(&self, user_input: &str) -> Option<String> {
        let base = self.system.as_deref()?;

        if let Some(memory) = &self.memory {
            let memory_ctx = memory.context_for_prompt(user_input).await;
            if memory_ctx.is_empty() {
                Some(base.to_string())
            } else {
                Some(format!("{base}{memory_ctx}"))
            }
        } else {
            Some(base.to_string())
        }
    }

    /// Run a single turn: user message -> (possibly multiple) API calls until end_turn.
    pub async fn run_turn(&mut self, auth: &mut AuthProvider, user_input: &str) -> Result<String> {
        self.messages.push(Message {
            role: "user".to_string(),
            content: Value::String(user_input.to_string()),
        });

        let mut full_response = String::new();
        let mut all_tool_results: Vec<Value> = Vec::new();

        // Get effective system prompt with memory context
        let effective_system = self.effective_system_prompt(user_input).await;

        let mut step: u32 = 0;
        loop {
            step += 1;
            // Show thinking spinner (context-aware: different messages after tool execution)
            let spinner = if step == 1 {
                display::Spinner::thinking()
            } else {
                display::print_agentic_continue(step);
                display::Spinner::thinking_follow_up()
            };

            // Start streaming — returns a channel
            let rx_result = self
                .client
                .stream_message(
                    auth,
                    &self.messages,
                    &self.tool_defs,
                    effective_system.as_deref(),
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
            let mut renderer = display::StreamRenderer::new();
            let mut input_tokens = 0u64;
            let mut output_tokens = 0u64;
            let mut stop_reason = String::from("end_turn");
            let mut spinner_active = true;
            let mut spinner = Some(spinner);
            let mut tool_prep_spinner: Option<display::Spinner> = None;
            let mut tool_gen_tracker: Option<display::ToolGenTracker> = None;

            while let Some(event) = rx.recv().await {
                // Stop thinking spinner on first content event
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

                // Render live — rich rendering for text, standard for other events
                if let StreamEvent::TextDelta { text, .. } = &event {
                    renderer.push(text);
                } else if let StreamEvent::InputJsonDelta { partial_json, .. } = &event {
                    // Show progress for tool call generation (especially for large writes)
                    if let Some(ref mut tracker) = tool_gen_tracker {
                        tracker.update(partial_json.len());
                    }
                } else {
                    display::print_stream_event(&event);
                }

                // Show tool generation progress when tool calls start
                if let StreamEvent::ContentBlockStart {
                    block_type: crate::streaming::ContentBlockType::ToolUse { name, .. },
                    ..
                } = &event
                {
                    if tool_prep_spinner.is_none() && tool_gen_tracker.is_none() {
                        renderer.finish();
                        // Use progress tracker instead of static spinner for better UX
                        tool_gen_tracker = Some(display::ToolGenTracker::start(name));
                    }
                }

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
                        if let Some(s) = tool_prep_spinner.take() {
                            s.stop().await;
                        }
                        if let Some(t) = tool_gen_tracker.take() {
                            t.stop().await;
                        }
                        renderer.finish();
                        return Err(AgentError::Stream(message.clone()));
                    }
                    _ => {}
                }
            }

            // Finish stream rendering and stop all spinners
            renderer.finish();
            if let Some(s) = tool_prep_spinner {
                s.stop().await;
            }
            if let Some(t) = tool_gen_tracker {
                t.stop().await;
            }
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

                let tool_count = accumulator.tool_use_blocks.len();
                for (tool_idx, tool) in accumulator.tool_use_blocks.iter().enumerate() {
                    display::print_tool_call(tool, tool_idx, tool_count);

                    let tool_spinner = display::tool_spinner(&tool.name, &tool.input);
                    let tool_start = std::time::Instant::now();
                    let result = self.executor.execute(&tool.name, &tool.input).await;
                    let tool_elapsed = tool_start.elapsed();
                    tool_spinner.stop().await;

                    match result {
                        Ok(output) => {
                            display::print_tool_result(&tool.name, &output, false);
                            display::print_tool_done(tool_elapsed, false);
                            tool_results.push(json!({
                                "type": "tool_result",
                                "tool_use_id": tool.id,
                                "content": output,
                            }));
                        }
                        Err(e) => {
                            let error_msg = e.to_string();
                            display::print_tool_result(&tool.name, &error_msg, true);
                            display::print_tool_done(tool_elapsed, true);
                            tool_results.push(json!({
                                "type": "tool_result",
                                "tool_use_id": tool.id,
                                "content": error_msg,
                                "is_error": true,
                            }));
                        }
                    }
                }

                // Collect tool results for reflection
                all_tool_results.extend(tool_results.iter().cloned());

                self.messages.push(Message {
                    role: "user".to_string(),
                    content: Value::Array(tool_results),
                });

                println!();
                continue;
            }

            // End of turn — show cost and session totals
            let turn_cost = estimate_cost(&self.model, input_tokens, output_tokens);
            let session_cost = estimate_cost(
                &self.model,
                self.total_input_tokens,
                self.total_output_tokens,
            );
            display::print_token_usage(
                input_tokens,
                output_tokens,
                turn_cost,
                self.total_input_tokens + self.total_output_tokens,
                session_cost,
            );
            break;
        }

        // Persist after each turn
        self.save_messages().await;

        // Trigger reflection (inline, uses cheap model)
        if let Some(memory) = &self.memory {
            let memory = Arc::clone(memory);
            let messages = self.messages.clone();
            let tool_results = all_tool_results;
            let session_id = self.session_id.clone();
            // Run reflection — pass auth for API call
            memory
                .reflect(auth, &messages, &tool_results, &session_id)
                .await;
        }

        Ok(full_response)
    }

    /// Run a skill turn: inject skill body + user args as a single user message.
    pub async fn run_skill_turn(
        &mut self,
        auth: &mut AuthProvider,
        skill_body: &str,
        user_args: &str,
    ) -> Result<String> {
        let prompt = format!(
            "<skill>\n{skill_body}\n</skill>\n\nUser request: {user_args}"
        );
        self.run_turn(auth, &prompt).await
    }

    /// Clear conversation history.
    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Get current message count (for rollback on cancel).
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Rollback messages to a previous count (used on Ctrl+C cancel).
    pub fn rollback_to(&mut self, count: usize) {
        self.messages.truncate(count);
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

    /// Get the current model name.
    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// Get the current provider name.
    pub fn provider_name(&self) -> &str {
        self.client.provider_name()
    }

    /// Hot-swap the LLM client and model mid-session.
    pub fn set_client(&mut self, client: LlmClient, model: String) {
        self.client = client;
        self.model = model;
    }
}

/// Rough cost estimation in microcents.
fn estimate_cost(model: &str, input_tokens: u64, output_tokens: u64) -> i64 {
    let (input_price, output_price) = if model.contains("opus") {
        (15_000_000i64, 75_000_000i64)
    } else if model.contains("haiku") {
        (250_000i64, 1_250_000i64)
    } else if model.contains("kimi") || model.contains("moonshotai") {
        // NVIDIA-hosted Kimi — free tier / pricing TBD
        (0i64, 0i64)
    } else {
        // Default: Claude Sonnet pricing
        (3_000_000i64, 15_000_000i64)
    };

    let input_cost = (input_tokens as i64 * input_price) / 1_000_000;
    let output_cost = (output_tokens as i64 * output_price) / 1_000_000;
    input_cost + output_cost
}
