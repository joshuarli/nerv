use std::time::Duration;

/// Configurable retry policy with exponential backoff.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub backoff_factor: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(5),
            backoff_factor: 2.0,
        }
    }
}

impl RetryPolicy {
    pub fn no_retry() -> Self {
        Self {
            max_retries: 0,
            ..Default::default()
        }
    }

    pub fn aggressive() -> Self {
        Self {
            max_retries: 5,
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_secs(10),
            backoff_factor: 3.0,
        }
    }

    /// Calculate the delay for a given attempt number.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let delay_ms = self.base_delay.as_millis() as f64
            * self.backoff_factor.powi(attempt as i32);
        let capped = Duration::from_millis(delay_ms as u64);
        capped.min(self.max_delay)
    }

    /// Total maximum wait time across all retries.
    pub fn total_max_wait(&self) -> Duration {
        let mut total = Duration::ZERO;
        for i in 0..self.max_retries {
            total += self.delay_for_attempt(i);
        }
        total
    }
}
