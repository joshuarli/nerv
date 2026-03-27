use std::io::BufRead;
use std::sync::atomic::Ordering;

use serde::Deserialize;

use crate::errors::ProviderError;

use super::convert::{LlmContent, LlmMessage};
use super::provider::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Typed SSE event structs — avoids serde_json::Value DOM on the hot path
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SseUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum SseContentBlock {
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum SseDelta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct SseMessageStart {
    message: SseMessageStartInner,
}
#[derive(Deserialize)]
struct SseMessageStartInner {
    usage: SseUsage,
}

#[derive(Deserialize)]
struct SseContentBlockStart {
    index: u32,
    content_block: SseContentBlock,
}

#[derive(Deserialize)]
struct SseContentBlockDelta {
    index: u32,
    delta: SseDelta,
}

#[derive(Deserialize)]
struct SseContentBlockStop {
    index: u32,
}

#[derive(Deserialize)]
struct SseMessageDeltaInner {
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct SseMessageDelta {
    delta: SseMessageDeltaInner,
    usage: SseUsage,
}

#[derive(Deserialize)]
struct SseError {
    error: SseErrorInner,
}
#[derive(Deserialize)]
struct SseErrorInner {
    message: String,
}

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
            // Extract retry-after before consuming the body.
            // Anthropic sends `retry-after` (seconds, float) on 429s and
            // `anthropic-ratelimit-requests-reset` (ISO-8601 timestamp) on rate-limit
            // headers — prefer the simpler `retry-after` value when present.
            let retry_after_ms: Option<u64> = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<f64>().ok())
                .map(|secs| (secs * 1000.0) as u64);

            let err_body = response.into_body().read_to_string().unwrap_or_default();
            crate::log::warn(&format!("anthropic HTTP {}: {}", status, err_body));
            let message = serde_json::from_str::<SseError>(&err_body)
                .ok()
                .map(|e| e.error.message)
                .unwrap_or_else(|| format!("HTTP {}", status));
            return Err(classify_status(status, &message, retry_after_ms));
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
                for event in parse_sse_event(&current_event_type, data, &mut sse_state) {
                    let is_stop = matches!(event, ProviderEvent::MessageStop { .. });
                    on_event(event);
                    if is_stop {
                        return Ok(());
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
    tool_ids: std::collections::HashMap<u32, String>,
}

fn parse_sse_event(event_type: &str, data: &str, state: &mut SseState) -> Vec<ProviderEvent> {
    match event_type {
        "message_start" => {
            let Ok(ev) = serde_json::from_str::<SseMessageStart>(data) else {
                return vec![];
            };
            let u = ev.message.usage;
            // input = total tokens in context window: non-cached + cache hits + cache writes.
            // Anthropic splits these when prompt caching is active; we must sum all three or
            // the context counter shows only the tiny non-cached slice (often just 1 token).
            let total_input = u.input_tokens
                + u.cache_read_input_tokens
                + u.cache_creation_input_tokens;
            if total_input > 0 || u.output_tokens > 0 {
                vec![ProviderEvent::UsageUpdate(Usage {
                    input: total_input,
                    output: u.output_tokens,
                    cache_read: u.cache_read_input_tokens,
                    cache_write: u.cache_creation_input_tokens,
                })]
            } else {
                vec![]
            }
        }
        "content_block_start" => {
            let Ok(ev) = serde_json::from_str::<SseContentBlockStart>(data) else {
                return vec![];
            };
            match ev.content_block {
                SseContentBlock::ToolUse { id, name } => {
                    state.tool_ids.insert(ev.index, id.clone());
                    vec![ProviderEvent::ToolCallStart { id, name }]
                }
                SseContentBlock::Other => vec![],
            }
        }
        "content_block_delta" => {
            let Ok(ev) = serde_json::from_str::<SseContentBlockDelta>(data) else {
                return vec![];
            };
            match ev.delta {
                SseDelta::Text { text } => vec![ProviderEvent::TextDelta(text)],
                SseDelta::Thinking { thinking } => {
                    vec![ProviderEvent::ThinkingDelta(thinking)]
                }
                SseDelta::InputJson { partial_json } => {
                    let id = state.tool_ids.get(&ev.index).cloned().unwrap_or_default();
                    vec![ProviderEvent::ToolCallArgsDelta {
                        id,
                        delta: partial_json,
                    }]
                }
                SseDelta::Other => vec![],
            }
        }
        "content_block_stop" => {
            let Ok(ev) = serde_json::from_str::<SseContentBlockStop>(data) else {
                return vec![];
            };
            // Only emit ToolCallEnd for tool_use blocks
            if let Some(id) = state.tool_ids.remove(&ev.index) {
                vec![ProviderEvent::ToolCallEnd { id }]
            } else {
                vec![]
            }
        }
        "message_delta" => {
            let Ok(ev) = serde_json::from_str::<SseMessageDelta>(data) else {
                return vec![];
            };
            let stop_reason = match ev.delta.stop_reason.as_deref() {
                Some("end_turn") => StopReason::EndTurn,
                Some("tool_use") => StopReason::ToolUse,
                Some("max_tokens") => StopReason::MaxTokens,
                _ => StopReason::EndTurn,
            };
            vec![ProviderEvent::MessageStop {
                stop_reason,
                usage: Usage {
                    input: ev.usage.input_tokens
                        + ev.usage.cache_read_input_tokens
                        + ev.usage.cache_creation_input_tokens,
                    output: ev.usage.output_tokens,
                    cache_read: ev.usage.cache_read_input_tokens,
                    cache_write: ev.usage.cache_creation_input_tokens,
                },
            }]
        }
        "error" => {
            let msg = serde_json::from_str::<SseError>(data)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| "unknown error".to_string());
            vec![ProviderEvent::MessageStop {
                stop_reason: StopReason::Error { message: msg },
                usage: Usage::default(),
            }]
        }
        _ => vec![],
    }
}

fn classify_status(status: u16, message: &str, retry_after_ms: Option<u64>) -> ProviderError {
    let message = message.to_string();
    match status {
        401 | 403 => ProviderError::Auth { message },
        429 => ProviderError::RateLimited { retry_after_ms },
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

    // Helper: serialize a json! literal to string, then run through the typed parser.
    // This keeps the test fixtures readable while exercising the real code path.
    fn p(event_type: &str, v: serde_json::Value, state: &mut SseState) -> Vec<ProviderEvent> {
        parse_sse_event(event_type, &v.to_string(), state)
    }

    #[test]
    fn text_block_stop_does_not_emit_tool_end() {
        let mut state = SseState::default();

        // content_block_start for text (index 0)
        let events = p(
            "content_block_start",
            serde_json::json!({"index": 0, "content_block": {"type": "text", "text": ""}}),
            &mut state,
        );
        assert!(events.is_empty());

        // content_block_stop for text (index 0) — should NOT emit ToolCallEnd
        let events = p(
            "content_block_stop",
            serde_json::json!({"index": 0}),
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

        let events = p(
            "content_block_start",
            serde_json::json!({"index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            &mut state,
        );
        assert!(events.is_empty());

        let events = p(
            "content_block_stop",
            serde_json::json!({"index": 0}),
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
        let events = p(
            "content_block_start",
            serde_json::json!({"index": 1, "content_block": {"type": "tool_use", "id": "toolu_abc123", "name": "read"}}),
            &mut state,
        );
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], ProviderEvent::ToolCallStart { id, name }
            if id == "toolu_abc123" && name == "read")
        );

        // input_json_delta — should use real tool ID, not block index
        let events = p(
            "content_block_delta",
            serde_json::json!({"index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}}),
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
        let events = p(
            "content_block_stop",
            serde_json::json!({"index": 1}),
            &mut state,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], ProviderEvent::ToolCallEnd { id } if id == "toolu_abc123"));
    }

    #[test]
    fn mixed_blocks_only_tool_gets_end_event() {
        let mut state = SseState::default();

        // Block 0: text
        p(
            "content_block_start",
            serde_json::json!({"index": 0, "content_block": {"type": "text", "text": ""}}),
            &mut state,
        );
        // Block 1: tool_use
        p(
            "content_block_start",
            serde_json::json!({"index": 1, "content_block": {"type": "tool_use", "id": "toolu_xyz", "name": "bash"}}),
            &mut state,
        );
        // Block 2: thinking
        p(
            "content_block_start",
            serde_json::json!({"index": 2, "content_block": {"type": "thinking", "thinking": ""}}),
            &mut state,
        );

        // Stop block 0 (text) — no ToolCallEnd
        let e = p(
            "content_block_stop",
            serde_json::json!({"index": 0}),
            &mut state,
        );
        assert!(e.is_empty());

        // Stop block 2 (thinking) — no ToolCallEnd
        let e = p(
            "content_block_stop",
            serde_json::json!({"index": 2}),
            &mut state,
        );
        assert!(e.is_empty());

        // Stop block 1 (tool_use) — ToolCallEnd with correct ID
        let e = p(
            "content_block_stop",
            serde_json::json!({"index": 1}),
            &mut state,
        );
        assert_eq!(e.len(), 1);
        assert!(matches!(&e[0], ProviderEvent::ToolCallEnd { id } if id == "toolu_xyz"));
    }

    #[test]
    fn message_start_extracts_usage() {
        let mut state = SseState::default();
        let events = p(
            "message_start",
            serde_json::json!({"message": {"usage": {"input_tokens": 150, "output_tokens": 0}}}),
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
        let events = p(
            "message_delta",
            serde_json::json!({"delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 50}}),
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
