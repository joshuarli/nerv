use std::borrow::Cow;
use std::sync::Arc;

use crate::agent::convert::{LlmContent, LlmMessage};
use crate::agent::provider::{CacheConfig, CompletionRequest, Provider, ProviderEvent, new_cancel_flag};
use crate::agent::types::{AgentMessage, ContentBlock, ContentItem};

// Per-field character caps applied during conversation serialisation for
// compaction. These bound individual fields before the overall prompt clamp
// in `clamp_conversation`. The caps are conservative: the summariser needs
// intent and key content, not verbatim reproduction.

/// Maximum characters kept from a single User or Assistant text block.
/// 4 000 chars ≈ 1 000 tokens — enough to capture intent and key content.
const FIELD_CAP_USER_ASSISTANT: usize = 4_000;

/// Maximum characters kept from a single ToolResult content item or
/// BashExecution output block. 2 000 chars ≈ 500 tokens. Tool output is
/// often repetitive; leading and trailing content carry the most signal.
const FIELD_CAP_TOOL_OUTPUT: usize = 2_000;

/// Truncate `s` to at most `cap` characters, appending `...[truncated]` if
/// the string was cut. Uses `floor_char_boundary` so the result is always
/// valid UTF-8.
fn trunc(s: &str, cap: usize) -> Cow<'_, str> {
    if s.len() <= cap {
        return Cow::Borrowed(s);
    }
    let end = s.floor_char_boundary(cap);
    Cow::Owned(format!("{}...[truncated]", &s[..end]))
}

