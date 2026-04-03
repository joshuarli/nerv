pub mod summarize;

use crate::agent::types::{AgentMessage, ContentBlock, ContentItem, Usage};
use crate::session::types::SessionEntry;

pub struct CompactionSettings {
    pub enabled: bool,
    /// Fraction of the model's context window at which auto-compact triggers
    /// (0.0–1.0).
    pub threshold_pct: f64,
    /// Total token budget for the post-compaction context (summary + verbatim
    /// window).
    pub keep_recent_tokens: usize,
    /// How many tokens of the kept window to preserve verbatim rather than
    /// summarize.
    ///
    /// When compaction fires, the kept window (newest `keep_recent_tokens` of
    /// history) is split into two parts:
    ///   - oldest part  → passed to the summarizer, replaced by a compact
    ///     summary
    ///   - newest part  → kept verbatim in the DB (this is the verbatim window)
    ///
    /// The verbatim window saves summarizer cost (fewer tokens sent to Haiku)
    /// and accelerates cache recovery: the first post-compaction API call
    /// is cache-cold regardless, but the cache breakpoint on the summary
    /// (bp3) means the summary is Rc from the second call onward. The
    /// verbatim messages, being byte-identical to what was sent
    /// pre-compaction, form a stable suffix that helps the prefix match
    /// on subsequent calls. Set to 0 to summarize the entire kept range.
    pub verbatim_window_tokens: usize,
    /// Token budget for extracting verbatim user messages from the summarized
    /// region. These are injected alongside the summary to preserve exact
    /// details (file paths, edge cases, preferences) that the LLM summary may
    /// have compressed away. Set to 0 to disable.
    pub preserved_user_tokens: usize,
    /// Maximum user turns since last full/summary compaction before
    /// summary-compact is considered too stale and a fresh LLM summary is
    /// generated instead. Set to 0 to disable summary-compact entirely.
    pub summary_compact_max_turns: usize,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_pct: 0.80,
            keep_recent_tokens: 20_000,
            verbatim_window_tokens: 5_000,
            preserved_user_tokens: 8_000,
            summary_compact_max_turns: 6,
        }
    }
}

/// Estimate token count using the chars/4 heuristic.
/// Approximate but fast — the authoritative count always comes from the API
/// usage response.
pub fn count_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Estimate tokens for an AgentMessage.
pub fn estimate_tokens(msg: &AgentMessage) -> usize {
    match msg {
        AgentMessage::User { content, .. } => {
            content_tokens(content) + 4 // role overhead
        }
        AgentMessage::Assistant(a) => {
            let mut tokens = 4; // role overhead
            for block in &a.content {
                match block {
                    ContentBlock::Text { text } => tokens += count_tokens(text),
                    ContentBlock::Thinking { thinking } => tokens += count_tokens(thinking),
                    ContentBlock::ToolCall { name, arguments, .. } => {
                        tokens += count_tokens(name) + count_tokens(&arguments.to_string()) + 10;
                    }
                }
            }
            tokens
        }
        AgentMessage::ToolResult { content, .. } => content_tokens(content) + 4,
        AgentMessage::Custom { content, .. } => content_tokens(content) + 4,
        AgentMessage::BashExecution { command, output, .. } => {
            count_tokens(command) + count_tokens(output) + 4
        }
        AgentMessage::CompactionSummary { summary, .. } => count_tokens(summary) + 4,
        AgentMessage::BranchSummary { summary, .. } => count_tokens(summary) + 4,
    }
}

fn content_tokens(content: &[ContentItem]) -> usize {
    content
        .iter()
        .map(|item| match item {
            ContentItem::Text { text } => count_tokens(text),
            ContentItem::Image { .. } => 1200, // image token estimate
        })
        .sum()
}

