use crate::agent::types::{AgentMessage, EffortLevel};
use serde::{Deserialize, Serialize};

pub const CURRENT_SESSION_VERSION: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionEntry {
    #[serde(rename = "message")]
    Message(MessageEntry),
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    #[serde(rename = "model_change")]
    ModelChange(ModelChangeEntry),
    #[serde(rename = "compaction")]
    Compaction(CompactionEntry),
    #[serde(rename = "branch_summary")]
    BranchSummary(BranchSummaryEntry),
    #[serde(rename = "custom_message")]
    CustomMessage(CustomMessageEntry),
    #[serde(rename = "label")]
    Label(LabelEntry),
    #[serde(rename = "session_info")]
    SessionInfo(SessionInfoEntry),
    #[serde(rename = "system_prompt")]
    SystemPrompt(SystemPromptEntry),
    #[serde(rename = "permission_accept")]
    PermissionAccept(PermissionAcceptEntry),
}

impl SessionEntry {
    pub fn id(&self) -> &str {
        match self {
            Self::Message(e) => &e.id,
            Self::ThinkingLevelChange(e) => &e.id,
            Self::ModelChange(e) => &e.id,
            Self::Compaction(e) => &e.id,
            Self::BranchSummary(e) => &e.id,
            Self::CustomMessage(e) => &e.id,
            Self::Label(e) => &e.id,
            Self::SessionInfo(e) => &e.id,
            Self::SystemPrompt(e) => &e.id,
            Self::PermissionAccept(e) => &e.id,
        }
    }

    pub fn set_id(&mut self, new_id: String) {
        match self {
            Self::Message(e) => e.id = new_id,
            Self::ThinkingLevelChange(e) => e.id = new_id,
            Self::ModelChange(e) => e.id = new_id,
            Self::Compaction(e) => e.id = new_id,
            Self::BranchSummary(e) => e.id = new_id,
            Self::CustomMessage(e) => e.id = new_id,
            Self::Label(e) => e.id = new_id,
            Self::SessionInfo(e) => e.id = new_id,
            Self::SystemPrompt(e) => e.id = new_id,
            Self::PermissionAccept(e) => e.id = new_id,
        }
    }

    pub fn set_parent_id(&mut self, new_parent: Option<String>) {
        match self {
            Self::Message(e) => e.parent_id = new_parent,
            Self::ThinkingLevelChange(e) => e.parent_id = new_parent,
            Self::ModelChange(e) => e.parent_id = new_parent,
            Self::Compaction(e) => e.parent_id = new_parent,
            Self::BranchSummary(e) => e.parent_id = new_parent,
            Self::CustomMessage(e) => e.parent_id = new_parent,
            Self::Label(e) => e.parent_id = new_parent,
            Self::SessionInfo(e) => e.parent_id = new_parent,
            Self::SystemPrompt(e) => e.parent_id = new_parent,
            Self::PermissionAccept(e) => e.parent_id = new_parent,
        }
    }

    pub fn parent_id(&self) -> Option<&str> {
        match self {
            Self::Message(e) => e.parent_id.as_deref(),
            Self::ThinkingLevelChange(e) => e.parent_id.as_deref(),
            Self::ModelChange(e) => e.parent_id.as_deref(),
            Self::Compaction(e) => e.parent_id.as_deref(),
            Self::BranchSummary(e) => e.parent_id.as_deref(),
            Self::CustomMessage(e) => e.parent_id.as_deref(),
            Self::Label(e) => e.parent_id.as_deref(),
            Self::SessionInfo(e) => e.parent_id.as_deref(),
            Self::SystemPrompt(e) => e.parent_id.as_deref(),
            Self::PermissionAccept(e) => e.parent_id.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub message: AgentMessage,
    /// Token counts at time of this message (optional for backwards compat).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
    pub context_used: u32,
    pub context_window: u32,
    /// Computed cost in USD for this API call. Zero for legacy entries.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub cost_usd: f64,
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingLevelChangeEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub thinking_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelChangeEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchSummaryEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub from_id: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomMessageEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub message: AgentMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfoEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemPromptEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub prompt: String,
    pub token_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionAcceptEntry {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    /// Tool name (e.g., "bash", "write")
    pub tool: String,
    /// Arguments to the tool (serialized as JSON for consistency)
    pub args: String,
}

/// Per-session config overrides. Stored as a JSON blob in the sessions table.
/// `None` fields fall back to the global `NervConfig` default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionConfig {
    pub default_model: Option<String>,
    pub default_thinking: Option<bool>,
    pub default_effort_level: Option<EffortLevel>,
    pub auto_compact: Option<bool>,
    pub compaction_model: Option<String>,
}

/// Tree node for `SessionManager::get_tree()`.
#[derive(Debug, Clone)]
pub struct SessionTreeNode {
    pub entry_id: String,
    /// "message", "compaction", "model_change", etc.
    pub entry_type: String,
    /// First ~80 chars of meaningful content.
    pub summary: String,
    /// Full raw text (for user messages — placed into editor when selected in /tree).
    pub raw_text: String,
    pub timestamp: String,
    pub children: Vec<SessionTreeNode>,
    /// True if this is a user message entry.
    pub is_user: bool,
    /// True if assistant message contains tool calls.
    pub has_tool_calls: bool,
}

pub fn gen_entry_id() -> String {
    format!("{:08x}", rand_u64() as u32)
}

/// Generate a random session ID (full 128-bit hex).
pub fn gen_session_id() -> String {
    format!(
        "{:016x}-{:04x}-{:04x}-{:04x}-{:012x}",
        rand_u64(),
        rand_u64() as u16,
        rand_u64() as u16,
        rand_u64() as u16,
        rand_u64() & 0xFFFF_FFFF_FFFF
    )
}

fn rand_u64() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0);

    // Lazy init from time + pid
    let mut s = STATE.load(Ordering::Relaxed);
    if s == 0 {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
            ^ (std::process::id() as u64) << 32;
        STATE.store(seed, Ordering::Relaxed);
        s = seed;
    }

    // xorshift64
    s ^= s << 13;
    s ^= s >> 7;
    s ^= s << 17;
    STATE.store(s, Ordering::Relaxed);
    s
}

pub fn now_iso() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let (year, month, day) = days_to_ymd(secs / 86400);
    let t = secs % 86400;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        t / 3600,
        (t % 3600) / 60,
        t % 60
    )
}

pub fn today_ymd() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (year, month, day) = days_to_ymd(secs / 86400);
    format!("{:04}-{:02}-{:02}", year, month, day)
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
