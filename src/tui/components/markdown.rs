use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::tui::highlight;
use crate::tui::tui::Component;
use crate::tui::utils::{char_wrap_with_ansi, visible_width, wrap_text_with_ansi};

/// Theme functions for markdown rendering. Each takes raw text and returns
/// ANSI-styled output.
pub struct MarkdownTheme {
    pub heading: fn(&str) -> String,
    pub code: fn(&str) -> String,
    pub code_block: fn(&str) -> String,
    pub code_block_border: fn(&str) -> String,
    pub quote: fn(&str) -> String,
    pub bold: fn(&str) -> String,
    pub italic: fn(&str) -> String,
    pub strikethrough: fn(&str) -> String,
    pub list_bullet: fn(&str) -> String,
    pub hr: fn(&str) -> String,
    pub table_header: fn(&str) -> String,
    pub table_border: fn(&str) -> String,
    pub code_block_indent: &'static str,
}

impl Default for MarkdownTheme {
    fn default() -> Self {
        Self {
            heading: |s| format!("\x1b[1;37m{}\x1b[0m", s),
            code: |s| format!("\x1b[48;5;236m{}\x1b[0m", s),
            code_block: |s| format!("  \x1b[38;5;252m{}\x1b[0m", s),
            code_block_border: |s| format!("\x1b[38;5;240m{}\x1b[0m", s),
            quote: |s| format!("\x1b[38;5;245m│ {}\x1b[0m", s),
            bold: |s| format!("\x1b[1m{}\x1b[22m", s),
            italic: |s| format!("\x1b[3m{}\x1b[23m", s),
            strikethrough: |s| format!("\x1b[9m{}\x1b[29m", s),
            list_bullet: |s| format!("\x1b[38;5;245m{}\x1b[0m", s),
            hr: |s| format!("\x1b[38;5;240m{}\x1b[0m", s),
            table_header: |s| format!("\x1b[1;37m{}\x1b[0m", s),
            table_border: |s| format!("\x1b[38;5;240m{}\x1b[0m", s),
            code_block_indent: "  ",
        }
    }
}

/// Transient state for building a table as we parse its events.
struct TableState {
    alignments: Vec<Alignment>,
    rows: Vec<TableRow>,
    current_row: Vec<String>,
    current_row_is_header: bool,
    in_header: bool,
}

struct TableRow {
    cells: Vec<String>,
    is_header: bool,
}

pub struct Markdown {
    text: String,
    padding_x: u16,
    padding_y: u16,
    theme: MarkdownTheme,
    cached: Option<(u16, Vec<String>)>,
}

