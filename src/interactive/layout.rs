use crate::interactive::btw_panel::BtwPanel;
use crate::interactive::chat_writer::ChatWriter;
use crate::interactive::footer::FooterComponent;
use crate::interactive::statusbar::StatusBar;
use crate::tui::Component;
use crate::tui::components::editor::Editor;
use crate::tui::components::spacer::Spacer;

pub struct AppLayout {
    spacer_top: Spacer,
    pub chat: ChatWriter,
    /// Inline /btw answer panel rendered just above the editor.
    pub btw_panel: Option<BtwPanel>,
    pub editor: Editor,
    pub statusbar: StatusBar,
    pub footer: FooterComponent,
    /// Cached count of fixed-bottom lines from the last render.
    cached_fixed: std::cell::Cell<usize>,
}

impl AppLayout {
    pub fn new(editor: Editor, statusbar: StatusBar, footer: FooterComponent) -> Self {
        Self {
            spacer_top: Spacer::new(1),
            chat: ChatWriter::new(),
            btw_panel: None,
            editor,
            statusbar,
            footer,
            cached_fixed: std::cell::Cell::new(10),
        }
    }

    /// Total lines in the fixed bottom area (queue + btw panel + editor +
    /// statusbar + footer).  Updated each time `render()` runs; the value
    /// is the actual line count from the most recent frame.
    pub fn fixed_bottom_lines(&self) -> usize {
        self.cached_fixed.get()
    }

    /// Render just the fixed UI (statusbar queue, btw panel, editor, statusbar,
    /// footer).
    pub fn render_fixed(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        lines.extend(self.statusbar.render_queue(width));
        if let Some(panel) = &self.btw_panel {
            lines.extend(panel.render(width));
        }
        lines.extend(self.editor.render(width));
        lines.extend(self.statusbar.render(width));
        lines.extend(self.footer.render(width));
        lines
    }
}

impl Component for AppLayout {
    fn render(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        lines.extend(self.spacer_top.render(width));
        lines.extend(self.chat.render(width));
        if let Some(last) = lines.last_mut()
            && !last.is_empty()
            && !last.ends_with(crate::interactive::theme::RESET)
        {
            last.push_str(crate::interactive::theme::RESET);
        }
        // --- fixed bottom area starts here ---
        let fixed_start = lines.len();
        lines.extend(self.statusbar.render_queue(width));
        // Render the btw panel (if open) between the chat and the editor.
        if let Some(panel) = &self.btw_panel {
            lines.extend(panel.render(width));
        }
        lines.extend(self.editor.render(width));
        lines.extend(self.statusbar.render(width));
        lines.extend(self.footer.render(width));
        self.cached_fixed.set(lines.len() - fixed_start);
        lines
    }
}
