use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Caching layer in front of InventoryService for read-heavy paths.
/// Not used by the order/cancellation flow — only by the storefront
/// product pages that need fast stock-level checks.
pub struct InventoryCache {
    entries: HashMap<String, CacheEntry>,
    ttl: Duration,
}

struct CacheEntry {
    available: u32,
    fetched_at: Instant,
}

impl InventoryCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
        }
    }

    pub fn get(&self, sku: &str) -> Option<u32> {
        let entry = self.entries.get(sku)?;
        if entry.fetched_at.elapsed() > self.ttl {
            return None;
        }
        Some(entry.available)
    }

    pub fn put(&mut self, sku: String, available: u32) {
        self.entries.insert(sku, CacheEntry {
            available,
            fetched_at: Instant::now(),
        });
    }

    pub fn invalidate(&mut self, sku: &str) {
        self.entries.remove(sku);
    }

    pub fn invalidate_all(&mut self) {
        self.entries.clear();
    }

    pub fn stats(&self) -> CacheStats {
        let total = self.entries.len();
        let expired = self.entries.values()
            .filter(|e| e.fetched_at.elapsed() > self.ttl)
            .count();
        CacheStats { total, expired, active: total - expired }
    }
}

pub struct CacheStats {
    pub total: usize,
    pub expired: usize,
    pub active: usize,
}
