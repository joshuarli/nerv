use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agent::types::EffortLevel;
/// Parse JSONC (JSON with `//` line comments). Strips comments before parsing.
pub fn read_jsonc<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&strip_jsonc_comments(&text)).ok()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NervConfig {
    #[serde(default)]
    pub custom_providers: Vec<CustomProviderConfig>,
    /// Locally-running providers (e.g. Ollama). Models are auto-discovered
    /// from `{base_url}/models` at startup; offline providers are silently
    /// skipped.
    #[serde(default)]
    pub local_providers: Vec<LocalProviderConfig>,
    pub default_model: Option<String>,
    pub default_thinking: Option<bool>,
    pub default_effort_level: Option<EffortLevel>,
    pub auto_compact: Option<bool>,
    /// Model used for background compaction summarisation.
    /// Accepts any model id recognised by the model registry (fuzzy match).
    /// Defaults to "claude-haiku-4-5" on the anthropic provider when unset.
    pub compaction_model: Option<String>,
    /// Lite-compaction age threshold (turns). Tool results older than this are
    /// zeroed before full LLM summarisation is attempted. Default: 8.
    #[serde(default)]
    pub lite_compact_age: Option<usize>,
    /// Extra HTTP headers per provider, e.g. {"anthropic": {"user-agent":
    /// "claude-cli/1.0.0"}}
    #[serde(default)]
    pub headers: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    /// Notification hooks fired on specific events.
    /// Matchers: "onPermissionDenied", "onCompactionDone",
    /// "onResponseComplete".
    #[serde(default)]
    pub notifications: Vec<super::notifications::NotificationRule>,
    /// Additional paths the shell may read from without prompting (beyond the
    /// repo root and /tmp). Supports absolute paths and `~/` prefix.
    /// Example: ["/usr/local/include", "~/.cargo"]
    #[serde(default)]
    pub allowed_read_paths: Vec<String>,
    /// Additional paths the shell may write to without prompting (beyond the
    /// repo root and /tmp). Supports absolute paths and `~/` prefix.
    /// Example: ["/tmp", "~/.nerv"]
    #[serde(default)]
    pub allowed_write_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomProviderConfig {
    pub name: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub models: Vec<CustomModelConfig>,
}

/// A locally-running provider (e.g. Ollama) whose available models are
/// discovered at startup by querying `{base_url}/models`. No API key is
/// required. Registration is non-fatal if the provider is offline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalProviderConfig {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomModelConfig {
    pub id: String,
    pub name: Option<String>,
    pub context_window: Option<u32>,
    pub reasoning: Option<bool>,
}

/// Built-in default headers for each provider, applied before user overrides.
fn builtin_default_headers()
-> std::collections::HashMap<String, std::collections::HashMap<String, String>> {
    let mut anthropic = std::collections::HashMap::new();
    anthropic
        .insert("anthropic-beta".to_string(), "claude-code-20250219,oauth-2025-04-20".to_string());
    anthropic.insert("user-agent".to_string(), "claude-cli/1.0.0".to_string());
    anthropic.insert("x-app".to_string(), "cli".to_string());
    // OpenRouter recommends these headers for attribution and app identification.
    let mut openrouter = std::collections::HashMap::new();
    openrouter.insert("HTTP-Referer".to_string(), "https://github.com/joshka/nerv".to_string());
    openrouter.insert("X-Title".to_string(), "nerv".to_string());

    let mut map = std::collections::HashMap::new();
    map.insert("anthropic".to_string(), anthropic);
    map.insert("openrouter".to_string(), openrouter);
    map
}

impl Default for NervConfig {
    fn default() -> Self {
        Self {
            custom_providers: Vec::new(),
            local_providers: vec![LocalProviderConfig {
                name: "ollama".into(),
                base_url: "http://localhost:11434/v1".into(),
                api_key: None,
            }],
            default_model: None,
            default_thinking: Some(true),
            default_effort_level: Some(EffortLevel::Medium),
            auto_compact: Some(true),
            compaction_model: Some(
                crate::core::model_registry::DEFAULT_COMPACTION_MODEL.to_string(),
            ),
            lite_compact_age: None,
            headers: builtin_default_headers(),
            notifications: Vec::new(),
            allowed_read_paths: Vec::new(),
            allowed_write_paths: Vec::new(),
        }
    }
}

impl NervConfig {
    /// Returns effective headers for a provider: built-in defaults overridden
    /// by user config.
    pub fn effective_headers(&self, provider: &str) -> Vec<(String, String)> {
        let defaults = builtin_default_headers();
        let mut merged: std::collections::HashMap<String, String> =
            defaults.get(provider).cloned().unwrap_or_default();
        if let Some(user) = self.headers.get(provider) {
            merged.extend(user.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        merged.into_iter().collect()
    }
}

impl NervConfig {
    pub fn load(nerv_dir: &Path) -> Self {
        let path = nerv_dir.join("config.json");

        if !path.exists() {
            // Write defaults on first run, then return them.
            let defaults = Self::default();
            if let Ok(value) = serde_json::to_value(&defaults) {
                let _ = write_json(&path, &value);
            }
            return defaults;
        }

        // Read existing config. Backfill any keys that exist in defaults but are
        // absent from the file, then re-save so the file is always complete.
        let existing: Self = read_jsonc(&path).unwrap_or_default();

        if let (Ok(mut merged), Ok(user)) =
            (serde_json::to_value(Self::default()), serde_json::to_value(&existing))
            && let (Some(merged_obj), Some(user_obj)) = (merged.as_object_mut(), user.as_object())
        {
            // Overwrite each default key with the user's value.
            for (k, v) in user_obj {
                merged_obj.insert(k.clone(), v.clone());
            }
            // If any key in merged_obj was absent from the user file, the file
            // is stale — rewrite it with the complete set.
            let needs_write = merged_obj.keys().any(|k| !user_obj.contains_key(k));
            if needs_write {
                let _ = write_json(&path, &merged);
            }
        }

        existing
    }

    pub fn save(&self, nerv_dir: &Path) -> anyhow::Result<()> {
        let path = nerv_dir.join("config.json");
        let value = serde_json::to_value(self)?;
        write_json(&path, &value)
    }

    /// Return a list of human-readable warnings for model fields that reference
    /// a model id not present in the registry. Call after building the
    /// registry.
    pub fn validate_model_ids(&self, known_ids: &[&str]) -> Vec<String> {
        let mut warnings = Vec::new();
        let check = |field: &str, id: &str| -> Option<String> {
            if !known_ids.iter().any(|k| k.contains(id) || id.contains(k) || *k == id) {
                Some(format!("config: {} = {:?} does not match any known model id", field, id))
            } else {
                None
            }
        };
        if let Some(ref id) = self.compaction_model
            && let Some(w) = check("compaction_model", id)
        {
            warnings.push(w);
        }
        if let Some(ref id) = self.default_model
            && let Some(w) = check("default_model", id)
        {
            warnings.push(w);
        }
        warnings
    }
}

fn strip_jsonc_comments(text: &str) -> String {
    text.lines()
        .map(|l| {
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
        .join("\n")
}

fn write_json(path: &Path, value: &serde_json::Value) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let content = serde_json::to_string_pretty(value)?;
    std::fs::write(&tmp, &content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
