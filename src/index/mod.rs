pub mod codemap;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Union,
    Trait,
    Type,
    Const,
    Module,
    Macro,
}

impl SymbolKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Method => "fn",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Union => "union",
            Self::Trait => "trait",
            Self::Type => "type",
            Self::Const => "const",
            Self::Module => "mod",
            Self::Macro => "macro",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "definition.function" => Some(Self::Function),
            "definition.method" => Some(Self::Method),
            "definition.struct" => Some(Self::Struct),
            "definition.enum" => Some(Self::Enum),
            "definition.union" => Some(Self::Union),
            "definition.type" => Some(Self::Type),
            "definition.interface" => Some(Self::Trait),
            "definition.module" => Some(Self::Module),
            "definition.macro" => Some(Self::Macro),
            "definition.const" => Some(Self::Const),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolDef {
    pub name: String,
    pub kind: SymbolKind,
    pub file: PathBuf,
    pub line: u32,
    pub end_line: u32,
    pub signature: String,
    pub parent: Option<String>,
}

struct FileEntry {
    mtime: SystemTime,
    symbols: Vec<SymbolDef>,
}

/// Persistent on-disk cache of parsed symbol data, keyed by (path, mtime).
///
/// Stored in `~/.nerv/symbol_cache.db` (SQLite). On a warm start every
/// unchanged file is a cache hit, so tree-sitter only runs for files that
/// have been modified since the last scan.
struct SymbolCache {
    db: sqlite::Connection,
    /// Repo root this cache is associated with. Paths stored as relative to this root.
    /// `None` means absolute paths are used (legacy / non-git).
    repo_root: Option<PathBuf>,
}

impl SymbolCache {
    fn open(repo_dir: &Path) -> Option<Self> {
        let _ = std::fs::create_dir_all(repo_dir);
        let path = repo_dir.join("symbol_cache.db");
        let db = match sqlite::open(&path) {
            Ok(db) => db,
            Err(e) => {
                crate::log::warn(&format!("symbol cache: open failed: {}", e));
                return None;
            }
        };
        db.execute("PRAGMA journal_mode=WAL").ok();
        db.execute("PRAGMA synchronous=NORMAL").ok();
        db.execute(
            "CREATE TABLE IF NOT EXISTS symbol_cache (
                path  TEXT NOT NULL,
                mtime INTEGER NOT NULL,
                data  TEXT NOT NULL,
                PRIMARY KEY (path, mtime)
            )",
        )
        .ok()?;
        db.execute(
            "CREATE INDEX IF NOT EXISTS idx_cache_path ON symbol_cache(path)",
        )
        .ok();
        // Evict rows older than 30 days to bound cache growth.
        let cutoff_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
            - 30 * 24 * 3600 * 1000;
        if let Ok(mut stmt) = db.prepare("DELETE FROM symbol_cache WHERE mtime < ?") {
            stmt.bind((1, cutoff_ms)).ok();
            stmt.next().ok();
        }
        Some(Self { db, repo_root: None })
    }

    /// Attach a repo root so the cache stores relative paths instead of absolute ones.
    /// Relative-path keys survive directory renames transparently.
    fn with_repo_root(mut self, root: PathBuf) -> Self {
        self.repo_root = Some(root);
        self
    }

    /// Normalise an absolute path to the key stored in the DB.
    /// With a `repo_root`, this is relative to the root; otherwise it's the absolute path.
    fn cache_key<'a>(&self, path: &'a Path) -> std::borrow::Cow<'a, Path> {
        if let Some(ref root) = self.repo_root {
            if let Ok(rel) = path.strip_prefix(root) {
                return std::borrow::Cow::Owned(rel.to_path_buf());
            }
        }
        std::borrow::Cow::Borrowed(path)
    }

    /// Return cached symbols for `path` if the stored mtime matches `mtime`.
    fn get(&self, path: &Path, mtime: u128) -> Option<Vec<SymbolDef>> {
        let key = self.cache_key(path);
        let key_str = key.to_string_lossy();
        let mut stmt = self
            .db
            .prepare("SELECT data FROM symbol_cache WHERE path = ? AND mtime = ?")
            .ok()?;
        stmt.bind((1, key_str.as_ref())).ok()?;
        stmt.bind((2, mtime as i64)).ok()?;
        if stmt.next().ok()? == sqlite::State::Row {
            let json: String = stmt.read("data").ok()?;
            serde_json::from_str(&json).ok()
        } else {
            None
        }
    }

    /// Store symbols for `path` at `mtime`, evicting any older entries.
    fn put(&self, path: &Path, mtime: u128, symbols: &[SymbolDef]) {
        let key = self.cache_key(path);
        let key_str = key.to_string_lossy();
        let json = match serde_json::to_string(symbols) {
            Ok(j) => j,
            Err(e) => {
                crate::log::warn(&format!("symbol cache: serialize failed for {}: {}", key_str, e));
                return;
            }
        };
        // Delete all rows for this path, then insert the fresh one.
        if let Ok(mut stmt) = self.db.prepare("DELETE FROM symbol_cache WHERE path = ?") {
            stmt.bind((1, key_str.as_ref())).ok();
            stmt.next().ok();
        }
        if let Ok(mut stmt) = self
            .db
            .prepare("INSERT INTO symbol_cache (path, mtime, data) VALUES (?, ?, ?)")
        {
            stmt.bind((1, key_str.as_ref())).ok();
            stmt.bind((2, mtime as i64)).ok();
            stmt.bind((3, json.as_str())).ok();
            if let Err(e) = stmt.next() {
                crate::log::warn(&format!("symbol cache: insert failed for {}: {}", key_str, e));
            }
        }
    }

    /// Remove the cache entry for a deleted file.
    fn remove(&self, path: &Path) {
        let key = self.cache_key(path);
        let key_str = key.to_string_lossy();
        if let Ok(mut stmt) = self.db.prepare("DELETE FROM symbol_cache WHERE path = ?") {
            stmt.bind((1, key_str.as_ref())).ok();
            stmt.next().ok();
        }
    }
}

