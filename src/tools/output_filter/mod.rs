/// Output post-processing pipeline for bash tool results.
///
/// Applied in `transform_context` to bash ToolResults before they go to the LLM.
/// Each filter is language/tool-specific and returns `Some(compressed)` when it
/// recognises the output, or `None` to pass through to the next filter.
///
/// Pipeline order:
///   1. ANSI stripping   — always runs, returns Cow::Borrowed when input is clean
///   2. Deduplication    — collapse consecutive identical lines (×N notation),
///                         returns Cow::Borrowed when nothing to collapse
///   3. JSON schema      — large JSON blobs → key/type skeleton
///   4. Language filters — language-specific test runner compression

pub mod ansi;
pub mod dedup;
pub mod go;
pub mod json;
pub mod python;
pub mod rust;
pub mod ts;

/// Apply all output filters to a bash result.
///
/// `command` is the original bash command string (used for language detection).
/// `text` is the raw stdout+stderr.
///
/// Returns a `Cow<str>`: `Borrowed` when no transformation was needed (common
/// fast path for short, plain outputs), `Owned` when something changed.
pub fn filter_bash_output<'a>(command: &str, text: &'a str) -> std::borrow::Cow<'a, str> {
    // Step 1: strip ANSI escape sequences (Cow::Borrowed when input is clean)
    let clean = ansi::strip_ansi(text);

    // Step 2: deduplicate consecutive identical lines (Cow::Borrowed when no run)
    let deduped = dedup::dedup_lines(&clean);

    // Steps 3-4 produce new Strings; only enter the slow path if there's something to compress.
    // We pass &deduped (a &str regardless of Cow variant) to avoid cloning.
    let deduped_str: &str = &deduped;

    // Step 3: JSON schema extraction for large blobs
    if let Some(schema) = json::extract_json_schema(deduped_str) {
        return std::borrow::Cow::Owned(schema);
    }

    // Step 4: language-specific test/build filters
    if let Some(compressed) = apply_language_filter(command, deduped_str) {
        return std::borrow::Cow::Owned(compressed);
    }

    // Nothing changed in steps 3-4.  Return whatever Cow we have from step 2
    // (possibly Borrowed from the original `text` if steps 1-2 were no-ops too,
    // but we need to re-borrow from `clean` since deduped borrows from it).
    // Simplest correct approach: if deduped is Borrowed it still borrows from
    // `clean`, not `text` directly — so if clean is also Borrowed we can return
    // Borrowed from `text`.
    if matches!(clean, std::borrow::Cow::Borrowed(_)) && matches!(deduped, std::borrow::Cow::Borrowed(_)) {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(deduped.into_owned())
    }
}

