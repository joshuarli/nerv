/// Full-screen session tree selector for /tree.
///
/// Implements [`FullscreenList`] so it can be driven by [`run_fullscreen_picker`].
///
/// Features per spec (docs/tree.md):
/// - ↑/↓ navigate depth-first
/// - ←/→ or PgUp/PgDn page
/// - Ctrl+←/Alt+← fold current node; if already folded jump to prev branch start
/// - Ctrl+→/Alt+→ unfold current node; jump to next branch start or branch end
/// - Enter selects
/// - Ctrl+U toggle: user messages only
/// - Ctrl+O toggle: show all (including label/custom)
/// - ⊟/⊞ fold indicators, `← active` marker, `•` active-path indicator
use std::collections::HashSet;
use std::io::Write;

use super::fullscreen_picker::FullscreenList;
use super::theme;
use crate::session::types::SessionTreeNode;
use crate::tui::keys;

// ─────────────────────────── filter mode ─────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilterMode {
    /// Default: hide label/custom entries, show everything else.
    Default,
    /// Ctrl+U: user messages only.
    UserOnly,
    /// Ctrl+O: show all entry types including label/custom.
    ShowAll,
}

// ─────────────────────────── flat node ───────────────────────────────────────

/// A displayable row in the flat rendering of the tree.
#[derive(Clone)]
struct FlatNode {
    entry_id: String,
    /// First ~80 chars of the entry content.
    summary: String,
    /// Raw text of the entry (for user messages only, placed into editor on select).
    raw_text: String,
    /// True if this is a user message entry.
    is_user: bool,
    has_tool_calls: bool,
    /// Indentation level (0 = root, 1 = child at branching point, …) — kept for debugging.
    #[allow(dead_code)]
    indent: usize,
    /// True if this node has siblings (draws ├─ or └─ connector).
    show_connector: bool,
    /// True if this is the last sibling at its level (uses └─ instead of ├─).
    is_last: bool,
    /// True for non-message entries (compaction, branch_summary, etc.).
    is_meta: bool,
    /// True if this node has children that are currently hidden (folded).
    is_folded: bool,
    /// True if this node is a branching point (has multiple visible children).
    is_branch_start: bool,
    /// Parent entry id, if any.
    parent_id: Option<String>,
    /// True if this node has any visible children (used for fold indicator).
    has_children: bool,
    /// For each ancestor level, whether that level still has more siblings below.
    /// Used to draw │ continuation lines through the prefix.
    ancestors_with_more: Vec<bool>,
}

// ─────────────────────────── selector ────────────────────────────────────────

pub struct TreeSelector {
    /// Full raw tree (never mutated after construction).
    tree: Vec<SessionTreeNode>,
    /// Flat list for current filter + fold state.
    nodes: Vec<FlatNode>,
    selected: usize,
    active_path: HashSet<String>,
    current_leaf_id: Option<String>,
    /// Set of entry IDs that are folded.
    folded: HashSet<String>,
    filter: FilterMode,
}

// ─────────────────────────── public API ──────────────────────────────────────

/// What was selected: entry ID, whether it's a user message, and its raw text.
pub struct TreeSelection {
    pub entry_id: String,
    pub is_user: bool,
    /// Raw text of a user message entry (empty for non-user).
    pub raw_text: String,
    /// Parent entry ID (None for root). Used to set leaf to parent for user msgs.
    pub parent_id: Option<String>,
    /// True if this is the very first message and has no parent.
    pub is_root: bool,
}

impl TreeSelector {
    pub fn new(tree: Vec<SessionTreeNode>, current_leaf_id: Option<String>) -> Self {
        let active_path = build_active_path(&tree, current_leaf_id.as_deref());
        let folded = HashSet::new();
        let nodes = flatten(&tree, &folded, FilterMode::Default);

        // Start selection on the current leaf.
        let selected = current_leaf_id
            .as_deref()
            .and_then(|lid| nodes.iter().position(|n| n.entry_id == lid))
            .unwrap_or(nodes.len().saturating_sub(1));

        Self {
            tree,
            nodes,
            selected,
            active_path,
            current_leaf_id,
            folded,
            filter: FilterMode::Default,
        }
    }

