use std::sync::Arc;

use crate::agent::agent::AgentTool;

pub struct ToolRegistry {
    all: Vec<Arc<dyn AgentTool>>,
    active: Vec<String>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { all: Vec::new(), active: Vec::new() }
    }

    pub fn register(&mut self, tool: Arc<dyn AgentTool>) {
        self.all.push(tool);
    }

    pub fn set_active(&mut self, names: &[&str]) {
        self.active = names.iter().map(|s| s.to_string()).collect();
    }

    pub fn active_tools(&self) -> Vec<Arc<dyn AgentTool>> {
        if self.active.is_empty() {
            return self.all.clone();
        }
        self.active
            .iter()
            .filter_map(|name| self.all.iter().find(|t| t.name() == name).cloned())
            .collect()
    }

    pub fn prompt_snippets(&self) -> Vec<(String, String)> {
        self.all
            .iter()
            .filter_map(|t| t.prompt_snippet().map(|s| (t.name().to_string(), s.to_string())))
            .collect()
    }

    pub fn prompt_guidelines(&self) -> Vec<String> {
        self.all.iter().flat_map(|t| t.prompt_guidelines()).collect()
    }
}
