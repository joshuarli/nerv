use std::collections::HashMap;

/// Collects operational metrics for monitoring and alerting.
pub struct MetricsCollector {
    counters: HashMap<String, u64>,
    timings: HashMap<String, Vec<f64>>,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            counters: HashMap::new(),
            timings: HashMap::new(),
        }
    }

    /// Track a named event with an associated entity ID.
    pub fn track_event(&mut self, event: &str, entity_id: &str) {
        let key = format!("{}:{}", event, entity_id);
        *self.counters.entry(event.to_string()).or_default() += 1;
        *self.counters.entry(key).or_default() += 1;
    }

    /// Record a timing measurement (in milliseconds).
    pub fn record_timing(&mut self, metric: &str, ms: f64) {
        self.timings.entry(metric.to_string()).or_default().push(ms);
    }

    pub fn get_count(&self, event: &str) -> u64 {
        *self.counters.get(event).unwrap_or(&0)
    }

    pub fn get_avg_timing(&self, metric: &str) -> Option<f64> {
        let timings = self.timings.get(metric)?;
        if timings.is_empty() {
            return None;
        }
        Some(timings.iter().sum::<f64>() / timings.len() as f64)
    }

    pub fn get_p99_timing(&self, metric: &str) -> Option<f64> {
        let timings = self.timings.get(metric)?;
        if timings.is_empty() {
            return None;
        }
        let mut sorted = timings.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let idx = ((sorted.len() as f64 * 0.99) as usize).min(sorted.len() - 1);
        Some(sorted[idx])
    }

    /// Export all metrics as a flat map for reporting.
    pub fn export(&self) -> HashMap<String, f64> {
        let mut out = HashMap::new();
        for (k, v) in &self.counters {
            out.insert(format!("counter.{}", k), *v as f64);
        }
        for (k, timings) in &self.timings {
            if !timings.is_empty() {
                let avg = timings.iter().sum::<f64>() / timings.len() as f64;
                out.insert(format!("timing.{}.avg", k), avg);
                out.insert(format!("timing.{}.count", k), timings.len() as f64);
            }
        }
        out
    }
}
