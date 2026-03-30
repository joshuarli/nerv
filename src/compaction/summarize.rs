use std::sync::Arc;

use crate::agent::convert::{LlmContent, LlmMessage};
use crate::agent::provider::{CacheConfig, CompletionRequest, Provider, ProviderEvent, new_cancel_flag};
use crate::agent::types::{AgentMessage, ContentItem};

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
            AgentMessage::ToolResult { content, is_error, .. } => {
                out.push_str(if *is_error { "Tool Error: " } else { "Tool Result: " });
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

/// Stop-words filtered out when building a session title.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
    "from", "is", "it", "its", "be", "as", "was", "are", "were", "been", "has", "have", "had",
    "do", "does", "did", "so", "if", "then", "that", "this", "these", "those", "my", "your", "me",
    "i", "we", "us", "our", "can", "could", "would", "should", "will", "may", "might", "just",
    "also", "not", "no", "up", "out", "how", "what", "when", "where", "which", "who", "why",
    "about", "into", "than", "there", "here", "all", "some", "any", "please", "help", "like",
    "using", "use", "used",
];

/// Derive a short session title from the first user message without calling any
/// model.
///
/// Strategy: split on whitespace/punctuation, drop stop-words and short tokens,
/// take the first 5 meaningful words, title-case each one.
pub fn generate_session_name(first_user_message: &str) -> String {
    // Work only with the first 400 chars to keep things fast.
    let text = if first_user_message.len() > 400 {
        let end = first_user_message.floor_char_boundary(400);
        &first_user_message[..end]
    } else {
        first_user_message
    };

    // Split on anything that isn't a letter, digit, underscore, dot, or hyphen.
    // This handles punctuation and whitespace in one pass.
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '.' && c != '-')
        .filter(|w| !w.is_empty())
        .collect();

    let mut kept: Vec<String> = Vec::with_capacity(5);
    for word in &words {
        if kept.len() >= 5 {
            break;
        }
        let lower = word.to_lowercase();
        // Skip stop-words and single-character tokens.
        if word.len() < 2 || STOP_WORDS.contains(&lower.as_str()) {
            continue;
        }
        // Title-case: uppercase first char, rest as-is.
        let mut chars = word.chars();
        let titled = match chars.next() {
            Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            None => continue,
        };
        kept.push(titled);
    }

    if kept.is_empty() {
        // Absolute fallback: use the raw start of the message.
        let fallback = text.trim();
        let end = fallback.char_indices().nth(40).map(|(i, _)| i).unwrap_or(fallback.len());
        return fallback[..end].to_string();
    }

    kept.join(" ")
}
