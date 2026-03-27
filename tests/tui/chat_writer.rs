use nerv::interactive::chat_writer::ChatWriter;
use nerv::tui::tui::Component;

#[test]
fn empty_writer_renders_nothing() {
    let w = ChatWriter::new();
    assert!(w.render(80).is_empty());
}

#[test]
fn push_user_renders_with_reverse() {
    let mut w = ChatWriter::new();
    w.push_user("hello world");
    let lines = w.render(80);
    assert!(!lines.is_empty());
    // User message should contain the text
    let joined = lines.join("");
    assert!(joined.contains("hello world"));
}

#[test]
fn push_styled_renders_with_spacer() {
    let mut w = ChatWriter::new();
    w.push_styled("\x1b[38;5;242m", "status line");
    let lines = w.render(80);
    // Should have the styled line + a spacer (empty line)
    assert!(lines.len() >= 2);
    assert!(lines.last().unwrap().is_empty()); // spacer
}

#[test]
fn push_markdown_renders() {
    let mut w = ChatWriter::new();
    w.push_markdown_source("**bold** text");
    let lines = w.render(80);
    assert!(!lines.is_empty());
    let joined = lines.join("");
    assert!(joined.contains("bold"));
}

#[test]
fn streaming_thinking_shows_last_lines() {
    let mut w = ChatWriter::new();
    w.begin_stream();
    w.append_thinking("line one\nline two\nline three\nline four\nline five");
    let lines = w.render(80);
    // Should show last 3 lines of thinking
    let joined = lines.join("\n");
    assert!(joined.contains("line three"));
    assert!(joined.contains("line four"));
    assert!(joined.contains("line five"));
    // First two should be trimmed
    assert!(!joined.contains("line one"));
}

#[test]
fn streaming_text_renders_markdown() {
    let mut w = ChatWriter::new();
    w.begin_stream();
    w.append_text("hello **world**");
    let lines = w.render(80);
    let joined = lines.join("");
    assert!(joined.contains("world"));
}

#[test]
fn thinking_committed_on_first_text() {
    let mut w = ChatWriter::new();
    w.begin_stream();
    w.append_thinking("deep thought");
    w.append_text("answer");
    let lines = w.render(80);
    let joined = lines.join("\n");
    // Both thinking and text should be present
    assert!(joined.contains("deep thought"));
    assert!(joined.contains("answer"));
}

#[test]
fn finish_stream_commits_to_blocks() {
    let mut w = ChatWriter::new();
    w.begin_stream();
    w.append_text("response text");
    w.finish_stream("response text", None);
    // After finishing, streaming state should be cleared
    assert_eq!(w.streaming_len(), 0);
    // Content should still render
    let lines = w.render(80);
    let joined = lines.join("");
    assert!(joined.contains("response"));
}

#[test]
fn cancel_stream_clears_streaming() {
    let mut w = ChatWriter::new();
    w.begin_stream();
    w.append_text("partial");
    w.cancel_stream();
    assert_eq!(w.streaming_len(), 0);
}

#[test]
fn clear_resets_everything() {
    let mut w = ChatWriter::new();
    w.push_user("msg1");
    w.push_user("msg2");
    assert!(!w.render(80).is_empty());
    w.clear();
    assert!(w.render(80).is_empty());
}

#[test]
fn block_cache_reused_on_same_width() {
    let mut w = ChatWriter::new();
    w.push_markdown_source("# Heading\n\nSome **bold** text.");
    // First render populates cache
    let lines1 = w.render(80);
    // Second render should return identical output (from cache)
    let lines2 = w.render(80);
    assert_eq!(lines1, lines2);
}

#[test]
fn block_cache_invalidated_on_width_change() {
    let mut w = ChatWriter::new();
    w.push_styled("\x1b[38;5;242m", &"word ".repeat(50));
    let lines_wide = w.render(120);
    let lines_narrow = w.render(40);
    // Narrower width should produce more wrapped lines
    assert!(lines_narrow.len() > lines_wide.len());
}

#[test]
fn picker_renders_ephemerally() {
    let mut w = ChatWriter::new();
    w.push_user("permanent");
    w.set_picker(vec!["pick1".into(), "pick2".into()]);
    let lines = w.render(80);
    let joined = lines.join("\n");
    assert!(joined.contains("permanent"));
    assert!(joined.contains("pick1"));
    assert!(joined.contains("pick2"));

    // Clear picker — permanent content remains
    w.clear_picker();
    let lines = w.render(80);
    let joined = lines.join("\n");
    assert!(joined.contains("permanent"));
    assert!(!joined.contains("pick1"));
}

#[test]
fn tool_call_renders() {
    let mut w = ChatWriter::new();
    w.push_tool_call("read", &serde_json::json!({"path": "src/main.rs"}));
    let lines = w.render(80);
    let joined = lines.join("");
    assert!(joined.contains("read"));
    assert!(joined.contains("src/main.rs"));
}

#[test]
fn tool_result_truncates_long_output() {
    let mut w = ChatWriter::new();
    let long = "x\n".repeat(100);
    w.push_tool_result(&long, false);
    let lines = w.render(80);
    // Should cap at 30 lines + overflow indicator
    assert!(lines.iter().any(|l| l.contains("more lines")));
}

#[test]
fn streaming_len_includes_thinking_and_text() {
    let mut w = ChatWriter::new();
    w.begin_stream();
    w.append_thinking("think");
    w.append_text("text");
    assert_eq!(w.streaming_len(), 9); // "think" (5) + "text" (4)
}