impl Markdown {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            padding_x: 1,
            padding_y: 0,
            theme: MarkdownTheme::default(),
            cached: None,
        }
    }

    pub fn with_theme(mut self, theme: MarkdownTheme) -> Self {
        self.theme = theme;
        self
    }

    pub fn with_padding(mut self, x: u16, y: u16) -> Self {
        self.padding_x = x;
        self.padding_y = y;
        self
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cached = None;
    }

    pub fn append_text(&mut self, text: &str) {
        self.text.push_str(text);
        self.cached = None;
    }

    fn render_markdown(&self, width: u16) -> Vec<String> {
        let content_width = width.saturating_sub(self.padding_x * 2);
        if content_width == 0 {
            return vec![];
        }

        let padding = " ".repeat(self.padding_x as usize);
        let mut opts = Options::empty();
        opts.insert(Options::ENABLE_TABLES);
        let parser = Parser::new_ext(&self.text, opts);
        let mut lines = Vec::new();
        let mut current_text = String::new();
        let mut in_code_block = false;
        let mut code_lang: Option<String> = None;
        let mut hl_state = highlight::HlState::Normal;
        let mut in_blockquote = false;
        let mut in_bold = false;
        let mut in_italic = false;
        let mut in_strikethrough = false;
        let mut list_depth: usize = 0;
        let mut ordered_index: Option<u64> = None;
        let mut table_state: Option<TableState> = None;

        // Add top padding
        for _ in 0..self.padding_y {
            lines.push(String::new());
        }

        for event in parser {
            match event {
                Event::Start(Tag::Heading { .. }) => {
                    flush_text(&mut current_text, &mut lines, content_width, &padding);
                }
                Event::End(TagEnd::Heading(_)) => {
                    let styled = (self.theme.heading)(&current_text);
                    lines.push(format!("{}{}", padding, styled));
                    current_text.clear();
                    lines.push(String::new());
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    flush_text(&mut current_text, &mut lines, content_width, &padding);
                    code_lang = match kind {
                        CodeBlockKind::Fenced(ref lang) if !lang.is_empty() => {
                            Some(lang.to_string())
                        }
                        _ => None,
                    };
                    hl_state = highlight::HlState::Normal;
                    let border =
                        (self.theme.code_block_border)(&"─".repeat(content_width as usize));
                    lines.push(format!("{}{}", padding, border));
                    in_code_block = true;
                }
                Event::End(TagEnd::CodeBlock) => {
                    if !current_text.is_empty() {
                        let rules = code_lang.as_deref().and_then(highlight::rules_for_lang);
                        // Code blocks are character-wrapped (not word-wrapped) so
                        // long lines are fully visible without breaking indentation.
                        let code_indent = 2u16;
                        let code_width = content_width.saturating_sub(code_indent);
                        for code_line in current_text.lines() {
                            let styled = if let Some(r) = rules {
                                highlight::highlight_line(code_line, &mut hl_state, r)
                            } else {
                                (self.theme.code_block)(code_line)
                            };
                            for wrapped in char_wrap_with_ansi(&styled, code_width) {
                                if rules.is_some() {
                                    lines.push(format!("{}  {}", padding, wrapped));
                                } else {
                                    lines.push(format!("{}{}", padding, wrapped));
                                }
                            }
                        }
                        current_text.clear();
                    }
                    code_lang = None;
                    let border =
                        (self.theme.code_block_border)(&"─".repeat(content_width as usize));
                    lines.push(format!("{}{}", padding, border));
                    in_code_block = false;
                }
                Event::Start(Tag::BlockQuote(_)) => {
                    flush_text(&mut current_text, &mut lines, content_width, &padding);
                    in_blockquote = true;
                }
                Event::End(TagEnd::BlockQuote(_)) => {
                    if !current_text.is_empty() {
                        let wrapped = wrap_text_with_ansi(&current_text, content_width - 2);
                        for line in wrapped {
                            lines.push(format!("{}{}", padding, (self.theme.quote)(&line)));
                        }
                        current_text.clear();
                    }
                    in_blockquote = false;
                }
                Event::Start(Tag::List(start)) => {
                    flush_text(&mut current_text, &mut lines, content_width, &padding);
                    list_depth += 1;
                    ordered_index = start;
                }
                Event::End(TagEnd::List(_)) => {
                    list_depth = list_depth.saturating_sub(1);
                    if list_depth == 0 {
                        ordered_index = None;
                        lines.push(String::new());
                    }
                }
                Event::Start(Tag::Item) => {
                    flush_text(&mut current_text, &mut lines, content_width, &padding);
                }
                Event::End(TagEnd::Item) => {
                    let indent = "  ".repeat(list_depth.saturating_sub(1));
                    let bullet = if let Some(ref mut idx) = ordered_index {
                        let b = format!("{}. ", idx);
                        *idx += 1;
                        b
                    } else {
                        "• ".to_string()
                    };
                    let styled_bullet = (self.theme.list_bullet)(&bullet);
                    let item_width =
                        content_width.saturating_sub((indent.len() + bullet.len()) as u16);
                    let wrapped = wrap_text_with_ansi(&current_text, item_width);
                    for (i, line) in wrapped.iter().enumerate() {
                        if i == 0 {
                            lines.push(format!("{}{}{}{}", padding, indent, styled_bullet, line));
                        } else {
                            let continuation = " ".repeat(indent.len() + bullet.len());
                            lines.push(format!("{}{}{}", padding, continuation, line));
                        }
                    }
                    current_text.clear();
                }
                Event::Start(Tag::Paragraph) => {}
                Event::End(TagEnd::Paragraph) => {
                    flush_text(&mut current_text, &mut lines, content_width, &padding);
                    lines.push(String::new());
                }
                Event::Start(Tag::Strong) => in_bold = true,
                Event::End(TagEnd::Strong) => in_bold = false,
                Event::Start(Tag::Emphasis) => in_italic = true,
                Event::End(TagEnd::Emphasis) => in_italic = false,
                Event::Start(Tag::Strikethrough) => in_strikethrough = true,
                Event::End(TagEnd::Strikethrough) => in_strikethrough = false,
                Event::Text(text) => {
                    let styled = if in_code_block {
                        text.to_string()
                    } else {
                        let mut s = text.to_string();
                        if in_bold {
                            s = (self.theme.bold)(&s);
                        }
                        if in_italic {
                            s = (self.theme.italic)(&s);
                        }
                        if in_strikethrough {
                            s = (self.theme.strikethrough)(&s);
                        }
                        s
                    };
                    current_text.push_str(&styled);
                }
                Event::Code(code) => {
                    current_text.push_str(&(self.theme.code)(&code));
                }
                Event::SoftBreak => {
                    current_text.push(' ');
                }
                Event::HardBreak => {
                    if in_blockquote {
                        let wrapped = wrap_text_with_ansi(&current_text, content_width - 2);
                        for line in wrapped {
                            lines.push(format!("{}{}", padding, (self.theme.quote)(&line)));
                        }
                    } else {
                        flush_text(&mut current_text, &mut lines, content_width, &padding);
                    }
                    current_text.clear();
                }
                Event::Rule => {
                    flush_text(&mut current_text, &mut lines, content_width, &padding);
                    let hr = (self.theme.hr)(&"─".repeat(content_width as usize));
                    lines.push(format!("{}{}", padding, hr));
                    lines.push(String::new());
                }
                Event::Start(Tag::Table(alignments)) => {
                    flush_text(&mut current_text, &mut lines, content_width, &padding);
                    table_state = Some(TableState {
                        alignments: alignments.to_vec(),
                        rows: Vec::new(),
                        current_row: Vec::new(),
                        current_row_is_header: false,
                        in_header: false,
                    });
                }
                Event::Start(Tag::TableHead) => {
                    if let Some(ref mut ts) = table_state {
                        ts.in_header = true;
                    }
                }
                Event::End(TagEnd::TableHead) => {
                    if let Some(ref mut ts) = table_state {
                        if !ts.current_row.is_empty() {
                            let cells = std::mem::take(&mut ts.current_row);
                            ts.rows.push(TableRow { cells, is_header: true });
                        }
                        ts.in_header = false;
                    }
                }
                Event::Start(Tag::TableRow) => {
                    if let Some(ref mut ts) = table_state {
                        ts.current_row.clear();
                        ts.current_row_is_header = ts.in_header;
                    }
                }
                Event::End(TagEnd::TableRow) => {
                    if let Some(ref mut ts) = table_state {
                        if !ts.current_row.is_empty() {
                            let cells = std::mem::take(&mut ts.current_row);
                            ts.rows.push(TableRow { cells, is_header: ts.current_row_is_header });
                        }
                    }
                }
                Event::Start(Tag::TableCell) => {}
                Event::End(TagEnd::TableCell) => {
                    if let Some(ref mut ts) = table_state {
                        ts.current_row.push(std::mem::take(&mut current_text));
                    }
                }
                Event::End(TagEnd::Table) => {
                    if let Some(ts) = table_state.take() {
                        let rendered = render_table(&ts, &padding, &self.theme);
                        lines.extend(rendered);
                        lines.push(String::new());
                    }
                }
                _ => {}
            }
        }

        flush_text(&mut current_text, &mut lines, content_width, &padding);

        // Add bottom padding
        for _ in 0..self.padding_y {
            lines.push(String::new());
        }

        // Remove trailing empty lines
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }

        lines
    }
}

