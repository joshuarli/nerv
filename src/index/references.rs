use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use tree_sitter::{Node, Parser};

use super::SymbolIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceSource {
    Ast,
    RgFallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceHit {
    pub file: PathBuf,
    pub line: u32,
    pub context: String,
    pub source: ReferenceSource,
}

/// Hard cap on candidate files to prevent runaway scans on large repos.
/// Partial-result summary message (WS5) is a TODO follow-up.
const MAX_CANDIDATE_FILES: usize = 500;

pub fn find_references(
    index: &SymbolIndex,
    query: &str,
    cwd: &Path,
    file_filter: Option<&Path>,
) -> Result<Vec<ReferenceHit>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("query must be non-empty when references=true".to_string());
    }

    let candidate_files = candidate_files(index, file_filter);
    // WS5: cap file list to prevent runaway scans on large repos.
    let candidate_files: Vec<PathBuf> = candidate_files.into_iter().take(MAX_CANDIDATE_FILES).collect();

    let mut dedup: HashMap<(PathBuf, u32), ReferenceHit> = HashMap::new();

    for file in candidate_files {
        let definition_lines = definition_lines(index, &file, query);
        let mut file_hits = references_for_file(&file, query, cwd);

        // WS3: for fallback hits landing on a definition line, retain them only when the
        // query appears more than once on the line (one occurrence is the definition name;
        // additional occurrences are genuine usages on the same line).
        file_hits.retain(|hit| {
            if !definition_lines.contains(&hit.line) {
                return true;
            }
            if hit.source == ReferenceSource::Ast {
                // AST already filtered the definition name via is_definition_name_node;
                // if the index also marks this line as a definition, drop conservatively.
                return false;
            }
            // RgFallback: retain when the query appears more than once on the line.
            let ext = hit.file.extension().and_then(|e| e.to_str()).unwrap_or("");
            count_word_occurrences(&hit.context, query, ext) > 1
        });

        for mut hit in file_hits {
            let normalized = normalize_file_identity(&hit.file, cwd);
            hit.file = normalized.clone();
            let key = (normalized, hit.line);
            match dedup.get_mut(&key) {
                Some(existing)
                    if existing.source == ReferenceSource::RgFallback
                        && hit.source == ReferenceSource::Ast =>
                {
                    *existing = hit;
                }
                Some(_) => {}
                None => {
                    dedup.insert(key, hit);
                }
            }
        }
    }

    let mut out: Vec<ReferenceHit> = dedup.into_values().collect();
    out.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.context.cmp(&b.context))
    });
    Ok(out)
}

fn candidate_files(index: &SymbolIndex, file_filter: Option<&Path>) -> Vec<PathBuf> {
    let canonical_filter = file_filter.and_then(|f| f.canonicalize().ok());
    let filter_ref = canonical_filter.as_deref().or(file_filter);
    let mut files: BTreeSet<PathBuf> = index
        .files
        .keys()
        .filter(|path| filter_ref.map(|f| path.starts_with(f)).unwrap_or(true))
        .cloned()
        .collect();

    if let Some(filter) = filter_ref {
        if filter.is_file() {
            files.insert(filter.to_path_buf());
        } else if filter.is_dir() {
            collect_all_files(filter, &mut files);
        }
    }

    files.into_iter().collect()
}

fn collect_all_files(dir: &Path, out: &mut BTreeSet<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == ".git" || name == "target" || name == "node_modules" {
                continue;
            }
            collect_all_files(&path, out);
        } else if path.is_file() {
            out.insert(path.canonicalize().unwrap_or(path));
        }
    }
}

fn definition_lines(index: &SymbolIndex, file: &Path, query: &str) -> HashSet<u32> {
    index
        .files
        .get(file)
        .map(|entry| {
            entry
                .symbols
                .iter()
                .filter(|sym| sym.name.as_ref() == query)
                .map(|sym| sym.line)
                .collect()
        })
        .unwrap_or_default()
}

fn references_for_file(file: &Path, query: &str, cwd: &Path) -> Vec<ReferenceHit> {
    let source = match std::fs::read_to_string(file) {
        Ok(source) => source,
        Err(_) => return vec![],
    };
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
    if is_ast_supported(ext) {
        if let Ok(hits) = ast_references_for_file(file, ext, &source, query) {
            return hits;
        }
    }
    // Pass source to fallback for WS2 blocked-lines pre-scan.
    rg_fallback_references_for_file(file, ext, &source, query, cwd)
}

