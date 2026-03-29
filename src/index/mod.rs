pub mod codemap;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
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
    /// `Box<str>` instead of `String`: saves one word (capacity) per field.
    pub name: Box<str>,
    pub kind: SymbolKind,
    pub file: PathBuf,
    pub line: u32,
    pub end_line: u32,
    /// Byte offset of the first byte of this symbol's node in the source file.
    /// Together with `end_byte`, allows body extraction without re-parsing.
    pub start_byte: u32,
    /// Byte offset one past the last byte of this symbol's node.
    pub end_byte: u32,
    pub signature: Box<str>,
    pub parent: Option<Box<str>>,
}

struct FileEntry {
    mtime: SystemTime,
    symbols: Vec<SymbolDef>,
    /// Cached file source, held as an `Arc` so callers can clone the pointer
    /// (not the bytes) while the index lock is held and then read the source
    /// after releasing the lock.  `None` only while the entry is being built
    /// from the SQLite symbol cache (symbols were cached but source was not).
    source: Option<Arc<String>>,
}

/// Persistent on-disk cache of parsed symbol data, keyed by (path, mtime).
///
/// Stored in `~/.nerv/symbol_cache.db` (SQLite). On a warm start every
/// unchanged file is a cache hit, so tree-sitter only runs for files that
/// have been modified since the last scan.
struct SymbolCache {
    db: Connection,
    /// Repo root this cache is associated with. Paths stored as relative to this root.
    /// `None` means absolute paths are used (legacy / non-git).
    repo_root: Option<PathBuf>,
}

// rusqlite::Connection wraps a *mut sqlite3 which is not Sync by default, but
// SymbolCache is only ever accessed while the outer RwLock write guard is held,
// so there is no actual concurrent access. The unsafety is sound.
unsafe impl Sync for SymbolCache {}

impl SymbolCache {
    fn open(repo_dir: &Path) -> Option<Self> {
        let _ = std::fs::create_dir_all(repo_dir);
        let path = repo_dir.join("symbol_cache.db");
        let db = match Connection::open(&path) {
            Ok(db) => db,
            Err(e) => {
                crate::log::warn(&format!("symbol cache: open failed: {}", e));
                return None;
            }
        };
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;").ok();
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS symbol_cache (
                path  TEXT NOT NULL,
                mtime INTEGER NOT NULL,
                data  TEXT NOT NULL,
                PRIMARY KEY (path, mtime)
            );",
        )
        .ok()?;
        db.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_cache_path ON symbol_cache(path);",
        )
        .ok();
        // Evict rows older than 30 days to bound cache growth.
        let cutoff_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
            - 30 * 24 * 3600 * 1000;
        db.execute("DELETE FROM symbol_cache WHERE mtime < ?1", params![cutoff_ms]).ok();
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
        let json: String = self.db
            .query_row(
                "SELECT data FROM symbol_cache WHERE path = ?1 AND mtime = ?2",
                params![key_str.as_ref(), mtime as i64],
                |row| row.get(0),
            )
            .ok()?;
        serde_json::from_str(&json).ok()
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
        self.db.execute("DELETE FROM symbol_cache WHERE path = ?1", params![key_str.as_ref()]).ok();
        if let Err(e) = self.db.execute(
            "INSERT INTO symbol_cache (path, mtime, data) VALUES (?1, ?2, ?3)",
            params![key_str.as_ref(), mtime as i64, json],
        ) {
            crate::log::warn(&format!("symbol cache: insert failed for {}: {}", key_str, e));
        }
    }

    /// Remove the cache entry for a deleted file.
    fn remove(&self, path: &Path) {
        let key = self.cache_key(path);
        let key_str = key.to_string_lossy();
        self.db.execute("DELETE FROM symbol_cache WHERE path = ?1", params![key_str.as_ref()]).ok();
    }
}

