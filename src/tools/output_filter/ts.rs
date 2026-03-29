//! Output filters for JavaScript/TypeScript test runners: Jest and Vitest.

/// Try to compress Jest or Vitest test output.
/// Returns Some(summary) if recognisably a jest/vitest run.
pub fn filter_jest(text: &str) -> Option<String> {
    // Must look like jest/vitest
    let is_jest = text.contains("PASS ")
        || text.contains("FAIL ")
        || text.contains("Tests:")
        || text.contains("Test Suites:");
    let is_vitest = text.contains("✓ ")
        || text.contains("✗ ")
        || text.contains("× ") // vitest failure marker
        || (text.contains("passed") && text.contains("failed") && text.contains("ms"));

    if !is_jest && !is_vitest {
        return None;
    }

    if is_jest { filter_jest_output(text) } else { filter_vitest_output(text) }
}

fn filter_jest_output(text: &str) -> Option<String> {
    // Summary lines
    let suites_line = text.lines().find(|l| l.trim_start().starts_with("Test Suites:"));
    let tests_line = text.lines().find(|l| l.trim_start().starts_with("Tests:"));

    // All passing
    if let (Some(s), Some(t)) = (suites_line, tests_line)
        && !s.contains("failed") && !t.contains("failed")
    {
        return Some(format!("{}\n{}", s.trim(), t.trim()));
    }

    // Failures: extract FAIL suite blocks
    let failures = extract_jest_failures(text);
    if failures.is_empty() {
        // Return the summary lines we have
        let summary: Vec<&str> = text
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                t.starts_with("Test Suites:") || t.starts_with("Tests:") || t.starts_with("FAIL ")
            })
            .collect();
        if !summary.is_empty() {
            return Some(summary.join("\n"));
        }
        return None;
    }

    let count = failures.len();
    let body = failures.join("\n\n");
    let summary = tests_line.map(|l| format!("\n{}", l.trim())).unwrap_or_default();
    Some(format!("{} failure(s):\n{}{}", count, body, summary))
}

/// Extract individual test failure blocks from Jest output.
///
/// Each block looks like:
/// ```text
///   ● TestSuite > test name
///
///     expect(received).toBe(expected)
///     ...
/// ```
fn extract_jest_failures(text: &str) -> Vec<String> {
    let mut failures: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut in_failure = false;

    for line in text.lines() {
        // Failure header: "  ● suite › test" or "  ✕ test name"
        if (line.starts_with("  ●") || line.starts_with("  ✕") || line.starts_with("  ✗"))
            && !line.trim_end().ends_with(':')
        {
            if in_failure && !current.is_empty() {
                failures.push(current.join("\n"));
                current.clear();
            }
            in_failure = true;
            current.push(line.trim());
        } else if in_failure {
            // End at the next major section
            let t = line.trim();
            if t.starts_with("Test Suites:")
                || t.starts_with("Tests:")
                || t.starts_with("Snapshots:")
                || t.starts_with("Time:")
            {
                failures.push(current.join("\n"));
                current.clear();
                in_failure = false;
            } else if !should_skip_jest_line(line) {
                current.push(line);
            }
        }
    }
    if in_failure && !current.is_empty() {
        failures.push(current.join("\n"));
    }
    failures
}

fn should_skip_jest_line(line: &str) -> bool {
    let t = line.trim();
    // Skip jest-internal stack frames
    t.starts_with("at ") && (t.contains("node_modules/jest") || t.contains("node_modules/vitest"))
}

fn filter_vitest_output(text: &str) -> Option<String> {
    // Summary line: "Tests  X failed | Y passed (Z)"
    let summary = text.lines().rfind(|l| {
        let t = l.trim();
        (t.starts_with("Tests") || t.starts_with("Test Files"))
            && (t.contains("passed") || t.contains("failed"))
    });

    if let Some(sum) = summary
        && !sum.contains("failed")
    {
        return Some(sum.trim().to_string());
    }

    // Extract × failures
    let failures = extract_vitest_failures(text);
    if failures.is_empty() {
        return summary.map(|s| s.trim().to_string());
    }
    let count = failures.len();
    let body = failures.join("\n\n");
    let tail = summary.map(|s| format!("\n{}", s.trim())).unwrap_or_default();
    Some(format!("{} failure(s):\n{}{}", count, body, tail))
}

