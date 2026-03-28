/// Trait for pipeline filters. Each filter can accept/reject items
/// and optionally transform them.
pub trait Filter: Send {
    fn name(&self) -> &str;
    fn accept(&self, item: &str) -> bool;

    /// Transform an accepted item. Default: identity.
    fn transform(&self, item: String) -> String {
        item
    }
}

/// Rejects items shorter than a minimum length.
pub struct MinLengthFilter {
    min: usize,
}

impl MinLengthFilter {
    pub fn new(min: usize) -> Self {
        Self { min }
    }
}

impl Filter for MinLengthFilter {
    fn name(&self) -> &str {
        "min_length"
    }

    fn accept(&self, item: &str) -> bool {
        item.len() >= self.min
    }
}

/// Rejects items matching a regex pattern.
pub struct RegexRejectFilter {
    pattern: String,
}

impl RegexRejectFilter {
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
        }
    }
}

impl Filter for RegexRejectFilter {
    fn name(&self) -> &str {
        "regex_reject"
    }

    fn accept(&self, item: &str) -> bool {
        !item.contains(&self.pattern)
    }
}

/// Transforms items by trimming whitespace and converting to lowercase.
pub struct NormalizeFilter;

impl Filter for NormalizeFilter {
    fn name(&self) -> &str {
        "normalize"
    }

    fn accept(&self, _item: &str) -> bool {
        true
    }

    fn transform(&self, item: String) -> String {
        item.trim().to_lowercase()
    }
}

/// Deduplication filter — tracks seen items and rejects repeats.
pub struct DeduplicateFilter {
    seen: std::cell::RefCell<std::collections::HashSet<String>>,
}

impl DeduplicateFilter {
    pub fn new() -> Self {
        Self {
            seen: std::cell::RefCell::new(std::collections::HashSet::new()),
        }
    }
}

impl Filter for DeduplicateFilter {
    fn name(&self) -> &str {
        "deduplicate"
    }

    fn accept(&self, item: &str) -> bool {
        self.seen.borrow_mut().insert(item.to_string())
    }
}
