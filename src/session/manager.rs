use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{Connection, params};

use super::types::*;
use crate::agent::types::{AgentMessage, ContentBlock, ContentItem, ThinkingLevel};
use crate::str::StrExt as _;

pub struct SessionContext {
    /// Messages for the agent's context window (post-compaction only).
    pub messages: Vec<AgentMessage>,
    /// Full branch history for UI display (includes pre-compaction messages).
    pub full_history: Vec<AgentMessage>,
    pub model: Option<(String, String)>,
    pub thinking_level: ThinkingLevel,
    /// Accumulated cost in USD across all API calls in this session branch.
    pub cost_usd: f64,
    /// Total input tokens sent across all API calls in this session branch.
    pub total_input: u64,
    /// Total output tokens received across all API calls in this session
    /// branch.
    pub total_output: u64,
    /// Number of API calls made in this session branch.
    pub api_calls: u32,
    /// All user-typed prompts ever submitted in this session, oldest first.
    pub input_history: Vec<String>,
    /// Per-session config overrides (model, thinking, effort, auto_compact,
    /// compaction_model).
    pub session_config: SessionConfig,
}

pub struct SessionManager {
    db: Connection,
    session_id: Option<String>,
    entries: Vec<SessionEntry>,
    by_id: HashMap<String, usize>,
    leaf_id: Option<String>,
    next_seq: i64,
}

