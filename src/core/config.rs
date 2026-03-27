use serde::{Deserialize, Serialize};
use std::path::Path;

/// Parse JSONC (JSON with `//` line comments). Strips comments before parsing.
pub fn read_jsonc<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    let stripped: String = text
        .lines()
        .map(|l| {
            // Find // that's not inside a string
            let mut in_string = false;
            let mut escape = false;
            for (i, ch) in l.char_indices() {
                if escape {
                    escape = false;
                    continue;
                }
                if ch == '\\' && in_string {
                    escape = true;
                    continue;
                }
                if ch == '"' {
                    in_string = !in_string;
                    continue;
                }
                if !in_string && ch == '/' && l[i..].starts_with("//") {
                    return &l[..i];
                }
            }
            l
        })
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&stripped).ok()
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct NervConfig {
    #[serde(default)]
    pub custom_providers: Vec<CustomProviderConfig>,
    pub default_model: Option<String>,
    pub default_thinking_level: Option<String>,
    pub default_effort_level: Option<String>,
    pub auto_compact: Option<bool>,
    /// Model used for background compaction summarisation.
    /// Accepts any model id recognised by the model registry (fuzzy match).
    /// Defaults to "claude-haiku-4-5" on the anthropic provider when unset.
    pub compaction_model: Option<String>,
    /// Model used for automatic session title generation after the first turn.
    /// Accepts any model id recognised by the model registry (fuzzy match).
    /// Defaults to "claude-haiku-4-5" on the anthropic provider when unset.
    pub session_naming_model: Option<String>,
    /// Extra HTTP headers per provider, e.g. {"anthropic": {"user-agent": "claude-cli/1.0.0"}}
    #[serde(default)]
    pub headers: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomProviderConfig {
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub models: Vec<CustomModelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomModelConfig {
    pub id: String,
    pub name: Option<String>,
    pub context_window: Option<u32>,
    pub reasoning: Option<bool>,
}

impl NervConfig {
    pub fn load(nerv_dir: &Path) -> Self {
        let path = nerv_dir.join("config.json");
        read_jsonc(&path).unwrap_or_default()
    }

    pub fn save(&self, nerv_dir: &Path) -> anyhow::Result<()> {
        let path = nerv_dir.join("config.json");
        let tmp = nerv_dir.join("config.json.tmp");
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, &content)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}
