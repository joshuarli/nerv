use std::collections::BTreeMap;
use std::path::Path;

use super::{SymbolDef, SymbolIndex, SymbolKind};

/// Max total lines in output before demoting remaining symbols to signatures.
const LINE_BUDGET: usize = 4000;

pub enum Depth {
    Signatures,
    Full,
}

pub struct CodemapParams<'a> {
    pub query: &'a str,
    pub kind: Option<SymbolKind>,
    pub file: Option<&'a Path>,
    pub depth: Depth,
}

/// Core codemap function. Takes an already-locked index, searches for symbols,
/// reads their source bodies from disk, and returns formatted output.
pub fn codemap(index: &SymbolIndex, project_root: &Path, params: &CodemapParams) -> String {
    let results = index.search(params.query, params.kind, params.file);
    if results.is_empty() {
        if !params.query.is_empty() {
            // Check if empty query would find definitions — nudge the model to use it.
            let total = index.search("", params.kind, params.file).len();
            if total > 0 {
                return format!(
                    "No symbols matching '{}'. {} definitions exist in this scope — use query: \"\" to see them all.",
                    params.query, total
                );
            }
        }
        return "No symbols found".to_string();
    }

    // Group by file, preserving search ordering within each file.
    // BTreeMap gives us sorted-by-path output.
    let mut by_file: BTreeMap<&Path, Vec<&SymbolDef>> = BTreeMap::new();
    for sym in &results {
        by_file.entry(sym.file.as_path()).or_default().push(sym);
    }

    // Pre-read all needed files into memory.
    let file_contents: BTreeMap<&Path, Vec<String>> = by_file
        .keys()
        .filter_map(|path| {
            let source = std::fs::read_to_string(path).ok()?;
            let lines: Vec<String> = source.lines().map(|l| l.to_string()).collect();
            Some((*path, lines))
        })
        .collect();

    // In full mode, build a set of symbols that fit within the line budget.
    // Results are in search-priority order (exact matches first), so we grant
    // full treatment to the highest-priority symbols regardless of which file
    // they land in.
    let full_mode = matches!(params.depth, Depth::Full);
    let full_set: std::collections::HashSet<*const SymbolDef> = if full_mode {
        let cutoff = find_demote_cutoff(&results, &file_contents);
        results[..cutoff].iter().map(|s| *s as *const SymbolDef).collect()
    } else {
        std::collections::HashSet::new()
    };

    let mut out = String::new();

    for (path, syms) in &by_file {
        let rel = path
            .strip_prefix(project_root)
            .unwrap_or(path)
            .display();
        out.push_str(&format!("{}:\n", rel));

        let lines = file_contents.get(path);

        for sym in syms {
            let use_full = full_set.contains(&(*sym as *const SymbolDef));

            if use_full {
                if let Some(lines) = lines {
                    format_full(&mut out, sym, lines);
                } else {
                    format_signature(&mut out, sym);
                }
            } else {
                format_signature(&mut out, sym);
            }
        }
        out.push('\n');
    }

    out.truncate(out.trim_end().len());
    out
}

/// Format a symbol with its full source body.
fn format_full(out: &mut String, sym: &SymbolDef, lines: &[String]) {
    let start = (sym.line as usize).saturating_sub(1);
    let end = (sym.end_line as usize).min(lines.len());
    if start >= lines.len() {
        format_signature_inline(out, sym);
        return;
    }
    for i in start..end {
        out.push_str(&format!("  {:<5} {}\n", i + 1, lines[i]));
    }
}

/// Format a symbol as a one-line signature.
fn format_signature(out: &mut String, sym: &SymbolDef) {
    format_signature_inline(out, sym);
}

fn format_signature_inline(out: &mut String, sym: &SymbolDef) {
    let parent_suffix = sym
        .parent
        .as_ref()
        .map(|p| format!("  ({})", p))
        .unwrap_or_default();
    // Signature already contains the keyword (fn, struct, etc.) — no need for kind label
    out.push_str(&format!(
        "  {:<5} {}{}\n",
        sym.line,
        sym.signature,
        parent_suffix,
    ));
}

/// Find how many symbols (in search order) we can show in full mode
/// before exceeding the line budget. Returns the count.
fn find_demote_cutoff(results: &[&SymbolDef], file_contents: &BTreeMap<&Path, Vec<String>>) -> usize {
    let mut total_lines = 0;
    for (i, sym) in results.iter().enumerate() {
        let body_lines = if file_contents.contains_key(sym.file.as_path()) {
            (sym.end_line - sym.line + 1) as usize
        } else {
            1
        };
        total_lines += body_lines;
        if total_lines > LINE_BUDGET {
            return i;
        }
    }
    results.len()
}

