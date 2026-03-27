use std::io::BufRead;
use std::sync::atomic::Ordering;

use crate::errors::ProviderError;

use super::convert::{LlmContent, LlmMessage};
use super::provider::*;
use super::types::*;

pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    use_bearer: bool,
    extra_headers: Vec<(String, String)>,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: "https://api.anthropic.com".to_string(),
            use_bearer: false,
            extra_headers: Vec::new(),
        }
    }

    /// Create a provider using OAuth Bearer token auth.
    pub fn new_oauth(access_token: String) -> Self {
        Self {
            api_key: access_token,
            base_url: "https://api.anthropic.com".to_string(),
            use_bearer: true,
            extra_headers: Vec::new(),
        }
    }

    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }

    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.extra_headers = headers;
        self
    }

    pub fn build_request_body(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": request.model_id,
            "max_tokens": request.max_tokens,
            "stream": true,
        });

        // System prompt — OAuth requires Claude Code identity prefix
        let mut system_content = Vec::new();
        if self.use_bearer {
            system_content.push(serde_json::json!({
                "type": "text",
                "text": "You are Claude Code, Anthropic's official CLI for Claude.",
            }));
        }
        system_content.push(serde_json::json!({
            "type": "text",
            "text": request.system_prompt,
        }));
        if request.cache.retention != CacheRetention::None
            && let Some(last) = system_content.last_mut()
        {
            last["cache_control"] = cache_control_value(&request.cache, &self.base_url);
        }
        body["system"] = serde_json::Value::Array(system_content);

        let wire_messages = messages_to_wire(&request.messages, &request.cache, &self.base_url);
        body["messages"] = serde_json::Value::Array(wire_messages);

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tools);
        }

        if let Some(ref thinking) = request.thinking {
            match thinking {
                ThinkingRequest::Budget { tokens } => {
                    body["thinking"] = serde_json::json!({
                        "type": "enabled",
                        "budget_tokens": tokens,
                    });
                }
                ThinkingRequest::Adaptive { effort } => {
                    let effort_str = match effort {
                        AdaptiveEffort::Low => "low",
                        AdaptiveEffort::Medium => "medium",
                        AdaptiveEffort::High => "high",
                        AdaptiveEffort::Max => "max",
                    };
                    body["thinking"] = serde_json::json!({"type": "enabled"});
                    body["effort"] = serde_json::json!(effort_str);
                }
            }
        }

        body
    }
}

impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn stream_completion(
        &self,
        request: &CompletionRequest,
        cancel: &CancelFlag,
        on_event: &mut dyn FnMut(ProviderEvent),
    ) -> Result<(), ProviderError> {
        let body = self.build_request_body(request);
        let url = format!("{}/v1/messages", self.base_url);

        let mut req = crate::http::agent()
            .post(&url)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");

        if self.use_bearer {
            req = req.header("authorization", &format!("Bearer {}", self.api_key));
            // OAuth requires these beta flags and identity headers
            let mut betas = vec!["claude-code-20250219", "oauth-2025-04-20"];
            if let Some(ThinkingRequest::Budget { .. }) = &request.thinking {
                betas.push("interleaved-thinking-2025-05-14");
            }
            req = req
                .header("anthropic-beta", &betas.join(","))
                .header("x-app", "cli")
                .header("user-agent", "claude-cli/1.0.0");
        } else {
            req = req.header("x-api-key", &self.api_key);
            if let Some(ThinkingRequest::Budget { .. }) = &request.thinking {
                req = req.header("anthropic-beta", "interleaved-thinking-2025-05-14");
            }
        }

        // Apply extra headers from config
        for (k, v) in &self.extra_headers {
            req = req.header(k, v);
        }

        crate::log::debug(&format!(
            "anthropic request: url={} use_bearer={} model={} body={}",
            url,
            self.use_bearer,
            request.model_id,
            serde_json::to_string(&body).unwrap_or_default(),
        ));

        let response = match req.send_json(&body) {
            Ok(r) => r,
            Err(e) => {
                crate::log::warn(&format!("anthropic request error: {}", e));
                return Err(ProviderError::SseParse {
                    message: e.to_string(),
                });
            }
        };

        let status = response.status().as_u16();
        if status != 200 {
            let err_body = response.into_body().read_to_string().unwrap_or_default();
            crate::log::warn(&format!("anthropic HTTP {}: {}", status, err_body));
            let message = serde_json::from_str::<serde_json::Value>(&err_body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| format!("HTTP {}", status));
            return Err(classify_status(status, &message));
        }

        // Read SSE lines in a background thread so the main thread can check
        // the cancel flag without being blocked on network I/O.
        let (line_tx, line_rx) = crossbeam_channel::bounded::<Result<String, String>>(64);
        std::thread::spawn(move || {
            let mut body = response.into_body();
            let reader = std::io::BufReader::new(body.as_reader());
            for line_result in reader.lines() {
                let msg = line_result.map_err(|e| e.to_string());
                if line_tx.send(msg).is_err() {
                    break; // receiver dropped (cancelled) — body drops, closing connection
                }
            }
        });

        let mut current_event_type = String::new();
        let mut sse_state = SseState::default();
        let poll_interval = std::time::Duration::from_millis(50);

        loop {
            if cancel.load(Ordering::Relaxed) {
                drop(line_rx);
                on_event(ProviderEvent::MessageStop {
                    stop_reason: StopReason::Aborted,
                    usage: Usage::default(),
                });
                return Ok(());
            }

            let line = match line_rx.recv_timeout(poll_interval) {
                Ok(Ok(line)) => line,
                Ok(Err(e)) => {
                    return Err(ProviderError::SseParse { message: e });
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return Ok(()),
            };

            if line.is_empty() {
                continue;
            }

            if let Some(event_type) = line.strip_prefix("event: ") {
                current_event_type = event_type.to_string();
            } else if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                    for event in parse_sse_event(&current_event_type, &json, &mut sse_state) {
                        let is_stop = matches!(event, ProviderEvent::MessageStop { .. });
                        on_event(event);
                        if is_stop {
                            return Ok(());
                        }
                    }
                }
                current_event_type.clear();
            }
        }
    }
}

