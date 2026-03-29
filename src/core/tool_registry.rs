use std::sync::Arc;

use crate::agent::agent::AgentTool;

pub struct ToolDefinition {
    pub tool: Arc<dyn AgentTool>,
}

pub struct ToolRegistry {
    all: Vec<(String, ToolDefinition)>,
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

    pub fn register(&mut self, def: ToolDefinition) {
        let name = def.tool.name().to_string();
        self.all.push((name, def));
    }

    pub fn set_active(&mut self, names: &[&str]) {
        self.active = names.iter().map(|s| s.to_string()).collect();
    }

    pub fn active_tools(&self) -> Vec<Arc<dyn AgentTool>> {
        if self.active.is_empty() {
            return self.all.iter().map(|(_, d)| d.tool.clone()).collect();
        }
        self.active
            .iter()
            .filter_map(|name| {
                self.all.iter().find(|(n, _)| n == name).map(|(_, d)| d.tool.clone())
            })
            .collect()
    }

    pub fn prompt_snippets(&self) -> Vec<(String, String)> {
        self.all
            .iter()
            .filter_map(|(name, def)| {
                def.tool.prompt_snippet().map(|s| (name.clone(), s.to_string()))
            })
            .collect()
    }

    pub fn prompt_guidelines(&self) -> Vec<String> {
        self.all.iter().flat_map(|(_, def)| def.tool.prompt_guidelines()).collect()
    }
}