pub fn parse_kind(s: &str) -> Option<SymbolKind> {
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

pub fn parse_depth(s: &str) -> Depth {
    match s {
        "full" => Depth::Full,
        _ => Depth::Signatures,
    }
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
            std::fs::write(&path, content).unwrap();
        }
        let mut index = SymbolIndex::new();
        index.force_index_dir(tmp.path());
        (tmp, index)
    }

    #[test]
    fn full_mode_returns_source_body() {
        let (tmp, index) = setup_index(&[(
            "lib.rs",
            "fn hello() {\n    println!(\"hi\");\n}\n",
        )]);
        let params = CodemapParams {
            query: "hello",
            kind: None,
            file: None,
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("println!(\"hi\")"), "should contain function body: {}", out);
        assert!(out.contains("fn hello()"), "should contain signature: {}", out);
    }

    #[test]
    fn signatures_mode_no_body() {
        let (tmp, index) = setup_index(&[(
            "lib.rs",
            "fn hello() {\n    println!(\"hi\");\n}\n",
        )]);
        let params = CodemapParams {
            query: "hello",
            kind: None,
            file: None,
            depth: Depth::Signatures,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("fn hello()"), "should contain signature: {}", out);
        assert!(!out.contains("println!"), "should NOT contain body: {}", out);
    }

    #[test]
    fn multi_file_grouped() {
        let (tmp, index) = setup_index(&[
            ("a.rs", "fn alpha() {}\n"),
            ("b.rs", "fn beta() {}\n"),
        ]);
        let params = CodemapParams {
            query: "",
            kind: Some(SymbolKind::Function),
            file: None,
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("a.rs:"), "should have a.rs header: {}", out);
        assert!(out.contains("b.rs:"), "should have b.rs header: {}", out);
    }

    #[test]
    fn kind_filter_works() {
        let (tmp, index) = setup_index(&[(
            "lib.rs",
            "struct Foo;\nfn bar() {}\n",
        )]);
        let params = CodemapParams {
            query: "",
            kind: Some(SymbolKind::Struct),
            file: None,
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("Foo"), "should find struct: {}", out);
        assert!(!out.contains("bar"), "should not find function: {}", out);
    }

    #[test]
    fn no_matches_returns_message() {
        let (tmp, index) = setup_index(&[("lib.rs", "fn hello() {}\n")]);
        let params = CodemapParams {
            query: "nonexistent",
            kind: None,
            file: None,
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert_eq!(out, "No symbols found");
    }

    #[test]
    fn budget_demotes_to_signatures() {
        // Create a file with many large functions to exceed LINE_BUDGET
        let mut source = String::new();
        for i in 0..200 {
            source.push_str(&format!("fn func_{}() {{\n", i));
            for j in 0..25 {
                source.push_str(&format!("    let _{} = {};\n", j, j));
            }
            source.push_str("}\n\n");
        }
        let (tmp, index) = setup_index(&[("big.rs", &source)]);
        let params = CodemapParams {
            query: "func_",
            kind: None,
            file: None,
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        let line_count = out.lines().count();
        // Should be under budget + some overhead for file headers
        assert!(
            line_count < LINE_BUDGET + 300,
            "output should be bounded: {} lines",
            line_count
        );
    }

    #[test]
    fn file_filter_restricts() {
        let (tmp, index) = setup_index(&[
            ("a.rs", "fn target() {}\n"),
            ("sub/b.rs", "fn target() {}\n"),
        ]);
        let sub = tmp.path().join("sub");
        let params = CodemapParams {
            query: "target",
            kind: None,
            file: Some(&sub),
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("sub/b.rs:") || out.contains("sub\\b.rs:"), "should contain sub/b.rs: {}", out);
        assert!(!out.contains("\na.rs:"), "should not contain a.rs: {}", out);
    }

    #[test]
    fn line_numbers_match_source() {
        let source = "// comment\n\nfn hello() {\n    42\n}\n";
        let (tmp, index) = setup_index(&[("lib.rs", source)]);
        let params = CodemapParams {
            query: "hello",
            kind: None,
            file: None,
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        // fn hello() is on line 3
        assert!(out.contains("3     fn hello()"), "line numbers should match source: {}", out);
    }

    #[test]
    fn methods_show_parent() {
        let source = "struct Agent;\nimpl Agent {\n    fn run(&self) {}\n}\n";
        let (tmp, index) = setup_index(&[("lib.rs", source)]);
        let params = CodemapParams {
            query: "run",
            kind: None,
            file: None,
            depth: Depth::Signatures,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("(impl Agent)"), "should show parent: {}", out);
    }
}
