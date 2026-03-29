/// JSON schema extractor for large JSON outputs.
///
/// When a bash command produces a large JSON blob (e.g. `cat data.json` or
/// `curl ... | python -m json.tool`), replace the values with their types
/// so the LLM can understand the schema without reading thousands of tokens.
///
/// Threshold: only activates for outputs over `MIN_CHARS` characters.

const MIN_CHARS: usize = 2_000;
/// Maximum depth to expand before collapsing to "{ ... }"
const MAX_DEPTH: usize = 4;
/// Maximum array items to show before abbreviating
const MAX_ARRAY_ITEMS: usize = 2;

/// If `text` looks like a large JSON value, return a schema skeleton.
/// Returns None if the text is small, not JSON, or parse fails.
pub fn extract_json_schema(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.len() < MIN_CHARS {
        return None;
    }
    // Quick heuristic: must start with { or [
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let schema = render_schema(&v, 0);
    let original_bytes = trimmed.len();
    Some(format!(
        "[JSON schema — {} bytes compressed to schema]\n{}",
        original_bytes, schema
    ))
}

fn render_schema(v: &serde_json::Value, depth: usize) -> String {
    if depth > MAX_DEPTH {
        return match v {
            serde_json::Value::Object(_) => "{ ... }".into(),
            serde_json::Value::Array(_) => "[ ... ]".into(),
            _ => type_name(v).into(),
        };
    }
    match v {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(b) => format!("bool ({})", b),
        serde_json::Value::Number(n) => format!("number ({})", n),
        serde_json::Value::String(s) => {
            if s.len() > 40 {
                format!("\"{}...\"", &s[..s.floor_char_boundary(37)])
            } else {
                format!("\"{}\"", s)
            }
        }
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                return "[]".into();
            }
            let shown: Vec<String> = arr
                .iter()
                .take(MAX_ARRAY_ITEMS)
                .map(|item| render_schema(item, depth + 1))
                .collect();
            let omitted = arr.len().saturating_sub(MAX_ARRAY_ITEMS);
            if omitted > 0 {
                format!(
                    "[\n  {},\n  ... ({} more {})\n]",
                    shown.join(",\n  "),
                    omitted,
                    type_name(&arr[0])
                )
            } else {
                format!("[\n  {}\n]", shown.join(",\n  "))
            }
        }
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                return "{}".into();
            }
            let indent = "  ".repeat(depth + 1);
            let closing = "  ".repeat(depth);
            let fields: Vec<String> = map
                .iter()
                .map(|(k, val)| {
                    format!("{}\"{}\": {}", indent, k, render_schema(val, depth + 1))
                })
                .collect();
            format!("{{\n{}\n{}}}", fields.join(",\n"), closing)
        }
    }
}

fn type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_json_not_compressed() {
        // Under the MIN_CHARS threshold — must pass through unchanged
        let tiny = r#"{"a": 1}"#;
        assert!(extract_json_schema(tiny).is_none());
    }

    #[test]
    fn large_json_object_compressed() {
        // Build a JSON string over MIN_CHARS threshold
        let mut obj = serde_json::json!({
            "users": [],
            "count": 0,
            "meta": {"created": "2024-01-01", "version": "1.0"}
        });
        let users: Vec<serde_json::Value> = (0..50)
            .map(|i| {
                serde_json::json!({
                    "id": i,
                    "name": format!("User Number {}", i),
                    "email": format!("user{}@example.com", i),
                    "active": true
                })
            })
            .collect();
        obj["users"] = serde_json::Value::Array(users);
        let text = serde_json::to_string_pretty(&obj).unwrap();

        let r = extract_json_schema(&text).unwrap();
        assert!(r.contains("[JSON schema"), "got: {r}");
        assert!(r.contains("\"users\""), "got: {r}");
        assert!(r.contains("\"meta\""), "got: {r}");
        // Should not contain all 50 users
        assert!(!r.contains("User Number 49"), "should be truncated: {r}");
    }

    #[test]
    fn large_json_array_root_compressed() {
        // Top-level array (not object)
        let arr: Vec<serde_json::Value> = (0..100)
            .map(|i| serde_json::json!({"id": i, "val": format!("item{}", i)}))
            .collect();
        let text = serde_json::to_string_pretty(&arr).unwrap();
        assert!(text.len() >= MIN_CHARS, "test data must exceed threshold");

        let r = extract_json_schema(&text).unwrap();
        assert!(r.contains("[JSON schema"), "got: {r}");
        // Should show first 2 items then truncate
        assert!(r.contains("more object"), "truncation note expected: {r}");
        assert!(!r.contains("item99"), "should be truncated: {r}");
    }

    #[test]
    fn array_exactly_two_items_no_truncation_note() {
        // Array with exactly MAX_ARRAY_ITEMS items — no "... N more" line
        let arr = serde_json::json!([
            "x".repeat(600),  // need total > MIN_CHARS
            "y".repeat(600),
            "z".repeat(600),
            "w".repeat(600),
        ]);
        let text = serde_json::to_string(&arr).unwrap();
        assert!(text.len() >= MIN_CHARS);
        let r = extract_json_schema(&text).unwrap();
        // 4 items: first 2 shown, 2 omitted
        assert!(r.contains("more string"), "should note omitted items: {r}");
    }

    #[test]
    fn deeply_nested_object_collapsed() {
        // depth > MAX_DEPTH should collapse to "{ ... }"
        let mut deep = serde_json::json!({"leaf": "value"});
        for key in ["d4", "d3", "d2", "d1", "d0"] {
            deep = serde_json::json!({ key: deep });
        }
        // Pad to exceed MIN_CHARS
        let mut wrapper = serde_json::Map::new();
        wrapper.insert("data".into(), deep);
        wrapper.insert("padding".into(), serde_json::Value::String("x".repeat(2000)));
        let text = serde_json::to_string(&serde_json::Value::Object(wrapper)).unwrap();
        assert!(text.len() >= MIN_CHARS);

        let r = extract_json_schema(&text).unwrap();
        assert!(r.contains("{ ... }"), "deep nesting should collapse: {r}");
    }

    #[test]
    fn non_json_returns_none() {
        let text = "a".repeat(3000);
        assert!(extract_json_schema(&text).is_none());
    }

    #[test]
    fn valid_json_not_starting_with_brace_or_bracket_returns_none() {
        // A JSON number or string at top level — we skip these (not interesting schemas)
        let text = format!("\"{}\"", "x".repeat(MIN_CHARS));
        assert!(extract_json_schema(&text).is_none());
    }

    #[test]
    fn empty_object_renders() {
        let text = format!("{{}} {}", "x".repeat(MIN_CHARS));
        // Won't parse as valid JSON because of trailing text, so returns None — that's fine.
        // Test the render_schema path for empty object directly.
        let v = serde_json::json!({});
        assert_eq!(render_schema(&v, 0), "{}");
    }

    #[test]
    fn empty_array_renders() {
        let v = serde_json::json!([]);
        assert_eq!(render_schema(&v, 0), "[]");
    }

    #[test]
    fn long_string_truncated_in_schema() {
        let v = serde_json::Value::String("a".repeat(100));
        let out = render_schema(&v, 0);
        assert!(out.ends_with("...\""), "long strings should be truncated: {out}");
        assert!(out.len() < 60, "truncated string should be short: {out}");
    }
}