    /// Get a `TreeSelection` for the currently selected node.
    pub fn selected_node(&self) -> Option<TreeSelection> {
        let n = self.nodes.get(self.selected)?;
        let is_root = n.parent_id.is_none();
        Some(TreeSelection {
            entry_id: n.entry_id.clone(),
            is_user: n.is_user,
            raw_text: n.raw_text.clone(),
            parent_id: n.parent_id.clone(),
            is_root,
        })
    }

    // ── private helpers ───────────────────────────────────────────────────────

    fn rebuild(&mut self) {
        let prev_id = self.nodes.get(self.selected).map(|n| n.entry_id.clone());
        self.nodes = flatten(&self.tree, &self.folded, self.filter);
        // Try to keep selection on the same entry; fall back to clamped position.
        if let Some(id) = prev_id {
            if let Some(pos) = self.nodes.iter().position(|n| n.entry_id == id) {
                self.selected = pos;
                return;
            }
        }
        self.selected = self.selected.min(self.nodes.len().saturating_sub(1));
    }

    fn toggle_fold(&mut self) {
        let Some(node) = self.nodes.get(self.selected) else { return };
        if !node.has_children {
            // Not foldable — jump to previous branch start instead.
            self.jump_prev_branch();
            return;
        }
        let id = node.entry_id.clone();
        if node.is_folded {
            self.folded.remove(&id);
        } else {
            self.folded.insert(id);
        }
        self.rebuild();
    }

    fn unfold_or_jump_next(&mut self) {
        let Some(node) = self.nodes.get(self.selected) else { return };
        if node.is_folded {
            let id = node.entry_id.clone();
            self.folded.remove(&id);
            self.rebuild();
        } else {
            self.jump_next_branch();
        }
    }

    /// Jump selection to the previous visible branch-segment start.
    fn jump_prev_branch(&mut self) {
        let start = self.selected;
        let mut i = start.saturating_sub(1);
        loop {
            if self.nodes[i].is_branch_start && i != start {
                self.selected = i;
                return;
            }
            if i == 0 { break; }
            i -= 1;
        }
    }

    /// Jump selection to the next visible branch-segment start, or branch end.
    fn jump_next_branch(&mut self) {
        let start = self.selected;
        for i in (start + 1)..self.nodes.len() {
            if self.nodes[i].is_branch_start {
                self.selected = i;
                return;
            }
        }
        // No further branch point — jump to the very last node.
        if !self.nodes.is_empty() {
            self.selected = self.nodes.len() - 1;
        }
    }

    fn set_filter(&mut self, f: FilterMode) {
        self.filter = f;
        self.folded.clear(); // reset folds on filter change
        self.rebuild();
    }
}

// ──────────────────────── FullscreenList impl ────────────────────────────────

