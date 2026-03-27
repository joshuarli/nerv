//! Allocation tracking for tool operations.
//! Uses a counting global allocator to measure alloc count and bytes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;

use nerv::agent::agent::{AgentTool, UpdateCallback};
use nerv::tools::*;

std::thread_local! {
    static TL_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static TL_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static TL_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            TL_ACTIVE.with(|a| {
                if a.get() {
                    TL_COUNT.with(|c| c.set(c.get() + 1));
                    TL_BYTES.with(|b| b.set(b.get() + layout.size() as u64));
                }
            });
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

struct AllocStats {
    count: u64,
    bytes: u64,
}

fn measure_allocs<F: FnOnce() -> R, R>(f: F) -> (R, AllocStats) {
    TL_COUNT.with(|c| c.set(0));
    TL_BYTES.with(|b| b.set(0));
    TL_ACTIVE.with(|a| a.set(true));
    let result = f();
    TL_ACTIVE.with(|a| a.set(false));
    let stats = AllocStats {
        count: TL_COUNT.with(|c| c.get()),
        bytes: TL_BYTES.with(|b| b.get()),
    };
    (result, stats)
}

fn noop_update() -> UpdateCallback {
    Arc::new(|_| {})
}

#[test]
fn read_100_lines_allocs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lines: Vec<String> = (1..=100).map(|i| format!("line {}", i)).collect();
    std::fs::write(tmp.path().join("test.txt"), lines.join("\n")).unwrap();

    let tool = ReadTool::new(tmp.path().to_path_buf());
    let input = serde_json::json!({"path": "test.txt"});
    let update = noop_update();

    // Warm up (first call may trigger lazy init)
    let _ = tool.execute(input.clone(), update.clone());

    let (result, stats) = measure_allocs(|| tool.execute(input.clone(), update.clone()));
    assert!(!result.is_error);
    eprintln!(
        "read 100 lines: {} allocs, {} bytes",
        stats.count, stats.bytes
    );
    assert!(
        stats.count < 500,
        "read 100 lines: too many allocs ({})",
        stats.count
    );
    assert!(
        stats.bytes < 100_000,
        "read 100 lines: too many bytes ({})",
        stats.bytes
    );
}

#[test]
fn edit_single_500_lines_allocs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lines: Vec<String> = (1..=500).map(|i| format!("fn func_{}() {{}}", i)).collect();
    let original = lines.join("\n");

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let update = noop_update();

    // Write + warm up
    std::fs::write(tmp.path().join("code.rs"), &original).unwrap();
    let input = serde_json::json!({
        "path": "code.rs",
        "old_text": "fn func_250() {}",
        "new_text": "fn func_250_renamed() {}"
    });
    let _ = tool.execute(input.clone(), update.clone());

    // Measure
    std::fs::write(tmp.path().join("code.rs"), &original).unwrap();
    let (result, stats) = measure_allocs(|| tool.execute(input.clone(), update.clone()));
    assert!(!result.is_error, "{}", result.content);
    eprintln!(
        "edit single 500 lines: {} allocs, {} bytes",
        stats.count, stats.bytes
    );
    assert!(
        stats.count < 300,
        "edit single: too many allocs ({})",
        stats.count
    );
    assert!(
        stats.bytes < 500_000,
        "edit single: too many bytes ({})",
        stats.bytes
    );
}

#[test]
fn edit_multi_5x_500_lines_allocs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lines: Vec<String> = (1..=500).map(|i| format!("fn func_{}() {{}}", i)).collect();
    let original = lines.join("\n");

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let update = noop_update();

    let input = serde_json::json!({
        "path": "code.rs",
        "edits": [
            {"old_text": "fn func_50() {}", "new_text": "fn a() {}"},
            {"old_text": "fn func_150() {}", "new_text": "fn b() {}"},
            {"old_text": "fn func_250() {}", "new_text": "fn c() {}"},
            {"old_text": "fn func_350() {}", "new_text": "fn d() {}"},
            {"old_text": "fn func_450() {}", "new_text": "fn e() {}"},
        ]
    });

    // Warm up
    std::fs::write(tmp.path().join("code.rs"), &original).unwrap();
    let _ = tool.execute(input.clone(), update.clone());

    // Measure
    std::fs::write(tmp.path().join("code.rs"), &original).unwrap();
    let (result, stats) = measure_allocs(|| tool.execute(input.clone(), update.clone()));
    assert!(!result.is_error, "{}", result.content);
    eprintln!(
        "edit multi 5x 500 lines: {} allocs, {} bytes",
        stats.count, stats.bytes
    );
    assert!(
        stats.count < 500,
        "edit multi: too many allocs ({})",
        stats.count
    );
    assert!(
        stats.bytes < 1_000_000,
        "edit multi: too many bytes ({})",
        stats.bytes
    );
}

