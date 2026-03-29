use crate::interactive::btw_panel::BtwPanel;
use crate::interactive::chat_writer::ChatWriter;
use crate::interactive::footer::FooterComponent;
use crate::interactive::statusbar::StatusBar;
use crate::tui::Component;
use crate::tui::components::editor::Editor;
use crate::tui::components::spacer::Spacer;

/// Fixed lines at the bottom (editor + statusbar + footer) that are never
/// flushed to scrollback.  Queue lines are added on top of this.
pub const BASE_FIXED_BOTTOM: usize = 10;

pub struct AppLayout {
    spacer_top: Spacer,
    pub chat: ChatWriter,
    /// Inline /btw answer panel rendered just above the editor.
    pub btw_panel: Option<BtwPanel>,
    pub editor: Editor,
    pub statusbar: StatusBar,
    pub footer: FooterComponent,
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
        }
    }

    /// Total lines in the fixed footer, including the nervHud line when the
    /// HUD is enabled and the btw panel when it is open.
    pub fn fixed_bottom_lines(&self) -> usize {
        let panel = self
            .btw_panel
            .as_ref()
            .map(|p| p.line_count(80)) // 80 is a safe lower-bound; TUI clips anyway
            .unwrap_or(0);
        BASE_FIXED_BOTTOM + self.footer.hud_line_count() + panel
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
        lines.extend(self.statusbar.render_queue(width));
        // Render the btw panel (if open) between the chat and the editor.
        if let Some(panel) = &self.btw_panel {
            lines.extend(panel.render(width));
        }
        lines.extend(self.editor.render(width));
        lines.extend(self.statusbar.render(width));
        lines.extend(self.footer.render(width));
        lines
    }
}