pub struct SymbolIndex {
    files: HashMap<PathBuf, FileEntry>,
    parser: Parser,
    /// Per-language queries: [rust, go, python, typescript].
    queries: [Query; 4],
    last_scan: Option<Instant>,
    /// Optional on-disk cache. `None` if `~/.nerv/` is unavailable.
    cache: Option<SymbolCache>,
}

/// Minimum interval between full directory scans.
const SCAN_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(5);

/// Per-language tree-sitter queries.
///
/// Every query uses two capture names:
///   @name — the symbol's identifier node
///   @definition.<kind> — the whole node; the tag suffix maps to SymbolKind.
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

const GO_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @definition.function
(method_declaration name: (field_identifier) @name) @definition.method
(type_spec name: (type_identifier) @name) @definition.struct
"#;

const PYTHON_QUERY: &str = r#"
(class_definition
    body: (block
        (function_definition name: (identifier) @name) @definition.method))
(function_definition name: (identifier) @name) @definition.function
(class_definition name: (identifier) @name) @definition.struct
"#;

const TS_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @definition.function
(method_definition name: (property_identifier) @name) @definition.method
(class_declaration name: (type_identifier) @name) @definition.struct
(interface_declaration name: (type_identifier) @name) @definition.interface
(type_alias_declaration name: (type_identifier) @name) @definition.type
(enum_declaration name: (identifier) @name) @definition.enum
(lexical_declaration
    (variable_declarator
        name: (identifier) @name
        value: [(arrow_function) (function_expression)]) @definition.function)
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
        let rust_lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let go_lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
        let py_lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
        let ts_lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();

        let mut parser = Parser::new();
        parser.set_language(&rust_lang).expect("Rust grammar");

        let queries = [
            Query::new(&rust_lang, RUST_QUERY).expect("valid Rust query"),
            Query::new(&go_lang,   GO_QUERY).expect("valid Go query"),
            Query::new(&py_lang,   PYTHON_QUERY).expect("valid Python query"),
            Query::new(&ts_lang,   TS_QUERY).expect("valid TypeScript query"),
        ];

        Self {
            files: HashMap::new(),
            parser,
            queries,
            last_scan: None,
            cache,
        }
    }

    /// Check whether the index is already up-to-date for all `.rs` files under `root`.
    ///
    /// This is a **read-only** operation: it stats files on disk and compares
    /// their mtimes against what is already indexed, but never mutates `self`.
    /// Callers that hold a read lock can use this to bail out early without
    /// ever acquiring a write lock:
    ///
    /// ```ignore
    /// // Fast path: read lock only
    /// if lock.read().unwrap().is_fresh(&cwd) { return; }
    /// // Slow path: take write lock, re-check (double-checked locking), update
    /// lock.write().unwrap().index_dir(&cwd);
    /// ```
    ///
    /// Returns `true` if every `.rs` file under `root` is already indexed at the
    /// correct mtime — meaning no write lock is needed.
    ///
    /// Callers holding an `RwLock<SymbolIndex>` should use this under a read lock
    /// before deciding whether to escalate to a write lock for [`index_dir`].
    pub fn is_fresh(&self, root: &Path) -> bool {
        // Always stat — this is the point of the read-lock fast path.
        // We deliberately do not apply the debounce here; debounce only applies
        // to index_dir (which does tree-sitter parsing) not to the cheap stat check.
        let on_disk = collect_rs_files(root);
        // Fresh if every file that exists on disk is already indexed at the
        // same mtime, and no indexed file has disappeared (count matches).
        on_disk.len() == self.files.len()
            && on_disk
                .iter()
                .all(|(path, mtime)| self.files.get(path).is_some_and(|e| &e.mtime == mtime))
    }

    /// Returns cached sources for the given paths, as `Arc<String>` clones.
    /// Cheap to call under a read lock — only bumps reference counts.
    /// Paths with no cached source (e.g. symbols-only SQLite hits that
    /// somehow lost their source) are absent from the returned map; callers
    /// should fall back to `fs::read_to_string` for those.
    pub fn sources_for(&self, paths: &[&Path]) -> HashMap<PathBuf, Arc<String>> {
        paths
            .iter()
            .filter_map(|p| {
                let src = self.files.get(*p)?.source.as_ref()?;
                Some((p.to_path_buf(), Arc::clone(src)))
            })
            .collect()
    }

    /// Index all `.rs` files under `root`, skipping files whose mtime hasn't changed.
    /// Debounced: no-ops if called within `SCAN_DEBOUNCE` of the last scan.
    ///
    /// Prefer the two-phase pattern (see [`is_fresh`]) when holding an
    /// `RwLock<SymbolIndex>` to avoid taking a write lock on warm invocations.
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
                    // SQLite hit: symbols already cached — no need to read source at
                    // all.  render() will read it on demand via fs::read_to_string.
                    self.files.insert(path, FileEntry { mtime, symbols, source: None });
                    continue;
                }
            }
            if let Ok(source) = std::fs::read_to_string(&path) {
                let symbols = self.parse_symbols(&path, &source);
                if let Some(ref cache) = self.cache {
                    cache.put(&path, mtime_ms, &symbols);
                }
                // Drop source after parsing — render() reads it on demand.
                // Keeping it would duplicate every indexed file in RAM.
                self.files.insert(path, FileEntry { mtime, symbols, source: None });
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
                self.files.insert(canonical, FileEntry { mtime, symbols, source: None });
                return;
            }
        }
        if let Ok(source) = std::fs::read_to_string(&canonical) {
            let symbols = self.parse_symbols(&canonical, &source);
            if let Some(ref cache) = self.cache {
                cache.put(&canonical, mtime_ms, &symbols);
            }
            // Drop source after parsing; render() re-reads on demand.
            self.files.insert(canonical, FileEntry { mtime, symbols, source: None });
        }
    }

    fn parse_symbols(&mut self, path: &Path, source: &str) -> Vec<SymbolDef> {
        // Select language and query based on file extension.
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let (lang, query_idx, find_parent): (tree_sitter::Language, usize, fn(tree_sitter::Node, &[u8]) -> Option<String>) = match ext {
            "go"  => (tree_sitter_go::LANGUAGE.into(), 1, find_go_method_parent),
            "py"  => (tree_sitter_python::LANGUAGE.into(), 2, find_python_method_parent),
            "ts" | "tsx" => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), 3, find_ts_method_parent),
            _     => (tree_sitter_rust::LANGUAGE.into(), 0, find_impl_parent), // default: Rust
        };

        // Set parser language for this file's extension.
        self.parser.set_language(&lang).expect("grammar");

        let tree = match self.parser.parse(source.as_bytes(), None) {
            Some(t) => t,
            None => return vec![],
        };

        let query = &self.queries[query_idx];
        let name_idx = query.capture_index_for_name("name").unwrap();
        let source_bytes = source.as_bytes();
        let mut cursor = QueryCursor::new();
        let mut symbols = Vec::new();
        // Track seen byte ranges to keep only the first (most specific) match
        // when multiple patterns overlap the same node.
        let mut seen_ranges = std::collections::HashSet::new();

        let mut matches = cursor.matches(query, tree.root_node(), source_bytes);
        while let Some(m) = { matches.advance(); matches.get() } {
            let name_capture = m.captures.iter().find(|c| c.index == name_idx);
            let def_capture  = m.captures.iter().find(|c| c.index != name_idx);

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

            let def_capture_name = &query.capture_names()[def_capture.index as usize];
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
                find_parent(node, source_bytes)
            } else {
                None
            };

            symbols.push(SymbolDef {
                name: name.into(),
                kind,
                file: path.to_path_buf(),
                line,
                end_line,
                start_byte: node.start_byte() as u32,
                end_byte: node.end_byte() as u32,
                signature: signature.into(),
                parent: parent.map(Into::into),
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

/// Walk up from a Rust method node to find the enclosing `impl` block.
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

/// Walk up from a Go method_declaration to extract the receiver type name.
/// The receiver parameter looks like: `(receiver_list (parameter_declaration type: ...))`
fn find_go_method_parent(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    // The method_declaration itself has a `receiver` field.
    let recv = node.child_by_field_name("receiver")?;
    // Walk children to find a type_identifier or pointer_type > type_identifier.
    for i in 0..recv.named_child_count() {
        let child = recv.named_child(i)?;
        let ty = extract_go_type_name(child, source);
        if ty.is_some() {
            return ty;
        }
    }
    None
}

fn extract_go_type_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" => node.utf8_text(source).ok().map(str::to_string),
        "pointer_type" => {
            // (*T) — descend to the type_identifier child
            for i in 0..node.named_child_count() {
                let child = node.named_child(i)?;
                if let Some(name) = extract_go_type_name(child, source) {
                    return Some(name);
                }
            }
            None
        }
        "parameter_declaration" => {
            node.child_by_field_name("type")
                .and_then(|t| extract_go_type_name(t, source))
        }
        _ => None,
    }
}

