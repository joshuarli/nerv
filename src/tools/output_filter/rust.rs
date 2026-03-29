//! Output filters for Rust / Cargo commands.
//!
//! Handles:
//! - `cargo build` / `cargo check` / `cargo clippy` — success → one line;
//!   failure → errors only
//! - `cargo test` — all pass → one line; partial/full failure → failing tests +
//!   error body

/// Try to compress the output of a cargo build/check/clippy invocation.
///
/// Returns Some(summary) if the output is recognisably a cargo build run.
pub fn filter_cargo_build(text: &str) -> Option<String> {
    // Must look like cargo build/check output
    let is_cargo = text.contains("Compiling ")
        || text.contains("Checking ")
        || text.contains("Finished ")
        || text.contains("error[E")
        || text.contains("warning[");
    if !is_cargo {
        return None;
    }

    // Success: find the Finished line
    if !text.contains("error[") && !text.contains("error:") {
        if let Some(line) = text.lines().rfind(|l| l.contains("Finished")) {
            return Some(line.trim().to_string());
        }
        // clippy with only warnings, no errors
        let warn_count = text
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                t.starts_with("warning[") || t.starts_with("warning:")
            })
            .count();
        if warn_count > 0 {
            return Some(format!("{} warning(s), no errors", warn_count));
        }
        return None;
    }

    // Failure: extract error blocks.
    let errors = extract_cargo_errors(text);
    if errors.is_empty() {
        return None;
    }
    let error_count = errors.len();
    let body = errors.join("\n\n");
    Some(format!("{} error(s):\n{}", error_count, body))
}

/// Try to compress the output of `cargo test`.
/// Returns Some(summary) if recognisably a cargo test run.
pub fn filter_cargo_test(text: &str) -> Option<String> {
    // Must have a "test result:" summary line
    let summary_line = text.lines().rfind(|l| l.starts_with("test result:"))?;

    // All passing
    if summary_line.contains("ok") && summary_line.contains("0 failed") {
        return Some(summary_line.trim().to_string());
    }

    // Partial/full failure — show failing test names + their panic output
    let failures = extract_rust_test_failures(text);
    if failures.is_empty() {
        // Fallback: just return the summary line
        return Some(summary_line.trim().to_string());
    }
    let count = failures.len();
    let body = failures.join("\n\n");
    Some(format!("{} test failure(s):\n{}\n{}", count, body, summary_line.trim()))
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Extract individual `error[Exxxx]: ...` blocks from cargo output.
/// Each block includes the message header and the file:line pointer lines.
fn extract_cargo_errors(text: &str) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut in_error = false;

    for line in text.lines() {
        let trimmed = line.trim_start();
        let is_error_start = (trimmed.starts_with("error[") || trimmed.starts_with("error:"))
            && !trimmed.starts_with("error: aborting")
            && !trimmed.starts_with("error: could not compile");

        if is_error_start {
            if in_error && !current.is_empty() {
                errors.push(current.join("\n"));
                current.clear();
            }
            in_error = true;
            current.push(line);
        } else if in_error {
            // Keep location lines (-->, |, = note:, = help:) and indented code lines
            if trimmed.starts_with("-->")
                || trimmed.starts_with('|')
                || trimmed.starts_with("= note:")
                || trimmed.starts_with("= help:")
                || trimmed.starts_with("= ")
                || trimmed.starts_with("...")
                || (line.starts_with("  ") && !line.trim().is_empty())
            {
                current.push(line);
            } else if line.trim().is_empty() {
                // blank line ends the error block
                if !current.is_empty() {
                    errors.push(current.join("\n"));
                    current.clear();
                }
                in_error = false;
            }
        }
    }
    if in_error && !current.is_empty() {
        errors.push(current.join("\n"));
    }
    errors
}

/// Extract individual failing test names and their panic/assertion output.
///
/// Cargo test output for failures looks like:
/// ```text
/// failures:
///
///     ---- test_name stdout ----
///     thread 'test_name' panicked at 'assertion failed: ...'
///     note: run with `RUST_BACKTRACE=1`...
///
/// failures:
///     test_name
/// ```
fn extract_rust_test_failures(text: &str) -> Vec<String> {
    let mut failures: Vec<String> = Vec::new();
    let mut in_failures_section = false;
    let mut in_test_block = false;
    let mut current_name: Option<&str> = None;
    let mut current_body: Vec<&str> = Vec::new();

    for line in text.lines() {
        if line.trim() == "failures:" {
            if in_test_block {
                if let Some(name) = current_name {
                    failures.push(format_test_failure(name, &current_body));
                }
                current_body.clear();
                current_name = None;
                in_test_block = false;
            }
            in_failures_section = !in_failures_section;
            continue;
        }

        if in_failures_section {
            // "---- test_name stdout ----"
            if line.starts_with("---- ") && line.ends_with(" ----") {
                if in_test_block {
                    if let Some(name) = current_name {
                        failures.push(format_test_failure(name, &current_body));
                    }
                    current_body.clear();
                }
                let name = line
                    .trim_start_matches('-')
                    .trim_end_matches('-')
                    .trim()
                    .trim_end_matches(" stdout")
                    .trim();
                current_name = Some(name);
                in_test_block = true;
            } else if in_test_block {
                // Skip noisy backtrace lines
                if line.contains("note: run with `RUST_BACKTRACE")
                    || line.trim().starts_with("stack backtrace:")
                {
                    continue;
                }
                current_body.push(line);
            }
        }
    }

    if in_test_block
        && let Some(name) = current_name
    {
        failures.push(format_test_failure(name, &current_body));
    }

    failures
}

