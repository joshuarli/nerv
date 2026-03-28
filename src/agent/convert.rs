use super::types::*;

/// Wire-format message that providers understand. Providers never see AgentMessage.
#[derive(Debug, Clone)]
pub enum LlmMessage {
    User {
        content: Vec<LlmContent>,
    },
    Assistant {
        content: Vec<LlmContent>,
    },
    ToolResult {
        tool_call_id: String,
        content: Vec<LlmContent>,
        is_error: bool,
    },
}

impl LlmMessage {
    pub fn role(&self) -> &'static str {
        match self {
            Self::User { .. } => "user",
            Self::Assistant { .. } => "assistant",
            Self::ToolResult { .. } => "tool_result",
        }
    }

    pub fn is_user(&self) -> bool {
        matches!(self, Self::User { .. })
    }

    pub fn is_assistant(&self) -> bool {
        matches!(self, Self::Assistant { .. })
    }
}

#[derive(Debug, Clone)]
pub enum LlmContent {
    Text(String),
    Image(ImageSource),
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    Thinking(String),
}

/// Convert AgentMessage[] to LlmMessage[]. Non-LLM messages (Custom,
/// BashExecution, CompactionSummary, BranchSummary) become User text messages.
/// Consecutive same-role messages are merged.
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<LlmMessage> {
    let mut result: Vec<LlmMessage> = Vec::with_capacity(messages.len());

    for msg in messages {
        let llm_msg = match msg {
            AgentMessage::User { content, .. } => {
                let items = content.iter().map(content_item_to_llm).collect();
                LlmMessage::User { content: items }
            }
            AgentMessage::Assistant(assistant) => {
                let items = assistant
                    .content
                    .iter()
                    .map(|block| match block {
                        ContentBlock::Text { text } => LlmContent::Text(text.clone()),
                        ContentBlock::Thinking { thinking } => {
                            LlmContent::Thinking(thinking.clone())
                        }
                        ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                        } => LlmContent::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                    })
                    .collect();
                LlmMessage::Assistant { content: items }
            }
            AgentMessage::ToolResult {
                tool_call_id,
                content,
                is_error,
                ..
            } => {
                let items = content.iter().map(content_item_to_llm).collect();
                LlmMessage::ToolResult {
                    tool_call_id: tool_call_id.clone(),
                    content: items,
                    is_error: *is_error,
                }
            }
            AgentMessage::Custom { content, .. } => {
                let text = content
                    .iter()
                    .filter_map(|item| match item {
                        ContentItem::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                LlmMessage::User {
                    content: vec![LlmContent::Text(text)],
                }
            }
            AgentMessage::BashExecution {
                command,
                output,
                exit_code,
                ..
            } => {
                let text = format!(
                    "[Bash execution]\n$ {}\n{}\n[exit code: {}]",
                    command,
                    output,
                    exit_code.unwrap_or(-1)
                );
                LlmMessage::User {
                    content: vec![LlmContent::Text(text)],
                }
            }
            AgentMessage::CompactionSummary { summary, .. } => LlmMessage::User {
                content: vec![LlmContent::Text(format!(
                    "[Context compacted. Summary of previous conversation:]\n{}",
                    summary
                ))],
            },
            AgentMessage::BranchSummary { summary, .. } => LlmMessage::User {
                content: vec![LlmContent::Text(format!("[Branch summary:]\n{}", summary))],
            },
        };

        // Merge consecutive same-role messages (Anthropic requires alternating roles)
        if let Some(last) = result.last_mut()
            && should_merge(last, &llm_msg)
        {
            merge_into(last, llm_msg);
            continue;
        }
        result.push(llm_msg);
    }

    result
}

fn content_item_to_llm(item: &ContentItem) -> LlmContent {
    match item {
        ContentItem::Text { text } => LlmContent::Text(text.clone()),
        ContentItem::Image { source } => LlmContent::Image(source.clone()),
    }
}

fn should_merge(existing: &LlmMessage, new: &LlmMessage) -> bool {
    matches!(
        (existing, new),
        (LlmMessage::User { .. }, LlmMessage::User { .. })
            | (LlmMessage::Assistant { .. }, LlmMessage::Assistant { .. })
    )
}

fn merge_into(existing: &mut LlmMessage, new: LlmMessage) {
    match (existing, new) {
        (
            LlmMessage::User {
                content: existing_content,
            },
            LlmMessage::User {
                content: new_content,
            },
        ) => {
            existing_content.extend(new_content);
        }
        (
            LlmMessage::Assistant {
                content: existing_content,
            },
            LlmMessage::Assistant {
                content: new_content,
            },
        ) => {
            existing_content.extend(new_content);
        }
        _ => {}
    }
}

/// Pre-LLM context transform:
/// 1. Strip orphaned tool calls (no matching ToolResult)
/// 2. Strip thinking blocks (never referenced by the model)
/// 3. Strip args from denied tool calls (is_error + "denied")
/// 4. Truncate stale tool results to save tokens
/// 5. Replace superseded read results (same file read again later)
///
/// # Cache stability
///
/// Operations 4 and 5 are position-dependent: they only apply to messages
/// before a "stale cutoff" index. If the cutoff is recomputed from
/// `messages.len()` on every API call within a tool loop, it advances each
/// iteration, mutating previously-sent messages and invalidating the prompt
/// cache prefix. This forces cache *writes* (~$3.75/M on Sonnet) instead of
/// cache *reads* (~$0.30/M) on every call — a 12x cost multiplier on input.
///
/// To avoid this, callers running an agentic tool loop should compute the
/// cutoff once before entering the loop and pass it via `stale_cutoff`.
/// New messages added during the loop are always beyond the frozen cutoff
/// and included in full, while older messages stay stable.
pub const RECENT_TURNS: usize = 10;
const RECENT_TURNS_MIN: usize = 6;
const RECENT_TURNS_MAX: usize = 16;
const TRUNCATED_MAX_CHARS: usize = 200;

/// Number of prior assistant messages before tool descriptions are pruned.
/// After this many responses, the model has internalized the tool interfaces.
const TOOL_PRUNE_THRESHOLD: usize = 4;

/// Returns true if there are enough prior assistant turns that tool descriptions
/// can be safely pruned to save tokens. The decision must be frozen once per
/// prompt loop for cache prefix stability.
pub fn should_prune_tool_descriptions(messages: &[AgentMessage]) -> bool {
    let assistant_count = messages
        .iter()
        .filter(|m| matches!(m, AgentMessage::Assistant(_)))
        .count();
    assistant_count >= TOOL_PRUNE_THRESHOLD
}

/// Compute how many recent messages to preserve verbatim, adapting based on
/// the diversity of files targeted by recent tool calls.
///
/// - Focused editing (1-2 distinct files): shrink to `RECENT_TURNS_MIN`
///   because the model only needs recent context for the file it's iterating on.
/// - Broad exploration (5+ distinct files): expand to `RECENT_TURNS_MAX`
///   because the model is building a mental model across many files.
/// - Otherwise: return `RECENT_TURNS` (the base value).
///
/// Only considers the last `RECENT_TURNS_MAX` messages to avoid looking too far back.
pub fn compute_adaptive_recent(messages: &[AgentMessage]) -> usize {
    if messages.len() < RECENT_TURNS * 2 {
        return RECENT_TURNS; // not enough history to adapt
    }

    // Look at recent tool calls within the analysis window
    let window_start = messages.len().saturating_sub(RECENT_TURNS_MAX * 2);
    let mut distinct_paths = std::collections::HashSet::new();

    for msg in &messages[window_start..] {
        if let AgentMessage::Assistant(a) = msg {
            for block in &a.content {
                if let ContentBlock::ToolCall { arguments, name, .. } = block {
                    match name.as_str() {
                        "read" | "edit" | "write" => {
                            if let Some(path) = arguments.get("path").and_then(|v| v.as_str()) {
                                distinct_paths.insert(path.to_string());
                            }
                        }
                        "grep" | "find" | "ls" => {
                            if let Some(path) = arguments.get("path").and_then(|v| v.as_str()) {
                                distinct_paths.insert(path.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let n = distinct_paths.len();
    if n <= 2 {
        RECENT_TURNS_MIN
    } else if n >= 5 {
        RECENT_TURNS_MAX
    } else {
        RECENT_TURNS
    }
}

pub fn transform_context(
    messages: Vec<AgentMessage>,
    _context_window: u32,
    stale_cutoff: Option<usize>,
) -> Vec<AgentMessage> {
    // Pass 0: find superseded tool calls — walk backwards, track latest call per key.
    // Earlier calls of the same file/path/pattern are replaced with a short marker.
    let superseded_ids = find_superseded_results(&messages);

    // Pass 0b: map tool_call_id → tool name for bash success detection
    let tool_names: std::collections::HashMap<String, String> = messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Assistant(a) => Some(a),
            _ => None,
        })
        .flat_map(|a| &a.content)
        .filter_map(|b| match b {
            ContentBlock::ToolCall { id, name, .. } => Some((id.clone(), name.clone())),
            _ => None,
        })
        .collect();

    // Pass 1: collect tool_call_ids that have a ToolResult
    let answered_ids: std::collections::HashSet<String> = messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::ToolResult { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();

    // Pass 1b: collect tool_call_ids that were denied
    let denied_ids: std::collections::HashSet<String> = messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::ToolResult {
                tool_call_id,
                content,
                is_error: true,
                ..
            } => {
                let text = content
                    .iter()
                    .filter_map(|c| match c {
                        ContentItem::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                if text.contains("denied") {
                    Some(tool_call_id.clone())
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();

    // Pass 1c: for stale read results, find lines referenced by later edits on the same file.
    // This lets us fold unreferenced ranges instead of blindly truncating.
    let read_referenced_lines = find_read_referenced_lines(&messages);

    // Pass 2: transform
    // When stale_cutoff is provided (frozen at the start of a prompt loop), use it
    // so that the stale/recent boundary doesn't shift between consecutive API calls
    // within the same tool loop — this keeps the message prefix stable for caching.
    let cutoff = stale_cutoff.unwrap_or_else(|| messages.len().saturating_sub(RECENT_TURNS));
    messages
        .into_iter()
        .enumerate()
        .filter_map(|(i, msg)| match msg {
            AgentMessage::Assistant(mut a) => {
                // Remove orphaned tool calls
                a.content.retain(|block| match block {
                    ContentBlock::ToolCall { id, .. } => answered_ids.contains(id),
                    _ => true,
                });

                // Strip thinking blocks — they're never referenced in context
                a.content
                    .retain(|block| !matches!(block, ContentBlock::Thinking { .. }));

                // Strip args from denied tool calls.
                // For stale turns, also strip bulky edit/write args (keep only path).
                a.content = a
                    .content
                    .into_iter()
                    .map(|block| match block {
                        ContentBlock::ToolCall { id, name, .. } if denied_ids.contains(&id) => {
                            ContentBlock::ToolCall {
                                id,
                                name,
                                arguments: serde_json::json!({}),
                            }
                        }
                        ContentBlock::ToolCall {
                            id,
                            ref name,
                            ref arguments,
                        } if i < cutoff && (name == "edit" || name == "write") => {
                            let path = arguments
                                .get("path")
                                .cloned()
                                .unwrap_or(serde_json::json!(""));
                            ContentBlock::ToolCall {
                                id,
                                name: name.clone(),
                                arguments: serde_json::json!({"path": path}),
                            }
                        }
                        other => other,
                    })
                    .collect();

                if a.content.is_empty() {
                    None
                } else {
                    Some(AgentMessage::Assistant(a))
                }
            }
            AgentMessage::ToolResult {
                tool_call_id,
                content: _,
                is_error,
                display: _,
                timestamp,
            } if superseded_ids.contains(&tool_call_id) => {
                Some(AgentMessage::ToolResult {
                    tool_call_id,
                    content: vec![ContentItem::Text {
                        text: "[superseded by later call]".into(),
                    }],
                    is_error,
                    display: None,
                    timestamp,
                })
            }
            AgentMessage::ToolResult {
                tool_call_id,
                content,
                is_error: false,
                display: _,
                timestamp,
            } if tool_names.get(&tool_call_id).map(|n| n.as_str()) == Some("bash") => {
                let text = content_text(&content);
                if let Some(compressed) = compress_bash_success(&text) {
                    Some(AgentMessage::ToolResult {
                        tool_call_id,
                        content: vec![ContentItem::Text { text: compressed }],
                        is_error: false,
                        display: None,
                        timestamp,
                    })
                } else if i < cutoff {
                    let summary = summarize_tool_content(&content);
                    Some(AgentMessage::ToolResult {
                        tool_call_id,
                        content: vec![ContentItem::Text { text: summary }],
                        is_error: false,
                        display: None,
                        timestamp,
                    })
                } else {
                    Some(AgentMessage::ToolResult {
                        tool_call_id,
                        content,
                        is_error: false,
                        display: None,
                        timestamp,
                    })
                }
            }
            AgentMessage::ToolResult {
                ref tool_call_id,
                ref content,
                is_error,
                timestamp,
                ..
            } if i < cutoff
                && tool_names.get(tool_call_id).map(|n| n.as_str()) == Some("read")
                && read_referenced_lines.contains_key(tool_call_id) =>
            {
                let text = content_text(content);
                let refs = &read_referenced_lines[tool_call_id];
                let folded = fold_read_result(&text, refs);
                Some(AgentMessage::ToolResult {
                    tool_call_id: tool_call_id.clone(),
                    content: vec![ContentItem::Text { text: folded }],
                    is_error,
                    display: None,
                    timestamp,
                })
            }
            AgentMessage::ToolResult {
                tool_call_id,
                content,
                is_error,
                display: _,
                timestamp,
            } if i < cutoff => {
                let summary = summarize_tool_content(&content);
                Some(AgentMessage::ToolResult {
                    tool_call_id,
                    content: vec![ContentItem::Text { text: summary }],
                    is_error,
                    display: None,
                    timestamp,
                })
            }
            other => Some(other),
        })
        .collect()
}

fn summarize_tool_content(content: &[ContentItem]) -> String {
    let full_text: String = content
        .iter()
        .filter_map(|item| match item {
            ContentItem::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    let line_count = full_text.lines().count();
    let char_count = full_text.len();

    if char_count <= TRUNCATED_MAX_CHARS {
        return full_text; // small enough to keep
    }

    // Take first few lines as preview
    let preview: String = full_text.lines().take(3).collect::<Vec<_>>().join("\n");
    let preview = if preview.len() > TRUNCATED_MAX_CHARS {
        format!("{}...", &preview[..TRUNCATED_MAX_CHARS])
    } else {
        preview
    };

    format!(
        "{}\n[truncated: {} lines, {} chars]",
        preview, line_count, char_count
    )
}

fn content_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            ContentItem::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// For each read tool call, find line numbers referenced by later edits on the same file.
/// Returns a map of tool_call_id → set of referenced line numbers (1-based).
fn find_read_referenced_lines(
    messages: &[AgentMessage],
) -> std::collections::HashMap<String, std::collections::HashSet<usize>> {
    // Collect read tool calls: (index, tool_call_id, path)
    let mut reads: Vec<(usize, String, String)> = Vec::new();
    for (i, msg) in messages.iter().enumerate() {
        if let AgentMessage::Assistant(a) = msg {
            for block in &a.content {
                if let ContentBlock::ToolCall {
                    id, name, arguments, ..
                } = block
                {
                    if name == "read" {
                        if let Some(path) = arguments.get("path").and_then(|v| v.as_str()) {
                            reads.push((i, id.clone(), path.to_string()));
                        }
                    }
                }
            }
        }
    }

    let mut result: std::collections::HashMap<String, std::collections::HashSet<usize>> =
        std::collections::HashMap::new();

    for (read_idx, read_id, read_path) in &reads {
        // Find the corresponding tool result to parse its lines
        let read_content = messages[*read_idx + 1..].iter().find_map(|m| match m {
            AgentMessage::ToolResult {
                tool_call_id,
                content,
                ..
            } if tool_call_id == read_id => Some(content_text(content)),
            _ => None,
        });
        let read_content = match read_content {
            Some(c) => c,
            None => continue,
        };

        // Parse read output: "  N\tcontent" → Vec<(line_num, content)>
        let parsed_lines: Vec<(usize, &str)> = read_content
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim_start();
                let tab_pos = trimmed.find('\t')?;
                let num: usize = trimmed[..tab_pos].parse().ok()?;
                let content = &trimmed[tab_pos + 1..];
                Some((num, content))
            })
            .collect();

        if parsed_lines.is_empty() {
            continue;
        }

        // Scan forward for edits on the same path
        for msg in &messages[*read_idx + 1..] {
            if let AgentMessage::Assistant(a) = msg {
                for block in &a.content {
                    if let ContentBlock::ToolCall {
                        name, arguments, ..
                    } = block
                    {
                        if name == "edit" {
                            let edit_path =
                                arguments.get("path").and_then(|v| v.as_str()).unwrap_or("");
                            if edit_path != read_path {
                                continue;
                            }
                            if let Some(old_str) =
                                arguments.get("old_string").and_then(|v| v.as_str())
                            {
                                // Find which lines in the read match this old_string
                                let old_lines: Vec<&str> = old_str.lines().collect();
                                if old_lines.is_empty() {
                                    continue;
                                }
                                // Sliding window match over parsed_lines
                                for window_start in
                                    0..parsed_lines.len().saturating_sub(old_lines.len() - 1)
                                {
                                    let matches = old_lines.iter().enumerate().all(|(j, ol)| {
                                        window_start + j < parsed_lines.len()
                                            && parsed_lines[window_start + j].1 == *ol
                                    });
                                    if matches {
                                        let refs = result.entry(read_id.clone()).or_default();
                                        for j in 0..old_lines.len() {
                                            refs.insert(parsed_lines[window_start + j].0);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    result
}

/// Fold a read result, keeping referenced lines + context and collapsing gaps.
const FOLD_CONTEXT: usize = 2;

fn fold_read_result(
    read_text: &str,
    referenced_lines: &std::collections::HashSet<usize>,
) -> String {
    // Parse into (line_num, full_original_line) pairs
    let lines: Vec<(usize, &str)> = read_text
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let tab_pos = trimmed.find('\t')?;
            let num: usize = trimmed[..tab_pos].parse().ok()?;
            Some((num, line))
        })
        .collect();

    if lines.is_empty() {
        return read_text.to_string();
    }

    // Build set of lines to keep: referenced + context
    let mut keep: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for &line_num in referenced_lines {
        for offset in 0..=(FOLD_CONTEXT * 2) {
            let n = (line_num + offset).saturating_sub(FOLD_CONTEXT);
            if n > 0 {
                keep.insert(n);
            }
        }
    }

    let mut output = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let (num, original) = lines[i];
        if keep.contains(&num) {
            output.push(original.to_string());
            i += 1;
        } else {
            // Find the end of this folded range
            let fold_start = num;
            while i < lines.len() && !keep.contains(&lines[i].0) {
                i += 1;
            }
            let fold_end = if i > 0 { lines[i - 1].0 } else { fold_start };
            if fold_start == fold_end {
                output.push(format!("[line {} omitted]", fold_start));
            } else {
                output.push(format!("[lines {}-{} omitted]", fold_start, fold_end));
            }
        }
    }

    output.join("\n")
}

/// Detect bash success patterns and return a one-line summary.
/// Returns None if the output doesn't match a known success pattern.
fn compress_bash_success(text: &str) -> Option<String> {
    // cargo check / cargo build with no errors
    if (text.contains("Compiling") || text.contains("Checking") || text.contains("Finished"))
        && !text.contains("error[")
        && !text.contains("error:")
    {
        if let Some(line) = text.lines().rfind(|l| l.contains("Finished")) {
            return Some(line.trim().to_string());
        }
    }

    // cargo test with all passing
    if let Some(line) = text.lines().rfind(|l| l.starts_with("test result:")) {
        if line.contains("0 failed") {
            return Some(line.trim().to_string());
        }
    }

    // Python unittest: "Ran N tests... OK"
    if text.contains("Ran ") && text.lines().any(|l| l.trim() == "OK") {
        if let Some(line) = text.lines().rfind(|l| l.starts_with("Ran ")) {
            return Some(line.trim().to_string());
        }
    }

    // pytest: "N passed" with no failures
    if let Some(line) = text.lines().rfind(|l| l.contains(" passed")) {
        if !line.contains("failed") && !line.contains("error") {
            return Some(line.trim().to_string());
        }
    }

    // make: no error, last line is a target or "Nothing to be done"
    if text.contains("make") && !text.contains("Error ") && !text.contains("error:") {
        if let Some(line) = text.lines().rfind(|l| l.contains("Nothing to be done")) {
            return Some(line.trim().to_string());
        }
    }

    None
}

/// Walk backwards through messages. For each "read" tool call, track the path.
/// If an earlier read targeted the same path, mark its tool_call_id as superseded.
/// Only supersede successful reads (not errors) — the model may need error context.
/// Find tool call IDs whose results can be replaced with a short marker because
/// a later call supersedes them:
/// - **read**: later read of the same path supersedes earlier reads
/// - **edit/write**: later edit of the same path supersedes earlier edit results
/// - **grep**: later grep with the same pattern and a path that is a child of
///   (or equal to) the earlier grep's path supersedes the earlier result
/// - **ls/find**: later ls/find whose path is a child of the earlier one supersedes it
fn find_superseded_results(messages: &[AgentMessage]) -> std::collections::HashSet<String> {
    use std::collections::{HashMap, HashSet};

    let mut superseded = HashSet::new();

    // read: path → latest tool_call_id
    let mut latest_read: HashMap<String, String> = HashMap::new();
    // edit/write: path → latest tool_call_id
    let mut latest_edit: HashMap<String, String> = HashMap::new();
    // grep: (pattern, path) → latest tool_call_id + path
    let mut latest_grep: Vec<(String, String, String)> = Vec::new(); // (pattern, path, id)
    // ls/find: path → latest tool_call_id
    let mut latest_ls: Vec<(String, String)> = Vec::new(); // (path, id)

    // Walk backwards: the first call we encounter for a key is the latest
    for msg in messages.iter().rev() {
        if let AgentMessage::Assistant(a) = msg {
            for block in &a.content {
                if let ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } = block
                {
                    match name.as_str() {
                        "read" => {
                            if let Some(path) = arguments.get("path").and_then(|v| v.as_str()) {
                                if latest_read.contains_key(path) {
                                    superseded.insert(id.clone());
                                } else {
                                    latest_read.insert(path.to_string(), id.clone());
                                }
                            }
                        }
                        "edit" | "write" => {
                            if let Some(path) = arguments.get("path").and_then(|v| v.as_str()) {
                                if latest_edit.contains_key(path) {
                                    superseded.insert(id.clone());
                                } else {
                                    latest_edit.insert(path.to_string(), id.clone());
                                }
                            }
                        }
                        "grep" => {
                            let pattern = arguments
                                .get("pattern")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let path = arguments
                                .get("path")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            // A later grep supersedes this one if same pattern and
                            // the later grep's path is equal to or a child of this path.
                            let is_superseded = latest_grep.iter().any(|(p, gp, _)| {
                                p == pattern && (gp == path || gp.starts_with(&format!("{}/", path.trim_end_matches('/'))))
                            });
                            if is_superseded {
                                superseded.insert(id.clone());
                            } else {
                                latest_grep.push((pattern.to_string(), path.to_string(), id.clone()));
                            }
                        }
                        "ls" | "find" => {
                            let path = arguments
                                .get("path")
                                .and_then(|v| v.as_str())
                                .unwrap_or(".");
                            // A later ls of a child path supersedes this one.
                            let is_superseded = latest_ls.iter().any(|(lp, _)| {
                                lp == path || lp.starts_with(&format!("{}/", path.trim_end_matches('/')))
                            });
                            if is_superseded {
                                superseded.insert(id.clone());
                            } else {
                                latest_ls.push((path.to_string(), id.clone()));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Don't supersede calls whose results were errors
    for msg in messages {
        if let AgentMessage::ToolResult {
            tool_call_id,
            is_error: true,
            ..
        } = msg
        {
            superseded.remove(tool_call_id);
        }
    }

    superseded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_converts() {
        let msgs = vec![AgentMessage::User {
            content: vec![ContentItem::Text {
                text: "hello".into(),
            }],
            timestamp: 0,
        }];
        let llm = convert_to_llm(&msgs);
        assert_eq!(llm.len(), 1);
        assert!(llm[0].is_user());
    }

    #[test]
    fn consecutive_user_messages_merge() {
        let msgs = vec![
            AgentMessage::User {
                content: vec![ContentItem::Text { text: "a".into() }],
                timestamp: 0,
            },
            AgentMessage::Custom {
                custom_type: "note".into(),
                content: vec![ContentItem::Text { text: "b".into() }],
                display: false,
                timestamp: 1,
            },
        ];
        let llm = convert_to_llm(&msgs);
        // Both become User, so they merge
        assert_eq!(llm.len(), 1);
    }

    #[test]
    fn tool_result_not_merged() {
        let msgs = vec![
            AgentMessage::User {
                content: vec![ContentItem::Text { text: "a".into() }],
                timestamp: 0,
            },
            AgentMessage::ToolResult {
                tool_call_id: "t1".into(),
                content: vec![ContentItem::Text {
                    text: "result".into(),
                }],
                is_error: false,
                display: None,
                timestamp: 1,
            },
        ];
        let llm = convert_to_llm(&msgs);
        assert_eq!(llm.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Helpers for building realistic conversations
    // -----------------------------------------------------------------------

    fn user(text: &str) -> AgentMessage {
        AgentMessage::User {
            content: vec![ContentItem::Text { text: text.into() }],
            timestamp: 0,
        }
    }

    fn assistant_text(text: &str) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text { text: text.into() }],
            stop_reason: StopReason::EndTurn,
            usage: None,
            timestamp: 0,
        })
    }

    fn assistant_thinking_text(thinking: &str, text: &str) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::Thinking {
                    thinking: thinking.into(),
                },
                ContentBlock::Text { text: text.into() },
            ],
            stop_reason: StopReason::EndTurn,
            usage: None,
            timestamp: 0,
        })
    }

    fn assistant_tool_call(id: &str, name: &str, args: serde_json::Value) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: args,
            }],
            stop_reason: StopReason::ToolUse,
            usage: None,
            timestamp: 0,
        })
    }

    fn assistant_thinking_tool_call(
        thinking: &str,
        id: &str,
        name: &str,
        args: serde_json::Value,
    ) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::Thinking {
                    thinking: thinking.into(),
                },
                ContentBlock::ToolCall {
                    id: id.into(),
                    name: name.into(),
                    arguments: args,
                },
            ],
            stop_reason: StopReason::ToolUse,
            usage: None,
            timestamp: 0,
        })
    }

    fn tool_result(id: &str, content: &str) -> AgentMessage {
        AgentMessage::ToolResult {
            tool_call_id: id.into(),
            content: vec![ContentItem::Text {
                text: content.into(),
            }],
            is_error: false,
            display: None,
            timestamp: 0,
        }
    }

    fn tool_error(id: &str, content: &str) -> AgentMessage {
        AgentMessage::ToolResult {
            tool_call_id: id.into(),
            content: vec![ContentItem::Text {
                text: content.into(),
            }],
            is_error: true,
            display: None,
            timestamp: 0,
        }
    }

    fn tool_denied(id: &str) -> AgentMessage {
        tool_error(id, "Tool call denied by user.")
    }

    fn get_assistant(msg: &AgentMessage) -> &AssistantMessage {
        match msg {
            AgentMessage::Assistant(a) => a,
            _ => panic!("expected assistant message"),
        }
    }

    fn count_thinking(msgs: &[AgentMessage]) -> usize {
        msgs.iter()
            .filter_map(|m| match m {
                AgentMessage::Assistant(a) => Some(a),
                _ => None,
            })
            .flat_map(|a| &a.content)
            .filter(|b| matches!(b, ContentBlock::Thinking { .. }))
            .count()
    }

    fn total_text_len(msgs: &[AgentMessage]) -> usize {
        msgs.iter()
            .map(|m| match m {
                AgentMessage::User { content, .. } | AgentMessage::ToolResult { content, .. } => {
                    content
                        .iter()
                        .map(|c| match c {
                            ContentItem::Text { text } => text.len(),
                            _ => 0,
                        })
                        .sum()
                }
                AgentMessage::Assistant(a) => a
                    .content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { text } => text.len(),
                        ContentBlock::Thinking { thinking } => thinking.len(),
                        ContentBlock::ToolCall { arguments, .. } => arguments.to_string().len(),
                    })
                    .sum(),
                _ => 0,
            })
            .sum()
    }

    // -----------------------------------------------------------------------
    // Context optimization tests
    // -----------------------------------------------------------------------

    #[test]
    fn thinking_blocks_stripped() {
        let msgs = vec![AgentMessage::Assistant(AssistantMessage {
            content: vec![
                ContentBlock::Thinking {
                    thinking: "Let me think about this very carefully...".into(),
                },
                ContentBlock::Text {
                    text: "The answer is 42.".into(),
                },
            ],
            stop_reason: StopReason::EndTurn,
            usage: None,
            timestamp: 0,
        })];
        let result = transform_context(msgs, 200_000, None);
        let a = match &result[0] {
            AgentMessage::Assistant(a) => a,
            _ => panic!("expected assistant"),
        };
        assert_eq!(a.content.len(), 1);
        assert!(matches!(&a.content[0], ContentBlock::Text { .. }));
    }

    #[test]
    fn denied_tool_args_stripped() {
        let msgs = vec![
            AgentMessage::Assistant(AssistantMessage {
                content: vec![ContentBlock::ToolCall {
                    id: "t1".into(),
                    name: "write".into(),
                    arguments: serde_json::json!({"path": "/etc/evil", "content": "x".repeat(5000)}),
                }],
                stop_reason: StopReason::ToolUse,
                usage: None,
                timestamp: 0,
            }),
            AgentMessage::ToolResult {
                tool_call_id: "t1".into(),
                content: vec![ContentItem::Text {
                    text: "Tool call denied by user.".into(),
                }],
                is_error: true,
                display: None,
                timestamp: 1,
            },
        ];
        let result = transform_context(msgs, 200_000, None);
        let a = match &result[0] {
            AgentMessage::Assistant(a) => a,
            _ => panic!("expected assistant"),
        };
        if let ContentBlock::ToolCall { arguments, .. } = &a.content[0] {
            assert_eq!(
                *arguments,
                serde_json::json!({}),
                "denied tool args should be stripped"
            );
        } else {
            panic!("expected tool call");
        }
    }

    #[test]
    fn non_denied_tool_args_preserved() {
        let msgs = vec![
            AgentMessage::Assistant(AssistantMessage {
                content: vec![ContentBlock::ToolCall {
                    id: "t1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "src/main.rs"}),
                }],
                stop_reason: StopReason::ToolUse,
                usage: None,
                timestamp: 0,
            }),
            AgentMessage::ToolResult {
                tool_call_id: "t1".into(),
                content: vec![ContentItem::Text {
                    text: "file contents...".into(),
                }],
                is_error: false,
                display: None,
                timestamp: 1,
            },
        ];
        let result = transform_context(msgs, 200_000, None);
        let a = match &result[0] {
            AgentMessage::Assistant(a) => a,
            _ => panic!("expected assistant"),
        };
        if let ContentBlock::ToolCall { arguments, .. } = &a.content[0] {
            assert_eq!(
                arguments["path"], "src/main.rs",
                "successful tool args should be preserved"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Realistic conversation scenarios
    // -----------------------------------------------------------------------

    /// Simulate: user asks to fix a bug. Model thinks hard, reads a file,
    /// edits it, runs tests. 15 turns with large thinking + large diffs.
    #[test]
    fn realistic_bug_fix_session() {
        let big_thinking = "a]".repeat(5000); // ~10k chars of thinking
        let big_file = (0..200)
            .map(|i| format!("    {} fn line_{}() {{}}", i + 1, i))
            .collect::<Vec<_>>()
            .join("\n");
        let mut msgs = vec![
            user("Fix the off-by-one error in parser.rs"),
            // Model thinks, then reads file
            assistant_thinking_tool_call(
                &big_thinking,
                "t1",
                "read",
                serde_json::json!({"path": "src/parser.rs"}),
            ),
            tool_result("t1", &big_file),
            // Model thinks again, edits
            assistant_thinking_tool_call(
                &big_thinking,
                "t2",
                "edit",
                serde_json::json!({
                    "path": "src/parser.rs",
                    "old_text": "old_code();",
                    "new_text": "new_code();"
                }),
            ),
            tool_result("t2", "Applied edit to src/parser.rs"),
            // Model runs tests
            assistant_thinking_tool_call(
                &big_thinking,
                "t3",
                "bash",
                serde_json::json!({"command": "cargo test"}),
            ),
            tool_result(
                "t3",
                &format!("test result: ok. 50 passed\n{}", "output ".repeat(500)),
            ),
            // Final response
            assistant_thinking_text(&big_thinking, "Fixed the off-by-one error."),
        ];

        // Add padding to push early messages past the truncation cutoff
        for i in 0..8 {
            msgs.push(user(&format!("follow-up question {}", i)));
            msgs.push(assistant_text(&format!("answer {}", i)));
        }

        let before_len = total_text_len(&msgs);
        let result = transform_context(msgs, 200_000, None);
        let after_len = total_text_len(&result);

        // All thinking should be gone
        assert_eq!(
            count_thinking(&result),
            0,
            "all thinking blocks should be stripped"
        );
        // Should save significant tokens (4 thinking blocks × ~10k each)
        assert!(
            after_len < before_len / 2,
            "should save >50% tokens: before={} after={}",
            before_len,
            after_len
        );
        // Early tool results should be truncated
        let early_result = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content
                .iter()
                .filter_map(|c| match c {
                    ContentItem::Text { text } => Some(text.len()),
                    _ => None,
                })
                .sum::<usize>(),
            _ => panic!("expected tool result at index 2"),
        };
        assert!(
            early_result < 300,
            "early tool result should be truncated: {}",
            early_result
        );
    }

    /// Simulate: model tries to write outside repo, denied. Then tries
    /// again inside repo, succeeds. The denied write had a 5KB payload.
    #[test]
    fn denied_write_then_retry() {
        let big_content = "x".repeat(5000);
        let msgs = vec![
            user("Create a config file"),
            // First attempt — outside repo, denied
            assistant_tool_call(
                "t1",
                "write",
                serde_json::json!({"path": "/etc/myapp.conf", "content": big_content}),
            ),
            tool_denied("t1"),
            // Model apologizes and retries inside repo
            assistant_tool_call(
                "t2",
                "write",
                serde_json::json!({"path": "config/myapp.conf", "content": big_content}),
            ),
            tool_result("t2", "Created config/myapp.conf"),
            assistant_text("Created the config file at config/myapp.conf instead."),
        ];

        let result = transform_context(msgs, 200_000, None);

        // Denied tool call args should be stripped
        let denied_assistant = get_assistant(&result[1]);
        if let ContentBlock::ToolCall {
            arguments, name, ..
        } = &denied_assistant.content[0]
        {
            assert_eq!(name, "write");
            assert_eq!(
                *arguments,
                serde_json::json!({}),
                "denied write args should be empty"
            );
        } else {
            panic!("expected tool call");
        }

        // Successful tool call args should be preserved
        let ok_assistant = get_assistant(&result[3]);
        if let ContentBlock::ToolCall { arguments, .. } = &ok_assistant.content[0] {
            assert!(arguments["content"].as_str().unwrap().len() == 5000);
        } else {
            panic!("expected tool call");
        }
    }

    /// Simulate: model reads same file 3 times, edits fail twice, succeeds
    /// on third. Old results should be truncated.
    #[test]
    fn repeated_read_edit_failures() {
        let file_content = "line\n".repeat(500); // ~2500 chars
        let mut msgs = Vec::new();

        msgs.push(user("Refactor the database module"));

        // Three read-edit cycles
        for i in 0..3 {
            let read_id = format!("r{}", i);
            let edit_id = format!("e{}", i);
            msgs.push(assistant_tool_call(
                &read_id,
                "read",
                serde_json::json!({"path": "src/db.rs"}),
            ));
            msgs.push(tool_result(&read_id, &file_content));

            msgs.push(assistant_tool_call(
                &edit_id,
                "edit",
                serde_json::json!({
                    "path": "src/db.rs",
                    "old_text": format!("old_v{}", i),
                    "new_text": format!("new_v{}", i)
                }),
            ));
            if i < 2 {
                msgs.push(tool_error(&edit_id, "No match found for old_text"));
            } else {
                msgs.push(tool_result(&edit_id, "Applied edit to src/db.rs"));
            }
        }

        // Pad to push early turns past cutoff
        for i in 0..8 {
            msgs.push(user(&format!("q{}", i)));
            msgs.push(assistant_text(&format!("a{}", i)));
        }

        msgs.push(assistant_text("Done refactoring."));

        let result = transform_context(msgs, 200_000, None);

        // Early read results (first two) should be truncated
        let first_read_result = &result[2]; // tool_result for r0
        if let AgentMessage::ToolResult { content, .. } = first_read_result {
            let text: String = content
                .iter()
                .filter_map(|c| match c {
                    ContentItem::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            assert!(
                text.len() < 300,
                "first read result should be truncated: {} chars",
                text.len()
            );
        }
    }

    /// Simulate: model produces massive thinking blocks across multiple turns.
    /// Verify all are stripped and we preserve message structure.
    #[test]
    fn massive_thinking_stripped_preserves_structure() {
        let huge_thinking = "reasoning step\n".repeat(2000); // ~30k chars
        let msgs = vec![
            user("Explain quantum computing"),
            assistant_thinking_text(&huge_thinking, "Here's a brief explanation."),
            user("Now explain it in more detail"),
            assistant_thinking_text(&huge_thinking, "In more detail: ..."),
            user("Compare with classical computing"),
            assistant_thinking_text(&huge_thinking, "The key differences are: ..."),
        ];

        let before_thinking = count_thinking(&msgs);
        assert_eq!(before_thinking, 3);
        let before_len = total_text_len(&msgs);

        let result = transform_context(msgs, 200_000, None);

        assert_eq!(count_thinking(&result), 0);
        assert_eq!(result.len(), 6, "message count should be preserved");
        let after_len = total_text_len(&result);
        // 3 × 30k thinking = ~90k removed
        assert!(
            before_len - after_len > 80_000,
            "should strip >80k chars of thinking: saved {}",
            before_len - after_len
        );
    }

    /// Simulate: orphaned tool calls (model called tool but was aborted
    /// before result came back).
    #[test]
    fn orphaned_tool_calls_from_abort() {
        let msgs = vec![
            user("Read all the files"),
            // Model starts two tool calls but gets aborted
            AgentMessage::Assistant(AssistantMessage {
                content: vec![
                    ContentBlock::ToolCall {
                        id: "t1".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({"path": "file1.rs"}),
                    },
                    ContentBlock::ToolCall {
                        id: "t2".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({"path": "file2.rs"}),
                    },
                ],
                stop_reason: StopReason::Aborted,
                usage: None,
                timestamp: 0,
            }),
            // Only t1 got a result before abort
            tool_result("t1", "contents of file1"),
            // User continues
            user("Just read file1 please"),
            assistant_text("Based on file1: ..."),
        ];

        let result = transform_context(msgs, 200_000, None);

        // The assistant message should only have t1 (t2 is orphaned)
        let a = get_assistant(&result[1]);
        assert_eq!(a.content.len(), 1, "orphaned t2 should be removed");
        if let ContentBlock::ToolCall { id, .. } = &a.content[0] {
            assert_eq!(id, "t1");
        }
    }

    /// Simulate: mixed conversation with thinking, tool calls, denials,
    /// errors, and successful operations. The kitchen sink test.
    #[test]
    fn kitchen_sink_conversation() {
        let thinking = "detailed analysis\n".repeat(500);
        let big_write = "x".repeat(3000);

        let msgs = vec![
            // Turn 1: user asks, model thinks and reads
            user("Set up a new REST API endpoint"),
            assistant_thinking_tool_call(
                &thinking,
                "t1",
                "read",
                serde_json::json!({"path": "src/routes.rs"}),
            ),
            tool_result("t1", &"fn route() {}\n".repeat(100)),
            // Turn 2: model tries to write outside repo — denied
            assistant_thinking_tool_call(
                &thinking,
                "t2",
                "write",
                serde_json::json!({"path": "/var/log/api.log", "content": big_write}),
            ),
            tool_denied("t2"),
            // Turn 3: model edits correctly
            assistant_thinking_tool_call(
                &thinking,
                "t3",
                "edit",
                serde_json::json!({
                    "path": "src/routes.rs",
                    "old_text": "fn route() {}",
                    "new_text": "fn new_endpoint() { /* ... */ }"
                }),
            ),
            tool_result("t3", "Applied edit"),
            // Turn 4: model runs tests — they fail
            assistant_tool_call("t4", "bash", serde_json::json!({"command": "cargo test"})),
            tool_error("t4", &format!("FAILED\n{}", "error output\n".repeat(200))),
            // Turn 5: model fixes and retries
            assistant_thinking_tool_call(
                &thinking,
                "t5",
                "edit",
                serde_json::json!({
                    "path": "src/routes.rs",
                    "old_text": "fn new_endpoint() { /* ... */ }",
                    "new_text": "fn new_endpoint() -> Result<()> { Ok(()) }"
                }),
            ),
            tool_result("t5", "Applied edit"),
            assistant_tool_call("t6", "bash", serde_json::json!({"command": "cargo test"})),
            tool_result("t6", "test result: ok. 50 passed"),
            // Final response
            assistant_thinking_text(&thinking, "The endpoint is set up and tests pass."),
        ];

        let msg_count = msgs.len();
        let before_len = total_text_len(&msgs);
        let result = transform_context(msgs, 200_000, None);

        // Structure preserved
        assert_eq!(result.len(), msg_count, "all messages should be present");

        // No thinking blocks
        assert_eq!(count_thinking(&result), 0);

        // Denied write args stripped
        let denied = get_assistant(&result[3]);
        if let ContentBlock::ToolCall { arguments, .. } = &denied.content[0] {
            assert_eq!(*arguments, serde_json::json!({}));
        }

        // Significant token savings
        let after_len = total_text_len(&result);
        let saved = before_len - after_len;
        assert!(
            saved > 10_000,
            "should save significant tokens: before={} after={} saved={}",
            before_len,
            after_len,
            saved
        );
    }

    /// Verify stale tool results are truncated but recent ones aren't.
    #[test]
    fn stale_vs_recent_tool_results() {
        let big_output = "x".repeat(1000);
        let mut msgs = Vec::new();

        // 15 turns of read operations
        for i in 0..15 {
            let id = format!("t{}", i);
            msgs.push(user(&format!("read file {}", i)));
            msgs.push(assistant_tool_call(
                &id,
                "read",
                serde_json::json!({"path": format!("src/mod_{}.rs", i)}),
            ));
            msgs.push(tool_result(&id, &big_output));
            msgs.push(assistant_text(&format!("Here's what's in mod_{}.rs", i)));
        }

        let result = transform_context(msgs, 200_000, None);

        // Early results (within first cutoff) should be truncated
        // Cutoff = len - RECENT_TURNS (10), so turns 0-9 are before cutoff
        let early_result = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content
                .iter()
                .filter_map(|c| match c {
                    ContentItem::Text { text } => Some(text.len()),
                    _ => None,
                })
                .sum::<usize>(),
            _ => panic!("expected tool result"),
        };
        assert!(
            early_result < 300,
            "early result should be truncated: {} chars",
            early_result
        );

        // Recent results (last few) should be preserved in full
        let last_result_idx = result.len() - 2; // second to last = last tool_result
        let recent_result = match &result[last_result_idx] {
            AgentMessage::ToolResult { content, .. } => content
                .iter()
                .filter_map(|c| match c {
                    ContentItem::Text { text } => Some(text.len()),
                    _ => None,
                })
                .sum::<usize>(),
            _ => panic!("expected tool result at {}", last_result_idx),
        };
        assert_eq!(
            recent_result, 1000,
            "recent result should be preserved in full"
        );
    }

    #[test]
    fn superseded_reads_replaced() {
        let msgs = vec![
            user("refactor the module"),
            // First read of main.rs
            assistant_tool_call("r1", "read", serde_json::json!({"path": "src/main.rs"})),
            tool_result("r1", &"fn main() {}\n".repeat(100)),
            // Edit
            assistant_tool_call("e1", "edit", serde_json::json!({"path": "src/main.rs", "old_text": "a", "new_text": "b"})),
            tool_result("e1", "Applied edit"),
            // Second read of same file (supersedes first)
            assistant_tool_call("r2", "read", serde_json::json!({"path": "src/main.rs"})),
            tool_result("r2", &"fn main() { updated }\n".repeat(100)),
            assistant_text("Done."),
        ];

        let result = transform_context(msgs, 200_000, None);

        // First read result should be superseded
        let first_read = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };
        assert!(
            first_read.contains("superseded"),
            "first read should be superseded: {}",
            first_read
        );

        // Second read should be preserved
        let second_read = match &result[6] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };
        assert!(
            second_read.contains("updated"),
            "second read should be preserved"
        );
    }

    #[test]
    fn bash_success_compressed() {
        let cargo_check_output = "\
   Compiling nerv v0.1.0 (/tmp/nerv)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.2s";

        let cargo_test_output = "\
running 25 tests
.........................
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s";

        assert_eq!(
            compress_bash_success(cargo_check_output),
            Some("Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.2s".into())
        );
        assert_eq!(
            compress_bash_success(cargo_test_output),
            Some("test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s".into())
        );

        // Error output should NOT be compressed
        let error_output = "error[E0308]: mismatched types";
        assert_eq!(compress_bash_success(error_output), None);
    }

    #[test]
    fn stale_edit_args_stripped() {
        let big_content = "x".repeat(3000);
        let mut msgs = vec![
            user("refactor"),
            assistant_tool_call(
                "e1",
                "edit",
                serde_json::json!({
                    "path": "src/lib.rs",
                    "old_text": big_content,
                    "new_text": "replaced"
                }),
            ),
            tool_result("e1", "Edited src/lib.rs"),
        ];

        // Pad to push the edit past cutoff
        for i in 0..12 {
            msgs.push(user(&format!("q{}", i)));
            msgs.push(assistant_text(&format!("a{}", i)));
        }

        let result = transform_context(msgs, 200_000, None);
        let edit_msg = get_assistant(&result[1]);
        if let ContentBlock::ToolCall { arguments, .. } = &edit_msg.content[0] {
            // Should only have path, not old_text/new_text
            assert!(
                arguments.get("path").is_some(),
                "path should be preserved"
            );
            assert!(
                arguments.get("old_text").is_none(),
                "old_text should be stripped from stale edit"
            );
            assert!(
                arguments.get("new_text").is_none(),
                "new_text should be stripped from stale edit"
            );
        } else {
            panic!("expected tool call");
        }
    }

    #[test]
    fn bash_success_python_unittest() {
        let output = "\
.....................
----------------------------------------------------------------------
Ran 21 tests in 0.003s

OK";
        assert_eq!(
            compress_bash_success(output),
            Some("Ran 21 tests in 0.003s".into())
        );
    }

    #[test]
    fn bash_success_pytest() {
        let output = "\
============================= test session starts ==============================
collected 15 items

test_foo.py ...............                                              [100%]

============================== 15 passed in 0.42s ==============================";
        assert_eq!(
            compress_bash_success(output),
            Some("============================== 15 passed in 0.42s ==============================".into())
        );
    }

    #[test]
    fn bash_failure_not_compressed() {
        // Python unittest failure
        let fail = "\
..F..
FAILED (failures=1)";
        assert_eq!(compress_bash_success(fail), None);

        // cargo test failure
        let cargo_fail = "test result: FAILED. 24 passed; 1 failed; 0 ignored";
        assert_eq!(compress_bash_success(cargo_fail), None);

        // cargo check error
        let check_fail = "Compiling nerv v0.1.0\nerror[E0308]: mismatched types";
        assert_eq!(compress_bash_success(check_fail), None);
    }

    #[test]
    fn recent_edit_args_preserved() {
        // Edit in recent turns should keep full args
        let big_content = "x".repeat(3000);
        let msgs = vec![
            user("fix it"),
            assistant_tool_call(
                "e1",
                "edit",
                serde_json::json!({
                    "path": "src/lib.rs",
                    "old_text": big_content,
                    "new_text": "replaced"
                }),
            ),
            tool_result("e1", "Edited src/lib.rs"),
            assistant_text("Done."),
        ];

        let result = transform_context(msgs, 200_000, None);
        let edit_msg = get_assistant(&result[1]);
        if let ContentBlock::ToolCall { arguments, .. } = &edit_msg.content[0] {
            assert!(
                arguments.get("old_text").is_some(),
                "recent edit should preserve old_text"
            );
        } else {
            panic!("expected tool call");
        }
    }

    // -----------------------------------------------------------------------
    // Cache prefix stability tests
    //
    // These test the core invariant that makes prompt caching work: within a
    // single tool loop, consecutive calls to transform_context must produce
    // identical output for all messages that existed in the previous call.
    // If a message that was already sent to the API changes on the next call,
    // the Anthropic prompt cache is invalidated and the full context gets
    // cache-written at the premium rate instead of cache-read.
    //
    // Design context: pi-mono's coding agent avoids this problem entirely by
    // using a pure, position-independent convertToLlm — the same message
    // always serializes identically regardless of conversation length.
    // nerv's transform_context applies position-dependent optimizations
    // (stripping stale edit args, summarizing old tool results) which are
    // valuable for context reduction but must not shift between consecutive
    // API calls within a single tool loop.
    // -----------------------------------------------------------------------

    /// Serialize transform_context output to a stable string for prefix comparison.
    fn serialize_messages(msgs: &[AgentMessage]) -> Vec<String> {
        msgs.iter()
            .map(|m| serde_json::to_string(m).unwrap())
            .collect()
    }

    /// Build a conversation where every tool call is an edit with large args,
    /// so that the stale/recent transition visibly strips content.
    fn build_edit_heavy_conversation(rounds: usize) -> Vec<AgentMessage> {
        let mut msgs = vec![user("implement /btw command")];
        for i in 0..rounds {
            let id = format!("t{}", i);
            msgs.push(assistant_tool_call(
                &id,
                "edit",
                serde_json::json!({
                    "path": format!("src/file_{}.rs", i),
                    "old_text": format!("original content line {}\n", i).repeat(50),
                    "new_text": format!("replaced content line {}\n", i).repeat(50),
                }),
            ));
            msgs.push(tool_result(&id, &format!("Edited src/file_{}.rs", i)));
        }
        msgs
    }

    /// Assert that every message in `prev` appears identically at the same
    /// position in `curr`. This is the cache prefix stability invariant.
    fn assert_prefix_stable(prev: &[String], curr: &[String], label: &str) {
        assert!(
            curr.len() >= prev.len(),
            "{}: output shrank ({} → {})",
            label,
            prev.len(),
            curr.len(),
        );
        for (i, (a, b)) in prev.iter().zip(curr.iter()).enumerate() {
            assert_eq!(
                a, b,
                "{}: message {} changed between calls",
                label, i,
            );
        }
    }

    #[test]
    fn frozen_cutoff_keeps_prefix_stable_across_tool_loop() {
        // Simulate a tool loop: start with RECENT_TURNS+5 rounds already in
        // history (so some are "stale"), then add 6 more rounds one at a time.
        // With a frozen cutoff, the prefix must be identical between calls.
        let base_rounds = RECENT_TURNS + 5;
        let mut msgs = build_edit_heavy_conversation(base_rounds);
        let frozen_cutoff = msgs.len().saturating_sub(RECENT_TURNS);

        let mut prev = serialize_messages(
            &transform_context(msgs.clone(), 200_000, Some(frozen_cutoff)),
        );

        for round in 0..6 {
            let id = format!("new_{}", round);
            msgs.push(assistant_tool_call(
                &id,
                "edit",
                serde_json::json!({
                    "path": format!("src/new_{}.rs", round),
                    "old_text": "x".repeat(2000),
                    "new_text": "y".repeat(2000),
                }),
            ));
            msgs.push(tool_result(&id, &format!("Edited src/new_{}.rs", round)));

            let curr = serialize_messages(
                &transform_context(msgs.clone(), 200_000, Some(frozen_cutoff)),
            );
            assert_prefix_stable(&prev, &curr, &format!("round {}", round));
            prev = curr;
        }
    }

    #[test]
    fn unfrozen_cutoff_shifts_and_mutates_prefix() {
        // Prove the problem: without a frozen cutoff, adding messages causes
        // the cutoff to advance, mutating previously-sent messages.
        // This test documents the OLD (broken) behavior.
        let total_rounds = RECENT_TURNS + 3;
        let mut msgs = build_edit_heavy_conversation(total_rounds);

        let prev = serialize_messages(
            &transform_context(msgs.clone(), 200_000, None),
        );

        // Add two more rounds so the cutoff advances, pushing previously-recent
        // edits into the stale zone where their args get stripped.
        for i in 0..2 {
            let id = format!("extra_{}", i);
            msgs.push(assistant_tool_call(
                &id,
                "edit",
                serde_json::json!({
                    "path": format!("src/extra_{}.rs", i),
                    "old_text": "x".repeat(2000),
                    "new_text": "y".repeat(2000),
                }),
            ));
            msgs.push(tool_result(&id, &format!("Edited src/extra_{}.rs", i)));
        }

        let curr = serialize_messages(
            &transform_context(msgs, 200_000, None),
        );

        let mut changed = 0;
        for (a, b) in prev.iter().zip(curr.iter()) {
            if a != b {
                changed += 1;
            }
        }
        assert!(
            changed > 0,
            "expected at least one prefix message to change when cutoff is not frozen \
             — if this passes, the sliding cutoff no longer causes instability and \
             this test should be updated",
        );
    }

    #[test]
    fn frozen_cutoff_still_strips_stale_content() {
        // The frozen cutoff should still apply stale-turn optimizations to
        // messages before the cutoff — we're not disabling compression,
        // just making it stable.
        let rounds = RECENT_TURNS + 5;
        let msgs = build_edit_heavy_conversation(rounds);
        let frozen_cutoff = msgs.len().saturating_sub(RECENT_TURNS);

        let result = transform_context(msgs, 200_000, Some(frozen_cutoff));

        // Messages before the cutoff: edit args stripped to just path
        let early_edit = get_assistant(&result[1]);
        if let ContentBlock::ToolCall {
            name, arguments, ..
        } = &early_edit.content[0]
        {
            assert_eq!(name, "edit");
            assert!(
                arguments.get("old_text").is_none(),
                "stale edit should have old_text stripped",
            );
            assert!(
                arguments.get("path").is_some(),
                "stale edit should keep path",
            );
        } else {
            panic!("expected tool call");
        }

        // Messages after the cutoff: full args preserved
        let last_edit_idx = result
            .iter()
            .rposition(|m| matches!(
                m,
                AgentMessage::Assistant(a) if a.content.iter().any(|b| matches!(
                    b,
                    ContentBlock::ToolCall { name, .. } if name == "edit"
                ))
            ))
            .expect("should have at least one edit in recent zone");
        let recent_edit = get_assistant(&result[last_edit_idx]);
        if let ContentBlock::ToolCall {
            name, arguments, ..
        } = &recent_edit.content[0]
        {
            assert_eq!(name, "edit");
            assert!(
                arguments.get("old_text").is_some(),
                "recent edit should preserve old_text",
            );
        } else {
            panic!("expected tool call");
        }
    }

    // --- generalized superseded dedup ---

    #[test]
    fn superseded_grep_narrower_search_supersedes_broader() {
        let msgs = vec![
            user("find the function"),
            // Broad grep across src/
            assistant_tool_call("g1", "grep", serde_json::json!({"pattern": "fn foo", "path": "src/"})),
            tool_result("g1", "src/main.rs:10: fn foo()\nsrc/lib.rs:5: fn foo_bar()"),
            // Narrower grep in src/main.rs (same pattern, narrower path)
            assistant_tool_call("g2", "grep", serde_json::json!({"pattern": "fn foo", "path": "src/main.rs"})),
            tool_result("g2", "src/main.rs:10: fn foo()"),
            assistant_text("Found it."),
        ];

        let result = transform_context(msgs, 200_000, None);
        let first_grep = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };
        assert!(
            first_grep.contains("superseded"),
            "broad grep should be superseded by narrower grep: {}",
            first_grep
        );
    }

    #[test]
    fn superseded_grep_different_pattern_not_superseded() {
        let msgs = vec![
            user("search"),
            assistant_tool_call("g1", "grep", serde_json::json!({"pattern": "fn foo", "path": "src/"})),
            tool_result("g1", "src/main.rs:10: fn foo()"),
            // Different pattern — should NOT supersede
            assistant_tool_call("g2", "grep", serde_json::json!({"pattern": "fn bar", "path": "src/"})),
            tool_result("g2", "src/lib.rs:5: fn bar()"),
            assistant_text("Done."),
        ];

        let result = transform_context(msgs, 200_000, None);
        let first_grep = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };
        assert!(
            !first_grep.contains("superseded"),
            "different pattern greps should not supersede each other"
        );
    }

    #[test]
    fn superseded_ls_child_supersedes_parent() {
        let msgs = vec![
            user("explore the codebase"),
            // List parent dir
            assistant_tool_call("l1", "ls", serde_json::json!({"path": "src/"})),
            tool_result("l1", "src/main.rs\nsrc/lib.rs\nsrc/agent/\nsrc/tools/"),
            // List child dir (narrower)
            assistant_tool_call("l2", "ls", serde_json::json!({"path": "src/agent/"})),
            tool_result("l2", "src/agent/agent.rs\nsrc/agent/convert.rs"),
            assistant_text("Found the agent module."),
        ];

        let result = transform_context(msgs, 200_000, None);
        let parent_ls = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };
        assert!(
            parent_ls.contains("superseded"),
            "parent ls should be superseded by child ls: {}",
            parent_ls
        );
    }

    #[test]
    fn superseded_edit_intermediate_results_collapsed() {
        let msgs = vec![
            user("refactor"),
            // Three edits to the same file
            assistant_tool_call("e1", "edit", serde_json::json!({"path": "src/main.rs", "old_text": "a", "new_text": "b"})),
            tool_result("e1", "Edited src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n-a\n+b"),
            assistant_tool_call("e2", "edit", serde_json::json!({"path": "src/main.rs", "old_text": "b", "new_text": "c"})),
            tool_result("e2", "Edited src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n-b\n+c"),
            assistant_tool_call("e3", "edit", serde_json::json!({"path": "src/main.rs", "old_text": "c", "new_text": "d"})),
            tool_result("e3", "Edited src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n-c\n+d"),
            assistant_text("Done."),
        ];

        let result = transform_context(msgs, 200_000, None);
        // First two edit results should be superseded, last preserved
        let edit1 = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };
        let edit2 = match &result[4] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };
        let edit3 = match &result[6] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };
        assert!(edit1.contains("superseded"), "first edit should be superseded: {}", edit1);
        assert!(edit2.contains("superseded"), "second edit should be superseded: {}", edit2);
        assert!(!edit3.contains("superseded"), "last edit should be preserved");
    }

    // --- adaptive stale cutoff ---

    #[test]
    fn adaptive_cutoff_shrinks_for_focused_editing() {
        // All recent tool calls target the same 1-2 files → window shrinks
        let mut msgs = vec![user("fix the bug")];
        for i in 0..12 {
            let id = format!("t{}", i);
            // All edits/reads target the same file
            let tool = if i % 2 == 0 { "edit" } else { "read" };
            msgs.push(assistant_tool_call(&id, tool, serde_json::json!({"path": "src/main.rs"})));
            msgs.push(tool_result(&id, "ok"));
        }

        let recent = compute_adaptive_recent(&msgs);
        assert!(
            recent < RECENT_TURNS,
            "focused editing should shrink window below {}, got {}",
            RECENT_TURNS, recent
        );
    }

    #[test]
    fn adaptive_cutoff_expands_for_exploration() {
        // Recent tool calls target many different files → window expands
        let mut msgs = vec![user("explore the codebase")];
        for i in 0..12 {
            let id = format!("t{}", i);
            msgs.push(assistant_tool_call(
                &id,
                "read",
                serde_json::json!({"path": format!("src/file_{}.rs", i)}),
            ));
            msgs.push(tool_result(&id, &format!("contents of file {}", i)));
        }

        let recent = compute_adaptive_recent(&msgs);
        assert!(
            recent >= RECENT_TURNS,
            "exploration should keep window at or above {}, got {}",
            RECENT_TURNS, recent
        );
    }

    #[test]
    fn adaptive_cutoff_returns_base_for_short_conversations() {
        let msgs = vec![
            user("hello"),
            assistant_text("hi"),
        ];
        let recent = compute_adaptive_recent(&msgs);
        assert_eq!(recent, RECENT_TURNS, "short conversations use base value");
    }

    // --- tool description pruning ---

    #[test]
    fn tool_pruning_false_for_short_conversations() {
        let msgs = vec![
            user("hi"),
            assistant_text("hello"),
            user("do something"),
            assistant_text("ok"),
        ];
        assert!(!should_prune_tool_descriptions(&msgs));
    }

    #[test]
    fn tool_pruning_true_after_threshold() {
        let mut msgs = vec![user("start")];
        for i in 0..TOOL_PRUNE_THRESHOLD {
            msgs.push(assistant_text(&format!("response {}", i)));
            msgs.push(user(&format!("follow up {}", i)));
        }
        assert!(should_prune_tool_descriptions(&msgs));
    }

    #[test]
    fn tool_pruning_counts_only_assistants() {
        // Many user messages but fewer assistant messages than threshold
        let msgs = vec![
            user("a"), user("b"), user("c"), user("d"), user("e"),
            assistant_text("one"),
            user("f"),
            assistant_text("two"),
        ];
        assert!(!should_prune_tool_descriptions(&msgs));
    }

    // --- read result folding ---

    #[test]
    fn stale_read_folds_unreferenced_lines() {
        // Build a read result with 50 numbered lines (matching read tool format)
        let read_content = (1..=50)
            .map(|i| format!("{:>2}\t{}", i, format!("line {} content here", i)))
            .collect::<Vec<_>>()
            .join("\n");

        let mut msgs = vec![
            user("read and edit"),
            assistant_tool_call("r1", "read", serde_json::json!({"path": "src/main.rs"})),
            tool_result("r1", &read_content),
            // Edit referencing lines 24-26
            assistant_tool_call("e1", "edit", serde_json::json!({
                "path": "src/main.rs",
                "old_string": "line 24 content here\nline 25 content here\nline 26 content here",
                "new_string": "modified content"
            })),
            tool_result("e1", "Edited src/main.rs"),
            assistant_text("done editing"),
        ];
        // Pad to push read into stale zone
        for i in 0..(RECENT_TURNS + 2) {
            let id = format!("pad_{}", i);
            msgs.push(assistant_tool_call(
                &id,
                "bash",
                serde_json::json!({"command": format!("echo {}", i)}),
            ));
            msgs.push(tool_result(&id, &format!("{}", i)));
        }

        let result = transform_context(msgs, 200_000, Some(6));
        let read_text = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };

        // Referenced lines (24-26) should be preserved
        assert!(read_text.contains("line 25 content here"), "referenced line should be kept: {}", read_text);
        // Distant unreferenced lines should be folded
        assert!(!read_text.contains("line 1 content here"), "far unreferenced line should be folded: {}", read_text);
        // Should have folding markers
        assert!(read_text.contains("[lines"), "should have fold markers: {}", read_text);
    }

    #[test]
    fn stale_read_without_edits_uses_normal_truncation() {
        // A stale read with no subsequent edits on the same file should fall through
        // to normal summarize_tool_content behavior
        let read_content = (1..=50)
            .map(|i| format!("{:>2}\t{}", i, format!("line {} stuff", i)))
            .collect::<Vec<_>>()
            .join("\n");

        let mut msgs = vec![
            user("read file"),
            assistant_tool_call("r1", "read", serde_json::json!({"path": "src/other.rs"})),
            tool_result("r1", &read_content),
            assistant_text("noted"),
        ];
        for i in 0..(RECENT_TURNS + 2) {
            let id = format!("pad_{}", i);
            msgs.push(assistant_tool_call(
                &id,
                "bash",
                serde_json::json!({"command": format!("echo {}", i)}),
            ));
            msgs.push(tool_result(&id, &format!("{}", i)));
        }

        let result = transform_context(msgs, 200_000, Some(4));
        let read_text = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };

        // Should use normal truncation (first few lines + truncated marker)
        assert!(read_text.contains("[truncated:"), "should use normal truncation: {}", read_text);
    }

    #[test]
    fn stale_read_fold_preserves_context_around_referenced_lines() {
        // Verify that a few context lines around referenced lines are kept
        let read_content = (1..=40)
            .map(|i| format!("{:>2}\t{}", i, format!("ctx line {}", i)))
            .collect::<Vec<_>>()
            .join("\n");

        let mut msgs = vec![
            user("work"),
            assistant_tool_call("r1", "read", serde_json::json!({"path": "src/foo.rs"})),
            tool_result("r1", &read_content),
            assistant_tool_call("e1", "edit", serde_json::json!({
                "path": "src/foo.rs",
                "old_string": "ctx line 20",
                "new_string": "modified"
            })),
            tool_result("e1", "Edited src/foo.rs"),
            assistant_text("done"),
        ];
        for i in 0..(RECENT_TURNS + 2) {
            let id = format!("pad_{}", i);
            msgs.push(assistant_tool_call(
                &id,
                "bash",
                serde_json::json!({"command": format!("echo {}", i)}),
            ));
            msgs.push(tool_result(&id, &format!("{}", i)));
        }

        let result = transform_context(msgs, 200_000, Some(6));
        let read_text = match &result[2] {
            AgentMessage::ToolResult { content, .. } => content_text(content),
            _ => panic!("expected tool result"),
        };

        // Line 20 and nearby context should be present
        assert!(read_text.contains("ctx line 20"), "exact referenced line: {}", read_text);
        // Line 1 should be folded away (far from reference)
        assert!(!read_text.contains("ctx line 1\n"), "distant line should be folded: {}", read_text);
    }

    #[test]
    fn frozen_cutoff_stable_with_superseded_reads() {
        // Superseded reads are another source of prefix instability: when a
        // new read of the same file arrives, earlier reads get replaced with
        // "[superseded by later call]". Verify that reads already superseded
        // before the loop stay stable when a new read arrives during the loop.
        let mut msgs = vec![
            user("check the file"),
            assistant_tool_call("r1", "read", serde_json::json!({"path": "src/main.rs"})),
            tool_result("r1", "fn main() { old version }"),
            assistant_tool_call("r2", "read", serde_json::json!({"path": "src/main.rs"})),
            tool_result("r2", "fn main() { updated version }"),
        ];
        // Pad to push reads into stale zone
        for i in 0..(RECENT_TURNS + 2) {
            let id = format!("pad_{}", i);
            msgs.push(assistant_tool_call(
                &id,
                "bash",
                serde_json::json!({"command": format!("echo {}", i)}),
            ));
            msgs.push(tool_result(&id, &format!("{}", i)));
        }

        let frozen_cutoff = msgs.len().saturating_sub(RECENT_TURNS);
        let prev = serialize_messages(
            &transform_context(msgs.clone(), 200_000, Some(frozen_cutoff)),
        );

        // Add a THIRD read of the same file during the tool loop
        msgs.push(assistant_tool_call(
            "r3",
            "read",
            serde_json::json!({"path": "src/main.rs"}),
        ));
        msgs.push(tool_result("r3", "fn main() { newest version }"));

        let curr = serialize_messages(
            &transform_context(msgs, 200_000, Some(frozen_cutoff)),
        );

        // r1 was already superseded before the loop — it should stay stable
        let r1_result_idx = 2; // tool_result for r1
        assert_eq!(
            prev[r1_result_idx], curr[r1_result_idx],
            "already-superseded read (r1) should not change during loop",
        );
    }
}
