use std::path::PathBuf;

use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::errors::ToolError;

pub struct MemoryTool {
    memory_path: PathBuf,
}

impl MemoryTool {
    pub fn new(nerv_dir: PathBuf) -> Self {
        Self {
            memory_path: nerv_dir.join("memory.md"),
        }
    }

    fn read_memories(&self) -> Vec<String> {
        std::fs::read_to_string(&self.memory_path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect()
    }

    fn write_memories(&self, memories: &[String]) -> std::io::Result<()> {
        let content = memories.join("\n") + "\n";
        std::fs::write(&self.memory_path, content)
    }
}

impl AgentTool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Read, add, or remove persistent memory entries. Memories persist across sessions and are included in every system prompt. Use sparingly — only for high-value patterns, preferences, and project facts that would be costly to rediscover. Each memory is a single compressed line."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "add", "remove"],
                    "description": "list: show all memories. add: add a new memory. remove: delete a memory by number."
                },
                "content": {
                    "type": "string",
                    "description": "For 'add': the memory to store (single line, compressed). For 'remove': the 1-based index to delete."
                }
            },
            "required": ["action"]
        })
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("memory — read/write persistent memories (use sparingly for high-value patterns only)")
    }

    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        let action = input["action"].as_str().unwrap_or("");
        if !["list", "add", "remove"].contains(&action) {
            return Err(ToolError::InvalidArguments {
                message: "action must be list, add, or remove".into(),
            });
        }
        if action == "add"
            && input["content"]
                .as_str()
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err(ToolError::InvalidArguments {
                message: "content is required for add".into(),
            });
        }
        if action == "remove" && input["content"].as_str().is_none() {
            return Err(ToolError::InvalidArguments {
                message: "content (index number) is required for remove".into(),
            });
        }
        Ok(())
    }

    fn execute(&self, input: serde_json::Value, _update: UpdateCallback) -> ToolResult {
        let action = input["action"].as_str().unwrap_or("list");
        let mut memories = self.read_memories();

        match action {
            "list" => {
                if memories.is_empty() {
                    ToolResult {
                        content: "No memories stored.".into(),
                        details: None,
                        is_error: false,
                    }
                } else {
                    let display: Vec<String> = memories
                        .iter()
                        .enumerate()
                        .map(|(i, m)| format!("{}. {}", i + 1, m))
                        .collect();
                    ToolResult {
                        content: display.join("\n"),
                        details: None,
                        is_error: false,
                    }
                }
            }
            "add" => {
                let content = input["content"].as_str().unwrap_or("").trim().to_string();
                // Compress to single line
                let line = content.replace('\n', " ");
                memories.push(line.clone());
                match self.write_memories(&memories) {
                    Ok(()) => ToolResult {
                        content: format!("Memory added: {}", line),
                        details: None,
                        is_error: false,
                    },
                    Err(e) => ToolResult {
                        content: format!("Failed to write memory: {}", e),
                        details: None,
                        is_error: true,
                    },
                }
            }
            "remove" => {
                let idx_str = input["content"].as_str().unwrap_or("0");
                let idx: usize = idx_str.parse().unwrap_or(0);
                if idx == 0 || idx > memories.len() {
                    return ToolResult {
                        content: format!(
                            "Invalid index: {}. Use 'list' to see available memories.",
                            idx_str
                        ),
                        details: None,
                        is_error: true,
                    };
                }
                let removed = memories.remove(idx - 1);
                match self.write_memories(&memories) {
                    Ok(()) => ToolResult {
                        content: format!("Removed: {}", removed),
                        details: None,
                        is_error: false,
                    },
                    Err(e) => ToolResult {
                        content: format!("Failed to write memory: {}", e),
                        details: None,
                        is_error: true,
                    },
                }
            }
            _ => ToolResult {
                content: "Unknown action".into(),
                details: None,
                is_error: true,
            },
        }
    }
}