/// Walk up from a Python function_definition to find an enclosing class_definition.
fn find_python_method_parent(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "class_definition" {
            return n
                .child_by_field_name("name")
                .and_then(|t| t.utf8_text(source).ok())
                .map(str::to_string);
        }
        current = n.parent();
    }
    None
}

/// Walk up from a TypeScript method_definition to find the enclosing class_declaration.
fn find_ts_method_parent(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "class_declaration" || n.kind() == "class" {
            return n
                .child_by_field_name("name")
                .and_then(|t| t.utf8_text(source).ok())
                .map(str::to_string);
        }
        current = n.parent();
    }
    None
}

const SOURCE_EXTENSIONS: &[&str] = &["rs", "go", "py", "ts", "tsx"];

/// Collect all indexable source files under `root`, respecting .gitignore via `fd`.
fn collect_rs_files(root: &Path) -> HashMap<PathBuf, SystemTime> {
    // Prefer fd: respects .gitignore by default.
    let ext_args: Vec<_> = SOURCE_EXTENSIONS
        .iter()
        .flat_map(|e| ["--extension", e])
        .collect();
    let output = std::process::Command::new("fd")
        .arg("--type").arg("f")
        .args(&ext_args)
        .arg("--absolute-path")
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
    let glob_args: Vec<_> = SOURCE_EXTENSIONS
        .iter()
        .map(|e| format!("*.{}", e))
        .collect();
    let output = std::process::Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .args(&glob_args)
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
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| SOURCE_EXTENSIONS.contains(&e)) {
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

    fn parse_go(source: &str) -> Vec<SymbolDef> {
        let mut index = SymbolIndex::new();
        index.parse_symbols(Path::new("test.go"), source)
    }

    fn parse_py(source: &str) -> Vec<SymbolDef> {
        let mut index = SymbolIndex::new();
        index.parse_symbols(Path::new("test.py"), source)
    }

    fn parse_ts(source: &str) -> Vec<SymbolDef> {
        let mut index = SymbolIndex::new();
        index.parse_symbols(Path::new("test.ts"), source)
    }

    #[test]
    fn free_function() {
        let syms = parse_source("fn hello(x: i32) -> bool {\n    true\n}\n");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name.as_ref(), "hello");
        assert_eq!(syms[0].kind, SymbolKind::Function);
        assert!(syms[0].signature.starts_with("fn hello"));
        assert!(syms[0].parent.is_none());
    }

    #[test]
    fn struct_and_enum() {
        let syms = parse_source("struct Foo;\nenum Bar { A, B }\n");
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name.as_ref(), "Foo");
        assert_eq!(syms[0].kind, SymbolKind::Struct);
        assert_eq!(syms[1].name.as_ref(), "Bar");
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
        assert_eq!(methods[0].name.as_ref(), "run");
        assert_eq!(methods[0].parent.as_deref(), Some("impl Agent"));
        assert_eq!(methods[1].name.as_ref(), "stop");
    }

    #[test]
    fn trait_impl_parent() {
        let syms = parse_source(
            "trait Foo {}\nstruct Bar;\nimpl Foo for Bar {\n    fn do_it(&self) {}\n}\n",
        );
        let method = syms.iter().find(|s| s.name.as_ref() == "do_it").unwrap();
        assert_eq!(method.kind, SymbolKind::Method);
        assert_eq!(method.parent.as_deref(), Some("impl Foo for Bar"));
    }

    #[test]
    fn trait_definition() {
        let syms = parse_source("pub trait AgentTool: Send + Sync {\n    fn name(&self) -> &str;\n}\n");
        let tr = syms.iter().find(|s| s.name.as_ref() == "AgentTool").unwrap();
        assert_eq!(tr.kind, SymbolKind::Trait);
    }

    #[test]
    fn type_alias() {
        let syms = parse_source("type Result<T> = std::result::Result<T, Error>;\n");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name.as_ref(), "Result");
        assert_eq!(syms[0].kind, SymbolKind::Type);
    }

    #[test]
    fn module_definition() {
        let syms = parse_source("mod inner {\n    fn private() {}\n}\n");
        let mods: Vec<_> = syms.iter().filter(|s| s.kind == SymbolKind::Module).collect();
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].name.as_ref(), "inner");
    }

    #[test]
    fn macro_definition() {
        let syms = parse_source("macro_rules! my_macro {\n    () => {};\n}\n");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name.as_ref(), "my_macro");
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
                source: None,
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
        assert_eq!(methods[0].name.as_ref(), "name");
        assert_eq!(methods[1].name.as_ref(), "default_impl");
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
            FileEntry { mtime: SystemTime::UNIX_EPOCH, symbols: syms, source: None },
        );

        let results = index.search("foo", None, None);
        assert_eq!(results[0].name.as_ref(), "foo", "exact match should sort first");
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
            FileEntry { mtime: SystemTime::UNIX_EPOCH, symbols: syms, source: None },
        );

        // Multi-word query matches any word
        let results = index.search("execute permission", None, None);
        let names: Vec<&str> = results.iter().map(|s| s.name.as_ref()).collect();
        assert!(names.contains(&"execute_tools"), "should match 'execute'");
        assert!(names.contains(&"check_permission"), "should match 'permission'");
        assert!(!names.contains(&"render_ui"), "should not match unrelated");

        // Single-word still works as substring
        let results = index.search("perm", None, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name.as_ref(), "check_permission");
    }

    #[test]
    fn const_and_static() {
        let syms = parse_source(
            "const MAX: usize = 100;\nstatic COUNTER: AtomicU32 = AtomicU32::new(0);\n",
        );
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name.as_ref(), "MAX");
        assert_eq!(syms[0].kind, SymbolKind::Const);
        assert_eq!(syms[1].name.as_ref(), "COUNTER");
        assert_eq!(syms[1].kind, SymbolKind::Const);
    }

    #[test]
    fn union_definition() {
        let syms = parse_source("union MyUnion {\n    i: i32,\n    f: f32,\n}\n");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name.as_ref(), "MyUnion");
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
        assert_eq!(methods[0].name.as_ref(), "name");
        assert_eq!(methods[1].name.as_ref(), "execute");
    }

    #[test]
    fn enum_kind_filter() {
        let mut index = SymbolIndex::new();
        let source = "struct Foo;\nenum Bar { A }\nfn baz() {}\n";
        let syms = index.parse_symbols(Path::new("test.rs"), source);
        index.files.insert(
            PathBuf::from("test.rs"),
            FileEntry { mtime: SystemTime::UNIX_EPOCH, symbols: syms, source: None },
        );

        let enums = index.search("", Some(SymbolKind::Enum), None);
        assert_eq!(enums.len(), 1);
        assert_eq!(enums[0].name.as_ref(), "Bar");

        let structs = index.search("", Some(SymbolKind::Struct), None);
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].name.as_ref(), "Foo");
    }

    #[test]
    fn syntax_errors_produce_partial_results() {
        // Missing closing brace — tree-sitter still produces a partial tree
        let syms = parse_source("fn good() {}\nfn broken( {}\n");
        // Should at least find the valid function
        assert!(
            syms.iter().any(|s| s.name.as_ref() == "good"),
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

    // ── is_fresh / two-phase RwLock tests ────────────────────────────────────

    #[test]
    fn is_fresh_returns_true_when_nothing_changed() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("lib.rs"), "fn foo() {}\n").unwrap();

        let mut index = SymbolIndex::new();
        index.force_index_dir(tmp.path());

        // Immediately after a full scan everything on disk matches the index.
        assert!(index.is_fresh(tmp.path()), "unchanged dir should be fresh");
    }

    #[test]
    fn is_fresh_returns_false_when_file_modified() {
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("lib.rs");
        std::fs::write(&f, "fn original() {}\n").unwrap();

        let mut index = SymbolIndex::new();
        index.force_index_dir(tmp.path());

        // Wait for mtime granularity, then modify.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&f, "fn changed() {}\n").unwrap();

        assert!(!index.is_fresh(tmp.path()), "modified file should make index stale");
    }

    #[test]
    fn is_fresh_returns_false_when_file_added() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn a() {}\n").unwrap();

        let mut index = SymbolIndex::new();
        index.force_index_dir(tmp.path());

        // Add a second file after the scan.
        std::fs::write(tmp.path().join("b.rs"), "fn b() {}\n").unwrap();

        assert!(!index.is_fresh(tmp.path()), "new file should make index stale");
    }

    #[test]
    fn is_fresh_returns_false_when_file_deleted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a.rs");
        let b = tmp.path().join("b.rs");
        std::fs::write(&a, "fn a() {}\n").unwrap();
        std::fs::write(&b, "fn b() {}\n").unwrap();

        let mut index = SymbolIndex::new();
        index.force_index_dir(tmp.path());

        std::fs::remove_file(&b).unwrap();

        assert!(!index.is_fresh(tmp.path()), "deleted file should make index stale");
    }

    #[test]
    fn two_phase_rwlock_serves_search_without_write_lock_when_fresh() {
        use std::sync::{Arc, RwLock};

        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("lib.rs"), "fn omega() {}\n").unwrap();

        let mut idx = SymbolIndex::new();
        idx.force_index_dir(tmp.path());

        let lock: Arc<RwLock<SymbolIndex>> = Arc::new(RwLock::new(idx));
        let root = tmp.path().to_path_buf();

        // Simulate what the tools do: read-lock freshness check → search.
        {
            let guard = lock.read().unwrap();
            assert!(guard.is_fresh(&root), "should be fresh — no write lock needed");
            let results = guard.search("omega", None, None);
            assert_eq!(results.len(), 1);
        }

        // Mutate on disk, then simulate the write-lock upgrade path.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(tmp.path().join("lib.rs"), "fn alpha() {}\n").unwrap();

        {
            let stale = !lock.read().unwrap().is_fresh(&root);
            assert!(stale, "after modification index should be stale");
        }
        // Write-lock upgrade: re-index, then search.
        lock.write().unwrap().force_index_dir(&root);
        {
            let guard = lock.read().unwrap();
            assert!(guard.is_fresh(&root));
            assert_eq!(guard.search("alpha", None, None).len(), 1);
            assert_eq!(guard.search("omega", None, None).len(), 0);
        }
    }

    #[test]
    fn two_phase_rwlock_allows_concurrent_readers() {
        use std::sync::{Arc, Barrier, RwLock};

        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("lib.rs"), "fn shared() {}\n").unwrap();

        let mut idx = SymbolIndex::new();
        idx.force_index_dir(tmp.path());

        let lock: Arc<RwLock<SymbolIndex>> = Arc::new(RwLock::new(idx));
        let root = tmp.path().to_path_buf();

        // Spin up 4 reader threads that all hold the read lock simultaneously.
        let barrier = Arc::new(Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let lock = Arc::clone(&lock);
                let root = root.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let guard = lock.read().unwrap();
                    // All threads meet here while all holding the read lock.
                    barrier.wait();
                    assert!(guard.is_fresh(&root));
                    assert_eq!(guard.search("shared", None, None).len(), 1);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }


    #[test]
    fn byte_offsets_point_to_exact_source() {
        // start_byte/end_byte must slice back to the exact symbol text.
        let source = "fn first() {}\nfn second() { let x = 1; }\n";
        let syms = parse_source(source);
        for sym in &syms {
            let slice = &source[sym.start_byte as usize..sym.end_byte as usize];
            assert!(
                slice.contains(sym.name.as_ref()),
                "byte slice for '{}' should contain the name; got: {:?}",
                sym.name,
                slice
            );
            // The slice must start with `fn`
            assert!(
                slice.trim_start().starts_with("fn "),
                "byte slice for '{}' should start with 'fn': {:?}",
                sym.name,
                slice
            );
        }
    }

    #[test]
    fn byte_offsets_with_unicode_prefix() {
        // Multi-byte characters before the target symbol shift byte offsets.
        // If start_byte/end_byte are char offsets instead of byte offsets this
        // will slice at the wrong position or panic.
        let source = "// café ☕ unicode comment\nfn after_unicode() { let x = 42; }\n";
        let syms = parse_source(source);
        assert_eq!(syms.len(), 1);
        let sym = &syms[0];
        assert_eq!(sym.name.as_ref(), "after_unicode");
        // Verify the slice is valid UTF-8 and contains the function.
        let slice = &source[sym.start_byte as usize..sym.end_byte as usize];
        assert!(slice.contains("after_unicode"), "wrong byte slice: {:?}", slice);
        assert!(slice.contains("42"), "body should be included: {:?}", slice);
    }

    #[test]
    fn byte_offsets_multiple_symbols_are_independent() {
        // Each symbol's byte range must cover only its own body, not bleed into
        // adjacent symbols.
        let source = concat!(
            "fn alpha() { let a = 1; }\n",
            "fn beta()  { let b = 2; }\n",
            "fn gamma() { let c = 3; }\n",
        );
        let syms = parse_source(source);
        assert_eq!(syms.len(), 3);
        // Ranges must be non-overlapping and in order.
        let mut sorted = syms.clone();
        sorted.sort_by_key(|s| s.start_byte);
        for w in sorted.windows(2) {
            assert!(
                w[0].end_byte <= w[1].start_byte,
                "ranges overlap: {:?} and {:?}",
                w[0],
                w[1]
            );
        }
        // Each slice must contain only that function's body.
        for sym in &sorted {
            let slice = &source[sym.start_byte as usize..sym.end_byte as usize];
            assert!(slice.contains(sym.name.as_ref()), "slice for '{}' missing name", sym.name);
        }
        let alpha = sorted.iter().find(|s| s.name.as_ref() == "alpha").unwrap();
        let slice = &source[alpha.start_byte as usize..alpha.end_byte as usize];
        assert!(!slice.contains("beta"),  "alpha slice bleeds into beta:  {:?}", slice);
        assert!(!slice.contains("gamma"), "alpha slice bleeds into gamma: {:?}", slice);
    }

    // ── Go ────────────────────────────────────────────────────────────────────

    #[test]
    fn go_free_function() {
        let syms = parse_go("package main\n\nfunc Hello(x int) bool {\n\treturn true\n}\n");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name.as_ref(), "Hello");
        assert_eq!(syms[0].kind, SymbolKind::Function);
        assert!(syms[0].parent.is_none());
    }

    #[test]
    fn go_method() {
        let src = "package main\n\ntype Dog struct{}\n\nfunc (d Dog) Speak() string {\n\treturn \"woof\"\n}\n";
        let syms = parse_go(src);
        let method = syms.iter().find(|s| s.name.as_ref() == "Speak").unwrap();
        assert_eq!(method.kind, SymbolKind::Method);
        assert_eq!(method.parent.as_deref(), Some("Dog"));
    }

    #[test]
    fn go_struct_type() {
        let syms = parse_go("package main\n\ntype Point struct {\n\tX, Y int\n}\n");
        let s = syms.iter().find(|s| s.name.as_ref() == "Point").unwrap();
        assert_eq!(s.kind, SymbolKind::Struct);
    }

    // ── Python ───────────────────────────────────────────────────────────────

    #[test]
    fn python_free_function() {
        let syms = parse_py("def greet(name: str) -> str:\n    return f'hello {name}'\n");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name.as_ref(), "greet");
        assert_eq!(syms[0].kind, SymbolKind::Function);
    }

    #[test]
    fn python_class() {
        let syms = parse_py("class Animal:\n    pass\n");
        let c = syms.iter().find(|s| s.name.as_ref() == "Animal").unwrap();
        assert_eq!(c.kind, SymbolKind::Struct);
    }

    #[test]
    fn python_method() {
        let src = "class Dog:\n    def speak(self) -> str:\n        return 'woof'\n";
        let syms = parse_py(src);
        let method = syms.iter().find(|s| s.name.as_ref() == "speak").unwrap();
        assert_eq!(method.kind, SymbolKind::Method);
        assert_eq!(method.parent.as_deref(), Some("Dog"));
    }

    // ── TypeScript ───────────────────────────────────────────────────────────

    #[test]
    fn ts_free_function() {
        let syms = parse_ts("function greet(name: string): string {\n  return `hello ${name}`;\n}\n");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name.as_ref(), "greet");
        assert_eq!(syms[0].kind, SymbolKind::Function);
    }

    #[test]
    fn ts_class_and_method() {
        let src = "class Animal {\n  speak(): string {\n    return 'roar';\n  }\n}\n";
        let syms = parse_ts(src);
        let cls = syms.iter().find(|s| s.name.as_ref() == "Animal").unwrap();
        assert_eq!(cls.kind, SymbolKind::Struct);
        let method = syms.iter().find(|s| s.name.as_ref() == "speak").unwrap();
        assert_eq!(method.kind, SymbolKind::Method);
        assert_eq!(method.parent.as_deref(), Some("Animal"));
    }

    #[test]
    fn ts_interface_and_type_alias() {
        let src = "interface Shape { area(): number; }\ntype Color = string;\n";
        let syms = parse_ts(src);
        let iface = syms.iter().find(|s| s.name.as_ref() == "Shape").unwrap();
        assert_eq!(iface.kind, SymbolKind::Trait);
        let alias = syms.iter().find(|s| s.name.as_ref() == "Color").unwrap();
        assert_eq!(alias.kind, SymbolKind::Type);
    }

    #[test]
    fn ts_enum() {
        let syms = parse_ts("enum Direction { Up, Down, Left, Right }\n");
        let e = syms.iter().find(|s| s.name.as_ref() == "Direction").unwrap();
        assert_eq!(e.kind, SymbolKind::Enum);
    }

    #[test]
    fn ts_arrow_function_const() {
        let syms = parse_ts("const add = (a: number, b: number): number => a + b;\n");
        let f = syms.iter().find(|s| s.name.as_ref() == "add").unwrap();
        assert_eq!(f.kind, SymbolKind::Function);
    }
}