impl FullscreenList for TreeSelector {
    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.nodes.len() {
            self.selected += 1;
        }
    }

    fn move_page_up(&mut self) {
        // Use a reasonable page size; exact terminal height isn't critical here.
        self.selected = self.selected.saturating_sub(10);
    }

    fn move_page_down(&mut self) {
        self.selected = (self.selected + 10).min(self.nodes.len().saturating_sub(1));
    }

    // Tree has no text search — these are no-ops.
    fn push_char(&mut self, _ch: char) {}
    fn pop_char(&mut self) {}
    fn clear_query(&mut self) {}

    fn enter(&self) -> Option<String> {
        self.nodes.get(self.selected).map(|n| n.entry_id.clone())
    }

    fn handle_extra_key(&mut self, seq: &[u8]) -> bool {
        if keys::matches_key(seq, "ctrl+left") || keys::matches_key(seq, "alt+left") {
            self.toggle_fold();
            true
        } else if keys::matches_key(seq, "ctrl+right") || keys::matches_key(seq, "alt+right") {
            self.unfold_or_jump_next();
            true
        } else if keys::matches_key(seq, "ctrl+u") {
            let next = match self.filter {
                FilterMode::UserOnly => FilterMode::Default,
                _ => FilterMode::UserOnly,
            };
            self.set_filter(next);
            true
        } else if keys::matches_key(seq, "ctrl+o") {
            let next = match self.filter {
                FilterMode::ShowAll => FilterMode::Default,
                _ => FilterMode::ShowAll,
            };
            self.set_filter(next);
            true
        } else {
            false
        }
    }

    fn render(&self, out: &mut dyn Write, cols: u16, rows: u16) {
        let cols = cols as usize;
        // 1 row for header, 1 row for footer, rest for list.
        let list_rows = (rows as usize).saturating_sub(3).max(1);

        // ── header ─────────────────────────────────────────────────────────
        let filter_label = match self.filter {
            FilterMode::Default => "",
            FilterMode::UserOnly => "  [user only]",
            FilterMode::ShowAll => "  [show all]",
        };
        let title = format!("  Session tree{}", filter_label);
        let hint = "↑↓ navigate · Enter select · ^U user-only · ^O show-all · Esc cancel  ";
        let gap = cols.saturating_sub(title.len() + hint.len());
        let _ = write!(
            out,
            "\x1b[H{bold}{title}{reset}{muted}{gap}{hint}{reset}\r\n",
            bold = theme::BOLD,
            reset = theme::RESET,
            muted = theme::MUTED,
            title = title,
            gap = " ".repeat(gap),
            hint = hint,
        );

        // ── scroll window: center selected ─────────────────────────────────
        let half = list_rows / 2;
        let scroll_offset = if self.selected <= half {
            0
        } else if self.selected + half >= self.nodes.len() {
            self.nodes.len().saturating_sub(list_rows)
        } else {
            self.selected - half
        };

        let end = (scroll_offset + list_rows).min(self.nodes.len());

        for i in scroll_offset..end {
            let node = &self.nodes[i];
            let is_selected = i == self.selected;
            let is_current = self.current_leaf_id.as_deref() == Some(&node.entry_id);
            let is_active = self.active_path.contains(&node.entry_id);

            // ── indent + connector ─────────────────────────────────────────
            // Each indent level is 2 chars wide.
            // Ancestor levels draw "│ " if that level still has more siblings,
            // or "  " if it was the last sibling at that level.
            // At the node itself: "├─" / "└─" / (nothing for root).
            let mut prefix = String::new();
            if node.show_connector {
                // Draw continuation lines for ancestor levels.
                for &more in &node.ancestors_with_more {
                    prefix.push_str(if more { "│ " } else { "  " });
                }
                // Draw this node's own connector.
                prefix.push_str(if node.is_last { "└─" } else { "├─" });
            }

            // ── fold indicator ──────────────────────────────────────────────
            // ⊟ = foldable (has visible children), ⊞ = folded
            // A '•' after fold indicator means this node is on the active path.
            let fold_indicator = if node.is_folded {
                "⊞"
            } else if node.has_children && node.is_branch_start {
                "⊟"
            } else {
                ""
            };
            let active_dot = if is_active && !fold_indicator.is_empty() { "•" } else { "" };

            // ── role label ─────────────────────────────────────────────────
            let (label, style) = if node.is_meta {
                ("[compaction]", theme::DIM)
            } else if node.is_user {
                ("user:", theme::ACCENT)
            } else if node.has_tool_calls {
                ("assistant:", theme::TOOL_NAME)
            } else {
                ("assistant:", "")
            };

            let raw = if node.summary.is_empty() { "(empty)" } else { &node.summary };
            // Extra: prefix + " label \"...\"" + fold + " ← active"
            let extra = prefix.len() + 1 + label.len() + 3 + fold_indicator.len() + active_dot.len() + 10;
            let content_width = cols.saturating_sub(extra);
            let trimmed = char_truncate(raw, content_width);
            // Non-meta summaries are wrapped in quotes.
            let quoted = if node.is_meta {
                trimmed.to_string()
            } else {
                format!("\"{}\"", trimmed)
            };

            let active_suffix = if is_current { " ← active" } else { "" };
            let fold_part = format!("{}{}", fold_indicator, active_dot);

            if is_selected {
                let _ = write!(
                    out,
                    "{rev}{prefix} {label} {fold}{quoted}{active}{reset}\x1b[K\r\n",
                    rev = theme::REVERSE, reset = theme::RESET,
                    prefix = prefix, label = label, fold = fold_part,
                    quoted = quoted, active = active_suffix,
                );
            } else if is_current {
                let _ = write!(
                    out,
                    "{bold}{prefix} {label} {fold}{quoted}{active}{reset}\x1b[K\r\n",
                    bold = theme::ACCENT_BOLD, reset = theme::RESET,
                    prefix = prefix, label = label, fold = fold_part,
                    quoted = quoted, active = active_suffix,
                );
            } else if is_active {
                let _ = write!(
                    out,
                    "{style}{prefix} {label} {fold}{quoted}{active}{reset}\x1b[K\r\n",
                    style = style, reset = theme::RESET,
                    prefix = prefix, label = label, fold = fold_part,
                    quoted = quoted, active = active_suffix,
                );
            } else {
                let _ = write!(
                    out,
                    "{muted}{prefix} {label} {fold}{quoted}{active}{reset}\x1b[K\r\n",
                    muted = theme::MUTED, reset = theme::RESET,
                    prefix = prefix, label = label, fold = fold_part,
                    quoted = quoted, active = active_suffix,
                );
            }
        }

        // Pad remaining rows so stale content is erased.
        for _ in end..scroll_offset + list_rows {
            let _ = write!(out, "\x1b[K\r\n");
        }

        // ── footer with scroll indicator ────────────────────────────────────
        let fold_hint = if !self.folded.is_empty() {
            format!("  [^←fold ^→unfold]  {}/{}", self.selected + 1, self.nodes.len())
        } else {
            format!("  ^←/^→ fold/jump  {}/{}", self.selected + 1, self.nodes.len())
        };
        let _ = write!(
            out, "{muted}{hint}{reset}\x1b[K\r\n",
            muted = theme::DIM, reset = theme::RESET, hint = fold_hint,
        );
    }
}

