//! OpenAI Codex provider — uses the ChatGPT backend Responses API.
//!
//! The wire format differs from the public OpenAI API:
//!   - Endpoint:  https://chatgpt.com/backend-api/codex/responses
//!   - Auth:      Bearer token + `chatgpt-account-id` extracted from the JWT
//!   - Body:      `input` (not `messages`), system prompt in `instructions`
//!   - SSE:       `response.output_text.delta`, `response.function_call_arguments.delta`,
//!     `response.output_item.added/done`, `response.completed`, `response.failed`
use std::collections::HashMap;
use std::io::BufRead;
use std::sync::atomic::Ordering;

use serde::Deserialize;

use super::convert::{LlmContent, LlmMessage};
use super::provider::*;
use super::types::*;
use crate::errors::ProviderError;

const BASE_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

// ── SSE event shapes ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct EvOutputItemAdded {
    // output_index lives on the outer event, not inside the item object.
    output_index: usize,
    item: OutputItem,
}

#[derive(Deserialize)]
struct EvOutputItemDone {
    output_index: usize,
    item: OutputItemDone,
}

#[derive(Deserialize)]
struct EvTextDelta {
    #[allow(dead_code)]
    output_index: usize,
    delta: String,
}

#[derive(Deserialize)]
struct EvFnArgsDelta {
    output_index: usize,
    delta: String,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum OutputItem {
    #[serde(rename = "function_call")]
    FunctionCall { call_id: String, name: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum OutputItemDone {
    #[serde(rename = "function_call")]
    FunctionCall,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct EvCompleted {
    // Optional: the Codex backend sometimes omits this field on certain
    // response.completed events (e.g., for cancelled/failed states).
    response: Option<EvCompletedResponse>,
}

#[derive(Deserialize)]
struct EvCompletedResponse {
    status: Option<String>,
    #[serde(default)]
    usage: EvUsage,
    error: Option<EvError>,
}

#[derive(Deserialize, Default)]
struct EvUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    input_tokens_details: EvInputTokensDetails,
}

#[derive(Deserialize, Default)]
struct EvInputTokensDetails {
    #[serde(default)]
    cached_tokens: u32,
}

#[derive(Deserialize)]
struct EvError {
    message: Option<String>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Deserialize a known SSE event, logging a warning on struct mismatch so
/// schema bugs don't silently swallow events.
fn parse_ev<T: serde::de::DeserializeOwned>(
    event_type: &str,
    raw: serde_json::Value,
) -> Option<T> {
    match serde_json::from_value::<T>(raw) {
        Ok(v) => Some(v),
        Err(e) => {
            crate::log::warn(&format!("codex: failed to parse {}: {}", event_type, e));
            None
        }
    }
}

// ── Provider ──────────────────────────────────────────────────────────────────

pub struct CodexProvider {
    api_key: String,
    extra_headers: Vec<(String, String)>,
}

impl CodexProvider {
    pub fn new(api_key: String) -> Self {
        Self { api_key, extra_headers: Vec::new() }
    }

    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Extract `chatgpt-account-id` from the JWT `https://api.openai.com/auth` claim.
    fn account_id(&self) -> Result<String, ProviderError> {
        let parts: Vec<&str> = self.api_key.split('.').collect();
        if parts.len() != 3 {
            return Err(ProviderError::Auth { message: "invalid JWT".into() });
        }
        let payload = base64_url_decode(parts[1])
            .map_err(|_| ProviderError::Auth { message: "failed to decode JWT payload".into() })?;
        let json: serde_json::Value = serde_json::from_slice(&payload)
            .map_err(|_| ProviderError::Auth { message: "failed to parse JWT payload".into() })?;
        json["https://api.openai.com/auth"]["chatgpt_account_id"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| ProviderError::Auth { message: "chatgpt_account_id not in JWT".into() })
    }

    fn build_request_body(&self, request: &CompletionRequest) -> serde_json::Value {
        // The Responses API takes `instructions` for the system prompt and
        // `input` for the conversation turns.
        let mut input: Vec<serde_json::Value> = Vec::new();

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
                    input.push(serde_json::json!({
                        "role": "user",
                        "content": [{ "type": "input_text", "text": text }]
                    }));
                }
                LlmMessage::Assistant { content } => {
                    let texts: Vec<serde_json::Value> = content
                        .iter()
                        .filter_map(|c| match c {
                            LlmContent::Text(t) if !t.is_empty() => {
                                Some(serde_json::json!({ "type": "output_text", "text": t }))
                            }
                            _ => None,
                        })
                        .collect();
                    if !texts.is_empty() {
                        input.push(serde_json::json!({
                            "role": "assistant",
                            "content": texts
                        }));
                    }
                    // Tool calls become standalone `function_call` items.
                    for c in content {
                        if let LlmContent::ToolCall { id, name, arguments } = c {
                            input.push(serde_json::json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": arguments.to_string()
                            }));
                        }
                    }
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
                    input.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": tool_call_id,
                        "output": text
                    }));
                }
            }
        }

        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                    "strict": false
                })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": request.model_id,
            "store": false,
            "stream": true,
            "instructions": request.system_prompt,
            "input": input,
            "text": { "verbosity": "medium" },
            "include": ["reasoning.encrypted_content"],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools);
        }

        if request.thinking.is_some() {
            body["reasoning"] = serde_json::json!({ "effort": "medium", "summary": "auto" });
        }

        body
    }
}

