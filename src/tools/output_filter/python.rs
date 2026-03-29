/// Output filters for Python test runners: pytest and unittest.

/// Try to compress pytest output.
/// Returns Some(summary) if recognisably a pytest run.
pub fn filter_pytest(text: &str) -> Option<String> {
    // Must look like pytest: has a session-starts banner or a result line
    if !text.contains("test session starts")
        && !text.contains(" passed")
        && !text.contains(" failed")
    {
        return None;
    }

    // Find the final summary line (===...===)
    let summary = text.lines().rfind(|l| {
        let t = l.trim();
        (t.starts_with("=====") || t.starts_with("-----"))
            && (t.contains("passed") || t.contains("failed") || t.contains("error"))
    });

    // All passing
    if let Some(sum) = summary {
        if !sum.contains("failed") && !sum.contains("error") {
            return Some(sum.trim().to_string());
        }
    }

    // Partial/full failure — extract failure sections
    let failures = extract_pytest_failures(text);
    if failures.is_empty() {
        // Fallback: return summary line
        return summary.map(|s| s.trim().to_string());
    }
    let count = failures.len();
    let body = failures.join("\n\n");
    let tail = summary.map(|s| format!("\n{}", s.trim())).unwrap_or_default();
    Some(format!("{} failure(s):\n{}{}", count, body, tail))
}

/// Try to compress Python unittest output.
/// Returns Some(summary) if recognisably a unittest run.
pub fn filter_unittest(text: &str) -> Option<String> {
    // Must have "Ran N tests"
    let ran_line = text.lines().find(|l| l.starts_with("Ran "))?;

    // All passing
    if text.lines().any(|l| l.trim() == "OK") {
        return Some(ran_line.trim().to_string());
    }

    // Failures
    let failures = extract_unittest_failures(text);
    if failures.is_empty() {
        return Some(ran_line.trim().to_string());
    }
    let count = failures.len();
    let body = failures.join("\n\n");
    Some(format!("{} failure(s):\n{}\n{}", count, body, ran_line.trim()))
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Extract FAILED test blocks from pytest output.
///
/// Failure sections look like:
/// ```text
/// ========================= FAILURES =========================
/// _________________________ test_name _________________________
///
/// ... traceback / assertion ...
/// E   AssertionError: ...
///
/// test_foo.py:5: AssertionError
/// ```
fn extract_pytest_failures(text: &str) -> Vec<String> {
    let mut failures: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut in_failures_section = false;
    let mut in_test_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // "FAILURES" or "ERRORS" section header (==== FAILURES ====)
        if is_section_header(trimmed, "FAILURES") || is_section_header(trimmed, "ERRORS") {
            in_failures_section = true;
            in_test_block = false;
            continue;
        }

        // End of failures section: a summary header (===...passed/failed...)
        if in_failures_section && is_final_summary_line(trimmed) {
            if in_test_block && !current.is_empty() {
                failures.push(current.join("\n"));
                current.clear();
                in_test_block = false;
            }
            in_failures_section = false;
            continue;
        }

        if !in_failures_section {
            continue;
        }

        // Test name separator: all underscores with a name in the middle
        // e.g. "_________________________ test_bar _________________________"
        if is_test_separator(trimmed) {
            if in_test_block && !current.is_empty() {
                failures.push(current.join("\n"));
                current.clear();
            }
            in_test_block = true;
            current.push(trimmed); // keep as header
            continue;
        }

        if in_test_block {
            current.push(line);
        }
    }

    if in_test_block && !current.is_empty() {
        failures.push(current.join("\n"));
    }
    failures
}

/// `==== TITLE ====` or `---- TITLE ----`
fn is_section_header(s: &str, title: &str) -> bool {
    let filler: char = if s.starts_with('=') {
        '='
    } else if s.starts_with('-') {
        '-'
    } else {
        return false;
    };
    let core = s.trim_matches(filler).trim();
    core.to_uppercase().contains(&title.to_uppercase())
        && s.chars().filter(|&c| c == filler).count() > 4
}

/// Summary line: `===== N failed, M passed in Xs =====`
fn is_final_summary_line(s: &str) -> bool {
    s.starts_with('=')
        && s.ends_with('=')
        && (s.contains(" passed") || s.contains(" failed") || s.contains(" error"))
}

