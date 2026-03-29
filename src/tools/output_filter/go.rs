//! Output filters for Go test runner (`go test`).
//!
//! `go test -v` produces plain text; `go test -json` produces NDJSON events.
//! We handle both formats.

/// Try to compress `go test` output.
/// Returns Some(summary) if recognisably a go test run.
pub fn filter_go_test(text: &str) -> Option<String> {
    // JSON format: lines start with `{"`
    if text.trim_start().starts_with("{\"") {
        return filter_go_test_json(text);
    }
    filter_go_test_text(text)
}

// ── text format ──────────────────────────────────────────────────────────────

fn filter_go_test_text(text: &str) -> Option<String> {
    // Must look like go test output
    if !text.contains("--- FAIL")
        && !text.contains("--- PASS")
        && !text.contains("ok  \t")
        && !text.contains("FAIL\t")
    {
        return None;
    }

    // All passing: only "ok" lines, no FAIL lines
    if !text.contains("--- FAIL") && !text.contains("FAIL\t") {
        let ok_lines: Vec<&str> =
            text.lines().filter(|l| l.starts_with("ok  \t") || l.starts_with("ok\t")).collect();
        if !ok_lines.is_empty() {
            return Some(ok_lines.join("\n"));
        }
    }

    // Failures: extract FAIL blocks
    let failures = extract_go_text_failures(text);
    if failures.is_empty() {
        // Return FAIL summary lines
        let fail_lines: Vec<&str> =
            text.lines().filter(|l| l.starts_with("FAIL\t") || l.starts_with("--- FAIL")).collect();
        if !fail_lines.is_empty() {
            return Some(fail_lines.join("\n"));
        }
        return None;
    }

    let count = failures.len();
    let body = failures.join("\n\n");
    // Add package-level FAIL lines
    let pkg_fails: Vec<&str> = text.lines().filter(|l| l.starts_with("FAIL\t")).collect();
    let tail =
        if !pkg_fails.is_empty() { format!("\n{}", pkg_fails.join("\n")) } else { String::new() };
    Some(format!("{} failure(s):\n{}{}", count, body, tail))
}

/// Extract `--- FAIL: TestName` blocks and their output.
///
/// In `go test -v` output, test output appears *between* `=== RUN TestName`
/// and `--- FAIL: TestName`, not after the FAIL marker.  We track per-test
/// output in a HashMap so subtests (`TestFoo/sub`) work correctly.
///
/// Non-verbose output (no `=== RUN` lines) just returns the `--- FAIL:`
/// markers with no body, which is handled by the fallback in
/// `filter_go_test_text`.
fn extract_go_text_failures(text: &str) -> Vec<String> {
    let mut failures: Vec<String> = Vec::new();
    // test_name → accumulated output lines
    let mut test_output: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut current_test: Option<String> = None;

    for line in text.lines() {
        if let Some(name) =
            line.strip_prefix("=== RUN   ").or_else(|| line.strip_prefix("=== RUN\t"))
        {
            current_test = Some(name.trim().to_string());
        } else if line.starts_with("--- FAIL:") {
            // Extract test name from "--- FAIL: TestName (0.00s)"
            let name =
                line.trim_start_matches('-').trim().strip_prefix("FAIL:").unwrap_or("").trim();
            let name = name.split_whitespace().next().unwrap_or(name).to_string();
            let body = test_output.remove(&name).unwrap_or_default();
            let body_str = body.join("\n");
            failures.push(if body_str.is_empty() {
                format!("FAILED: {}", name)
            } else {
                format!("FAILED: {}\n{}", name, body_str)
            });
            current_test = None;
        } else if line.starts_with("--- PASS:") {
            // Clear output for passing tests (don't accumulate memory for them)
            if let Some(ref name) = current_test {
                test_output.remove(name);
            }
            current_test = None;
        } else if let Some(ref name) = current_test {
            // Accumulate output for this test (skip blank lines to save space)
            if !line.trim().is_empty() {
                test_output.entry(name.clone()).or_default().push(line.to_string());
            }
        }
    }
    failures
}

// ── JSON (NDJSON) format
// ──────────────────────────────────────────────────────

#[derive(Default)]
struct GoTestAgg {
    packages: std::collections::HashMap<String, PkgResult>,
}

#[derive(Default)]
struct PkgResult {
    passed: usize,
    failed: usize,
    failures: Vec<String>, // "TestName: output"
    current_test: Option<String>,
    current_output: Vec<String>,
}

