use nerv::agent::types::*;
use nerv::compaction::*;
use nerv::session::types::*;

// ── Helpers
// ───────────────────────────────────────────────────────────────────

fn user_entry(text: &str, id: &str, parent: Option<&str>) -> SessionEntry {
    SessionEntry::Message(MessageEntry {
        id: id.to_string(),
        parent_id: parent.map(|s| s.to_string()),
        timestamp: now_iso(),
        tokens: None,
        message: AgentMessage::User {
            content: vec![ContentItem::Text { text: text.into() }],
            timestamp: now_millis(),
        },
    })
}

fn assistant_entry(text: &str, id: &str, parent: &str) -> SessionEntry {
    SessionEntry::Message(MessageEntry {
        id: id.to_string(),
        parent_id: Some(parent.to_string()),
        timestamp: now_iso(),
        tokens: None,
        message: AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text { text: text.into() }],
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage { input: 100, output: 50, ..Default::default() }),
            timestamp: now_millis(),
        }),
    })
}

fn tool_result_entry(text: &str, id: &str, parent: &str) -> SessionEntry {
    SessionEntry::Message(MessageEntry {
        id: id.to_string(),
        parent_id: Some(parent.to_string()),
        timestamp: now_iso(),
        tokens: None,
        message: AgentMessage::ToolResult {
            tool_call_id: "tc1".into(),
            content: vec![ContentItem::Text { text: text.into() }],
            is_error: false,
            display: None,
            details: None,
            timestamp: now_millis(),
        },
    })
}

/// Build a linear chain of alternating user/assistant entries with predictable
/// ids. Each entry is padded to ~`tokens_each` tokens so token-budget tests are
/// precise.
fn linear_chain(count: usize, tokens_each: usize) -> Vec<SessionEntry> {
    let word_count = tokens_each.saturating_sub(4); // subtract overhead
    let text = "word ".repeat(word_count);
    let mut entries = Vec::new();
    let mut prev: Option<String> = None;
    for i in 0..count {
        let id = format!("e{i}");
        let role_text = format!("{} {}", if i % 2 == 0 { "user" } else { "asst" }, text);
        let entry = if i % 2 == 0 {
            user_entry(&role_text, &id, prev.as_deref())
        } else {
            assistant_entry(&role_text, &id, prev.as_deref().unwrap_or("e0"))
        };
        entries.push(entry);
        prev = Some(id);
    }
    entries
}

// ── Token counting
// ────────────────────────────────────────────────────────────

#[test]
fn count_tokens_returns_reasonable_values() {
    // "Hello, world!" is typically 4 tokens in cl100k_base
    let n = count_tokens("Hello, world!");
    assert!((3..=6).contains(&n), "got {n}");
    assert_eq!(count_tokens(""), 0);

    // Longer text should have proportionally more tokens
    let short = count_tokens("hello");
    let long = count_tokens(&"hello ".repeat(100));
    assert!(long > short * 10);
}

#[test]
fn estimate_tokens_includes_role_overhead() {
    let msg =
        AgentMessage::User { content: vec![ContentItem::Text { text: "Hi".into() }], timestamp: 0 };
    // "Hi" ≈ 1 token + 4 overhead
    assert!(estimate_tokens(&msg) >= 4);
}

#[test]
fn estimate_tokens_grows_with_content() {
    let short =
        AgentMessage::User { content: vec![ContentItem::Text { text: "hi".into() }], timestamp: 0 };
    let long = AgentMessage::User {
        content: vec![ContentItem::Text { text: "word ".repeat(200) }],
        timestamp: 0,
    };
    assert!(estimate_tokens(&long) > estimate_tokens(&short) * 10);
}

// ── should_compact
// ────────────────────────────────────────────────────────────

#[test]
fn should_compact_triggers_at_threshold() {
    let s = CompactionSettings {
        enabled: true,
        threshold_pct: 0.90,
        keep_recent_tokens: 20_000,
        verbatim_window_tokens: 5_000,
        preserved_user_tokens: 0,
        summary_compact_max_turns: 0,
    };
    assert!(!should_compact(89_999, 100_000, &s));
    assert!(should_compact(90_001, 100_000, &s));
}

#[test]
fn should_compact_respects_enabled_flag() {
    let s = CompactionSettings {
        enabled: false,
        threshold_pct: 0.10,
        keep_recent_tokens: 1_000,
        verbatim_window_tokens: 0,
        preserved_user_tokens: 0,
        summary_compact_max_turns: 0,
    };
    // Even 99% full, disabled means no compact
    assert!(!should_compact(99_999, 100_000, &s));
}

#[test]
fn should_compact_default_settings_threshold_is_80pct() {
    let s = CompactionSettings::default();
    assert!(!should_compact(79_999, 100_000, &s));
    assert!(should_compact(80_001, 100_000, &s));
}