// WS1: per-language AST traversal config.
struct LanguageConfig {
    /// Node kinds that count as identifier-like and will be matched against the query.
    identifier_kinds: &'static [&'static str],
    /// (parent_kind, field_name) pairs: if the node is the child at `field_name` of a
    /// parent whose kind matches `parent_kind`, the node is a definition name.
    definition_parents: &'static [(&'static str, &'static str)],
}

static RUST_CONFIG: LanguageConfig = LanguageConfig {
    identifier_kinds: &["identifier", "type_identifier", "field_identifier"],
    definition_parents: &[
        ("function_item", "name"),
        ("function_signature_item", "name"),
        ("struct_item", "name"),
        ("enum_item", "name"),
        ("union_item", "name"),
        ("type_item", "name"),
        ("trait_item", "name"),
        ("mod_item", "name"),
        ("macro_definition", "name"),
        ("const_item", "name"),
        ("static_item", "name"),
    ],
};

static GO_CONFIG: LanguageConfig = LanguageConfig {
    // Go method names are field_identifier; function names are identifier.
    identifier_kinds: &["identifier", "field_identifier", "type_identifier"],
    definition_parents: &[
        ("function_declaration", "name"),
        ("method_declaration", "name"),
        ("type_spec", "name"),
        ("var_spec", "name"),
        ("const_spec", "name"),
        // short_var_declaration and range_clause are handled as special cases below.
    ],
};

static PYTHON_CONFIG: LanguageConfig = LanguageConfig {
    identifier_kinds: &["identifier"],
    definition_parents: &[
        ("function_definition", "name"),
        ("class_definition", "name"),
        ("named_expression", "name"),    // walrus operator :=
        ("typed_parameter", "name"),
        ("default_parameter", "name"),
        ("typed_default_parameter", "name"),
        // Simple positional params (identifier direct child of `parameters`) and
        // for-loop targets are handled as special cases below.
    ],
};

static TYPESCRIPT_CONFIG: LanguageConfig = LanguageConfig {
    identifier_kinds: &[
        "identifier",
        "type_identifier",
        "property_identifier",
        "shorthand_property_identifier",
        "shorthand_property_identifier_pattern",
    ],
    definition_parents: &[
        ("function_declaration", "name"),
        ("function_signature", "name"),
        ("class_declaration", "name"),
        ("abstract_class_declaration", "name"),
        ("interface_declaration", "name"),
        ("type_alias_declaration", "name"),
        ("enum_declaration", "name"),
        ("variable_declarator", "name"),
        ("method_definition", "name"),
        ("required_parameter", "name"),
        ("optional_parameter", "name"),
    ],
};

fn language_config(ext: &str) -> &'static LanguageConfig {
    match ext {
        "rs" => &RUST_CONFIG,
        "go" => &GO_CONFIG,
        "py" => &PYTHON_CONFIG,
        "ts" | "tsx" => &TYPESCRIPT_CONFIG,
        _ => &TYPESCRIPT_CONFIG,
    }
}

