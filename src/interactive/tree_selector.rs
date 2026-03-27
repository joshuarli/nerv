use std::collections::HashSet;

use super::theme;
use crate::session::types::SessionTreeNode;

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
    scroll_offset: usize,
    active_path: HashSet<String>,
    current_leaf_id: Option<String>,
}

impl TreeSelector {
    pub fn new(tree: Vec<SessionTreeNode>, current_leaf_id: Option<String>) -> Self {
        let active_path = build_active_path(&tree, current_leaf_id.as_deref());
        let nodes = flatten(&tree, &active_path);

        // Start selection on the current leaf
        let selected = current_leaf_id
            .as_deref()
            .and_then(|lid| nodes.iter().position(|n| n.entry_id == lid))
            .unwrap_or(nodes.len().saturating_sub(1));

        Self {
            nodes,
            selected,
            scroll_offset: 0,
            active_path,
            current_leaf_id,
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.adjust_scroll();
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.nodes.len() {
            self.selected += 1;
            self.adjust_scroll();
        }
    }

    pub fn selected_entry_id(&self) -> Option<&str> {
        self.nodes.get(self.selected).map(|n| n.entry_id.as_str())
    }

    fn adjust_scroll(&mut self) {
        let max_visible = 20;
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
        if self.selected >= self.scroll_offset + max_visible {
            self.scroll_offset = self.selected - max_visible + 1;
        }
    }

    pub fn render_lines(&self) -> Vec<String> {
        let max_visible = 20;
        let mut lines = Vec::new();

        lines.push(format!(
            "{}Session tree (↑↓ navigate, Enter select, Esc cancel):{}",
            theme::MUTED,
            theme::RESET,
        ));

        let end = (self.scroll_offset + max_visible).min(self.nodes.len());
        for i in self.scroll_offset..end {
            let node = &self.nodes[i];
            let mut line = String::new();

            // Build indent + connectors
            line.push(' ');
            if node.indent > 0 {
                // Simple indent with connector
                let base_indent = (node.indent - 1) * 3;
                for _ in 0..base_indent {
                    line.push(' ');
                }
                if node.show_connector {
                    if node.is_last {
                        line.push_str("└─ ");
                    } else {
                        line.push_str("├─ ");
                    }
                } else {
                    line.push_str("   ");
                }
            }

            // Role marker + summary
            let (marker, style) = if node.is_meta {
                ("~", theme::DIM)
            } else if node.is_user {
                (">", theme::ACCENT)
            } else if node.has_tool_calls {
                ("⚙", theme::TOOL_NAME)
            } else {
                ("●", "")
            };

            let is_active = self.active_path.contains(&node.entry_id);
            let is_current = self.current_leaf_id.as_deref() == Some(&node.entry_id);
            let is_selected = i == self.selected;

            let summary = if node.summary.is_empty() {
                "(empty)".to_string()
            } else {
                node.summary.clone()
            };

            if is_selected {
                line = format!(
                    "{}{} {} {}{}",
                    theme::REVERSE,
                    line,
                    marker,
                    summary,
                    theme::RESET,
                );
            } else if is_current {
                line = format!(
                    "{}{} {} {} ◀{}",
                    theme::ACCENT_BOLD,
                    line,
                    marker,
                    summary,
                    theme::RESET,
                );
            } else if is_active {
                line = format!(
                    "{}{} {} {}{}",
                    style,
                    line,
                    marker,
                    summary,
                    theme::RESET,
                );
            } else {
                line = format!(
                    "{}{} {} {}{}",
                    theme::MUTED,
                    line,
                    marker,
                    summary,
                    theme::RESET,
                );
            }

            lines.push(line);
        }

        if self.nodes.len() > max_visible {
            lines.push(format!(
                "{} ({}/{}){}", theme::DIM, self.selected + 1, self.nodes.len(), theme::RESET
            ));
        }

        lines
    }
}

/// Walk from current leaf to root, collecting IDs on the active path.
fn build_active_path(tree: &[SessionTreeNode], leaf_id: Option<&str>) -> HashSet<String> {
    let Some(leaf_id) = leaf_id else {
        return HashSet::new();
    };

    // Build id→parent map from the tree
    let mut parent_of: std::collections::HashMap<&str, Option<&str>> =
        std::collections::HashMap::new();

    fn index_tree<'a>(
        node: &'a SessionTreeNode,
        parent_id: Option<&'a str>,
        parent_of: &mut std::collections::HashMap<&'a str, Option<&'a str>>,
    ) {
        parent_of.insert(&node.entry_id, parent_id);
        for child in &node.children {
            index_tree(child, Some(&node.entry_id), parent_of);
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

/// Flatten tree into display order, prioritizing the active branch.
fn flatten(tree: &[SessionTreeNode], active_path: &HashSet<String>) -> Vec<FlatNode> {
    let mut result = Vec::new();

    // Stack: (node, indent, show_connector, is_last)
    let mut stack: Vec<(&SessionTreeNode, usize, bool, bool)> = Vec::new();

    // Push roots in reverse (so first root is processed first)
    let roots: Vec<&SessionTreeNode> = tree.iter().collect();
    for (i, root) in roots.iter().enumerate().rev() {
        let multi = roots.len() > 1;
        stack.push((root, if multi { 1 } else { 0 }, multi, i == roots.len() - 1));
    }

    while let Some((node, indent, show_connector, is_last)) = stack.pop() {
        // Skip system_prompt and tool_result entries — too noisy
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

        // Sort children: active branch first, then by timestamp
        let mut children: Vec<&SessionTreeNode> = node.children.iter().collect();
        children.sort_by(|a, b| {
            let a_active = contains_active(a, active_path);
            let b_active = contains_active(b, active_path);
            b_active.cmp(&a_active).then(a.timestamp.cmp(&b.timestamp))
        });

        let multi = children.len() > 1;
        let child_indent = if multi { indent + 1 } else { indent };

        // Push in reverse so first child is popped first
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