/// Track state across SSE events for Anthropic's streaming protocol.
/// Needed because content_block_delta/stop reference blocks by index,
/// but we need to know the block type and tool ID.
#[derive(Default)]
struct SseState {
    /// Maps content block index → tool_use ID (only for tool_use blocks)
    tool_ids: std::collections::HashMap<u64, String>,
}

fn parse_sse_event(
    event_type: &str,
    data: &serde_json::Value,
    state: &mut SseState,
) -> Vec<ProviderEvent> {
    match event_type {
        "message_start" => {
            let usage = parse_usage(&data["message"]["usage"]);
            if usage.input > 0 {
                vec![ProviderEvent::UsageUpdate(usage)]
            } else {
                vec![]
            }
        }
        "content_block_start" => {
            let index = data["index"].as_u64().unwrap_or(0);
            let block = &data["content_block"];
            match block["type"].as_str() {
                Some("tool_use") => {
                    let id = block["id"].as_str().unwrap_or("").to_string();
                    state.tool_ids.insert(index, id.clone());
                    vec![ProviderEvent::ToolCallStart {
                        id,
                        name: block["name"].as_str().unwrap_or("").to_string(),
                    }]
                }
                _ => vec![],
            }
        }
        "content_block_delta" => {
            let delta = &data["delta"];
            match delta["type"].as_str() {
                Some("text_delta") => {
                    vec![ProviderEvent::TextDelta(
                        delta["text"].as_str().unwrap_or("").to_string(),
                    )]
                }
                Some("thinking_delta") => {
                    vec![ProviderEvent::ThinkingDelta(
                        delta["thinking"].as_str().unwrap_or("").to_string(),
                    )]
                }
                Some("input_json_delta") => {
                    let index = data["index"].as_u64().unwrap_or(0);
                    let id = state.tool_ids.get(&index).cloned().unwrap_or_default();
                    vec![ProviderEvent::ToolCallArgsDelta {
                        id,
                        delta: delta["partial_json"].as_str().unwrap_or("").to_string(),
                    }]
                }
                _ => vec![],
            }
        }
        "content_block_stop" => {
            let index = data["index"].as_u64().unwrap_or(0);
            // Only emit ToolCallEnd for tool_use blocks
            if let Some(id) = state.tool_ids.remove(&index) {
                vec![ProviderEvent::ToolCallEnd { id }]
            } else {
                vec![]
            }
        }
        "message_delta" => {
            let stop_reason = match data["delta"]["stop_reason"].as_str() {
                Some("end_turn") => StopReason::EndTurn,
                Some("tool_use") => StopReason::ToolUse,
                Some("max_tokens") => StopReason::MaxTokens,
                _ => StopReason::EndTurn,
            };
            let usage = parse_usage(&data["usage"]);
            vec![ProviderEvent::MessageStop { stop_reason, usage }]
        }
        "error" => {
            let msg = data["error"]["message"]
                .as_str()
                .unwrap_or("unknown error")
                .to_string();
            vec![ProviderEvent::MessageStop {
                stop_reason: StopReason::Error { message: msg },
                usage: Usage::default(),
            }]
        }
        _ => vec![],
    }
}

