use std::collections::HashMap;
use std::path::Path;

use super::types::*;
use crate::agent::types::{AgentMessage, ContentItem, ThinkingLevel};

pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub model: Option<(String, String)>,
    pub thinking_level: ThinkingLevel,
}

pub struct SessionManager {
    db: sqlite::Connection,
    session_id: Option<String>,
    entries: Vec<SessionEntry>,
    by_id: HashMap<String, usize>,
    leaf_id: Option<String>,
    next_seq: i64,
}

impl SessionManager {
    pub fn new(nerv_dir: &Path) -> Self {
        let db_path = nerv_dir.join("sessions.db");
        let db = sqlite::open(&db_path).expect("failed to open sessions.db");

        db.execute("PRAGMA journal_mode=WAL").ok();
        db.execute("PRAGMA synchronous=NORMAL").ok();

        db.execute(
            "CREATE TABLE IF NOT EXISTS sessions (
                id         TEXT PRIMARY KEY,
                cwd        TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                preview    TEXT NOT NULL DEFAULT ''
            )",
        )
        .expect("failed to create sessions table");

        db.execute(
            "CREATE TABLE IF NOT EXISTS entries (
                id         TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                parent_id  TEXT,
                seq        INTEGER NOT NULL,
                data       TEXT NOT NULL
            )",
        )
        .expect("failed to create entries table");

        db.execute("CREATE INDEX IF NOT EXISTS idx_entries_session ON entries(session_id, seq)")
            .ok();

        Self {
            db,
            session_id: None,
            entries: Vec::new(),
            by_id: HashMap::new(),
            leaf_id: None,
            next_seq: 0,
        }
    }

    pub fn new_session(&mut self, cwd: &Path) -> anyhow::Result<()> {
        let id = gen_session_id();
        let now = now_iso();
        let cwd_str = cwd.to_string_lossy().to_string();

        let mut stmt = self.db.prepare(
            "INSERT INTO sessions (id, cwd, created_at, updated_at) VALUES (?, ?, ?, ?)",
        )?;
        stmt.bind((1, id.as_str()))?;
        stmt.bind((2, cwd_str.as_str()))?;
        stmt.bind((3, now.as_str()))?;
        stmt.bind((4, now.as_str()))?;
        stmt.next()?;

        self.session_id = Some(id);
        self.entries.clear();
        self.by_id.clear();
        self.leaf_id = None;
        self.next_seq = 0;

        Ok(())
    }

    pub fn load_session(&mut self, session_id: &str) -> anyhow::Result<SessionContext> {
        // Find full session ID from prefix
        let full_id = {
            let mut stmt = self
                .db
                .prepare("SELECT id FROM sessions WHERE id LIKE ? LIMIT 1")?;
            stmt.bind((1, format!("{}%", session_id).as_str()))?;
            if stmt.next()? == sqlite::State::Row {
                stmt.read::<String, _>("id")?
            } else {
                anyhow::bail!("session not found: {}", session_id);
            }
        };

        self.session_id = Some(full_id.clone());
        self.entries.clear();
        self.by_id.clear();
        self.leaf_id = None;
        self.next_seq = 0;

        let mut stmt = self.db.prepare(
            "SELECT id, parent_id, seq, data FROM entries WHERE session_id = ? ORDER BY seq",
        )?;
        stmt.bind((1, full_id.as_str()))?;

        while stmt.next()? == sqlite::State::Row {
            let data: String = stmt.read("data")?;
            let seq: i64 = stmt.read("seq")?;

            match serde_json::from_str::<SessionEntry>(&data) {
                Ok(entry) => {
                    let idx = self.entries.len();
                    self.by_id.insert(entry.id().to_string(), idx);
                    self.leaf_id = Some(entry.id().to_string());
                    self.entries.push(entry);
                    self.next_seq = seq + 1;
                }
                Err(e) => {
                    crate::log::warn(&format!("skipping malformed entry: {}", e));
                }
            }
        }

        Ok(self.build_session_context())
    }

    pub fn append_entry(&mut self, entry: SessionEntry) -> anyhow::Result<()> {
        let session_id = self
            .session_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no active session"))?
            .clone();

        let data = serde_json::to_string(&entry)?;
        let entry_id = entry.id().to_string();
        let parent_id = entry.parent_id().map(|s| s.to_string());

        let mut stmt = self.db.prepare(
            "INSERT INTO entries (id, session_id, parent_id, seq, data) VALUES (?, ?, ?, ?, ?)",
        )?;
        stmt.bind((1, entry_id.as_str()))?;
        stmt.bind((2, session_id.as_str()))?;
        match parent_id.as_deref() {
            Some(p) => stmt.bind((3, p))?,
            None => stmt.bind((3, sqlite::Value::Null))?,
        };
        stmt.bind((4, self.next_seq))?;
        stmt.bind((5, data.as_str()))?;
        stmt.next()?;

        // Update session timestamp
        let now = now_iso();
        let mut stmt = self
            .db
            .prepare("UPDATE sessions SET updated_at = ? WHERE id = ?")?;
        stmt.bind((1, now.as_str()))?;
        stmt.bind((2, session_id.as_str()))?;
        stmt.next()?;

        // Update preview from first user message
        if self.entries.is_empty()
            && let SessionEntry::Message(ref me) = entry
            && let AgentMessage::User { ref content, .. } = me.message
        {
            let preview: String = content
                .iter()
                .filter_map(|c| match c {
                    ContentItem::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
                .chars()
                .take(80)
                .collect();
            let mut stmt = self
                .db
                .prepare("UPDATE sessions SET preview = ? WHERE id = ?")?;
            stmt.bind((1, preview.as_str()))?;
            stmt.bind((2, session_id.as_str()))?;
            stmt.next()?;
        }

        let idx = self.entries.len();
        self.by_id.insert(entry.id().to_string(), idx);
        self.leaf_id = Some(entry.id().to_string());
        self.next_seq += 1;
        self.entries.push(entry);

        Ok(())
    }

    fn next_id(&self) -> String {
        loop {
            let id = gen_entry_id();
            if !self.by_id.contains_key(&id) {
                return id;
            }
        }
    }

    pub fn append_message(
        &mut self,
        msg: &AgentMessage,
        tokens: Option<TokenInfo>,
    ) -> anyhow::Result<()> {
        let entry = SessionEntry::Message(MessageEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso(),
            message: msg.clone(),
            tokens,
        });
        self.append_entry(entry)
    }

    pub fn append_compaction(
        &mut self,
        summary: String,
        first_kept_entry_id: String,
        tokens_before: u32,
    ) -> anyhow::Result<()> {
        let session_id = self
            .session_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no active session"))?
            .clone();

        // Find the seq of the first kept entry
        let kept_seq = {
            let mut stmt = self
                .db
                .prepare("SELECT seq FROM entries WHERE session_id = ? AND id = ?")?;
            stmt.bind((1, session_id.as_str()))?;
            stmt.bind((2, first_kept_entry_id.as_str()))?;
            if stmt.next()? == sqlite::State::Row {
                stmt.read::<i64, _>("seq")?
            } else {
                0
            }
        };

        // Delete entries before the cut point
        {
            let mut stmt = self
                .db
                .prepare("DELETE FROM entries WHERE session_id = ? AND seq < ?")?;
            stmt.bind((1, session_id.as_str()))?;
            stmt.bind((2, kept_seq))?;
            stmt.next()?;
        }

        // Append compaction entry
        let entry = SessionEntry::Compaction(CompactionEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso(),
            summary,
            first_kept_entry_id,
            tokens_before,
        });
        self.append_entry(entry)?;

        // Reload in-memory cache
        self.reload_entries()?;

        Ok(())
    }

    pub fn append_model_change(&mut self, provider: &str, model_id: &str) -> anyhow::Result<()> {
        let entry = SessionEntry::ModelChange(ModelChangeEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso(),
            provider: provider.to_string(),
            model_id: model_id.to_string(),
        });
        self.append_entry(entry)
    }

    pub fn append_thinking_level_change(&mut self, level: ThinkingLevel) -> anyhow::Result<()> {
        let level_str = serde_json::to_value(level)?
            .as_str()
            .unwrap_or("off")
            .to_string();
        let entry = SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso(),
            thinking_level: level_str,
        });
        self.append_entry(entry)
    }

    fn reload_entries(&mut self) -> anyhow::Result<()> {
        let session_id = self
            .session_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no active session"))?
            .clone();

        self.entries.clear();
        self.by_id.clear();
        self.leaf_id = None;
        self.next_seq = 0;

        let mut stmt = self
            .db
            .prepare("SELECT seq, data FROM entries WHERE session_id = ? ORDER BY seq")?;
        stmt.bind((1, session_id.as_str()))?;

        while stmt.next()? == sqlite::State::Row {
            let data: String = stmt.read("data")?;
            let seq: i64 = stmt.read("seq")?;

            if let Ok(entry) = serde_json::from_str::<SessionEntry>(&data) {
                let idx = self.entries.len();
                self.by_id.insert(entry.id().to_string(), idx);
                self.leaf_id = Some(entry.id().to_string());
                self.entries.push(entry);
                self.next_seq = seq + 1;
            }
        }

        Ok(())
    }

    pub fn get_branch(&self) -> Vec<&SessionEntry> {
        // With linear sessions (no branching), the branch is just all entries in order
        self.entries.iter().collect()
    }

    pub fn build_session_context(&self) -> SessionContext {
        let branch = self.get_branch();
        let mut messages = Vec::new();
        let mut model: Option<(String, String)> = None;
        let mut thinking_level = ThinkingLevel::Off;

        let last_compact = branch.iter().enumerate().rev().find_map(|(i, e)| match e {
            SessionEntry::Compaction(c) => Some((i, c)),
            _ => None,
        });

        let walk_start = if let Some((compact_idx, ce)) = last_compact {
            messages.push(AgentMessage::CompactionSummary {
                summary: ce.summary.clone(),
                tokens_before: ce.tokens_before,
                timestamp: crate::agent::types::now_millis(),
            });
            branch
                .iter()
                .position(|e| e.id() == ce.first_kept_entry_id)
                .unwrap_or(compact_idx + 1)
        } else {
            0
        };

        for entry in branch.iter().skip(walk_start) {
            match entry {
                SessionEntry::Message(e) => messages.push(e.message.clone()),
                SessionEntry::Compaction(_) => {}
                SessionEntry::ModelChange(e) => {
                    model = Some((e.provider.clone(), e.model_id.clone()));
                }
                SessionEntry::ThinkingLevelChange(e) => {
                    if let Ok(level) =
                        serde_json::from_value(serde_json::Value::String(e.thinking_level.clone()))
                    {
                        thinking_level = level;
                    }
                }
                SessionEntry::CustomMessage(e) => messages.push(e.message.clone()),
                _ => {}
            }
        }

        SessionContext {
            messages,
            model,
            thinking_level,
        }
    }

    pub fn has_session(&self) -> bool {
        self.session_id.is_some()
    }

    pub fn leaf_id(&self) -> Option<&str> {
        self.leaf_id.as_deref()
    }

    pub fn session_id(&self) -> &str {
        self.session_id.as_deref().unwrap_or("")
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }

    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let mut stmt = match self.db.prepare(
            "SELECT id, cwd, created_at, updated_at, preview FROM sessions ORDER BY updated_at DESC LIMIT 100",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let mut sessions = Vec::new();
        while stmt.next().unwrap_or(sqlite::State::Done) == sqlite::State::Row {
            let id: String = stmt.read("id").unwrap_or_default();
            let cwd: String = stmt.read("cwd").unwrap_or_default();
            let created_at: String = stmt.read("created_at").unwrap_or_default();
            let updated_at: String = stmt.read("updated_at").unwrap_or_default();
            let preview: String = stmt.read("preview").unwrap_or_default();

            // Count entries for this session
            let message_count = self.count_entries(&id);

            let id_short = if id.len() >= 8 {
                id[..8].to_string()
            } else {
                id.clone()
            };

            // Parse updated_at to SystemTime for age display
            let modified = parse_iso_to_system_time(&updated_at);

            sessions.push(SessionSummary {
                id_short,
                timestamp: created_at,
                cwd,
                preview,
                message_count,
                modified,
            });
        }
        sessions
    }

    fn count_entries(&self, session_id: &str) -> u32 {
        let mut stmt = match self
            .db
            .prepare("SELECT COUNT(*) as c FROM entries WHERE session_id = ?")
        {
            Ok(s) => s,
            Err(_) => return 0,
        };
        stmt.bind((1, session_id)).ok();
        if stmt.next().unwrap_or(sqlite::State::Done) == sqlite::State::Row {
            stmt.read::<i64, _>("c").unwrap_or(0) as u32
        } else {
            0
        }
    }

    /// Export all entries for the current session as JSONL lines.
    pub fn export_jsonl(&self) -> Option<String> {
        let session_id = self.session_id.as_ref()?;
        let mut lines = Vec::new();

        // Header line
        let header = serde_json::json!({
            "type": "session",
            "version": CURRENT_SESSION_VERSION,
            "id": session_id,
            "timestamp": now_iso(),
            "cwd": "",
        });
        lines.push(serde_json::to_string(&header).ok()?);

        // Entry lines
        let mut stmt = self
            .db
            .prepare("SELECT data FROM entries WHERE session_id = ? ORDER BY seq")
            .ok()?;
        stmt.bind((1, session_id.as_str())).ok()?;
        while stmt.next().ok()? == sqlite::State::Row {
            let data: String = stmt.read("data").ok()?;
            lines.push(data);
        }

        Some(lines.join("\n"))
    }
}

