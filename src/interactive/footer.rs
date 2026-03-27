use crate::agent::types::*;
use crate::interactive::theme;
use crate::tui::tui::Component;
use crate::tui::utils::{truncate_to_width, visible_width};

pub struct FooterComponent {
    cwd: String,
    git_branch: Option<String>,
    model_id: String,
    provider_name: String,
    thinking_level: ThinkingLevel,
    context_window: u32,
    context_used: u32,
    total_cost: f64,
    provider_online: Option<bool>,
}

impl FooterComponent {
    pub fn new(cwd: &str) -> Self {
        let home = crate::home_dir().map(|h| h.to_string_lossy().to_string());
        let display_cwd = if let Some(ref h) = home {
            if cwd.starts_with(h.as_str()) {
                format!("~{}", &cwd[h.len()..])
            } else {
                cwd.to_string()
            }
        } else {
            cwd.to_string()
        };

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
            cwd: display_cwd,
            git_branch,
            model_id: String::new(),
            provider_name: String::new(),
            thinking_level: ThinkingLevel::Off,
            context_window: 0,
            context_used: 0,
            total_cost: 0.0,
            provider_online: None,
        }
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

    pub fn set_thinking(&mut self, level: ThinkingLevel) {
        self.thinking_level = level;
    }

    pub fn set_context_used(&mut self, tokens: u32) {
        self.context_used = tokens;
    }

    pub fn add_cost(&mut self, usage: &Usage, pricing: &ModelPricing) {
        self.total_cost += (pricing.input / 1_000_000.0) * usage.input as f64;
        self.total_cost += (pricing.output / 1_000_000.0) * usage.output as f64;
        self.total_cost += (pricing.cache_read / 1_000_000.0) * usage.cache_read as f64;
        self.total_cost += (pricing.cache_write / 1_000_000.0) * usage.cache_write as f64;
    }
}

impl Component for FooterComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let r = theme::RESET;
        let dim = theme::FOOTER_DIM;
        let label = theme::FOOTER_LABEL;
        let w = width as usize;

        // Line 1: ~/path (branch) ... thinking level
        let mut pwd = self.cwd.clone();
        if let Some(ref branch) = self.git_branch {
            pwd = format!("{} {}({}){}", pwd, theme::ACCENT, branch, r);
        }
        let pwd_left = format!("{}{}{}", dim, pwd, r);

        let think_right = match self.thinking_level {
            ThinkingLevel::Off => format!("{}thinking off{}", dim, r),
            ThinkingLevel::Minimal | ThinkingLevel::Low => {
                let name = format!("{:?}", self.thinking_level).to_lowercase();
                format!("{}{} thinking{}", theme::THINKING_LOW, name, r)
            }
            ThinkingLevel::Medium => {
                let name = format!("{:?}", self.thinking_level).to_lowercase();
                format!("{}{} thinking{}", theme::THINKING, name, r)
            }
            ThinkingLevel::High | ThinkingLevel::Xhigh => {
                let name = format!("{:?}", self.thinking_level).to_lowercase();
                format!("{}{} thinking{}", theme::THINKING_HIGH, name, r)
            }
        };

        let line1 = right_align(&pwd_left, &think_right, w);

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
        let cost = if self.total_cost > 0.001 {
            format!(" {}${:.3}{}", theme::COST, self.total_cost, r)
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

        let info = format!("{}{} {}", counter, cost, model);
        let info_width = visible_width(&info) as usize;
        let pad = w.saturating_sub(info_width) / 2;
        let line3 = format!("{}{}", " ".repeat(pad), info);

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
