use crate::tui::tui::Component;

const SPINNER_FRAMES: &[&str] = &["┼", "─", "┼", "│"];
const SPINNER_WORDS: &[&str] = &[
    "Reticulating splines",
    "Tracing neural link",
    "Routing synapses",
    "Collapsing waveform",
    "Probing the Wired",
    "Calibrating AT field",
    "Dissolving ego boundary",
    "Closing the schism",
    "Accessing Protocol 7",
    "Initiating contact",
    "Loading the construct",
    "Bending the spoon",
    "Taking the red pill",
    "Freeing your mind",
    "Entering the Source",
];

/// Fixed-position status bar between the editor and footer.
/// Shows spinner (during streaming), per-response timer + tokens, and queued messages.
pub struct StatusBar {
    frame: usize,
    word: String,
    streaming: bool,
    start: Option<std::time::Instant>,
    /// When the first output token arrived (for tok/s calculation).
    first_output: Option<std::time::Instant>,
    input_tokens: u32,
    output_tokens: u32,
    completed: Option<CompletedInfo>,
    queued: Vec<String>,
    editing_idx: Option<usize>,
}

struct CompletedInfo {
    elapsed: std::time::Duration,
    input_tokens: u32,
    output_tokens: u32,
    tok_per_sec: Option<f64>,
    interrupted: bool,
}

impl StatusBar {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            frame: 0,
            word: String::new(),
            streaming: false,
            start: None,
            first_output: None,
            input_tokens: 0,
            output_tokens: 0,
            completed: None,
            queued: Vec::new(),
            editing_idx: None,
        }
    }

    pub fn start_streaming(&mut self) {
        let idx = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as usize
            % SPINNER_WORDS.len();
        self.word = SPINNER_WORDS[idx].to_string();
        self.streaming = true;
        self.start = Some(std::time::Instant::now());
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.first_output = None;
        self.completed = None;
        self.frame = 0;
    }

    /// Add tokens from a completed message (accumulates across multiple messages in one turn).
    pub fn add_tokens(&mut self, input: u32, output: u32) {
        self.input_tokens += input;
        self.output_tokens += output;
    }

    /// Update live input token count (from API usage events).
    pub fn set_input_tokens(&mut self, tokens: u32) {
        self.input_tokens = tokens;
    }

    /// Update live output token count during streaming.
    pub fn set_output_tokens(&mut self, tokens: u32) {
        if self.first_output.is_none() && tokens > 0 {
            self.first_output = Some(std::time::Instant::now());
        }
        self.output_tokens = tokens;
    }

    /// Mark the entire agent turn as complete (called on AgentEnd).
    pub fn finish(&mut self) {
        let elapsed = self.start.map(|s| s.elapsed()).unwrap_or_default();
        let tok_per_sec = self.output_tok_per_sec();
        self.completed = Some(CompletedInfo {
            elapsed,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            tok_per_sec,
            interrupted: false,
        });
        self.streaming = false;
        self.start = None;
    }

    pub fn cancel_streaming(&mut self) {
        let elapsed = self.start.map(|s| s.elapsed()).unwrap_or_default();
        let tok_per_sec = self.output_tok_per_sec();
        self.completed = Some(CompletedInfo {
            elapsed,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            tok_per_sec,
            interrupted: true,
        });
        self.streaming = false;
        self.start = None;
    }

    pub fn set_queue(&mut self, messages: &[String], editing_idx: Option<usize>) {
        self.queued = messages.to_vec();
        self.editing_idx = editing_idx;
    }
}

impl Component for StatusBar {
    fn render(&self, _width: u16) -> Vec<String> {
        use crate::interactive::theme;
        let mut lines = Vec::new();
        let r = theme::RESET;

        if self.streaming {
            self.render_spinner(&mut lines);
        } else if let Some(ref info) = self.completed {
            let tps = info
                .tok_per_sec
                .map(|t| format!(" {}{:.1} t/s{}", theme::FOOTER_DIM, t, r))
                .unwrap_or_default();
            let tok = if info.input_tokens > 0 || info.output_tokens > 0 {
                format!(
                    " {}·{} ↑{} ↓{}{}",
                    theme::DIM,
                    theme::FOOTER_LABEL,
                    fmt_tok(info.input_tokens),
                    fmt_tok(info.output_tokens),
                    tps,
                )
            } else {
                String::new()
            };
            if info.interrupted {
                lines.push(format!(
                    "{}⚡ Interrupted{} {}({}{}){}",
                    theme::WARN,
                    r,
                    theme::DIM,
                    fmt_elapsed(info.elapsed),
                    tok,
                    r,
                ));
            } else {
                lines.push(format!(
                    "{}✓ Completed{} {}({}{}){}",
                    theme::SUCCESS,
                    r,
                    theme::DIM,
                    fmt_elapsed(info.elapsed),
                    tok,
                    r,
                ));
            }
        }

        if !self.queued.is_empty() {
            for (i, msg) in self.queued.iter().enumerate() {
                let marker = if self.editing_idx == Some(i) {
                    theme::ACCENT
                } else {
                    theme::DIM
                };
                let preview = if msg.len() > 60 {
                    &msg[..60]
                } else {
                    msg.as_str()
                };
                lines.push(format!(" {}▸ {}{}", marker, preview, r));
            }
        }

        lines
    }
}

impl StatusBar {
    fn render_spinner(&self, lines: &mut Vec<String>) {
        use crate::interactive::theme;
        let r = theme::RESET;
        let elapsed = self.start.map(|s| s.elapsed()).unwrap_or_default();
        let spinner = SPINNER_FRAMES[self.frame % SPINNER_FRAMES.len()];

        // Highlight the active direction in yellow, show tok/s during output
        let tok = if self.input_tokens > 0 || self.output_tokens > 0 {
            let (up_color, down_color) = if self.output_tokens == 0 {
                (theme::WARN, theme::FOOTER_DIM) // input phase
            } else {
                (theme::FOOTER_DIM, theme::WARN) // output phase
            };
            let tps = self.output_tok_per_sec();
            let tps_str = if let Some(tps) = tps {
                format!(" {}{:.1} t/s{}", theme::FOOTER_DIM, tps, r)
            } else {
                String::new()
            };
            format!(
                " {}·{} {}↑{}{} {}↓{}{}{}",
                theme::DIM,
                r,
                up_color,
                fmt_tok(self.input_tokens),
                r,
                down_color,
                fmt_tok(self.output_tokens),
                r,
                tps_str,
            )
        } else {
            String::new()
        };

        lines.push(format!(
            "{}{}{} {}{}… {}({}{}){}",
            theme::ACCENT,
            spinner,
            r,
            theme::DIM,
            self.word,
            theme::FOOTER_DIM,
            fmt_elapsed(elapsed),
            tok,
            r,
        ));
    }

    /// Output tokens per second, measured from first output token to now.
    fn output_tok_per_sec(&self) -> Option<f64> {
        let first = self.first_output?;
        let elapsed = first.elapsed().as_secs_f64();
        if elapsed < 0.5 || self.output_tokens < 2 {
            return None;
        }
        Some(self.output_tokens as f64 / elapsed)
    }

    /// Advance spinner frame. Call on render tick.
    pub fn tick(&mut self) {
        if self.streaming {
            self.frame += 1;
        }
    }
}

fn fmt_elapsed(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

fn fmt_tok(n: u32) -> String {
    if n == 0 {
        "0".into()
    } else if n < 1_000 {
        n.to_string()
    } else if n < 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{}k", n / 1_000)
    }
}
