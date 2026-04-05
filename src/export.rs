//! Session export (HTML, JSONL).

use crate::agent::types::{AgentMessage, ContentBlock, ContentItem};
use crate::session::types::{CompactionEntry, SessionEntry};

const TEMPLATE_HTML: &str = include_str!("export/template.html");
const TEMPLATE_CSS: &str = include_str!("export/template.css");
const TEMPLATE_JS: &str = include_str!("export/template.js");
const MARKED_JS: &str = include_str!("export/vendor/marked.min.js");
const HIGHLIGHT_JS: &str = include_str!("export/vendor/highlight.min.js");

/// Find the repo-scoped DB directory that contains a session matching the given prefix.
fn find_repo_dir_for_session(
    session_id: &str,
    nerv_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let repos_dir = nerv_dir.join("repos");
    let entries = std::fs::read_dir(&repos_dir).ok()?;
    for entry in entries.flatten() {
        let repo_dir = entry.path();
        let db_path = repo_dir.join("sessions.db");
        if !db_path.exists() {
            continue;
        }
        let check = rusqlite::Connection::open(&db_path).ok().and_then(|db| {
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

fn repo_dir_for(session_id: &str, nerv_dir: &std::path::Path) -> std::path::PathBuf {
    find_repo_dir_for_session(session_id, nerv_dir).unwrap_or_else(|| nerv_dir.to_path_buf())
}

/// Export a session from the database by ID as JSONL.
pub fn export_session_jsonl(
    session_id: &str,
    path: &std::path::Path,
    nerv_dir: &std::path::Path,
) -> Result<String, String> {
    let mut session_manager =
        crate::session::SessionManager::new(&repo_dir_for(session_id, nerv_dir));
    session_manager.load_session(session_id).map_err(|e| e.to_string())?;
    let content =
        session_manager.export_jsonl().ok_or_else(|| "no session content".to_string())?;
    std::fs::write(path, content).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

/// Export a session from the database by ID as HTML.
pub fn export_session_html(
    session_id: &str,
    path: &std::path::Path,
    nerv_dir: &std::path::Path,
) -> Result<String, String> {
    let mut session_manager =
        crate::session::SessionManager::new(&repo_dir_for(session_id, nerv_dir));
    session_manager.load_session(session_id).map_err(|e| e.to_string())?;
    let entries = session_manager.entries().to_vec();
    let html = render_session_html(&entries);
    std::fs::write(path, &html).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

/// Export entries from a live session (falls back to agent messages if entries empty).
pub fn export_entries_html(
    entries: &[SessionEntry],
    messages: &[AgentMessage],
    path: &std::path::Path,
) -> Result<String, String> {
    let effective: Vec<SessionEntry> = if entries.is_empty() {
        messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                SessionEntry::Message(crate::session::types::MessageEntry {
                    id: format!("synth-{i}"),
                    parent_id: if i == 0 { None } else { Some(format!("synth-{}", i - 1)) },
                    timestamp: String::new(),
                    message: m.clone(),
                    tokens: None,
                })
            })
            .collect()
    } else {
        entries.to_vec()
    };
    let html = render_session_html(&effective);
    std::fs::write(path, &html).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

/// Build the JSON payload for the session.
pub(crate) fn build_session_json(entries: &[SessionEntry]) -> serde_json::Value {
    let flat = flatten_entries(entries);
    let header = extract_header(&flat);
    let json_entries: Vec<serde_json::Value> =
        flat.iter().filter_map(serialize_entry).collect();
    serde_json::json!({ "header": header, "entries": json_entries })
}

#[cfg(test)]
fn base64_decode(s: &str) -> Vec<u8> {
    const DECODE: [u8; 128] = {
        let mut t = [255u8; 128];
        let enc = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0usize;
        while i < enc.len() { t[enc[i] as usize] = i as u8; i += 1; }
        t
    };
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut i = 0;
    while i + 3 < bytes.len() {
        let b = bytes[i..i+4].iter().map(|&c| DECODE[c as usize]).collect::<Vec<_>>();
        let n = ((b[0] as u32) << 18) | ((b[1] as u32) << 12) | ((b[2] as u32) << 6) | (b[3] as u32);
        out.push((n >> 16) as u8);
        if bytes[i+2] != b'=' { out.push((n >> 8) as u8); }
        if bytes[i+3] != b'=' { out.push(n as u8); }
        i += 4;
    }
    out
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 63) as usize] as char);
        out.push(CHARS[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { CHARS[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { CHARS[(n & 63) as usize] as char } else { '=' });
    }
    out
}

fn render_session_html(entries: &[SessionEntry]) -> String {
    let session_json = build_session_json(entries);
    let json_str = serde_json::to_string(&session_json).unwrap_or_default();
    // Base64-encode the JSON so it contains only [A-Za-z0-9+/=] — no HTML
    // parser or JS parser special characters at all. The JS side decodes with
    // atob() + TextDecoder. This avoids every </script> detection edge case.
    let b64 = base64_encode(json_str.as_bytes());

    // Inject static assets first, then session data last.
    // If we did session data first, any {{...}} placeholder that appears in
    // the session content (e.g. inside code blocks or plan text) would be
    // expanded by the subsequent replacements, corrupting the output.
    TEMPLATE_HTML
        .replace("{{CSS}}", TEMPLATE_CSS)
        .replace("{{MARKED_JS}}", MARKED_JS)
        .replace("{{HIGHLIGHT_JS}}", HIGHLIGHT_JS)
        .replace("{{APP_JS}}", TEMPLATE_JS)
        .replace("{{SESSION_DATA}}", &b64)
}

/// Flatten archived messages from compaction entries into the main list,
/// injecting them chronologically before the compaction marker.
fn flatten_entries(entries: &[SessionEntry]) -> Vec<SessionEntry> {
    // Build fingerprints of the verbatim window for each compaction so we
    // don't emit archived messages that are still present in the live entries.
    let mut verbatim_fps: std::collections::HashMap<
        &str,
        std::collections::HashSet<String>,
    > = std::collections::HashMap::new();
    for entry in entries {
        if let SessionEntry::Compaction(ce) = entry {
            let mut collecting = false;
            for e2 in entries {
                if let SessionEntry::Message(me) = e2 {
                    if me.id == ce.first_kept_entry_id {
                        collecting = true;
                    }
                    if collecting {
                        let key = serde_json::to_string(&me.message).unwrap_or_default();
                        verbatim_fps.entry(ce.id.as_str()).or_default().insert(key);
                    }
                }
                if let SessionEntry::Compaction(ce2) = e2 {
                    if ce2.id == ce.id {
                        break;
                    }
                }
            }
        }
    }

    let mut out: Vec<SessionEntry> = Vec::with_capacity(entries.len() * 2);
    for entry in entries {
        if let SessionEntry::Compaction(ce) = entry {
            // Inject archived messages before the compaction marker.
            if !ce.archived_messages.is_empty() {
                let fps = verbatim_fps.get(ce.id.as_str());
                for (i, msg) in ce.archived_messages.iter().enumerate() {
                    let key = serde_json::to_string(msg).unwrap_or_default();
                    if fps.map(|f| f.contains(&key)).unwrap_or(false) {
                        continue;
                    }
                    let prev_id = if i == 0 {
                        ce.parent_id.clone()
                    } else {
                        Some(format!("archived-{}-{}", ce.id, i - 1))
                    };
                    // Wrap archived AgentMessage in a MessageEntry
                    let me = crate::session::types::MessageEntry {
                        id: format!("archived-{}-{}", ce.id, i),
                        parent_id: prev_id,
                        timestamp: String::new(),
                        message: msg.clone(),
                        tokens: None,
                    };
                    out.push(SessionEntry::Message(me));
                }
            }
        }
        out.push(entry.clone());
    }
    out
}

fn extract_header(entries: &[SessionEntry]) -> serde_json::Value {
    let mut session_id = String::new();
    let mut timestamp = String::new();
    for entry in entries {
        if let SessionEntry::SessionInfo(si) = entry {
            if let Some(ref name) = si.name {
                if session_id.is_empty() {
                    session_id = name.clone();
                }
            }
            if timestamp.is_empty() {
                timestamp = si.timestamp.clone();
            }
        }
        // Use first entry timestamp as fallback
        if timestamp.is_empty() {
            timestamp = match entry {
                SessionEntry::Message(me) => me.timestamp.clone(),
                SessionEntry::Compaction(ce) => ce.timestamp.clone(),
                SessionEntry::ModelChange(me) => me.timestamp.clone(),
                _ => String::new(),
            };
        }
        if !timestamp.is_empty() {
            break;
        }
    }
    serde_json::json!({ "id": session_id, "timestamp": timestamp, "cwd": "" })
}

/// Join `ContentItem::Text` parts into a single string.
fn content_to_string(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|c| if let ContentItem::Text { text } = c { Some(text.as_str()) } else { None })
        .collect::<Vec<_>>()
        .join("")
}

fn serialize_entry(entry: &SessionEntry) -> Option<serde_json::Value> {
    match entry {
        SessionEntry::Message(me) => Some(serialize_message_entry(me)),
        SessionEntry::Compaction(ce) => Some(serialize_compaction(ce)),
        SessionEntry::ModelChange(mc) => Some(serde_json::json!({
            "type": "modelChange",
            "id": mc.id,
            "parent_id": mc.parent_id,
            "timestamp": mc.timestamp,
            "provider": mc.provider,
            "model_id": mc.model_id,
        })),
        SessionEntry::ThinkingLevelChange(tc) => Some(serde_json::json!({
            "type": "thinkingLevelChange",
            "id": tc.id,
            "parent_id": tc.parent_id,
            "timestamp": tc.timestamp,
            "level": tc.thinking_level,
        })),
        SessionEntry::BranchSummary(bs) => Some(serde_json::json!({
            "type": "branchSummary",
            "id": bs.id,
            "parent_id": bs.parent_id,
            "timestamp": bs.timestamp,
            "summary": bs.summary,
            "from_id": bs.from_id,
        })),
        SessionEntry::CustomMessage(cm) => Some(serialize_custom_message(cm)),
        SessionEntry::Label(lbl) => Some(serde_json::json!({
            "type": "label",
            "id": lbl.id,
            "parent_id": lbl.parent_id,
            "timestamp": lbl.timestamp,
            "label": lbl.label,
        })),
        SessionEntry::SessionInfo(si) => Some(serde_json::json!({
            "type": "sessionInfo",
            "id": si.id,
            "parent_id": si.parent_id,
            "timestamp": si.timestamp,
            "name": si.name,
        })),
        SessionEntry::SystemPrompt(sp) => Some(serde_json::json!({
            "type": "systemPrompt",
            "id": sp.id,
            "parent_id": sp.parent_id,
            "timestamp": sp.timestamp,
            "prompt": sp.prompt,
            "token_count": sp.token_count,
        })),
        SessionEntry::Btw(bw) => Some(serde_json::json!({
            "type": "btw",
            "id": bw.id,
            "parent_id": bw.parent_id,
            "timestamp": bw.timestamp,
            "note": bw.note,
            "response": bw.response,
            "model_id": bw.model_id,
        })),
        // PermissionAccept is internal bookkeeping — skip from export.
        SessionEntry::PermissionAccept(_) => None,
    }
}

fn serialize_message_entry(me: &crate::session::types::MessageEntry) -> serde_json::Value {
    match &me.message {
        AgentMessage::User { content, .. } => {
            let text = content_to_string(content);
            serde_json::json!({
                "type": "message",
                "role": "user",
                "id": me.id,
                "parent_id": me.parent_id,
                "timestamp": if me.timestamp.is_empty() { serde_json::Value::Null } else { me.timestamp.clone().into() },
                "content": text,
            })
        }
        AgentMessage::Assistant(a) => {
            // Serialize content blocks
            let content: Vec<serde_json::Value> = a
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => {
                        serde_json::json!({ "type": "text", "text": text })
                    }
                    ContentBlock::Thinking { thinking } => {
                        serde_json::json!({ "type": "thinking", "thinking": thinking })
                    }
                    ContentBlock::ToolCall { id, name, arguments } => {
                        // Emit as "tool_use" with "input" so the JS template
                        // (which follows the plan's naming convention) works
                        // without special-casing.
                        serde_json::json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": arguments,
                        })
                    }
                })
                .collect();

            let stop_reason = match &a.stop_reason {
                crate::agent::types::StopReason::EndTurn => "end_turn",
                crate::agent::types::StopReason::ToolUse => "tool_use",
                crate::agent::types::StopReason::MaxTokens => "max_tokens",
                crate::agent::types::StopReason::Aborted => "aborted",
                crate::agent::types::StopReason::Error { .. } => "error",
            };

            // Usage: prefer TokenInfo from the session DB row; fall back to
            // inner AgentMessage usage (archived messages have no TokenInfo).
            let usage = if let Some(tok) = &me.tokens {
                Some(serde_json::json!({
                    "input": tok.input,
                    "output": tok.output,
                    "cache_read": tok.cache_read,
                    "cache_write": tok.cache_write,
                    "context_used": tok.context_used,
                    "context_window": tok.context_window,
                    "cost_usd": tok.cost_usd,
                }))
            } else if let Some(u) = &a.usage {
                Some(serde_json::json!({
                    "input": u.input,
                    "output": u.output,
                    "cache_read": u.cache_read,
                    "cache_write": u.cache_write,
                }))
            } else {
                None
            };

            serde_json::json!({
                "type": "message",
                "role": "assistant",
                "id": me.id,
                "parent_id": me.parent_id,
                "timestamp": if me.timestamp.is_empty() { serde_json::Value::Null } else { me.timestamp.clone().into() },
                "content": content,
                "stop_reason": stop_reason,
                "usage": usage,
            })
        }
        AgentMessage::ToolResult {
            tool_call_id, content, is_error, display, details, ..
        } => {
            let content_text = content_to_string(content);
            let display_val = display
                .as_deref()
                .or_else(|| details.as_ref().and_then(|d| d.display.as_deref()));
            let tool_name: Option<&str> = None; // ToolDetails has no tool_name field
            serde_json::json!({
                "type": "message",
                "role": "toolResult",
                "id": me.id,
                "parent_id": me.parent_id,
                "timestamp": if me.timestamp.is_empty() { serde_json::Value::Null } else { me.timestamp.clone().into() },
                "tool_call_id": tool_call_id,
                "tool_name": tool_name,
                "is_error": is_error,
                "content": content_text,
                "display": display_val,
                "details": details,
            })
        }
        AgentMessage::BashExecution { command, output, exit_code, .. } => {
            serde_json::json!({
                "type": "message",
                "role": "bashExecution",
                "id": me.id,
                "parent_id": me.parent_id,
                "timestamp": if me.timestamp.is_empty() { serde_json::Value::Null } else { me.timestamp.clone().into() },
                "command": command,
                "output": output,
                "exit_code": exit_code,
            })
        }
        AgentMessage::Custom { custom_type, content, display, .. } => {
            let content_text = content_to_string(content);
            serde_json::json!({
                "type": "message",
                "role": "custom",
                "id": me.id,
                "parent_id": me.parent_id,
                "timestamp": if me.timestamp.is_empty() { serde_json::Value::Null } else { me.timestamp.clone().into() },
                "custom_type": custom_type,
                "content": content_text,
                "display": display,
            })
        }
        AgentMessage::CompactionSummary { summary, tokens_before, .. } => {
            serde_json::json!({
                "type": "compactionSummary",
                "id": me.id,
                "parent_id": me.parent_id,
                "timestamp": if me.timestamp.is_empty() { serde_json::Value::Null } else { me.timestamp.clone().into() },
                "summary": summary,
                "tokens_before": tokens_before,
            })
        }
        AgentMessage::BranchSummary { summary, from_id, .. } => {
            serde_json::json!({
                "type": "branchSummary",
                "id": me.id,
                "parent_id": me.parent_id,
                "timestamp": if me.timestamp.is_empty() { serde_json::Value::Null } else { me.timestamp.clone().into() },
                "summary": summary,
                "from_id": from_id,
            })
        }
    }
}

