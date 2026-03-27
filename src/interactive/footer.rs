use crate::agent::types::*;
use crate::interactive::theme;
use crate::tui::tui::Component;
use crate::tui::utils::{truncate_to_width, visible_width};

pub struct FooterComponent {
    cwd: String,
    git_branch: Option<String>,
    session_id: Option<String>,
    session_name: Option<String>,
    model_id: String,
    provider_name: String,
    thinking_on: bool,
    effort_level: Option<EffortLevel>,
    context_window: u32,
    context_used: u32,
    /// Input and output costs accumulated across all API calls in this session.
    cost_input: f64,
    cost_output: f64,
    provider_online: Option<bool>,
    plan_mode: bool,
    /// Total input/output tokens sent across all API calls in this session.
    total_input: u64,
    total_output: u64,
    /// Number of API calls made in this session.
    api_calls: u32,
}

impl FooterComponent {
    fn abbrev_cwd(cwd: &str) -> String {
        // Replace the home prefix with ~ for display.
        // home_dir() is a OnceLock cache hit after the first call.
        if let Some(h) = crate::home_dir() {
            let h = h.to_string_lossy();
            if cwd.starts_with(h.as_ref()) {
                return format!("~{}", &cwd[h.len()..]);
            }
        }
        cwd.to_string()
    }

    pub fn new(cwd: &str) -> Self {
        let git_branch = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(cwd)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            });

        Self {
            cwd: Self::abbrev_cwd(cwd),
            git_branch,
            session_id: None,
            session_name: None,
            model_id: String::new(),
            provider_name: String::new(),
            thinking_on: false,
            effort_level: None,
            context_window: 0,
            context_used: 0,
            cost_input: 0.0,
            cost_output: 0.0,
            provider_online: None,
            plan_mode: false,
            total_input: 0,
            total_output: 0,
            api_calls: 0,
        }
    }

    pub fn set_cwd(&mut self, cwd: &str) {
        self.cwd = Self::abbrev_cwd(cwd);
        self.git_branch = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(cwd)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            });
    }

    pub fn set_model(&mut self, model: &Model) {
        self.model_id = model.id.clone();
        self.provider_name = model.provider_name.clone();
        self.context_window = model.context_window;
    }

    pub fn set_provider_online(&mut self, provider: &str, online: bool) {
        if provider == self.provider_name {
            self.provider_online = Some(online);
        }
    }

    pub fn set_plan_mode(&mut self, enabled: bool) {
        self.plan_mode = enabled;
    }

    pub fn set_session_id(&mut self, id: String) {
        self.session_id = Some(id);
    }

    pub fn set_session_name(&mut self, name: Option<String>) {
        self.session_name = name;
    }

    pub fn set_thinking(&mut self, level: ThinkingLevel) {
        self.thinking_on = level == ThinkingLevel::On;
    }

    pub fn set_effort(&mut self, level: Option<EffortLevel>) {
        self.effort_level = level;
    }

    pub fn set_context_used(&mut self, tokens: u32) {
        // Only increase within a turn — prevents stale/partial provider
        // updates from resetting the counter mid-session.
        if tokens > self.context_used {
            self.context_used = tokens;
        }
    }

    pub fn reset_context(&mut self) {
        self.context_used = 0;
        self.total_input = 0;
        self.total_output = 0;
        self.api_calls = 0;
        self.cost_input = 0.0;
        self.cost_output = 0.0;
    }

    /// Record an API call's input tokens (called on each UsageUpdate).
    pub fn record_api_call(&mut self, input_tokens: u32) {
        self.total_input += input_tokens as u64;
        self.api_calls += 1;
    }

    pub fn add_cost(&mut self, usage: &Usage, pricing: &ModelPricing) {
        self.cost_input += (pricing.input / 1_000_000.0) * usage.input as f64;
        self.cost_input += (pricing.cache_read / 1_000_000.0) * usage.cache_read as f64;
        self.cost_input += (pricing.cache_write / 1_000_000.0) * usage.cache_write as f64;
        self.cost_output += (pricing.output / 1_000_000.0) * usage.output as f64;
        self.total_output += usage.output as u64;
    }
}

