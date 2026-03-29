use std::io::BufRead;
use std::sync::atomic::Ordering;

use serde::Deserialize;

use super::convert::{LlmContent, LlmMessage};
use super::provider::*;
use super::types::*;
use crate::errors::ProviderError;

#[derive(Deserialize)]
struct OaiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

#[derive(Deserialize)]
struct OaiFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct OaiToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<OaiFunction>,
}

#[derive(Deserialize)]
struct OaiDelta {
    #[serde(default)]
    content: Option<String>,
    // Reasoning from local models (QwQ, DeepSeek-R1, etc.)
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<OaiToolCall>,
}

#[derive(Deserialize)]
struct OaiChoice {
    delta: OaiDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OaiChunk {
    #[serde(default)]
    choices: Vec<OaiChoice>,
    #[serde(default)]
    usage: Option<OaiUsage>,
}

pub struct OpenAICompatProvider {
    api_key: Option<String>,
    base_url: String,
    name: String,
}

impl OpenAICompatProvider {
    pub fn new(name: String, base_url: String, api_key: Option<String>) -> Self {
        Self { api_key, base_url, name }
    }

    pub fn build_request_body(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": request.model_id,
            "max_tokens": request.max_tokens,
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        let mut messages =
            vec![serde_json::json!({"role": "system", "content": request.system_prompt})];

        for msg in &request.messages {
            match msg {
                LlmMessage::User { content } => {
                    let text = content
                        .iter()
                        .filter_map(|c| match c {
                            LlmContent::Text(t) => Some(t.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    messages.push(serde_json::json!({"role": "user", "content": text}));
                }
                LlmMessage::Assistant { content } => {
                    let mut tool_calls = Vec::new();
                    let mut text_parts = Vec::new();
                    for c in content {
                        match c {
                            LlmContent::Text(t) => text_parts.push(t.as_str()),
                            LlmContent::ToolCall { id, name, arguments } => {
                                tool_calls.push(serde_json::json!({
                                    "id": id, "type": "function",
                                    "function": {"name": name, "arguments": arguments.to_string()},
                                }));
                            }
                            _ => {}
                        }
                    }
                    let has_text = !text_parts.is_empty();
                    let has_tools = !tool_calls.is_empty();
                    let mut msg = serde_json::json!({"role": "assistant"});
                    if has_text {
                        msg["content"] = serde_json::json!(text_parts.join(""));
                    }
                    if has_tools {
                        msg["tool_calls"] = serde_json::Value::Array(tool_calls);
                    }
                    // OpenAI-compat requires at least content or tool_calls
                    if !has_text && !has_tools {
                        msg["content"] = serde_json::json!("");
                    }
                    messages.push(msg);
                }
                LlmMessage::ToolResult { tool_call_id, content, .. } => {
                    let text = content
                        .iter()
                        .filter_map(|c| match c {
                            LlmContent::Text(t) => Some(t.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    messages.push(serde_json::json!({"role": "tool", "tool_call_id": tool_call_id, "content": text}));
                }
            }
        }
        body["messages"] = serde_json::Value::Array(messages);

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {"name": t.name, "description": t.description, "parameters": t.parameters},
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tools);
        }

        if let Some(ref thinking) = request.thinking {
            match thinking {
                ThinkingRequest::Budget { tokens } => {
                    body["reasoning_effort"] = serde_json::json!("medium");
                    body["max_completion_tokens"] = serde_json::json!(request.max_tokens + tokens);
                }
            }
        }
        body
    }
}

impl Provider for OpenAICompatProvider {
    fn name(&self) -> &str {
        &self.name
    }

    /// Probe `GET /models` — standard OpenAI-compat endpoint, no tokens
    /// consumed.
    fn healthcheck(&self) -> bool {
        let url = format!("{}/models", self.base_url);
        let mut req = crate::http::agent().get(&url);
        if let Some(ref key) = self.api_key {
            req = req.header("authorization", &format!("Bearer {}", key));
        }
        matches!(req.call().map(|r| r.status().as_u16()), Ok(200))
    }

    fn stream_completion(
        &self,
        request: &CompletionRequest,
        cancel: &CancelFlag,
        on_event: &mut dyn FnMut(ProviderEvent),
    ) -> Result<(), ProviderError> {
        let body = self.build_request_body(request);
        let url = format!("{}/chat/completions", self.base_url);

        let mut req = crate::http::agent().post(&url).header("content-type", "application/json");
        if let Some(ref key) = self.api_key {
            req = req.header("authorization", &format!("Bearer {}", key));
        }

        let response =
            req.send_json(&body).map_err(|e| ProviderError::SseParse { message: e.to_string() })?;

        let status = response.status().as_u16();
        if status != 200 {
            let err_body = response.into_body().read_to_string().unwrap_or_default();
            crate::log::warn(&format!("{} HTTP {}: {}", self.name, status, err_body));
            let message = serde_json::from_str::<serde_json::Value>(&err_body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| format!("HTTP {}", status));
            return Err(ProviderError::Server { status, message });
        }

        // Read SSE lines in a background thread so the main thread can check
        // the cancel flag without being blocked on network I/O.
        let (line_tx, line_rx) = crossbeam_channel::bounded::<Result<String, String>>(64);
        std::thread::Builder::new()
            .name("nerv-sse-reader".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                let mut body = response.into_body();
                let reader = std::io::BufReader::new(body.as_reader());
                for line_result in reader.lines() {
                    let msg = line_result.map_err(|e| e.to_string());
                    if line_tx.send(msg).is_err() {
                        break; // receiver dropped (cancelled) — body drops, closing connection
                    }
                }
            })
            .expect("failed to spawn SSE reader thread");

        let mut usage = Usage::default();
        let poll_interval = std::time::Duration::from_millis(50);

        loop {
            if cancel.load(Ordering::Relaxed) {
                // Drop receiver — reader thread will see send error, drop body,
                // close TCP connection, causing the server to stop generating.
                drop(line_rx);
                on_event(ProviderEvent::MessageStop { stop_reason: StopReason::Aborted, usage });
                return Ok(());
            }

            let line = match line_rx.recv_timeout(poll_interval) {
                Ok(Ok(line)) => line,
                Ok(Err(e)) => {
                    return Err(ProviderError::SseParse { message: e });
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    // Reader thread finished (EOF or error)
                    on_event(ProviderEvent::MessageStop {
                        stop_reason: StopReason::EndTurn,
                        usage,
                    });
                    return Ok(());
                }
            };

            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line == "data: [DONE]" {
                on_event(ProviderEvent::MessageStop { stop_reason: StopReason::EndTurn, usage });
                return Ok(());
            }
            let Some(data) = line.strip_prefix("data: ") else {
                continue;
            };
            let Ok(chunk) = serde_json::from_str::<OaiChunk>(data) else {
                continue;
            };

            if let Some(u) = chunk.usage.filter(|u| u.prompt_tokens > 0) {
                usage.input = u.prompt_tokens;
                usage.output = u.completion_tokens;
                on_event(ProviderEvent::UsageUpdate(usage.clone()));
            }
            let Some(choice) = chunk.choices.into_iter().next() else {
                continue;
            };
            let delta = choice.delta;

            if let Some(text) = delta.content.filter(|t| !t.is_empty()) {
                on_event(ProviderEvent::TextDelta(text));
            }
            // Reasoning/thinking content from local models (QwQ, DeepSeek-R1, etc.)
            if let Some(thinking) =
                delta.reasoning_content.or(delta.reasoning).filter(|t| !t.is_empty())
            {
                on_event(ProviderEvent::ThinkingDelta(thinking));
            }
            for tc in delta.tool_calls {
                let id = tc.id.unwrap_or_default();
                if let Some(func) = tc.function {
                    if let Some(name) = func.name.filter(|n| !n.is_empty()) {
                        on_event(ProviderEvent::ToolCallStart { id: id.clone(), name });
                    }
                    if let Some(args) = func.arguments.filter(|a| !a.is_empty()) {
                        on_event(ProviderEvent::ToolCallArgsDelta { id: id.clone(), delta: args });
                    }
                }
            }
            if let Some(reason) = choice.finish_reason.as_deref() {
                let sr = match reason {
                    "stop" => StopReason::EndTurn,
                    "tool_calls" => StopReason::ToolUse,
                    "length" => StopReason::MaxTokens,
                    _ => StopReason::EndTurn,
                };
                on_event(ProviderEvent::MessageStop { stop_reason: sr, usage });
                return Ok(());
            }
        }
    }
}
