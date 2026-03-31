//! Full-screen plan interview TUI.
//!
//! Displays each question one at a time with multiple-choice options.
//! A freeform "✎ Custom answer..." option is always appended automatically.
//! The user navigates with ↑/↓, confirms with Enter, goes back with ←,
//! skips with Tab, and submits all answers with Ctrl+D.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use crate::core::agent_session::{PlanOption, PlanQuestion};
use crate::tui::utils::wrap_text_with_ansi;

const CUSTOM_LABEL: &str = "✎ Custom answer...";

/// Answer to a single interview question.
#[derive(Clone)]
enum Answer {
    /// User picked one of the provided options (by text).
    Option(String),
    /// User typed a freeform answer.
    Custom(String),
    /// User skipped this question.
    Skipped,
}

impl Answer {
    /// Serialize for injection into the model prompt.
    fn as_str(&self) -> &str {
        match self {
            Answer::Option(s) | Answer::Custom(s) => s.as_str(),
            Answer::Skipped => "",
        }
    }
}

struct QuestionState {
    question: PlanQuestion,
    /// Options including the auto-appended freeform sentinel.
    display_options: Vec<String>,
    /// Which option row is focused.
    cursor: usize,
    /// Current freeform input text (active when cursor == last option).
    custom_text: String,
    /// True while the user is typing a freeform answer.
    in_custom_input: bool,
    answer: Option<Answer>,
}

impl QuestionState {
    fn new(question: PlanQuestion) -> Self {
        let mut display_options: Vec<String> =
            question.options.iter().map(|o| o.label.clone()).collect();
        display_options.push(CUSTOM_LABEL.to_string());
        Self {
            question,
            display_options,
            cursor: 0,
            custom_text: String::new(),
            in_custom_input: false,
            answer: None,
        }
    }

    fn custom_option_idx(&self) -> usize {
        self.display_options.len() - 1
    }

    fn is_on_custom(&self) -> bool {
        self.cursor == self.custom_option_idx()
    }
}

/// Run the full-screen interview. Returns answers as `(question_text, answer_text)` pairs,
/// or `None` if the user cancelled with Ctrl+C / Escape.
pub fn run_plan_interview(
    questions: Vec<PlanQuestion>,
    plan_path: &PathBuf,
) -> Option<Vec<(String, String)>> {
    if questions.is_empty() {
        return Some(Vec::new());
    }

    let mut states: Vec<QuestionState> =
        questions.into_iter().map(QuestionState::new).collect();
    let total = states.len();
    let mut current = 0usize;

    // Enter alt-screen
    let stdout = io::stdout();
    let mut out = stdout.lock();
    write!(out, "\x1b[?1049h\x1b[?25l").ok(); // alt screen + hide cursor
    out.flush().ok();

    let result = interview_loop(&mut states, total, current, &mut out, plan_path);

    // Leave alt-screen
    write!(out, "\x1b[?1049l\x1b[?25h").ok(); // main screen + show cursor
    out.flush().ok();

    result
}

