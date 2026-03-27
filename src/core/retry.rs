use crate::agent::provider::CancelFlag;
use crate::agent::types::{AssistantMessage, StopReason};
use std::sync::atomic::Ordering;

pub struct RetrySettings {
    pub enabled: bool,
    pub max_attempts: u32,
    pub initial_delay_ms: u64,
    pub backoff_factor: f64,
    pub max_delay_ms: u64,
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: 3,
            initial_delay_ms: 1_000,
            backoff_factor: 2.0,
            max_delay_ms: 30_000,
        }
    }
}

pub enum RetryDecision {
    Continue,
    GiveUp,
}

pub struct RetryManager {
    settings: RetrySettings,
    attempt: u32,
}

impl RetryManager {
    pub fn new(settings: RetrySettings) -> Self {
        Self {
            settings,
            attempt: 0,
        }
    }

    pub fn is_retryable(&self, msg: &AssistantMessage) -> bool {
        if !self.settings.enabled || self.attempt >= self.settings.max_attempts {
            return false;
        }
        match &msg.stop_reason {
            StopReason::Error { message } => {
                let m = message.to_lowercase();
                m.contains("overloaded")
                    || m.contains("rate limit")
                    || m.contains("529")
                    || m.contains("503")
                    || m.contains("500")
                    || m.contains("server error")
            }
            _ => false,
        }
    }

    pub fn wait(&mut self, cancel: &CancelFlag) -> RetryDecision {
        self.attempt += 1;
        if self.attempt > self.settings.max_attempts {
            return RetryDecision::GiveUp;
        }
        let delay = (self.settings.initial_delay_ms as f64
            * self.settings.backoff_factor.powi(self.attempt as i32 - 1))
            as u64;
        let delay = delay.min(self.settings.max_delay_ms);
        // Sleep in small increments checking cancellation
        let mut remaining = delay;
        while remaining > 0 && !cancel.load(Ordering::Relaxed) {
            let chunk = remaining.min(100);
            std::thread::sleep(std::time::Duration::from_millis(chunk));
            remaining -= chunk;
        }
        if cancel.load(Ordering::Relaxed) {
            RetryDecision::GiveUp
        } else {
            RetryDecision::Continue
        }
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}
