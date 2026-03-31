//! Session export (HTML, JSONL).

use crate::agent::types::{AgentMessage, ContentBlock, ContentItem};
use crate::str::StrExt as _;
use crate::session::types::{CompactionEntry, SessionEntry};

/// Aggregate token stats across all API calls in a session.
struct SessionStats {
    api_calls: u32,
    total_input: u64,
    total_output: u64,
    total_cache_read: u64,
    total_cache_write: u64,
    max_context: u32,
    context_window: u32,
}

impl SessionStats {
    fn from_entries(entries: &[SessionEntry]) -> Self {
        let mut s = Self {
            api_calls: 0,
            total_input: 0,
            total_output: 0,
            total_cache_read: 0,
            total_cache_write: 0,
            max_context: 0,
            context_window: 0,
        };
        for entry in entries {
            // Count tokens from live message entries.
            if let SessionEntry::Message(me) = entry
                && matches!(me.message, AgentMessage::Assistant(_))
                && let Some(ref tok) = me.tokens
            {
                s.api_calls += 1;
                s.total_input += tok.input as u64;
                s.total_output += tok.output as u64;
                s.total_cache_read += tok.cache_read as u64;
                s.total_cache_write += tok.cache_write as u64;
                if tok.context_used > s.max_context {
                    s.max_context = tok.context_used;
                }
                if tok.context_window > 0 {
                    s.context_window = tok.context_window;
                }
            }
            // Also count the pre-compaction messages that were archived.
            // They don't carry TokenInfo so we can only count them as calls.
            if let SessionEntry::Compaction(ce) = entry {
                for msg in &ce.archived_messages {
                    if matches!(msg, AgentMessage::Assistant(_)) {
                        s.api_calls += 1;
                    }
                }
            }
        }
        s
    }

    fn cache_hit_rate(&self) -> f64 {
        let total_cacheable = self.total_cache_read + self.total_cache_write;
        if total_cacheable == 0 {
            return 0.0;
        }
        self.total_cache_read as f64 / total_cacheable as f64 * 100.0
    }