fn ast_references_for_file(
    file: &Path,
    ext: &str,
    source: &str,
    query: &str,
) -> Result<Vec<ReferenceHit>, ()> {
    let language = language_for_extension(ext).ok_or(())?;
    let config = language_config(ext);
    let mut parser = Parser::new();
    parser.set_language(&language).map_err(|_| ())?;
    let tree = parser.parse(source, None).ok_or(())?;

    let source_bytes = source.as_bytes();
    let lines: Vec<&str> = source.lines().collect();
    let mut stack = vec![tree.root_node()];
    let mut hits = Vec::new();

    while let Some(node) = stack.pop() {
        if config.identifier_kinds.contains(&node.kind())
            && !is_inside_comment_or_string(node)
            && !is_definition_name_node(node, config)
            && let Ok(text) = node.utf8_text(source_bytes)
            && text == query
        {
            let line = node.start_position().row as usize + 1;
            let context = lines
                .get(line.saturating_sub(1))
                .map(|l| l.trim_end().to_string())
                .unwrap_or_default();
            hits.push(ReferenceHit {
                file: file.to_path_buf(),
                line: line as u32,
                context,
                source: ReferenceSource::Ast,
            });
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    Ok(hits)
}

fn rg_fallback_references_for_file(
    file: &Path,
    ext: &str,
    source: &str,       // WS2: used for blocked-lines pre-scan
    query: &str,
    cwd: &Path,
) -> Vec<ReferenceHit> {
    let rg = match crate::rg() {
        Some(rg) => rg,
        None => return vec![],
    };

    // WS2: compute lines that are inside block comments or multiline strings.
    let blocked = blocked_lines(source, ext);

    let output = match Command::new(rg)
        .args([
            "--with-filename",
            "--no-heading",
            "--line-number",
            "--color=never",
            "--word-regexp",
            "--fixed-strings",
            query,
            &file.to_string_lossy(),
        ])
        .current_dir(cwd)
        .output()
    {
        Ok(output) => output,
        Err(_) => return vec![],
    };

    if !output.status.success() && output.status.code() != Some(1) {
        return vec![];
    }

    let mut hits = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.splitn(3, ':');
        let path_part = match parts.next() {
            Some(path) => path,
            None => continue,
        };
        let line_part = match parts.next() {
            Some(line_part) => line_part,
            None => continue,
        };
        let context = match parts.next() {
            Some(context) => context,
            None => continue,
        };
        let line_no: u32 = match line_part.parse() {
            Ok(line_no) => line_no,
            Err(_) => continue,
        };

        // WS2: skip lines inside block comments or multiline strings.
        if blocked.contains(&line_no) {
            continue;
        }

        if !line_has_identifier_outside_comment_or_string(context, query, ext) {
            continue;
        }

        let parsed_path = if path_part.is_empty() {
            file.to_path_buf()
        } else {
            let p = PathBuf::from(path_part);
            if p.is_absolute() { p } else { cwd.join(p) }
        };
        hits.push(ReferenceHit {
            file: parsed_path,
            line: line_no,
            context: context.to_string(),
            source: ReferenceSource::RgFallback,
        });
    }
    hits
}

fn normalize_file_identity(path: &Path, cwd: &Path) -> PathBuf {
    let abs = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
    abs.canonicalize().unwrap_or(abs)
}

fn is_ast_supported(ext: &str) -> bool {
    matches!(ext, "rs" | "go" | "py" | "ts" | "tsx")
}

fn language_for_extension(ext: &str) -> Option<tree_sitter::Language> {
    match ext {
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "py" => Some(tree_sitter_python::LANGUAGE.into()),
        "ts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        _ => None,
    }
}

fn is_inside_comment_or_string(node: Node<'_>) -> bool {
    let mut current = Some(node);
    while let Some(n) = current {
        let kind = n.kind();
        if kind.contains("comment")
            || kind.contains("string")
            || matches!(
                kind,
                "char_literal"
                    | "interpreted_string_literal"
                    | "raw_string_literal"
                    | "rune_literal"
                    | "template_string"
            )
        {
            return true;
        }
        current = n.parent();
    }
    false
}

/// WS1: Returns true if `node` is the name identifier of a definition construct.
/// Uses per-language config for standard cases, plus explicit special-case checks
/// for constructs not expressible as (parent_kind, field_name) pairs.
fn is_definition_name_node(node: Node<'_>, config: &LanguageConfig) -> bool {
    let Some(parent) = node.parent() else { return false };

    // Standard table check: traverse children_by_field_name to handle multiple-name
    // constructs correctly (e.g. Go `const A, B = 1, 2` where var_spec.name is multiple).
    for &(parent_kind, field_name) in config.definition_parents {
        if parent.kind() == parent_kind {
            let mut cursor = parent.walk();
            if parent.children_by_field_name(field_name, &mut cursor).any(|n| n.id() == node.id())
            {
                return true;
            }
        }
    }

    // Go: short_var_declaration LHS — `target := foo()`
    //     Tree: short_var_declaration → left: expression_list → identifier(target)
    // Go: range_clause LHS — `for i, v := range items`
    //     Tree: range_clause → left: expression_list → identifier(i), identifier(v)
    if parent.kind() == "expression_list" {
        if let Some(grandparent) = parent.parent() {
            if matches!(grandparent.kind(), "short_var_declaration" | "range_clause")
                && grandparent.child_by_field_name("left").is_some_and(|n| n.id() == parent.id())
            {
                return true;
            }
        }
    }

    // Python: simple positional parameter — `def foo(x):`
    //     Tree: function_definition → parameters → identifier(x)
    //     (identifier satisfies the `parameter` supertype; no wrapper node)
    if parent.kind() == "parameters" {
        return true;
    }

    // Python: for-loop target — `for x in items:` and `[x for x in items]`
    //     Tree: for_statement → left: identifier(x)  (simple)
    //           for_statement → left: pattern_list → identifier(a), identifier(b)  (tuple)
    //     Same structure for for_in_clause (list comprehensions).
    if matches!(parent.kind(), "for_statement" | "for_in_clause")
        && parent.child_by_field_name("left").is_some_and(|n| n.id() == node.id())
    {
        return true;
    }
    if parent.kind() == "pattern_list" {
        if let Some(grandparent) = parent.parent() {
            if matches!(grandparent.kind(), "for_statement" | "for_in_clause")
                && grandparent
                    .child_by_field_name("left")
                    .is_some_and(|n| n.id() == parent.id())
            {
                return true;
            }
        }
    }

    false
}

/// WS2: Returns the set of 1-based line numbers that are fully inside a block
/// comment (`/* ... */`) or a Python/JS triple-quoted string at the **start** of
/// the line.  A line is "blocked" when a multi-line delimiter was already open
/// when the line began; the opening and closing lines themselves are NOT blocked.
///
/// Scanning is byte-level which is safe because none of the ASCII delimiter bytes
/// (`/`, `*`, `"`, `'`, `\n`) appear as interior bytes in multi-byte UTF-8 sequences.
fn blocked_lines(source: &str, ext: &str) -> HashSet<u32> {
    let use_c_block = !matches!(ext, "py"); // /* */ comments in most languages
    let use_python_triple = ext == "py";
    // JS/TS template literals span lines too, but they can contain nested expressions
    // making accurate tracking complex; omit for now (rare false negatives only).

    let mut blocked = HashSet::new();
    let mut line: u32 = 1;
    let mut in_block = false;
    let mut in_triple: Option<u8> = None; // delimiter byte: b'"' or b'\''
    let bytes = source.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];

        if b == b'\n' {
            line += 1;
            // Mark the NEW line as blocked if a multi-line delimiter is still open.
            if in_block || in_triple.is_some() {
                blocked.insert(line);
            }
            i += 1;
            continue;
        }

        if let Some(delim) = in_triple {
            // Scan for closing triple.
            if b == delim
                && bytes.get(i + 1) == Some(&delim)
                && bytes.get(i + 2) == Some(&delim)
            {
                in_triple = None;
                i += 3;
            } else {
                i += 1;
            }
            continue;
        }

        if in_block {
            if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                in_block = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        // Normal state: look for new delimiters.
        if use_python_triple
            && (b == b'"' || b == b'\'')
            && bytes.get(i + 1) == Some(&b)
            && bytes.get(i + 2) == Some(&b)
        {
            in_triple = Some(b);
            i += 3;
            continue;
        }
        if use_c_block && b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            in_block = true;
            i += 2;
            continue;
        }
        i += 1;
    }

    blocked
}

