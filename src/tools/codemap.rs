use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::agent::agent::{AgentTool, ToolResult};
use crate::agent::provider::CancelFlag;
use crate::agent::types::ToolDetails;
use crate::errors::ToolError;
use crate::index::SymbolIndex;
use crate::index::codemap::{self, CodemapParams};

pub struct CodemapTool {
    cwd: PathBuf,
    index: Arc<RwLock<SymbolIndex>>,
}

const CODEMAP_ALLOWED_KEYS: &[&str] = &["query", "kind", "file", "depth", "match", "from"];

fn normalize_empty_query_literal(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "\"\"" || trimmed == "''" { Some("") } else { None }
}


impl CodemapTool {
    pub fn new(cwd: PathBuf, index: Arc<RwLock<SymbolIndex>>) -> Self {
        Self { cwd, index }
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        crate::resolve_path(path, &self.cwd)
    }

    fn parse_match_mode(&self, input: &serde_json::Value) -> Result<codemap::MatchMode, ToolError> {
        let Some(match_value) = input.get("match") else {
            return Ok(codemap::MatchMode::Substring);
        };
        let mode_str = match_value.as_str().ok_or(ToolError::InvalidArguments {
            message: "match must be a string: substring|exact".into(),
        })?;
        codemap::parse_match_mode(mode_str).ok_or(ToolError::InvalidArguments {
            message: "match must be one of: substring, exact".into(),
        })
    }

    fn validate_from_path(&self, input: &serde_json::Value) -> Result<Option<PathBuf>, ToolError> {
        let Some(from) = input.get("from") else {
            return Ok(None);
        };
        let from_str = from
            .as_str()
            .ok_or(ToolError::InvalidArguments { message: "from must be a string path".into() })?;
        let resolved = self.resolve_path(from_str);
        let canonical = resolved.canonicalize().map_err(|_| ToolError::InvalidArguments {
            message: format!("from path is invalid or unresolvable: {}", from_str),
        })?;
        let meta = std::fs::metadata(&canonical).map_err(|_| ToolError::InvalidArguments {
            message: format!("from path is unreadable: {}", from_str),
        })?;
        if !meta.is_file() {
            return Err(ToolError::InvalidArguments {
                message: format!("from path must be a readable file: {}", from_str),
            });
        }
        std::fs::File::open(&canonical).map_err(|_| ToolError::InvalidArguments {
            message: format!("from path is unreadable: {}", from_str),
        })?;
        Ok(Some(canonical))
    }
}

impl AgentTool for CodemapTool {
    fn name(&self) -> &str {
        "codemap"
    }
    fn is_readonly(&self) -> bool {
        true
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
                },
                "match": {
                    "type": "string",
                    "enum": ["substring", "exact"],
                    "description": "Search mode: substring (default) or exact case-sensitive symbol match"
                },
                "from": {
                    "type": "string",
                    "description": "Optional file path hint used only with match=exact to constrain disambiguation"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn prompt_guidelines(&self) -> Vec<String> {
        vec![
            "Call with `query: \"\"` and a `file` filter to get all signatures in a file. Use `depth: full` only when you need the body of a specific named symbol. Use `match: \"exact\"` for deterministic targeting; add `from` when exact matches are ambiguous across files.".into(),
            "Canonical empty query is exactly `\"\"`; do not pass the literal quoted text `\"\\\"\\\"\"`.".into(),
        ]
    }

    fn normalize(&self, mut input: serde_json::Value) -> serde_json::Value {
        if let Some(obj) = input.as_object_mut()
            && let Some(raw) = obj.get("query").and_then(|v| v.as_str())
            && let Some(normalized) = normalize_empty_query_literal(raw)
        {
            obj.insert("query".into(), serde_json::Value::String(normalized.into()));
        }
        input
    }

    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        super::validate_known_keys(input, CODEMAP_ALLOWED_KEYS)?;
        let query_raw = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or(ToolError::InvalidArguments { message: "query (string) is required".into() })?;
        let query = normalize_empty_query_literal(query_raw).unwrap_or(query_raw);
        let mode = self.parse_match_mode(input)?;
        if matches!(mode, codemap::MatchMode::Exact) {
            if query.trim().is_empty() {
                return Err(ToolError::InvalidArguments {
                    message: "query must be non-empty for match=exact".into(),
                });
            }
            if query.chars().count() < 2 {
                return Err(ToolError::InvalidArguments {
                    message: "query must be at least 2 characters for match=exact".into(),
                });
            }
        }
        self.validate_from_path(input)?;
        Ok(())
    }