    /// Per-call cache hit details: how many calls had >50% cache reads.
    fn cache_hit_calls(entries: &[SessionEntry]) -> (u32, u32) {
        let mut good = 0u32;
        let mut total = 0u32;
        for entry in entries {
            if let SessionEntry::Message(me) = entry
                && matches!(me.message, AgentMessage::Assistant(_))
                && let Some(ref tok) = me.tokens
            {
                total += 1;
                let cacheable = tok.cache_read + tok.cache_write;
                if cacheable > 0 && tok.cache_read > cacheable / 2 {
                    good += 1;
                }
            }
        }
        (good, total)
    }
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Find the repo-scoped DB directory that contains a session matching the given prefix.
/// Searches `nerv_dir/repos/*/sessions.db` and returns the repo dir on the first match.
fn find_repo_dir_for_session(session_id: &str, nerv_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let repos_dir = nerv_dir.join("repos");
    let entries = std::fs::read_dir(&repos_dir).ok()?;
    for entry in entries.flatten() {
        let repo_dir = entry.path();
        let db_path = repo_dir.join("sessions.db");
        if !db_path.exists() {
            continue;
        }
        // Quick prefix check directly against the DB — avoids loading all entries.
        let check = rusqlite::Connection::open(&db_path)
            .ok()
            .and_then(|db| {
                db.query_row(
                    "SELECT id FROM sessions WHERE id LIKE ?1 LIMIT 1",
                    rusqlite::params![format!("{}%", session_id)],
                    |row| row.get::<_, String>(0),
                )
                .ok()
            });
        if check.is_some() {
            return Some(repo_dir);
        }
    }
    None
}

/// Resolve a session-id prefix to its repo dir, falling back to nerv_dir itself.
fn repo_dir_for(session_id: &str, nerv_dir: &std::path::Path) -> std::path::PathBuf {
    find_repo_dir_for_session(session_id, nerv_dir).unwrap_or_else(|| nerv_dir.to_path_buf())
}

/// Export a session from the database by ID as JSONL.
pub fn export_session_jsonl(
    session_id: &str,
    path: &std::path::Path,
    nerv_dir: &std::path::Path,
) -> Result<String, String> {
    let mut session_manager = crate::session::SessionManager::new(&repo_dir_for(session_id, nerv_dir));
    session_manager.load_session(session_id).map_err(|e| e.to_string())?;
    let mut content =
        session_manager.export_jsonl().ok_or_else(|| "no session content".to_string())?;

    // Append session summary line
    let entries = session_manager.entries().to_vec();
    let stats = SessionStats::from_entries(&entries);
    if stats.api_calls > 0 {
        let (good_calls, _) = SessionStats::cache_hit_calls(&entries);
        let summary = serde_json::json!({
            "type": "summary",
            "api_calls": stats.api_calls,
            "total_input": stats.total_input,
            "total_output": stats.total_output,
            "cache_read": stats.total_cache_read,
            "cache_write": stats.total_cache_write,
            "cache_hit_rate": (stats.cache_hit_rate() * 10.0).round() / 10.0,
            "cache_hit_calls": good_calls,
            "max_context": stats.max_context,
            "context_window": stats.context_window,
        });
        if let Ok(line) = serde_json::to_string(&summary) {
            content.push('\n');
            content.push_str(&line);
        }
    }

    std::fs::write(path, content).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

/// Export a session from the database by ID.
pub fn export_session_html(
    session_id: &str,
    path: &std::path::Path,
    nerv_dir: &std::path::Path,
) -> Result<String, String> {
    let mut session_manager = crate::session::SessionManager::new(&repo_dir_for(session_id, nerv_dir));
    session_manager.load_session(session_id).map_err(|e| e.to_string())?;
    let entries = session_manager.entries().to_vec();
    render_html_to_file(&entries, path)
}

/// Export entries from a live session (falls back to agent messages if entries
/// empty).
pub fn export_entries_html(
    entries: &[SessionEntry],
    messages: &[AgentMessage],
    path: &std::path::Path,
) -> Result<String, String> {
    if entries.is_empty() {
        let synth: Vec<SessionEntry> = messages
            .iter()
            .map(|msg| {
                SessionEntry::Message(crate::session::types::MessageEntry {
                    id: String::new(),
                    parent_id: None,
                    timestamp: String::new(),
                    message: msg.clone(),
                    tokens: None,
                })
            })
            .collect();
        render_html_to_file(&synth, path)
    } else {
        render_html_to_file(entries, path)
    }
}

/// Render a unified diff string as HTML with line-level color highlighting.
fn highlight_diff_html(diff: &str) -> String {
    let mut out = String::with_capacity(diff.len() * 2);
    for line in diff.lines() {
        let (cls, content) = if line.starts_with("--- ") || line.starts_with("+++ ") {
            ("diff-header", line)
        } else if line.starts_with("@@") {
            ("diff-hunk", line)
        } else if line.starts_with('+') {
            ("diff-add", line)
        } else if line.starts_with('-') {
            ("diff-del", line)
        } else {
            ("", line)
        };
        if cls.is_empty() {
            out.push_str(&html_escape_no_br(content));
            out.push('\n');
        } else {
            // display:block spans generate their own line break; don't add \n or it creates
            // an extra blank line between adjacent diff-header / diff-hunk /
            // diff-add / diff-del spans.
            out.push_str(&format!("<span class='{}'>{}</span>", cls, html_escape_no_br(content)));
        }
    }
    out
}

fn highlight_for_html(text: &str) -> String {
    use crate::tui::highlight::{HlState, highlight_line_html, rules_for_lang};

    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let lang = if first.starts_with("#!/usr/bin/env python")
        || first.starts_with("python")
        || text.contains("\ndef ")
    {
        "python"
    } else if first.starts_with("#!/") || first.contains("&&") || first.contains("| ") {
        "bash"
    } else if text.contains("fn ") && text.contains("->") {
        "rust"
    } else if text.contains("function ") || text.contains("const ") || text.contains("=> {") {
        "javascript"
    } else {
        "bash"
    };

    let Some(rules) = rules_for_lang(lang) else {
        return html_escape(text);
    };

    let mut out = String::with_capacity(text.len() * 2);
    let mut state = HlState::Normal;
    for line in text.lines() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&highlight_line_html(line, &mut state, rules));
    }
    out
}

/// Build a concise args preview string for the tool-call header.
/// For edit/read we prefer a path-based summary over the full JSON dump.
fn args_preview_for(name: &str, arguments: &serde_json::Value, args_str: &str) -> String {
    match name {
        "read" => {
            let path = arguments.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let mut s = path.to_string();
            if let Some(offset) = arguments.get("offset").and_then(|v| v.as_u64()) {
                s.push_str(&format!("  offset={offset}"));
            }
            if let Some(limit) = arguments.get("limit").and_then(|v| v.as_u64()) {
                s.push_str(&format!("  limit={limit}"));
            }
            s
        }
        "edit" => {
            // Show the path(s) being edited
            if let Some(path) = arguments.get("path").and_then(|v| v.as_str()) {
                path.to_string()
            } else if let Some(_edits) = arguments.get("edits").and_then(|v| v.as_array()) {
                // multi-edit: collect unique paths if present, otherwise fall back
                let path = arguments.get("path").and_then(|v| v.as_str());
                if let Some(p) = path {
                    p.to_string()
                } else {
                    // path is a sibling of edits at the top level for multi-edit
                    let t = args_str.truncate_chars(120);
                    if t.len() < args_str.len() { format!("{}...", t) } else { t.to_string() }
                }
            } else {
                let t = args_str.truncate_chars(120);
                if t.len() < args_str.len() { format!("{}...", t) } else { t.to_string() }
            }
        }
        _ => {
            if args_str.len() > 120 {
                format!("{}...", args_str.truncate_chars(120))
            } else {
                args_str.to_string()
            }
        }
    }
}

