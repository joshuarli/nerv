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
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_pct: 0.80,
            keep_recent_tokens: 20_000,
            verbatim_window_tokens: 5_000,
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

/// Calculate context tokens from the last API usage response.
pub fn calculate_context_tokens(usage: &Usage) -> u32 {
    usage.input + usage.output + usage.cache_read + usage.cache_write
}

pub fn should_compact(tokens: usize, context_window: u32, settings: &CompactionSettings) -> bool {
    if !settings.enabled {
        return false;
    }
    let threshold = (context_window as f64 * settings.threshold_pct) as usize;
    tokens > threshold
}

pub struct CompactionResult {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u32,
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
