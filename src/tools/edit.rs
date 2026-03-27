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

impl AgentTool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> &str {
        "Replace exact text in a file."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}},"required":["path","old_text","new_text"]})
    }
    fn validate(&self, input: &serde_json::Value) -> Result<(), ToolError> {
        for field in &["path", "old_text", "new_text"] {
            if input.get(*field).and_then(|v| v.as_str()).is_none() {
                return Err(ToolError::InvalidArguments {
                    message: format!("{} is required", field),
                });
            }
        }
        Ok(())
    }
    fn execute(&self, input: serde_json::Value, _update: UpdateCallback) -> ToolResult {
        let path_str = input["path"].as_str().unwrap_or("");
        let old_text = input["old_text"].as_str().unwrap_or("");
        let new_text = input["new_text"].as_str().unwrap_or("");
        let abs_path = self.resolve_path(path_str);

        self.mutation_queue.with(&abs_path, || {

        let bytes = match std::fs::read(&abs_path) {
            Ok(b) => b,
            Err(e) => return ToolResult { content: format!("Error reading {}: {}", path_str, e), details: None, is_error: true },
        };
        let content = String::from_utf8_lossy(&bytes);
        let line_ending = if content.contains("\r\n") { "\r\n" } else { "\n" };
        let normalized = content.replace("\r\n", "\n");
        let normalized_old = old_text.replace("\r\n", "\n");

        let matches: Vec<_> = normalized.match_indices(&normalized_old).collect();

        if matches.is_empty() {
            // Try fuzzy match
            let fuzzy_old = normalize_for_fuzzy(&normalized_old);
            let fuzzy_content = normalize_for_fuzzy(&normalized);
            if let Some(fuzzy_pos) = fuzzy_content.find(&fuzzy_old) {
                let fuzzy_line = fuzzy_content[..fuzzy_pos].matches('\n').count();
                let fuzzy_end_line = fuzzy_line + fuzzy_old.matches('\n').count();
                let orig_lines: Vec<&str> = normalized.lines().collect();
                if fuzzy_end_line < orig_lines.len() {
                    let orig_start: usize = orig_lines[..fuzzy_line].iter().map(|l| l.len() + 1).sum();
                    let orig_end: usize = orig_lines[..=fuzzy_end_line].iter().map(|l| l.len() + 1).sum::<usize>().saturating_sub(1);
                    let fuzzy_matches = fuzzy_content.matches(&fuzzy_old).count();
                    if fuzzy_matches > 1 {
                        return ToolResult { content: format!("Error: old_text fuzzy-matches {} times in {}. Must be unique.", fuzzy_matches, path_str), details: None, is_error: true };
                    }
                    let new_content = format!("{}{}{}", &normalized[..orig_start], new_text.replace("\r\n", "\n"), &normalized[orig_end.min(normalized.len())..]);
                    let final_content = if line_ending == "\r\n" { new_content.replace('\n', "\r\n") } else { new_content };
                    if let Err(e) = std::fs::write(&abs_path, &final_content) {
                        return ToolResult { content: format!("Error writing {}: {}", path_str, e), details: None, is_error: true };
                    }
                    let diff_str = super::diff::unified_diff(&content, &final_content, &format!("a/{}", path_str), &format!("b/{}", path_str));
                    return ToolResult { content: format!("(fuzzy match applied)\n{}", diff_str), details: Some(serde_json::json!({"diff": diff_str, "path": path_str, "fuzzy": true})), is_error: false };
                }
            }
            return ToolResult { content: format!("Error: old_text not found in {}", path_str), details: None, is_error: true };
        }
        if matches.len() > 1 {
            return ToolResult { content: format!("Error: old_text found {} times in {}. Must be unique.", matches.len(), path_str), details: None, is_error: true };
        }

        let new_content = normalized.replacen(&normalized_old, &new_text.replace("\r\n", "\n"), 1);
        let final_content = if line_ending == "\r\n" { new_content.replace('\n', "\r\n") } else { new_content };
        if let Err(e) = std::fs::write(&abs_path, &final_content) {
            return ToolResult { content: format!("Error writing {}: {}", path_str, e), details: None, is_error: true };
        }
        let diff_str = super::diff::unified_diff(&content, &final_content, &format!("a/{}", path_str), &format!("b/{}", path_str));
        ToolResult { content: diff_str.clone(), details: Some(serde_json::json!({"diff": diff_str, "path": path_str})), is_error: false }
        }) // close mutation_queue.with
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