#[test]
fn diff_2000_lines_allocs() {
    let old: String = (1..=2000)
        .map(|i| format!("line {}\n", i))
        .collect();
    let new = old
        .replace("line 100\n", "CHANGED\n")
        .replace("line 1500\n", "CHANGED\n");

    // Warm up
    let _ = nerv::tools::diff::unified_diff(&old, &new, "a", "b");

    let (_, stats) = measure_allocs(|| {
        nerv::tools::diff::unified_diff(&old, &new, "a", "b")
    });
    eprintln!(
        "diff 2000 lines: {} allocs, {} bytes",
        stats.count, stats.bytes
    );
    assert!(
        stats.count < 200,
        "diff: too many allocs ({})",
        stats.count
    );
    assert!(
        stats.bytes < 2_000_000,
        "diff: too many bytes ({})",
        stats.bytes
    );
}

#[test]
fn write_10kb_allocs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let content = "x".repeat(10_000);
    let tool = WriteTool::new(tmp.path().to_path_buf());
    let update = noop_update();

    let input = serde_json::json!({"path": "out.txt", "content": &content});

    // Warm up
    let _ = tool.execute(input.clone(), update.clone());

    let (result, stats) = measure_allocs(|| tool.execute(input.clone(), update.clone()));
    assert!(!result.is_error);
    eprintln!(
        "write 10kb: {} allocs, {} bytes",
        stats.count, stats.bytes
    );
    assert!(
        stats.count < 100,
        "write: too many allocs ({})",
        stats.count
    );
}

#[test]
fn edit_lf_vs_crlf_overhead() {
    // Compare allocation bytes for LF vs CRLF files of the same logical content.
    // LF should allocate less since normalize_crlf and restore_line_endings
    // are no-ops (Cow::Borrowed).
    let tmp = tempfile::TempDir::new().unwrap();
    let lines: Vec<String> = (1..=200).map(|i| format!("line_{}", i)).collect();
    let lf_content = lines.join("\n") + "\n";
    let crlf_content = lines.join("\r\n") + "\r\n";

    let mq = Arc::new(FileMutationQueue::new());
    let tool = EditTool::new(tmp.path().to_path_buf(), mq);
    let update = noop_update();

    let input = serde_json::json!({
        "path": "test.txt",
        "old_text": "line_100",
        "new_text": "CHANGED_100"
    });

    // Measure LF
    std::fs::write(tmp.path().join("test.txt"), &lf_content).unwrap();
    let _ = tool.execute(input.clone(), update.clone()); // warm up
    std::fs::write(tmp.path().join("test.txt"), &lf_content).unwrap();
    let (r1, lf_stats) = measure_allocs(|| tool.execute(input.clone(), update.clone()));
    assert!(!r1.is_error, "{}", r1.content);

    // Measure CRLF
    std::fs::write(tmp.path().join("test.txt"), &crlf_content).unwrap();
    let _ = tool.execute(input.clone(), update.clone()); // warm up
    std::fs::write(tmp.path().join("test.txt"), &crlf_content).unwrap();
    let (r2, crlf_stats) = measure_allocs(|| tool.execute(input.clone(), update.clone()));
    assert!(!r2.is_error, "{}", r2.content);

    eprintln!(
        "LF:   {} allocs, {} bytes\nCRLF: {} allocs, {} bytes",
        lf_stats.count, lf_stats.bytes, crlf_stats.count, crlf_stats.bytes,
    );
    // LF should allocate fewer bytes (no CRLF→LF conversion, no LF→CRLF restore)
    assert!(
        lf_stats.bytes < crlf_stats.bytes,
        "LF ({} bytes) should allocate less than CRLF ({} bytes)",
        lf_stats.bytes, crlf_stats.bytes,
    );
}