// ─────────────────────────── tree helpers ────────────────────────────────────

/// Walk from current leaf to root, collecting IDs on the active path.
fn build_active_path(tree: &[SessionTreeNode], leaf_id: Option<&str>) -> HashSet<String> {
    let Some(leaf_id) = leaf_id else {
        return HashSet::new();
    };

    let mut parent_of: std::collections::HashMap<&str, Option<&str>> =
        std::collections::HashMap::new();

    fn index_tree<'a>(
        node: &'a SessionTreeNode,
        parent_id: Option<&'a str>,
        map: &mut std::collections::HashMap<&'a str, Option<&'a str>>,
    ) {
        map.insert(&node.entry_id, parent_id);
        for child in &node.children {
            index_tree(child, Some(&node.entry_id), map);
        }
    }

    for root in tree {
        index_tree(root, None, &mut parent_of);
    }

    let mut path = HashSet::new();
    let mut current: Option<&str> = Some(leaf_id);
    while let Some(id) = current {
        path.insert(id.to_string());
        current = parent_of.get(id).copied().flatten();
    }
    path
}

/// Determine whether a node should be shown for the given filter mode.
fn should_show(entry_type: &str, is_user: bool, filter: FilterMode) -> bool {
    match filter {
        FilterMode::UserOnly => is_user,
        FilterMode::Default => !matches!(entry_type, "system_prompt" | "tool_result" | "custom_message" | "label"),
        FilterMode::ShowAll => entry_type != "system_prompt" && entry_type != "tool_result",
    }
}

