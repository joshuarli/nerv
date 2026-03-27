use crate::tui::keys;
use crate::tui::tui::Component;
use crate::tui::utils;

pub struct SelectItem {
    pub label: String,
    pub selectable: bool,
}

impl SelectItem {
    pub fn item(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            selectable: true,
        }
    }

    pub fn header(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            selectable: false,
        }
    }
}

pub struct SelectList {
    items: Vec<SelectItem>,
    selected: usize,
    scroll_offset: usize,
    max_visible: usize,
    header_style: fn(&str) -> String,
    selected_style: fn(&str) -> String,
    normal_style: fn(&str) -> String,
}

impl SelectList {
    pub fn new(items: Vec<SelectItem>) -> Self {
        let first_selectable = items.iter().position(|i| i.selectable).unwrap_or(0);
        Self {
            items,
            selected: first_selectable,
            scroll_offset: 0,
            max_visible: 20,
            header_style: |s| format!("\x1b[1;38;5;245m{}\x1b[0m", s),
            selected_style: |s| format!("\x1b[7m {}\x1b[0m", s),
            normal_style: |s| format!("  {}", s),
        }
    }

    pub fn set_items(&mut self, items: Vec<SelectItem>) {
        self.items = items;
        self.selected = self.items.iter().position(|i| i.selectable).unwrap_or(0);
        self.scroll_offset = 0;
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn selected_label(&self) -> Option<&str> {
        self.items.get(self.selected).map(|i| i.label.as_str())
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.items.len() as i32;
        if len == 0 {
            return;
        }

        let mut next = self.selected as i32 + delta;
        // Skip non-selectable items
        loop {
            if next < 0 {
                next = 0;
                break;
            }
            if next >= len {
                next = len - 1;
                break;
            }
            if self.items[next as usize].selectable {
                break;
            }
            next += delta.signum();
        }

        if (0..len).contains(&next) && self.items[next as usize].selectable {
            self.selected = next as usize;
            // Adjust scroll
            if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            }
            if self.selected >= self.scroll_offset + self.max_visible {
                self.scroll_offset = self.selected - self.max_visible + 1;
            }
        }
    }
}

impl Component for SelectList {
    fn render(&self, width: u16) -> Vec<String> {
        let visible_end = (self.scroll_offset + self.max_visible).min(self.items.len());
        let visible = &self.items[self.scroll_offset..visible_end];

        visible
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let global_idx = self.scroll_offset + i;
                let truncated = utils::truncate_to_width(&item.label, width.saturating_sub(3));
                if !item.selectable {
                    (self.header_style)(&truncated)
                } else if global_idx == self.selected {
                    (self.selected_style)(&truncated)
                } else {
                    (self.normal_style)(&truncated)
                }
            })
            .collect()
    }

    fn handle_input(&mut self, input: &[u8]) -> bool {
        if keys::matches_key(input, "up") {
            self.move_selection(-1);
            true
        } else if keys::matches_key(input, "down") || keys::matches_key(input, "tab") {
            self.move_selection(1);
            true
        } else {
            false
        }
    }
}