fn parse_iso_to_system_time(iso: &str) -> std::time::SystemTime {
    // Parse "2026-03-26T01:07:35Z" to SystemTime
    // Simple parser — good enough for our own output
    let parts: Vec<&str> = iso.split('T').collect();
    if parts.len() != 2 {
        return std::time::SystemTime::now();
    }
    let date_parts: Vec<u64> = parts[0].split('-').filter_map(|s| s.parse().ok()).collect();
    let time_str = parts[1].trim_end_matches('Z');
    let time_parts: Vec<u64> = time_str.split(':').filter_map(|s| s.parse().ok()).collect();

    if date_parts.len() != 3 || time_parts.len() != 3 {
        return std::time::SystemTime::now();
    }

    // Days from epoch to date (reuse existing days_to_ymd inverse)
    let (y, m, d) = (date_parts[0], date_parts[1], date_parts[2]);
    let days = ymd_to_days(y, m, d);
    let secs = days * 86400 + time_parts[0] * 3600 + time_parts[1] * 60 + time_parts[2];

    std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
}

fn ymd_to_days(y: u64, m: u64, d: u64) -> u64 {
    let y = if m <= 2 { y - 1 } else { y };
    let m = if m <= 2 { m + 9 } else { m - 3 };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id_short: String,
    pub timestamp: String,
    pub cwd: String,
    pub preview: String,
    pub message_count: u32,
    pub modified: std::time::SystemTime,
}