fn render_table(ts: &TableState, padding: &str, theme: &MarkdownTheme) -> Vec<String> {
    if ts.rows.is_empty() {
        return vec![];
    }

    let ncols = ts.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
    if ncols == 0 {
        return vec![];
    }

    // Compute natural column widths (visible chars only, no ANSI in cell text
    // at this point since cells are collected from current_text which may have
    // inline styling applied).
    let mut col_widths: Vec<u16> = vec![0u16; ncols];
    for row in &ts.rows {
        for (ci, cell) in row.cells.iter().enumerate() {
            let w = visible_width(cell) as u16;
            if w > col_widths[ci] {
                col_widths[ci] = w;
            }
        }
    }

    let border = |s: &str| (theme.table_border)(s);

    // Build a separator row: ├─────┼─────┤ or ╞═════╪═════╡
    let make_sep = |left: &str, mid: &str, right: &str, fill: &str| -> String {
        let mut s = border(left).to_string();
        for (i, &w) in col_widths.iter().enumerate() {
            s.push_str(&border(&fill.repeat((w + 2) as usize)));
            if i + 1 < ncols {
                s.push_str(&border(mid));
            }
        }
        s.push_str(&border(right));
        s
    };

    let top_border = make_sep("┌", "┬", "┐", "─");
    let header_sep = make_sep("╞", "╪", "╡", "═");
    let row_sep = make_sep("├", "┼", "┤", "─");
    let bot_border = make_sep("└", "┴", "┘", "─");

    let fmt_cell = |content: &str, col: usize, is_header: bool| -> String {
        let w = col_widths[col];
        let align = ts.alignments.get(col).copied().unwrap_or(Alignment::None);
        let visible = visible_width(content) as u16;
        let pad_total = w.saturating_sub(visible);
        let (pad_left, pad_right) = match align {
            Alignment::Center => {
                let l = pad_total / 2;
                (l, pad_total - l)
            }
            Alignment::Right => (pad_total, 0),
            _ => (0, pad_total),
        };
        let cell_str = format!(
            " {}{}{} ",
            " ".repeat(pad_left as usize),
            content,
            " ".repeat(pad_right as usize)
        );
        if is_header { (theme.table_header)(&cell_str) } else { cell_str }
    };

    let fmt_row = |row: &TableRow| -> String {
        let mut s = border("│").to_string();
        for ci in 0..ncols {
            let cell = row.cells.get(ci).map(String::as_str).unwrap_or("");
            s.push_str(&fmt_cell(cell, ci, row.is_header));
            s.push_str(&border("│"));
        }
        s
    };

    let mut out: Vec<String> = Vec::new();
    out.push(format!("{}{}", padding, top_border));

    for (ri, row) in ts.rows.iter().enumerate() {
        out.push(format!("{}{}", padding, fmt_row(row)));
        if row.is_header {
            out.push(format!("{}{}", padding, header_sep));
        } else if ri + 1 < ts.rows.len() {
            out.push(format!("{}{}", padding, row_sep));
        }
    }

    out.push(format!("{}{}", padding, bot_border));
    out
}