pub struct SymbolIndex {
    files: HashMap<PathBuf, FileEntry>,
    parser: Parser,
    query: Query,
    last_scan: Option<Instant>,
    /// Optional on-disk cache. `None` if `~/.nerv/` is unavailable.
    cache: Option<SymbolCache>,
}

/// Minimum interval between full directory scans.
const SCAN_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(5);

/// tree-sitter query for Rust symbol definitions.
///
/// Captures: @name = symbol name, @def = whole node (for range/signature).
/// Tag names after @def encode the SymbolKind.
const RUST_QUERY: &str = r#"
(struct_item name: (type_identifier) @name) @definition.struct
(enum_item name: (type_identifier) @name) @definition.enum
(union_item name: (type_identifier) @name) @definition.union
(type_item name: (type_identifier) @name) @definition.type
(trait_item name: (type_identifier) @name) @definition.interface
(mod_item name: (identifier) @name) @definition.module
(macro_definition name: (identifier) @name) @definition.macro
(const_item name: (identifier) @name) @definition.const
(static_item name: (identifier) @name) @definition.const

(declaration_list
    (function_item name: (identifier) @name) @definition.method)

(declaration_list
    (function_signature_item name: (identifier) @name) @definition.method)

(function_item name: (identifier) @name) @definition.function
"#;

impl SymbolIndex {
    pub fn new() -> Self {
        Self::new_inner(None)
    }

    /// Construct with a persistent on-disk cache at `repo_dir/symbol_cache.db`.
    pub fn new_with_cache(repo_dir: &Path) -> Self {
        Self::new_inner(SymbolCache::open(repo_dir))
    }

    /// Create with a cache, and attach a repo root so paths are stored relative to it.
    /// This makes the cache survive directory renames.
    pub fn new_with_cache_and_root(repo_dir: &Path, repo_root: &Path) -> Self {
        let cache = SymbolCache::open(repo_dir)
            .map(|c| c.with_repo_root(repo_root.to_path_buf()));
        Self::new_inner(cache)
    }

