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
    /// Indentation level (0 = root, 1 = child, 2 = grandchild …)
    indent: usize,
    /// True if the connector line should show ├─/└─ (vs continuing vertically).
    show_connector: bool,
    /// True if this is the last sibling at its level (uses └─ instead of ├─).
    is_last: bool,
    /// True for non-message entries (compaction, branch_summary, etc.).
    is_meta: bool,
    /// True if this node has children that are currently hidden (folded).
    is_folded: bool,
    /// True if this node is a branch-segment start (has siblings = branching point or root with multi-children).
    is_branch_start: bool,
    /// Parent entry id, if any.
    parent_id: Option<String>,
    /// True if this node has any visible children (used for fold indicator).
    has_children: bool,
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
        let nodes = flatten(&tree, &active_path, &folded, FilterMode::Default);

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
        self.nodes = flatten(&self.tree, &self.active_path, &self.folded, self.filter);
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
        // Reserve 2 rows for header + footer; use half-terminal height per spec.
        let list_rows = (rows as usize / 2).saturating_sub(2).max(4);

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
            // Each indent level is 3 chars wide: "│  " or "   "
            // At the node itself: "├─ " / "└─ " / "   "
            let mut prefix = String::from(" ");
            if node.indent > 0 {
                let base = (node.indent - 1) * 3;
                for _ in 0..base {
                    prefix.push(' ');
                }
                if node.show_connector {
                    prefix.push_str(if node.is_last { "└─ " } else { "├─ " });
                } else {
                    prefix.push_str("   ");
                }
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

            // ── role marker ────────────────────────────────────────────────
            let (marker, style) = if node.is_meta {
                ("~", theme::DIM)
            } else if node.is_user {
                (">", theme::ACCENT)
            } else if node.has_tool_calls {
                ("⚙", theme::TOOL_NAME)
            } else {
                ("●", "")
            };

            let summary = if node.summary.is_empty() { "(empty)" } else { &node.summary };
            // Extra chars: " marker " + fold_indicator + active_dot + " ← active"
            let extra = 3 + fold_indicator.len() + active_dot.len();
            let content_width = cols.saturating_sub(prefix.len() + extra + 10);
            // Safe substring at char boundary
            let summary = char_truncate(summary, content_width);

            let active_suffix = if is_current { " ← active" } else { "" };
            let fold_part = format!("{}{}", fold_indicator, active_dot);

            if is_selected {
                let _ = write!(
                    out,
                    "{rev}{prefix} {marker} {fold}{summary}{active}{reset}\x1b[K\r\n",
                    rev = theme::REVERSE, reset = theme::RESET,
                    prefix = prefix, marker = marker, fold = fold_part,
                    summary = summary, active = active_suffix,
                );
            } else if is_current {
                let _ = write!(
                    out,
                    "{bold}{prefix} {marker} {fold}{summary}{active}{reset}\x1b[K\r\n",
                    bold = theme::ACCENT_BOLD, reset = theme::RESET,
                    prefix = prefix, marker = marker, fold = fold_part,
                    summary = summary, active = active_suffix,
                );
            } else if is_active {
                let _ = write!(
                    out,
                    "{style}{prefix} {marker} {fold}{summary}{active}{reset}\x1b[K\r\n",
                    style = style, reset = theme::RESET,
                    prefix = prefix, marker = marker, fold = fold_part,
                    summary = summary, active = active_suffix,
                );
            } else {
                let _ = write!(
                    out,
                    "{muted}{prefix} {marker} {fold}{summary}{active}{reset}\x1b[K\r\n",
                    muted = theme::MUTED, reset = theme::RESET,
                    prefix = prefix, marker = marker, fold = fold_part,
                    summary = summary, active = active_suffix,
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

/// Flatten tree into display order, active branch first, respecting fold + filter state.
fn flatten(
    tree: &[SessionTreeNode],
    active_path: &HashSet<String>,
    folded: &HashSet<String>,
    filter: FilterMode,
) -> Vec<FlatNode> {
    let mut result = Vec::new();

    // Stack entry: (node, indent, show_connector, is_last, parent_id)
    let mut stack: Vec<(&SessionTreeNode, usize, bool, bool, Option<String>)> = Vec::new();

    let roots: Vec<&SessionTreeNode> = tree.iter().collect();
    let multi_root = roots.len() > 1;
    for (i, root) in roots.iter().enumerate().rev() {
        stack.push((root, if multi_root { 1 } else { 0 }, multi_root, i == roots.len() - 1, None));
    }

    while let Some((node, indent, show_connector, is_last, parent_id)) = stack.pop() {
        let entry_type = node.entry_type.as_str();
        let is_meta = matches!(
            entry_type,
            "compaction" | "branch_summary" | "model_change" | "thinking_change" | "label"
                | "session_info" | "permission_accept"
        );

        let visible = should_show(entry_type, node.is_user, filter);

        // Collect visible children (sorted: active branch first, then by timestamp).
        let mut children: Vec<&SessionTreeNode> = node.children.iter().collect();
        children.sort_by(|a, b| {
            let a_active = contains_active(a, active_path);
            let b_active = contains_active(b, active_path);
            b_active.cmp(&a_active).then(a.timestamp.cmp(&b.timestamp))
        });
        // Filter children to only those that would produce ≥1 visible descendant.
        let visible_children: Vec<&SessionTreeNode> = children
            .iter()
            .cloned()
            .filter(|c| has_visible(c, filter))
            .collect();

        let has_children = !visible_children.is_empty();
        let is_folded = folded.contains(&node.entry_id);
        // A node is a branch start if it has multiple visible children (branching point)
        // or if it is the first node at the root level and there are multiple roots.
        let is_branch_start = visible_children.len() > 1 || (indent == 0 && multi_root);

        if visible {
            result.push(FlatNode {
                entry_id: node.entry_id.clone(),
                summary: node.summary.clone(),
                raw_text: node.raw_text.clone(),
                is_user: node.is_user,
                has_tool_calls: node.has_tool_calls,
                indent,
                show_connector,
                is_last,
                is_meta,
                is_folded,
                is_branch_start,
                parent_id: parent_id.clone(),
                has_children,
            });
        }

        // If folded, don't push children onto the stack.
        if is_folded {
            continue;
        }

        let multi = visible_children.len() > 1;
        let child_indent = if multi { indent + 1 } else { indent };

        for (i, child) in visible_children.iter().enumerate().rev() {
            stack.push((
                child,
                child_indent,
                multi,
                i == visible_children.len() - 1,
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

fn contains_active(node: &SessionTreeNode, active_path: &HashSet<String>) -> bool {
    if active_path.contains(&node.entry_id) {
        return true;
    }
    node.children
        .iter()
        .any(|c| contains_active(c, active_path))
}

/// Truncate a string to at most `max` chars at a char boundary.
fn char_truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
