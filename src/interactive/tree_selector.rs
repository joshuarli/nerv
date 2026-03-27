/// Full-screen session tree selector for /tree.
///
/// Implements [`FullscreenList`] so it can be driven by [`run_fullscreen_picker`].
use std::collections::HashSet;
use std::io::Write;

use super::fullscreen_picker::FullscreenList;
use super::theme;
use crate::session::types::SessionTreeNode;

// ─────────────────────────── types ──────────────────────────────────────────

struct FlatNode {
    entry_id: String,
    summary: String,
    is_user: bool,
    has_tool_calls: bool,
    indent: usize,
    show_connector: bool,
    is_last: bool,
    /// True for non-message entries we still display (compaction, branch summary).
    is_meta: bool,
}

pub struct TreeSelector {
    nodes: Vec<FlatNode>,
    selected: usize,
    active_path: HashSet<String>,
    current_leaf_id: Option<String>,
}

// ─────────────────────────── impl ───────────────────────────────────────────

impl TreeSelector {
    pub fn new(tree: Vec<SessionTreeNode>, current_leaf_id: Option<String>) -> Self {
        let active_path = build_active_path(&tree, current_leaf_id.as_deref());
        let nodes = flatten(&tree, &active_path);

        // Start selection on the current leaf.
        let selected = current_leaf_id
            .as_deref()
            .and_then(|lid| nodes.iter().position(|n| n.entry_id == lid))
            .unwrap_or(nodes.len().saturating_sub(1));

        Self {
            nodes,
            selected,
            active_path,
            current_leaf_id,
        }
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

    // Tree has no text search — these are no-ops.
    fn push_char(&mut self, _ch: char) {}
    fn pop_char(&mut self) {}
    fn clear_query(&mut self) {}

    fn enter(&self) -> Option<String> {
        self.nodes.get(self.selected).map(|n| n.entry_id.clone())
    }

    fn render(&self, out: &mut dyn Write, cols: u16, rows: u16) {
        let cols = cols as usize;
        let list_rows = (rows as usize).saturating_sub(2); // header + footer

        // ── header ─────────────────────────────────────────────────────────
        let title = "  Session tree";
        let hint = "↑↓ navigate · Enter select · Esc cancel  ";
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

        // ── scroll window ──────────────────────────────────────────────────
        // Keep selected in the center of the viewport when possible.
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
            let mut prefix = String::from(" ");
            if node.indent > 0 {
                let base_indent = (node.indent - 1) * 3;
                for _ in 0..base_indent {
                    prefix.push(' ');
                }
                if node.show_connector {
                    prefix.push_str(if node.is_last { "└─ " } else { "├─ " });
                } else {
                    prefix.push_str("   ");
                }
            }

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
            // Truncate so we don't wrap.
            let content_width = cols.saturating_sub(prefix.len() + 2 /* " X " */);
            let summary = &summary[..summary.len().min(content_width)];

            if is_selected {
                let _ = write!(
                    out, "{rev}{prefix} {marker} {summary}{reset}\x1b[K\r\n",
                    rev = theme::REVERSE, reset = theme::RESET,
                    prefix = prefix, marker = marker, summary = summary,
                );
            } else if is_current {
                let _ = write!(
                    out, "{bold}{prefix} {marker} {summary} ◀{reset}\x1b[K\r\n",
                    bold = theme::ACCENT_BOLD, reset = theme::RESET,
                    prefix = prefix, marker = marker, summary = summary,
                );
            } else if is_active {
                let _ = write!(
                    out, "{style}{prefix} {marker} {summary}{reset}\x1b[K\r\n",
                    style = style, reset = theme::RESET,
                    prefix = prefix, marker = marker, summary = summary,
                );
            } else {
                let _ = write!(
                    out, "{muted}{prefix} {marker} {summary}{reset}\x1b[K\r\n",
                    muted = theme::MUTED, reset = theme::RESET,
                    prefix = prefix, marker = marker, summary = summary,
                );
            }
        }

        // ── scroll indicator ───────────────────────────────────────────────
        if self.nodes.len() > list_rows {
            let _ = write!(
                out, "{muted}  {sel}/{total}{reset}\x1b[K\r\n",
                muted = theme::DIM, reset = theme::RESET,
                sel = self.selected + 1, total = self.nodes.len(),
            );
        }
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

/// Flatten tree into display order, active branch first.
fn flatten(tree: &[SessionTreeNode], active_path: &HashSet<String>) -> Vec<FlatNode> {
    let mut result = Vec::new();

    // Stack: (node, indent, show_connector, is_last)
    let mut stack: Vec<(&SessionTreeNode, usize, bool, bool)> = Vec::new();

    let roots: Vec<&SessionTreeNode> = tree.iter().collect();
    for (i, root) in roots.iter().enumerate().rev() {
        let multi = roots.len() > 1;
        stack.push((root, if multi { 1 } else { 0 }, multi, i == roots.len() - 1));
    }

    while let Some((node, indent, show_connector, is_last)) = stack.pop() {
        // Skip noisy entry types.
        let dominated = matches!(
            node.entry_type.as_str(),
            "system_prompt" | "tool_result" | "custom_message"
        );
        let is_meta = matches!(
            node.entry_type.as_str(),
            "compaction" | "branch_summary" | "model_change" | "thinking_change" | "label"
                | "session_info"
        );

        if !dominated {
            result.push(FlatNode {
                entry_id: node.entry_id.clone(),
                summary: node.summary.clone(),
                is_user: node.is_user,
                has_tool_calls: node.has_tool_calls,
                indent,
                show_connector,
                is_last,
                is_meta,
            });
        }

        // Sort children: active branch first, then by timestamp.
        let mut children: Vec<&SessionTreeNode> = node.children.iter().collect();
        children.sort_by(|a, b| {
            let a_active = contains_active(a, active_path);
            let b_active = contains_active(b, active_path);
            b_active.cmp(&a_active).then(a.timestamp.cmp(&b.timestamp))
        });

        let multi = children.len() > 1;
        let child_indent = if multi { indent + 1 } else { indent };

        for (i, child) in children.iter().enumerate().rev() {
            stack.push((child, child_indent, multi, i == children.len() - 1));
        }
    }

    result
}

fn contains_active(node: &SessionTreeNode, active_path: &HashSet<String>) -> bool {
    if active_path.contains(&node.entry_id) {
        return true;
    }
    node.children
        .iter()
        .any(|c| contains_active(c, active_path))
}
