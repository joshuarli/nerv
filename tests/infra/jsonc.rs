use std::io::Write;

use nerv::core::config::read_jsonc;
use tempfile::NamedTempFile;

fn write_temp(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

#[test]
fn plain_json_parses() {
    let f = write_temp(r#"{"key": "value"}"#);
    let v: serde_json::Value = read_jsonc(f.path()).unwrap();
    assert_eq!(v["key"], "value");
}

#[test]
fn line_comments_stripped() {
    let f = write_temp(
        r#"{
  // this is a comment
  "key": "value" // inline comment
}"#,
    );
    let v: serde_json::Value = read_jsonc(f.path()).unwrap();
    assert_eq!(v["key"], "value");
}

#[test]
fn comments_inside_strings_preserved() {
    let f = write_temp(r#"{"url": "http://example.com"}"#);
    let v: serde_json::Value = read_jsonc(f.path()).unwrap();
    assert_eq!(v["url"], "http://example.com");
}

#[test]
fn array_with_comments() {
    let f = write_temp(
        r#"[
  "one",   // first
  "two",   // second
  "three"  // third
]"#,
    );
    let v: Vec<String> = read_jsonc(f.path()).unwrap();
    assert_eq!(v, vec!["one", "two", "three"]);
}

#[test]
fn missing_file_returns_none() {
    let result: Option<serde_json::Value> = read_jsonc(std::path::Path::new("/nonexistent"));
    assert!(result.is_none());
}

#[test]
fn invalid_json_returns_none() {
    let f = write_temp("not json at all");
    let result: Option<serde_json::Value> = read_jsonc(f.path());
    assert!(result.is_none());
}
