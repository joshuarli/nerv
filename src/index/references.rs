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
    let mut dedup: HashMap<(PathBuf, u32), ReferenceHit> = HashMap::new();

    for file in candidate_files {
        let definition_lines = definition_lines(index, &file, query);
        let mut file_hits = references_for_file(&file, query, cwd);
        file_hits.retain(|hit| !definition_lines.contains(&hit.line));

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
    rg_fallback_references_for_file(file, ext, query, cwd)
}

fn ast_references_for_file(
    file: &Path,
    ext: &str,
    source: &str,
    query: &str,
) -> Result<Vec<ReferenceHit>, ()> {
    let language = language_for_extension(ext).ok_or(())?;
    let mut parser = Parser::new();
    parser.set_language(&language).map_err(|_| ())?;
    let tree = parser.parse(source, None).ok_or(())?;

    let source_bytes = source.as_bytes();
    let lines: Vec<&str> = source.lines().collect();
    let mut stack = vec![tree.root_node()];
    let mut hits = Vec::new();

    while let Some(node) = stack.pop() {
        if is_identifier_node(node.kind())
            && !is_inside_comment_or_string(node)
            && !is_definition_name_node(node)
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
    query: &str,
    cwd: &Path,
) -> Vec<ReferenceHit> {
    let rg = match crate::rg() {
        Some(rg) => rg,
        None => return vec![],
    };

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

fn is_identifier_node(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "field_identifier"
            | "property_identifier"
            | "type_identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern"
    )
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

fn is_definition_name_node(node: Node<'_>) -> bool {
    const DEFINITION_NODE_KINDS: &[&str] = &[
        "function_item",
        "function_signature_item",
        "struct_item",
        "enum_item",
        "union_item",
        "type_item",
        "trait_item",
        "mod_item",
        "macro_definition",
        "const_item",
        "static_item",
        "function_declaration",
        "method_declaration",
        "type_spec",
        "function_definition",
        "class_definition",
        "function_declaration",
        "method_definition",
        "class_declaration",
        "interface_declaration",
        "type_alias_declaration",
        "enum_declaration",
        "variable_declarator",
    ];
    let parent = match node.parent() {
        Some(parent) => parent,
        None => return false,
    };
    if !DEFINITION_NODE_KINDS.contains(&parent.kind()) {
        return false;
    }
    parent.child_by_field_name("name").is_some_and(|name| name.id() == node.id())
}

fn line_has_identifier_outside_comment_or_string(line: &str, query: &str, ext: &str) -> bool {
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
                return true;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    false
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
}
