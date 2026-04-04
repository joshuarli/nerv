use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::agent::agent::{AgentTool, ToolResult};
use crate::agent::provider::CancelFlag;
use crate::agent::types::ToolDetails;
use crate::errors::ToolError;
use crate::index::{SymbolIndex, SymbolKind};

pub struct SymbolsTool {
    cwd: PathBuf,
    index: Arc<RwLock<SymbolIndex>>,
}

impl SymbolsTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd, index: Arc::new(RwLock::new(SymbolIndex::new())) }
    }

    /// Construct with a persistent on-disk symbol cache stored in `nerv_dir`.
    /// If `cwd` is inside a git repo, paths are stored relative to the repo
    /// root so the cache survives directory renames.
    ///
    /// When `cwd` is not inside a git repo we skip the on-disk cache entirely —
    /// there is no stable fingerprint to key the per-repo directory on, and the
    /// fallback from `repo_data_dir` would be `~/.nerv` itself, causing
    /// `~/.nerv/symbol_cache.db` to accumulate entries from arbitrary
    /// directories.
    pub fn new_with_cache(cwd: PathBuf, nerv_dir: &std::path::Path) -> Self {
        let _ = nerv_dir; // kept in signature for API stability; path now derived from cwd
        // Only open a cache when we have a stable per-repo path. Without a
        // fingerprint `repo_data_dir` falls back to `~/.nerv`, which would
        // create a global `~/.nerv/symbol_cache.db` mixing entries from every
        // project.
        let index = match crate::find_repo_root(&cwd) {
            Some(repo_root) if crate::repo_fingerprint(&repo_root).is_some() => {
                let repo_dir = crate::repo_data_dir(&cwd);
                crate::index::SymbolIndex::new_with_cache_and_root(&repo_dir, &repo_root)
            }
            _ => crate::index::SymbolIndex::new(),
        };
        Self { cwd, index: Arc::new(RwLock::new(index)) }
    }

    pub fn index(&self) -> Arc<RwLock<SymbolIndex>> {
        self.index.clone()
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        crate::resolve_path(path, &self.cwd)
    }
}

const MAX_RESULTS: usize = 50;
const SYMBOLS_ALLOWED_KEYS: &[&str] = &["query", "kind", "file", "references"];

fn normalize_empty_query_literal(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "\"\"" || trimmed == "''" { Some("") } else { None }
}

