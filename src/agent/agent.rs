use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use super::convert::convert_to_llm;
use super::transform::{prepare_context, transform_context};
use super::provider::*;
use super::types::*;

pub type UpdateCallback = Arc<dyn Fn(String) + Send + Sync>;

pub trait AgentTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    fn prompt_snippet(&self) -> Option<&str> {
        None
    }
    fn prompt_guidelines(&self) -> Vec<String> {
        vec![]
    }

    /// Coerce model output before validate/execute. Default: identity.
    fn normalize(&self, input: serde_json::Value) -> serde_json::Value {
        input
    }

    fn validate(&self, input: &serde_json::Value) -> Result<(), crate::errors::ToolError>;

    /// Execute the tool synchronously.
    fn execute(&self, input: serde_json::Value, update: UpdateCallback) -> ToolResult;
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: String,
    pub details: Option<serde_json::Value>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self { content: content.into(), details: None, is_error: false }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self { content: content.into(), details: None, is_error: true }
    }

    pub fn ok_with_details(content: impl Into<String>, details: serde_json::Value) -> Self {
        Self { content: content.into(), details: Some(details), is_error: false }
    }
}

/// Callback that checks if a tool call is allowed. Returns true to proceed.
pub type PermissionFn = Arc<dyn Fn(&str, &serde_json::Value) -> bool + Send + Sync>;

/// Context gate — called before each API request with (estimated_tokens, context_window).
/// Returns true to proceed, false to abort the turn. Used to implement circuit breakers
/// when context grows unexpectedly.
pub type ContextGateFn = Arc<dyn Fn(ContextGateInfo) -> bool + Send + Sync>;

#[derive(Debug, Clone)]
pub struct ContextGateInfo {
    pub estimated_tokens: usize,
    pub prev_tokens: usize,
    pub context_window: u32,
    pub tool_rounds: usize,
}

pub struct AgentState {
    pub messages: Vec<AgentMessage>,
    pub model: Option<Model>,
    pub thinking_level: ThinkingLevel,
    /// When set, uses Anthropic's adaptive effort API instead of a fixed budget.
    pub effort_level: Option<EffortLevel>,
    pub system_prompt: String,
    pub tools: Vec<Arc<dyn AgentTool>>,
    pub is_streaming: bool,
    pub permission_fn: Option<PermissionFn>,
    pub context_gate_fn: Option<ContextGateFn>,
}

pub struct Agent {
    pub state: AgentState,
    /// Shared cancel flag — set from main thread to interrupt streaming.
    pub cancel: CancelFlag,
    pub provider_registry: Arc<RwLock<ProviderRegistry>>,
    /// Estimated token count from the previous API call (for circuit breaker delta).
    prev_estimated_tokens: usize,
}

impl Agent {
    pub fn new(provider_registry: Arc<RwLock<ProviderRegistry>>) -> Self {
        Self {
            state: AgentState {
                messages: Vec::new(),
                model: None,
                thinking_level: ThinkingLevel::default(),
                effort_level: None,
                system_prompt: String::new(),
                tools: Vec::new(),
                is_streaming: false,
                permission_fn: None,
                context_gate_fn: None,
            },
            cancel: new_cancel_flag(),
            provider_registry,
            prev_estimated_tokens: 0,
        }
    }

