use crate::tui::tui::Component;

/// Emits N empty lines.
pub struct Spacer {
    lines: usize,
}

impl Spacer {
    pub fn new(lines: usize) -> Self {
        Self { lines }
    }
}

impl Component for Spacer {
    fn render(&self, _width: u16) -> Vec<String> {
        vec![String::new(); self.lines]
    }
}
