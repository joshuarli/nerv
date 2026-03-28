use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::agent::types::*;
use crate::interactive::theme;
use crate::tui::tui::Component;
use crate::tui::utils::{truncate_to_width, visible_width};

/// Returns `true` when the `NERV_DEBUG` environment variable is set to `1`.
pub fn nerv_debug() -> bool {
    std::env::var("NERV_DEBUG").as_deref() == Ok("1")
}

/// How many extra lines the nervHud adds to the fixed footer (0 or 1).
pub fn hud_line_count() -> usize {
    if nerv_debug() { 1 } else { 0 }
}

/// Sample the whole-process RSS in kilobytes.
/// Returns 0 on unsupported platforms or if the syscall fails.
pub(crate) fn sample_rss_kb() -> u64 {
    #[cfg(target_os = "macos")]
    {
        // MACH_TASK_BASIC_INFO (flavor 20) — stable 64-bit struct, whole process.
        use std::mem;
        #[allow(non_camel_case_types)]
        type natural_t = u32;
        #[allow(non_camel_case_types)]
        type integer_t = i32;
        const MACH_TASK_BASIC_INFO: natural_t = 20;
        #[repr(C)]
        struct MachTaskBasicInfo {
            virtual_size: u64,
            resident_size: u64,
            resident_size_max: u64,
            user_time: [u32; 2],   // time_value_t
            system_time: [u32; 2], // time_value_t
            policy: i32,
            suspend_count: i32,
        }
        unsafe extern "C" {
            fn task_info(
                target_task: u32,
                flavor: natural_t,
                task_info_out: *mut integer_t,
                task_info_outCnt: *mut natural_t,
            ) -> i32;
        }
        unsafe {
            #[allow(deprecated)]
            let task = libc::mach_task_self();
            let mut info: MachTaskBasicInfo = mem::zeroed();
            let mut count = (mem::size_of::<MachTaskBasicInfo>() / mem::size_of::<integer_t>()) as natural_t;
            let kr = task_info(
                task,
                MACH_TASK_BASIC_INFO,
                &mut info as *mut MachTaskBasicInfo as *mut integer_t,
                &mut count,
            );
            if kr == 0 {
                info.resident_size / 1024
            } else {
                0
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        // /proc/self/status contains "VmRSS: <n> kB"
        if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    let kb: u64 = rest.split_whitespace().next()
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                    return kb;
                }
            }
        }
        0
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

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
    /// Auto-compact threshold (0–100). Default 50.
    compact_threshold_pct: u8,
    /// When true, the hexagon bar animates as a loading sweep instead of showing fill.
    compacting: bool,
    /// Animation frame counter for the compaction sweep (incremented on each tick).
    compact_tick: u8,
    /// Total input/output tokens sent across all API calls in this session.
    total_input: u64,
    total_output: u64,
    /// Cumulative cache read/write tokens across all API calls.
    total_cache_read: u64,
    total_cache_write: u64,
    /// Number of API calls made in this session.
    api_calls: u32,
    /// nervHud: current process RSS in KiB, written by a background thread.
    /// Only displayed when `NERV_DEBUG=1`.
    rss_kb: Arc<AtomicU64>,
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
            compact_threshold_pct: 50,
            compacting: false,
            compact_tick: 0,
            total_input: 0,
            total_output: 0,
            total_cache_read: 0,
            total_cache_write: 0,
            api_calls: 0,
            rss_kb: {
                let cell = Arc::new(AtomicU64::new(0));
                if nerv_debug() {
                    // nervHud background poller: sample RSS every 500 ms.
                    let cell2 = cell.clone();
                    std::thread::Builder::new()
                        .name("nerv-hud".into())
                        .spawn(move || loop {
                            cell2.store(sample_rss_kb(), Ordering::Relaxed);
                            std::thread::sleep(std::time::Duration::from_millis(500));
                        })
                        .ok();
                }
                cell
            },
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

    pub fn set_compact_threshold(&mut self, pct: u8) {
        self.compact_threshold_pct = pct;
    }

    pub fn set_compacting(&mut self, active: bool) {
        self.compacting = active;
        if !active {
            self.compact_tick = 0;
        }
    }

    /// Advance the compaction animation frame. Called every ~100ms from the main tick.
    pub fn tick(&mut self) {
        if self.compacting {
            self.compact_tick = self.compact_tick.wrapping_add(1);
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
        self.total_cache_read = 0;
        self.total_cache_write = 0;
        self.api_calls = 0;
        self.cost_input = 0.0;
        self.cost_output = 0.0;
    }

    /// Record an API call's input tokens (called on each UsageUpdate).
    pub fn record_api_call(&mut self, input_tokens: u32) {
        self.total_input += input_tokens as u64;
        self.api_calls += 1;
    }

    /// Restore accumulated cost from a loaded session. Sets cost_input to the
    /// full restored total (we don't have input/output split from legacy entries,
    /// so we put everything in cost_input for display).
    pub fn restore_cost(&mut self, total_usd: f64) {
        self.cost_input = total_usd;
        self.cost_output = 0.0;
    }

    /// Return the total accumulated cost so far (input + output).
    pub fn current_cost(&self) -> f64 {
        self.cost_input + self.cost_output
    }

    pub fn add_cost(&mut self, usage: &Usage, pricing: &ModelPricing) {
        // usage.input is the full context window (uncached + cache_read + cache_write).
        // Only the uncached slice is billed at the regular input rate; cache tokens are
        // billed separately at their own rates.
        let uncached = usage.input.saturating_sub(usage.cache_read + usage.cache_write);
        self.cost_input += (pricing.input / 1_000_000.0) * uncached as f64;
        self.cost_input += (pricing.cache_read / 1_000_000.0) * usage.cache_read as f64;
        self.cost_input += (pricing.cache_write / 1_000_000.0) * usage.cache_write as f64;
        self.cost_output += (pricing.output / 1_000_000.0) * usage.output as f64;
        self.total_output += usage.output as u64;
        self.total_cache_read += usage.cache_read as u64;
        self.total_cache_write += usage.cache_write as u64;
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
        let pwd_left = format!("{}{}{}", dim, pwd, r);

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

        // Line 3: full-width hexagon progress bar
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
        let hex_bar = if self.compacting {
            // Sweep animation: a window of filled hexagons scrolls across the bar.
            // Window is ~20% of bar width; wraps with a short trailing gap.
            let window = (bar_len / 5).max(4);
            let period = bar_len + window; // full cycle length
            let offset = (self.compact_tick as usize * 2) % period; // 2 cells/tick = ~20 cells/s
            let mut buf = String::new();
            buf.push_str(theme::ACCENT);
            for i in 0..bar_len {
                // Position within the sweep cycle (leading edge at `offset`)
                let rel = (offset + bar_len - i) % period;
                if rel < window {
                    buf.push('⬢');
                } else {
                    buf.push_str(&format!("{}{}{}", theme::FOOTER_DIM, '⬡', theme::ACCENT));
                }
            }
            buf.push_str(r);
            buf
        } else {
            let filled = ((context_pct / 100.0) * bar_len as f64).round() as usize;
            let empty = bar_len.saturating_sub(filled);
            format!(
                "{}{}{}{}{}",
                ctx_color,
                "⬢".repeat(filled),
                theme::FOOTER_DIM,
                "⬡".repeat(empty),
                r,
            )
        };

        // Line 2: session name/id (left) — model name (right)
        let session_label = if let Some(name) = &self.session_name {
            format!("{}{}{}", theme::ACCENT, name, r)
        } else if let Some(id) = &self.session_id {
            format!("{}#{}{}", theme::ACCENT, &id[..id.len().min(8)], r)
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
        let line2 = right_align(&session_label, &model, w);

        // Line 4 (below hex bar): token counter (left) + cost / api_info (right)
        let compact_tag = format!(
            " {}(compact @ {}%){}",
            dim, self.compact_threshold_pct, r,
        );
        let counter = format!(
            "{}{}/{}{}{}",
            ctx_color,
            fmt_tokens(self.context_used),
            fmt_tokens(self.context_window),
            r,
            compact_tag,
        );
        let cache_stats = {
            let mut parts = String::new();
            if self.total_cache_read > 0 {
                parts.push_str(&format!(" {}Rc{}{}", dim, fmt_tokens_u64(self.total_cache_read), r));
            }
            if self.total_cache_write > 0 {
                parts.push_str(&format!(" {}Wc{}{}", dim, fmt_tokens_u64(self.total_cache_write), r));
            }
            parts
        };
        let total_cost = self.cost_input + self.cost_output;
        let cost = if total_cost > 0.001 {
            format!("{}${:.3}{}", theme::COST, total_cost, r)
        } else {
            String::new()
        };
        // Show cumulative API usage breakdown alongside the cost — always visible.
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
        let api_info = format!(
            "{}({} calls, {} tok in{}, {} tok out{}){}",
            dim,
            self.api_calls,
            fmt_tokens_u64(self.total_input),
            in_cost,
            fmt_tokens_u64(self.total_output),
            out_cost,
            r,
        );

        // Line 4: centered token counter + cache stats
        let counter_line = format!("{}{}", counter, cache_stats);
        let counter_width = visible_width(&counter_line) as usize;
        let counter_pad = w.saturating_sub(counter_width) / 2;
        let line4 = format!("{}{}", " ".repeat(counter_pad), counter_line);

        // Line 5: centered cost + api call breakdown (always shown)
        let cost_line = if cost.is_empty() {
            api_info.clone()
        } else {
            format!("{} {}", cost, api_info)
        };
        let cost_width = visible_width(&cost_line) as usize;
        let cost_pad = w.saturating_sub(cost_width) / 2;
        let line5 = format!("{}{}", " ".repeat(cost_pad), cost_line);

        // nervHud: process RSS — shown only when NERV_DEBUG=1
        if nerv_debug() {
            let rss = self.rss_kb.load(Ordering::Relaxed);
            let rss_str = if rss >= 1024 {
                format!("{:.1} MB", rss as f64 / 1024.0)
            } else {
                format!("{} KB", rss)
            };
            let hud_line = format!(
                "{}nervHud  rss {}{}",
                theme::HUD_PINK,
                rss_str,
                r,
            );
            vec![line1, line2, hex_bar, line4, line5, hud_line]
        } else {
            vec![line1, line2, hex_bar, line4, line5]
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_rss_returns_sane_value() {
        let kb = sample_rss_kb();
        eprintln!("sample_rss_kb() = {} KB ({:.1} MB)", kb, kb as f64 / 1024.0);
        // A Rust test binary should use at least a few MB of RSS.
        // 150 KB would indicate the bug (thread-only or wrong struct).
        assert!(kb > 1024, "RSS {} KB is suspiciously low — probably not whole-process", kb);
        // And it shouldn't be insanely high (sanity: <16 GB).
        assert!(kb < 16 * 1024 * 1024, "RSS {} KB is suspiciously high", kb);
    }
}
