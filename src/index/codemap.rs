use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// Result of the search phase — tells the caller what to do next.
pub enum SearchResult {
    /// Matched symbols (owned, safe to use after dropping the index lock).
    Found(Vec<SymbolDef>),
    /// Non-empty query matched nothing, but definitions exist. Contains redirect message.
    Redirect(String),
    /// No definitions in scope at all.
    Empty,
}

/// Search phase: queries the index and returns owned results.
/// Call this while holding the index lock, then drop the lock before calling `render`.
pub fn search(index: &SymbolIndex, params: &CodemapParams) -> SearchResult {
    let results = index.search(params.query, params.kind, params.file);
    if results.is_empty() {
        if !params.query.is_empty() {
            let total = index.search("", params.kind, params.file).len();
            if total > 0 {
                return SearchResult::Redirect(format!(
                    "No symbols matching '{}'. {} definitions exist in this scope — use query: \"\" to see them all.",
                    params.query, total
                ));
            }
        }
        return SearchResult::Empty;
    }
    SearchResult::Found(results.into_iter().cloned().collect())
}

/// Render phase: reads source files and formats output. No index lock needed.
///
/// `cached_sources` — map of path → source pre-fetched from `SymbolIndex::sources_for`
/// while the read lock was held.  On a warm index this covers every file and
/// no I/O happens here.  For any path that is absent (cold start, file just
/// written), sources are read in parallel via scoped threads.
pub fn render(
    results: &[SymbolDef],
    project_root: &Path,
    depth: &Depth,
    cached_sources: &HashMap<PathBuf, Arc<String>>,
) -> String {
    let mut by_file: BTreeMap<&Path, Vec<&SymbolDef>> = BTreeMap::new();
    for sym in results {
        by_file.entry(sym.file.as_path()).or_default().push(sym);
    }

    let full_mode = matches!(depth, Depth::Full);

    // For full mode: build a path → source map.
    // 1. Use pre-fetched Arc<String> from the index cache (zero I/O on warm calls).
    // 2. Read any missing files in parallel via scoped threads.
    let file_sources: BTreeMap<&Path, Arc<String>> = if full_mode {
        // Collect which paths need a fresh read.
        let missing: Vec<&Path> = by_file
            .keys()
            .copied()
            .filter(|p| !cached_sources.contains_key(*p))
            .collect();

        // Parallel reads for cache misses.
        let fresh: HashMap<&Path, Arc<String>> = if missing.is_empty() {
            HashMap::new()
        } else {
            let mut results_vec: Vec<Option<(&Path, Arc<String>)>> = vec![None; missing.len()];
            std::thread::scope(|s| {
                let handles: Vec<_> = missing
                    .iter()
                    .zip(results_vec.iter_mut())
                    .map(|(path, slot)| {
                        s.spawn(move || {
                            if let Ok(src) = std::fs::read_to_string(path) {
                                *slot = Some((*path, Arc::new(src)));
                            }
                        })
                    })
                    .collect();
                for h in handles {
                    h.join().ok();
                }
            });
            results_vec.into_iter().flatten().collect()
        };

        by_file
            .keys()
            .filter_map(|path| {
                let src = cached_sources
                    .get(*path)
                    .map(Arc::clone)
                    .or_else(|| fresh.get(path).map(Arc::clone))?;
                Some((*path, src))
            })
            .collect()
    } else {
        BTreeMap::new()
    };

    let full_set: std::collections::HashSet<usize> = if full_mode {
        let refs: Vec<&SymbolDef> = results.iter().collect();
        let cutoff = find_demote_cutoff(&refs, &file_sources);
        (0..cutoff).collect()
    } else {
        std::collections::HashSet::new()
    };

    let mut out = String::new();

    for (path, syms) in &by_file {
        let rel = path.strip_prefix(project_root).unwrap_or(path).display();
        out.push_str(&format!("{}:\n", rel));

        let source = file_sources.get(path).map(|s| s.as_str());

        for sym in syms {
            // Find this symbol's index in the original results to check full_set
            let idx = results.iter().position(|s| std::ptr::eq(s, *sym)).unwrap_or(usize::MAX);
            let use_full = full_set.contains(&idx);

            if use_full {
                if let Some(src) = source {
                    format_full(&mut out, sym, src);
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

/// Convenience wrapper: search + render in one call (holds index for the full duration).
/// Used by CLI; the tool wrapper uses search/render separately to release the lock early.
pub fn codemap(index: &SymbolIndex, project_root: &Path, params: &CodemapParams) -> String {
    match search(index, params) {
        SearchResult::Found(results) => {
            let paths: Vec<&Path> = results.iter().map(|s| s.file.as_path()).collect();
            let sources = index.sources_for(&paths);
            render(&results, project_root, &params.depth, &sources)
        }
        SearchResult::Redirect(msg) => msg,
        SearchResult::Empty => "No symbols found".to_string(),
    }
}

/// Format a symbol with its full source body.
fn format_full(out: &mut String, sym: &SymbolDef, source: &str) {
    let start = sym.start_byte as usize;
    let end = sym.end_byte as usize;
    if start >= source.len() {
        format_signature_inline(out, sym);
        return;
    }
    // Clamp to valid char boundaries (defensive; byte offsets from tree-sitter are exact).
    let start = source.floor_char_boundary(start);
    let end = source.floor_char_boundary(end.min(source.len()));
    let body = &source[start..end];
    let start_line = sym.line as usize; // 1-based line of first byte
    for (i, line) in body.lines().enumerate() {
        out.push_str(&format!("  {:<5} {}\n", start_line + i, line));
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
fn find_demote_cutoff(results: &[&SymbolDef], file_sources: &BTreeMap<&Path, Arc<String>>) -> usize {
    let mut total_lines = 0;
    for (i, sym) in results.iter().enumerate() {
        let body_lines = if file_sources.contains_key(sym.file.as_path()) {
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
        // Non-empty query, definitions exist → redirect message
        assert!(out.contains("No symbols matching 'nonexistent'"), "should redirect: {}", out);
        assert!(out.contains("1 definitions exist"), "should count: {}", out);
    }

    #[test]
    fn no_matches_no_definitions_at_all() {
        // Empty file — no definitions exist, so no redirect
        let (tmp, index) = setup_index(&[("lib.rs", "// just a comment\n")]);
        let params = CodemapParams {
            query: "anything",
            kind: None,
            file: None,
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert_eq!(out, "No symbols found");
    }

    #[test]
    fn empty_query_no_redirect() {
        // Empty query with no kind/file filter on empty index → plain message
        let index = SymbolIndex::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let params = CodemapParams {
            query: "",
            kind: None,
            file: None,
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert_eq!(out, "No symbols found");
    }

    #[test]
    fn redirect_includes_count() {
        let (tmp, index) = setup_index(&[(
            "lib.rs",
            "fn one() {}\nfn two() {}\nfn three() {}\n",
        )]);
        let params = CodemapParams {
            query: "nonexistent",
            kind: None,
            file: None,
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("3 definitions exist"), "should count all defs: {}", out);
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

    #[test]
    fn empty_query_returns_all_definitions() {
        let (tmp, index) = setup_index(&[(
            "lib.rs",
            "fn alpha() {}\nstruct Beta;\nenum Gamma { A, B }\n",
        )]);
        let params = CodemapParams {
            query: "",
            kind: None,
            file: None,
            depth: Depth::Signatures,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("alpha"), "should contain alpha: {}", out);
        assert!(out.contains("Beta"), "should contain Beta: {}", out);
        assert!(out.contains("Gamma"), "should contain Gamma: {}", out);
    }

    #[test]
    fn full_mode_contains_body_and_closing_brace() {
        let source = "fn example() {\n    let x = 1;\n    let y = 2;\n    x + y\n}\n";
        let (tmp, index) = setup_index(&[("lib.rs", source)]);
        let params = CodemapParams {
            query: "example",
            kind: None,
            file: None,
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("let x = 1"), "should contain body line 1: {}", out);
        assert!(out.contains("let y = 2"), "should contain body line 2: {}", out);
        assert!(out.contains("x + y"), "should contain body line 3: {}", out);
        // Closing brace should be included
        let lines: Vec<&str> = out.lines().collect();
        let last_code = lines.iter().rev().find(|l| !l.trim().is_empty()).unwrap();
        assert!(last_code.contains('}'), "should end with closing brace: {}", out);
    }

    #[test]
    fn multi_file_sorted_by_path() {
        let (tmp, index) = setup_index(&[
            ("z.rs", "fn zeta() {}\n"),
            ("a.rs", "fn alpha() {}\n"),
            ("m.rs", "fn mu() {}\n"),
        ]);
        let params = CodemapParams {
            query: "",
            kind: None,
            file: None,
            depth: Depth::Signatures,
        };
        let out = codemap(&index, tmp.path(), &params);
        let a_pos = out.find("a.rs:").unwrap();
        let m_pos = out.find("m.rs:").unwrap();
        let z_pos = out.find("z.rs:").unwrap();
        assert!(a_pos < m_pos, "a.rs should come before m.rs");
        assert!(m_pos < z_pos, "m.rs should come before z.rs");
    }

    #[test]
    fn directory_file_filter() {
        let (tmp, index) = setup_index(&[
            ("src/core/mod.rs", "fn core_fn() {}\n"),
            ("src/tools/mod.rs", "fn tool_fn() {}\n"),
            ("lib.rs", "fn root_fn() {}\n"),
        ]);
        let core_dir = tmp.path().join("src/core");
        let params = CodemapParams {
            query: "",
            kind: None,
            file: Some(&core_dir),
            depth: Depth::Signatures,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("core_fn"), "should contain core_fn: {}", out);
        assert!(!out.contains("tool_fn"), "should not contain tool_fn: {}", out);
        assert!(!out.contains("root_fn"), "should not contain root_fn: {}", out);
    }

    #[test]
    fn kind_filter_with_empty_query() {
        let (tmp, index) = setup_index(&[(
            "lib.rs",
            "fn foo() {}\nstruct Bar;\ntrait Baz {}\nenum Qux { A }\n",
        )]);
        let params = CodemapParams {
            query: "",
            kind: Some(SymbolKind::Trait),
            file: None,
            depth: Depth::Signatures,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("Baz"), "should find trait: {}", out);
        assert!(!out.contains("foo"), "should not find fn: {}", out);
        assert!(!out.contains("Bar"), "should not find struct: {}", out);
        assert!(!out.contains("Qux"), "should not find enum: {}", out);
    }

    #[test]
    fn budget_demotion_still_shows_all_symbols() {
        // All symbols should appear even when budget exceeded — excess as signatures
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
        // First function should have full body (high priority)
        assert!(out.contains("let _0 = 0"), "first func should have body: {}", out);
        // Last function should still appear (as signature)
        assert!(out.contains("func_199"), "last func should be present: {}", out);
    }

    #[test]
    fn impl_methods_grouped_under_file() {
        let source = "\
struct Foo;
impl Foo {
    fn method_a(&self) {}
    fn method_b(&self) -> i32 { 42 }
}
";
        let (tmp, index) = setup_index(&[("lib.rs", source)]);
        let params = CodemapParams {
            query: "",
            kind: None,
            file: None,
            depth: Depth::Signatures,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("Foo"), "should contain struct: {}", out);
        assert!(out.contains("method_a"), "should contain method_a: {}", out);
        assert!(out.contains("method_b"), "should contain method_b: {}", out);
        // Both methods should have parent annotation
        let method_lines: Vec<&str> = out.lines().filter(|l| l.contains("method_")).collect();
        for line in &method_lines {
            assert!(line.contains("(impl Foo)"), "method should show parent: {}", line);
        }
    }


    #[test]
    fn full_body_mid_file_correct() {
        // The target function is NOT at the start of the file.  The old line-Vec
        // approach and the new byte-slice approach must agree on the output.
        let source = concat!(
            "fn preamble_a() { let _ = 1; }\n",
            "fn preamble_b() { let _ = 2; }\n",
            "\n",
            "fn target() {\n",
            "    let answer = 42;\n",
            "    answer\n",
            "}\n",
        );
        let (tmp, index) = setup_index(&[("lib.rs", source)]);
        let params = CodemapParams {
            query: "target",
            kind: None,
            file: None,
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("fn target()"),    "should have signature: {}", out);
        assert!(out.contains("answer = 42"),    "should have body line: {}", out);
        assert!(!out.contains("preamble"),      "should not include preamble: {}", out);
        // Line numbers: fn target() is on line 4
        assert!(out.contains("4     fn target()"), "line number should be 4: {}", out);
    }

    #[test]
    fn full_body_unicode_before_symbol() {
        // Multi-byte characters in preceding code must not corrupt the byte-slice
        // used to extract the target symbol's body.
        let source = concat!(
            "// 日本語 comment: こんにちは\n",
            "fn before() { let _emoji = '🦀'; }\n",
            "\n",
            "fn after_unicode() {\n",
            "    let x = 99;\n",
            "}\n",
        );
        let (tmp, index) = setup_index(&[("lib.rs", source)]);
        let params = CodemapParams {
            query: "after_unicode",
            kind: None,
            file: None,
            depth: Depth::Full,
        };
        let out = codemap(&index, tmp.path(), &params);
        assert!(out.contains("fn after_unicode()"), "should have signature: {}", out);
        assert!(out.contains("x = 99"),             "should have body: {}", out);
        assert!(!out.contains("emoji"),             "should not bleed into before(): {}", out);
    }
}
