use std::sync::Arc;

use crate::agent::provider::*;
use crate::agent::types::*;

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
                            out.push_str(&text[..500]);
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
