use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::file_mutation_queue::FileMutationQueue;
use crate::agent::agent::{AgentTool, ToolResult, UpdateCallback};
use crate::errors::ToolError;

pub struct EditTool {
    cwd: PathBuf,
    mutation_queue: Arc<FileMutationQueue>,
}

impl EditTool {
    pub fn new(cwd: PathBuf, mutation_queue: Arc<FileMutationQueue>) -> Self {
        Self {
            cwd,
            mutation_queue,
        }
    }
    fn resolve_path(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        }
    }
}

struct Edit {
    old_text: String,
    new_text: String,
}

impl AgentTool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> &str {
        "Replace exact text in a file. Supports multiple edits in one call via the `edits` parameter."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file to edit"},
                "old_text": {"type": "string", "description": "Exact text to find (single edit mode)"},
                "new_text": {"type": "string", "description": "Replacement text (single edit mode)"},
                "edits": {
                    "type": "array",
                    "description": "Multiple edits to apply atomically. Each edit is matched against the original file content, not incrementally. Edits must not overlap.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": {"type": "string", "description": "Exact text to find"},
                            "new_text": {"type": "string", "description": "Replacement text"}
                        },
                        "required": ["old_text", "new_text"]
                    }
                }
            },
            "required": ["path"]
        })
    }
    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        if input.get("path").and_then(|v| v.as_str()).is_none() {
            return Err(ToolError::InvalidArguments {
                message: "path is required".into(),
            });
        }
        let has_single = input.get("old_text").is_some() || input.get("new_text").is_some();
        let has_multi = input.get("edits").is_some();
        if has_single && has_multi {
            return Err(ToolError::InvalidArguments {
                message: "use either old_text/new_text or edits, not both".into(),
            });
        }
        if !has_single && !has_multi {
            return Err(ToolError::InvalidArguments {
                message: "provide old_text/new_text or edits".into(),
            });
        }
        if has_single {
            if input.get("old_text").and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::InvalidArguments {
                    message: "old_text is required".into(),
                });
            }
            if input.get("new_text").and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::InvalidArguments {
                    message: "new_text is required".into(),
                });
            }
        }
        if has_multi {
            let arr = input["edits"]
                .as_array()
                .ok_or(ToolError::InvalidArguments {
                    message: "edits must be an array".into(),
                })?;
            if arr.is_empty() {
                return Err(ToolError::InvalidArguments {
                    message: "edits must not be empty".into(),
                });
            }
            for (i, e) in arr.iter().enumerate() {
                if e.get("old_text").and_then(|v| v.as_str()).is_none() {
                    return Err(ToolError::InvalidArguments {
                        message: format!("edits[{}].old_text is required", i),
                    });
                }
                if e.get("new_text").and_then(|v| v.as_str()).is_none() {
                    return Err(ToolError::InvalidArguments {
                        message: format!("edits[{}].new_text is required", i),
                    });
                }
            }
        }
        Ok(())
    }

    fn execute(&self, input: serde_json::Value, _update: UpdateCallback) -> ToolResult {
        let path_str = input["path"].as_str().unwrap_or("");
        let abs_path = self.resolve_path(path_str);

        // Build edit list
        let edits: Vec<Edit> = if let Some(arr) = input.get("edits").and_then(|v| v.as_array()) {
            arr.iter()
                .map(|e| Edit {
                    old_text: e["old_text"].as_str().unwrap_or("").to_string(),
                    new_text: e["new_text"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        } else {
            vec![Edit {
                old_text: input["old_text"].as_str().unwrap_or("").to_string(),
                new_text: input["new_text"].as_str().unwrap_or("").to_string(),
            }]
        };

        self.mutation_queue.with(&abs_path, || {
            let bytes = match std::fs::read(&abs_path) {
                Ok(b) => b,
                Err(e) => {
                    return ToolResult {
                        content: format!("Error reading {}: {}", path_str, e),
                        details: None,
                        is_error: true,
                    }
                }
            };
            let content = String::from_utf8_lossy(&bytes);
            let (bom, content_no_bom) = strip_bom(&content);
            let line_ending = if content_no_bom.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            let normalized = content_no_bom.replace("\r\n", "\n");

            if edits.len() == 1 {
                return apply_single_edit(
                    &edits[0],
                    &content,
                    &normalized,
                    bom,
                    line_ending,
                    &abs_path,
                    path_str,
                );
            }

            apply_multi_edit(&edits, &content, &normalized, bom, line_ending, &abs_path, path_str)
        })
    }
}

/// Single-edit path with fuzzy matching fallback.
fn apply_single_edit(
    edit: &Edit,
    content: &str,
    normalized: &str,
    bom: &str,
    line_ending: &str,
    abs_path: &Path,
    path_str: &str,
) -> ToolResult {
    let normalized_old = edit.old_text.replace("\r\n", "\n");
    let matches: Vec<_> = normalized.match_indices(&normalized_old).collect();

    if matches.is_empty() {
        // Fuzzy match fallback
        let fuzzy_old = normalize_for_fuzzy(&normalized_old);
        let fuzzy_content = normalize_for_fuzzy(normalized);
        if let Some(fuzzy_pos) = fuzzy_content.find(&fuzzy_old) {
            let fuzzy_line = fuzzy_content[..fuzzy_pos].matches('\n').count();
            let fuzzy_end_line = fuzzy_line + fuzzy_old.matches('\n').count();
            let orig_lines: Vec<&str> = normalized.lines().collect();
            if fuzzy_end_line < orig_lines.len() {
                let orig_start: usize =
                    orig_lines[..fuzzy_line].iter().map(|l| l.len() + 1).sum();
                let orig_end: usize = orig_lines[..=fuzzy_end_line]
                    .iter()
                    .map(|l| l.len() + 1)
                    .sum::<usize>()
                    .saturating_sub(1);
                let fuzzy_matches = fuzzy_content.matches(&fuzzy_old).count();
                if fuzzy_matches > 1 {
                    return ToolResult {
                        content: format!(
                            "Error: old_text fuzzy-matches {} times in {}. Must be unique.",
                            fuzzy_matches, path_str
                        ),
                        details: None,
                        is_error: true,
                    };
                }
                let new_content = format!(
                    "{}{}{}",
                    &normalized[..orig_start],
                    edit.new_text.replace("\r\n", "\n"),
                    &normalized[orig_end.min(normalized.len())..]
                );
                let final_content = format!("{}{}", bom, restore_line_endings(&new_content, line_ending));
                if let Err(e) = std::fs::write(abs_path, &final_content) {
                    return ToolResult {
                        content: format!("Error writing {}: {}", path_str, e),
                        details: None,
                        is_error: true,
                    };
                }
                let diff_str = super::diff::unified_diff(
                    content,
                    &final_content,
                    &format!("a/{}", path_str),
                    &format!("b/{}", path_str),
                );
                return ToolResult {
                    content: format!("(fuzzy match applied)\n{}", diff_str),
                    details: Some(
                        serde_json::json!({"diff": diff_str, "path": path_str, "fuzzy": true}),
                    ),
                    is_error: false,
                };
            }
        }
        return ToolResult {
            content: format!("Error: old_text not found in {}", path_str),
            details: None,
            is_error: true,
        };
    }

    if matches.len() > 1 {
        return ToolResult {
            content: format!(
                "Error: old_text found {} times in {}. Must be unique.",
                matches.len(),
                path_str
            ),
            details: None,
            is_error: true,
        };
    }

    let new_content = normalized.replacen(&normalized_old, &edit.new_text.replace("\r\n", "\n"), 1);
    if new_content == normalized {
        return ToolResult {
            content: format!("No changes: old_text and new_text are identical in {}", path_str),
            details: None,
            is_error: true,
        };
    }
    let final_content = format!("{}{}", bom, restore_line_endings(&new_content, line_ending));
    if let Err(e) = std::fs::write(abs_path, &final_content) {
        return ToolResult {
            content: format!("Error writing {}: {}", path_str, e),
            details: None,
            is_error: true,
        };
    }
    let diff_str = super::diff::unified_diff(
        content,
        &final_content,
        &format!("a/{}", path_str),
        &format!("b/{}", path_str),
    );
    ToolResult {
        content: diff_str.clone(),
        details: Some(serde_json::json!({"diff": diff_str, "path": path_str})),
        is_error: false,
    }
}

/// Multi-edit: sort by position, apply with forward cursor, one write + one diff.
fn apply_multi_edit(
    edits: &[Edit],
    content: &str,
    normalized: &str,
    bom: &str,
    line_ending: &str,
    abs_path: &Path,
    path_str: &str,
) -> ToolResult {
    // Normalize all old_text values and find their positions in original content
    let mut positioned: Vec<(usize, &Edit, String)> = Vec::with_capacity(edits.len());
    for (i, edit) in edits.iter().enumerate() {
        let norm_old = edit.old_text.replace("\r\n", "\n");
        let pos = match normalized.find(&norm_old) {
            Some(p) => p,
            None => {
                return ToolResult {
                    content: format!(
                        "Error: edits[{}].old_text not found in {}",
                        i, path_str
                    ),
                    details: None,
                    is_error: true,
                }
            }
        };
        positioned.push((pos, edit, norm_old));
    }

    // Sort by position (top-to-bottom in file)
    positioned.sort_by_key(|(pos, _, _)| *pos);

    // Check for overlaps
    for w in positioned.windows(2) {
        let (pos_a, _, ref old_a) = w[0];
        let (pos_b, _, _) = w[1];
        if pos_a + old_a.len() > pos_b {
            return ToolResult {
                content: format!("Error: edits overlap in {}", path_str),
                details: None,
                is_error: true,
            };
        }
    }

    // Preflight: verify all matches exist with forward cursor on original content
    {
        let mut cursor = 0usize;
        for (i, (_, _, norm_old)) in positioned.iter().enumerate() {
            match normalized[cursor..].find(norm_old.as_str()) {
                Some(rel) => cursor += rel + norm_old.len(),
                None => {
                    return ToolResult {
                        content: format!(
                            "Error: edits[{}].old_text not found (forward scan) in {}",
                            i, path_str
                        ),
                        details: None,
                        is_error: true,
                    }
                }
            }
        }
    }

    // Apply all replacements (reverse order to preserve positions)
    let mut result = normalized.to_string();
    for (pos, edit, norm_old) in positioned.iter().rev() {
        let norm_new = edit.new_text.replace("\r\n", "\n");
        result.replace_range(*pos..*pos + norm_old.len(), &norm_new);
    }

    if result == normalized {
        return ToolResult {
            content: format!("No changes: all edits produced identical content in {}", path_str),
            details: None,
            is_error: true,
        };
    }

    let final_content = format!("{}{}", bom, restore_line_endings(&result, line_ending));
    if let Err(e) = std::fs::write(abs_path, &final_content) {
        return ToolResult {
            content: format!("Error writing {}: {}", path_str, e),
            details: None,
            is_error: true,
        };
    }

    let diff_str = super::diff::unified_diff(
        content,
        &final_content,
        &format!("a/{}", path_str),
        &format!("b/{}", path_str),
    );
    ToolResult {
        content: format!("Applied {} edits.\n{}", edits.len(), diff_str),
        details: Some(serde_json::json!({"diff": diff_str, "path": path_str, "edits": edits.len()})),
        is_error: false,
    }
}

fn strip_bom(content: &str) -> (&str, &str) {
    if let Some(rest) = content.strip_prefix('\u{FEFF}') {
        ("\u{FEFF}", rest)
    } else {
        ("", content)
    }
}

fn restore_line_endings(content: &str, line_ending: &str) -> String {
    if line_ending == "\r\n" {
        content.replace('\n', "\r\n")
    } else {
        content.to_string()
    }
}

fn normalize_for_fuzzy(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' => '\'',
            '\u{201C}' | '\u{201D}' => '"',
            '\u{2013}' | '\u{2014}' => '-',
            _ => c,
        })
        .collect::<String>()
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}