impl Provider for CodexProvider {
    fn name(&self) -> &str {
        "codex"
    }

    fn stream_completion(
        &self,
        request: &CompletionRequest,
        cancel: &CancelFlag,
        on_event: &mut dyn FnMut(ProviderEvent),
    ) -> Result<(), ProviderError> {
        let account_id = self.account_id()?;
        let body = self.build_request_body(request);

        let mut req = crate::http::agent()
            .post(BASE_URL)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .header("authorization", &format!("Bearer {}", self.api_key))
            .header("chatgpt-account-id", &account_id)
            .header("openai-beta", "responses=experimental")
            .header("originator", "pi");
        for (k, v) in &self.extra_headers {
            req = req.header(k, v);
        }

        let response =
            req.send_json(&body).map_err(|e| ProviderError::SseParse { message: e.to_string() })?;

        let status = response.status().as_u16();
        if status != 200 {
            let err_body = response.into_body().read_to_string().unwrap_or_default();
            crate::log::warn(&format!("codex HTTP {}: {}", status, err_body));
            let message = serde_json::from_str::<serde_json::Value>(&err_body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| format!("server error ({})", status));
            return Err(ProviderError::Server { status, message });
        }

        // Background reader thread so the main loop can check the cancel flag
        // without blocking on network I/O.
        let (line_tx, line_rx) = crossbeam_channel::bounded::<Result<String, String>>(64);
        std::thread::Builder::new()
            .name("nerv-codex-sse".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                let mut body = response.into_body();
                let reader = std::io::BufReader::new(body.as_reader());
                for line_result in reader.lines() {
                    if line_tx.send(line_result.map_err(|e| e.to_string())).is_err() {
                        break; // receiver dropped (cancelled) — closes connection
                    }
                }
            })
            .expect("failed to spawn SSE reader thread");

        // Track in-flight function calls keyed by output_index so arg deltas
        // from parallel tool calls don't clobber each other.
        let mut pending_fns: HashMap<usize, String> = HashMap::new(); // output_index → call_id
        let mut usage = Usage::default();
        let poll = std::time::Duration::from_millis(50);

        loop {
            if cancel.load(Ordering::Relaxed) {
                drop(line_rx);
                on_event(ProviderEvent::MessageStop { stop_reason: StopReason::Aborted, usage });
                return Ok(());
            }

            let line = match line_rx.recv_timeout(poll) {
                Ok(Ok(line)) => line,
                Ok(Err(e)) => return Err(ProviderError::SseParse { message: e }),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    on_event(ProviderEvent::MessageStop {
                        stop_reason: StopReason::EndTurn,
                        usage,
                    });
                    return Ok(());
                }
            };

            let line = line.trim();
            crate::log::debug(&format!("codex sse line: {}", line));
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            // SSE spec allows "data:" with or without a trailing space.
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim_start();
            if data == "[DONE]" {
                on_event(ProviderEvent::MessageStop { stop_reason: StopReason::EndTurn, usage });
                return Ok(());
            }

            let Ok(raw) = serde_json::from_str::<serde_json::Value>(data) else {
                crate::log::debug(&format!("codex sse parse error on: {}", data));
                continue;
            };
            let Some(event_type) = raw["type"].as_str() else {
                crate::log::debug(&format!("codex sse missing 'type' in: {}", data));
                continue;
            };

