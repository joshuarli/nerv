use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::convert::LlmMessage;
use super::types::*;

#[derive(Debug, Clone)]
pub enum ProviderEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallArgsDelta {
        id: String,
        delta: String,
    },
    ToolCallEnd {
        id: String,
    },
    /// Partial usage update (e.g. input tokens from Anthropic message_start).
    UsageUpdate(Usage),
    MessageStop {
        stop_reason: StopReason,
        usage: Usage,
    },
}

#[derive(Debug, Clone)]
pub struct WireTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug)]
pub struct CompletionRequest {
    pub model_id: String,
    pub system_prompt: String,
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<WireTool>,
    pub max_tokens: u32,
    pub thinking: Option<ThinkingRequest>,
    pub cache: CacheConfig,
}

#[derive(Debug, Clone)]
pub enum ThinkingRequest {
    Budget { tokens: u32 },
}

#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub retention: CacheRetention,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            retention: CacheRetention::Short,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheRetention {
    None,
    Short,
    Long,
}

impl CacheRetention {
    pub fn from_env() -> Self {
        match std::env::var("NERV_CACHE_RETENTION").as_deref() {
            Ok("long") => Self::Long,
            Ok("none") => Self::None,
            _ => Self::Short,
        }
    }
}

/// Cancelled flag shared between the caller and the provider.
pub type CancelFlag = Arc<AtomicBool>;

pub fn new_cancel_flag() -> CancelFlag {
    Arc::new(AtomicBool::new(false))
}

/// Sync provider trait. Streams events via callback.
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;

    /// Stream a completion synchronously, calling `on_event` for each SSE
    /// event as it arrives from the network. The implementation should check
    /// `cancel.load(Ordering::Relaxed)` between chunks and return early
    /// with `StopReason::Aborted` if set.
    fn stream_completion(
        &self,
        request: &CompletionRequest,
        cancel: &CancelFlag,
        on_event: &mut dyn FnMut(ProviderEvent),
    ) -> Result<(), crate::errors::ProviderError>;

    /// Check if the provider endpoint is reachable and the credentials are valid.
    /// Must not consume any tokens — use a cheap list/ping endpoint.
    /// Default implementation returns `true` (used by providers without a dedicated check).
    fn healthcheck(&self) -> bool {
        true
    }
}

#[derive(Clone)]
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    pub fn register(&mut self, name: &str, provider: Arc<dyn Provider>) {
        self.providers.insert(name.to_string(), provider);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(name).cloned()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(|s| s.as_str())
    }
}

pub fn resolve_thinking(
    level: ThinkingLevel,
    effort: Option<EffortLevel>,
    _model: &Model,
) -> Option<ThinkingRequest> {
    // Effort overrides thinking level — map to concrete token budgets
    if let Some(e) = effort {
        let tokens = match e {
            EffortLevel::Low    =>  2_000,
            EffortLevel::Medium =>  8_000,
            EffortLevel::High   => 16_000,
            EffortLevel::Max    => 32_000,
        };
        return Some(ThinkingRequest::Budget { tokens });
    }
    if level == ThinkingLevel::Off {
        return None;
    }
    // thinking on: use a sensible default budget
    Some(ThinkingRequest::Budget { tokens: 10_000 })
}

pub fn adjust_max_tokens_for_thinking(
    base_max: u32,
    model_max: u32,
    thinking: &ThinkingRequest,
) -> (u32, u32) {
    match thinking {
        ThinkingRequest::Budget { tokens } => {
            let adjusted = (base_max + tokens).min(model_max);
            let budget = if adjusted <= *tokens {
                adjusted.saturating_sub(1024)
            } else {
                *tokens
            };
            (adjusted, budget)
        }
    }
}
