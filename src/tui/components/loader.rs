use crate::tui::tui::Component;

const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Braille spinner. Advances frame on each `render()` call.
pub struct Loader {
    frame: usize,
    label: String,
}

impl Loader {
    pub fn new(label: impl Into<String>) -> Self {
        Self { frame: 0, label: label.into() }
    }

    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = label.into();
    }
}

impl Component for Loader {
    fn render(&self, _width: u16) -> Vec<String> {
        let spinner = FRAMES[self.frame % FRAMES.len()];
        let line = format!("{} {}", spinner, self.label);
        vec![line]
    }

    fn invalidate(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }
}