fn serialize_custom_message(cm: &crate::session::types::CustomMessageEntry) -> serde_json::Value {
    // CustomMessageEntry wraps an AgentMessage. Serialize as its underlying role.
    let fake_me = crate::session::types::MessageEntry {
        id: cm.id.clone(),
        parent_id: cm.parent_id.clone(),
        timestamp: cm.timestamp.clone(),
        message: cm.message.clone(),
        tokens: None,
    };
    serialize_message_entry(&fake_me)
}

fn serialize_compaction(ce: &CompactionEntry) -> serde_json::Value {
    serde_json::json!({
        "type": "compaction",
        "id": ce.id,
        "parent_id": ce.parent_id,
        "timestamp": ce.timestamp,
        "summary": ce.summary,
        "tokens_before": ce.tokens_before,
        "tokens_after": ce.tokens_after,
        "cost_usd_before": ce.cost_usd_before,
        "first_kept_entry_id": ce.first_kept_entry_id,
        "model_id": ce.model_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::{
        AgentMessage, AssistantMessage, ContentBlock, ContentItem, StopReason,
    };
    use crate::session::types::{CompactionEntry, MessageEntry, SessionEntry};

    fn extract_session_json(html: &str) -> serde_json::Value {
        let open = "'";
        let start = html
            .find("var __SESSION_B64__='")
            .expect("session-b64 not found")
            + "var __SESSION_B64__='".len();
        let end = html[start..].find("';").expect("closing ';");
        let b64 = html[start..start + end].trim();
        // Decode base64
        let json_bytes = base64_decode(b64);
        let json_str = std::str::from_utf8(&json_bytes).expect("utf8");
        serde_json::from_str(json_str).expect("valid JSON")
    }

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

    fn assistant_entry_with_tokens(text: &str, cost_usd: f64, context_used: u32) -> SessionEntry {
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
                context_used,
                context_window: 200_000,
                cost_usd,
            }),
        })
    }

    fn tool_result_entry(tool_call_id: &str, content: &str, display: Option<&str>) -> SessionEntry {
        SessionEntry::Message(MessageEntry {
            id: "tr1".to_string(),
            parent_id: None,
            timestamp: "2026-01-01T00:00:02Z".to_string(),
            message: AgentMessage::ToolResult {
                tool_call_id: tool_call_id.to_string(),
                content: vec![ContentItem::Text { text: content.to_string() }],
                is_error: false,
                display: display.map(|s| s.to_string()),
                details: None,
                timestamp: 0,
            },
            tokens: None,
        })
    }

    fn btw_entry(note: &str, response: &str) -> SessionEntry {
        SessionEntry::Btw(crate::session::types::BtwEntry {
            id: "btw1".to_string(),
            parent_id: None,
            timestamp: "2026-01-01T00:00:03Z".to_string(),
            note: note.to_string(),
            response: response.to_string(),
            model_id: String::new(),
        })
    }

    fn compaction_with_archived(archived: Vec<AgentMessage>) -> SessionEntry {
        SessionEntry::Compaction(CompactionEntry {
            id: "c1".to_string(),
            parent_id: None,
            timestamp: "2026-01-01T00:01:00Z".to_string(),
            summary: "Summary.".to_string(),
            first_kept_entry_id: "u1".to_string(),
            tokens_before: 80_000,
            tokens_after: 3_000,
            model_id: "claude-haiku".to_string(),
            cost_usd_before: 1.23,
            compaction_type: "full".to_string(),
            lite_compact_zeroed: 0,
            archived_messages: archived,
            preserved_user_messages: vec![],
        })
    }

    fn permission_accept_entry() -> SessionEntry {
        SessionEntry::PermissionAccept(crate::session::types::PermissionAcceptEntry {
            id: "pa1".to_string(),
            parent_id: None,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            tool: "epsh".to_string(),
            args: "{}".to_string(),
        })
    }

    fn render_html(entries: &[SessionEntry]) -> String {
        render_session_html(entries)
    }

    #[test]
    fn test_user_message_serialized() {
        let entries = vec![user_entry("hello world")];
        let html = render_html(&entries);
        let json = extract_session_json(&html);
        let e = &json["entries"][0];
        assert_eq!(e["role"], "user");
        assert_eq!(e["content"], "hello world");
    }

    #[test]
    fn test_assistant_usage_from_token_info() {
        let entries = vec![assistant_entry_with_tokens("hi", 0.05, 5000)];
        let html = render_html(&entries);
        let json = extract_session_json(&html);
        let usage = &json["entries"][0]["usage"];
        assert!((usage["cost_usd"].as_f64().unwrap() - 0.05).abs() < 1e-9);
        assert_eq!(usage["context_used"].as_u64().unwrap(), 5000);
    }

    #[test]
    fn test_tool_result_has_content_and_display() {
        let entries = vec![tool_result_entry("call_abc", "raw content", Some("display html"))];
        let html = render_html(&entries);
        let json = extract_session_json(&html);
        let e = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["role"] == "toolResult")
            .unwrap();
        assert_eq!(e["content"], "raw content");
        assert_eq!(e["display"], "display html");
    }

    #[test]
    fn test_archived_messages_flattened_before_compaction() {
        let entries = vec![compaction_with_archived(vec![
            AgentMessage::User {
                content: vec![ContentItem::Text { text: "archived q".to_string() }],
                timestamp: 0,
            },
            AgentMessage::Assistant(AssistantMessage {
                content: vec![ContentBlock::Text { text: "archived a".to_string() }],
                stop_reason: StopReason::EndTurn,
                usage: None,
                timestamp: 0,
            }),
        ])];
        let html = render_html(&entries);
        let json = extract_session_json(&html);
        let arr = json["entries"].as_array().unwrap();
        let compaction_idx = arr.iter().position(|e| e["type"] == "compaction").unwrap();
        let archived_count = arr[..compaction_idx]
            .iter()
            .filter(|e| e["id"].as_str().unwrap_or("").starts_with("archived-"))
            .count();
        assert_eq!(archived_count, 2);
    }

    #[test]
    fn test_btw_entry_serialized() {
        let entries = vec![btw_entry("a note", "a response")];
        let html = render_html(&entries);
        let json = extract_session_json(&html);
        let e = &json["entries"][0];
        assert_eq!(e["type"], "btw");
        assert_eq!(e["note"], "a note");
        assert_eq!(e["response"], "a response");
    }

    #[test]
    fn test_compaction_has_cost_usd_before() {
        let entries = vec![compaction_with_archived(vec![])];
        let html = render_html(&entries);
        let json = extract_session_json(&html);
        let e = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["type"] == "compaction")
            .unwrap();
        assert!((e["cost_usd_before"].as_f64().unwrap() - 1.23).abs() < 1e-9);
    }

    #[test]
    fn test_permission_accept_not_in_entries() {
        let entries = vec![permission_accept_entry(), user_entry("hi")];
        let html = render_html(&entries);
        let json = extract_session_json(&html);
        let arr = json["entries"].as_array().unwrap();
        assert!(!arr.iter().any(|e| e["type"] == "permissionAccept"));
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn test_synthesized_entries_from_messages() {
        let messages = vec![AgentMessage::User {
            content: vec![ContentItem::Text { text: "synthesized".to_string() }],
            timestamp: 0,
        }];
        let tmp = std::env::temp_dir().join("nerv_synth_test.html");
        export_entries_html(&[], &messages, &tmp).expect("export failed");
        let html = std::fs::read_to_string(&tmp).unwrap();
        let json = extract_session_json(&html);
        let e = &json["entries"][0];
        assert_eq!(e["role"], "user");
        assert_eq!(e["content"], "synthesized");
        assert_eq!(e["id"], "synth-0");
    }

    #[test]
    fn test_tool_call_normalized_to_tool_use() {
        // ToolCall blocks should be serialized as type:"tool_use" with "input"
        // so the JS template can use them uniformly.
        let entries = vec![SessionEntry::Message(MessageEntry {
            id: "a1".to_string(),
            parent_id: None,
            timestamp: String::new(),
            message: AgentMessage::Assistant(AssistantMessage {
                content: vec![ContentBlock::ToolCall {
                    id: "call_1".to_string(),
                    name: "read".to_string(),
                    arguments: serde_json::json!({ "path": "src/foo.rs" }),
                }],
                stop_reason: StopReason::EndTurn,
                usage: None,
                timestamp: 0,
            }),
            tokens: None,
        })];
        let html = render_html(&entries);
        let json = extract_session_json(&html);
        let block = &json["entries"][0]["content"][0];
        assert_eq!(block["type"], "tool_use");
        assert_eq!(block["name"], "read");
        assert_eq!(block["input"]["path"], "src/foo.rs");
    }

    #[test]
    fn test_no_inner_script_close_tag_in_any_script_block() {
        // Every <script> block in the rendered HTML must not contain </script
        // (case-insensitive) as raw bytes — the HTML parser would terminate the
        // block early and cause JS parse errors in the browser.
        //
        // This covers the regression where:
        //  - session content containing </script> strings wasn't escaped
        //  - a comment in template.js contained </script> literally
        let entries = vec![
            user_entry("check </script> and </SCRIPT> and </Script> in content"),
            tool_result_entry(
                "call_1",
                "output with </script> inside\nand another </SCRIPT> line",
                Some("display with </script>"),
            ),
        ];
        let html = render_html(&entries);

        // Walk every <script>...</script> block and assert no inner </script.
        let mut pos = 0;
        let mut block_count = 0;
        loop {
            let Some(open) = html[pos..].find("<script") else { break };
            let open = pos + open;
            let Some(content_start_rel) = html[open..].find('>') else { break };
            let content_start = open + content_start_rel + 1;

            // Find the closing </script> — the HTML parser uses case-insensitive match.
            let content_lower = html[content_start..].to_ascii_lowercase();
            let Some(close_rel) = content_lower.find("</script") else { break };
            let content = &html[content_start..content_start + close_rel];

            // Assert no nested </script (any case) inside this block's content.
            let inner = content.to_ascii_lowercase().find("</script");
            assert!(
                inner.is_none(),
                "script block #{block_count} contains inner </script at offset {}:\n…{}…",
                inner.unwrap(),
                &content[inner.unwrap().saturating_sub(60)
                    ..std::cmp::min(content.len(), inner.unwrap() + 30)],
            );

            block_count += 1;
            pos = content_start + close_rel + 9; // advance past </script>
        }
        assert!(block_count >= 2, "expected at least 2 script blocks, got {block_count}");
    }

    #[test]
    fn test_template_source_files_contain_no_script_close_tag() {
        // template.js and template.css must not contain the literal string
        // </script (any case). If they do, the string will appear verbatim in
        // the rendered HTML inside a <script> block and terminate it early.
        let template_js = TEMPLATE_JS;
        let template_css = TEMPLATE_CSS;
        assert!(
            template_js.to_ascii_lowercase().find("</script").is_none(),
            "template.js contains </script — this will break the rendered HTML"
        );
        assert!(
            template_css.to_ascii_lowercase().find("</script").is_none(),
            "template.css contains </script — this will break the rendered HTML"
        );
    }

    #[test]
    fn test_session_data_base64_is_pure_base64_alphabet() {
        // The session data is embedded as var __SESSION_B64__='...'; — the value
        // must contain only base64 alphabet characters so the JS string literal
        // is never broken by quotes, backslashes, or HTML-special characters.
        let entries = vec![
            user_entry("content with 'single quotes', \"double\", backslash \\, and </html>"),
            tool_result_entry("c1", "multi\nline\noutput\twith\ttabs", None),
        ];
        let html = render_html(&entries);
        let marker = "var __SESSION_B64__='";
        let start = html.find(marker).expect("marker not found") + marker.len();
        let end = html[start..].find("';").expect("closing ';") + start;
        let b64 = &html[start..end];
        assert!(!b64.is_empty(), "base64 data must not be empty");
        for (i, ch) in b64.chars().enumerate() {
            assert!(
                matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' | '='),
                "non-base64 char {ch:?} at position {i}"
            );
        }
        // Also verify it decodes to valid JSON with expected entries.
        let json = extract_session_json(&html);
        assert_eq!(json["entries"].as_array().unwrap().len(), 2);
    }
}