/// WS3: Count how many times `query` appears as a word-boundary identifier
/// outside comments and strings on `line`.  Used to determine whether a line
/// that is also a definition line contains additional genuine usages.
fn count_word_occurrences(line: &str, query: &str, ext: &str) -> usize {
    let mut count = 0usize;
    let mut in_string: Option<char> = None;
    let mut escaped = false;
    let is_python = ext == "py";
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let (idx, ch) = chars[i];

        if let Some(delim) = in_string {
            if escaped {
                escaped = false;
                i += 1;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                i += 1;
                continue;
            }
            if ch == delim {
                in_string = None;
            }
            i += 1;
            continue;
        }

        if is_python && ch == '#' {
            break;
        }
        if ch == '/' && i + 1 < chars.len() {
            let (_, next) = chars[i + 1];
            if next == '/' || next == '*' {
                break;
            }
        }
        if ch == '"' || ch == '\'' || (!is_python && ch == '`') {
            in_string = Some(ch);
            i += 1;
            continue;
        }
        if is_ident_start(ch) {
            let start = idx;
            let mut end = idx + ch.len_utf8();
            let mut j = i + 1;
            while j < chars.len() {
                let (jidx, jch) = chars[j];
                if !is_ident_continue(jch) {
                    break;
                }
                end = jidx + jch.len_utf8();
                j += 1;
            }
            if &line[start..end] == query && is_word_boundary(line, start, end) {
                count += 1;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    count
}

fn line_has_identifier_outside_comment_or_string(line: &str, query: &str, ext: &str) -> bool {
    count_word_occurrences(line, query, ext) > 0
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn is_word_boundary(line: &str, start: usize, end: usize) -> bool {
    let prev_ok =
        line.get(..start).and_then(|s| s.chars().next_back()).is_none_or(|c| !is_ident_continue(c));
    let next_ok =
        line.get(end..).and_then(|s| s.chars().next()).is_none_or(|c| !is_ident_continue(c));
    prev_ok && next_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_index(files: &[(&str, &str)]) -> (tempfile::TempDir, SymbolIndex) {
        let tmp = tempfile::TempDir::new().unwrap();
        for (name, content) in files {
            let path = tmp.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, content).unwrap();
        }
        let mut index = SymbolIndex::new();
        index.force_index_dir(tmp.path());
        (tmp, index)
    }

    #[test]
    fn references_are_usages_only_for_ast_languages() {
        let (tmp, index) = setup_index(&[
            (
                "a.rs",
                "fn target() {}\nfn rust_use() { target(); }\n// target\nfn x(){ let _s = \"target\"; }\n",
            ),
            ("b.go", "func target() {}\nfunc goUse(){ target() }\n// target\nvar _ = \"target\"\n"),
            (
                "c.py",
                "def target():\n    pass\n\ndef py_use():\n    target()\n# target\ns = \"target\"\n",
            ),
            (
                "d.ts",
                "function target() {}\nfunction tsUse(){ target(); }\n// target\nconst s = \"target\";\n",
            ),
            (
                "e.tsx",
                "function target() { return 1; }\nexport function View(){ return <div>{target()}</div>; }\n// target\nconst s = \"target\";\n",
            ),
        ]);

        let hits = find_references(&index, "target", tmp.path(), None).unwrap();
        let root = tmp.path().canonicalize().unwrap_or_else(|_| tmp.path().to_path_buf());
        let rel_lines: BTreeSet<String> = hits
            .iter()
            .map(|h| {
                let rel = h
                    .file
                    .strip_prefix(tmp.path())
                    .or_else(|_| h.file.strip_prefix(&root))
                    .unwrap();
                format!("{}:{}", rel.display(), h.line)
            })
            .collect();

        assert!(rel_lines.contains("a.rs:2"), "missing rust usage: {rel_lines:#?}");
        assert!(rel_lines.contains("b.go:2"), "missing go usage: {rel_lines:#?}");
        assert!(rel_lines.contains("c.py:5"), "missing python usage: {rel_lines:#?}");
        assert!(rel_lines.contains("d.ts:2"), "missing ts usage: {rel_lines:#?}");
        assert!(rel_lines.contains("e.tsx:2"), "missing tsx usage: {rel_lines:#?}");

        assert!(!rel_lines.contains("a.rs:1"), "definition should be excluded");
        assert!(!rel_lines.contains("b.go:1"), "definition should be excluded");
        assert!(!rel_lines.contains("c.py:1"), "definition should be excluded");
        assert!(!rel_lines.contains("d.ts:1"), "definition should be excluded");
        assert!(!rel_lines.contains("e.tsx:1"), "definition should be excluded");
    }

    #[test]
    fn unsupported_files_use_rg_fallback_with_word_boundary() {
        let (tmp, index) = setup_index(&[("lib.rs", "fn keep_index_non_empty() {}\n")]);
        let notes = tmp.path().join("notes.txt");
        std::fs::write(&notes, "targeter\nname = target\n").unwrap();

        let hits = find_references(&index, "target", tmp.path(), Some(&notes)).unwrap();
        let root = tmp.path().canonicalize().unwrap_or_else(|_| tmp.path().to_path_buf());
        let rel_lines: BTreeSet<String> = hits
            .iter()
            .map(|h| {
                let rel = h
                    .file
                    .strip_prefix(tmp.path())
                    .or_else(|_| h.file.strip_prefix(&root))
                    .unwrap();
                format!("{}:{}", rel.display(), h.line)
            })
            .collect();
        assert_eq!(rel_lines, BTreeSet::from([String::from("notes.txt:2")]));
    }

    #[test]
    fn empty_query_is_validation_error() {
        let (_tmp, index) = setup_index(&[("lib.rs", "fn hello() {}\n")]);
        let err = find_references(&index, "   ", Path::new("."), None).unwrap_err();
        assert!(err.contains("query must be non-empty"));
    }

    // WS1 tests

    #[test]
    fn go_short_var_declaration_lhs_is_not_a_reference() {
        // `x := target()` — x is being defined, not used
        let (tmp, index) = setup_index(&[(
            "main.go",
            "func use() {\n\tx := target()\n\t_ = x\n}\nfunc target() int { return 1 }\n",
        )]);
        let hits = find_references(&index, "x", tmp.path(), None).unwrap();
        let lines: Vec<u32> = hits.iter().map(|h| h.line).collect();
        assert!(!lines.contains(&2), "short_var lhs should not be a reference: {:?}", lines);
        assert!(lines.contains(&3), "usage of x should be found: {:?}", lines);
    }

    #[test]
    fn go_range_clause_lhs_is_not_a_reference() {
        // `for i, v := range items` — i and v are being defined
        let (tmp, index) = setup_index(&[(
            "range.go",
            "func f() {\n\titems := []int{1,2}\n\tfor i, v := range items {\n\t\t_ = i + v\n\t}\n}\n",
        )]);
        let hits = find_references(&index, "i", tmp.path(), None).unwrap();
        let lines: Vec<u32> = hits.iter().map(|h| h.line).collect();
        assert!(!lines.contains(&3), "range lhs should not be a reference: {:?}", lines);
        assert!(lines.contains(&4), "usage of i should be found: {:?}", lines);
    }

    #[test]
    fn python_for_loop_variable_is_not_a_reference() {
        // `for x in items:` — x is the loop variable definition
        let (tmp, index) = setup_index(&[(
            "loop.py",
            "items = [1, 2]\nfor x in items:\n    print(x)\n",
        )]);
        let hits = find_references(&index, "x", tmp.path(), None).unwrap();
        let lines: Vec<u32> = hits.iter().map(|h| h.line).collect();
        assert!(!lines.contains(&2), "for-loop variable should not be a reference: {:?}", lines);
        assert!(lines.contains(&3), "body usage of loop var should be found: {:?}", lines);
    }

    #[test]
    fn python_simple_parameter_is_not_a_reference() {
        // `def foo(x):` — x is a parameter definition
        let (tmp, index) = setup_index(&[(
            "param.py",
            "def foo(x):\n    return x + 1\n",
        )]);
        let hits = find_references(&index, "x", tmp.path(), None).unwrap();
        let lines: Vec<u32> = hits.iter().map(|h| h.line).collect();
        assert!(!lines.contains(&1), "simple param should not be a reference: {:?}", lines);
        assert!(lines.contains(&2), "usage of param in body should be found: {:?}", lines);
    }

    #[test]
    fn go_var_spec_name_is_not_a_reference() {
        // `var x int = 0` — x is being defined
        let (tmp, index) = setup_index(&[(
            "vars.go",
            "var x int = 0\nfunc use() int { return x }\n",
        )]);
        let hits = find_references(&index, "x", tmp.path(), None).unwrap();
        let lines: Vec<u32> = hits.iter().map(|h| h.line).collect();
        assert!(!lines.contains(&1), "var_spec name should not be a reference: {:?}", lines);
        assert!(lines.contains(&2), "usage of x should be found: {:?}", lines);
    }

    // WS2 tests

    #[test]
    fn blocked_lines_c_style_block_comment() {
        // Opening line is NOT blocked; interior lines ARE blocked; closing line IS blocked
        // (the block was open at the START of the closing line).
        let src = "code1\n/* block\ninterior\n*/\ncode2\n";
        let b = blocked_lines(src, "js");
        assert!(!b.contains(&1), "code1 should not be blocked");
        assert!(!b.contains(&2), "opening line should not be blocked (block not yet open at line start)");
        assert!(b.contains(&3), "interior line should be blocked: {:?}", b);
        assert!(b.contains(&4), "closing line should be blocked (block open at line 4 start): {:?}", b);
        assert!(!b.contains(&5), "code after block should not be blocked");
    }

    #[test]
    fn blocked_lines_python_triple_double_quote() {
        let src = "x = 1\n\"\"\"\ninterior\n\"\"\"\ny = 2\n";
        let b = blocked_lines(src, "py");
        assert!(!b.contains(&1));
        assert!(!b.contains(&2), "triple opens on line 2, not open at line 2 start");
        assert!(b.contains(&3), "open at line 3 start: {:?}", b);
        assert!(b.contains(&4), "still open at line 4 start: {:?}", b);
        assert!(!b.contains(&5), "closed by end of line 4");
    }

    #[test]
    fn blocked_lines_python_triple_single_quote() {
        let src = "a = '''\nfoo\n'''\nb = 1\n";
        let b = blocked_lines(src, "py");
        assert!(!b.contains(&1));
        assert!(b.contains(&2), "interior: {:?}", b);
        assert!(b.contains(&3), "closing line start still blocked: {:?}", b);
        assert!(!b.contains(&4));
    }

    #[test]
    fn blocked_lines_rust_no_c_block_in_py_mode() {
        // Python mode should not treat /* */ as block comments
        let src = "/* not a block */\ncode\n";
        let b = blocked_lines(src, "py");
        assert!(b.is_empty(), "python mode should not block C-style comment lines: {:?}", b);
    }

    #[test]
    fn fallback_block_comment_interior_suppressed() {
        // rg_fallback should skip hits on lines blocked by /* ... */
        let (tmp, index) = setup_index(&[("lib.rs", "fn placeholder() {}\n")]);
        // Use a .js file so it goes through rg fallback (unsupported extension → fallback)
        // AND has C-style block comments.
        let js = tmp.path().join("code.js");
        std::fs::write(&js, "/* \ntarget\n*/\nconst x = target;\n").unwrap();
        let hits = find_references(&index, "target", tmp.path(), Some(&js)).unwrap();
        let root = tmp.path().canonicalize().unwrap_or_else(|_| tmp.path().to_path_buf());
        let lines: BTreeSet<u32> = hits
            .iter()
            .filter(|h| {
                let rel = h.file.strip_prefix(tmp.path()).or_else(|_| h.file.strip_prefix(&root));
                rel.map(|r| r == std::path::Path::new("code.js")).unwrap_or(false)
            })
            .map(|h| h.line)
            .collect();
        assert!(!lines.contains(&2), "interior block-comment hit should be suppressed: {:?}", lines);
        assert!(lines.contains(&4), "real usage on line 4 should be found: {:?}", lines);
    }

    // WS3 tests

    #[test]
    fn count_word_occurrences_basic() {
        assert_eq!(count_word_occurrences("fn target() {}", "target", "rs"), 1);
        assert_eq!(count_word_occurrences("const target = target();", "target", "js"), 2);
        assert_eq!(count_word_occurrences("let _s = \"target\";", "target", "rs"), 0);
    }

    #[test]
    fn count_word_occurrences_comment_excluded() {
        assert_eq!(count_word_occurrences("code; // target", "target", "rs"), 0);
        assert_eq!(count_word_occurrences("let x = target; // target", "target", "rs"), 1);
    }

    #[test]
    fn definition_line_with_dual_occurrence_retained_for_fallback() {
        // A line where query appears twice — once as the definition, once as a genuine usage.
        // WS3: count > 1 should cause the hit to be RETAINED.
        assert_eq!(
            count_word_occurrences("const target = target();", "target", "js"),
            2,
            "should count both the variable name and the function call"
        );
    }

    #[test]
    fn definition_line_single_occurrence_would_be_dropped() {
        assert_eq!(count_word_occurrences("fn target() {}", "target", "rs"), 1);
        // count == 1 → WS3 would drop this hit on a definition line
    }
}
