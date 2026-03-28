use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::errors::ToolError;
use crate::index::codemap::{self, CodemapParams};
use crate::index::SymbolIndex;

pub struct CodemapTool {
    cwd: PathBuf,
    index: Arc<Mutex<SymbolIndex>>,
}

impl CodemapTool {
    pub fn new(cwd: PathBuf, index: Arc<Mutex<SymbolIndex>>) -> Self {
        Self { cwd, index }
    }
}

impl AgentTool for CodemapTool {
    fn name(&self) -> &str {
        "codemap"
    }

    fn description(&self) -> &str {
        "Show symbol implementations from the codebase. Returns source bodies for matching functions, structs, traits, etc. grouped by file. Replaces multiple read calls when you need to understand how something works."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Symbol name to search for (case-insensitive substring match)"
                },
                "kind": {
                    "type": "string",
                    "enum": ["function", "method", "struct", "enum", "union", "trait", "type", "const", "module", "macro"],
                    "description": "Filter by symbol kind"
                },
                "file": {
                    "type": "string",
                    "description": "Restrict search to a file or directory path"
                },
                "depth": {
                    "type": "string",
                    "enum": ["signatures", "full"],
                    "description": "Level of detail: 'signatures' (default) for one-line signatures, 'full' for complete source bodies"
                }
            },
            "required": ["query"]
        })
    }

    fn prompt_guidelines(&self) -> Vec<String> {
        vec![
            "ALWAYS call with `query: \"\"` and a `file` filter. Non-empty queries miss definitions — never use them. One call per file, never re-read. `depth: full` = source bodies, `depth: signatures` = one-line summaries.".into(),
        ]
    }

    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        if input.get("query").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::InvalidArguments {
                message: "query (string) is required".into(),
            });
        }
        Ok(())
    }

    fn execute(&self, input: serde_json::Value, _update: UpdateCallback) -> ToolResult {
        let query = input["query"].as_str().unwrap_or("");
        let kind = input
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(codemap::parse_kind);
        let depth = input
            .get("depth")
            .and_then(|v| v.as_str())
            .map(codemap::parse_depth)
            .unwrap_or(codemap::Depth::Signatures);

        let file_str = input.get("file").and_then(|v| v.as_str());
        let file_path = file_str.map(|f| {
            if f.starts_with('/') {
                PathBuf::from(f)
            } else {
                self.cwd.join(f)
            }
        });

        let mut idx = self.index.lock().unwrap();
        idx.index_dir(&self.cwd);

        let params = CodemapParams {
            query,
            kind,
            file: file_path.as_deref(),
            depth,
        };
        let content = codemap::codemap(&idx, &self.cwd, &params);

        let sym_count = if content == "No symbols found" {
            0
        } else {
            // Count non-header, non-empty lines as a rough symbol count proxy
            content.lines().filter(|l| l.ends_with(':')).count()
        };
        let display = format!("{} files", sym_count);
        ToolResult::ok_with_details(content, serde_json::json!({"display": display}))
    }
}