/// Render numbered file content as syntax-highlighted HTML.
/// Detects language from the path argument to give better highlighting.
fn render_file_content_html(content: &str, arguments: &serde_json::Value) -> String {
    use crate::tui::highlight::{HlState, highlight_line_html, rules_for_lang};

    let path = arguments.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let lang = if path.ends_with(".rs") {
        "rust"
    } else if path.ends_with(".py") {
        "python"
    } else if path.ends_with(".js")
        || path.ends_with(".ts")
        || path.ends_with(".jsx")
        || path.ends_with(".tsx")
    {
        "javascript"
    } else if path.ends_with(".sh") || path.ends_with(".bash") {
        "bash"
    } else if path.ends_with(".toml")
        || path.ends_with(".json")
        || path.ends_with(".yaml")
        || path.ends_with(".yml")
    {
        "toml"
    } else {
        // Guess from content (same heuristics as highlight_for_html)
        let first = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        if first.contains("fn ") && content.contains("->") {
            "rust"
        } else if content.contains("\ndef ") {
            "python"
        } else if content.contains("function ") || content.contains("const ") {
            "javascript"
        } else {
            "bash"
        }
    };

    let Some(rules) = rules_for_lang(lang) else {
        return html_escape(content);
    };

    let mut out = String::with_capacity(content.len() * 2);
    let mut state = HlState::Normal;
    for line in content.lines() {
        if !out.is_empty() {
            out.push('\n');
        }
        // Each line is "NNN\tcontent" — highlight everything after the tab as code,
        // keep the line number prefix as plain (grey) text.
        if let Some(tab_pos) = line.find('\t') {
            let (num, rest) = line.split_at(tab_pos);
            let rest = &rest[1..]; // skip the tab
            out.push_str(&format!(
                "<span style='color:#4a5568;user-select:none'>{}\t</span>",
                html_escape_no_br(num)
            ));
            out.push_str(&highlight_line_html(rest, &mut state, rules));
        } else {
            out.push_str(&highlight_line_html(line, &mut state, rules));
        }
    }
    out
}

