use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};

use crate::tui::highlight;
use crate::tui::tui::Component;
use crate::tui::utils::{char_wrap_with_ansi, wrap_text_with_ansi};

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
            code_block_indent: "  ",
        }
    }
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
        let parser = Parser::new(&self.text);
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