// ── find_cut_point: basic invariants ─────────────────────────────────────────

#[test]
fn cut_point_empty_entries_returns_start() {
    let entries: Vec<SessionEntry> = vec![];
    let cut = find_cut_point(&entries, 0, 0, 1_000, 0);
    assert_eq!(cut.first_kept_entry_index, 0);
    assert_eq!(cut.verbatim_start_index, 0);
    assert!(!cut.is_split_turn);
}

#[test]
fn cut_point_never_cuts_at_tool_result() {
    // Tool results must follow their tool call. Cutting between them produces an
    // invalid context. Verify the algorithm never places the cut there.
    let entries = vec![
        user_entry("question", "e0", None),
        assistant_entry("I'll read the file", "e1", "e0"),
        tool_result_entry(&"x".repeat(500), "e2", "e1"), // large — pressure to cut here
        user_entry("thanks", "e3", Some("e2")),
        assistant_entry("you're welcome", "e4", "e3"),
    ];

    let cut = find_cut_point(&entries, 0, entries.len(), 10, 0);

    if let SessionEntry::Message(me) = &entries[cut.first_kept_entry_index] {
        assert!(
            !matches!(me.message, AgentMessage::ToolResult { .. }),
            "cut at index {} is a ToolResult — must never happen",
            cut.first_kept_entry_index
        );
    }
}

#[test]
fn cut_point_keeps_budget_tokens_from_end() {
    // 20 entries each ~50 tokens; budget of 200 should keep ~4 from the end.
    let entries = linear_chain(20, 50);
    let cut = find_cut_point(&entries, 0, entries.len(), 200, 0);

    // Cut should be at least at index 14 (keeping 6 or fewer) and at most 18
    assert!(
        cut.first_kept_entry_index >= 14,
        "expected cut ≥ 14, got {}",
        cut.first_kept_entry_index
    );
    assert!(
        cut.first_kept_entry_index < entries.len(),
        "cut out of bounds: {}",
        cut.first_kept_entry_index
    );
}

#[test]
fn cut_point_with_huge_budget_keeps_everything() {
    // When the budget exceeds the total context, the cut should be at the start
    // (nothing gets dropped).
    let entries = linear_chain(10, 50);
    let cut = find_cut_point(&entries, 0, entries.len(), 1_000_000, 0);
    assert_eq!(cut.first_kept_entry_index, 0, "giant budget should keep everything");
}

#[test]
fn cut_point_user_message_boundary_not_split_turn() {
    // When the cut lands exactly on a user message, is_split_turn must be false.
    let entries = vec![
        user_entry(&"x".repeat(200), "e0", None),
        assistant_entry(&"x".repeat(200), "e1", "e0"),
        user_entry("recent user", "e2", Some("e1")), // cut should land here
        assistant_entry("recent asst", "e3", "e2"),
    ];

    let cut = find_cut_point(&entries, 0, entries.len(), 50, 0);

    if let SessionEntry::Message(me) = &entries[cut.first_kept_entry_index]
        && matches!(me.message, AgentMessage::User { .. })
    {
        assert!(!cut.is_split_turn, "user-message cut must not be flagged as split turn");
    }
}

#[test]
fn cut_point_assistant_boundary_detects_split_turn() {
    // If the only viable cut is mid-turn (assistant message that's not the start
    // of a turn), is_split_turn should be true and turn_start_index set.
    let entries = vec![
        user_entry(&"x".repeat(400), "e0", None), // huge — will be cut
        assistant_entry("response", "e1", "e0"),  // cut may land here
    ];

    let cut = find_cut_point(&entries, 0, entries.len(), 30, 0);
    // If cut_index is at e1 (an assistant message), it's a split turn
    if cut.first_kept_entry_index == 1 {
        assert!(cut.is_split_turn);
        assert!(cut.turn_start_index.is_some());
    }
}

// ── find_cut_point: verbatim window (partial compaction) ─────────────────────

#[test]
fn verbatim_window_zero_gives_no_verbatim_region() {
    // With verbatim_window_tokens=0, verbatim_start_index ==
    // first_kept_entry_index: the entire kept range is summarized.
    let entries = linear_chain(10, 50);
    let cut = find_cut_point(&entries, 0, entries.len(), 300, 0);
    assert_eq!(
        cut.verbatim_start_index, cut.first_kept_entry_index,
        "zero verbatim window: no verbatim region"
    );
}