/// Test separator line: `_____ test_name _____` (at least 5 underscores on each side).
/// Allows alphanumerics, underscores, brackets, dashes, spaces, dots, and `::` for
/// parametrized / class-qualified test names.
fn is_test_separator(s: &str) -> bool {
    if !s.starts_with('_') {
        return false;
    }
    let inner = s.trim_matches('_').trim();
    !inner.is_empty()
        && inner.chars().all(|c| {
            c.is_alphanumeric()
                || c == '_'
                || c == '['
                || c == ']'
                || c == '-'
                || c == ' '
                || c == '.'
                || c == ':'
        })
}

/// Extract FAIL/ERROR blocks from unittest output.
///
/// Unittest failure sections look like:
/// ```text
/// ======================================================================
/// FAIL: test_name (module.TestCase)
/// ----------------------------------------------------------------------
/// Traceback (most recent call last):
///   ...
/// AssertionError: ...
/// ======================================================================
/// FAIL: next_test ...
/// ```
/// The `======` separator before `FAIL:` starts a new block; the one preceding
/// `Ran N tests` is the end of all blocks.
fn extract_unittest_failures(text: &str) -> Vec<String> {
    let mut failures: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut in_block = false;

    for line in text.lines() {
        let t = line.trim();

        if t.starts_with("FAIL:") || t.starts_with("ERROR:") {
            if !current.is_empty() {
                failures.push(current.join("\n"));
                current.clear();
            }
            in_block = true;
            current.push(line);
        } else if in_block {
            // A long "=====" separator signals end of this block (next FAIL or summary)
            if t.starts_with("======") && t.len() > 30 {
                failures.push(current.join("\n"));
                current.clear();
                in_block = false;
            } else if t.starts_with("Traceback") || is_traceback_frame(line) {
                // Skip raw traceback frames — keep assertion error lines
            } else {
                current.push(line);
            }
        }
    }
    if in_block && !current.is_empty() {
        failures.push(current.join("\n"));
    }
    failures
}