fn format_test_failure(name: &str, body: &[&str]) -> String {
    // Trim trailing blank lines from body
    let mut end = body.len();
    while end > 0 && body[end - 1].trim().is_empty() {
        end -= 1;
    }
    let body_trimmed = &body[..end];
    if body_trimmed.is_empty() {
        return format!("FAILED: {}", name);
    }
    format!("FAILED: {}\n{}", name, body_trimmed.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── cargo build ──────────────────────────────────────────────────────────

    #[test]
    fn cargo_build_success() {
        let output = "   Compiling nerv v0.1.0\n    Finished `dev` profile [unoptimized] target(s) in 3.2s\n";
        let r = filter_cargo_build(output);
        assert!(r.is_some());
        assert!(r.unwrap().contains("Finished"));
    }

    #[test]
    fn cargo_check_success() {
        let output = "    Checking nerv v0.1.0 (path)\n    Finished `dev` profile in 0.8s\n";
        let r = filter_cargo_build(output).unwrap();
        assert!(r.contains("Finished"), "got: {r}");
    }

    #[test]
    fn cargo_clippy_warnings_only() {
        let output = "\
    Checking nerv v0.1.0
warning: unused variable `x`
  --> src/main.rs:5:9
   |
5  |     let x = 1;
   |         ^ warning: unused variable
warning: 1 warning emitted
";
        let r = filter_cargo_build(output).unwrap();
        assert!(r.contains("warning(s)"), "got: {r}");
        assert!(!r.contains("let x"), "should not include source: {r}");
    }

    #[test]
    fn cargo_build_single_error() {
        let output = "\
   Compiling nerv v0.1.0
error[E0308]: mismatched types
  --> src/main.rs:10:5
   |
10 |     foo(42u32);
   |         ^^^^^ expected `i32`, found `u32`

error: aborting due to 1 previous error
";
        let r = filter_cargo_build(output).unwrap();
        assert!(r.contains("error[E0308]"), "got: {r}");
        assert!(r.contains("src/main.rs:10"), "got: {r}");
    }

    #[test]
    fn cargo_build_multiple_errors() {
        let output = "\
   Compiling nerv v0.1.0
error[E0308]: mismatched types
  --> src/main.rs:5:5
   |
5  |     let _: i32 = \"hello\";
   |                  ^^^^^^^ expected `i32`, found `&str`

error[E0425]: cannot find value `undefined_var` in this scope
  --> src/main.rs:8:9
   |
8  |     let _ = undefined_var;
   |             ^^^^^^^^^^^^^ not found in this scope

error: aborting due to 2 previous errors
";
        let r = filter_cargo_build(output).unwrap();
        assert!(r.starts_with("2 error(s)"), "got: {r}");
        assert!(r.contains("E0308"), "got: {r}");
        assert!(r.contains("E0425"), "got: {r}");
    }

    #[test]
    fn non_cargo_output_returns_none() {
        assert!(filter_cargo_build("some random make output").is_none());
    }

    // ── cargo test ───────────────────────────────────────────────────────────

    #[test]
    fn cargo_test_all_pass() {
        let output = "\
running 25 tests
.........................
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
";
        let r = filter_cargo_test(output).unwrap();
        assert_eq!(
            r,
            "test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s"
        );
    }

    #[test]
    fn cargo_test_single_failure() {
        let output = "\
running 3 tests
..F
failures:

---- test_addition stdout ----
thread 'test_addition' panicked at 'assertion failed: 2 + 2 == 5'
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    test_addition

test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
";
        let r = filter_cargo_test(output).unwrap();
        assert!(r.contains("FAILED: test_addition"), "got: {r}");
        assert!(r.contains("assertion failed"), "got: {r}");
        assert!(!r.contains("RUST_BACKTRACE"), "backtrace hint should be stripped: {r}");
    }

    #[test]
    fn cargo_test_multiple_failures() {
        let output = "\
running 3 tests
FFF
failures:

---- test_a stdout ----
thread 'test_a' panicked at 'a failed'

---- test_b stdout ----
thread 'test_b' panicked at 'b failed'

failures:
    test_a
    test_b

test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
";
        let r = filter_cargo_test(output).unwrap();
        assert!(r.contains("FAILED: test_a"), "got: {r}");
        assert!(r.contains("FAILED: test_b"), "got: {r}");
        assert!(r.starts_with("2 test failure(s)"), "got: {r}");
    }

    #[test]
    fn cargo_test_no_test_result_line_returns_none() {
        // Not a cargo test run — filter_cargo_test must return None
        assert!(filter_cargo_test("just some output\nno summary here\n").is_none());
    }
}