impl SessionManager {
    pub fn new(repo_dir: &Path) -> Self {
        let _ = std::fs::create_dir_all(repo_dir);
        let db_path = repo_dir.join("sessions.db");
        let db = Connection::open(&db_path).expect("failed to open sessions.db");

        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;").ok();

        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id                TEXT PRIMARY KEY,
                cwd               TEXT NOT NULL,
                created_at        TEXT NOT NULL,
                updated_at        TEXT NOT NULL,
                preview           TEXT NOT NULL DEFAULT '',
                worktree          TEXT,
                name              TEXT,
                compact_threshold REAL,
                repo_id           TEXT,
                input_history     TEXT
            );",
        )
        .expect("failed to create sessions table");

        // Migrate: add input_history column to existing databases that pre-date it.
        db.execute_batch("ALTER TABLE sessions ADD COLUMN input_history TEXT;").ok(); // Ignore error — column already exists on fresh DBs.

        // Migrate: add session_config column for per-session config overrides.
        db.execute_batch("ALTER TABLE sessions ADD COLUMN session_config TEXT;").ok();

        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS entries (
                id         TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                parent_id  TEXT,
                seq        INTEGER NOT NULL,
                data       TEXT NOT NULL
            );",
        )
        .expect("failed to create entries table");

        db.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_entries_session ON entries(session_id, seq);",
        )
        .ok();

        db.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(
                text,
                session_id UNINDEXED,
                entry_id UNINDEXED,
                role UNINDEXED,
                tokenize = 'porter unicode61'
            );",
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
        let repo_id = crate::find_repo_root(cwd).and_then(|root| crate::repo_fingerprint(&root));

        let worktree_str: Option<String> = worktree.map(|wt| wt.to_string_lossy().to_string());

        self.db.execute(
            "INSERT INTO sessions (id, cwd, created_at, updated_at, worktree, repo_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, cwd_str, now, now, worktree_str, repo_id],
        )?;

        self.session_id = Some(id);
        self.entries.clear();
        self.by_id.clear();
        self.leaf_id = None;
        self.next_seq = 0;

        Ok(())
    }

    pub fn load_session(&mut self, session_id: &str) -> anyhow::Result<SessionContext> {
        // Find full session ID from prefix
        let full_id: String = self
            .db
            .query_row(
                "SELECT id FROM sessions WHERE id LIKE ?1 LIMIT 1",
                params![format!("{}%", session_id)],
                |row| row.get(0),
            )
            .map_err(|_| anyhow::anyhow!("session not found: {}", session_id))?;

        self.session_id = Some(full_id.clone());
        self.entries.clear();
        self.by_id.clear();
        self.leaf_id = None;
        self.next_seq = 0;

        let mut stmt = self.db.prepare(
            "SELECT id, parent_id, seq, data FROM entries WHERE session_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![full_id], |row| {
            let id: String = row.get(0)?;
            let _parent_id: Option<String> = row.get(1)?;
            let seq: i64 = row.get(2)?;
            let data: String = row.get(3)?;
            Ok((id, seq, data))
        })?;

        for row in rows {
            let (_id, seq, data) = row?;
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
        let session_id =
            self.session_id.as_ref().ok_or_else(|| anyhow::anyhow!("no active session"))?.clone();

        let data = serde_json::to_string(&entry)?;
        let entry_id = entry.id().to_string();

        let _result = {
            let tx = self.db.transaction()?;
            let r = Self::append_entry_inner_tx(
                &tx,
                &session_id,
                &entry_id,
                &data,
                &entry,
                self.next_seq,
                self.entries.is_empty(),
            );
            if r.is_ok() {
                tx.commit()?;
            } else {
                tx.rollback().ok();
            }
            r
        };

        let idx = self.entries.len();
        self.by_id.insert(entry.id().to_string(), idx);
        self.leaf_id = Some(entry.id().to_string());
        self.next_seq += 1;
        self.entries.push(entry);

        Ok(())
    }

    fn append_entry_inner_tx(
        tx: &rusqlite::Transaction<'_>,
        session_id: &str,
        entry_id: &str,
        data: &str,
        entry: &SessionEntry,
        next_seq: i64,
        is_first: bool,
    ) -> anyhow::Result<()> {
        let parent_id = entry.parent_id().map(|s| s.to_string());
        tx.execute(
            "INSERT INTO entries (id, session_id, parent_id, seq, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![entry_id, session_id, parent_id, next_seq, data],
        )?;

        // Index searchable text in FTS5
        if let Some((text, role)) = extract_searchable_text(entry)
            && !text.is_empty()
        {
            tx.execute(
                "INSERT INTO search_index (text, session_id, entry_id, role) VALUES (?1, ?2, ?3, ?4)",
                params![text, session_id, entry_id, role],
            )?;
        }

        // Update session timestamp (and preview on first message) in one statement
        let now = now_iso();
        if is_first
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
            tx.execute(
                "UPDATE sessions SET updated_at = ?1, preview = ?2 WHERE id = ?3",
                params![now, preview, session_id],
            )?;
            return Ok(());
        }
        tx.execute("UPDATE sessions SET updated_at = ?1 WHERE id = ?2", params![now, session_id])?;

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
        record: CompactionRecord,
    ) -> anyhow::Result<()> {
        if self.session_id.is_none() {
            anyhow::bail!("no active session");
        }

        let CompactionRecord {
            summary,
            first_kept_entry_id,
            tokens_before,
            tokens_after,
            model_id,
            cost_usd_before,
            archived_messages,
        } = record;

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
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 1))
                .collect::<Vec<_>>()
                .join(",");

            let fts_sql = format!("DELETE FROM search_index WHERE entry_id IN ({})", placeholders);
            let mut stmt = self.db.prepare(&fts_sql)?;
            for (i, id) in branch_ids_to_delete.iter().enumerate() {
                stmt.raw_bind_parameter(i + 1, id.as_str())?;
            }
            stmt.raw_execute()?;

            let del_sql = format!("DELETE FROM entries WHERE id IN ({})", placeholders);
            let mut stmt = self.db.prepare(&del_sql)?;
            for (i, id) in branch_ids_to_delete.iter().enumerate() {
                stmt.raw_bind_parameter(i + 1, id.as_str())?;
            }
            stmt.raw_execute()?;
        }

        // Append compaction entry
        let entry = SessionEntry::Compaction(CompactionEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso(),
            summary,
            first_kept_entry_id,
            tokens_before,
            tokens_after,
            model_id,
            cost_usd_before,
            archived_messages,
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

    pub fn append_btw(&mut self, note: &str, response: &str, model_id: &str) -> anyhow::Result<()> {
        use crate::session::types::BtwEntry;
        let entry = SessionEntry::Btw(BtwEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso(),
            note: note.to_string(),
            response: response.to_string(),
            model_id: model_id.to_string(),
        });
        self.append_entry(entry)
    }

    pub fn append_thinking_level_change(&mut self, level: ThinkingLevel) -> anyhow::Result<()> {
        let level_str = serde_json::to_value(level)?.as_str().unwrap_or("off").to_string();
        let entry = SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
            id: self.next_id(),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso(),
            thinking_level: level_str,
        });
        self.append_entry(entry)
    }

    fn reload_entries(&mut self) -> anyhow::Result<()> {
        let session_id =
            self.session_id.as_ref().ok_or_else(|| anyhow::anyhow!("no active session"))?.clone();

        self.entries.clear();
        self.by_id.clear();
        self.leaf_id = None;
        self.next_seq = 0;

        let mut stmt =
            self.db.prepare("SELECT seq, data FROM entries WHERE session_id = ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![session_id], |row| {
            let seq: i64 = row.get(0)?;
            let data: String = row.get(1)?;
            Ok((seq, data))
        })?;

        for row in rows {
            let (seq, data) = row?;
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

        // Find the most recent compaction first so we can seed cost/token totals
        // from its snapshot. Pre-compaction MessageEntry rows are hard-deleted from
        // the DB, so without this offset the accumulated cost would only reflect
        // the post-compaction window.
        let last_compact = branch.iter().enumerate().rev().find_map(|(i, e)| match e {
            SessionEntry::Compaction(c) => Some((i, c)),
            _ => None,
        });

        // Seed accumulated stats from the compaction snapshot, then add all
        // surviving (post-compaction) MessageEntry records on top.
        let mut cost_usd: f64 = last_compact.as_ref().map(|(_, c)| c.cost_usd_before).unwrap_or(0.0);
        let mut total_input: u64 = 0;
        let mut total_output: u64 = 0;
        let mut api_calls: u32 = 0;
        for e in branch.iter() {
            if let SessionEntry::Message(me) = e
                && let Some(t) = &me.tokens
            {
                cost_usd += t.cost_usd;
                total_input += t.input as u64;
                total_output += t.output as u64;
                api_calls += 1;
            }
        }

        let walk_start = if let Some((compact_idx, ce)) = last_compact {
            messages.push(AgentMessage::CompactionSummary {
                summary: ce.summary.clone(),
                tokens_before: ce.tokens_before,
                timestamp: crate::agent::types::now_millis(),
            });
            branch.iter().position(|e| e.id() == ce.first_kept_entry_id).unwrap_or(compact_idx + 1)
        } else {
            0
        };

        // Full history for UI display — all messages from the branch, ordered.
        // Includes pre-compaction messages so the scrollback shows everything.
        let mut full_history: Vec<AgentMessage> = Vec::new();
        for entry in branch.iter() {
            match entry {
                SessionEntry::Message(e) => full_history.push(e.message.clone()),
                SessionEntry::CustomMessage(e) => full_history.push(e.message.clone()),
                _ => {}
            }
        }

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
            full_history,
            model,
            thinking_level,
            cost_usd,
            total_input,
            total_output,
            api_calls,
            input_history: self.load_input_history(),
            session_config: self.get_session_config(),
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
            self.db
                .execute(
                    "UPDATE sessions SET cwd = ?1, worktree = ?2 WHERE id = ?3",
                    params![
                        cwd.to_string_lossy().as_ref(),
                        worktree.to_string_lossy().as_ref(),
                        sid
                    ],
                )
                .ok();
        }
    }

    /// Persist the full input history (all user-typed prompts) for the current
    /// session.
    pub fn save_input_history(&self, history: &[String]) {
        if let Some(ref sid) = self.session_id {
            let json = serde_json::to_string(history).unwrap_or_else(|_| "[]".to_string());
            self.db
                .execute("UPDATE sessions SET input_history = ?1 WHERE id = ?2", params![json, sid])
                .ok();
        }
    }

    /// Load the persisted input history for the current session.
    pub fn load_input_history(&self) -> Vec<String> {
        let sid = match self.session_id.as_deref() {
            Some(s) => s,
            None => return Vec::new(),
        };
        self.db
            .query_row("SELECT input_history FROM sessions WHERE id = ?1", params![sid], |row| {
                row.get::<_, Option<String>>(0)
            })
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
            .unwrap_or_default()
    }

    /// Set the human-readable name for the current session.
    pub fn set_name(&self, name: &str) {
        if let Some(ref sid) = self.session_id {
            self.db.execute("UPDATE sessions SET name = ?1 WHERE id = ?2", params![name, sid]).ok();
        }
    }

    /// Return the current name (if any) for the active session.
    pub fn name(&self) -> Option<String> {
        let sid = self.session_id.as_deref()?;
        self.db
            .query_row("SELECT name FROM sessions WHERE id = ?1", params![sid], |row| {
                row.get::<_, Option<String>>(0)
            })
            .ok()
            .flatten()
    }

    /// Get the auto-compact threshold (fraction 0.0–1.0) for the current
    /// session, if set.
    pub fn get_compact_threshold(&self) -> Option<f64> {
        let sid = self.session_id.as_deref()?;
        self.db
            .query_row(
                "SELECT compact_threshold FROM sessions WHERE id = ?1",
                params![sid],
                |row| row.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten()
    }

    /// Persist the auto-compact threshold for the current session.
    pub fn set_compact_threshold(&self, pct: f64) {
        if let Some(ref sid) = self.session_id {
            self.db
                .execute(
                    "UPDATE sessions SET compact_threshold = ?1 WHERE id = ?2",
                    params![pct, sid],
                )
                .ok();
        }
    }

    /// Load per-session config overrides from the DB.
    pub fn get_session_config(&self) -> SessionConfig {
        let sid = match self.session_id.as_deref() {
            Some(s) => s,
            None => return SessionConfig::default(),
        };
        self.db
            .query_row("SELECT session_config FROM sessions WHERE id = ?1", params![sid], |row| {
                row.get::<_, Option<String>>(0)
            })
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    /// Save per-session config overrides to the DB.
    pub fn set_session_config(&self, config: &SessionConfig) {
        if let Some(ref sid) = self.session_id
            && let Ok(json) = serde_json::to_string(config)
        {
            self.db
                .execute(
                    "UPDATE sessions SET session_config = ?1 WHERE id = ?2",
                    params![json, sid],
                )
                .ok();
        }
    }

    /// Update a single field of the per-session config. Reads current, applies
    /// the closure, writes back. This is safe because the session thread is
    /// the only writer.
    pub fn update_session_config<F: FnOnce(&mut SessionConfig)>(&self, f: F) {
        let mut cfg = self.get_session_config();
        f(&mut cfg);
        self.set_session_config(&cfg);
    }

    /// Clear the worktree association for the current session.
    pub fn clear_worktree(&self) {
        if let Some(ref sid) = self.session_id {
            self.db.execute("UPDATE sessions SET worktree = NULL WHERE id = ?1", params![sid]).ok();
        }
    }

    /// Get the worktree path for the current session, if any.
    pub fn session_worktree(&self) -> Option<std::path::PathBuf> {
        let sid = self.session_id.as_ref()?;
        let wt: Option<String> = self
            .db
            .query_row("SELECT worktree FROM sessions WHERE id = ?1", params![sid], |row| {
                row.get(0)
            })
            .ok()
            .flatten();
        wt.filter(|s| !s.is_empty()).map(std::path::PathBuf::from)
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
        let children_of: HashSet<&str> =
            self.entries.iter().filter_map(|e| e.parent_id()).collect();

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

    /// Return the `repo_id` (initial-commit SHA) for the current active
    /// session, if any. Return true if any session in the DB was started in
    /// a repository identified by `repo_id` (the initial-commit SHA
    /// fingerprint). Copy the current branch (root → leaf) into a brand-new
    /// session and switch to it.
    ///
    /// All entries on the active branch are duplicated into the new session,
    /// preserving their existing `id`, `parent_id`, and `seq` values so the
    /// tree structure is intact.  The `search_index` rows are copied as
    /// well.  Sibling branches from the source session are intentionally
    /// left behind — only the path you are currently on is forked.
    ///
    /// Returns the new session id on success.
    pub fn fork_session(&mut self) -> anyhow::Result<String> {
        let src_id =
            self.session_id.as_ref().ok_or_else(|| anyhow::anyhow!("no active session"))?.clone();

        // Collect the branch entry ids we want to copy.
        let branch_ids: Vec<String> =
            self.get_branch().iter().map(|e| e.id().to_string()).collect();

        if branch_ids.is_empty() {
            anyhow::bail!("nothing to fork — session has no entries");
        }

        let new_id = crate::session::types::gen_session_id();
        let now = now_iso();

        // Everything runs inside a transaction; roll back on any error.
        {
            let tx = self.db.transaction()?;
            let result = Self::fork_session_inner_tx(&tx, &src_id, &new_id, &now, &branch_ids);
            if result.is_err() {
                tx.rollback().ok();
                return result.map(|_| new_id);
            }
            tx.commit()?;
        }

        // Switch to the new session and reload in-memory state.  The entry ids
        // are identical to the source but the owning session_id has changed, so
        // we must reload rather than relying on the stale in-memory view.
        self.session_id = Some(new_id.clone());
        self.reload_entries()?;

        Ok(new_id)
    }

    fn fork_session_inner_tx(
        tx: &rusqlite::Transaction<'_>,
        src_id: &str,
        new_id: &str,
        now: &str,
        branch_ids: &[String],
    ) -> anyhow::Result<()> {
        // Copy session metadata row, stamping a fresh id and timestamp.
        // name, cwd, worktree, compact_threshold, repo_id and preview all
        // carry over so the fork looks like the original in /resume.
        tx.execute(
            "INSERT INTO sessions (id, cwd, created_at, updated_at, preview, worktree, name, compact_threshold, repo_id)
             SELECT ?1, cwd, ?2, ?3, preview, worktree, name, compact_threshold, repo_id
             FROM sessions WHERE id = ?4",
            params![new_id, now, now, src_id],
        )?;

        // Assign a fresh entry id for every copied entry.  Entry ids are a
        // PRIMARY KEY across the whole DB (not scoped to session_id), so we
        // cannot reuse the source ids within the same database file.  We also
        // remap parent_id references so the parent chain stays intact.
        let id_map: HashMap<String, String> =
            branch_ids.iter().map(|old| (old.clone(), gen_entry_id())).collect();

        // Prepare the search_index read once outside the loop.
        let mut si_stmt = tx.prepare("SELECT text, role FROM search_index WHERE entry_id = ?1")?;

        for old_id in branch_ids {
            let new_entry_id = &id_map[old_id];

            // Read the source row.
            let (parent_id_raw, seq, data): (Option<String>, i64, String) = tx
                .query_row(
                    "SELECT parent_id, seq, data FROM entries WHERE id = ?1",
                    params![old_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(|_| anyhow::anyhow!("entry {old_id} not found during fork"))?;

            // Remap parent_id if it was also in this branch (it always is for a
            // linear branch, but be safe).
            let mapped_parent: Option<String> = parent_id_raw
                .as_deref()
                .map(|p| id_map.get(p).map(|s| s.as_str()).unwrap_or(p).to_string());

            // Patch the id and parent_id inside the JSON data blob so that
            // reload_entries sees consistent state (entry.id() must match the
            // DB id column, and parent chain must use the new ids).
            let patched_data = {
                let mut entry: SessionEntry = serde_json::from_str(&data)
                    .map_err(|e| anyhow::anyhow!("deserialise entry {old_id}: {e}"))?;
                entry.set_id(new_entry_id.clone());
                entry.set_parent_id(mapped_parent.clone());
                serde_json::to_string(&entry)
                    .map_err(|e| anyhow::anyhow!("serialise fork entry: {e}"))?
            };

            tx.execute(
                "INSERT INTO entries (id, session_id, parent_id, seq, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![new_entry_id, new_id, mapped_parent, seq, patched_data],
            )?;

            // Copy search_index rows, updating entry_id to the new id.
            let si_rows: Vec<(String, String)> = si_stmt
                .query_map(params![old_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect();
            for (text, role) in si_rows {
                tx.execute(
                    "INSERT INTO search_index (text, session_id, entry_id, role) VALUES (?1, ?2, ?3, ?4)",
                    params![text, new_id, new_entry_id, role],
                )?;
            }
        }

        Ok(())
    }

    pub fn has_sessions_for_repo(&self, repo_id: &str) -> bool {
        self.db
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE repo_id = ?1 LIMIT 1",
                params![repo_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false)
    }

    pub fn repo_id(&self) -> Option<String> {
        let sid = self.session_id.as_deref()?;
        self.db
            .query_row("SELECT repo_id FROM sessions WHERE id = ?1", params![sid], |row| {
                row.get::<_, Option<String>>(0)
            })
            .ok()
            .flatten()
    }

    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        // Single query: join entries count so we avoid N+1 round-trips.
        let mut stmt = match self.db.prepare(
            "SELECT s.id, s.cwd, s.created_at, s.updated_at, s.preview, s.name, s.repo_id,
                    COUNT(e.id) as message_count
             FROM sessions s
             LEFT JOIN entries e ON e.session_id = s.id
             GROUP BY s.id
             ORDER BY s.updated_at DESC
             LIMIT 100",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, i64>(7)?,
            ))
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.flatten()
            .map(|(id, cwd, created_at, updated_at, preview, name, repo_id, count)| {
                let id_short = if id.len() >= 8 { id[..8].to_string() } else { id.clone() };
                let modified = parse_iso_to_system_time(&updated_at);
                SessionSummary {
                    id_short,
                    timestamp: created_at,
                    cwd,
                    preview,
                    name,
                    repo_id,
                    message_count: count as u32,
                    modified,
                }
            })
            .collect()
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
            WHERE text MATCH ?1
            ORDER BY rank
            LIMIT 100",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows: Vec<(String, String, String, f64)> =
            match stmt.query_map(params![fts_query], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                ))
            }) {
                Ok(r) => r.filter_map(|r| r.ok()).collect(),
                Err(_) => return Vec::new(),
            };

        // Dedup by session_id keeping best (lowest) rank, then join with sessions
        // in a single CTE query — no per-session round-trips.
        //
        // Build a VALUES list from the in-memory dedup so SQLite can join against it.
        let mut best: HashMap<String, (String, String, f64)> = HashMap::new();
        for (excerpt, session_id, role, rank) in rows {
            best.entry(session_id)
                .and_modify(|existing| {
                    if rank < existing.2 {
                        *existing = (excerpt.clone(), role.clone(), rank);
                    }
                })
                .or_insert((excerpt, role, rank));
        }
        if best.is_empty() {
            return Vec::new();
        }

        // Build a single query: join sessions + entries count for all matched session
        // ids.
        let placeholders = best
            .keys()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT s.id, s.cwd, s.updated_at, COUNT(e.id)
             FROM sessions s
             LEFT JOIN entries e ON e.session_id = s.id
             WHERE s.id IN ({})
             GROUP BY s.id",
            placeholders
        );
        let mut stmt = match self.db.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let ids: Vec<&String> = best.keys().collect();
        for (i, id) in ids.iter().enumerate() {
            stmt.raw_bind_parameter(i + 1, id.as_str()).ok();
        }
        let session_rows: Vec<(String, String, String, i64)> = stmt
            .raw_query()
            .mapped(|row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .filter_map(|r| r.ok())
            .collect();

        let mut results: Vec<SearchResult> = session_rows
            .into_iter()
            .filter_map(|(id, cwd, updated_at, count)| {
                let (excerpt, role, rank) = best.get(&id)?.clone();
                let id_short = if id.len() >= 8 { id[..8].to_string() } else { id.clone() };
                let modified = parse_iso_to_system_time(&updated_at);
                Some(SearchResult {
                    session_id: id,
                    id_short,
                    excerpt,
                    role,
                    cwd,
                    modified,
                    message_count: count as u32,
                    rank,
                })
            })
            .collect();

        results.sort_by(|a, b| a.rank.partial_cmp(&b.rank).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(20);
        results
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
        // Compaction entries: emit archived messages inline first so the
        // reader sees a flat timeline, then emit the compaction marker
        // without the (now-redundant) archived_messages blob.
        for entry in &branch {
            if let SessionEntry::Compaction(ce) = entry {
                for msg in &ce.archived_messages {
                    let line = serde_json::json!({
                        "type": "message",
                        "archived": true,
                        "message": msg,
                    });
                    if let Ok(data) = serde_json::to_string(&line) {
                        lines.push(data);
                    }
                }
                // Emit a lean compaction marker: no archived_messages blob.
                let marker = serde_json::json!({
                    "type": "compaction",
                    "id": ce.id,
                    "parent_id": ce.parent_id,
                    "timestamp": ce.timestamp,
                    "summary": ce.summary,
                    "first_kept_entry_id": ce.first_kept_entry_id,
                    "tokens_before": ce.tokens_before,
                    "tokens_after": ce.tokens_after,
                    "model_id": ce.model_id,
                    "cost_usd_before": ce.cost_usd_before,
                });
                if let Ok(data) = serde_json::to_string(&marker) {
                    lines.push(data);
                }
                continue;
            }
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

/// Extract type string, summary text, raw_text, is_user, has_tool_calls from an
/// entry.
fn summarize_entry(entry: &SessionEntry) -> (String, String, String, bool, bool) {
    match entry {
        SessionEntry::Message(e) => match &e.message {
            AgentMessage::User { content, .. } => {
                let text = content_text(content);
                ("message".into(), truncate(&text, 80), text, true, false)
            }
            AgentMessage::Assistant(a) => {
                let text = a.text_content();
                let has_tools =
                    a.content.iter().any(|b| matches!(b, ContentBlock::ToolCall { .. }));
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
        SessionEntry::SystemPrompt(_) => {
            ("system_prompt".into(), String::new(), String::new(), false, false)
        }
        SessionEntry::CustomMessage(_) => {
            ("custom_message".into(), String::new(), String::new(), false, false)
        }
        SessionEntry::PermissionAccept(p) => (
            "permission_accept".into(),
            format!("{}({})", p.tool, truncate(&p.args, 40)),
            String::new(),
            false,
            false,
        ),
        SessionEntry::Btw(b) => (
            "btw".into(),
            truncate(&b.note, 80),
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
        SessionEntry::Btw(e) => e.timestamp.clone(),
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
    if line.len() <= max { line.to_string() } else { format!("{}...", line.truncate_chars(max - 3)) }
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
                if text.len() < 2000 { Some((text, "tool_result")) } else { None }
            }
            _ => None,
        },
        _ => None,
    }
}

fn backfill_search_index(db: &Connection) {
    let has_fts: i64 =
        db.query_row("SELECT COUNT(*) FROM search_index", [], |row| row.get(0)).unwrap_or(0);
    let has_entries: i64 =
        db.query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0)).unwrap_or(0);

    if has_fts > 0 || has_entries == 0 {
        return;
    }

    db.execute_batch("BEGIN").ok();
    let mut stmt =
        match db.prepare("SELECT id, session_id, data FROM entries ORDER BY session_id, seq") {
            Ok(s) => s,
            Err(_) => return,
        };
    let rows: Vec<(String, String, String)> =
        match stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))) {
            Ok(r) => r.filter_map(|r| r.ok()).collect(),
            Err(_) => return,
        };
    for (entry_id, session_id, data) in rows {
        if let Ok(entry) = serde_json::from_str::<SessionEntry>(&data)
            && let Some((text, role)) = extract_searchable_text(&entry)
            && !text.is_empty()
        {
            db.execute(
                "INSERT INTO search_index (text, session_id, entry_id, role) VALUES (?1, ?2, ?3, ?4)",
                params![text, session_id, entry_id, role],
            )
            .ok();
        }
    }
    db.execute_batch("COMMIT").ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::types::{AgentMessage, AssistantMessage, ContentBlock, StopReason};

    /// Build an in-memory SessionManager (no on-disk DB).
    fn make_manager() -> SessionManager {
        let db = Connection::open_in_memory().expect("in-memory db");
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;").ok();

        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id                TEXT PRIMARY KEY,
                cwd               TEXT NOT NULL,
                created_at        TEXT NOT NULL,
                updated_at        TEXT NOT NULL,
                preview           TEXT NOT NULL DEFAULT '',
                worktree          TEXT,
                name              TEXT,
                compact_threshold REAL,
                repo_id           TEXT
            );",
        )
        .unwrap();
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS entries (
                id         TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                parent_id  TEXT,
                seq        INTEGER NOT NULL,
                data       TEXT NOT NULL
            );",
        )
        .unwrap();
        db.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_entries_session ON entries(session_id, seq);",
        )
        .ok();
        db.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(
                text,
                session_id UNINDEXED,
                entry_id UNINDEXED,
                role UNINDEXED,
                tokenize = 'porter unicode61'
            );",
        )
        .unwrap();

        SessionManager {
            db,
            session_id: None,
            entries: Vec::new(),
            by_id: HashMap::new(),
            leaf_id: None,
            next_seq: 0,
        }
    }

    fn user_msg(text: &str) -> AgentMessage {
        AgentMessage::User {
            content: vec![crate::agent::types::ContentItem::Text { text: text.to_string() }],
            timestamp: 0,
        }
    }

    fn assistant_msg(text: &str) -> AgentMessage {
        AgentMessage::Assistant(AssistantMessage {
            content: vec![ContentBlock::Text { text: text.to_string() }],
            stop_reason: StopReason::EndTurn,
            usage: None,
            timestamp: 0,
        })
    }

    /// Count entries directly from the DB for a given session.
    fn db_entry_count(mgr: &SessionManager, session_id: &str) -> usize {
        mgr.db
            .query_row(
                "SELECT COUNT(*) FROM entries WHERE session_id = ?1",
                params![session_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c as usize)
            .unwrap_or(0)
    }

    /// Fetch all entry ids from the DB for a given session, ordered by seq.
    fn db_entry_ids(mgr: &SessionManager, session_id: &str) -> Vec<String> {
        let mut stmt =
            mgr.db.prepare("SELECT id FROM entries WHERE session_id = ?1 ORDER BY seq").unwrap();
        stmt.query_map(params![session_id], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    /// Count search_index rows for a session.
    fn db_search_count(mgr: &SessionManager, session_id: &str) -> usize {
        mgr.db
            .query_row(
                "SELECT COUNT(*) FROM search_index WHERE session_id = ?1",
                params![session_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c as usize)
            .unwrap_or(0)
    }

    // ── basic: fork copies entries and creates a new session ──────────────────

    #[test]
    fn fork_creates_new_session_row() {
        let mut mgr = make_manager();
        mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();
        let src_id = mgr.session_id().to_string();

        mgr.append_message(&user_msg("hello"), None).unwrap();
        mgr.append_message(&assistant_msg("world"), None).unwrap();

        let fork_id = mgr.fork_session().unwrap();

        assert_ne!(fork_id, src_id, "fork must have a different session id");
        assert_eq!(mgr.session_id(), fork_id, "manager must switch to the fork");
    }

    #[test]
    fn fork_copies_all_branch_entries() {
        let mut mgr = make_manager();
        mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();
        let src_id = mgr.session_id().to_string();

        mgr.append_message(&user_msg("one"), None).unwrap();
        mgr.append_message(&assistant_msg("two"), None).unwrap();
        mgr.append_message(&user_msg("three"), None).unwrap();

        let fork_id = mgr.fork_session().unwrap();

        assert_eq!(db_entry_count(&mgr, &src_id), 3, "source entries unchanged");
        assert_eq!(db_entry_count(&mgr, &fork_id), 3, "fork has same entry count");
    }

    #[test]
    fn fork_entry_ids_are_distinct_from_source() {
        let mut mgr = make_manager();
        mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();
        let src_id = mgr.session_id().to_string();

        mgr.append_message(&user_msg("a"), None).unwrap();
        mgr.append_message(&assistant_msg("b"), None).unwrap();

        let fork_id = mgr.fork_session().unwrap();

        let src_ids = db_entry_ids(&mgr, &src_id);
        let fork_ids = db_entry_ids(&mgr, &fork_id);
        // Fresh ids are generated for each fork entry — no collisions in the same DB.
        assert_eq!(src_ids.len(), fork_ids.len(), "same number of entries");
        for id in &fork_ids {
            assert!(!src_ids.contains(id), "fork entry id {id} must not collide with source");
        }
    }

    // ── in-memory state is consistent after fork ──────────────────────────────

    #[test]
    fn fork_reloads_in_memory_state() {
        let mut mgr = make_manager();
        mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();
        mgr.append_message(&user_msg("msg1"), None).unwrap();
        mgr.append_message(&assistant_msg("msg2"), None).unwrap();

        let fork_id = mgr.fork_session().unwrap();

        // entry count and leaf must reflect the forked session
        assert_eq!(mgr.entry_count(), 2);
        assert_eq!(mgr.session_id(), fork_id);
        assert!(mgr.leaf_id().is_some());
    }

    #[test]
    fn fork_leaf_points_to_last_fork_entry() {
        let mut mgr = make_manager();
        mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();
        mgr.append_message(&user_msg("x"), None).unwrap();
        mgr.append_message(&assistant_msg("y"), None).unwrap();

        mgr.fork_session().unwrap();
        let fork_leaf = mgr.leaf_id().unwrap().to_string();

        // The leaf must exist and must be one of the entries owned by the fork session.
        assert!(!fork_leaf.is_empty());
        let fork_id = mgr.session_id().to_string();
        let fork_entry_ids = db_entry_ids(&mgr, &fork_id);
        assert_eq!(fork_entry_ids.len(), 2);
        assert!(fork_entry_ids.contains(&fork_leaf), "leaf id must be in fork's entries");
        // The leaf must be the *last* entry (highest seq) in the fork.
        assert_eq!(*fork_entry_ids.last().unwrap(), fork_leaf);
    }

    // ── source session is untouched ───────────────────────────────────────────

    #[test]
    fn fork_does_not_mutate_source_session() {
        let mut mgr = make_manager();
        mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();
        let src_id = mgr.session_id().to_string();

        mgr.append_message(&user_msg("hi"), None).unwrap();
        mgr.append_message(&assistant_msg("there"), None).unwrap();
        let src_entry_ids = db_entry_ids(&mgr, &src_id);

        mgr.fork_session().unwrap();

        // Reload source and verify it is unchanged
        assert_eq!(db_entry_count(&mgr, &src_id), 2);
        assert_eq!(db_entry_ids(&mgr, &src_id), src_entry_ids);
    }

    // ── search index is copied ────────────────────────────────────────────────

    #[test]
    fn fork_copies_search_index() {
        let mut mgr = make_manager();
        mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();
        let src_id = mgr.session_id().to_string();

        mgr.append_message(&user_msg("searchable text"), None).unwrap();
        mgr.append_message(&assistant_msg("also searchable"), None).unwrap();

        let src_search = db_search_count(&mgr, &src_id);
        let fork_id = mgr.fork_session().unwrap();
        let fork_search = db_search_count(&mgr, &fork_id);

        assert_eq!(src_search, fork_search, "search index row count must match");
    }

    // ── append after fork writes to the fork, not the source ─────────────────

    #[test]
    fn append_after_fork_targets_fork() {
        let mut mgr = make_manager();
        mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();
        let src_id = mgr.session_id().to_string();

        mgr.append_message(&user_msg("before"), None).unwrap();
        mgr.fork_session().unwrap();
        let fork_id = mgr.session_id().to_string();

        mgr.append_message(&user_msg("after"), None).unwrap();

        assert_eq!(db_entry_count(&mgr, &src_id), 1, "source untouched after append");
        assert_eq!(db_entry_count(&mgr, &fork_id), 2, "fork received new entry");
    }

    // ── error: fork with no active session ───────────────────────────────────

    #[test]
    fn fork_with_no_session_returns_error() {
        let mut mgr = make_manager();
        assert!(mgr.fork_session().is_err());
    }

    // ── fork only copies current branch, not sibling branches ────────────────

    #[test]
    fn fork_copies_only_current_branch() {
        let mut mgr = make_manager();
        mgr.new_session(std::path::Path::new("/tmp"), None).unwrap();

        // Build a two-entry trunk
        mgr.append_message(&user_msg("root"), None).unwrap();
        mgr.append_message(&assistant_msg("reply"), None).unwrap();
        let branch_point = mgr.leaf_id().unwrap().to_string();

        // Create a sibling branch by re-rooting at the first entry and adding extra
        mgr.branch(&branch_point);
        mgr.append_message(&user_msg("sibling only"), None).unwrap();
        // current branch is now: root → reply → sibling only  (3 entries)

        // Reset to branch_point and go back to original trunk direction
        mgr.branch(&branch_point);
        // current branch: root → reply  (2 entries)
        assert_eq!(mgr.get_branch().len(), 2);

        let src_id = mgr.session_id().to_string();
        let fork_id = mgr.fork_session().unwrap();

        assert_eq!(
            db_entry_count(&mgr, &fork_id),
            2,
            "fork must only contain the active branch (2 entries), not the sibling"
        );
        assert_eq!(db_entry_count(&mgr, &src_id), 3, "source still has all 3 entries");
    }
}