fn is_traceback_frame(line: &str) -> bool {
    // "  File "..." line N, in function"
    let t = line.trim();
    t.starts_with("File \"") || (t.starts_with("File ") && t.contains(", line "))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── pytest ───────────────────────────────────────────────────────────────

    #[test]
    fn pytest_all_pass() {
        let output = "\
============================= test session starts ==============================
collected 15 items

test_foo.py ...............                                              [100%]

============================== 15 passed in 0.42s ==============================";
        let r = filter_pytest(output).unwrap();
        assert!(r.contains("15 passed"), "got: {r}");
    }

    #[test]
    fn pytest_single_failure() {
        let output = "\
============================= test session starts ==============================
collected 2 items

test_foo.py .F                                                          [100%]

=================================== FAILURES ===================================
_________________________ test_bar _________________________

    def test_bar():
>       assert 1 == 2
E       AssertionError: assert 1 == 2

test_foo.py:5: AssertionError
========================= 1 failed, 1 passed in 0.03s =========================";
        let r = filter_pytest(output).unwrap();
        assert!(r.contains("failure"), "got: {r}");
        assert!(r.contains("AssertionError"), "got: {r}");
        assert!(r.contains("1 failed"), "got: {r}");
    }

    #[test]
    fn pytest_multiple_failures() {
        let output = "\
============================= test session starts ==============================
collected 3 items

test_foo.py FFF                                                         [100%]

=================================== FAILURES ===================================
_________________________ test_one _________________________

    def test_one():
>       assert False
E       AssertionError

test_foo.py:2: AssertionError
_________________________ test_two _________________________

    def test_two():
>       1/0
E       ZeroDivisionError: division by zero

test_foo.py:6: ZeroDivisionError
========================= 2 failed in 0.03s =========================";
        let r = filter_pytest(output).unwrap();
        assert!(r.starts_with("2 failure(s)"), "got: {r}");
        assert!(r.contains("test_one"), "got: {r}");
        assert!(r.contains("test_two"), "got: {r}");
    }

    #[test]
    fn pytest_errors_section() {
        // pytest uses ERRORS for collection errors, not FAILURES
        let output = "\
============================= test session starts ==============================
collected 1 item / 1 error

==================================== ERRORS ====================================
_________________ ERROR collecting test_broken.py __________________

ImportError while importing test module 'test_broken.py'.

test_broken.py:1: ImportError
========================= 1 error in 0.05s =========================";
        let r = filter_pytest(output).unwrap();
        assert!(r.contains("ImportError") || r.contains("error"), "got: {r}");
    }

    #[test]
    fn pytest_parametrized_test_name() {
        // Parametrized test separator contains brackets: "test_add[1-2-3]"
        let output = "\
=================================== FAILURES ===================================
_____________________ test_add[1-2-3] _____________________

    @pytest.mark.parametrize(...)
>       assert add(1, 2) == 3
E       AssertionError

test_math.py:5: AssertionError
========================= 1 failed in 0.01s =========================";
        // filter_pytest needs "test session starts" or " failed" to trigger
        let output_with_header = format!("test session starts\n{output}");
        let r = filter_pytest(&output_with_header).unwrap();
        assert!(r.contains("test_add[1-2-3]") || r.contains("AssertionError"), "got: {r}");
    }

    #[test]
    fn pytest_not_a_pytest_run_returns_none() {
        assert!(filter_pytest("just some unrelated output\n").is_none());
    }

    // ── unittest ─────────────────────────────────────────────────────────────

    #[test]
    fn unittest_pass() {
        let output = "\
.....................
----------------------------------------------------------------------
Ran 21 tests in 0.003s

OK";
        let r = filter_unittest(output).unwrap();
        assert_eq!(r, "Ran 21 tests in 0.003s");
    }

    #[test]
    fn unittest_single_failure() {
        let output = "\
.F.
======================================================================
FAIL: test_add (test_math.MathTests)
----------------------------------------------------------------------
AssertionError: 2 != 3
----------------------------------------------------------------------
Ran 3 tests in 0.001s

FAILED (failures=1)";
        let r = filter_unittest(output).unwrap();
        assert!(r.contains("FAIL:"), "got: {r}");
        assert!(r.contains("AssertionError"), "got: {r}");
    }

    #[test]
    fn unittest_error_block() {
        // ERROR: (not FAIL:) for exceptions raised outside assertions
        let output = "\
E
======================================================================
ERROR: test_connect (test_net.NetTests)
----------------------------------------------------------------------
ConnectionRefusedError: [Errno 111] Connection refused
----------------------------------------------------------------------
Ran 1 test in 0.002s

FAILED (errors=1)";
        let r = filter_unittest(output).unwrap();
        assert!(r.contains("ERROR:"), "got: {r}");
        assert!(r.contains("ConnectionRefusedError"), "got: {r}");
    }

    #[test]
    fn unittest_multiple_failures() {
        let output = "\
FF
======================================================================
FAIL: test_a (tests.T)
----------------------------------------------------------------------
AssertionError: a
======================================================================
FAIL: test_b (tests.T)
----------------------------------------------------------------------
AssertionError: b
----------------------------------------------------------------------
Ran 2 tests in 0.001s

FAILED (failures=2)";
        let r = filter_unittest(output).unwrap();
        assert!(r.starts_with("2 failure(s)"), "got: {r}");
        assert!(r.contains("test_a"), "got: {r}");
        assert!(r.contains("test_b"), "got: {r}");
    }

    #[test]
    fn unittest_traceback_stripped() {
        let output = "\
F
======================================================================
FAIL: test_x (tests.T)
----------------------------------------------------------------------
Traceback (most recent call last):
  File \"test_x.py\", line 5, in test_x
    self.assertEqual(1, 2)
AssertionError: 1 != 2
----------------------------------------------------------------------
Ran 1 test in 0.001s

FAILED (failures=1)";
        let r = filter_unittest(output).unwrap();
        // Traceback frame lines should be gone
        assert!(!r.contains("File \"test_x.py\""), "traceback frame should be stripped: {r}");
        // But the assertion error itself must remain
        assert!(r.contains("AssertionError"), "assertion must be kept: {r}");
    }

    #[test]
    fn unittest_not_a_unittest_run_returns_none() {
        assert!(filter_unittest("some other output\n").is_none());
    }
}
