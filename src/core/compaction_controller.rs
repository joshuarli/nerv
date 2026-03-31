use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::compaction::CompactionSettings;

/// Groups all compaction-related state that was previously scattered across
/// AgentSession. AgentSession holds one of these as `pub compaction`.
pub struct CompactionController {
    pub settings: CompactionSettings,
    /// 0–100; shared with the UI so `/compact at N` takes effect immediately.
    pub threshold_pct: Arc<AtomicU32>,
    /// Set by the UsageUpdate callback when mid-stream threshold is crossed.
    /// Checked after prompt() returns to decide whether to compact + retry.
    pub triggered: Arc<AtomicBool>,
    /// Whether automatic threshold-based compaction is enabled for this session.
    pub auto_compact: bool,
}

impl Default for CompactionController {
    fn default() -> Self {
        Self {
            settings: CompactionSettings::default(),
            threshold_pct: Arc::new(AtomicU32::new(80)),
            triggered: Arc::new(AtomicBool::new(false)),
            auto_compact: true,
        }
    }
}

impl CompactionController {
    pub fn reset_triggered(&self) {
        self.triggered.store(false, Ordering::Relaxed);
    }

    /// Returns true and clears the flag if a mid-stream trigger fired.
    pub fn check_and_clear_triggered(&self) -> bool {
        self.triggered.swap(false, Ordering::Relaxed)
    }
}