fn flush_text(text: &mut String, lines: &mut Vec<String>, width: u16, padding: &str) {
    if text.is_empty() {
        return;
    }
    let wrapped = wrap_text_with_ansi(text, width);
    for line in wrapped {
        lines.push(format!("{}{}", padding, line));
    }
    text.clear();
}

impl Component for Markdown {
    fn render(&self, width: u16) -> Vec<String> {
        if let Some((cached_width, ref cached_lines)) = self.cached
            && cached_width == width
        {
            return cached_lines.clone();
        }
        self.render_markdown(width)
    }

    fn invalidate(&mut self) {
        self.cached = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(text: &str) -> Markdown {
        Markdown::new(text.to_string())
    }

    #[test]
    fn table_basic() {
        let text = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        let lines = md(text).render_markdown(80);
        // Should have top border, header row, header-sep, data row, bot border
        assert!(lines.iter().any(|l| l.contains('┌')), "missing top border: {:?}", lines);
        assert!(lines.iter().any(|l| l.contains('╞')), "missing header sep: {:?}", lines);
        assert!(lines.iter().any(|l| l.contains('└')), "missing bottom border: {:?}", lines);
    }

    #[test]
    fn table_column_count() {
        let text = "| Flag | Name | Description |\n|------|------|-------------|\n| -n | noexec | Parse but don't execute |\n";
        let lines = md(text).render_markdown(120);
        // Every non-empty table line should have 4 │ characters (3 columns = 4 pipes)
        for line in &lines {
            let stripped = line.trim();
            if stripped.starts_with('│') || stripped.starts_with('├') || stripped.starts_with('╞')
            {
                let pipe_count = stripped.chars().filter(|&c| c == '│').count();
                assert_eq!(pipe_count, 4, "wrong pipe count in: {}", stripped);
            }
        }
    }

    #[test]
    fn table_no_truncation() {
        // Even when the table is wider than the render width, cell content must
        // be displayed in full — no ellipsis or chopping.
        let text = "| Very long column header | Another long header |\n|---|---|\n| long cell content here | more content |\n";
        let lines = md(text).render_markdown(40);
        let all = lines.join("\n");
        assert!(all.contains("Very long column header"), "header cell was truncated");
        assert!(all.contains("Another long header"), "second header cell was truncated");
        assert!(all.contains("long cell content here"), "data cell was truncated");
    }
}
