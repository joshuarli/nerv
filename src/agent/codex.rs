use std::io::BufRead;
use std::sync::atomic::Ordering;

use serde::Deserialize;

use super::convert::{LlmContent, LlmMessage};
use super::provider::*;
use super::types::*;
use crate::errors::ProviderError;

#[derive(Deserialize)]
struct CodexUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_tokens: u32,
}

#[derive(Deserialize)]
struct CodexMessage {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct CodexChoice {
    #[serde(default)]
    delta: Option<CodexMessage>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct CodexChunk {
    #[serde(default)]
    choices: Vec<CodexChoice>,
    #[serde(default)]
    usage: Option<CodexUsage>,
}

pub struct CodexProvider {
    api_key: Option<String>,
    base_url: String,
    name: String,
    extra_headers: Vec<(String, String)>,
}

impl CodexProvider {
    pub fn new(name: String, base_url: String, api_key: Option<String>) -> Self {
        Self {
            api_key,
            base_url,
            name,
            extra_headers: Vec::new(),
        }
    }

    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.extra_headers = headers;
        self
    }

    fn build_request_body(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": request.model_id,
            "max_tokens": request.max_tokens,
            "stream": true,
        });

        let mut messages = Vec::new();

        // Add system prompt
        messages.push(serde_json::json!({"role": "system", "content": request.system_prompt}));

        // Add conversation messages
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
                    let mut text_parts = Vec::new();
                    let mut tool_calls = Vec::new();
                    for c in content {
                        match c {
                            LlmContent::Text(text) => {
                                text_parts.push(text.as_str());
                            }
                            LlmContent::ToolCall { id, name, arguments } => {
                                tool_calls.push(serde_json::json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": arguments
                                    }
                                }));
                            }
                            // Ignore unsupported content types (Image, Thinking)
                            _ => {}
                        }
                    }

                    // Assistant message with text (if any)
                    if !text_parts.is_empty() || !tool_calls.is_empty() {
                        let mut msg = serde_json::json!({"role": "assistant"});
                        if !text_parts.is_empty() {
                            msg["content"] = serde_json::json!(text_parts.join("\n"));
                        }
                        if !tool_calls.is_empty() {
                            msg["tool_calls"] = serde_json::json!(tool_calls);
                        }
                        messages.push(msg);
                    }
                }
                LlmMessage::ToolResult { tool_call_id, content, .. } => {
                    // Tool results go in user messages in the next turn
                    let text = content
                        .iter()
                        .filter_map(|c| match c {
                            LlmContent::Text(t) => Some(t.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": format!("Tool {} result: {}", tool_call_id, text)
                    }));
                }
            }
        }

        body["input"] = serde_json::json!(messages);

        // Add tools if present
        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(tools);
        }

        body
    }

    fn parse_usage(usage: &CodexUsage) -> Usage {
        Usage {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_read: usage.cache_read_tokens,
            cache_write: 0,
        }
    }

    fn parse_stop_reason(reason: &Option<String>) -> StopReason {
        match reason.as_deref() {
            Some("stop") => StopReason::EndTurn,
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::MaxTokens,
            Some("abort") => StopReason::Aborted,
            _ => StopReason::EndTurn,
        }
    }
}

impl Provider for CodexProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn healthcheck(&self) -> bool {
        // Codex API doesn't have a simple healthcheck endpoint
        // We'll assume it's healthy if we have a valid configuration
        self.api_key.is_some() || !self.base_url.is_empty()
    }

    fn stream_completion(
        &self,
        request: &CompletionRequest,
        cancel: &CancelFlag,
        on_event: &mut dyn FnMut(ProviderEvent),
    ) -> Result<(), ProviderError> {
        let body = self.build_request_body(request);
        let url = format!("{}/v1/chat/completions", self.base_url);

        let mut req = crate::http::agent()
            .post(&url)
            .header("content-type", "application/json");

        if let Some(key) = &self.api_key {
            req = req.header("authorization", &format!("Bearer {}", key));
        }

        // Apply extra headers from config
        for (k, v) in &self.extra_headers {
            req = req.header(k, v);
        }

        crate::log::debug(&format!(
            "codex request: url={} model={} body={}",
            url,
            request.model_id,
            serde_json::to_string(&body).unwrap_or_default(),
        ));

        let response = match req.send_json(&body) {
            Ok(r) => r,
            Err(e) => {
                crate::log::warn(&format!("codex request error: {}", e));
                return Err(ProviderError::SseParse { message: e.to_string() });
            }
        };

        // Use ureq's into_body().as_reader()
        let mut body = response.into_body();
        let reader = std::io::BufReader::new(body.as_reader());
        let mut lines = reader.lines();

        let mut usage: Option<Usage> = None;

        while let Some(Ok(line)) = lines.next() {
            // Check for cancellation
            if cancel.load(Ordering::Relaxed) {
                return Err(ProviderError::SseParse {
                    message: "Request cancelled".to_string(),
                });
            }

            // Skip empty lines and SSE comment lines
            if line.is_empty() || line.starts_with(':') {
                continue;
            }

            // Parse the line as JSON
            let chunk: CodexChunk = match serde_json::from_str(&line) {
                Ok(c) => c,
                Err(e) => {
                    crate::log::warn(&format!("Failed to parse SSE line as JSON: {}", e));
                    continue;
                }
            };

            // Handle usage info
            if let Some(u) = &chunk.usage {
                usage = Some(Self::parse_usage(u));
            }

            // Handle choices/delta
            for choice in &chunk.choices {
                if let Some(delta) = &choice.delta {
                    if let Some(content) = &delta.content {
                        on_event(ProviderEvent::TextDelta(content.clone()));
                    }
                }

                // Check for finish reason
                if let Some(reason) = &choice.finish_reason {
                    let usage_val = usage.take().unwrap_or_else(Usage::default);
                    on_event(ProviderEvent::MessageStop {
                        stop_reason: Self::parse_stop_reason(&Some(reason.clone())),
                        usage: usage_val,
                    });
                }
            }
        }

        Ok(())
    }
}
