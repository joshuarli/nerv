use std::borrow::Cow;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::agent::convert::{LlmContent, LlmMessage};
use crate::str::StrExt as _;
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
    let t = s.truncate_chars(cap);
    if t.len() == s.len() {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(format!("{}...[truncated]", t))
    }
}

/// Structured output from the LLM summarizer. Every field is bounded by the
/// prompt (200 chars per string, 10 items per array) so the formatted markdown
/// is predictably sized for context injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredSummary {
    pub goal: String,
    pub progress: String,
    #[serde(default)]
    pub files_modified: Vec<String>,
    #[serde(default)]
    pub key_decisions: Vec<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub critical_context: String,
}

impl StructuredSummary {
    /// Format as readable markdown for injection into the conversation as the
    /// CompactionSummary message. This is what the LLM sees post-compaction.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("**Goal:** {}\n", self.goal));
        out.push_str(&format!("**Progress:** {}\n", self.progress));
        if !self.files_modified.is_empty() {
            out.push_str(&format!("**Files modified:** {}\n", self.files_modified.join(", ")));
        }
        if !self.key_decisions.is_empty() {
            out.push_str(&format!("**Key decisions:** {}\n", self.key_decisions.join("; ")));
        }
        if !self.next_steps.is_empty() {
            out.push_str(&format!("**Next steps:** {}\n", self.next_steps.join("; ")));
        }
        if !self.open_questions.is_empty() {
            out.push_str(&format!("**Open questions:** {}\n", self.open_questions.join("; ")));
        }
        if !self.critical_context.is_empty() {
            out.push_str(&format!("**Critical context:** {}\n", self.critical_context));
        }
        out
    }
}

/// Result of `generate_summary()`. The structured variant is preferred; prose
/// is the fallback when JSON parsing fails.
pub enum GeneratedSummary {
    Structured(StructuredSummary),
    /// Fallback: JSON parse failed, raw LLM output preserved.
    Prose(String),
}

impl GeneratedSummary {
    pub fn to_markdown(&self) -> String {
        match self {
            Self::Structured(s) => s.to_markdown(),
            Self::Prose(s) => s.clone(),
        }
    }

    pub fn structured(&self) -> Option<&StructuredSummary> {
        match self {
            Self::Structured(s) => Some(s),
            Self::Prose(_) => None,
        }
    }
}

/// Strip markdown code fences that LLMs commonly wrap around JSON output.
fn strip_json_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix("```json").or_else(|| s.strip_prefix("```")).unwrap_or(s);
    let s = s.strip_suffix("```").unwrap_or(s);
    s.trim()
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
    let t = s.truncate_chars(char_cap);
    if t.len() == s.len() {
        return s;
    }
    format!("{}...\n[Conversation truncated: exceeded summariser context limit]", t)
}

const SUMMARIZATION_PROMPT: &str = "\
Summarize the conversation above. Output ONLY valid JSON matching this exact schema:
{
  \"goal\": \"string — what the user is trying to accomplish\",
  \"progress\": \"string — what has been done so far\",
  \"files_modified\": [\"list of file paths that were changed\"],
  \"key_decisions\": [\"important choices made\"],
  \"next_steps\": [\"what remains to be done\"],
  \"open_questions\": [\"unresolved questions or blockers\"],
  \"critical_context\": \"string — any other essential context\"
}
Constraints: each string value max 200 chars; arrays max 10 items.
Output only the JSON object, no prose before or after.";

pub fn generate_summary(
    messages: &[AgentMessage],
    previous_summary: Option<&str>,
    provider: Arc<dyn Provider>,
    model_id: &str,
    summarizer_context_window: u32,
) -> anyhow::Result<GeneratedSummary> {
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
             Update the previous summary with information from the new conversation.\n\n\
             {SUMMARIZATION_PROMPT}"
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

    let stripped = strip_json_fences(&result);
    match serde_json::from_str::<StructuredSummary>(stripped) {
        Ok(structured) => Ok(GeneratedSummary::Structured(structured)),
        Err(e) => {
            crate::log::warn(&format!("Compaction summary JSON parse failed, using prose fallback: {e}"));
            Ok(GeneratedSummary::Prose(result))
        }
    }
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
    let text = first_user_message.truncate_chars(400);

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
        return fallback.truncate_chars(40).to_string();
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

    #[test]
    fn structured_summary_to_markdown_all_fields() {
        let s = StructuredSummary {
            goal: "fix bug".into(),
            progress: "halfway".into(),
            files_modified: vec!["a.rs".into(), "b.rs".into()],
            key_decisions: vec!["use X".into()],
            next_steps: vec!["test".into(), "deploy".into()],
            open_questions: vec!["perf?".into()],
            critical_context: "deadline friday".into(),
        };
        let md = s.to_markdown();
        assert!(md.contains("**Goal:** fix bug"));
        assert!(md.contains("**Files modified:** a.rs, b.rs"));
        assert!(md.contains("**Next steps:** test; deploy"));
        assert!(md.contains("**Critical context:** deadline friday"));
    }

    #[test]
    fn structured_summary_to_markdown_empty_arrays_omitted() {
        let s = StructuredSummary {
            goal: "goal".into(),
            progress: "done".into(),
            files_modified: vec![],
            key_decisions: vec![],
            next_steps: vec![],
            open_questions: vec![],
            critical_context: String::new(),
        };
        let md = s.to_markdown();
        assert!(!md.contains("Files modified"));
        assert!(!md.contains("Key decisions"));
        assert!(!md.contains("Critical context"));
    }

    #[test]
    fn strip_json_fences_basic() {
        assert_eq!(strip_json_fences("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_json_fences("```\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_json_fences("{\"a\":1}"), "{\"a\":1}");
    }

    #[test]
    fn generated_summary_prose_fallback() {
        let raw = "Just some prose summary.";
        let result: Result<StructuredSummary, _> = serde_json::from_str(raw);
        assert!(result.is_err(), "prose should not parse as JSON");
        // Verify the fallback path works
        let gs = GeneratedSummary::Prose(raw.into());
        assert_eq!(gs.to_markdown(), raw);
        assert!(gs.structured().is_none());
    }

    #[test]
    fn generated_summary_structured_roundtrip() {
        let json = r#"{"goal":"g","progress":"p","files_modified":[],"key_decisions":[],"next_steps":["s1"],"open_questions":[],"critical_context":""}"#;
        let parsed: StructuredSummary = serde_json::from_str(json).unwrap();
        let gs = GeneratedSummary::Structured(parsed);
        assert!(gs.structured().is_some());
        assert!(gs.to_markdown().contains("**Goal:** g"));
    }

    #[test]
    fn structured_summary_parses_with_missing_optional_fields() {
        // LLMs may omit array fields or critical_context entirely.
        let json = r#"{"goal":"fix it","progress":"done"}"#;
        let parsed: StructuredSummary = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.goal, "fix it");
        assert!(parsed.files_modified.is_empty());
        assert!(parsed.next_steps.is_empty());
        assert!(parsed.critical_context.is_empty());
    }
}
