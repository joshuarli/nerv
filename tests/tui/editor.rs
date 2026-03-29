use nerv::tui::components::editor::Editor;

#[test]
fn empty_editor() {
    let editor = Editor::new();
    assert!(editor.is_empty());
    assert_eq!(editor.text(), "");
}

#[test]
fn set_and_take_text() {
    let mut editor = Editor::new();
    editor.set_text("hello world");
    assert_eq!(editor.text(), "hello world");
    assert!(!editor.is_empty());

    let taken = editor.take_text();
    assert_eq!(taken, "hello world");
    assert!(editor.is_empty());
}

#[test]
fn multiline_text() {
    let mut editor = Editor::new();
    editor.set_text("line1\nline2\nline3");
    assert_eq!(editor.text(), "line1\nline2\nline3");
}

#[test]
fn paste_small_inserted_directly() {
    let mut editor = Editor::new();
    editor.insert_paste("hello\nworld");
    assert_eq!(editor.text(), "hello\nworld");
}

#[test]
fn paste_large_creates_marker() {
    let mut editor = Editor::new();
    let large = "line\n".repeat(20);
    editor.insert_paste(&large);
    let text = editor.text();
    assert!(text.contains("[paste #1"), "expected paste marker, got: {}", text);
    assert!(!text.contains("line\nline"), "raw content should not be in buffer");
}

#[test]
fn paste_marker_expanded_on_take() {
    let mut editor = Editor::new();
    let large = "line\n".repeat(20);
    editor.insert_paste(&large);

    let taken = editor.take_text();
    assert!(taken.contains("line\nline"), "content should be expanded");
    assert!(!taken.contains("[paste #"), "marker should be gone");
}

#[test]
fn multiple_pastes_get_distinct_ids() {
    let mut editor = Editor::new();
    editor.insert_paste(&"a\n".repeat(15));
    editor.insert_paste(&"b\n".repeat(15));
    let text = editor.text();
    assert!(text.contains("[paste #1"), "first paste marker");
    assert!(text.contains("[paste #2"), "second paste marker");
}

#[test]
fn take_clears_pastes() {
    let mut editor = Editor::new();
    editor.insert_paste(&"x\n".repeat(15));
    let _ = editor.take_text();
    // After take, paste store is cleared
    editor.insert_paste(&"y\n".repeat(15));
    let text = editor.text();
    assert!(text.contains("[paste #1"), "paste counter should reset after take");
}

#[test]
fn clear_resets_state() {
    let mut editor = Editor::new();
    editor.set_text("some text");
    editor.clear();
    assert!(editor.is_empty());
    assert_eq!(editor.text(), "");
}
