use crate::filters::Filter;

/// A data processing pipeline that applies a chain of filters to items.
pub struct Pipeline {
    filters: Vec<Box<dyn Filter>>,
    stats: PipelineStats,
}

#[derive(Default, Debug)]
pub struct PipelineStats {
    pub items_in: usize,
    pub items_out: usize,
    pub items_dropped: usize,
}

impl Pipeline {
    pub fn new() -> Self {
        Self {
            filters: Vec::new(),
            stats: PipelineStats::default(),
        }
    }

    pub fn add_filter(&mut self, filter: Box<dyn Filter>) {
        self.filters.push(filter);
    }

    /// Process a batch of items through all filters in order.
    /// Items that fail any filter are dropped.
    pub fn process(&mut self, items: Vec<String>) -> Vec<String> {
        self.stats.items_in += items.len();
        let mut current = items;

        for filter in &self.filters {
            current = current
                .into_iter()
                .filter(|item| filter.accept(item))
                .map(|item| filter.transform(item))
                .collect();
        }

        self.stats.items_out += current.len();
        self.stats.items_dropped += self.stats.items_in - self.stats.items_out;
        current
    }

    pub fn stats(&self) -> &PipelineStats {
        &self.stats
    }

    /// Run the pipeline on a schedule, processing batches from a source.
    pub fn run_scheduled(
        &mut self,
        source: &mut dyn Iterator<Item = Vec<String>>,
        scheduler: &crate::scheduler::Scheduler,
    ) -> Vec<String> {
        let mut all_results = Vec::new();
        for batch in source {
            if !scheduler.should_run() {
                continue;
            }
            let results = self.process(batch);
            all_results.extend(results);
        }
        all_results
    }
}