    fn new_inner(cache: Option<SymbolCache>) -> Self {
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language).expect("Rust grammar");
        let query = Query::new(&language, RUST_QUERY).expect("valid query");
        Self {
            files: HashMap::new(),
            parser,
            query,
            last_scan: None,
            cache,
        }
    }

    /// Index all `.rs` files under `root`, skipping files whose mtime hasn't changed.
    /// Debounced: no-ops if called within `SCAN_DEBOUNCE` of the last scan.
    pub fn index_dir(&mut self, root: &Path) {
        if let Some(last) = self.last_scan {
            if last.elapsed() < SCAN_DEBOUNCE {
                return;
            }
        }
        self.force_index_dir(root);
    }

    /// Full scan ignoring debounce. Used in tests and after known-dirty events.
    pub fn force_index_dir(&mut self, root: &Path) {
        let rs_files = collect_rs_files(root);
        // Remove entries for files that no longer exist, cleaning up cache too.
        let removed: Vec<PathBuf> = self
            .files
            .keys()
            .filter(|p| !rs_files.contains_key(*p))
            .cloned()
            .collect();
        for path in removed {
            self.files.remove(&path);
            if let Some(ref cache) = self.cache {
                cache.remove(&path);
            }
        }
        for (path, mtime) in rs_files {
            if let Some(entry) = self.files.get(&path) {
                if entry.mtime == mtime {
                    continue;
                }
            }
            let mtime_ms = mtime
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            // Try the on-disk cache before running tree-sitter.
            if let Some(ref cache) = self.cache {
                if let Some(symbols) = cache.get(&path, mtime_ms) {
                    self.files.insert(path, FileEntry { mtime, symbols });
                    continue;
                }
            }
            if let Ok(source) = std::fs::read_to_string(&path) {
                let symbols = self.parse_symbols(&path, &source);
                if let Some(ref cache) = self.cache {
                    cache.put(&path, mtime_ms, &symbols);
                }
                self.files.insert(path, FileEntry { mtime, symbols });
            }
        }
        self.last_scan = Some(Instant::now());
    }

    /// Force the next `index_dir` call to do a full scan.
    pub fn mark_dirty(&mut self) {
        self.last_scan = None;
    }

    /// Re-index a single file (called after edit/write).
    pub fn index_file(&mut self, path: &Path) {
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                self.files.remove(path);
                if let Some(ref cache) = self.cache {
                    cache.remove(path);
                }
                return;
            }
        };
        let mtime = match std::fs::metadata(&canonical).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => {
                self.files.remove(&canonical);
                if let Some(ref cache) = self.cache {
                    cache.remove(&canonical);
                }
                return;
            }
        };
        let mtime_ms = mtime
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        // Try cache first.
        if let Some(ref cache) = self.cache {
            if let Some(symbols) = cache.get(&canonical, mtime_ms) {
                self.files.insert(canonical, FileEntry { mtime, symbols });
                return;
            }
        }
        if let Ok(source) = std::fs::read_to_string(&canonical) {
            let symbols = self.parse_symbols(&canonical, &source);
            if let Some(ref cache) = self.cache {
                cache.put(&canonical, mtime_ms, &symbols);
            }
            self.files.insert(canonical, FileEntry { mtime, symbols });
        }
    }

    fn parse_symbols(&mut self, path: &Path, source: &str) -> Vec<SymbolDef> {
        let tree = match self.parser.parse(source.as_bytes(), None) {
            Some(t) => t,
            None => return vec![],
        };

        let name_idx = self.query.capture_index_for_name("name").unwrap();
        let source_bytes = source.as_bytes();
        let mut cursor = QueryCursor::new();
        let mut symbols = Vec::new();
        // A function_item inside a declaration_list matches both the method and
        // free-function patterns. Track seen byte ranges to keep only the first
        // (more specific) match.
        let mut seen_ranges = std::collections::HashSet::new();

        let mut matches = cursor.matches(&self.query, tree.root_node(), source_bytes);
        while let Some(m) = { matches.advance(); matches.get() } {
            let name_capture = m.captures.iter().find(|c| c.index == name_idx);
            let def_capture = m.captures.iter().find(|c| c.index != name_idx);

            let (name_capture, def_capture) = match (name_capture, def_capture) {
                (Some(n), Some(d)) => (n, d),
                _ => continue,
            };

            let node = def_capture.node;
            let byte_range = (node.start_byte(), node.end_byte());
            if !seen_ranges.insert(byte_range) {
                continue;
            }

            let name = match name_capture.node.utf8_text(source_bytes) {
                Ok(s) => s.to_string(),
                Err(_) => continue,
            };

            let def_capture_name = &self.query.capture_names()[def_capture.index as usize];
            let kind = match SymbolKind::from_tag(def_capture_name) {
                Some(k) => k,
                None => continue,
            };

            let line = node.start_position().row as u32 + 1;
            let end_line = node.end_position().row as u32 + 1;

            let start = node.start_byte();
            let sig_end = source[start..]
                .find('\n')
                .map(|i| start + i)
                .unwrap_or(node.end_byte());
            let signature = source[start..sig_end].trim().to_string();

            let parent = if kind == SymbolKind::Method {
                find_impl_parent(node, source_bytes)
            } else {
                None
            };

            symbols.push(SymbolDef {
                name,
                kind,
                file: path.to_path_buf(),
                line,
                end_line,
                signature,
                parent,
            });
        }

        symbols
    }

    /// Search for symbols matching `query` (case-insensitive substring).
    pub fn search(
        &self,
        query: &str,
        kind_filter: Option<SymbolKind>,
        file_filter: Option<&Path>,
    ) -> Vec<&SymbolDef> {
        let query_lower = query.to_lowercase();
        let words: Vec<&str> = query_lower.split_whitespace().collect();
        let canonical_filter = file_filter.and_then(|f| f.canonicalize().ok());
        let filter_ref = canonical_filter.as_deref().or(file_filter);
        let mut results: Vec<&SymbolDef> = self
            .files
            .iter()
            .filter(|(path, _)| {
                filter_ref
                    .map(|f| path.starts_with(f))
                    .unwrap_or(true)
            })
            .flat_map(|(_, entry)| entry.symbols.iter())
            .filter(|sym| {
                let name = sym.name.to_lowercase();
                let matches = if words.len() > 1 {
                    // Multi-word: match if ANY word is a substring of the name
                    words.iter().any(|w| name.contains(w))
                } else {
                    name.contains(&query_lower)
                };
                matches && kind_filter.map(|k| sym.kind == k).unwrap_or(true)
            })
            .collect();

        // Sort: exact match first, then by word-match count (more = better),
        // then by file path and line.
        results.sort_by(|a, b| {
            let a_name = a.name.to_lowercase();
            let b_name = b.name.to_lowercase();
            let a_exact = a.name.eq_ignore_ascii_case(query);
            let b_exact = b.name.eq_ignore_ascii_case(query);
            let a_hits = words.iter().filter(|w| a_name.contains(*w)).count();
            let b_hits = words.iter().filter(|w| b_name.contains(*w)).count();
            b_exact
                .cmp(&a_exact)
                .then_with(|| b_hits.cmp(&a_hits))
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.line.cmp(&b.line))
        });

        results
    }

}

