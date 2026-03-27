use std::sync::Arc;

use crate::agent::provider::*;
use crate::agent::types::*;

/// Default lightweight model used for background tasks (compaction, session naming).
pub const DEFAULT_UTILITY_MODEL: &str = "claude-haiku-4-5";
pub const DEFAULT_UTILITY_PROVIDER: &str = "anthropic";

pub fn serialize_conversation(messages: &[AgentMessage]) -> String {
    let mut out = String::new();
    for msg in messages {
        match msg {
            AgentMessage::User { content, .. } => {
                out.push_str("User: ");
                for item in content {
                    if let ContentItem::Text { text } = item {
                        out.push_str(text);
                    }
                }
                out.push('\n');
            }
            AgentMessage::Assistant(a) => {
                out.push_str("Assistant: ");
                out.push_str(&a.text_content());
                out.push('\n');
            }
            AgentMessage::ToolResult {
                content, is_error, ..
            } => {
                out.push_str(if *is_error {
                    "Tool Error: "
                } else {
                    "Tool Result: "
                });
                for item in content {
                    if let ContentItem::Text { text } = item {
                        if text.len() > 500 {
                            let end = text.floor_char_boundary(500);
                            out.push_str(&text[..end]);
                            out.push_str("...[truncated]");
                        } else {
                            out.push_str(text);
                        }
                    }
                }
                out.push('\n');
            }
            AgentMessage::CompactionSummary { summary, .. } => {
                out.push_str(&format!("[Previous summary: {}]\n", summary));
            }
            _ => {}
        }
    }
    out
}

const SUMMARIZATION_PROMPT: &str = "Summarize the conversation above in structured format: Goal, Progress, Key Decisions, Next Steps, Critical Context. Be concise.";

pub fn generate_summary(
    messages: &[AgentMessage],
    previous_summary: Option<&str>,
    provider: Arc<dyn Provider>,
    model_id: &str,
) -> anyhow::Result<String> {
    let conversation = serialize_conversation(messages);
    let prompt = if let Some(prev) = previous_summary {
        format!(
            "<previous_summary>\n{prev}\n</previous_summary>\n\n<conversation>\n{conversation}\n</conversation>\n\nUpdate the previous summary.\n\n{SUMMARIZATION_PROMPT}"
        )
    } else {
        format!("<conversation>\n{conversation}\n</conversation>\n\n{SUMMARIZATION_PROMPT}")
    };

    let request = CompletionRequest {
        model_id: model_id.to_string(),
        system_prompt: "You are a conversation summarizer.".to_string(),
        messages: vec![crate::agent::convert::LlmMessage::User {
            content: vec![crate::agent::convert::LlmContent::Text(prompt)],
        }],
        tools: vec![],
        max_tokens: 4096,
        thinking: None,
        cache: CacheConfig::default(),
    };
    let cancel = new_cancel_flag();
    let mut result = String::new();
    provider.stream_completion(&request, &cancel, &mut |event| {
        if let ProviderEvent::TextDelta(delta) = event {
            result.push_str(&delta);
        }
    })?;
    Ok(result)
}

/// Generate a short session title (4–6 words) from the first user message.
/// Returns an error if the provider call fails; callers should treat errors as non-fatal.
pub fn generate_session_name(
    first_user_message: &str,
    provider: Arc<dyn Provider>,
    model_id: &str,
) -> anyhow::Result<String> {
    // Truncate long messages so the naming call stays cheap.
    let snippet = if first_user_message.len() > 400 {
        let end = first_user_message.floor_char_boundary(400);
        &first_user_message[..end]
    } else {
        first_user_message
    };

    let prompt = format!(
        "Reply with only a short title of 4–6 words (no punctuation, no quotes) \
         that describes this request: {snippet}"
    );

    let request = CompletionRequest {
        model_id: model_id.to_string(),
        system_prompt: "You are a session title generator. Reply with only the title, nothing else.".to_string(),
        messages: vec![crate::agent::convert::LlmMessage::User {
            content: vec![crate::agent::convert::LlmContent::Text(prompt)],
        }],
        tools: vec![],
        max_tokens: 20,
        thinking: None,
        cache: CacheConfig::default(),
    };

    let cancel = new_cancel_flag();
    let mut result = String::new();
    provider.stream_completion(&request, &cancel, &mut |event| {
        if let ProviderEvent::TextDelta(delta) = event {
            result.push_str(&delta);
        }
    })?;

    // Strip surrounding whitespace and any wrapping quotes the model may add.
    let name = result.trim().trim_matches('"').trim_matches('\'').trim().to_string();
    if name.is_empty() {
        anyhow::bail!("empty session name returned by model");
    }
    Ok(name)
}