    pub fn abort(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn reset_cancel(&self) {
        self.cancel.store(false, Ordering::Relaxed);
    }

    /// Run the agentic loop synchronously. Calls `on_event` for each event.
    /// If `persist_fn` is provided, each new message is persisted to the session
    /// DB as it's produced (per-iteration), so a mid-turn crash doesn't lose work.
    /// Returns the new messages produced during this prompt.
    pub fn prompt(
        &mut self,
        prompt_messages: Vec<AgentMessage>,
        on_event: &dyn Fn(AgentEvent),
        persist_fn: Option<&mut dyn FnMut(&AgentMessage)>,
    ) -> Vec<AgentMessage> {
        self.reset_cancel();
        self.state.is_streaming = true;

        let mut new_messages: Vec<AgentMessage> = Vec::new();

        // Rebind as mutable so we can pass &mut into closures at each call site.
        let mut persist_fn = persist_fn;

        on_event(AgentEvent::AgentStart);
        on_event(AgentEvent::TurnStart);

        for msg in &prompt_messages {
            self.state.messages.push(msg.clone());
            new_messages.push(msg.clone());
            if let Some(ref mut f) = persist_fn {
                f(msg);
            }
            on_event(AgentEvent::MessageStart {
                message: msg.clone(),
            });
        }

        // Freeze all context decisions once before the tool loop — critical for
        // prompt-cache prefix stability across consecutive API calls.
        let ctx = prepare_context(&self.state.messages);

        let mut has_tool_calls = true;
        while has_tool_calls {
            let assistant = self.stream_response(on_event, &ctx);
            let assistant_msg = AgentMessage::Assistant(assistant.clone());
            new_messages.push(assistant_msg.clone());
            if let Some(ref mut f) = persist_fn {
                f(&assistant_msg);
            }

            if assistant.stop_reason.is_error()
                || matches!(assistant.stop_reason, StopReason::Aborted)
            {
                on_event(AgentEvent::TurnEnd);
                break;
            }

            let tool_calls: Vec<_> = assistant
                .content
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } = b
                    {
                        Some((id.clone(), name.clone(), arguments.clone()))
                    } else {
                        None
                    }
                })
                .collect();

            has_tool_calls = !tool_calls.is_empty();

            if has_tool_calls {
                let results = self.execute_tools(&tool_calls, on_event);
                for r in results {
                    self.state.messages.push(r.clone());
                    new_messages.push(r.clone());
                    if let Some(ref mut f) = persist_fn {
                        f(&r);
                    }
                }
            }

            on_event(AgentEvent::TurnEnd);