/// Walk up the tree from a method node to find the enclosing impl_item and extract its type.
fn find_impl_parent(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "impl_item" {
            let trait_part = n
                .child_by_field_name("trait")
                .and_then(|t| t.utf8_text(source).ok());
            let type_part = n
                .child_by_field_name("type")
                .and_then(|t| t.utf8_text(source).ok());
            return match (trait_part, type_part) {
                (Some(tr), Some(ty)) => Some(format!("impl {} for {}", tr, ty)),
                (None, Some(ty)) => Some(format!("impl {}", ty)),
                _ => None,
            };
        }
        current = n.parent();
    }
    None
}

/// Collect all `.rs` files under `root`, respecting .gitignore via `fd`.
fn collect_rs_files(root: &Path) -> HashMap<PathBuf, SystemTime> {
    // Prefer fd: respects .gitignore by default.
    let output = std::process::Command::new("fd")
        .args(["--type", "f", "--extension", "rs", "--absolute-path"])
        .current_dir(root)
        .output();

    if let Ok(o) = output {
        if o.status.success() {
            let stdout = String::from_utf8_lossy(&o.stdout);
            return stdout
                .lines()
                .filter(|l| !l.is_empty())
                .filter_map(|l| {
                    let path = PathBuf::from(l);
                    let path = path.canonicalize().unwrap_or(path);
                    let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
                    Some((path, mtime))
                })
                .collect();
        }
    }

    // Fallback: `git ls-files` also respects .gitignore.
    let output = std::process::Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "*.rs"])
        .current_dir(root)
        .output();

    if let Ok(o) = output {
        if o.status.success() {
            let stdout = String::from_utf8_lossy(&o.stdout);
            return stdout
                .lines()
                .filter(|l| !l.is_empty())
                .filter_map(|l| {
                    let path = root.join(l);
                    let path = path.canonicalize().unwrap_or(path);
                    let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
                    Some((path, mtime))
                })
                .collect();
        }
    }

    // Last resort: manual walk. Only skips obvious non-source dirs; does not
    // read .gitignore. Should rarely be reached in practice.
    walk_rs_files(root)
}