fn parse_usage(v: &serde_json::Value) -> Usage {
    Usage {
        input: v["input_tokens"].as_u64().unwrap_or(0) as u32,
        output: v["output_tokens"].as_u64().unwrap_or(0) as u32,
        cache_read: v["cache_read_input_tokens"].as_u64().unwrap_or(0) as u32,
        cache_write: v["cache_creation_input_tokens"].as_u64().unwrap_or(0) as u32,
    }
}

fn classify_status(status: u16, message: &str) -> ProviderError {
    let message = message.to_string();
    match status {
        401 | 403 => ProviderError::Auth { message },
        429 => ProviderError::RateLimited {
            retry_after_ms: None,
        },
        529 => ProviderError::Overloaded,
        500..=599 => ProviderError::Server { status, message },
        _ => ProviderError::Server { status, message },
    }
}

fn messages_to_wire(
    messages: &[LlmMessage],
    cache: &CacheConfig,
    base_url: &str,
) -> Vec<serde_json::Value> {
    let mut wire = Vec::with_capacity(messages.len());

    for msg in messages {
        match msg {
            LlmMessage::User { content } => {
                let blocks = content.iter().map(llm_content_to_anthropic).collect();
                wire.push(serde_json::json!({
                    "role": "user",
                    "content": serde_json::Value::Array(blocks),
                }));
            }
            LlmMessage::Assistant { content } => {
                let blocks = content
                    .iter()
                    .filter_map(|c| match c {
                        LlmContent::Text(text) => {
                            Some(serde_json::json!({"type": "text", "text": text}))
                        }
                        LlmContent::Thinking(text) => {
                            Some(serde_json::json!({"type": "thinking", "thinking": text}))
                        }
                        LlmContent::ToolCall {
                            id,
                            name,
                            arguments,
                        } => Some(serde_json::json!({
                            "type": "tool_use", "id": id, "name": name, "input": arguments,
                        })),
                        LlmContent::Image(_) => None,
                    })
                    .collect();
                wire.push(serde_json::json!({
                    "role": "assistant",
                    "content": serde_json::Value::Array(blocks),
                }));
            }
            LlmMessage::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                let blocks: Vec<serde_json::Value> =
                    content.iter().map(llm_content_to_anthropic).collect();
                wire.push(serde_json::json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": serde_json::Value::Array(blocks),
                        "is_error": is_error,
                    }],
                }));
            }
        }
    }

    if cache.retention != CacheRetention::None
        && let Some(last_user_idx) = wire.iter().rposition(|m| m["role"] == "user")
        && let Some(content) = wire[last_user_idx]["content"].as_array_mut()
        && let Some(last_block) = content.last_mut()
    {
        last_block["cache_control"] = cache_control_value(cache, base_url);
    }

    wire
}

fn llm_content_to_anthropic(content: &LlmContent) -> serde_json::Value {
    match content {
        LlmContent::Text(text) => serde_json::json!({"type": "text", "text": text}),
        LlmContent::Image(source) => serde_json::json!({
            "type": "image",
            "source": {"type": "base64", "media_type": source.media_type, "data": source.data},
        }),
        LlmContent::ToolCall {
            id,
            name,
            arguments,
        } => serde_json::json!({
            "type": "tool_use", "id": id, "name": name, "input": arguments,
        }),
        LlmContent::Thinking(text) => serde_json::json!({"type": "thinking", "thinking": text}),
    }
}

