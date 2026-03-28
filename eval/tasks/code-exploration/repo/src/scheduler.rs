use std::time::{Duration, Instant};

/// Controls when pipeline batches should be processed.
pub struct Scheduler {
    interval: Duration,
    last_run: Option<Instant>,
    max_runs: Option<usize>,
    run_count: std::cell::Cell<usize>,
}

impl Scheduler {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_run: None,
            max_runs: None,
            run_count: std::cell::Cell::new(0),
        }
    }

    pub fn with_max_runs(mut self, max: usize) -> Self {
        self.max_runs = Some(max);
        self
    }

    /// Check if the pipeline should process the next batch.
    pub fn should_run(&self) -> bool {
        if let Some(max) = self.max_runs {
            if self.run_count.get() >= max {
                return false;
            }
        }
        self.run_count.set(self.run_count.get() + 1);
        true
    }

    pub fn run_count(&self) -> usize {
        self.run_count.get()
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }
}

/// A rate limiter that works with the scheduler.
pub struct RateLimiter {
    tokens: std::cell::Cell<u32>,
    max_tokens: u32,
    refill_rate: u32,
}

impl RateLimiter {
    pub fn new(max_tokens: u32, refill_rate: u32) -> Self {
        Self {
            tokens: std::cell::Cell::new(max_tokens),
            max_tokens,
            refill_rate,
        }
    }

    pub fn try_acquire(&self) -> bool {
        let current = self.tokens.get();
        if current > 0 {
            self.tokens.set(current - 1);
            true
        } else {
            false
        }
    }

    pub fn refill(&self) {
        let current = self.tokens.get();
        self.tokens
            .set((current + self.refill_rate).min(self.max_tokens));
    }

    pub fn available(&self) -> u32 {
        self.tokens.get()
    }
}