impl Component for FooterComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let r = theme::RESET;
        let dim = theme::FOOTER_DIM;
        let label = theme::FOOTER_LABEL;
        let w = width as usize;

        // Line 1: ~/path (branch) [session name] ... thinking level
        let mut pwd = self.cwd.clone();
        if let Some(ref branch) = self.git_branch {
            pwd = format!("{} {}({}){}", pwd, theme::ACCENT, branch, r);
        }
        let pwd_left = if let Some(ref name) = self.session_name {
            format!(
                "{}{}{} {}\"{}\"{} ",
                dim, pwd, r,
                theme::FOOTER_LABEL, name, r,
            )
        } else {
            format!("{}{}{}", dim, pwd, r)
        };

        let plan_tag = if self.plan_mode {
            format!("{}PLAN{} ", theme::ACCENT_BOLD, r)
        } else {
            String::new()
        };

        // Show thinking on/off, and effort level if set (effort shown regardless of thinking state).
        let effort_suffix = if let Some(effort) = self.effort_level {
            let name = match effort {
                EffortLevel::Low => "low",
                EffortLevel::Medium => "medium",
                EffortLevel::High => "high",
                EffortLevel::Max => "max",
            };
            let color = match effort {
                EffortLevel::Low => theme::THINKING_LOW,
                EffortLevel::Medium => theme::THINKING,
                EffortLevel::High | EffortLevel::Max => theme::THINKING_HIGH,
            };
            format!(" {}[{}]{}", color, name, r)
        } else {
            String::new()
        };
        let think_right = if self.thinking_on {
            format!("{}thinking on{}{}", theme::THINKING, r, effort_suffix)
        } else {
            format!("{}thinking off{}{}", dim, r, effort_suffix)
        };

        let mode_right = format!("{}{}", plan_tag, think_right);
        let line1 = right_align(&pwd_left, &mode_right, w);

        // Line 2: full-width hexagon progress bar
        let context_pct = if self.context_window > 0 {
            (self.context_used as f64 / self.context_window as f64) * 100.0
        } else {
            0.0
        };
        let ctx_color = if context_pct > 90.0 {
            theme::ERROR
        } else if context_pct > 80.0 {
            theme::WARN
        } else if context_pct > 50.0 {
            theme::CAUTION
        } else {
            theme::SUCCESS
        };

        let bar_len = w;
        let filled = ((context_pct / 100.0) * bar_len as f64).round() as usize;
        let empty = bar_len.saturating_sub(filled);
        let hex_bar = format!(
            "{}{}{}{}{}",
            ctx_color,
            "⬢".repeat(filled),
            theme::FOOTER_DIM,
            "⬡".repeat(empty),
            r,
        );

        // Line 3: centered counter + cost + model
        let counter = format!(
            "{}{}/{}{}",
            ctx_color,
            fmt_tokens(self.context_used),
            fmt_tokens(self.context_window),
            r,
        );
        let total_cost = self.cost_input + self.cost_output;
        let cost = if total_cost > 0.001 {
            format!(" {}${:.3}{}", theme::COST, total_cost, r)
        } else {
            String::new()
        };
        // Show cumulative API usage when the agentic loop made multiple calls,
        // so the user understands why cost is higher than context_used suggests.
        let api_info = if self.api_calls > 1 {
            let in_cost = if self.cost_input > 0.001 {
                format!(" (${}){}", fmt_cost(self.cost_input), dim)
            } else {
                String::new()
            };
            let out_cost = if self.cost_output > 0.001 {
                format!(" (${}){}", fmt_cost(self.cost_output), dim)
            } else {
                String::new()
            };
            format!(
                " {}({} calls, {} tok in{}, {} tok out{}){}",
                dim,
                self.api_calls,
                fmt_tokens_u64(self.total_input),
                in_cost,
                fmt_tokens_u64(self.total_output),
                out_cost,
                r,
            )
        } else {
            String::new()
        };
        let model = if self.model_id.is_empty() {
            format!("{}no model{}", theme::ERROR, r)
        } else {
            match self.provider_online {
                Some(false) => format!("{}(offline) {}{}", theme::ERROR, self.model_id, r),
                Some(true) => format!("{}{}{}", theme::SUCCESS, self.model_id, r),
                None => format!("{}{}{}", label, self.model_id, r),
            }
        };

        let info = format!("{}{}{} {}", counter, cost, api_info, model);

        // Session label: use name if set, else the first 8 chars of the session id.
        let session_label = if let Some(name) = &self.session_name {
            format!("{}{}{}", theme::DIM, name, r)
        } else if let Some(id) = &self.session_id {
            format!("{}#{}{}", theme::DIM, &id[..id.len().min(8)], r)
        } else {
            String::new()
        };

        let line3 = if session_label.is_empty() {
            let info_width = visible_width(&info) as usize;
            let pad = w.saturating_sub(info_width) / 2;
            format!("{}{}", " ".repeat(pad), info)
        } else {
            right_align(&session_label, &info, w)
        };

        vec![line1, hex_bar, line3]
    }
}

fn right_align(left: &str, right: &str, width: usize) -> String {
    let lw = visible_width(left) as usize;
    let rw = visible_width(right) as usize;
    if lw + 2 + rw <= width {
        let padding = " ".repeat(width - lw - rw);
        format!("{}{}{}", left, padding, right)
    } else if lw + 2 <= width {
        let avail = width.saturating_sub(lw + 2);
        let trunc = truncate_to_width(right, avail as u16);
        let tw = visible_width(&trunc) as usize;
        let padding = " ".repeat(width.saturating_sub(lw + tw));
        format!("{}{}{}", left, padding, trunc)
    } else {
        truncate_to_width(left, width as u16).to_string()
    }
}

fn fmt_cost(dollars: f64) -> String {
    if dollars < 0.01 {
        format!("{:.4}", dollars)
    } else if dollars < 1.0 {
        format!("{:.3}", dollars)
    } else {
        format!("{:.2}", dollars)
    }
}

fn fmt_tokens_u64(count: u64) -> String {
    if count == 0 {
        "0".to_string()
    } else if count < 1_000 {
        count.to_string()
    } else if count < 10_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else if count < 1_000_000 {
        format!("{}k", count / 1_000)
    } else if count < 10_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else {
        format!("{}M", count / 1_000_000)
    }
}

fn fmt_tokens(count: u32) -> String {
    if count == 0 {
        "0".to_string()
    } else if count < 1_000 {
        count.to_string()
    } else if count < 10_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else if count < 1_000_000 {
        format!("{}k", count / 1_000)
    } else if count < 10_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else {
        format!("{}M", count / 1_000_000)
    }
}