fn render_html_to_file(entries: &[SessionEntry], path: &std::path::Path) -> Result<String, String> {
    let mut html = String::from(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>nerv session</title>
<style>
*{box-sizing:border-box}
body{font-family:-apple-system,system-ui,'Segoe UI',sans-serif;max-width:720px;margin:0 auto;padding:2rem 1rem;color:#1a1a1a;line-height:1.7;background:#fff}
.user{background:#fef3c7;border-radius:8px;padding:0.75rem 1rem;margin:1.5rem 0;font-weight:500;border-left:3px solid #f59e0b}
.assistant{margin:1.5rem 0}
.assistant h1,.assistant h2,.assistant h3{margin:1rem 0 0.5rem;font-weight:600}
.assistant h1{font-size:1.4rem}
.assistant h2{font-size:1.2rem}
.assistant h3{font-size:1.1rem}
.assistant code{background:#f0f0f0;padding:0.15em 0.4em;border-radius:3px;font-size:0.9em;font-family:'SF Mono',Menlo,monospace}
.assistant pre{background:#f7f7f7;padding:1rem;border-radius:6px;overflow-x:auto;border:1px solid #e5e5e5}
.assistant pre code{background:none;padding:0}
.assistant blockquote{border-left:3px solid #ddd;padding-left:1rem;color:#555;margin:0.75rem 0}
.assistant ul,.assistant ol{padding-left:1.5rem}
.tool-wrapper{margin:1rem 0;width:80vw;position:relative;left:50%;transform:translateX(-50%)}
.tool-header{display:flex;align-items:center;gap:0.75rem;background:#2d3748;padding:0.5rem 0.75rem;border:1px solid #1a202c;border-radius:6px 6px 0 0;cursor:pointer;user-select:none;color:#e2e8f0;font-family:'SF Mono',Menlo,monospace;font-size:0.8rem}
.tool-header.collapsed{border-radius:6px}
.tool-header:hover{background:#374151}
.tool-name{color:#60a5fa;font-weight:600}
.tool-args{color:#94a3b8;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;flex:1;min-width:0}
.tool-output{border:1px solid #1a202c;border-top:none;border-radius:0 0 6px 6px;padding:1rem;font-family:'SF Mono',Menlo,monospace;font-size:0.8rem;white-space:pre-wrap;overflow-y:auto;background:#1a202c;color:#e2e8f0}
.tool-output.hidden{display:none}
.hl-keyword{color:#c084fc}
.hl-string{color:#6ee7b7}
.hl-comment{color:#6b7280;font-style:italic}
.hl-number{color:#fbbf24}
.hl-type{color:#60a5fa}
.hl-function{color:#f87171}
.hl-operator{color:#94a3b8}
.hl-bracket{color:#e2e8f0}
.hl-constant{color:#fb923c}
.hl-macro{color:#e879f9}
.diff-add{color:#86efac;background:rgba(134,239,172,0.08);display:block}
.diff-del{color:#fca5a5;background:rgba(252,165,165,0.08);display:block}
.diff-hunk{color:#67e8f9;font-style:italic;display:block}
.diff-header{color:#94a3b8;display:block}
.meta{font-size:0.75rem;color:#999;margin-top:0.25rem}
.controls{margin-bottom:1.5rem;padding:1rem;background:#fafafa;border-radius:6px;border:1px solid #eee}
.controls button{padding:0.5rem 1rem;background:#2563eb;color:#fff;border:none;border-radius:4px;cursor:pointer;font-weight:500;transition:background 0.2s}
.controls button:hover{background:#1d4ed8}
hr{border:none;border-top:1px solid #eee;margin:2rem 0}
</style>
</head>
<body>
<div class="controls">
<button onclick="toggleAllTools()">Collapse/Expand All Tool Outputs</button>
</div>
<script>
function toggleAllTools() {
  const outputs = document.querySelectorAll('.tool-output');
  const firstHidden = Array.from(outputs).some(el => el.classList.contains('hidden'));
  outputs.forEach(el => {
    if (firstHidden) {
      el.classList.remove('hidden');
      el.previousElementSibling.classList.remove('collapsed');
    } else {
      el.classList.add('hidden');
      el.previousElementSibling.classList.add('collapsed');
    }
  });
}
function toggleTool(header) {
  const output = header.nextElementSibling;
  output.classList.toggle('hidden');
  header.classList.toggle('collapsed');
}
</script>
"#,
    );

    // Prepass: build lookup maps for tool results so we can inline them into
    // ToolCall divs. call_id → tool_name (from Assistant messages)
    let mut call_name: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // call_id → (display_text, content_text, is_error)
    let mut call_result: std::collections::HashMap<String, (Option<String>, String, bool)> =
        std::collections::HashMap::new();
    for entry in entries.iter() {
        if let SessionEntry::Message(me) = entry {
            match &me.message {
                AgentMessage::Assistant(a) => {
                    for block in &a.content {
                        if let ContentBlock::ToolCall { id, name, .. } = block {
                            call_name.insert(id.clone(), name.clone());
                        }
                    }
                }
                AgentMessage::ToolResult { tool_call_id, content, is_error, display, .. } => {
                    let content_text = content
                        .iter()
                        .filter_map(|c| {
                            if let ContentItem::Text { text } = c {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    call_result
                        .insert(tool_call_id.clone(), (display.clone(), content_text, *is_error));
                }
                _ => {}
            }
        }
    }

    // For edit and read tool calls, we inline the result into the tool-call div and
    // skip the separate "output" div. Track which call_ids are handled this
    // way.
    let inlined_calls: std::collections::HashSet<String> = call_name
        .iter()
        .filter(|(id, name)| {
            matches!(name.as_str(), "edit" | "read") && call_result.contains_key(id.as_str())
        })
        .map(|(id, _)| id.clone())
        .collect();

    // Build a map of first_kept_entry_id → CompactionEntry so we can inject
    // archived messages immediately before the surviving entry they precede,
    // rather than at the compaction entry's position (which is after the
    // verbatim window in the branch order).
    let mut pending_archived: std::collections::HashMap<&str, &CompactionEntry> =
        std::collections::HashMap::new();
    for entry in entries {
        if let SessionEntry::Compaction(ce) = entry {
            if !ce.archived_messages.is_empty() {
                pending_archived.insert(ce.first_kept_entry_id.as_str(), ce);
            }
        }
    }

    // Build per-compaction fingerprint sets of live verbatim-window messages
    // so we can skip duplicates (old sessions stored the full branch in
    // archived_messages, including the verbatim window that stays in the DB).
    let mut verbatim_fingerprints: std::collections::HashMap<&str, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for entry in entries {
        if let SessionEntry::Compaction(ce) = entry {
            let mut collecting = false;
            for e2 in entries {
                match e2 {
                    SessionEntry::Message(me) => {
                        if me.id == ce.first_kept_entry_id { collecting = true; }
                        if collecting {
                            let key = serde_json::to_string(&me.message).unwrap_or_default();
                            verbatim_fingerprints.entry(ce.id.as_str()).or_default().insert(key);
                        }
                    }
                    SessionEntry::Compaction(ce2) if ce2.id == ce.id => break,
                    _ => {}
                }
            }
        }
    }

    let render_archived_msgs =
        |ce: &CompactionEntry,
         html: &mut String,
         call_result: &std::collections::HashMap<String, (Option<String>, String, bool)>,
         tool_idx: &mut usize| {
            let fingerprints = verbatim_fingerprints.get(ce.id.as_str());
            for msg in &ce.archived_messages {
                if let Some(fps) = fingerprints {
                    let key = serde_json::to_string(msg).unwrap_or_default();
                    if fps.contains(&key) {
                        continue;
                    }
                }
                match msg {
                    AgentMessage::User { content, .. } => {
                        html.push_str("<div class='user'>");
                        for item in content {
                            if let ContentItem::Text { text } = item {
                                html.push_str(&html_escape(text));
                            }
                        }
                        html.push_str("</div>\n");
                    }
                    AgentMessage::Assistant(a) => {
                        html.push_str("<div class='assistant'>");
                        for block in &a.content {
                            match block {
                                ContentBlock::Text { text } if !text.is_empty() => {
                                    html.push_str(&markdown_to_html(text));
                                }
                                ContentBlock::ToolCall { id, name, arguments } => {
                                    let args_str = serde_json::to_string(arguments)
                                        .unwrap_or_else(|_| arguments.to_string());
                                    let preview =
                                        args_preview_for(name, arguments, &args_str);
                                    let output_html =
                                        if let Some((disp, content_text, is_err)) =
                                            call_result.get(id.as_str())
                                        {
                                            let txt = disp.as_deref().unwrap_or(content_text);
                                            let style = if *is_err {
                                                "color:#f87171"
                                            } else {
                                                ""
                                            };
                                            format!(
                                                "<div class='tool-output' style='{style}'>{}</div>",
                                                html_escape(txt)
                                            )
                                        } else {
                                            String::new()
                                        };
                                    html.push_str(&format!(
                                        "<div class='tool-wrapper'>\
                                        <div class='tool-header' onclick='toggleTool(this)'>\
                                        <span class='tool-name'>{}</span>\
                                        <span class='tool-args'>{}</span>\
                                        </div>{}</div>\n",
                                        html_escape_no_br(name),
                                        html_escape_no_br(&preview),
                                        output_html,
                                    ));
                                    *tool_idx += 1;
                                }
                                _ => {}
                            }
                        }
                        if let Some(ref u) = a.usage {
                            let context_used = u.input + u.output;
                            let mut meta = format!("↑{} ↓{}", u.input, u.output);
                            if u.cache_read > 0 {
                                meta.push_str(&format!(" Rc{}", u.cache_read));
                            }
                            if u.cache_write > 0 {
                                meta.push_str(&format!(" Wc{}", u.cache_write));
                            }
                            meta.push_str(&format!(" · {} context", context_used));
                            html.push_str(&format!("<div class='meta'>{}</div>", meta));
                        }
                        html.push_str("</div>\n");
                    }
                    _ => {}
                }
            }
        };

    let mut tool_idx = 0;
    for entry in entries {
        // Before rendering this entry, inject any archived messages whose
        // chronological position is immediately before it.
        let entry_id = match entry {
            SessionEntry::Message(me) => Some(me.id.as_str()),
            SessionEntry::Compaction(ce) => Some(ce.id.as_str()),
            _ => None,
        };
        if let Some(id) = entry_id {
            if let Some(ce) = pending_archived.remove(id) {
                render_archived_msgs(ce, &mut html, &call_result, &mut tool_idx);
            }
        }

        if let SessionEntry::Compaction(ce) = entry {
            // Compaction banner (archived turns already emitted above).
            let model_label = if ce.model_id.is_empty() {
                String::new()
            } else {
                format!(" · {}", html_escape_no_br(&ce.model_id))
            };
            html.push_str(&format!(
                "<div style='margin:1.5rem 0;padding:0.75rem 1rem;\
                background:#1e293b;border-radius:6px;border:1px solid #334155;\
                font-size:0.8rem;color:#94a3b8'>\
                <span style='color:#f59e0b;font-weight:600'>⟳ compaction</span>\
                <span style='margin-left:0.75rem'>{model_label}</span>\
                <details style='margin-top:0.5rem'>\
                <summary style='cursor:pointer;color:#64748b'>summary</summary>\
                <div style='margin-top:0.4rem;color:#cbd5e1;white-space:pre-wrap'>{summary}</div>\
                </details></div>\n",
                summary = html_escape_no_br(&ce.summary),
            ));
            continue;
        }
        if let SessionEntry::Btw(bw) = entry {
            html.push_str(&format!(
                "<div style='margin:1.5rem 0;padding:0.75rem 1rem;\
                background:#1e293b;border-radius:6px;border:1px solid #334155;\
                font-size:0.85rem'>\
                <div style='color:#a78bfa;font-weight:600;margin-bottom:0.4rem'>\
                ◆ /btw</div>\
                <div style='color:#94a3b8;margin-bottom:0.5rem'>{}</div>\
                <div style='color:#e2e8f0'>{}</div>\
                </div>\n",
                html_escape_no_br(&bw.note),
                markdown_to_html(&bw.response),
            ));
            continue;
        }
        if let SessionEntry::SystemPrompt(sp) = entry {
            html.push_str(&format!(
                "<details><summary class='meta'>System prompt ({} tok)</summary><pre class='tool'>{}</pre></details>\n",
                sp.token_count,
                html_escape(&sp.prompt),
            ));
            continue;
        }
        if let SessionEntry::Message(me) = entry {
            match &me.message {
                AgentMessage::User { content, .. } => {
                    html.push_str("<div class='user'>");
                    for item in content {
                        if let ContentItem::Text { text } = item {
                            html.push_str(&html_escape(text));
                        }
                    }
                    html.push_str("</div>\n");
                }
                AgentMessage::Assistant(a) => {
                    html.push_str("<div class='assistant'>");
                    for block in &a.content {
                        match block {
                            ContentBlock::Text { text } if !text.is_empty() => {
                                html.push_str(&markdown_to_html(text));
                            }
                            ContentBlock::ToolCall { id, name, arguments } => {
                                let args_str = serde_json::to_string_pretty(arguments)
                                    .unwrap_or_else(|_| arguments.to_string());

                                // For edit and read, show a condensed args summary in the header
                                // and render the actual result (diff / file content) in the body.
                                let args_preview = args_preview_for(name, arguments, &args_str);

                                html.push_str(&format!(
                                    "<div class='tool-wrapper'>\
                                    <div class='tool-header collapsed' onclick='toggleTool(this)'>\
                                    <span class='tool-name'>{}</span>\
                                    <span class='tool-args'>{}</span>\
                                    </div>\
                                    <div class='tool-output hidden' id='tool-{}'>",
                                    html_escape(name),
                                    html_escape(&args_preview),
                                    tool_idx,
                                ));

                                if inlined_calls.contains(id) {
                                    // Inline the tool result (diff for edit, file content for read)
                                    if let Some((display, content_text, _is_err)) =
                                        call_result.get(id)
                                    {
                                        let body = match name.as_str() {
                                            "edit" => {
                                                // display = unified diff
                                                if let Some(d) = display {
                                                    highlight_diff_html(d)
                                                } else {
                                                    highlight_for_html(content_text)
                                                }
                                            }
                                            "read" => {
                                                // content_text = file with line numbers;
                                                // display is just a terse summary, skip it.
                                                if !content_text.is_empty() {
                                                    render_file_content_html(
                                                        content_text,
                                                        arguments,
                                                    )
                                                } else if let Some(d) = display {
                                                    highlight_for_html(d)
                                                } else {
                                                    String::new()
                                                }
                                            }
                                            _ => highlight_for_html(content_text),
                                        };
                                        html.push_str(&body);
                                    }
                                } else {
                                    html.push_str(&highlight_for_html(&args_str));
                                }

                                html.push_str("</div></div>\n");
                                tool_idx += 1;
                            }
                            _ => {}
                        }
                    }
                    if let Some(ref tok) = me.tokens {
                        let mut meta = format!("↑{} ↓{}", tok.input, tok.output);
                        if tok.cache_read > 0 {
                            meta.push_str(&format!(" Rc{}", tok.cache_read));
                        }
                        if tok.cache_write > 0 {
                            meta.push_str(&format!(" Wc{}", tok.cache_write));
                        }
                        meta.push_str(&format!(
                            " · {}/{} context",
                            tok.context_used, tok.context_window
                        ));
                        html.push_str(&format!("<div class='meta'>{}</div>", meta));
                    }
                    html.push_str("</div>\n");
                }
                AgentMessage::ToolResult { tool_call_id, content, is_error, display, .. } => {
                    // Skip tool results that were already inlined into the tool-call div above.
                    if inlined_calls.contains(tool_call_id) {
                        continue;
                    }
                    // For HTML export: prefer full tool content (what the LLM sees) over the terse
                    // display string, which is intentionally compact for the TUI (e.g. grep shows
                    // "12 matches" in the statusbar but we want the actual ripgrep output here).
                    // Exception: if the content is empty or very short, fall back to display.
                    let content_text: String = content
                        .iter()
                        .filter_map(|c| {
                            if let ContentItem::Text { text } = c {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    let display_text: String = if !content_text.is_empty() {
                        content_text.clone()
                    } else if let Some(d) = display {
                        d.clone()
                    } else {
                        content_text.clone()
                    };
                    if !display_text.is_empty() && tool_idx > 0 {
                        let class = if *is_error { "tool-output" } else { "tool-output hidden" };
                        // Unified diffs get syntax-highlighted diff rendering.
                        let rendered = if display_text.starts_with("--- ") {
                            highlight_diff_html(&display_text)
                        } else {
                            highlight_for_html(&display_text)
                        };
                        html.push_str(&format!(
                            "<div class='tool-wrapper'>\
                            <div class='tool-header{}' onclick='toggleTool(this)'>\
                            <span class='tool-name'>output</span>\
                            </div>\
                            <div class='{}'>{}</div></div>\n",
                            if *is_error { "" } else { " collapsed" },
                            class,
                            rendered,
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    // Session summary
    let stats = SessionStats::from_entries(entries);
    if stats.api_calls > 0 {
        let (good_calls, total_calls) = SessionStats::cache_hit_calls(entries);
        html.push_str("<hr>\n<div class='meta' style='font-size:0.8rem;line-height:1.8'>");
        html.push_str(&format!(
            "<strong>Session</strong>: {} API calls · ↑{} ↓{} · Rc {} · Wc {} · cache hit {:.0}% ({}/{})<br>",
            stats.api_calls,
            fmt_tokens(stats.total_input),
            fmt_tokens(stats.total_output),
            fmt_tokens(stats.total_cache_read),
            fmt_tokens(stats.total_cache_write),
            stats.cache_hit_rate(),
            good_calls,
            total_calls,
        ));
        html.push_str(&format!(
            "peak context {}/{}\n",
            fmt_tokens(stats.max_context as u64),
            fmt_tokens(stats.context_window as u64),
        ));
        html.push_str("</div>\n");
    }

    html.push_str("</body>\n</html>\n");
    std::fs::write(path, &html).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

fn markdown_to_html(markdown: &str) -> String {
    let parser = pulldown_cmark::Parser::new(markdown);
    let mut html_output = String::new();
    pulldown_cmark::html::push_html(&mut html_output, parser);
    html_output
}

fn html_escape_no_br(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\n', "<br>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::{AgentMessage, AssistantMessage, ContentBlock, ContentItem, StopReason};
    use crate::session::types::{CompactionEntry, MessageEntry, SessionEntry};

    pub(crate) const USER_ENTRY_ID: &str = "u1";

    fn user_entry(text: &str) -> SessionEntry {
        SessionEntry::Message(MessageEntry {
            id: "u1".to_string(),
            parent_id: None,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            message: AgentMessage::User {
                content: vec![ContentItem::Text { text: text.to_string() }],
                timestamp: 0,
            },
            tokens: None,
        })
    }

    fn assistant_entry(text: &str) -> SessionEntry {
        SessionEntry::Message(MessageEntry {
            id: "a1".to_string(),
            parent_id: None,
            timestamp: "2026-01-01T00:00:01Z".to_string(),
            message: AgentMessage::Assistant(AssistantMessage {
                content: vec![ContentBlock::Text { text: text.to_string() }],
                stop_reason: StopReason::EndTurn,
                usage: None,
                timestamp: 0,
            }),
            tokens: Some(crate::session::types::TokenInfo {
                input: 100,
                output: 50,
                cache_read: 0,
                cache_write: 0,
                context_used: 150,
                context_window: 200_000,
                cost_usd: 0.0,
            }),
        })
    }

    fn archived_user(text: &str) -> AgentMessage {
        AgentMessage::User {
            content: vec![ContentItem::Text { text: text.to_string() }],
            timestamp: 0,
        }
    }

    fn archived_assistant(text: &str) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text { text: text.to_string() }],
            stop_reason: StopReason::EndTurn,
            usage: None,
            timestamp: 0,
        })
    }

    fn archived_assistant_with_usage(text: &str) -> AgentMessage {
        use crate::agent::types::Usage;
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text { text: text.to_string() }],
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage { input: 1000, output: 200, cache_read: 500, cache_write: 50 }),
            timestamp: 0,
        })
    }

    fn compaction_entry_with_archived(archived: Vec<AgentMessage>) -> SessionEntry {
        SessionEntry::Compaction(CompactionEntry {
            id: "c1".to_string(),
            parent_id: None,
            timestamp: "2026-01-01T00:01:00Z".to_string(),
            summary: "Summary of archived work.".to_string(),
            first_kept_entry_id: USER_ENTRY_ID.to_string(),
            tokens_before: 80_000,
            tokens_after: 3_000,
            model_id: "claude-haiku".to_string(),
            cost_usd_before: 0.50,
            archived_messages: archived,
        })
    }

    fn render_to_string(entries: &[SessionEntry]) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("nerv_export_test_{n}.html"));
        render_html_to_file(entries, &tmp).expect("render failed");
        std::fs::read_to_string(&tmp).expect("read failed")
    }

    // ── HTML: archived messages appear inline, no <details> ──────────────────

    #[test]
    fn html_archived_messages_rendered_inline() {
        let entries = vec![
            user_entry("post-compaction question"),
            assistant_entry("post-compaction answer"),
            compaction_entry_with_archived(vec![
                archived_user("archived question"),
                archived_assistant("archived answer"),
            ]),
        ];

        let html = render_to_string(&entries);

        // Archived content must appear verbatim in the HTML
        assert!(
            html.contains("archived question"),
            "archived user text must be in HTML"
        );
        assert!(
            html.contains("archived answer"),
            "archived assistant text must be in HTML"
        );
    }

    #[test]
    fn html_no_details_tag_for_archived_messages() {
        let entries = vec![
            user_entry("surviving turn"),
            compaction_entry_with_archived(vec![
                archived_user("old turn"),
                archived_assistant("old reply"),
            ]),
        ];

        let html = render_to_string(&entries);
        // Archived text must appear in the HTML directly, not only inside a <details>.
        // Find the position of the archived text vs any <details> open/close tags.
        let old_turn_pos = html.find("old turn").expect("archived text missing");
        // If the text is inside a <details>, a <details> tag would appear before it
        // with no matching </details> before the text. Check it's outside.
        let details_before: Vec<_> = html[..old_turn_pos].match_indices("<details").collect();
        let details_closed_before: Vec<_> = html[..old_turn_pos].match_indices("</details>").collect();
        assert_eq!(
            details_before.len(),
            details_closed_before.len(),
            "archived text must not be inside a <details> element (unclosed details before pos {old_turn_pos})"
        );
    }

    #[test]
    fn html_archived_messages_appear_before_compaction_banner() {
        let entries = vec![
            user_entry("after compaction"),
            compaction_entry_with_archived(vec![
                archived_user("before compaction"),
            ]),
        ];

        let html = render_to_string(&entries);

        let archived_pos = html.find("before compaction").expect("archived text missing");
        // The compaction banner contains the '⟳ compaction' marker.
        let banner_pos = html.find("compaction").expect("compaction banner missing");

        assert!(
            archived_pos < banner_pos,
            "archived content (pos {archived_pos}) must appear before the compaction banner (pos {banner_pos})"
        );
    }

    #[test]
    fn html_surviving_messages_appear_after_compaction_banner() {
        let entries = vec![
            user_entry("new question after compaction"),
            assistant_entry("new answer after compaction"),
            compaction_entry_with_archived(vec![archived_user("old")]),
        ];

        let html = render_to_string(&entries);

        // Banner contains '⟳ compaction' marker text.
        let banner_pos = html.find("⟳ compaction").expect("compaction banner missing");
        let surviving_pos = html.find("new question after compaction").expect("surviving text missing");

        assert!(
            surviving_pos < banner_pos,
            "surviving message must appear before the compaction banner (it precedes it in history)"
        );
    }

    #[test]
    fn html_archived_assistant_messages_show_token_metadata() {
        let entries = vec![
            user_entry("post question"),
            compaction_entry_with_archived(vec![
                archived_user("old question"),
                archived_assistant_with_usage("old answer"),
            ]),
        ];
        let html = render_to_string(&entries);

        // The meta div must appear and contain the usage fields.
        // context_used = input + output = 1000 + 200 = 1200
        assert!(html.contains("↑1000"), "input tokens missing: {html}");
        assert!(html.contains("↓200"), "output tokens missing: {html}");
        assert!(html.contains("Rc500"), "cache_read missing: {html}");
        assert!(html.contains("Wc50"), "cache_write missing: {html}");
        assert!(html.contains("1200 context"), "context_used missing: {html}");
        assert!(html.contains("class='meta'"), "meta div missing: {html}");
    }

    #[test]
    fn html_empty_archived_messages_no_crash() {
        let entries = vec![
            compaction_entry_with_archived(vec![]),
            user_entry("clean session"),
        ];

        let html = render_to_string(&entries);
        assert!(html.contains("clean session"));
    }

    // ── SessionStats: archived assistant messages count toward api_calls ──────

    #[test]
    fn session_stats_counts_archived_assistant_calls() {
        let entries = vec![
            compaction_entry_with_archived(vec![
                archived_user("q1"),
                archived_assistant("a1"),
                archived_user("q2"),
                archived_assistant("a2"),
            ]),
            user_entry("live q"),
            assistant_entry("live a"),
        ];

        let stats = SessionStats::from_entries(&entries);
        // 2 archived assistant calls + 1 live assistant call = 3
        assert_eq!(stats.api_calls, 3, "archived assistant messages must count toward api_calls");
    }

    #[test]
    fn session_stats_no_archived_messages_unaffected() {
        let entries = vec![
            user_entry("q"),
            assistant_entry("a"),
        ];

        let stats = SessionStats::from_entries(&entries);
        assert_eq!(stats.api_calls, 1);
    }
}