#[test]
fn verbatim_window_splits_kept_range() {
    // 10 entries × 100 tokens each. Budget 800 keeps ~8 entries.
    // Verbatim window 200 should carve ~2 entries from the tail as verbatim.
    let entries = linear_chain(10, 100);
    let cut = find_cut_point(&entries, 0, entries.len(), 800, 200);

    // verbatim_start_index must be >= first_kept_entry_index
    assert!(
        cut.verbatim_start_index >= cut.first_kept_entry_index,
        "verbatim_start must be >= first_kept"
    );
    // and < entries.len() (there must be at least some verbatim entries)
    assert!(
        cut.verbatim_start_index < entries.len(),
        "verbatim_start should be before end: {}",
        cut.verbatim_start_index
    );
    // The verbatim window must be smaller than the full kept range
    let kept_range = entries.len() - cut.first_kept_entry_index;
    let verbatim_range = entries.len() - cut.verbatim_start_index;
    assert!(
        verbatim_range < kept_range,
        "verbatim range ({verbatim_range}) should be smaller than full kept range ({kept_range})"
    );
}

#[test]
fn verbatim_window_larger_than_budget_falls_back_to_no_verbatim() {
    // When verbatim_window_tokens >= keep_recent_tokens, treat as "no verbatim
    // window" (summarize everything), which is the same as verbatim_window=0.
    let entries = linear_chain(10, 50);
    let cut_no_verbatim = find_cut_point(&entries, 0, entries.len(), 300, 0);
    let cut_over_budget = find_cut_point(&entries, 0, entries.len(), 300, 999_999);

    assert_eq!(
        cut_no_verbatim.verbatim_start_index, cut_no_verbatim.first_kept_entry_index,
        "zero verbatim: start == first_kept"
    );
    assert_eq!(
        cut_over_budget.verbatim_start_index, cut_over_budget.first_kept_entry_index,
        "over-budget verbatim: same as no-verbatim fallback"
    );
}

#[test]
fn verbatim_region_contains_most_recent_entries() {
    // The verbatim window should always pull from the newest (tail) entries,
    // not arbitrary ones from the middle.
    let entries = linear_chain(12, 80);
    let n = entries.len();
    let cut = find_cut_point(&entries, 0, n, 640, 160); // keep ~8 entries, verbatim ~2

    // The verbatim entries should be the very last ones
    assert!(
        cut.verbatim_start_index >= n - 4,
        "verbatim window should be near the end; verbatim_start={}, n={}",
        cut.verbatim_start_index,
        n
    );
}

#[test]
fn verbatim_start_is_always_valid_cut_point() {
    // verbatim_start_index must land on an entry that is a valid cut point
    // (i.e. not a ToolResult), since summarization ends there.
    let entries = vec![
        user_entry(&"w ".repeat(100), "e0", None),
        assistant_entry(&"w ".repeat(100), "e1", "e0"),
        tool_result_entry(&"x".repeat(200), "e2", "e1"), // NOT a valid cut point
        user_entry("recent", "e3", Some("e2")),
        assistant_entry("done", "e4", "e3"),
    ];

    let cut = find_cut_point(&entries, 0, entries.len(), 400, 100);

    // verbatim_start must not be a ToolResult
    if let SessionEntry::Message(me) = &entries[cut.verbatim_start_index] {
        assert!(
            !matches!(me.message, AgentMessage::ToolResult { .. }),
            "verbatim_start at {} is a ToolResult — invalid cut point",
            cut.verbatim_start_index
        );
    }
}

#[test]
fn three_regions_are_contiguous_and_cover_all_entries() {
    // [0, first_kept) ++ [first_kept, verbatim_start) ++ [verbatim_start, end)
    // must tile exactly over [0, end).
    let entries = linear_chain(20, 60);
    let n = entries.len();
    let cut = find_cut_point(&entries, 0, n, 600, 120);

    // first_kept <= verbatim_start <= end
    assert!(cut.first_kept_entry_index <= cut.verbatim_start_index);
    assert!(cut.verbatim_start_index <= n);

    // The three regions together cover all indices without overlap or gap
    let deleted = cut.first_kept_entry_index;
    let summarized = cut.verbatim_start_index - cut.first_kept_entry_index;
    let verbatim = n - cut.verbatim_start_index;
    assert_eq!(deleted + summarized + verbatim, n);
}

// ── find_cut_point: range parameter ──────────────────────────────────────────

#[test]
fn cut_point_respects_start_parameter() {
    // With start=5, the cut should never be below index 5.
    let entries = linear_chain(10, 50);
    let cut = find_cut_point(&entries, 5, entries.len(), 100, 0);
    assert!(
        cut.first_kept_entry_index >= 5,
        "cut {} is before start=5",
        cut.first_kept_entry_index
    );
}

// ── CompactionSettings defaults
// ───────────────────────────────────────────────

#[test]
fn default_settings_have_verbatim_window() {
    let s = CompactionSettings::default();
    assert!(s.enabled);
    assert!(s.verbatim_window_tokens > 0, "default should have a non-zero verbatim window");
    assert!(
        s.verbatim_window_tokens < s.keep_recent_tokens,
        "verbatim window must be smaller than the keep budget"
    );
}
