use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::types::*;
use crate::agent::types::{AgentMessage, ContentBlock, ContentItem, ThinkingLevel};

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

        // Migration: add worktree column
        db.execute("ALTER TABLE sessions ADD COLUMN worktree TEXT").ok();
        // Migration: add name column for auto-generated session titles
        db.execute("ALTER TABLE sessions ADD COLUMN name TEXT").ok();
        // Migration: per-session auto-compact threshold (fraction 0.0–1.0)
        db.execute("ALTER TABLE sessions ADD COLUMN compact_threshold REAL").ok();
        // Migration: stable repo fingerprint (SHA of initial commit) for rename-safe filtering
        db.execute("ALTER TABLE sessions ADD COLUMN repo_id TEXT").ok();

        db.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(
                text,
                session_id UNINDEXED,
                entry_id UNINDEXED,
                role UNINDEXED,
                tokenize = 'porter unicode61'
            )",
        )
        .expect("failed to create search_index FTS5 table");

        backfill_search_index(&db);

        Self {
            db,
            session_id: None,
            entries: Vec::new(),
            by_id: HashMap::new(),
            leaf_id: None,
            next_seq: 0,
        }
    }

    pub fn new_session(&mut self, cwd: &Path, worktree: Option<&Path>) -> anyhow::Result<()> {
        let id = gen_session_id();
        let now = now_iso();
        let cwd_str = cwd.to_string_lossy().to_string();

        // Compute stable repo fingerprint from the initial git commit SHA.
        // NULL for non-git directories — cwd-prefix fallback still works for them.
        let repo_id = crate::find_repo_root(cwd)
            .and_then(|root| crate::repo_fingerprint(&root));

        let mut stmt = self.db.prepare(
            "INSERT INTO sessions (id, cwd, created_at, updated_at, worktree, repo_id) VALUES (?, ?, ?, ?, ?, ?)",
        )?;
        stmt.bind((1, id.as_str()))?;
        stmt.bind((2, cwd_str.as_str()))?;
        stmt.bind((3, now.as_str()))?;
        stmt.bind((4, now.as_str()))?;
        match worktree {
            Some(wt) => stmt.bind((5, wt.to_string_lossy().as_ref()))?,
            None => stmt.bind((5, sqlite::Value::Null))?,
        };
        match repo_id.as_deref() {
            Some(rid) => stmt.bind((6, rid))?,
            None => stmt.bind((6, sqlite::Value::Null))?,
        };
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
                    self.entries.push(entry);
                    self.next_seq = seq + 1;
                }
                Err(e) => {
                    crate::log::warn(&format!("skipping malformed entry: {}", e));
                }
            }
        }

        // Find the actual latest leaf: highest-seq entry with no children
        self.leaf_id = self.find_latest_leaf();

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

        self.db.execute("BEGIN")?;

        let result = self.append_entry_inner(&session_id, &entry_id, parent_id.as_deref(), &data, &entry);
        if result.is_err() {
            self.db.execute("ROLLBACK").ok();
            return result;
        }

        self.db.execute("COMMIT")?;

        let idx = self.entries.len();
        self.by_id.insert(entry.id().to_string(), idx);
        self.leaf_id = Some(entry.id().to_string());
        self.next_seq += 1;
        self.entries.push(entry);

        Ok(())
    }

    fn append_entry_inner(
        &self,
        session_id: &str,
        entry_id: &str,
        parent_id: Option<&str>,
        data: &str,
        entry: &SessionEntry,
    ) -> anyhow::Result<()> {
        let mut stmt = self.db.prepare(
            "INSERT INTO entries (id, session_id, parent_id, seq, data) VALUES (?, ?, ?, ?, ?)",
        )?;
        stmt.bind((1, entry_id))?;
        stmt.bind((2, session_id))?;
        match parent_id {
            Some(p) => stmt.bind((3, p))?,
            None => stmt.bind((3, sqlite::Value::Null))?,
        };
        stmt.bind((4, self.next_seq))?;
        stmt.bind((5, data))?;
        stmt.next()?;

        // Index searchable text in FTS5
        if let Some((text, role)) = extract_searchable_text(entry) {
            if !text.is_empty() {
                let mut fts_stmt = self.db.prepare(
                    "INSERT INTO search_index (text, session_id, entry_id, role) VALUES (?, ?, ?, ?)",
                )?;
                fts_stmt.bind((1, text.as_str()))?;
                fts_stmt.bind((2, session_id))?;
                fts_stmt.bind((3, entry_id))?;
                fts_stmt.bind((4, role))?;
                fts_stmt.next()?;
            }
        }

        // Update session timestamp
        let now = now_iso();
        let mut stmt = self
            .db
            .prepare("UPDATE sessions SET updated_at = ? WHERE id = ?")?;
        stmt.bind((1, now.as_str()))?;
        stmt.bind((2, session_id))?;
        stmt.next()?;

        // Update preview from first user message
        if self.entries.is_empty()
            && let SessionEntry::Message(me) = entry
            && let AgentMessage::User { content, .. } = &me.message
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
            stmt.bind((2, session_id))?;
            stmt.next()?;
        }

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
        if self.session_id.is_none() {
            anyhow::bail!("no active session");
        }

        // Collect the IDs of branch ancestors that fall before the cut point.
        // We only delete entries on the current branch (root → leaf), not siblings.
        // take_while stops before first_kept_entry_id, so that entry and everything
        // after it (on any branch) is preserved.
        let branch_ids_to_delete: Vec<String> = self
            .get_branch()
            .iter()
            .map(|e| e.id().to_string())
            .take_while(|id| id != &first_kept_entry_id)
            .collect();

        if !branch_ids_to_delete.is_empty() {
            // Build a parameterised IN list
            let placeholders = branch_ids_to_delete
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",");

            let fts_sql = format!(
                "DELETE FROM search_index WHERE entry_id IN ({})",
                placeholders
            );
            let mut stmt = self.db.prepare(&fts_sql)?;
            for (i, id) in branch_ids_to_delete.iter().enumerate() {
                stmt.bind((i + 1, id.as_str()))?;
            }
            stmt.next()?;

            let del_sql = format!("DELETE FROM entries WHERE id IN ({})", placeholders);
            let mut stmt = self.db.prepare(&del_sql)?;
            for (i, id) in branch_ids_to_delete.iter().enumerate() {
                stmt.bind((i + 1, id.as_str()))?;
            }
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
                self.entries.push(entry);
                self.next_seq = seq + 1;
            }
        }

        self.leaf_id = self.find_latest_leaf();

        Ok(())
    }

    pub fn get_branch(&self) -> Vec<&SessionEntry> {
        let Some(ref leaf) = self.leaf_id else {
            return Vec::new();
        };
        let mut path = Vec::new();
        let mut current_id: Option<&str> = Some(leaf);
        while let Some(id) = current_id {
            if let Some(&idx) = self.by_id.get(id) {
                let entry = &self.entries[idx];
                path.push(entry);
                current_id = entry.parent_id();
            } else {
                break;
            }
        }
        path.reverse();
        path
    }

    /// Get current branch entries as owned values (for use in AgentSession)
    pub fn current_branch_entries(&self) -> Vec<SessionEntry> {
        self.get_branch().iter().map(|e| (*e).clone()).collect()
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

    /// Update the worktree and cwd for the current session.
    pub fn update_worktree(&self, cwd: &Path, worktree: &Path) {
        if let Some(ref sid) = self.session_id {
            if let Ok(mut stmt) = self
                .db
                .prepare("UPDATE sessions SET cwd = ?, worktree = ? WHERE id = ?")
            {
                stmt.bind((1, cwd.to_string_lossy().as_ref())).ok();
                stmt.bind((2, worktree.to_string_lossy().as_ref())).ok();
                stmt.bind((3, sid.as_str())).ok();
                stmt.next().ok();
            }
        }
    }

    /// Set the human-readable name for the current session.
    pub fn set_name(&self, name: &str) {
        if let Some(ref sid) = self.session_id {
            if let Ok(mut stmt) = self
                .db
                .prepare("UPDATE sessions SET name = ? WHERE id = ?")
            {
                stmt.bind((1, name)).ok();
                stmt.bind((2, sid.as_str())).ok();
                stmt.next().ok();
            }
        }
    }

    /// Return the current name (if any) for the active session.
    pub fn name(&self) -> Option<String> {
        let sid = self.session_id.as_deref()?;
        let mut stmt = self
            .db
            .prepare("SELECT name FROM sessions WHERE id = ?")
            .ok()?;
        stmt.bind((1, sid)).ok()?;
        if stmt.next().ok()? == sqlite::State::Row {
            stmt.read::<Option<String>, _>("name").ok().flatten()
        } else {
            None
        }
    }

    /// Get the auto-compact threshold (fraction 0.0–1.0) for the current session, if set.
    pub fn get_compact_threshold(&self) -> Option<f64> {
        let sid = self.session_id.as_deref()?;
        let mut stmt = self
            .db
            .prepare("SELECT compact_threshold FROM sessions WHERE id = ?")
            .ok()?;
        stmt.bind((1, sid)).ok()?;
        if stmt.next().ok()? == sqlite::State::Row {
            stmt.read::<Option<f64>, _>("compact_threshold").ok().flatten()
        } else {
            None
        }
    }

    /// Persist the auto-compact threshold for the current session.
    pub fn set_compact_threshold(&self, pct: f64) {
        if let Some(ref sid) = self.session_id {
            if let Ok(mut stmt) = self
                .db
                .prepare("UPDATE sessions SET compact_threshold = ? WHERE id = ?")
            {
                stmt.bind((1, pct)).ok();
                stmt.bind((2, sid.as_str())).ok();
                stmt.next().ok();
            }
        }
    }

    /// Clear the worktree association for the current session.
    pub fn clear_worktree(&self) {
        if let Some(ref sid) = self.session_id {
            if let Ok(mut stmt) = self
                .db
                .prepare("UPDATE sessions SET worktree = NULL WHERE id = ?")
            {
                stmt.bind((1, sid.as_str())).ok();
                stmt.next().ok();
            }
        }
    }

    /// Get the worktree path for the current session, if any.
    pub fn session_worktree(&self) -> Option<std::path::PathBuf> {
        let sid = self.session_id.as_ref()?;
        let mut stmt = self
            .db
            .prepare("SELECT worktree FROM sessions WHERE id = ?")
            .ok()?;
        stmt.bind((1, sid.as_str())).ok()?;
        if stmt.next().ok()? == sqlite::State::Row {
            let wt: String = stmt.read("worktree").ok()?;
            if wt.is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(wt))
            }
        } else {
            None
        }
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }

    /// Move the leaf pointer to an earlier entry, forking on next append.
    pub fn branch(&mut self, from_id: &str) {
        if self.by_id.contains_key(from_id) {
            self.leaf_id = Some(from_id.to_string());
        }
    }

    /// Reset leaf to None (next append creates a new root).
    pub fn reset_leaf(&mut self) {
        self.leaf_id = None;
    }

    /// Find the latest leaf: highest-seq entry that has no children.
    fn find_latest_leaf(&self) -> Option<String> {
        let children_of: HashSet<&str> = self
            .entries
            .iter()
            .filter_map(|e| e.parent_id())
            .collect();

        // Walk entries in reverse (highest seq first), pick first with no children
        self.entries
            .iter()
            .rev()
            .find(|e| !children_of.contains(e.id()))
            .map(|e| e.id().to_string())
    }

    /// True if the session has any branch points (entries with >1 child).
    pub fn has_branches(&self) -> bool {
        let mut child_count: HashMap<&str, u32> = HashMap::new();
        for entry in &self.entries {
            if let Some(pid) = entry.parent_id() {
                *child_count.entry(pid).or_default() += 1;
            }
        }
        child_count.values().any(|&c| c > 1)
    }

    /// Build tree structure from entries for the tree selector UI.
    pub fn get_tree(&self) -> Vec<SessionTreeNode> {
        let mut nodes: HashMap<&str, SessionTreeNode> = HashMap::new();
        let mut roots: Vec<&str> = Vec::new();

        // Create nodes
        for entry in &self.entries {
            let (entry_type, summary, raw_text, is_user, has_tool_calls) = summarize_entry(entry);
            nodes.insert(
                entry.id(),
                SessionTreeNode {
                    entry_id: entry.id().to_string(),
                    entry_type,
                    summary,
                    raw_text,
                    timestamp: entry_timestamp(entry),
                    children: Vec::new(),
                    is_user,
                    has_tool_calls,
                },
            );
        }

        // Build parent→children relationships
        // We need to collect child IDs first, then move nodes out
        let mut children_map: HashMap<&str, Vec<&str>> = HashMap::new();
        for entry in &self.entries {
            match entry.parent_id() {
                Some(pid) if nodes.contains_key(pid) => {
                    children_map.entry(pid).or_default().push(entry.id());
                }
                _ => roots.push(entry.id()),
            }
        }

        // Recursive builder — take ownership of nodes from the map
        fn build(
            id: &str,
            nodes: &mut HashMap<&str, SessionTreeNode>,
            children_map: &HashMap<&str, Vec<&str>>,
        ) -> Option<SessionTreeNode> {
            let mut node = nodes.remove(id)?;
            if let Some(child_ids) = children_map.get(id) {
                for &cid in child_ids {
                    if let Some(child) = build(cid, nodes, children_map) {
                        node.children.push(child);
                    }
                }
                // Sort children by timestamp
                node.children.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
            }
            Some(node)
        }

        let mut tree = Vec::new();
        for &rid in &roots {
            if let Some(node) = build(rid, &mut nodes, &children_map) {
                tree.push(node);
            }
        }
        tree.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        tree
    }

    /// Return the `repo_id` (initial-commit SHA) for the current active session, if any.
    pub fn repo_id(&self) -> Option<String> {
        let sid = self.session_id.as_deref()?;
        let mut stmt = self
            .db
            .prepare("SELECT repo_id FROM sessions WHERE id = ?")
            .ok()?;
        stmt.bind((1, sid)).ok()?;
        if stmt.next().ok()? == sqlite::State::Row {
            stmt.read::<Option<String>, _>("repo_id").ok().flatten()
        } else {
            None
        }
    }

    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let mut stmt = match self.db.prepare(
            "SELECT id, cwd, created_at, updated_at, preview, name, repo_id FROM sessions ORDER BY updated_at DESC LIMIT 100",
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
            let name: Option<String> = stmt.read("name").unwrap_or(None);
            let repo_id: Option<String> = stmt.read("repo_id").unwrap_or(None);

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
                name,
                repo_id,
                message_count,
                modified,
            });
        }
        sessions
    }

    pub fn search_sessions(&self, query: &str) -> Vec<SearchResult> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }

        // Sanitize for FTS5: quote each token to avoid syntax errors from special chars
        let fts_query: String = query
            .split_whitespace()
            .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");

        let mut stmt = match self.db.prepare(
            "SELECT
                snippet(search_index, 0, '<<HL>>', '<</HL>>', '…', 20) as excerpt,
                session_id,
                role,
                rank
            FROM search_index
            WHERE text MATCH ?
            ORDER BY rank
            LIMIT 100",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        if stmt.bind((1, fts_query.as_str())).is_err() {
            return Vec::new();
        }

        // Collect raw hits, dedup by session_id keeping best rank
        let mut best: std::collections::HashMap<String, (String, String, f64)> =
            std::collections::HashMap::new();
        while stmt.next().unwrap_or(sqlite::State::Done) == sqlite::State::Row {
            let excerpt: String = stmt.read("excerpt").unwrap_or_default();
            let session_id: String = stmt.read("session_id").unwrap_or_default();
            let role: String = stmt.read("role").unwrap_or_default();
            let rank: f64 = stmt.read("rank").unwrap_or(0.0);
            best.entry(session_id)
                .and_modify(|existing| {
                    if rank < existing.2 {
                        *existing = (excerpt.clone(), role.clone(), rank);
                    }
                })
                .or_insert((excerpt, role, rank));
        }

        // Join with sessions table for metadata
        let mut results: Vec<SearchResult> = Vec::new();
        for (sid, (excerpt, role, rank)) in &best {
            let mut s = match self
                .db
                .prepare("SELECT id, cwd, updated_at FROM sessions WHERE id = ?")
            {
                Ok(s) => s,
                Err(_) => continue,
            };
            if s.bind((1, sid.as_str())).is_err() {
                continue;
            }
            if s.next().unwrap_or(sqlite::State::Done) != sqlite::State::Row {
                continue;
            }
            let id: String = s.read("id").unwrap_or_default();
            let cwd: String = s.read("cwd").unwrap_or_default();
            let updated_at: String = s.read("updated_at").unwrap_or_default();
            let id_short = if id.len() >= 8 {
                id[..8].to_string()
            } else {
                id.clone()
            };
            let message_count = self.count_entries(&id);
            let modified = parse_iso_to_system_time(&updated_at);
            results.push(SearchResult {
                session_id: id,
                id_short,
                excerpt: excerpt.clone(),
                role: role.clone(),
                cwd,
                modified,
                message_count,
                rank: *rank,
            });
        }

        results.sort_by(|a, b| a.rank.partial_cmp(&b.rank).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(20);
        results
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

    /// Export the current branch (leaf → root) as JSONL lines.
    ///
    /// Walks the parent_id chain from the current leaf back to the root so only
    /// the linear history for this branch is included, not sibling branches.
    pub fn export_jsonl(&self) -> Option<String> {
        let session_id = self.session_id.as_ref()?;
        let leaf_id = self.leaf_id.as_deref()?;
        let branch = self.get_branch();
        let mut lines = Vec::new();

        // Header line — include leaf_id so the file is unambiguous.
        let header = serde_json::json!({
            "type": "session",
            "version": CURRENT_SESSION_VERSION,
            "id": session_id,
            "leaf_id": leaf_id,
            "timestamp": now_iso(),
            "cwd": "",
        });
        lines.push(serde_json::to_string(&header).ok()?);

        // Serialize each entry in branch order (root → leaf).
        for entry in &branch {
            if let Ok(data) = serde_json::to_string(entry) {
                lines.push(data);
            }
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

/// Extract type string, summary text, raw_text, is_user, has_tool_calls from an entry.
fn summarize_entry(entry: &SessionEntry) -> (String, String, String, bool, bool) {
    match entry {
        SessionEntry::Message(e) => match &e.message {
            AgentMessage::User { content, .. } => {
                let text = content_text(content);
                ("message".into(), truncate(&text, 80), text, true, false)
            }
            AgentMessage::Assistant(a) => {
                let text = a.text_content();
                let has_tools = a
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolCall { .. }));
                ("message".into(), truncate(&text, 80), String::new(), false, has_tools)
            }
            AgentMessage::ToolResult { .. } => {
                ("tool_result".into(), String::new(), String::new(), false, false)
            }
            _ => ("message".into(), String::new(), String::new(), false, false),
        },
        SessionEntry::Compaction(c) => {
            ("compaction".into(), truncate(&c.summary, 60), String::new(), false, false)
        }
        SessionEntry::BranchSummary(b) => {
            ("branch_summary".into(), truncate(&b.summary, 60), String::new(), false, false)
        }
        SessionEntry::ModelChange(m) => {
            ("model_change".into(), m.model_id.clone(), String::new(), false, false)
        }
        SessionEntry::ThinkingLevelChange(t) => {
            ("thinking_change".into(), t.thinking_level.clone(), String::new(), false, false)
        }
        SessionEntry::Label(l) => ("label".into(), l.label.clone(), String::new(), false, false),
        SessionEntry::SessionInfo(s) => {
            ("session_info".into(), s.name.clone().unwrap_or_default(), String::new(), false, false)
        }
        SessionEntry::SystemPrompt(_) => ("system_prompt".into(), String::new(), String::new(), false, false),
        SessionEntry::CustomMessage(_) => ("custom_message".into(), String::new(), String::new(), false, false),
        SessionEntry::PermissionAccept(p) => (
            "permission_accept".into(),
            format!("{}({})", p.tool, truncate(&p.args, 40)),
            String::new(),
            false,
            false,
        ),
    }
}

fn entry_timestamp(entry: &SessionEntry) -> String {
    match entry {
        SessionEntry::Message(e) => e.timestamp.clone(),
        SessionEntry::Compaction(e) => e.timestamp.clone(),
        SessionEntry::BranchSummary(e) => e.timestamp.clone(),
        SessionEntry::ModelChange(e) => e.timestamp.clone(),
        SessionEntry::ThinkingLevelChange(e) => e.timestamp.clone(),
        SessionEntry::Label(e) => e.timestamp.clone(),
        SessionEntry::SessionInfo(e) => e.timestamp.clone(),
        SessionEntry::SystemPrompt(e) => e.timestamp.clone(),
        SessionEntry::CustomMessage(e) => e.timestamp.clone(),
        SessionEntry::PermissionAccept(e) => e.timestamp.clone(),
    }
}

fn content_text(items: &[ContentItem]) -> String {
    items
        .iter()
        .filter_map(|c| match c {
            ContentItem::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    // Take first line only
    let line = s.lines().next().unwrap_or("");
    if line.len() <= max {
        line.to_string()
    } else {
        format!("{}...", &line[..max - 3])
    }
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id_short: String,
    pub timestamp: String,
    pub cwd: String,
    pub preview: String,
    /// Auto-generated session title (set after the first completed turn).
    pub name: Option<String>,
    /// Stable repo fingerprint (SHA of initial commit). None for legacy rows.
    pub repo_id: Option<String>,
    pub message_count: u32,
    pub modified: std::time::SystemTime,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub session_id: String,
    pub id_short: String,
    pub excerpt: String,
    pub role: String,
    pub cwd: String,
    pub modified: std::time::SystemTime,
    pub message_count: u32,
    rank: f64,
}

fn extract_searchable_text(entry: &SessionEntry) -> Option<(String, &'static str)> {
    match entry {
        SessionEntry::Message(e) => match &e.message {
            AgentMessage::User { content, .. } => Some((content_text(content), "user")),
            AgentMessage::Assistant(a) => Some((a.text_content(), "assistant")),
            AgentMessage::ToolResult { content, .. } => {
                let text = content_text(content);
                if text.len() < 2000 {
                    Some((text, "tool_result"))
                } else {
                    None
                }
            }
            _ => None,
        },
        _ => None,
    }
}

fn backfill_search_index(db: &sqlite::Connection) {
    let has_fts: i64 = db
        .prepare("SELECT COUNT(*) as c FROM search_index")
        .and_then(|mut s| {
            s.next()?;
            s.read::<i64, _>("c")
        })
        .unwrap_or(0);
    let has_entries: i64 = db
        .prepare("SELECT COUNT(*) as c FROM entries")
        .and_then(|mut s| {
            s.next()?;
            s.read::<i64, _>("c")
        })
        .unwrap_or(0);

    if has_fts > 0 || has_entries == 0 {
        return;
    }

    db.execute("BEGIN").ok();
    let mut stmt = match db.prepare("SELECT id, session_id, data FROM entries ORDER BY session_id, seq") {
        Ok(s) => s,
        Err(_) => return,
    };
    while stmt.next().unwrap_or(sqlite::State::Done) == sqlite::State::Row {
        let entry_id: String = stmt.read("id").unwrap_or_default();
        let session_id: String = stmt.read("session_id").unwrap_or_default();
        let data: String = stmt.read("data").unwrap_or_default();
        if let Ok(entry) = serde_json::from_str::<SessionEntry>(&data) {
            if let Some((text, role)) = extract_searchable_text(&entry) {
                if !text.is_empty() {
                    if let Ok(mut fts_stmt) = db.prepare(
                        "INSERT INTO search_index (text, session_id, entry_id, role) VALUES (?, ?, ?, ?)",
                    ) {
                        fts_stmt.bind((1, text.as_str())).ok();
                        fts_stmt.bind((2, session_id.as_str())).ok();
                        fts_stmt.bind((3, entry_id.as_str())).ok();
                        fts_stmt.bind((4, role)).ok();
                        fts_stmt.next().ok();
                    }
                }
            }
        }
    }
    db.execute("COMMIT").ok();
}