fn cache_control_value(cache: &CacheConfig, base_url: &str) -> serde_json::Value {
    match cache.retention {
        CacheRetention::None => serde_json::Value::Null,
        CacheRetention::Short => serde_json::json!({"type": "ephemeral"}),
        CacheRetention::Long => {
            if base_url.contains("api.anthropic.com") {
                serde_json::json!({"type": "ephemeral", "ttl": "1h"})
            } else {
                serde_json::json!({"type": "ephemeral"})
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_block_stop_does_not_emit_tool_end() {
        let mut state = SseState::default();

        // content_block_start for text (index 0)
        let events = parse_sse_event(
            "content_block_start",
            &serde_json::json!({"index": 0, "content_block": {"type": "text", "text": ""}}),
            &mut state,
        );
        assert!(events.is_empty());

        // content_block_stop for text (index 0) — should NOT emit ToolCallEnd
        let events = parse_sse_event(
            "content_block_stop",
            &serde_json::json!({"index": 0}),
            &mut state,
        );
        assert!(
            events.is_empty(),
            "text block stop should not emit ToolCallEnd"
        );
    }

    #[test]
    fn thinking_block_stop_does_not_emit_tool_end() {
        let mut state = SseState::default();

        let events = parse_sse_event(
            "content_block_start",
            &serde_json::json!({"index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            &mut state,
        );
        assert!(events.is_empty());

        let events = parse_sse_event(
            "content_block_stop",
            &serde_json::json!({"index": 0}),
            &mut state,
        );
        assert!(
            events.is_empty(),
            "thinking block stop should not emit ToolCallEnd"
        );
    }

    #[test]
    fn tool_use_block_lifecycle() {
        let mut state = SseState::default();

        // content_block_start for tool_use (index 1)
        let events = parse_sse_event(
            "content_block_start",
            &serde_json::json!({"index": 1, "content_block": {"type": "tool_use", "id": "toolu_abc123", "name": "read"}}),
            &mut state,
        );
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], ProviderEvent::ToolCallStart { id, name }
            if id == "toolu_abc123" && name == "read")
        );

        // input_json_delta — should use real tool ID, not block index
        let events = parse_sse_event(
            "content_block_delta",
            &serde_json::json!({"index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}}),
            &mut state,
        );
        assert_eq!(events.len(), 1);
        if let ProviderEvent::ToolCallArgsDelta { id, delta } = &events[0] {
            assert_eq!(id, "toolu_abc123", "should use real tool ID");
            assert_eq!(delta, "{\"path\":");
        } else {
            panic!("expected ToolCallArgsDelta");
        }

        // content_block_stop for tool_use (index 1) — should emit ToolCallEnd
        let events = parse_sse_event(
            "content_block_stop",
            &serde_json::json!({"index": 1}),
            &mut state,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], ProviderEvent::ToolCallEnd { id } if id == "toolu_abc123"));
    }

    #[test]
    fn mixed_blocks_only_tool_gets_end_event() {
        let mut state = SseState::default();

        // Block 0: text
        parse_sse_event(
            "content_block_start",
            &serde_json::json!({"index": 0, "content_block": {"type": "text", "text": ""}}),
            &mut state,
        );
        // Block 1: tool_use
        parse_sse_event(
            "content_block_start",
            &serde_json::json!({"index": 1, "content_block": {"type": "tool_use", "id": "toolu_xyz", "name": "bash"}}),
            &mut state,
        );
        // Block 2: thinking
        parse_sse_event(
            "content_block_start",
            &serde_json::json!({"index": 2, "content_block": {"type": "thinking", "thinking": ""}}),
            &mut state,
        );

        // Stop block 0 (text) — no ToolCallEnd
        let e = parse_sse_event(
            "content_block_stop",
            &serde_json::json!({"index": 0}),
            &mut state,
        );
        assert!(e.is_empty());

        // Stop block 2 (thinking) — no ToolCallEnd
        let e = parse_sse_event(
            "content_block_stop",
            &serde_json::json!({"index": 2}),
            &mut state,
        );
        assert!(e.is_empty());

        // Stop block 1 (tool_use) — ToolCallEnd with correct ID
        let e = parse_sse_event(
            "content_block_stop",
            &serde_json::json!({"index": 1}),
            &mut state,
        );
        assert_eq!(e.len(), 1);
        assert!(matches!(&e[0], ProviderEvent::ToolCallEnd { id } if id == "toolu_xyz"));
    }

    #[test]
    fn message_start_extracts_usage() {
        let mut state = SseState::default();
        let events = parse_sse_event(
            "message_start",
            &serde_json::json!({"message": {"usage": {"input_tokens": 150, "output_tokens": 0}}}),
            &mut state,
        );
        assert_eq!(events.len(), 1);
        if let ProviderEvent::UsageUpdate(u) = &events[0] {
            assert_eq!(u.input, 150);
        } else {
            panic!("expected UsageUpdate");
        }
    }

    #[test]
    fn message_delta_stop_reason() {
        let mut state = SseState::default();
        let events = parse_sse_event(
            "message_delta",
            &serde_json::json!({"delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 50}}),
            &mut state,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            ProviderEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                ..
            }
        ));
    }
}