            if has_tool_calls && !self.cancel.load(Ordering::Relaxed) {
                on_event(AgentEvent::TurnStart);
            }
        }

        self.state.is_streaming = false;
        on_event(AgentEvent::AgentEnd {
            messages: new_messages.clone(),
        });
        new_messages
    }

    fn stream_response(&mut self, on_event: &dyn Fn(AgentEvent), ctx: &super::transform::ContextConfig) -> AssistantMessage {
        let model = match &self.state.model {
            Some(m) => m.clone(),
            None => {
                let msg = AssistantMessage {
                    content: vec![],
                    stop_reason: StopReason::Error {
                        message: "no model configured".into(),
                    },
                    usage: None,
                    timestamp: now_millis(),
                };
                on_event(AgentEvent::MessageEnd {
                    message: msg.clone(),
                });
                return msg;
            }
        };

        let provider = match self
            .provider_registry
            .read()
            .unwrap()
            .get(&model.provider_name)
        {
            Some(p) => p,
            None => {
                let msg = AssistantMessage {
                    content: vec![],
                    stop_reason: StopReason::Error {
                        message: format!("provider '{}' not found", model.provider_name),
                    },
                    usage: None,
                    timestamp: now_millis(),
                };
                on_event(AgentEvent::MessageEnd {
                    message: msg.clone(),
                });
                return msg;
            }
        };

        let transformed = transform_context(self.state.messages.clone(), model.context_window, Some(ctx.stale_cutoff));
        let estimated_tokens: usize = transformed.iter().map(crate::compaction::estimate_tokens).sum();

        // Circuit breaker: if context grew by >10% since last call (and we're past 10k),
        // ask user to confirm before sending a large request.
        if let Some(ref gate_fn) = self.state.context_gate_fn {
            let tool_rounds = transformed
                .iter()
                .filter(|m| matches!(m, AgentMessage::Assistant(_)))
                .count();
            let info = ContextGateInfo {
                estimated_tokens,
                prev_tokens: self.prev_estimated_tokens,
                context_window: model.context_window,
                tool_rounds,
            };
            if !gate_fn(info) {
                let msg = AssistantMessage {
                    content: vec![],
                    stop_reason: StopReason::Aborted,
                    usage: None,
                    timestamp: now_millis(),
                };
                on_event(AgentEvent::MessageEnd {
                    message: msg.clone(),
                });
                return msg;
            }
        }
        self.prev_estimated_tokens = estimated_tokens;

        let llm_messages = convert_to_llm(&transformed);

        let wire_tools: Vec<WireTool> = self
            .state
            .tools
            .iter()
            .map(|t| WireTool {
                name: t.name().to_string(),
                description: if ctx.prune_tools {
                    String::new()
                } else {
                    t.description().to_string()
                },
                parameters: t.parameters_schema(),
            })
            .collect();

        let thinking = resolve_thinking(self.state.thinking_level, self.state.effort_level, &model);
        let base_max = 32_000u32.min(model.max_output_tokens);
        let max_tokens = if let Some(ref t) = thinking {
            adjust_max_tokens_for_thinking(base_max, model.max_output_tokens, t).0
        } else {
            base_max
        };

        let request = CompletionRequest {
            model_id: model.id.clone(),
            system_prompt: self.state.system_prompt.clone(),
            messages: llm_messages,
            tools: wire_tools,
            max_tokens,
            thinking,
            cache: CacheConfig::default(),
        };

        // Input token count is NOT estimated locally — we wait for the API's authoritative
        // value from the `message_start` SSE event. Local tiktoken estimates diverge from
        // Claude's tokenizer and don't account for message framing / tool schema overhead.

        // Retry loop for transient API errors (overloaded / rate-limited).
        // Resets all accumulation state on each attempt so partial stream events
        // from a failed attempt are discarded before the next try.
        const MAX_RETRIES: u32 = 4;
        // Base backoff delays in seconds: attempt 1 → 5s, 2 → 30s, 3 → 60s, 4 → 60s
        const BACKOFF_SECS: [u64; MAX_RETRIES as usize] = [5, 30, 60, 60];

        let mut attempt = 0u32;
        let (content_blocks, stop_reason, usage) = loop {

        // Reset accumulators at the top so a failed partial stream is discarded cleanly.
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut current_text = String::new();
        let mut current_thinking = String::new();
        let mut current_tool_id = String::new();
        let mut current_tool_name = String::new();
        let mut current_tool_args = String::new();
        let mut stop_reason = StopReason::EndTurn;
        let mut usage = Usage::default();

        let result = provider.stream_completion(&request, &self.cancel, &mut |event| match event {
            ProviderEvent::TextDelta(delta) => {
                current_text.push_str(&delta);
                on_event(AgentEvent::MessageUpdate {
                    delta: StreamDelta::Text(delta),
                });
            }
            ProviderEvent::ThinkingDelta(delta) => {
                current_thinking.push_str(&delta);
                on_event(AgentEvent::MessageUpdate {
                    delta: StreamDelta::Thinking(delta),
                });
            }
            ProviderEvent::ToolCallStart { id, name } => {
                if !current_text.is_empty() {
                    content_blocks.push(ContentBlock::Text {
                        text: std::mem::take(&mut current_text),
                    });
                }
                if !current_thinking.is_empty() {
                    content_blocks.push(ContentBlock::Thinking {
                        thinking: std::mem::take(&mut current_thinking),
                    });
                }
                current_tool_id.clone_from(&id);
                current_tool_name.clone_from(&name);
                current_tool_args.clear();
                on_event(AgentEvent::MessageUpdate {
                    delta: StreamDelta::ToolCallArgsStart { id, name },
                });
            }
            ProviderEvent::ToolCallArgsDelta { id, delta } => {
                current_tool_args.push_str(&delta);
                on_event(AgentEvent::MessageUpdate {
                    delta: StreamDelta::ToolCallArgsDelta { id, delta },
                });
            }
            ProviderEvent::ToolCallEnd { .. } => {
                // Only create a tool call block if we actually have a tool in progress
                if !current_tool_id.is_empty() {
                    let arguments = serde_json::from_str(&current_tool_args)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    content_blocks.push(ContentBlock::ToolCall {
                        id: std::mem::take(&mut current_tool_id),
                        name: std::mem::take(&mut current_tool_name),
                        arguments,
                    });
                    current_tool_args.clear();
                }
            }
            ProviderEvent::UsageUpdate(u) => {
                usage = u.clone();
                on_event(AgentEvent::UsageUpdate { usage: u });
            }
            ProviderEvent::MessageStop {
                stop_reason: sr,
                usage: u,
            } => {
                stop_reason = sr;
                usage = u;
            }
        });

        if let Err(e) = result {
            // Retry on transient errors (overloaded / rate-limited) up to MAX_RETRIES times.
            if e.is_retryable() && attempt < MAX_RETRIES {
                let wait_secs = if let crate::errors::ProviderError::RateLimited {
                    retry_after_ms: Some(ms),
                } = &e
                {
                    // Anthropic told us exactly how long to wait; honour it.
                    (*ms + 999) / 1000
                } else {
                    BACKOFF_SECS[attempt as usize]
                };
                attempt += 1;
                on_event(AgentEvent::Retrying {
                    attempt,
                    wait_secs,
                    reason: e.to_string(),
                });
                std::thread::sleep(Duration::from_secs(wait_secs));
                continue;
            }
            let msg = AssistantMessage {
                content: vec![],
                stop_reason: StopReason::Error {
                    message: e.to_string(),
                },
                usage: None,
                timestamp: now_millis(),
            };
            on_event(AgentEvent::MessageEnd {
                message: msg.clone(),
            });
            return msg;
        }

        if !current_thinking.is_empty() {
            content_blocks.push(ContentBlock::Thinking {
                thinking: current_thinking,
            });
        }
        if !current_text.is_empty() {
            content_blocks.push(ContentBlock::Text { text: current_text });
        }
        if !current_tool_id.is_empty() {
            let arguments = serde_json::from_str(&current_tool_args)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            content_blocks.push(ContentBlock::ToolCall {
                id: current_tool_id,
                name: current_tool_name,
                arguments,
            });
        }

        break (content_blocks, stop_reason, usage);
        }; // end retry loop

        let msg = AssistantMessage {
            content: content_blocks,
            stop_reason,
            usage: Some(usage),
            timestamp: now_millis(),
        };

        self.state
            .messages
            .push(AgentMessage::Assistant(msg.clone()));
        on_event(AgentEvent::MessageEnd {
            message: msg.clone(),
        });

        msg
    }

    fn execute_tools(
        &self,
        tool_calls: &[(String, String, serde_json::Value)],
        on_event: &dyn Fn(AgentEvent),
    ) -> Vec<AgentMessage> {
        // Execute sequentially (sync — no join_all needed)
        tool_calls
            .iter()
            .map(|(id, name, args)| {
                let tool = self.state.tools.iter().find(|t| t.name() == name).cloned();

                on_event(AgentEvent::ToolExecutionStart {
                    id: id.clone(),
                    name: name.clone(),
                    args: args.clone(),
                });

                // Check permissions before execution
                let permitted = self
                    .state
                    .permission_fn
                    .as_ref()
                    .map(|f| f(name, args))
                    .unwrap_or(true);

                let result = if !permitted {
                    ToolResult {
                        content: "Tool call denied by user.".into(),
                        details: None,
                        is_error: true,
                    }
                } else if let Some(tool) = tool {
                    let args = tool.normalize(args.clone());
                    match tool.validate(&args) {
                        Ok(()) => {
                            let update_cb: UpdateCallback = Arc::new(|_output: String| {});
                            tool.execute(args, update_cb)
                        }
                        Err(e) => ToolResult {
                            content: format!("Validation error: {}", e),
                            details: None,
                            is_error: true,
                        },
                    }
                } else {
                    ToolResult {
                        content: format!("Unknown tool: {}", name),
                        details: None,
                        is_error: true,
                    }
                };

                let display = result.details.as_ref().and_then(|d| {
                    d.get("display").and_then(|v| v.as_str()).map(|s| s.to_string())
                });

                on_event(AgentEvent::ToolExecutionEnd {
                    id: id.clone(),
                    result: ToolResultData {
                        content: result.content.clone(),
                        display: display.clone(),
                        is_error: result.is_error,
                    },
                });

                AgentMessage::ToolResult {
                    tool_call_id: id.clone(),
                    content: vec![ContentItem::Text {
                        text: result.content,
                    }],
                    is_error: result.is_error,
                    display: display.clone(),
                    timestamp: now_millis(),
                }
            })
            .collect()
    }
}