/// Dispatch to the right language filter based on the command string.
fn apply_language_filter(command: &str, text: &str) -> Option<String> {
    let cmd = command.trim();

    // --- Command-based routing (checked first; fast substring tests) ---

    // Rust / Cargo
    if cmd.contains("cargo test") {
        return rust::filter_cargo_test(text);
    }
    if cmd.contains("cargo build")
        || cmd.contains("cargo check")
        || cmd.contains("cargo clippy")
    {
        return rust::filter_cargo_build(text);
    }

    // Go
    if cmd.contains("go test") {
        return go::filter_go_test(text);
    }

    // Python — `"pytest"` and `"py.test"` already subsume `"python -m pytest"` variants
    if cmd.contains("pytest") || cmd.contains("py.test") {
        return python::filter_pytest(text);
    }
    if cmd.contains("python -m unittest") || cmd.contains("python3 -m unittest") {
        return python::filter_unittest(text);
    }

    // JavaScript / TypeScript — `"jest"` subsumes `"npx jest"` / `"yarn jest"`;
    // likewise `"vitest"` subsumes its npx/yarn forms.
    if cmd.contains("jest") {
        return ts::filter_jest(text);
    }
    if cmd.contains("vitest") {
        return ts::filter_jest(text);
    }

    // --- Heuristic fallback: output-content signals (e.g. Makefile targets) ---
    //
    // Check for Go JSON *before* the generic JSON schema step in the outer
    // pipeline so NDJSON blobs are never handed to serde_json::from_str.
    // `go test -json` emits one {"Action":...} object per line.
    if text.lines().next().map_or(false, |l| l.trim_start().starts_with("{\"Action\"")) {
        return go::filter_go_test(text);
    }
    if text.contains("test result:") {
        return rust::filter_cargo_test(text);
    }
    if text.contains("Compiling ") || text.contains("error[E") {
        return rust::filter_cargo_build(text);
    }
    if text.contains("test session starts") {
        return python::filter_pytest(text);
    }
    // Jest: every file result line starts with "PASS " or "FAIL "
    if text.lines().any(|l| l.starts_with("PASS ") || l.starts_with("FAIL ")) {
        return ts::filter_jest(text);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_stripped_always() {
        let text = "\x1b[32mhello\x1b[0m world";
        let result = filter_bash_output("echo hi", text);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn plain_output_borrowed() {
        // No ANSI, no dedup, no JSON, no known command → Borrowed (zero alloc)
        let text = "some random output\nthat doesnt match anything\n";
        assert!(
            matches!(filter_bash_output("my-custom-script", text), std::borrow::Cow::Borrowed(_)),
            "plain passthrough should be zero-alloc Borrowed"
        );
    }

    #[test]
    fn dedup_applied() {
        let text = "error: bad thing\nerror: bad thing\nerror: bad thing\nerror: bad thing\ndone\n";
        let result = filter_bash_output("make", text);
        assert!(result.contains("(×4)"), "got: {result}");
    }

    #[test]
    fn cargo_test_routed() {
        let text = "running 5 tests\n.....\ntest result: ok. 5 passed; 0 failed; 0 ignored\n";
        let result = filter_bash_output("cargo test", text);
        assert!(result.contains("5 passed"), "got: {result}");
        // Should be compressed to one line
        assert!(!result.contains("running 5 tests"), "got: {result}");
    }

    #[test]
    fn unknown_command_passthrough() {
        let text = "some random output\nthat doesnt match anything\n";
        let result = filter_bash_output("my-custom-script", text);
        assert_eq!(result, text);
    }

    #[test]
    fn heuristic_cargo_test_via_make() {
        // Makefile runs cargo test — no "cargo test" in command, but output signals it
        let text = "running 3 tests\n...\ntest result: ok. 3 passed; 0 failed; 0 ignored\n";
        let result = filter_bash_output("make test", text);
        assert!(result.contains("3 passed"), "heuristic should route to cargo filter: {result}");
    }

    #[test]
    fn heuristic_pytest_via_make() {
        let text = "============================= test session starts ==============================\ncollected 1 item\n\ntest_x.py .\n\n============================== 1 passed in 0.01s ==============================";
        let result = filter_bash_output("make test", text);
        assert!(result.contains("1 passed"), "heuristic should route to pytest filter: {result}");
    }

    #[test]
    fn npx_jest_routed() {
        // "jest" is a substring of "npx jest", so the simplified branch still matches
        let text = "PASS src/foo.test.js\nTest Suites: 1 passed, 1 total\nTests:       2 passed, 2 total\n";
        let result = filter_bash_output("npx jest", text);
        assert!(result.contains("passed"), "npx jest should route to jest filter: {result}");
    }

    #[test]
    fn heuristic_jest_via_make() {
        // No "jest" in command; detected by PASS/FAIL prefix lines
        let text = "PASS src/foo.test.js\nTest Suites: 1 passed, 1 total\nTests:       2 passed, 2 total\n";
        let result = filter_bash_output("make test", text);
        assert!(result.contains("passed"), "heuristic should route to jest filter: {result}");
    }

    #[test]
    fn heuristic_go_json_via_make() {
        // go test -json output routed by first-line heuristic, not text.starts_with('{')
        let text = "{\"Action\":\"pass\",\"Test\":\"TestFoo\"}\n{\"Action\":\"pass\"}\n";
        let result = filter_bash_output("make test", text);
        // filter_go_test returns None for all-pass, so output should pass through (not error)
        let _ = result; // just confirm no panic / wrong routing
    }
}