/// Flatten tree into natural depth-first display order (root first), respecting fold + filter.
///
/// Each level of indentation is 2 chars. Children of a branching node (multiple visible
/// children) get `indent+1`; single-child chains stay at the same indent so linear
/// conversations don't waste horizontal space.
///
/// `ancestor_has_more`: a bitmask / Vec tracking which ancestor levels still have
/// siblings below them, so we can draw `│` continuation lines correctly.
fn flatten(
    tree: &[SessionTreeNode],
    folded: &HashSet<String>,
    filter: FilterMode,
) -> Vec<FlatNode> {
    let mut result = Vec::new();

    // Stack entry: (node, indent, ancestors_with_more: Vec<bool>, is_last, parent_id)
    // ancestors_with_more[i] = true means level i still has more siblings below.
    let mut stack: Vec<(&SessionTreeNode, usize, Vec<bool>, bool, Option<String>)> = Vec::new();

    let roots: Vec<&SessionTreeNode> = tree.iter().collect();
    // Push roots in reverse so we pop them in order (first root = first displayed).
    for (i, root) in roots.iter().enumerate().rev() {
        stack.push((root, 0, vec![], i != roots.len() - 1, None));
    }

    while let Some((node, indent, ancestors_with_more, has_more_siblings, parent_id)) = stack.pop() {
        let entry_type = node.entry_type.as_str();
        let is_meta = matches!(
            entry_type,
            "compaction" | "branch_summary" | "model_change" | "thinking_change" | "label"
                | "session_info" | "permission_accept"
        );

        let visible = should_show(entry_type, node.is_user, filter);

        // Collect visible children sorted by timestamp (natural order).
        let visible_children: Vec<&SessionTreeNode> = node
            .children
            .iter()
            .filter(|c| has_visible(c, filter))
            .collect();

        let has_children = !visible_children.is_empty();
        let is_folded = folded.contains(&node.entry_id);
        let is_branch_start = visible_children.len() > 1;

        if visible {
            result.push(FlatNode {
                entry_id: node.entry_id.clone(),
                summary: node.summary.clone(),
                raw_text: node.raw_text.clone(),
                is_user: node.is_user,
                has_tool_calls: node.has_tool_calls,
                indent,
                // show_connector = true when the parent had multiple children (we drew a branch)
                show_connector: !ancestors_with_more.is_empty(),
                is_last: !has_more_siblings,
                is_meta,
                is_folded,
                is_branch_start,
                parent_id: parent_id.clone(),
                has_children,
                // Pass through the ancestor continuation flags for │ lines.
                ancestors_with_more: ancestors_with_more.clone(),
            });
        }

        if is_folded {
            continue;
        }

        let multi = visible_children.len() > 1;

        // Indentation rules:
        // - Hidden nodes (system_prompt etc.) are transparent — pass through
        //   indent and ancestors unchanged so they don't add visual depth.
        // - Visible nodes always indent their children one level deeper,
        //   whether branching or not. Gives a nested tree appearance.
        // - Only at branch points do we record a │ continuation flag.
        let (child_indent, child_ancestors) = if !visible {
            // Hidden: fully transparent.
            (indent, ancestors_with_more.clone())
        } else if multi {
            // Branch point: record whether this level has more siblings so
            // │ continuation lines are drawn at child levels.
            let mut a = ancestors_with_more.clone();
            a.push(has_more_siblings);
            (indent + 1, a)
        } else {
            // Single visible child: increase indent, no │ continuation.
            let mut a = ancestors_with_more.clone();
            a.push(false);
            (indent + 1, a)
        };

        // Push in reverse order so first child is popped first.
        for (i, child) in visible_children.iter().enumerate().rev() {
            let is_last_child = i == visible_children.len() - 1;
            stack.push((
                child,
                child_indent,
                child_ancestors.clone(),
                !is_last_child,
                Some(node.entry_id.clone()),
            ));
        }
    }

    result
}

/// True if a subtree has at least one visible node for the given filter.
fn has_visible(node: &SessionTreeNode, filter: FilterMode) -> bool {
    if should_show(node.entry_type.as_str(), node.is_user, filter) {
        return true;
    }
    node.children.iter().any(|c| has_visible(c, filter))
}

/// Truncate a string to at most `max` chars at a char boundary.
fn char_truncate(s: &str, max: usize) -> &str {
    &s[..s.floor_char_boundary(max)]
}
