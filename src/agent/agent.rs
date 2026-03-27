use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};

use super::convert::{convert_to_llm, transform_context};
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

pub struct AgentState {
    pub messages: Vec<AgentMessage>,
    pub model: Option<Model>,
    pub thinking_level: ThinkingLevel,
    pub system_prompt: String,
    pub tools: Vec<Arc<dyn AgentTool>>,
    pub is_streaming: bool,
    pub permission_fn: Option<PermissionFn>,
}

pub struct Agent {
    pub state: AgentState,
    /// Shared cancel flag — set from main thread to interrupt streaming.
    pub cancel: CancelFlag,
    pub provider_registry: Arc<RwLock<ProviderRegistry>>,
}

impl Agent {
    pub fn new(provider_registry: Arc<RwLock<ProviderRegistry>>) -> Self {
        Self {
            state: AgentState {
                messages: Vec::new(),
                model: None,
                thinking_level: ThinkingLevel::default(),
                system_prompt: String::new(),
                tools: Vec::new(),
                is_streaming: false,
                permission_fn: None,
            },
            cancel: new_cancel_flag(),
            provider_registry,
        }
    }

    pub fn abort(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn reset_cancel(&self) {
        self.cancel.store(false, Ordering::Relaxed);
    }

    /// Run the agentic loop synchronously. Calls `on_event` for each event.
    /// Returns the new messages produced during this prompt.
    pub fn prompt(
        &mut self,
        prompt_messages: Vec<AgentMessage>,
        on_event: &dyn Fn(AgentEvent),
    ) -> Vec<AgentMessage> {
        self.reset_cancel();
        self.state.is_streaming = true;

        let mut new_messages: Vec<AgentMessage> = Vec::new();

        on_event(AgentEvent::AgentStart);
        on_event(AgentEvent::TurnStart);

        for msg in &prompt_messages {
            self.state.messages.push(msg.clone());
            new_messages.push(msg.clone());
            on_event(AgentEvent::MessageStart {
                message: msg.clone(),
            });
        }

        let mut has_tool_calls = true;
        while has_tool_calls {
            let assistant = self.stream_response(on_event);
            new_messages.push(AgentMessage::Assistant(assistant.clone()));

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
                    new_messages.push(r);
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

    fn stream_response(&mut self, on_event: &dyn Fn(AgentEvent)) -> AssistantMessage {
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

        let transformed = transform_context(self.state.messages.clone(), model.context_window);

        // Estimate current context usage and inject budget note so the model
        // can self-regulate (batch more aggressively when context is growing).
        let estimated_tokens: usize = transformed.iter().map(crate::compaction::estimate_tokens).sum();
        let tool_rounds = transformed
            .iter()
            .filter(|m| matches!(m, AgentMessage::Assistant(_)))
            .count();
        let system_prompt = if tool_rounds > 1 {
            format!(
                "{}\n\n[Context: ~{}k/{}k tokens, {} tool rounds]",
                self.state.system_prompt,
                estimated_tokens / 1000,
                model.context_window / 1000,
                tool_rounds,
            )
        } else {
            self.state.system_prompt.clone()
        };

        let llm_messages = convert_to_llm(&transformed);

        let wire_tools: Vec<WireTool> = self
            .state
            .tools
            .iter()
            .map(|t| WireTool {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect();

        let thinking = resolve_thinking(self.state.thinking_level, &model);
        let base_max = 32_000u32.min(model.max_output_tokens);
        let max_tokens = if let Some(ref t) = thinking {
            adjust_max_tokens_for_thinking(base_max, model.max_output_tokens, t).0
        } else {
            base_max
        };

        let request = CompletionRequest {
            model_id: model.id.clone(),
            system_prompt,
            messages: llm_messages,
            tools: wire_tools,
            max_tokens,
            thinking,
            cache: CacheConfig::default(),
        };

        // Input token count is NOT estimated locally — we wait for the API's authoritative
        // value from the `message_start` SSE event. Local tiktoken estimates diverge from
        // Claude's tokenizer and don't account for message framing / tool schema overhead.

        // Accumulate streamed events into an AssistantMessage.
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
                        display,
                        is_error: result.is_error,
                    },
                });

                AgentMessage::ToolResult {
                    tool_call_id: id.clone(),
                    content: vec![ContentItem::Text {
                        text: result.content,
                    }],
                    is_error: result.is_error,
                    timestamp: now_millis(),
                }
            })
            .collect()
    }
}