/// Fallback: walk the directory manually if neither `fd` nor `git` is available.
fn walk_rs_files(root: &Path) -> HashMap<PathBuf, SystemTime> {
    let mut result = HashMap::new();
    walk_dir_recursive(root, &mut result);
    result
}

fn walk_dir_recursive(dir: &Path, out: &mut HashMap<PathBuf, SystemTime>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            walk_dir_recursive(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                let path = path.canonicalize().unwrap_or(path);
                out.insert(path, mtime);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_source(source: &str) -> Vec<SymbolDef> {
        let mut index = SymbolIndex::new();
        index.parse_symbols(Path::new("test.rs"), source)
    }

    #[test]
    fn free_function() {
        let syms = parse_source("fn hello(x: i32) -> bool {\n    true\n}\n");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "hello");
        assert_eq!(syms[0].kind, SymbolKind::Function);
        assert!(syms[0].signature.starts_with("fn hello"));
        assert!(syms[0].parent.is_none());
    }

    #[test]
    fn struct_and_enum() {
        let syms = parse_source("struct Foo;\nenum Bar { A, B }\n");
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "Foo");
        assert_eq!(syms[0].kind, SymbolKind::Struct);
        assert_eq!(syms[1].name, "Bar");
        assert_eq!(syms[1].kind, SymbolKind::Enum);
    }

    #[test]
    fn method_in_impl() {
        let syms = parse_source(
            "struct Agent;\nimpl Agent {\n    fn run(&self) {}\n    fn stop(&self) {}\n}\n",
        );
        // 1 struct + 2 methods, no duplicates from the general function_item pattern
        assert_eq!(syms.len(), 3, "got: {:#?}", syms);
        let methods: Vec<_> = syms.iter().filter(|s| s.kind == SymbolKind::Method).collect();
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].name, "run");
        assert_eq!(methods[0].parent.as_deref(), Some("impl Agent"));
        assert_eq!(methods[1].name, "stop");
    }

    #[test]
    fn trait_impl_parent() {
        let syms = parse_source(
            "trait Foo {}\nstruct Bar;\nimpl Foo for Bar {\n    fn do_it(&self) {}\n}\n",
        );
        let method = syms.iter().find(|s| s.name == "do_it").unwrap();
        assert_eq!(method.kind, SymbolKind::Method);
        assert_eq!(method.parent.as_deref(), Some("impl Foo for Bar"));
    }

    #[test]
    fn trait_definition() {
        let syms = parse_source("pub trait AgentTool: Send + Sync {\n    fn name(&self) -> &str;\n}\n");
        let tr = syms.iter().find(|s| s.name == "AgentTool").unwrap();
        assert_eq!(tr.kind, SymbolKind::Trait);
    }

    #[test]
    fn type_alias() {
        let syms = parse_source("type Result<T> = std::result::Result<T, Error>;\n");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Result");
        assert_eq!(syms[0].kind, SymbolKind::Type);
    }

    #[test]
    fn module_definition() {
        let syms = parse_source("mod inner {\n    fn private() {}\n}\n");
        let mods: Vec<_> = syms.iter().filter(|s| s.kind == SymbolKind::Module).collect();
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].name, "inner");
    }

    #[test]
    fn macro_definition() {
        let syms = parse_source("macro_rules! my_macro {\n    () => {};\n}\n");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "my_macro");
        assert_eq!(syms[0].kind, SymbolKind::Macro);
    }

    #[test]
    fn search_filters() {
        let mut index = SymbolIndex::new();
        let source = "fn foo() {}\nfn foobar() {}\nstruct Foo;\n";
        let syms = index.parse_symbols(Path::new("test.rs"), source);
        index.files.insert(
            PathBuf::from("test.rs"),
            FileEntry {
                mtime: SystemTime::UNIX_EPOCH,
                symbols: syms,
            },
        );

        // Substring match
        let results = index.search("foo", None, None);
        assert_eq!(results.len(), 3);

        // Kind filter
        let results = index.search("foo", Some(SymbolKind::Function), None);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn line_numbers_are_one_based() {
        let syms = parse_source("\n\nfn third_line() {}\n");
        assert_eq!(syms[0].line, 3);
    }

    #[test]
    fn trait_methods_both_kinds() {
        // Both declarations (no body) and default methods (with body) are extracted
        let syms = parse_source(
            "pub trait AgentTool {\n    fn name(&self) -> &str;\n    fn default_impl(&self) { }\n}\n",
        );
        let methods: Vec<_> = syms.iter().filter(|s| s.kind == SymbolKind::Method).collect();
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].name, "name");
        assert_eq!(methods[1].name, "default_impl");
    }

    #[test]
    fn incremental_skips_unchanged_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("lib.rs");
        std::fs::write(&f, "fn original() {}\n").unwrap();

        let mut index = SymbolIndex::new();
        index.force_index_dir(tmp.path());
        assert_eq!(index.search("original", None, None).len(), 1);

        // Re-index without modifying the file — should be a no-op
        // (verifiable because parse count stays the same; we just check
        // the result is still valid)
        index.force_index_dir(tmp.path());
        assert_eq!(index.search("original", None, None).len(), 1);
    }

    #[test]
    fn incremental_picks_up_changed_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("lib.rs");
        std::fs::write(&f, "fn first() {}\n").unwrap();

        let mut index = SymbolIndex::new();
        index.force_index_dir(tmp.path());
        assert_eq!(index.search("first", None, None).len(), 1);

        // Slight delay to ensure mtime changes
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&f, "fn second() {}\n").unwrap();

        index.force_index_dir(tmp.path());
        assert_eq!(index.search("first", None, None).len(), 0);
        assert_eq!(index.search("second", None, None).len(), 1);
    }

    #[test]
    fn deleted_files_removed_from_index() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("lib.rs");
        std::fs::write(&f, "fn gone() {}\n").unwrap();

        let mut index = SymbolIndex::new();
        index.force_index_dir(tmp.path());
        assert_eq!(index.search("gone", None, None).len(), 1);

        std::fs::remove_file(&f).unwrap();
        index.force_index_dir(tmp.path());
        assert_eq!(index.search("gone", None, None).len(), 0);
    }

    #[test]
    fn index_file_single() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("one.rs");
        std::fs::write(&f, "fn alpha() {}\n").unwrap();

        let mut index = SymbolIndex::new();
        index.index_file(&f);
        assert_eq!(index.search("alpha", None, None).len(), 1);

        std::fs::write(&f, "fn beta() {}\n").unwrap();
        index.index_file(&f);
        assert_eq!(index.search("alpha", None, None).len(), 0);
        assert_eq!(index.search("beta", None, None).len(), 1);
    }

    #[test]
    fn search_exact_match_sorts_first() {
        let mut index = SymbolIndex::new();
        let source = "fn foobar() {}\nfn foo() {}\nfn foo_baz() {}\n";
        let syms = index.parse_symbols(Path::new("test.rs"), source);
        index.files.insert(
            PathBuf::from("test.rs"),
            FileEntry { mtime: SystemTime::UNIX_EPOCH, symbols: syms },
        );

        let results = index.search("foo", None, None);
        assert_eq!(results[0].name, "foo", "exact match should sort first");
    }

    #[test]
    fn multi_file_search() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn shared() {}\n").unwrap();
        std::fs::write(tmp.path().join("b.rs"), "fn shared() {}\n").unwrap();

        let mut index = SymbolIndex::new();
        index.force_index_dir(tmp.path());
        let results = index.search("shared", None, None);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn file_filter_restricts_results() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(tmp.path().join("top.rs"), "fn target() {}\n").unwrap();
        std::fs::write(sub.join("nested.rs"), "fn target() {}\n").unwrap();

        let mut index = SymbolIndex::new();
        index.force_index_dir(tmp.path());

        let all = index.search("target", None, None);
        assert_eq!(all.len(), 2);

        let sub_only = index.search("target", None, Some(&sub));
        assert_eq!(sub_only.len(), 1);
    }

    #[test]
    fn multi_word_search() {
        let mut index = SymbolIndex::new();
        let source = "fn execute_tools() {}\nfn check_permission() {}\nfn render_ui() {}\n";
        let syms = index.parse_symbols(Path::new("test.rs"), source);
        index.files.insert(
            PathBuf::from("test.rs"),
            FileEntry { mtime: SystemTime::UNIX_EPOCH, symbols: syms },
        );

        // Multi-word query matches any word
        let results = index.search("execute permission", None, None);
        let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"execute_tools"), "should match 'execute'");
        assert!(names.contains(&"check_permission"), "should match 'permission'");
        assert!(!names.contains(&"render_ui"), "should not match unrelated");

        // Single-word still works as substring
        let results = index.search("perm", None, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "check_permission");
    }

    #[test]
    fn const_and_static() {
        let syms = parse_source(
            "const MAX: usize = 100;\nstatic COUNTER: AtomicU32 = AtomicU32::new(0);\n",
        );
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "MAX");
        assert_eq!(syms[0].kind, SymbolKind::Const);
        assert_eq!(syms[1].name, "COUNTER");
        assert_eq!(syms[1].kind, SymbolKind::Const);
    }

    #[test]
    fn union_definition() {
        let syms = parse_source("union MyUnion {\n    i: i32,\n    f: f32,\n}\n");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "MyUnion");
        assert_eq!(syms[0].kind, SymbolKind::Union);
    }

    #[test]
    fn trait_method_declarations() {
        // Trait method declarations (no body) are function_signature_item
        let syms = parse_source(
            "pub trait AgentTool {\n    fn name(&self) -> &str;\n    fn execute(&self);\n}\n",
        );
        let methods: Vec<_> = syms.iter().filter(|s| s.kind == SymbolKind::Method).collect();
        assert_eq!(methods.len(), 2, "trait method declarations should be indexed: {:#?}", syms);
        assert_eq!(methods[0].name, "name");
        assert_eq!(methods[1].name, "execute");
    }

    #[test]
    fn enum_kind_filter() {
        let mut index = SymbolIndex::new();
        let source = "struct Foo;\nenum Bar { A }\nfn baz() {}\n";
        let syms = index.parse_symbols(Path::new("test.rs"), source);
        index.files.insert(
            PathBuf::from("test.rs"),
            FileEntry { mtime: SystemTime::UNIX_EPOCH, symbols: syms },
        );

        let enums = index.search("", Some(SymbolKind::Enum), None);
        assert_eq!(enums.len(), 1);
        assert_eq!(enums[0].name, "Bar");

        let structs = index.search("", Some(SymbolKind::Struct), None);
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].name, "Foo");
    }

    #[test]
    fn syntax_errors_produce_partial_results() {
        // Missing closing brace — tree-sitter still produces a partial tree
        let syms = parse_source("fn good() {}\nfn broken( {}\n");
        // Should at least find the valid function
        assert!(
            syms.iter().any(|s| s.name == "good"),
            "should recover valid symbols despite syntax errors: {:#?}",
            syms
        );
    }

    #[test]
    fn debounce_skips_redundant_scans() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn first() {}\n").unwrap();

        let mut index = SymbolIndex::new();
        index.index_dir(tmp.path());
        assert_eq!(index.search("first", None, None).len(), 1);

        // Write a new file, but index_dir should be debounced
        std::fs::write(tmp.path().join("b.rs"), "fn second() {}\n").unwrap();
        index.index_dir(tmp.path());
        assert_eq!(
            index.search("second", None, None).len(),
            0,
            "debounce should skip the scan"
        );

        // mark_dirty clears the debounce
        index.mark_dirty();
        index.index_dir(tmp.path());
        assert_eq!(index.search("second", None, None).len(), 1);
    }

    // --- SymbolCache tests ---

    fn cached_index(dir: &Path) -> SymbolIndex {
        let cache_dir = dir.join(".nerv");
        std::fs::create_dir_all(&cache_dir).unwrap();
        SymbolIndex::new_with_cache(&cache_dir)
    }

    #[test]
    fn cache_survives_new_index_instance() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("lib.rs"), "fn cached_fn() {}\n").unwrap();

        // First index: parses and populates cache.
        let mut idx1 = cached_index(tmp.path());
        idx1.force_index_dir(tmp.path());
        assert_eq!(idx1.search("cached_fn", None, None).len(), 1);

        // Second index: loads from cache without re-parsing.
        let mut idx2 = cached_index(tmp.path());
        idx2.force_index_dir(tmp.path());
        assert_eq!(idx2.search("cached_fn", None, None).len(), 1);
    }

    #[test]
    fn cache_invalidates_on_edit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("lib.rs");
        std::fs::write(&f, "fn original() {}\n").unwrap();

        let mut idx = cached_index(tmp.path());
        idx.force_index_dir(tmp.path());
        assert_eq!(idx.search("original", None, None).len(), 1);

        // Modify file — cache entry should be invalidated on next scan.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&f, "fn replaced() {}\n").unwrap();

        let mut idx2 = cached_index(tmp.path());
        idx2.force_index_dir(tmp.path());
        assert_eq!(idx2.search("original", None, None).len(), 0);
        assert_eq!(idx2.search("replaced", None, None).len(), 1);
    }

    #[test]
    fn cache_handles_deleted_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("lib.rs");
        std::fs::write(&f, "fn doomed() {}\n").unwrap();

        let mut idx = cached_index(tmp.path());
        idx.force_index_dir(tmp.path());
        assert_eq!(idx.search("doomed", None, None).len(), 1);

        std::fs::remove_file(&f).unwrap();

        let mut idx2 = cached_index(tmp.path());
        idx2.force_index_dir(tmp.path());
        assert_eq!(idx2.search("doomed", None, None).len(), 0);
    }

    #[test]
    fn cache_index_file_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("one.rs");
        std::fs::write(&f, "fn alpha() {}\n").unwrap();

        let mut idx = cached_index(tmp.path());
        idx.index_file(&f);
        assert_eq!(idx.search("alpha", None, None).len(), 1);

        // New index: should load from cache via index_file.
        let mut idx2 = cached_index(tmp.path());
        idx2.index_file(&f);
        assert_eq!(idx2.search("alpha", None, None).len(), 1);
    }

    #[test]
    fn cache_no_nerv_dir_falls_back() {
        // If the cache dir doesn't exist, SymbolIndex still works (in-memory only).
        let bogus = Path::new("/tmp/nerv-test-nonexistent-dir-12345");
        let mut idx = SymbolIndex::new_with_cache(bogus);
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("lib.rs"), "fn fallback() {}\n").unwrap();
        idx.force_index_dir(tmp.path());
        assert_eq!(idx.search("fallback", None, None).len(), 1);
    }

    #[test]
    fn cache_millis_precision() {
        // Verify that two writes within the same second produce different cache entries
        // (i.e., millisecond precision is used, not second precision).
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("lib.rs");
        std::fs::write(&f, "fn version_one() {}\n").unwrap();

        let mut idx = cached_index(tmp.path());
        idx.force_index_dir(tmp.path());
        assert_eq!(idx.search("version_one", None, None).len(), 1);

        // Sleep just enough for mtime to differ at ms precision.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&f, "fn version_two() {}\n").unwrap();

        let mut idx2 = cached_index(tmp.path());
        idx2.force_index_dir(tmp.path());
        assert_eq!(idx2.search("version_one", None, None).len(), 0, "stale cache entry should not persist");
        assert_eq!(idx2.search("version_two", None, None).len(), 1);
    }
}