/// Extract verbatim user message texts from a slice of messages, selecting
/// most-recent-first up to `token_budget` tokens. Returns messages in
/// chronological order. Skips CompactionSummary messages (prior summaries)
/// and image-only user messages.
pub fn extract_user_messages(messages: &[AgentMessage], token_budget: usize) -> Vec<String> {
    if token_budget == 0 {
        return Vec::new();
    }
    let mut selected: Vec<String> = Vec::new();
    let mut remaining = token_budget;
    for msg in messages.iter().rev() {
        if remaining == 0 {
            break;
        }
        let text = match msg {
            AgentMessage::User { content, .. } => {
                let parts: Vec<&str> = content
                    .iter()
                    .filter_map(|item| match item {
                        ContentItem::Text { text } if !text.is_empty() => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if parts.is_empty() {
                    continue;
                }
                parts.join("\n")
            }
            _ => continue,
        };
        let tokens = count_tokens(&text);
        if tokens <= remaining {
            selected.push(text);
            remaining = remaining.saturating_sub(tokens);
        } else {
            // Truncate to fit the remaining budget (chars ≈ tokens * 4).
            let char_budget = remaining * 4;
            if char_budget > 0 && !text.is_empty() {
                let end = text
                    .char_indices()
                    .nth(char_budget)
                    .map(|(i, _)| i)
                    .unwrap_or(text.len());
                let truncated = &text[..end];
                if !truncated.is_empty() {
                    selected.push(truncated.to_string());
                }
            }
            break;
        }
    }
    selected.reverse();
    selected
}

/// Calculate context tokens from the last API usage response.
pub fn calculate_context_tokens(usage: &Usage) -> u32 {
    usage.input + usage.output + usage.cache_read + usage.cache_write
}

/// Find the true context size right before compaction fires.
///
/// Walks the branch in reverse to find the most recent assistant message whose
/// `TokenInfo.context_used` is non-zero — that is the API-reported context the
/// user sees in the footer. Falls back to summing `estimate_tokens` across all
/// branch messages if no usage data exists (e.g. first turn not yet complete).
pub fn tokens_before_compaction(branch: &[SessionEntry]) -> u32 {
    branch
        .iter()
        .rev()
        .find_map(|e| {
            if let SessionEntry::Message(me) = e {
                let info = me.tokens.as_ref()?;
                if info.context_used > 0 { Some(info.context_used) } else { None }
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            branch
                .iter()
                .filter_map(|e| {
                    if let SessionEntry::Message(me) = e {
                        Some(estimate_tokens(&me.message) as u32)
                    } else {
                        None
                    }
                })
                .sum()
        })
}

/// Estimated context size after compaction: summary tokens + preserved user
/// messages + verbatim window tokens.
///
/// This represents what will actually be sent on the next API call — the
/// replacement summary, preserved user messages, plus the unmodified verbatim
/// window entries.
pub fn tokens_after_compaction(
    summary: &str,
    preserved_user_messages: &[String],
    verbatim_window: &[SessionEntry],
) -> u32 {
    let verbatim_tokens: u32 = verbatim_window
        .iter()
        .filter_map(|e| {
            if let SessionEntry::Message(me) = e {
                Some(estimate_tokens(&me.message) as u32)
            } else {
                None
            }
        })
        .sum();
    let preserved_tokens: u32 = preserved_user_messages
        .iter()
        .map(|m| count_tokens(m) as u32)
        .sum();
    count_tokens(summary) as u32 + preserved_tokens + verbatim_tokens
}

pub fn should_compact(tokens: usize, context_window: u32, settings: &CompactionSettings) -> bool {
    if !settings.enabled {
        return false;
    }
    let threshold = (context_window as f64 * settings.threshold_pct) as usize;
    tokens > threshold
}

/// Count user turns (User messages) on the branch since the most recent
/// non-lite compaction. Returns 0 if there are no user turns after the last
/// compaction, or the total user turn count if no compaction exists.
pub fn count_user_turns_since_compaction(branch: &[SessionEntry]) -> usize {
    let mut count = 0;
    for entry in branch.iter().rev() {
        match entry {
            SessionEntry::Compaction(c) if c.compaction_type != "lite" => {
                return count;
            }
            SessionEntry::Message(me) if matches!(me.message, AgentMessage::User { .. }) => {
                count += 1;
            }
            _ => {}
        }
    }
    count
}

pub struct CompactionResult {
    pub summary: String,
    pub structured: Option<summarize::StructuredSummary>,
    pub first_kept_entry_id: String,
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub model_id: String,
}

/// Outcome of `run_compaction()`.
pub enum CompactionOutcome {
    /// Full LLM compaction performed — caller should reload from DB and retry.
    Full(CompactionResult),
    /// Lite-compact zeroed enough stale outputs that the threshold is no longer
    /// exceeded. Messages are already mutated in place — do NOT reload from DB.
    LiteCompact { zeroed: usize },
    /// Nothing to compact (context too small or no messages before cut point).
    None,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::{AgentMessage, AssistantMessage, ContentBlock, StopReason};
    use crate::session::types::{MessageEntry, TokenInfo};

    fn user_entry(text: &str, context_used: u32) -> SessionEntry {
        SessionEntry::Message(MessageEntry {
            id: "test".into(),
            parent_id: None,
            timestamp: String::new(),
            message: AgentMessage::User {
                content: vec![crate::agent::types::ContentItem::Text { text: text.to_string() }],
                timestamp: 0,
            },
            tokens: if context_used > 0 {
                Some(TokenInfo {
                    input: context_used / 2,
                    output: context_used / 2,
                    cache_read: 0,
                    cache_write: 0,
                    context_used,
                    context_window: 200_000,
                    cost_usd: 0.0,
                })
            } else {
                None
            },
        })
    }

    fn assistant_entry(text: &str, context_used: u32) -> SessionEntry {
        SessionEntry::Message(MessageEntry {
            id: "test".into(),
            parent_id: None,
            timestamp: String::new(),
            message: AgentMessage::Assistant(AssistantMessage {
                content: vec![ContentBlock::Text { text: text.to_string() }],
                stop_reason: StopReason::EndTurn,
                usage: None,
                timestamp: 0,
            }),
            tokens: if context_used > 0 {
                Some(TokenInfo {
                    input: context_used / 2,
                    output: context_used / 2,
                    cache_read: 0,
                    cache_write: 0,
                    context_used,
                    context_window: 200_000,
                    cost_usd: 0.0,
                })
            } else {
                None
            },
        })
    }

    // tokens_before_compaction

    #[test]
    fn tokens_before_uses_last_context_used() {
        // Simulates a resumed session: first message starts at 100k context
        // because it carries over prior history.
        let branch = vec![
            user_entry("first prompt", 0),
            assistant_entry("first reply", 100_477),
            user_entry("second prompt", 0),
            assistant_entry("second reply", 102_000),
        ];
        // Must pick the *last* assistant context_used (102_000), not estimate sums.
        assert_eq!(tokens_before_compaction(&branch), 102_000);
    }

    #[test]
    fn tokens_before_picks_most_recent_nonzero() {
        // Only the first message has usage data — later ones don't.
        let branch = vec![
            assistant_entry("a", 85_301),
            user_entry("b", 0),
        ];
        assert_eq!(tokens_before_compaction(&branch), 85_301);
    }

    #[test]
    fn tokens_before_falls_back_to_estimate_when_no_usage() {
        // No TokenInfo at all — must fall back to estimate_tokens sum.
        let branch = vec![
            user_entry("hello world", 0),
            user_entry("another message", 0),
        ];
        let estimated: u32 = branch
            .iter()
            .filter_map(|e| {
                if let SessionEntry::Message(me) = e {
                    Some(estimate_tokens(&me.message) as u32)
                } else {
                    None
                }
            })
            .sum();
        assert!(estimated > 0);
        assert_eq!(tokens_before_compaction(&branch), estimated);
    }

    #[test]
    fn tokens_before_ignores_zero_context_used() {
        // An aborted assistant message has context_used=0 and must not win.
        let branch = vec![
            assistant_entry("good reply", 80_000),
            user_entry("follow up", 0),
            // aborted assistant — context_used=0
            SessionEntry::Message(MessageEntry {
                id: "aborted".into(),
                parent_id: None,
                timestamp: String::new(),
                message: AgentMessage::Assistant(AssistantMessage {
                    content: vec![],
                    stop_reason: StopReason::EndTurn,
                    usage: None,
                    timestamp: 0,
                }),
                tokens: Some(TokenInfo {
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    context_used: 0,
                    context_window: 200_000,
                    cost_usd: 0.0,
                }),
            }),
        ];
        assert_eq!(tokens_before_compaction(&branch), 80_000);
    }

    // tokens_after_compaction

    #[test]
    fn tokens_after_includes_summary_and_verbatim() {
        let summary = "a".repeat(400); // 400 chars / 4 = 100 tokens
        let verbatim = vec![
            user_entry("recent user message", 0),   // ~4 chars/4 + 4 overhead = ~5
            assistant_entry("recent reply", 0),     // ~12 chars/4 + 4 overhead = ~7
        ];
        let result = tokens_after_compaction(&summary, &[], &verbatim);
        let summary_toks = count_tokens(&summary) as u32;
        let verbatim_toks: u32 = verbatim
            .iter()
            .filter_map(|e| {
                if let SessionEntry::Message(me) = e {
                    Some(estimate_tokens(&me.message) as u32)
                } else {
                    None
                }
            })
            .sum();
        assert_eq!(result, summary_toks + verbatim_toks);
    }

    #[test]
    fn tokens_after_degenerate_verbatim_still_counts_summary() {
        // Old bug: when verbatim window = aborted message (4 tok), tokens_after
        // was reported as 4. Now it must include the summary.
        let summary = "a".repeat(2560); // ~640 tokens (typical compaction summary)
        let aborted_verbatim = vec![SessionEntry::Message(MessageEntry {
            id: "aborted".into(),
            parent_id: None,
            timestamp: String::new(),
            message: AgentMessage::Assistant(AssistantMessage {
                content: vec![],
                stop_reason: StopReason::EndTurn,
                usage: None,
                timestamp: 0,
            }),
            tokens: None,
        })];
        let result = tokens_after_compaction(&summary, &[], &aborted_verbatim);
        // Must be much more than 4 — summary alone is ~640 tokens
        assert!(result > 100, "tokens_after was {result}, expected > 100 (summary not counted)");
    }

    #[test]
    fn tokens_after_empty_verbatim_is_just_summary() {
        let summary = "a".repeat(800); // 200 tokens
        let result = tokens_after_compaction(&summary, &[], &[]);
        assert_eq!(result, count_tokens(&summary) as u32);
    }

    #[test]
    fn tokens_after_includes_preserved_user_messages() {
        let summary = "a".repeat(400); // 100 tokens
        let preserved = vec!["hello world".to_string(), "another message".to_string()];
        let result_with = tokens_after_compaction(&summary, &preserved, &[]);
        let result_without = tokens_after_compaction(&summary, &[], &[]);
        assert!(result_with > result_without, "preserved messages should add tokens");
    }

    // extract_user_messages

    fn make_user_msg(text: &str) -> AgentMessage {
        AgentMessage::User {
            content: vec![ContentItem::Text { text: text.to_string() }],
            timestamp: 0,
        }
    }

    fn make_assistant_msg(text: &str) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text { text: text.to_string() }],
            stop_reason: StopReason::EndTurn,
            usage: None,
            timestamp: 0,
        })
    }

    #[test]
    fn extract_user_messages_basic() {
        let msgs = vec![
            make_user_msg("first"),
            make_assistant_msg("reply"),
            make_user_msg("second"),
            make_assistant_msg("reply2"),
            make_user_msg("third"),
        ];
        let result = extract_user_messages(&msgs, 10_000);
        assert_eq!(result, vec!["first", "second", "third"]);
    }

    #[test]
    fn extract_user_messages_respects_budget() {
        // Each "x".repeat(400) is ~100 tokens. Budget of 150 should get 1 full + partial.
        let msgs = vec![
            make_user_msg(&"a".repeat(400)),
            make_user_msg(&"b".repeat(400)),
        ];
        let result = extract_user_messages(&msgs, 150);
        // Most recent first: "b" fits (100 tokens), "a" gets truncated
        assert_eq!(result.len(), 2);
        assert!(result[1].starts_with("bbb"));
    }

    #[test]
    fn extract_user_messages_skips_compaction_summary() {
        let msgs = vec![
            AgentMessage::CompactionSummary {
                summary: "prior".to_string(),
                tokens_before: 0,
                timestamp: 0,
            },
            make_user_msg("real message"),
        ];
        let result = extract_user_messages(&msgs, 10_000);
        assert_eq!(result, vec!["real message"]);
    }

    #[test]
    fn extract_user_messages_empty_budget() {
        let msgs = vec![make_user_msg("hello")];
        let result = extract_user_messages(&msgs, 0);
        assert!(result.is_empty());
    }

    #[test]
    fn extract_user_messages_empty_input() {
        let result = extract_user_messages(&[], 10_000);
        assert!(result.is_empty());
    }

    // count_user_turns_since_compaction

    fn compaction_entry(compaction_type: &str) -> SessionEntry {
        SessionEntry::Compaction(crate::session::types::CompactionEntry {
            id: "c1".into(),
            parent_id: None,
            timestamp: String::new(),
            summary: "summary".into(),
            first_kept_entry_id: String::new(),
            tokens_before: 0,
            tokens_after: 0,
            model_id: String::new(),
            cost_usd_before: 0.0,
            compaction_type: compaction_type.to_string(),
            lite_compact_zeroed: 0,
            archived_messages: vec![],
            preserved_user_messages: vec![],
        })
    }

    #[test]
    fn count_turns_no_compaction() {
        let branch = vec![
            user_entry("a", 0),
            assistant_entry("b", 0),
            user_entry("c", 0),
        ];
        assert_eq!(count_user_turns_since_compaction(&branch), 2);
    }

    #[test]
    fn count_turns_after_full_compaction() {
        let branch = vec![
            compaction_entry("full"),
            user_entry("a", 0),
            assistant_entry("b", 0),
            user_entry("c", 0),
        ];
        assert_eq!(count_user_turns_since_compaction(&branch), 2);
    }

    #[test]
    fn count_turns_after_summary_compaction() {
        let branch = vec![
            compaction_entry("summary"),
            user_entry("a", 0),
        ];
        assert_eq!(count_user_turns_since_compaction(&branch), 1);
    }

    #[test]
    fn count_turns_skips_lite_compaction() {
        let branch = vec![
            compaction_entry("full"),
            user_entry("a", 0),
            compaction_entry("lite"),
            user_entry("b", 0),
            user_entry("c", 0),
        ];
        // Lite compaction doesn't reset the counter — count back to the "full"
        assert_eq!(count_user_turns_since_compaction(&branch), 3);
    }

    #[test]
    fn count_turns_zero_after_compaction() {
        let branch = vec![
            compaction_entry("full"),
            assistant_entry("reply", 0),
        ];
        assert_eq!(count_user_turns_since_compaction(&branch), 0);
    }

    // count_tokens / estimate_tokens sanity

    #[test]
    fn count_tokens_div_ceil() {
        assert_eq!(count_tokens("abcd"), 1);
        assert_eq!(count_tokens("abcde"), 2);
        assert_eq!(count_tokens(""), 0);
        assert_eq!(count_tokens("a"), 1);
    }
}