fn interview_loop(
    states: &mut Vec<QuestionState>,
    total: usize,
    mut current: usize,
    out: &mut impl Write,
    plan_path: &PathBuf,
) -> Option<Vec<(String, String)>> {
    render_question(states, current, total, plan_path, out);

    let stdin = io::stdin();
    let mut buf = [0u8; 32];

    loop {
        let n = match stdin.lock().read(&mut buf) {
            Ok(n) if n > 0 => n,
            _ => continue,
        };
        let bytes = &buf[..n];

        let st = &mut states[current];

        if st.in_custom_input {
            // Inside freeform text entry
            match bytes {
                // Escape — leave freeform mode, restore option list
                b"\x1b" => {
                    st.in_custom_input = false;
                    st.custom_text.clear();
                }
                // Ctrl+D — submit all (treat custom text as the answer for this Q)
                b"\x04" => {
                    if !st.custom_text.is_empty() {
                        st.answer = Some(Answer::Custom(st.custom_text.clone()));
                    }
                    return Some(collect_answers(states));
                }
                // Enter — confirm custom answer, advance
                b"\r" | b"\n" => {
                    let text = st.custom_text.trim().to_string();
                    st.in_custom_input = false;
                    if text.is_empty() {
                        st.custom_text.clear();
                        // treat as skip
                        st.answer = Some(Answer::Skipped);
                    } else {
                        st.answer = Some(Answer::Custom(text));
                    }
                    if let Some(next) = advance(states, &mut current, total) {
                        return next;
                    }
                }
                // Backspace
                b"\x7f" | b"\x08" => {
                    st.custom_text.pop();
                }
                // Printable chars
                _ => {
                    for &b in bytes {
                        if b >= 0x20 && b < 0x7f {
                            st.custom_text.push(b as char);
                        }
                    }
                }
            }
        } else {
            match bytes {
                // Escape / Ctrl+C → cancel
                b"\x1b" | b"\x03" => return None,

                // Up arrow
                b"\x1b[A" => {
                    if states[current].cursor > 0 {
                        states[current].cursor -= 1;
                    }
                }

                // Down arrow
                b"\x1b[B" => {
                    let max = states[current].display_options.len() - 1;
                    if states[current].cursor < max {
                        states[current].cursor += 1;
                    }
                }

                // Left arrow — go back one question
                b"\x1b[D" => {
                    if current > 0 {
                        current -= 1;
                    }
                }

                // Tab — skip this question
                b"\t" => {
                    states[current].answer = Some(Answer::Skipped);
                    if let Some(result) = advance(states, &mut current, total) {
                        return result;
                    }
                }

                // Ctrl+D — submit all answers as-is
                b"\x04" => {
                    return Some(collect_answers(states));
                }

                // Enter — select focused option
                b"\r" | b"\n" => {
                    let st = &mut states[current];
                    if st.is_on_custom() {
                        // Switch into freeform input mode
                        st.in_custom_input = true;
                        st.custom_text.clear();
                    } else {
                        let chosen = st.display_options[st.cursor].clone();
                        st.answer = Some(Answer::Option(chosen));
                        if let Some(result) = advance(states, &mut current, total) {
                            return result;
                        }
                    }
                }
                _ => {}
            }
        }

        render_question(states, current, total, plan_path, out);
    }
}

/// Advance to the next unanswered question. Returns `Some(answers)` when all
/// questions are answered (caller should return it), or `None` to continue.
fn advance(
    states: &mut Vec<QuestionState>,
    current: &mut usize,
    total: usize,
) -> Option<Option<Vec<(String, String)>>> {
    // Find next question without an answer
    let next = (*current + 1..total).find(|&i| states[i].answer.is_none());
    if let Some(n) = next {
        *current = n;
        None
    } else {
        // All answered — done
        Some(Some(collect_answers(states)))
    }
}

fn collect_answers(states: &[QuestionState]) -> Vec<(String, String)> {
    states
        .iter()
        .map(|s| (s.question.q.clone(), s.answer.as_ref().map(|a| a.as_str().to_string()).unwrap_or_default()))
        .collect()
}