fn extract_vitest_failures(text: &str) -> Vec<String> {
    let mut failures: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut in_failure = false;

    for line in text.lines() {
        // Failure marker: "× test name" or "✗ test name"
        if line.trim_start().starts_with("× ") || line.trim_start().starts_with("✗ ") {
            if in_failure && !current.is_empty() {
                failures.push(current.join("\n"));
                current.clear();
            }
            in_failure = true;
            current.push(line.trim());
        } else if in_failure {
            let t = line.trim();
            if t.starts_with("Tests ") || t.starts_with("Test Files ") || t.starts_with("Duration")
            {
                failures.push(current.join("\n"));
                current.clear();
                in_failure = false;
            } else if !t.is_empty() {
                current.push(line);
            }
        }
    }
    if in_failure && !current.is_empty() {
        failures.push(current.join("\n"));
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Jest ─────────────────────────────────────────────────────────────────

    #[test]
    fn jest_all_pass() {
        let output = "\
PASS src/math.test.js
PASS src/utils.test.js

Test Suites: 2 passed, 2 total
Tests:       10 passed, 10 total
Snapshots:   0 total
Time:        1.234 s
";
        let r = filter_jest(output).unwrap();
        assert!(r.contains("Test Suites:"), "got: {r}");
        assert!(!r.contains("FAIL"), "got: {r}");
    }

    #[test]
    fn jest_single_failure() {
        let output = "\
FAIL src/math.test.js
  ● add › returns correct sum

    expect(received).toBe(expected)

    Expected: 5
    Received: 4

      3 | test('returns correct sum', () => {
    > 4 |   expect(add(2, 2)).toBe(5);
        |                     ^
      5 | });

Test Suites: 1 failed, 1 total
Tests:       1 failed, 1 total
";
        let r = filter_jest(output).unwrap();
        assert!(r.contains("failure"), "got: {r}");
        assert!(r.contains("Expected: 5"), "got: {r}");
    }

    #[test]
    fn jest_multiple_failures() {
        let output = "\
FAIL src/math.test.js
  ● add › wrong result

    Expected: 3
    Received: 2

  ● subtract › wrong result

    Expected: 1
    Received: 0

Test Suites: 1 failed, 1 total
Tests:       2 failed, 2 total
";
        let r = filter_jest(output).unwrap();
        assert!(r.starts_with("2 failure(s)"), "got: {r}");
        assert!(r.contains("add"), "got: {r}");
        assert!(r.contains("subtract"), "got: {r}");
    }

    #[test]
    fn jest_cross_marker() {
        // Some jest versions use ✕ instead of ●
        let output = "\
FAIL src/foo.test.ts
  ✕ foo should work

    Error: not working

Test Suites: 1 failed, 1 total
Tests:       1 failed, 1 total
";
        let r = filter_jest(output).unwrap();
        assert!(r.contains("foo should work") || r.contains("failure"), "got: {r}");
    }

    #[test]
    fn jest_internal_frames_stripped() {
        let output = "\
FAIL src/math.test.js
  ● test

    Error: boom
    at Object.<anonymous> (src/math.test.js:3:10)
    at runTest (node_modules/jest-circus/build/run.js:120:12)
    at node_modules/jest-circus/build/utils.js:456:15

Test Suites: 1 failed, 1 total
Tests:       1 failed, 1 total
";
        let r = filter_jest(output).unwrap();
        // Internal jest frame should be stripped
        assert!(!r.contains("node_modules/jest-circus"), "internal frame should be stripped: {r}");
        // User frame should be kept
        assert!(r.contains("math.test.js"), "got: {r}");
    }

    #[test]
    fn jest_no_match_returns_none() {
        assert!(filter_jest("some random output\n").is_none());
    }

    // ── Vitest ───────────────────────────────────────────────────────────────

    #[test]
    fn vitest_all_pass() {
        let output = "\
 ✓ src/math.test.ts (3)
 ✓ src/utils.test.ts (5)

Tests  8 passed (8)
";
        let r = filter_jest(output).unwrap();
        assert!(r.contains("passed"), "got: {r}");
        assert!(!r.contains("failed"), "got: {r}");
    }

    #[test]
    fn vitest_failure() {
        let output = "\
 ✓ src/utils.test.ts (5)
 × src/math.test.ts (1)
   × add returns 4

     AssertionError: expected 4 to be 5

Tests  1 failed | 5 passed (6)
Duration  42ms
";
        let r = filter_jest(output).unwrap();
        assert!(r.contains("failure") || r.contains("failed"), "got: {r}");
        assert!(r.contains("AssertionError"), "got: {r}");
    }
}