impl AgentTool for SymbolsTool {
    fn name(&self) -> &str {
        "symbols"
    }
    fn is_readonly(&self) -> bool {
        true
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
                    "description": "Also find usages (definitions excluded), AST-first with per-file rg fallback (default: false)"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn prompt_guidelines(&self) -> Vec<String> {
        vec![
            "`symbols` with `query: \"\"` returns every definition (name, kind, file, line, signature). Use to orient before targeted `codemap` calls.".into(),
            "Canonical empty query is exactly `\"\"`; do not pass the literal quoted text `\"\\\"\\\"\"`.".into(),
            "Use `references: true` only with a non-empty identifier query. References are usages-only (definitions excluded), AST-first for Rust/Go/Python/TS/TSX with per-file fallback.".into(),
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
        super::validate_known_keys(input, SYMBOLS_ALLOWED_KEYS)?;
        let query_raw = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or(ToolError::InvalidArguments { message: "query (string) is required".into() })?;
        let query = normalize_empty_query_literal(query_raw).unwrap_or(query_raw);
        let want_refs = input.get("references").and_then(|v| v.as_bool()).unwrap_or(false);
        if want_refs && query.trim().is_empty() {
            return Err(ToolError::InvalidArguments {
                message: "query must be non-empty when references=true".into(),
            });
        }
        Ok(())
    }

    fn execute(&self, input: serde_json::Value, _cancel: &CancelFlag) -> ToolResult {
        let query_raw = input["query"].as_str().unwrap_or("");
        let query = normalize_empty_query_literal(query_raw).unwrap_or(query_raw);
        let kind_filter = input.get("kind").and_then(|v| v.as_str()).and_then(parse_kind_filter);
        let file_filter = input.get("file").and_then(|v| v.as_str());
        let want_refs = input.get("references").and_then(|v| v.as_bool()).unwrap_or(false);

        let file_path = file_filter.map(|f| self.resolve_path(f));

        // Fast path: read lock only — check if index is already current.
        // Only escalate to a write lock when files have actually changed.
        if !self.index.read().unwrap().is_fresh(&self.cwd) {
            self.index.write().unwrap().index_dir(&self.cwd);
        }
        let idx = self.index.read().unwrap();
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
                let rel = sym.file.strip_prefix(&self.cwd).unwrap_or(&sym.file).display();
                let parent_suffix =
                    sym.parent.as_ref().map(|p| format!("  ({})", p)).unwrap_or_default();
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

        // List doc files on broad queries (empty query, no file filter)
        if query.is_empty() && file_filter.is_none() {
            let docs = find_doc_files(&self.cwd);
            if !docs.is_empty() {
                out.push_str("\nDOCS:\n");
                for doc in &docs {
                    out.push_str(&format!("  {}\n", doc));
                }
            }
        }

        // Reference search (AST-first, with per-file ripgrep fallback)
        if want_refs {
            match crate::index::references::find_references(
                &idx,
                query,
                &self.cwd,
                file_path.as_deref(),
            ) {
                Ok(refs) => {
                    if !refs.is_empty() {
                        out.push_str("\nREFERENCES:\n");
                        for hit in refs.iter().take(MAX_RESULTS) {
                            out.push_str(&format!(
                                "  {}\n",
                                format_reference_line(
                                    hit.file.as_path(),
                                    hit.line,
                                    &hit.context,
                                    &self.cwd
                                )
                            ));
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
                Err(message) => {
                    return ToolResult::error(format!("invalid arguments: {}", message));
                }
            }
        }

        let def_count = total.min(MAX_RESULTS);
        let display = if want_refs {
            format!("{} definitions + references", def_count)
        } else {
            format!("{} definitions", def_count)
        };
        ToolResult::ok_with_details(
            out,
            ToolDetails { display: Some(display), ..Default::default() },
        )
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

/// Find markdown doc files in the project root.
fn find_doc_files(cwd: &std::path::Path) -> Vec<String> {
    // Prefer git ls-files: respects .gitignore, any depth, no extra deps.
    let output = std::process::Command::new(crate::git())
        .args(["ls-files", "--", "*.md"])
        .current_dir(cwd)
        .output();
    if let Ok(out) = output
        && out.status.success()
    {
        let mut files: Vec<String> =
            String::from_utf8_lossy(&out.stdout).lines().map(|l| l.to_owned()).collect();
        files.sort();
        return files;
    }
    // Outside a git repo (e.g. tests): fall back to rg.
    if let Some(rg) = crate::rg() {
        let output = std::process::Command::new(rg)
            .args(["--files", "--glob", "*.md", "--sort=path"])
            .current_dir(cwd)
            .output();
        if let Ok(out) = output
            && out.status.success()
        {
            return String::from_utf8_lossy(&out.stdout).lines().map(|l| l.to_owned()).collect();
        }
    }
    vec![]
}

fn format_reference_line(file: &Path, line: u32, context: &str, cwd: &Path) -> String {
    let rel = file.strip_prefix(cwd).unwrap_or(file).display();
    format!("{}:{}:{}", rel, line, context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::agent::AgentTool;
    use crate::agent::provider::new_cancel_flag;

    #[test]
    fn docs_section_on_empty_query() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("lib.rs"), "fn hello() {}\n").unwrap();
        std::fs::write(tmp.path().join("README.md"), "# Hello\n").unwrap();
        std::fs::write(tmp.path().join("docs.md"), "# Docs\n").unwrap();
        // Init a git repo and stage files so git ls-files works (no rg dependency).
        std::process::Command::new("git").args(["init"]).current_dir(tmp.path()).output().unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let tool = SymbolsTool::new(tmp.path().to_path_buf());
        let cancel = new_cancel_flag();
        let result = tool.execute(serde_json::json!({"query": ""}), &cancel);
        assert!(result.content.contains("DOCS:"), "should have DOCS section: {}", result.content);
        assert!(result.content.contains("README.md"), "should list README.md: {}", result.content);
        assert!(result.content.contains("docs.md"), "should list docs.md: {}", result.content);
    }

    #[test]
    fn no_docs_section_on_specific_query() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("lib.rs"), "fn hello() {}\n").unwrap();
        std::fs::write(tmp.path().join("README.md"), "# Hello\n").unwrap();

        let tool = SymbolsTool::new(tmp.path().to_path_buf());
        let cancel = new_cancel_flag();
        let result = tool.execute(serde_json::json!({"query": "hello"}), &cancel);
        assert!(
            !result.content.contains("DOCS:"),
            "specific query should NOT have DOCS: {}",
            result.content
        );
    }

    #[test]
    fn no_docs_section_with_file_filter() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("lib.rs"), "fn hello() {}\n").unwrap();
        std::fs::write(tmp.path().join("README.md"), "# Hello\n").unwrap();

        let tool = SymbolsTool::new(tmp.path().to_path_buf());
        let cancel = new_cancel_flag();
        let result = tool.execute(serde_json::json!({"query": "", "file": "lib.rs"}), &cancel);
        assert!(
            !result.content.contains("DOCS:"),
            "file-filtered query should NOT have DOCS: {}",
            result.content
        );
    }

    #[test]
    fn normalize_converts_literal_quoted_empty_query() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = SymbolsTool::new(tmp.path().to_path_buf());
        let normalized = tool.normalize(serde_json::json!({"query": "\"\""}));
        assert_eq!(normalized["query"], serde_json::json!(""));
    }

    #[test]
    fn validate_rejects_unknown_argument() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = SymbolsTool::new(tmp.path().to_path_buf());
        let err = tool.validate(&serde_json::json!({"query": "hello", "bogus": true})).unwrap_err();
        assert!(err.to_string().contains("unknown argument"), "{}", err);
    }

    #[test]
    fn validate_references_requires_non_empty_query() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = SymbolsTool::new(tmp.path().to_path_buf());
        let err = tool
            .validate(&serde_json::json!({"query": "", "references": true}))
            .unwrap_err();
        assert!(err.to_string().contains("non-empty"), "{}", err);
    }

    #[test]
    fn validate_references_requires_non_whitespace_query() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = SymbolsTool::new(tmp.path().to_path_buf());
        let err = tool
            .validate(&serde_json::json!({"query": "   ", "references": true}))
            .unwrap_err();
        assert!(err.to_string().contains("non-empty"), "{}", err);
    }

    #[test]
    fn references_output_is_deterministically_ordered() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn foo() {}\nfn bar() { foo(); }\n").unwrap();
        std::fs::write(tmp.path().join("b.rs"), "fn baz() { foo(); }\n").unwrap();

        // Run twice; output should be identical.
        let tool = SymbolsTool::new(tmp.path().to_path_buf());
        let cancel = new_cancel_flag();
        let r1 = tool.execute(serde_json::json!({"query": "foo", "references": true}), &cancel);
        let r2 = tool.execute(serde_json::json!({"query": "foo", "references": true}), &cancel);
        assert_eq!(r1.content, r2.content, "references output must be deterministic");
    }
}
