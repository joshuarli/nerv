use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::agent::provider::CancelFlag;
use crate::errors::ToolError;
use crate::index::codemap::{self, CodemapParams};
use crate::index::SymbolIndex;

pub struct CodemapTool {
    cwd: PathBuf,
    index: Arc<RwLock<SymbolIndex>>,
}

impl CodemapTool {
    pub fn new(cwd: PathBuf, index: Arc<RwLock<SymbolIndex>>) -> Self {
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
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn prompt_guidelines(&self) -> Vec<String> {
        vec![
            "Call with `query: \"\"` and a `file` filter to get all signatures in a file. Use `depth: full` only when you need the body of a *specific named symbol* — pass a non-empty query so you get just that symbol, not the whole file. To read an entire file's source, use `read` instead.".into(),
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

    fn execute(&self, input: serde_json::Value, _update: UpdateCallback, _cancel: &CancelFlag) -> ToolResult {
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

        let params = CodemapParams {
            query,
            kind,
            file: file_path.as_deref(),
            depth,
        };

        // Fast path: check freshness under a read lock.
        // Only take a write lock if something on disk has actually changed.
        if !self.index.read().unwrap().is_fresh(&self.cwd) {
            self.index.write().unwrap().index_dir(&self.cwd);
        }
        let (search_result, cached_sources) = {
            let idx = self.index.read().unwrap();
            let result = codemap::search(&idx, &params);
            // Clone Arc<String> pointers while holding the read lock — zero-copy,
            // just bumps reference counts.  render() uses these to skip I/O entirely
            // on warm calls.
            let sources = if let codemap::SearchResult::Found(ref syms) = result {
                let paths: Vec<&std::path::Path> = syms.iter().map(|s| s.file.as_path()).collect();
                idx.sources_for(&paths)
            } else {
                std::collections::HashMap::new()
            };
            (result, sources)
        };

        let content = match search_result {
            codemap::SearchResult::Found(results) => {
                codemap::render(&results, &self.cwd, &params.depth, &cached_sources)
            }
            codemap::SearchResult::Redirect(msg) => msg,
            codemap::SearchResult::Empty => "No symbols found".to_string(),
        };

        let sym_count = if content == "No symbols found" || content.starts_with("No symbols matching") {
            0
        } else {
            content.lines().filter(|l| l.ends_with(':')).count()
        };
        let display = format!("{} files", sym_count);
        ToolResult::ok_with_details(content, serde_json::json!({"display": display}))
    }
}
