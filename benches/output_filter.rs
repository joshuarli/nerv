/// Benchmarks for the output_filter pipeline.
///
/// Run with:
///   cargo bench --bench output_filter
///
/// The benchmarks cover:
///   - `strip_ansi`    — fast path (clean input, Borrowed) vs slow path (ANSI
///     codes)
///   - `dedup_lines`   — fast path (no dedup needed, Borrowed) vs slow path
///     (run present)
///   - `filter_bash_output` — end-to-end pipeline for each language filter
///
/// The corpus strings are representative of real-world outputs: sizes and
/// content patterns match what agents typically produce.
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use nerv::tools::output_filter::{ansi, dedup, filter_bash_output};

// ── corpus helpers
// ────────────────────────────────────────────────────────────

fn cargo_build_success() -> &'static str {
    "   Compiling serde v1.0.0\n   Compiling serde_derive v1.0.0\n   Compiling nerv v0.1.6\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.21s\n"
}

fn cargo_build_with_ansi() -> String {
    "\x1b[1m\x1b[32m   Compiling\x1b[0m serde v1.0.0\n\
     \x1b[1m\x1b[32m   Compiling\x1b[0m nerv v0.1.6\n\
     \x1b[1m\x1b[32m    Finished\x1b[0m `dev` profile in 2.1s\n"
        .to_string()
}

fn cargo_build_error() -> &'static str {
    "\
   Compiling nerv v0.1.6
error[E0308]: mismatched types
  --> src/main.rs:10:5
   |
10 |     foo(42u32);
   |         ^^^^^ expected `i32`, found `u32`
   |
   = note: expected type `i32`
              found type `u32`

error[E0425]: cannot find value `bar`
  --> src/main.rs:15:9
   |
15 |     let x = bar;
   |             ^^^ not found in this scope

error: aborting due to 2 previous errors
"
}

fn cargo_test_pass() -> &'static str {
    "running 244 tests\n................................................................................................................................................................................................................................................................................................................\ntest result: ok. 244 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.38s\n"
}

fn cargo_test_failure() -> &'static str {
    "\
running 3 tests
..F
failures:

---- test_addition stdout ----
thread 'test_addition' panicked at 'assertion failed: 2 + 2 == 5'
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- test_subtraction stdout ----
thread 'test_subtraction' panicked at 'assertion failed: 5 - 3 == 1'

failures:
    test_addition
    test_subtraction

test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
"
}

fn pytest_pass() -> &'static str {
    "\
============================= test session starts ==============================
platform linux -- Python 3.11.0, pytest-7.4.0, pluggy-1.3.0
rootdir: /home/user/project
collected 42 items

test_math.py ....................                                        [ 47%]
test_utils.py ......................                                     [100%]

============================== 42 passed in 0.23s ==============================
"
}

fn pytest_failure() -> &'static str {
    "\
============================= test session starts ==============================
collected 3 items

test_foo.py .FF                                                         [100%]

=================================== FAILURES ===================================
_________________________ test_bar _________________________

    def test_bar():
>       assert 1 == 2
E       AssertionError: assert 1 == 2

test_foo.py:5: AssertionError
_________________________ test_baz _________________________

    def test_baz():
>       assert 'a' == 'b'
E       AssertionError: assert 'a' == 'b'

test_foo.py:9: AssertionError
========================= 2 failed, 1 passed in 0.04s =========================
"
}

fn go_test_pass() -> &'static str {
    "\
=== RUN   TestFoo
--- PASS: TestFoo (0.00s)
=== RUN   TestBar
--- PASS: TestBar (0.00s)
=== RUN   TestBaz
--- PASS: TestBaz (0.00s)
ok  \texample.com/mymod\t0.001s
"
}

fn go_test_failure() -> &'static str {
    "\
=== RUN   TestFoo
--- PASS: TestFoo (0.00s)
=== RUN   TestBar
    bar_test.go:10: expected 2, got 3
    bar_test.go:11: additional context: x was nil
--- FAIL: TestBar (0.00s)
FAIL\texample.com/mymod\t0.001s
"
}

fn jest_pass() -> &'static str {
    "\
PASS src/math.test.js
PASS src/utils.test.js
PASS src/api.test.js

Test Suites: 3 passed, 3 total
Tests:       18 passed, 18 total
Snapshots:   0 total
Time:        2.341 s
Ran all test suites.
"
}

fn jest_failure() -> &'static str {
    "\
FAIL src/math.test.js
  ● add › returns correct sum

    expect(received).toBe(expected)

    Expected: 5
    Received: 4

      3 | test('returns correct sum', () => {
    > 4 |   expect(add(2, 2)).toBe(5);
        |                     ^
      5 | });

    at Object.<anonymous> (src/math.test.js:4:21)
    at node_modules/jest-circus/build/run.js:120:12