fn filter_go_test_json(text: &str) -> Option<String> {
    let mut agg = GoTestAgg::default();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let action = v["Action"].as_str().unwrap_or("");
            let pkg = v["Package"].as_str().unwrap_or("").to_string();
            let test = v["Test"].as_str().map(|s| s.to_string());
            let output = v["Output"].as_str().unwrap_or("").to_string();

            let entry = agg.packages.entry(pkg).or_default();

            match action {
                "run" => {
                    if let Some(t) = test {
                        entry.current_test = Some(t);
                        entry.current_output.clear();
                    }
                }
                "output" if !output.trim().is_empty() => {
                    entry.current_output.push(output.trim_end_matches('\n').to_string());
                }
                "pass" if test.is_some() => {
                    entry.passed += 1;
                    entry.current_test = None;
                    entry.current_output.clear();
                }
                "fail" => {
                    if let Some(t) = test {
                        entry.failed += 1;
                        let body = entry.current_output.join("\n");
                        entry.failures.push(if body.is_empty() {
                            format!("FAILED: {}", t)
                        } else {
                            format!("FAILED: {}\n{}", t, body)
                        });
                        entry.current_test = None;
                        entry.current_output.clear();
                    }
                }
                _ => {}
            }
        }
    }

    if agg.packages.is_empty() {
        return None;
    }

    let total_fail: usize = agg.packages.values().map(|p| p.failed).sum();
    let total_pass: usize = agg.packages.values().map(|p| p.passed).sum();

    if total_fail == 0 {
        return Some(format!("{} passed across {} package(s)", total_pass, agg.packages.len()));
    }

    let mut parts: Vec<String> = Vec::new();
    for (pkg, result) in &agg.packages {
        if result.failed > 0 {
            parts.push(format!("{}:", pkg));
            for f in &result.failures {
                parts.push(format!("  {}", f));
            }
        }
    }
    Some(format!("{} failure(s), {} passed:\n{}", total_fail, total_pass, parts.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── text format ──────────────────────────────────────────────────────────

    #[test]
    fn go_test_all_pass_text() {
        let output = "\
=== RUN   TestFoo
--- PASS: TestFoo (0.00s)
=== RUN   TestBar
--- PASS: TestBar (0.00s)
ok  \texample.com/mymod\t0.001s
";
        let r = filter_go_test(output).unwrap();
        assert!(r.contains("ok"), "got: {r}");
        assert!(!r.contains("FAIL"), "got: {r}");
    }

    #[test]
    fn go_test_failure_text() {
        let output = "\
=== RUN   TestFoo
--- PASS: TestFoo (0.00s)
=== RUN   TestBar
    bar_test.go:10: expected 2, got 3
--- FAIL: TestBar (0.00s)
FAIL\texample.com/mymod\t0.001s
";
        let r = filter_go_test(output).unwrap();
        assert!(r.contains("FAILED: TestBar"), "got: {r}");
        assert!(r.contains("expected 2, got 3"), "got: {r}");
    }

    #[test]
    fn go_test_multiple_failures_text() {
        let output = "\
=== RUN   TestA
    a_test.go:5: A failed
--- FAIL: TestA (0.00s)
=== RUN   TestB
    b_test.go:9: B failed
--- FAIL: TestB (0.00s)
FAIL\texample.com/mymod\t0.002s
";
        let r = filter_go_test(output).unwrap();
        assert!(r.starts_with("2 failure(s)"), "got: {r}");
        assert!(r.contains("A failed"), "got: {r}");
        assert!(r.contains("B failed"), "got: {r}");
    }

    #[test]
    fn go_test_subtest_failure_text() {
        // Subtest name contains a slash
        let output = "\
=== RUN   TestMath
=== RUN   TestMath/add
    math_test.go:12: want 5 got 4
--- FAIL: TestMath/add (0.00s)
--- FAIL: TestMath (0.00s)
FAIL\texample.com/mymod\t0.001s
";
        let r = filter_go_test(output).unwrap();
        // Should capture output for the subtest
        assert!(r.contains("want 5 got 4"), "got: {r}");
    }

    #[test]
    fn go_test_no_verbose_no_output() {
        // Non-verbose run: no === RUN lines, just --- FAIL at the end
        let output = "\
--- FAIL: TestX (0.00s)
FAIL\texample.com/mod\t0.001s
";
        let r = filter_go_test(output).unwrap();
        // Falls back to FAIL summary lines
        assert!(r.contains("FAIL"), "got: {r}");
    }

    #[test]
    fn go_test_not_a_go_test_returns_none() {
        assert!(filter_go_test("unrelated output\n").is_none());
    }

    // ── JSON format ──────────────────────────────────────────────────────────

    #[test]
    fn go_test_json_pass() {
        let output = r#"{"Time":"2024-01-01T00:00:00Z","Action":"run","Package":"pkg1","Test":"TestFoo"}
{"Time":"2024-01-01T00:00:00Z","Action":"output","Package":"pkg1","Test":"TestFoo","Output":"=== RUN   TestFoo\n"}
{"Time":"2024-01-01T00:00:00Z","Action":"pass","Package":"pkg1","Test":"TestFoo","Elapsed":0.001}
{"Time":"2024-01-01T00:00:00Z","Action":"pass","Package":"pkg1","Elapsed":0.001}
"#;
        let r = filter_go_test(output).unwrap();
        assert!(r.contains("passed"), "got: {r}");
        assert!(!r.contains("FAILED"), "got: {r}");
    }

    #[test]
    fn go_test_json_failure() {
        let output = r#"{"Action":"run","Package":"pkg1","Test":"TestBad"}
{"Action":"output","Package":"pkg1","Test":"TestBad","Output":"    bad_test.go:5: assertion failed\n"}
{"Action":"fail","Package":"pkg1","Test":"TestBad","Elapsed":0.001}
{"Action":"fail","Package":"pkg1","Elapsed":0.001}
"#;
        let r = filter_go_test(output).unwrap();
        assert!(r.contains("FAILED: TestBad"), "got: {r}");
        assert!(r.contains("assertion failed"), "got: {r}");
    }

    #[test]
    fn go_test_json_multiple_packages() {
        let output = r#"{"Action":"run","Package":"pkg1","Test":"TestA"}
{"Action":"pass","Package":"pkg1","Test":"TestA","Elapsed":0.001}
{"Action":"pass","Package":"pkg1","Elapsed":0.001}
{"Action":"run","Package":"pkg2","Test":"TestB"}
{"Action":"output","Package":"pkg2","Test":"TestB","Output":"    fail!\n"}
{"Action":"fail","Package":"pkg2","Test":"TestB","Elapsed":0.001}
{"Action":"fail","Package":"pkg2","Elapsed":0.001}
"#;
        let r = filter_go_test(output).unwrap();
        assert!(r.contains("pkg2"), "got: {r}");
        assert!(r.contains("TestB"), "got: {r}");
        assert!(r.contains("fail!"), "got: {r}");
    }
}
