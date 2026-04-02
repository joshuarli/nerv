use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub provider_name: String,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub reasoning: bool,
    pub supports_adaptive_thinking: bool,
    pub supports_xhigh: bool,
    pub pricing: ModelPricing,
}

/// Per-token pricing for a model. All values are **USD per million tokens**.
#[derive(Debug, Clone)]
pub struct ModelPricing {
    /// Cost of uncached input tokens (USD / 1 000 000 tokens).
    pub input: f64,
    /// Cost of output tokens (USD / 1 000 000 tokens).
    pub output: f64,
    /// Cost of cache-read input tokens (USD / 1 000 000 tokens).
    pub cache_read: f64,
    /// Cost of cache-write input tokens (USD / 1 000 000 tokens).
    pub cache_write: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

impl Cost {
    pub fn add_usage(&mut self, usage: &Usage, pricing: &ModelPricing) {
        // usage.input includes cache_read + cache_write — subtract them to avoid
        // double-counting.
        let uncached = usage.input.saturating_sub(usage.cache_read + usage.cache_write);
        let di = (pricing.input / 1_000_000.0) * uncached as f64;
        let do_ = (pricing.output / 1_000_000.0) * usage.output as f64;
        let dr = (pricing.cache_read / 1_000_000.0) * usage.cache_read as f64;
        let dw = (pricing.cache_write / 1_000_000.0) * usage.cache_write as f64;
        self.input += di;
        self.output += do_;
        self.cache_read += dr;
        self.cache_write += dw;
        self.total += di + do_ + dr + dw;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum ThinkingLevel {
    #[default]
    Off,
    On,
}

/// Effort level for Anthropic's adaptive thinking API.
/// When set, the model decides its own thinking token budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    Max,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum AgentMessage {
    #[serde(rename = "user")]
    User { content: MessageContent, timestamp: u64 },
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    #[serde(rename = "toolResult")]
    ToolResult {
        tool_call_id: String,
        content: MessageContent,
        is_error: bool,
        /// Rich display text for the TUI/HTML (e.g. unified diff). Not sent to
        /// the LLM.
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<String>,
        /// Typed tool-level metadata. Not sent to the LLM. Optional so old
        /// serialized sessions without this field deserialize fine.
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<ToolDetails>,
        timestamp: u64,
    },
    #[serde(rename = "custom")]
    Custom { custom_type: String, content: MessageContent, display: bool, timestamp: u64 },
    #[serde(rename = "bashExecution")]
    BashExecution { command: String, output: String, exit_code: Option<i32>, timestamp: u64 },
    #[serde(rename = "compactionSummary")]
    CompactionSummary { summary: String, tokens_before: u32, timestamp: u64 },
    #[serde(rename = "branchSummary")]
    BranchSummary { summary: String, from_id: String, timestamp: u64 },
}

impl AgentMessage {
    pub fn timestamp(&self) -> u64 {
        match self {
            Self::User { timestamp, .. }
            | Self::ToolResult { timestamp, .. }
            | Self::Custom { timestamp, .. }
            | Self::BashExecution { timestamp, .. }
            | Self::CompactionSummary { timestamp, .. }
            | Self::BranchSummary { timestamp, .. } => *timestamp,
            Self::Assistant(msg) => msg.timestamp,
        }
    }

    pub fn is_assistant(&self) -> bool {
        matches!(self, Self::Assistant(_))
    }

    pub fn as_assistant(&self) -> Option<&AssistantMessage> {
        if let Self::Assistant(a) = self { Some(a) } else { None }
    }
}

pub type MessageContent = Vec<ContentItem>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentItem {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: ImageSource },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    pub media_type: String,
    pub data: String, // base64
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub usage: Option<Usage>,
    pub timestamp: u64,
}

impl AssistantMessage {
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> Vec<&ContentBlock> {
        self.content.iter().filter(|b| matches!(b, ContentBlock::ToolCall { .. })).collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "toolCall")]
    ToolCall { id: String, name: String, arguments: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Aborted,
    Error { message: String },
}

impl StopReason {
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }

    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Error { message } => Some(message),
            _ => None,
        }
    }

    /// Check if this error indicates the context exceeded the model's window.
    pub fn is_context_overflow(&self) -> bool {
        let Self::Error { message } = self else {
            return false;
        };
        let lower = message.to_lowercase();
        // Anthropic: "prompt is too long: 213462 tokens > 200000 maximum"
        // OpenAI: "exceeds the context window"
        // OpenAI-compat/OpenRouter: "maximum context length is N tokens"
        // Generic fallbacks
        lower.contains("prompt is too long")
            || lower.contains("exceeds the context window")
            || lower.contains("maximum context length")
            || lower.contains("too many tokens")
            || lower.contains("token limit exceeded")
            || lower.contains("context length exceeded")
            || lower.contains("context_length_exceeded")
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Total input tokens charged (includes `cache_read` and `cache_write`).
    /// When computing uncached cost, subtract those fields to avoid
    /// double-counting: `uncached = input - cache_read - cache_write`.
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
}

/// Events emitted by the Agent during a prompt cycle.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<AgentMessage>,
        system_prompt: String,
    },
    MessageStart {
        message: AgentMessage,
    },
    MessageUpdate {
        delta: StreamDelta,
    },
    UsageUpdate {
        usage: Usage,
    },
    MessageEnd {
        message: AssistantMessage,
    },
    ToolExecutionStart {
        id: String,
        name: String,
        args: serde_json::Value,
    },
    ToolExecutionUpdate {
        id: String,
        output: String,
    },
    ToolExecutionEnd {
        id: String,
        result: ToolResultData,
    },
    TurnStart,
    TurnEnd,
    /// Emitted after tool results are recorded and before the next API call.
    /// Allows the caller to trigger compaction before context overflows.
    ContextEstimate { estimated_tokens: usize },
    /// Emitted when a retryable error (rate limit / overload) triggers a retry.
    Retrying {
        attempt: u32,
        wait_secs: u64,
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub enum StreamDelta {
    Text(String),
    Thinking(String),
    ToolCallArgsStart { id: String, name: String },
    ToolCallArgsDelta { id: String, delta: String },
}

#[derive(Debug, Clone)]
pub struct ToolResultData {
    pub content: String,
    /// Short summary for TUI display. If set, the TUI shows this
    /// instead of the full content. Content still goes to the LLM.
    pub display: Option<String>,
    pub is_error: bool,
}

/// Typed metadata attached to a `ToolResult` / `AgentMessage::ToolResult`.
/// Not sent to the LLM — used by the TUI, transform_context, and export.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolDetails {
    /// Short display summary shown in the TUI instead of the raw content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// Unified diff string (edit/write tools). Used for HTML export.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// True when the bash output filter has already been applied at execution
    /// time, so transform_context can skip it on subsequent passes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub filtered: bool,
    /// Exit code from bash. Informational only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

pub use crate::now_millis;