/// Result of finding a cut point for compaction.
///
/// The session history is split into three regions:
///
/// ```text
/// [0 .. first_kept_entry_index)         → deleted from DB, replaced by summary
/// [first_kept_entry_index .. verbatim_start_index)  → summarized by LLM call
/// [verbatim_start_index .. end)         → kept verbatim (cache-warm after compaction)
/// ```
///
/// When `verbatim_window_tokens == 0`, `verbatim_start_index ==
/// first_kept_entry_index` and the entire kept range is summarized (no verbatim
/// window).
pub struct CutPointResult {
    /// First entry index to keep in the DB (deletion boundary).
    pub first_kept_entry_index: usize,
    /// First entry of the verbatim window. Everything between here and `end` is
    /// left byte-for-byte in the DB so it remains a cache-read hit
    /// post-compaction.
    pub verbatim_start_index: usize,
    /// Index of user message that starts the turn being split, or None.
    pub turn_start_index: Option<usize>,
    /// Whether this cut splits a turn (cut point is not a user message).
    pub is_split_turn: bool,
}

/// Find valid cut points: indices of entries that can be cut at.
/// Never cut at tool results (they must follow their tool call).
fn find_valid_cut_points(entries: &[SessionEntry], start: usize, end: usize) -> Vec<usize> {
    let mut points = Vec::new();
    for (i, entry) in entries.iter().enumerate().take(end).skip(start) {
        match entry {
            SessionEntry::Message(me) => {
                match &me.message {
                    AgentMessage::User { .. }
                    | AgentMessage::Assistant(_)
                    | AgentMessage::Custom { .. }
                    | AgentMessage::BashExecution { .. }
                    | AgentMessage::CompactionSummary { .. }
                    | AgentMessage::BranchSummary { .. } => {
                        points.push(i);
                    }
                    AgentMessage::ToolResult { .. } => {} // never cut at tool results
                }
            }
            SessionEntry::BranchSummary(_) | SessionEntry::CustomMessage(_) => {
                points.push(i);
            }
            _ => {} // metadata entries: skip
        }
    }
    points
}

