use nerv::tui::highlight::*;

#[test]
fn rust_keywords_highlighted() {
    let mut state = HlState::Normal;
    let result = highlight_line("fn main() {", &mut state, rules_for_lang("rust").unwrap());
    assert!(result.contains("\x1b[33m")); // yellow for keyword "fn"
    assert!(result.contains("main"));
}

#[test]
fn rust_strings_highlighted() {
    let mut state = HlState::Normal;
    let result = highlight_line(
        r#"let s = "hello";"#,
        &mut state,
        rules_for_lang("rust").unwrap(),
    );
    assert!(result.contains("\x1b[32m")); // green for string
}

#[test]
fn rust_comments_highlighted() {
    let mut state = HlState::Normal;
    let result = highlight_line(
        "// this is a comment",
        &mut state,
        rules_for_lang("rust").unwrap(),
    );
    assert!(result.contains("\x1b[90m")); // grey for comment
}

#[test]
fn rust_numbers_highlighted() {
    let mut state = HlState::Normal;
    let result = highlight_line("let x = 42;", &mut state, rules_for_lang("rust").unwrap());
    assert!(result.contains("\x1b[31m")); // red for number
}

#[test]
fn rust_macros_highlighted() {
    let mut state = HlState::Normal;
    let result = highlight_line(
        "println!(\"hello\");",
        &mut state,
        rules_for_lang("rust").unwrap(),
    );
    assert!(result.contains("\x1b[35;1m")); // bold magenta for macro
}

#[test]
fn python_keywords() {
    let mut state = HlState::Normal;
    let result = highlight_line("def foo():", &mut state, rules_for_lang("python").unwrap());
    assert!(result.contains("\x1b[33m")); // yellow for "def"
}

#[test]
fn python_triple_string_multiline() {
    let rules = rules_for_lang("python").unwrap();
    let mut state = HlState::Normal;
    let line1 = highlight_line(r#"s = """hello"#, &mut state, rules);
    assert!(matches!(state, HlState::MultiLineString(_)));
    let line2 = highlight_line(r#"world""""#, &mut state, rules);
    assert_eq!(state, HlState::Normal);
    // Both lines should have string coloring
    assert!(line1.contains("\x1b[32m"));
    assert!(line2.contains("\x1b[32m"));
}

#[test]
fn block_comment_multiline() {
    let rules = rules_for_lang("rust").unwrap();
    let mut state = HlState::Normal;
    let _line1 = highlight_line("/* start of", &mut state, rules);
    assert_eq!(state, HlState::BlockComment);
    let _line2 = highlight_line("end of comment */", &mut state, rules);
    assert_eq!(state, HlState::Normal);
}

#[test]
fn rules_for_lang_aliases() {
    assert!(rules_for_lang("rs").is_some());
    assert!(rules_for_lang("py").is_some());
    assert!(rules_for_lang("js").is_some());
    assert!(rules_for_lang("ts").is_some());
    assert!(rules_for_lang("sh").is_some());
    assert!(rules_for_lang("golang").is_some());
    assert!(rules_for_lang("c++").is_some());
    assert!(rules_for_lang("unknown_lang").is_none());
}

#[test]
fn plain_text_unchanged() {
    let mut state = HlState::Normal;
    let result = highlight_line("hello world", &mut state, rules_for_lang("rust").unwrap());
    // No ANSI codes for plain identifiers
    assert!(!result.contains("\x1b[33m")); // not a keyword
}

#[test]
fn function_calls_highlighted() {
    let mut state = HlState::Normal;
    let result = highlight_line("foo(bar)", &mut state, rules_for_lang("rust").unwrap());
    assert!(result.contains("\x1b[34m")); // blue for function
}
