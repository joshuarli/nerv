use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::errors::ToolError;
use crate::index::{SymbolIndex, SymbolKind};

pub struct SymbolsTool {
    cwd: PathBuf,
    index: Arc<Mutex<SymbolIndex>>,
}

impl SymbolsTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            index: Arc::new(Mutex::new(SymbolIndex::new())),
        }
    }

    pub fn index(&self) -> Arc<Mutex<SymbolIndex>> {
        self.index.clone()
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        if let Some(rest) = path.strip_prefix('~') {
            if let Some(home) = crate::home_dir() {
                return home.join(rest.trim_start_matches('/'));
            }
        }
        if path.starts_with('/') {
            PathBuf::from(path)
        } else {
            self.cwd.join(path)
        }
    }
}

const MAX_RESULTS: usize = 50;

impl AgentTool for SymbolsTool {
    fn name(&self) -> &str {
        "symbols"
    }

    fn description(&self) -> &str {
        "Search the project's tree-sitter symbol index for definitions. Returns symbol names, kinds, file locations, and signatures. Use before reading files to understand code structure."
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
                "references": {
                    "type": "boolean",
                    "description": "Also find call sites / usages via ripgrep (default: false)"
                }
            },
            "required": ["query"]
        })
    }

    fn prompt_guidelines(&self) -> Vec<String> {
        vec![
            "Before reading a file to understand its structure, use the `symbols` tool to find definitions and call sites. Only `read` specific line ranges after.".into(),
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
        let kind_filter = input
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(parse_kind_filter);
        let file_filter = input.get("file").and_then(|v| v.as_str());
        let want_refs = input
            .get("references")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let file_path = file_filter.map(|f| self.resolve_path(f));

        let mut idx = self.index.lock().unwrap();
        idx.index_dir(&self.cwd);

        let results = idx.search(query, kind_filter, file_path.as_deref());
        let total = results.len();
        let capped = total > MAX_RESULTS;
        let show = if capped { &results[..MAX_RESULTS] } else { &results };

        let mut out = String::new();

        if show.is_empty() {
            out.push_str("No definitions found");
        } else {
            out.push_str("DEFINITIONS:\n");
            for sym in show {
                let rel = sym
                    .file
                    .strip_prefix(&self.cwd)
                    .unwrap_or(&sym.file)
                    .display();
                let parent_suffix = sym
                    .parent
                    .as_ref()
                    .map(|p| format!("  ({})", p))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "  {}:{:<4}  {} {}{}\n",
                    rel,
                    sym.line,
                    sym.kind.label(),
                    sym.signature,
                    parent_suffix,
                ));
            }
            if capped {
                out.push_str(&format!("  ... ({} total, showing {})\n", total, MAX_RESULTS));
            }
        }

        // Reference search via ripgrep
        if want_refs {
            drop(idx); // release lock before shelling out
            let refs = find_references(query, &self.cwd);
            if !refs.is_empty() {
                out.push_str("\nREFERENCES:\n");
                for line in refs.iter().take(MAX_RESULTS) {
                    out.push_str(&format!("  {}\n", line));
                }
                if refs.len() > MAX_RESULTS {
                    out.push_str(&format!(
                        "  ... ({} total, showing {})\n",
                        refs.len(),
                        MAX_RESULTS
                    ));
                }
            }
        }

        let def_count = total.min(MAX_RESULTS);
        let display = if want_refs {
            format!("{} definitions + references", def_count)
        } else {
            format!("{} definitions", def_count)
        };
        ToolResult::ok_with_details(out, serde_json::json!({"display": display}))
    }
}

fn parse_kind_filter(s: &str) -> Option<SymbolKind> {
    match s {
        "function" | "fn" => Some(SymbolKind::Function),
        "method" => Some(SymbolKind::Method),
        "struct" => Some(SymbolKind::Struct),
        "enum" => Some(SymbolKind::Enum),
        "union" => Some(SymbolKind::Union),
        "trait" => Some(SymbolKind::Trait),
        "type" => Some(SymbolKind::Type),
        "const" | "static" => Some(SymbolKind::Const),
        "module" | "mod" => Some(SymbolKind::Module),
        "macro" => Some(SymbolKind::Macro),
        _ => None,
    }
}

fn find_references(symbol: &str, cwd: &std::path::Path) -> Vec<String> {
    let output = match std::process::Command::new("rg")
        .args([
            "--no-heading",
            "--line-number",
            "--color=never",
            "--word-regexp",
            "--max-count=100",
            symbol,
        ])
        .current_dir(cwd)
        .output()
    {
        Ok(o) => o,
        Err(_) => return vec![],
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect()
}