    fn execute(&self, input: serde_json::Value, _cancel: &CancelFlag) -> ToolResult {
        let match_mode = match self.parse_match_mode(&input) {
            Ok(mode) => mode,
            Err(err) => return ToolResult::error(err.to_string()),
        };
        let query_raw = input["query"].as_str().unwrap_or("");
        let query = normalize_empty_query_literal(query_raw).unwrap_or(query_raw);
        let kind = input.get("kind").and_then(|v| v.as_str()).and_then(codemap::parse_kind);
        let depth = input
            .get("depth")
            .and_then(|v| v.as_str())
            .map(codemap::parse_depth)
            .unwrap_or(codemap::Depth::Signatures);

        let file_str = input.get("file").and_then(|v| v.as_str());
        let file_path = file_str.map(|f| self.resolve_path(f));
        let from = match self.validate_from_path(&input) {
            Ok(from) => from,
            Err(err) => return ToolResult::error(err.to_string()),
        };

        let params = CodemapParams {
            query,
            kind,
            file: file_path.as_deref(),
            depth,
            match_mode,
            from: from.as_deref(),
        };

        // Fast path: check freshness under a read lock.
        // Only take a write lock if something on disk has actually changed.
        if !self.index.read().unwrap().is_fresh(&self.cwd) {
            self.index.write().unwrap().index_dir(&self.cwd);
        }
        let search_result = {
            let idx = self.index.read().unwrap();
            codemap::search(&idx, &params)
        };

        let content = codemap::format_search_result(search_result, &self.cwd, &params.depth);

        let sym_count = if content == "No symbols found"
            || content.starts_with("No symbols matching")
            || content.starts_with("No exact symbol")
            || content.starts_with("Ambiguous exact symbol")
        {
            0
        } else {
            content.lines().filter(|l| l.ends_with(':')).count()
        };
        let display = format!("{} files", sym_count);
        ToolResult::ok_with_details(
            content,
            ToolDetails { display: Some(display), ..Default::default() },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::agent::AgentTool;

    fn setup_tool(root: &std::path::Path) -> CodemapTool {
        CodemapTool::new(root.to_path_buf(), Arc::new(RwLock::new(SymbolIndex::new())))
    }

    #[test]
    fn exact_query_length_gate_is_enforced() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = setup_tool(tmp.path());
        let err = tool
            .validate(&serde_json::json!({
                "query": "a",
                "match": "exact"
            }))
            .unwrap_err();
        assert!(err.to_string().contains("at least 2 characters"));
    }

    #[test]
    fn invalid_from_path_is_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = setup_tool(tmp.path());
        let err = tool
            .validate(&serde_json::json!({
                "query": "target",
                "match": "exact",
                "from": "missing.rs"
            }))
            .unwrap_err();
        assert!(err.to_string().contains("invalid or unresolvable"));
    }

    #[test]
    fn readable_from_file_is_accepted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let from = tmp.path().join("lib.rs");
        std::fs::write(&from, "fn target() {}\n").unwrap();
        let tool = setup_tool(tmp.path());
        let result = tool.validate(&serde_json::json!({
            "query": "target",
            "match": "exact",
            "from": "lib.rs"
        }));
        assert!(result.is_ok(), "expected valid from file, got: {result:?}");
    }

    #[test]
    fn normalize_converts_literal_quoted_empty_query() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = setup_tool(tmp.path());
        let normalized = tool.normalize(serde_json::json!({"query": "\"\""}));
        assert_eq!(normalized["query"], serde_json::json!(""));
    }

    #[test]
    fn validate_rejects_unknown_argument() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = setup_tool(tmp.path());
        let err =
            tool.validate(&serde_json::json!({"query": "target", "unexpected": 1})).unwrap_err();
        assert!(err.to_string().contains("unknown argument"), "{}", err);
    }
}