/// Find the user message that starts the turn containing `entry_index`.
fn find_turn_start(entries: &[SessionEntry], entry_index: usize, start: usize) -> Option<usize> {
    for i in (start..=entry_index).rev() {
        if let SessionEntry::Message(me) = &entries[i]
            && matches!(me.message, AgentMessage::User { .. } | AgentMessage::BashExecution { .. })
        {
            return Some(i);
        }
        if matches!(entries[i], SessionEntry::BranchSummary(_) | SessionEntry::CustomMessage(_)) {
            return Some(i);
        }
    }
    None
}

/// Get message from a session entry (if it has one).
fn entry_message(entry: &SessionEntry) -> Option<&AgentMessage> {
    match entry {
        SessionEntry::Message(me) => Some(&me.message),
        SessionEntry::CustomMessage(me) => Some(&me.message),
        _ => None,
    }
}

/// Find the cut point that keeps approximately `keep_recent_tokens`.
///
/// Algorithm: Walk backwards from newest, accumulating token counts.
/// Stop when we exceed the budget. Cut at the closest valid cut point.
pub fn find_cut_point(
    entries: &[SessionEntry],
    start: usize,
    end: usize,
    keep_recent_tokens: usize,
    verbatim_window_tokens: usize,
) -> CutPointResult {
    let cut_points = find_valid_cut_points(entries, start, end);

    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start,
            verbatim_start_index: start,
            turn_start_index: None,
            is_split_turn: false,
        };
    }

    let mut accumulated = 0usize;
    let mut cut_index = cut_points[0];

    // Walk backwards from newest, accumulating token estimates
    for i in (start..end).rev() {
        let Some(msg) = entry_message(&entries[i]) else {
            continue;
        };
        accumulated += estimate_tokens(msg);

        if accumulated >= keep_recent_tokens {
            // Find the closest valid cut point at or after this entry
            for &cp in &cut_points {
                if cp >= i {
                    cut_index = cp;
                    break;
                }
            }
            break;
        }
    }

    // Include preceding non-message entries (settings changes, etc.)
    while cut_index > start {
        let prev = &entries[cut_index - 1];
        if matches!(prev, SessionEntry::Compaction(_)) {
            break;
        }
        if matches!(prev, SessionEntry::Message(_)) {
            break;
        }
        cut_index -= 1;
    }

    // Determine if this is a split turn
    let is_user_message = matches!(
        &entries[cut_index],
        SessionEntry::Message(me) if matches!(me.message, AgentMessage::User { .. })
    );
    let turn_start =
        if is_user_message { None } else { find_turn_start(entries, cut_index, start) };

    // Find the verbatim_start_index: the boundary within the kept window between
    // "summarize this" and "keep this verbatim". Walk backwards from the end of the
    // kept window, accumulating token counts, until we've claimed
    // verbatim_window_tokens worth of entries. Everything older than that
    // boundary is handed to the summarizer.
    //
    // Why: the entries in the verbatim window were cache-read (Rc) hits in the
    // requests that preceded compaction. Keeping them byte-identical in the DB
    // means they stay Rc on the first post-compaction API call. Only the new
    // summary prefix is cache-cold.
    let verbatim_start_index =
        if verbatim_window_tokens == 0 || verbatim_window_tokens >= keep_recent_tokens {
            // No verbatim window — summarize the entire kept range.
            cut_index
        } else {
            let mut verbatim_accum = 0usize;
            let mut vs_idx = end; // fallback: keep everything verbatim
            for i in (cut_index..end).rev() {
                let Some(msg) = entry_message(&entries[i]) else {
                    continue;
                };
                verbatim_accum += estimate_tokens(msg);
                if verbatim_accum >= verbatim_window_tokens {
                    // Find the nearest valid cut point at or after i
                    let vcp = cut_points.iter().find(|&&cp| cp >= i).copied().unwrap_or(cut_index);
                    vs_idx = vcp;
                    break;
                }
            }
            // Clamp: verbatim_start must be >= first_kept_entry_index
            vs_idx.max(cut_index)
        };

    CutPointResult {
        first_kept_entry_index: cut_index,
        verbatim_start_index,
        turn_start_index: turn_start,
        is_split_turn: turn_start.is_some(),
    }
}
