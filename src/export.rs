//! Session export (HTML, JSONL).

use crate::agent::types::*;
use crate::session::types::SessionEntry;

/// Export a session from the database by ID as JSONL.
pub fn export_session_jsonl(
    session_id: &str,
    path: &std::path::Path,
    nerv_dir: &std::path::Path,
) -> Result<String, String> {
    let mut session_manager = crate::session::SessionManager::new(nerv_dir);
    session_manager
        .load_session(session_id)
        .map_err(|e| e.to_string())?;
    let content = session_manager
        .export_jsonl()
        .ok_or_else(|| "no session content".to_string())?;
    std::fs::write(path, content).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

/// Export a session from the database by ID.
pub fn export_session_html(
    session_id: &str,
    path: &std::path::Path,
    nerv_dir: &std::path::Path,
) -> Result<String, String> {
    let mut session_manager = crate::session::SessionManager::new(nerv_dir);
    session_manager
        .load_session(session_id)
        .map_err(|e| e.to_string())?;
    let entries = session_manager.entries().to_vec();
    render_html_to_file(&entries, path)
}

/// Export entries from a live session (falls back to agent messages if entries empty).
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

fn highlight_for_html(text: &str) -> String {
    use crate::tui::highlight::{highlight_line_html, rules_for_lang, HlState};

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

fn render_html_to_file(
    entries: &[SessionEntry],
    path: &std::path::Path,
) -> Result<String, String> {
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

    let mut tool_idx = 0;
    for entry in entries {
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
                            ContentBlock::ToolCall {
                                name, arguments, ..
                            } => {
                                let args_str = serde_json::to_string_pretty(arguments)
                                    .unwrap_or_else(|_| arguments.to_string());
                                let args_preview = if args_str.len() > 120 {
                                    format!("{}...", &args_str[..120])
                                } else {
                                    args_str.clone()
                                };
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
                                html.push_str(&highlight_for_html(&args_str));
                                html.push_str("</div></div>\n");
                                tool_idx += 1;
                            }
                            _ => {}
                        }
                    }
                    if let Some(ref tok) = me.tokens {
                        let mut meta = format!("↑{} ↓{}", tok.input, tok.output);
                        if tok.cache_read > 0 {
                            meta.push_str(&format!(" R{}", tok.cache_read));
                        }
                        if tok.cache_write > 0 {
                            meta.push_str(&format!(" W{}", tok.cache_write));
                        }
                        meta.push_str(&format!(" · {}/{} context", tok.context_used, tok.context_window));
                        html.push_str(&format!("<div class='meta'>{}</div>", meta));
                    }
                    html.push_str("</div>\n");
                }
                AgentMessage::ToolResult {
                    content, is_error, ..
                } => {
                    let text: String = content
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
                    if !text.is_empty() && tool_idx > 0 {
                        let class = if *is_error {
                            "tool-output"
                        } else {
                            "tool-output hidden"
                        };
                        html.push_str(&format!(
                            "<div class='tool-wrapper'>\
                            <div class='tool-header{}' onclick='toggleTool(this)'>\
                            <span class='tool-name'>output</span>\
                            </div>\
                            <div class='{}'>{}</div></div>\n",
                            if *is_error { "" } else { " collapsed" },
                            class,
                            highlight_for_html(&text),
                        ));
                    }
                }
                _ => {}
            }
        }
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

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "<br>")
}