            match event_type {
                "response.output_item.added" => {
                    if let Some(ev) = parse_ev::<EvOutputItemAdded>("response.output_item.added", raw)
                        && let OutputItem::FunctionCall { call_id, name } = ev.item
                    {
                        pending_fns.insert(ev.output_index, call_id.clone());
                        on_event(ProviderEvent::ToolCallStart { id: call_id, name });
                    }
                }
                "response.output_text.delta" => {
                    if let Some(ev) = parse_ev::<EvTextDelta>("response.output_text.delta", raw)
                        && !ev.delta.is_empty()
                    {
                        on_event(ProviderEvent::TextDelta(ev.delta));
                    }
                }
                "response.function_call_arguments.delta" => {
                    if let Some(ev) = parse_ev::<EvFnArgsDelta>("response.function_call_arguments.delta", raw)
                        && !ev.delta.is_empty()
                    {
                        let id = pending_fns
                            .get(&ev.output_index)
                            .cloned()
                            .unwrap_or_default();
                        on_event(ProviderEvent::ToolCallArgsDelta { id, delta: ev.delta });
                    }
                }
                "response.output_item.done" => {
                    if let Some(ev) = parse_ev::<EvOutputItemDone>("response.output_item.done", raw)
                        && let OutputItemDone::FunctionCall = ev.item
                    {
                        if let Some(call_id) = pending_fns.remove(&ev.output_index) {
                            on_event(ProviderEvent::ToolCallEnd { id: call_id });
                        }
                    }
                }
                "response.completed" | "response.done" | "response.incomplete" => {
                    if let Some(ev) = parse_ev::<EvCompleted>("response.completed", raw) {
                        if let Some(resp) = &ev.response {
                            let u = &resp.usage;
                            usage = Usage {
                                input: u.input_tokens,
                                output: u.output_tokens,
                                cache_read: u.input_tokens_details.cached_tokens,
                                cache_write: 0,
                            };
                            on_event(ProviderEvent::UsageUpdate(usage));

                            if let Some(err) = &resp.error {
                                let msg = err
                                    .message
                                    .clone()
                                    .unwrap_or_else(|| "response failed".into());
                                return Err(ProviderError::Server { status: 200, message: msg });
                            }
                        }

                        let stop_reason = ev
                            .response
                            .as_ref()
                            .and_then(|r| r.status.as_deref())
                            .map(|s| match s {
                                "incomplete" => StopReason::MaxTokens,
                                "cancelled" => StopReason::Aborted,
                                _ => StopReason::EndTurn,
                            })
                            .unwrap_or(StopReason::EndTurn);
                        on_event(ProviderEvent::MessageStop { stop_reason, usage });
                        return Ok(());
                    }
                }
                "response.failed" | "error" => {
                    let msg = raw["response"]["error"]["message"]
                        .as_str()
                        .or_else(|| raw["message"].as_str())
                        .unwrap_or("codex response failed")
                        .to_string();
                    crate::log::warn(&format!("codex error event: {}", msg));
                    return Err(ProviderError::Server { status: 200, message: msg });
                }
                other => {
                    crate::log::debug(&format!("codex sse unhandled event: {}", other));
                }
            }
        }
    }
}

// ── Base64url decode (no deps) ────────────────────────────────────────────────

fn base64_url_decode(input: &str) -> Result<Vec<u8>, ()> {
    let mut s: String = input.replace('-', "+").replace('_', "/");
    match s.len() % 4 {
        2 => s.push_str("=="),
        3 => s.push('='),
        _ => {}
    }
    base64_decode(&s)
}

fn base64_decode(input: &str) -> Result<Vec<u8>, ()> {
    const TABLE: [u8; 256] = {
        let mut t = [255u8; 256];
        let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0usize;
        while i < 64 {
            t[chars[i] as usize] = i as u8;
            i += 1;
        }
        t
    };

    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
    let mut i = 0;
    while i + 3 < bytes.len() {
        let [a, b, c, d] = [
            TABLE[bytes[i] as usize],
            TABLE[bytes[i + 1] as usize],
            TABLE[bytes[i + 2] as usize],
            TABLE[bytes[i + 3] as usize],
        ];
        if a == 255 || b == 255 {
            return Err(());
        }
        out.push((a << 2) | (b >> 4));
        if c != 255 {
            out.push((b << 4) | (c >> 2));
        }
        if d != 255 {
            out.push((c << 6) | d);
        }
        i += 4;
    }
    Ok(out)
}