Test Suites: 1 failed, 1 total
Tests:       1 failed, 1 total
Time:        1.234 s
"
}

fn large_json() -> String {
    let users: Vec<serde_json::Value> = (0..200)
        .map(|i| {
            serde_json::json!({
                "id": i,
                "name": format!("User Number {}", i),
                "email": format!("user{}@example.com", i),
                "active": i % 2 == 0,
                "roles": ["user", "editor"],
                "meta": {"created": "2024-01-01", "score": i * 10}
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({ "users": users, "total": 200 })).unwrap()
}

fn plain_output() -> &'static str {
    "Hello from my script\nProcessed 42 items\nDone.\n"
}

fn repeated_lines() -> String {
    let mut s = String::new();
    for _ in 0..50 {
        s.push_str("error: something went wrong\n");
    }
    s.push_str("build failed\n");
    s
}

// ── benchmarks ───────────────────────────────────────────────────────────────

fn bench_ansi(c: &mut Criterion) {
    let mut g = c.benchmark_group("ansi");

    // Fast path: no ANSI codes — should return Borrowed, zero allocation
    g.bench_function("strip_clean_input", |b| {
        let text = cargo_build_success();
        b.iter(|| ansi::strip_ansi(black_box(text)));
    });

    // Slow path: ANSI codes present
    g.bench_function("strip_ansi_codes", |b| {
        let text = cargo_build_with_ansi();
        b.iter(|| ansi::strip_ansi(black_box(&text)));
    });

    g.finish();
}

fn bench_dedup(c: &mut Criterion) {
    let mut g = c.benchmark_group("dedup");

    // Fast path: no dedup needed
    g.bench_function("no_run_borrowed", |b| {
        let text = cargo_build_success();
        b.iter(|| dedup::dedup_lines(black_box(text)));
    });

    // Slow path: 50 repeated lines
    g.bench_function("large_run", |b| {
        let text = repeated_lines();
        b.iter(|| dedup::dedup_lines(black_box(&text)));
    });

    g.finish();
}

fn bench_pipeline(c: &mut Criterion) {
    let mut g = c.benchmark_group("filter_bash_output");

    g.bench_function("plain_passthrough", |b| {
        let text = plain_output();
        b.iter(|| filter_bash_output(black_box("./myscript.sh"), black_box(text)));
    });

    g.bench_function("cargo_build_success", |b| {
        let text = cargo_build_success();
        b.iter(|| filter_bash_output(black_box("cargo build"), black_box(text)));
    });

    g.bench_function("cargo_build_with_ansi", |b| {
        let text = cargo_build_with_ansi();
        b.iter(|| filter_bash_output(black_box("cargo build"), black_box(&text)));
    });

    g.bench_function("cargo_build_error", |b| {
        let text = cargo_build_error();
        b.iter(|| filter_bash_output(black_box("cargo build"), black_box(text)));
    });

    g.bench_function("cargo_test_pass", |b| {
        let text = cargo_test_pass();
        b.iter(|| filter_bash_output(black_box("cargo test"), black_box(text)));
    });

    g.bench_function("cargo_test_failure", |b| {
        let text = cargo_test_failure();
        b.iter(|| filter_bash_output(black_box("cargo test"), black_box(text)));
    });

    g.bench_function("pytest_pass", |b| {
        let text = pytest_pass();
        b.iter(|| filter_bash_output(black_box("pytest"), black_box(text)));
    });

    g.bench_function("pytest_failure", |b| {
        let text = pytest_failure();
        b.iter(|| filter_bash_output(black_box("pytest"), black_box(text)));
    });

    g.bench_function("go_test_pass", |b| {
        let text = go_test_pass();
        b.iter(|| filter_bash_output(black_box("go test ./..."), black_box(text)));
    });

    g.bench_function("go_test_failure", |b| {
        let text = go_test_failure();
        b.iter(|| filter_bash_output(black_box("go test ./..."), black_box(text)));
    });

    g.bench_function("jest_pass", |b| {
        let text = jest_pass();
        b.iter(|| filter_bash_output(black_box("npx jest"), black_box(text)));
    });

    g.bench_function("jest_failure", |b| {
        let text = jest_failure();
        b.iter(|| filter_bash_output(black_box("npx jest"), black_box(text)));
    });

    g.bench_function("large_json_schema", |b| {
        let text = large_json();
        b.iter(|| filter_bash_output(black_box("cat data.json"), black_box(&text)));
    });

    g.finish();
}

criterion_group!(benches, bench_ansi, bench_dedup, bench_pipeline);
criterion_main!(benches);