pub fn serialize_conversation(messages: &[AgentMessage]) -> String {
    let mut out = String::new();
    for msg in messages {
        match msg {
            AgentMessage::User { content, .. } => {
                out.push_str("User: ");
                for item in content {
                    if let ContentItem::Text { text } = item {
                        out.push_str(&trunc(text, FIELD_CAP_USER_ASSISTANT));
                    }
                }
                out.push('\n');
            }
            AgentMessage::Assistant(a) => {
                // Only emit Text blocks. Thinking blocks are internal
                // chain-of-thought; including them wastes tokens and can
                // mislead the summariser.
                let text: String = a
                    .content
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::Text { text } = b {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                if !text.is_empty() {
                    out.push_str("Assistant: ");
                    out.push_str(&trunc(&text, FIELD_CAP_USER_ASSISTANT));
                    out.push('\n');
                }
            }
            AgentMessage::ToolResult { content, is_error, .. } => {
                out.push_str(if *is_error { "Tool Error: " } else { "Tool Result: " });
                for item in content {
                    if let ContentItem::Text { text } = item {
                        out.push_str(&trunc(text, FIELD_CAP_TOOL_OUTPUT));
                    }
                }
                out.push('\n');
            }
            AgentMessage::BashExecution { command, output, .. } => {
                // command is always short; output can be very large.
                out.push_str("Bash: ");
                out.push_str(command);
                out.push('\n');
                out.push_str("Output: ");
                out.push_str(&trunc(output, FIELD_CAP_TOOL_OUTPUT));
                out.push('\n');
            }
            AgentMessage::CompactionSummary { summary, .. } => {
                out.push_str(&format!("[Previous summary: {}]\n", summary));
            }
            AgentMessage::BranchSummary { summary, .. } => {
                out.push_str(&format!("[Branch summary: {}]\n", summary));
            }
            _ => {}
        }
    }
    out
}

/// Clamp a serialised conversation string to `char_cap` characters.
///
/// If the string exceeds `char_cap`, it is truncated at the nearest valid
/// UTF-8 char boundary and a notice is appended so the summariser knows the
/// input was cut. This is the final safety net before the string is embedded
/// in the summariser prompt.
pub fn clamp_conversation(s: String, char_cap: usize) -> String {
    if s.len() <= char_cap {
        return s;
    }
    let end = s.floor_char_boundary(char_cap);
    format!(
        "{}...\n[Conversation truncated: exceeded summariser context limit]",
        &s[..end]
    )
}

const SUMMARIZATION_PROMPT: &str = "Summarize the conversation above in structured format: Goal, Progress, Key Decisions, Next Steps, Critical Context. Be concise.";

pub fn generate_summary(
    messages: &[AgentMessage],
    previous_summary: Option<&str>,
    provider: Arc<dyn Provider>,
    model_id: &str,
    summarizer_context_window: u32,
) -> anyhow::Result<String> {
    let conversation = serialize_conversation(messages);

    // Reserve the 4 096-token output budget from the context window before
    // applying the 15 % headroom factor. The headroom covers XML framing,
    // the system prompt, and approximation error in the chars/4 estimator.
    //
    // Example (Haiku, 200k window):
    //   char_cap = (200_000 - 4_096) * 4 * 85 / 100 ≈ 666 526 chars
    //
    // In practice the per-field caps above usually cut the serialised output to
    // a small fraction of this; the clamp fires only for pathological sessions
    // or small-window compaction models.
    let char_cap = (summarizer_context_window as usize)
        .saturating_sub(4096)
        .saturating_mul(4)
        .saturating_mul(85)
        / 100;
    let conversation = clamp_conversation(conversation, char_cap);

    let prompt = if let Some(prev) = previous_summary {
        format!(
            "<previous_summary>\n{prev}\n</previous_summary>\n\n\
             <conversation>\n{conversation}\n</conversation>\n\n\
             Update the previous summary.\n\n{SUMMARIZATION_PROMPT}"
        )
    } else {
        format!("<conversation>\n{conversation}\n</conversation>\n\n{SUMMARIZATION_PROMPT}")
    };

    let request = CompletionRequest {
        model_id: model_id.to_string(),
        system_prompt: "You are a conversation summarizer.".to_string(),
        messages: vec![LlmMessage::User {
            content: vec![LlmContent::Text(prompt)],
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_user(text: &str) -> AgentMessage {
        AgentMessage::User {
            content: vec![ContentItem::Text { text: text.to_string() }],
            timestamp: 0,
        }
    }

    fn make_tool_result(text: &str, is_error: bool) -> AgentMessage {
        AgentMessage::ToolResult {
            tool_call_id: "id".to_string(),
            content: vec![ContentItem::Text { text: text.to_string() }],
            is_error,
            display: None,
            details: None,
            timestamp: 0,
        }
    }

    fn make_bash(command: &str, output: &str) -> AgentMessage {
        AgentMessage::BashExecution {
            command: command.to_string(),
            output: output.to_string(),
            exit_code: None,
            timestamp: 0,
        }
    }

    fn long(n: usize) -> String {
        "x".repeat(n)
    }

    #[test]
    fn trunc_multibyte_boundary() {
        // "é" is 2 bytes; cap at 1 byte must not panic and must snap to a valid char boundary.
        let s = "éàü";
        let result = trunc(s, 1);
        assert!(result.ends_with("...[truncated]") || result.as_ref() == s);
    }

    // serialize_conversation

    #[test]
    fn serialize_caps_user() {
        let msg = make_user(&long(FIELD_CAP_USER_ASSISTANT + 100));
        let out = serialize_conversation(&[msg]);
        assert!(out.contains("...[truncated]"));
        assert!(out.starts_with("User: "));
    }

    #[test]
    fn serialize_caps_tool_result() {
        let msg = make_tool_result(&long(FIELD_CAP_TOOL_OUTPUT + 100), false);
        let out = serialize_conversation(&[msg]);
        assert!(out.contains("...[truncated]"));
        assert!(out.starts_with("Tool Result: "));
    }

    #[test]
    fn serialize_tool_result_error_prefix() {
        let msg = make_tool_result("oops", true);
        let out = serialize_conversation(&[msg]);
        assert!(out.starts_with("Tool Error: "));
    }

    #[test]
    fn serialize_bash_execution() {
        let msg = make_bash("cargo test", &long(FIELD_CAP_TOOL_OUTPUT + 100));
        let out = serialize_conversation(&[msg]);
        assert!(out.contains("Bash: cargo test"));
        assert!(out.contains("Output: "));
        assert!(out.contains("...[truncated]"));
    }

    #[test]
    fn serialize_bash_command_not_truncated() {
        let msg = make_bash("cargo test", &long(FIELD_CAP_TOOL_OUTPUT + 100));
        let out = serialize_conversation(&[msg]);
        // The command line itself must appear verbatim.
        assert!(out.contains("Bash: cargo test\n"));
    }

    #[test]
    fn serialize_compaction_summary() {
        let msg = AgentMessage::CompactionSummary {
            summary: "prior work".to_string(),
            tokens_before: 0,
            timestamp: 0,
        };
        let out = serialize_conversation(&[msg]);
        assert_eq!(out, "[Previous summary: prior work]\n");
    }

    #[test]
    fn serialize_branch_summary() {
        let msg = AgentMessage::BranchSummary {
            summary: "branch context".to_string(),
            from_id: "abc".to_string(),
            timestamp: 0,
        };
        let out = serialize_conversation(&[msg]);
        assert_eq!(out, "[Branch summary: branch context]\n");
    }

    // clamp_conversation

    #[test]
    fn clamp_conversation_under() {
        let s = "short".to_string();
        assert_eq!(clamp_conversation(s.clone(), 100), s);
    }

    #[test]
    fn clamp_conversation_over() {
        let s = long(200);
        let result = clamp_conversation(s, 100);
        assert!(result.contains("[Conversation truncated"));
        assert!(result.len() < 200 + 100); // well under original
    }

    #[test]
    fn clamp_conversation_multibyte() {
        let s = "é".repeat(50); // 100 bytes, 50 chars
        // cap at 3 bytes — must not panic
        let result = clamp_conversation(s, 3);
        assert!(result.contains("[Conversation truncated"));
    }
}