fn render_question(
    states: &[QuestionState],
    current: usize,
    total: usize,
    plan_path: &PathBuf,
    out: &mut impl Write,
) {
    let (cols, rows) = terminal_size();
    let st = &states[current];

    let mut buf = String::with_capacity(2048);

    // Clear screen, home cursor
    buf.push_str("\x1b[2J\x1b[H");

    // Header bar
    let plan_str = plan_path.display().to_string();
    let header = format!("  Plan: {}", plan_str);
    let header = truncate_to_cols(&header, cols);
    buf.push_str("\x1b[1m");
    buf.push_str(&header);
    buf.push_str("\x1b[0m\r\n");

    // Divider
    buf.push_str(&"─".repeat(cols.min(80)));
    buf.push_str("\r\n\r\n");

    // Progress
    let progress = format!("  Question {} of {}", current + 1, total);
    buf.push_str("\x1b[2m");
    buf.push_str(&progress);
    buf.push_str("\x1b[0m\r\n\r\n");

    // Question text (word-wrapped to cols-4, ANSI-aware)
    let q_width = (cols as u16).saturating_sub(4);
    for line in wrap_text_with_ansi(&st.question.q, q_width) {
        buf.push_str("  ");
        buf.push_str(&line);
        buf.push_str("\r\n");
    }
    buf.push_str("\r\n");

    // Options
    let opt_width = (cols as u16).saturating_sub(8); // indent 6 + 2 padding
    for (i, opt) in st.display_options.iter().enumerate() {
        let selected = i == st.cursor;
        let answered = st.answer.as_ref().map(|a| match a {
            Answer::Option(s) => s == opt,
            Answer::Custom(_) => i == st.custom_option_idx(),
            Answer::Skipped => false,
        }).unwrap_or(false);

        let is_custom_opt = i == st.custom_option_idx();
        // Fetch structured option data (not available for the synthetic custom row).
        let plan_opt: Option<&PlanOption> = st.question.options.get(i);

        let marker = if selected { "●" } else if answered { "✓" } else { "○" };

        if selected {
            buf.push_str("\x1b[1;36m");
        } else if answered {
            buf.push_str("\x1b[32m");
        } else {
            buf.push_str("\x1b[2m");
        }

        if selected && is_custom_opt && st.in_custom_input {
            // Show text input inline
            buf.push_str(&format!("    {} {} ", marker, CUSTOM_LABEL));
            buf.push_str("\x1b[0m\x1b[1m");
            buf.push_str(&st.custom_text);
            buf.push('_'); // cursor
        } else {
            // Label line — include ★ recommended badge inline
            let badge = if plan_opt.map(|o| o.recommended).unwrap_or(false) {
                " \x1b[0m\x1b[33m★ recommended\x1b[0m"
            } else {
                ""
            };
            buf.push_str(&format!("    {} {}{}", marker, opt, badge));

            // Show already-typed custom text if set
            if is_custom_opt {
                if let Some(Answer::Custom(ref c)) = st.answer {
                    buf.push_str("\x1b[0m\x1b[2m");
                    buf.push_str(&format!("  → {}", c));
                } else if !st.custom_text.is_empty() {
                    buf.push_str("\x1b[0m\x1b[2m");
                    buf.push_str(&format!("  → {}", st.custom_text));
                }
            }
        }

        buf.push_str("\x1b[0m\r\n");

        // Subtext line — only for structured options that have it
        if let Some(po) = plan_opt {
            if !po.subtext.is_empty() {
                let subtext_color = if selected { "\x1b[36m" } else { "\x1b[2m" };
                buf.push_str(subtext_color);
                for line in wrap_text_with_ansi(&po.subtext, opt_width) {
                    buf.push_str(&format!("        {}\r\n", line));
                }
                buf.push_str("\x1b[0m");
            }
        }
    }

    // Footer hints
    let footer_row = rows.saturating_sub(2);
    buf.push_str(&format!("\x1b[{};1H", footer_row));
    buf.push_str("\x1b[2m");
    buf.push_str("  [↑/↓] move  [Enter] select  [←] back  [Tab] skip  [Ctrl+D] submit all");
    buf.push_str("\x1b[0m");

    out.write_all(buf.as_bytes()).ok();
    out.flush().ok();
}

fn truncate_to_cols(s: &str, cols: usize) -> String {
    let mut width = 0usize;
    let mut end = s.len();
    for (i, ch) in s.char_indices() {
        let w = unicode_width(ch);
        if width + w > cols {
            end = i;
            break;
        }
        width += w;
    }
    s[..end].to_string()
}

fn unicode_width(ch: char) -> usize {
    // CJK wide chars are 2; everything else 1.
    if (ch as u32) > 0x2E7F {
        2
    } else {
        1
    }
}

fn terminal_size() -> (usize, usize) {
    #[cfg(unix)]
    {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
        if r == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            return (ws.ws_col as usize, ws.ws_row as usize);
        }
    }
    (80, 24)
}
