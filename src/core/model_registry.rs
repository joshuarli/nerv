use std::sync::{Arc, RwLock};

use super::auth::AuthStorage;
use super::config::{CustomProviderConfig, NervConfig};
use super::local_models::load_models;
use crate::agent::provider::ProviderRegistry;
use crate::agent::types::{Model, ModelPricing};
use crate::agent::{AnthropicProvider, OpenAICompatProvider};

/// Default model used for background compaction summarisation when no
/// `compaction_model` is set in config and the Anthropic provider is available.
pub const DEFAULT_COMPACTION_MODEL: &str = "claude-haiku-4-5";

pub struct ModelRegistry {
    built_in: Vec<Model>,
    custom: Vec<Model>,
    /// Shared with the Agent so login/logout are reflected immediately in
    /// available_models().
    pub provider_registry: Arc<RwLock<ProviderRegistry>>,
}

impl ModelRegistry {
    pub fn empty() -> Self {
        Self {
            built_in: Vec::new(),
            custom: Vec::new(),
            provider_registry: Arc::new(RwLock::new(ProviderRegistry::new())),
        }
    }

    pub fn new(config: &NervConfig, auth: &mut AuthStorage, nerv_dir: &std::path::Path) -> Self {
        let mut registry = ProviderRegistry::new();

        // Always include built-in models; available_models() filters by registered providers.
        let mut built_in = builtin_anthropic_models();
        built_in.extend(builtin_codex_models());

        // Register Anthropic provider if auth is available
        let is_oauth = auth.is_oauth("anthropic");
        let extra_headers: Vec<(String, String)> = config.effective_headers("anthropic");
        if let Some(api_key) = auth.api_key("anthropic") {
            let provider = if is_oauth {
                AnthropicProvider::new_oauth(api_key)
            } else {
                AnthropicProvider::new(api_key)
            }
            .with_headers(extra_headers);
            registry.register("anthropic", Arc::new(provider));
        }

        // Register Codex provider if auth is available.
        // Codex uses the ChatGPT backend Responses API, not the public OpenAI API.
        if let Some(api_key) = auth.api_key("codex") {
            let extra_headers = config.effective_headers("codex");
            let provider =
                crate::agent::CodexProvider::new(api_key).with_headers(extra_headers);
            registry.register("codex", Arc::new(provider));
        }

        // Register custom providers
        let mut custom = Vec::new();
        for provider_cfg in &config.custom_providers {
            let provider = OpenAICompatProvider::new(
                provider_cfg.name.clone(),
                provider_cfg.base_url.clone(),
                provider_cfg.api_key.clone(),
            );
            registry.register(&provider_cfg.name, Arc::new(provider));

            for model_cfg in &provider_cfg.models {
                custom.push(Model {
                    id: model_cfg.id.clone(),
                    name: model_cfg.name.clone().unwrap_or_else(|| model_cfg.id.clone()),
                    provider_name: provider_cfg.name.clone(),
                    context_window: model_cfg.context_window.unwrap_or(128_000),
                    max_output_tokens: 32_000,
                    reasoning: model_cfg.reasoning.unwrap_or(false),
                    supports_adaptive_thinking: false,
                    supports_xhigh: false,
                    pricing: ModelPricing {
                        input: 0.0,
                        output: 0.0,
                        cache_read: 0.0,
                        cache_write: 0.0,
                    },
                });
            }
        }

        // Register local GGUF models from ~/.nerv/models.json.
        // Each model gets its own synthetic OpenAI-compat provider pointing at its
        // port.
        for local in load_models(nerv_dir) {
            let provider_name = format!("local/{}", local.alias);
            let base_url = format!("http://127.0.0.1:{}/v1", local.port);
            let provider = OpenAICompatProvider::new(provider_name.clone(), base_url, None);
            registry.register(&provider_name, Arc::new(provider));
            custom.push(Model {
                id: local.alias.clone(),
                name: local.alias.clone(),
                provider_name,
                context_window: local.context_length,
                max_output_tokens: 32_000,
                reasoning: local.reasoning,
                supports_adaptive_thinking: false,
                supports_xhigh: false,
                pricing: ModelPricing::default_custom(),
            });
        }

        Self { built_in, custom, provider_registry: Arc::new(RwLock::new(registry)) }
    }

    pub fn all_models(&self) -> Vec<&Model> {
        self.built_in.iter().chain(self.custom.iter()).collect()
    }

    pub fn available_models(&self) -> Vec<&Model> {
        let reg = self.provider_registry.read().unwrap();
        self.all_models().into_iter().filter(|m| reg.get(&m.provider_name).is_some()).collect()
    }

    pub fn get_model(&self, provider: &str, id: &str) -> Option<&Model> {
        self.all_models().into_iter().find(|m| m.provider_name == provider && m.id == id)
    }

    /// Find a model by partial/fuzzy match. Checks id, name, and common
    /// aliases.
    pub fn find_model(&self, query: &str) -> Option<&Model> {
        let q = query.to_lowercase();
        let models = self.all_models();

        // Exact id match
        if let Some(m) = models.iter().find(|m| m.id == query) {
            return Some(m);
        }

        // Substring match on id or name
        if let Some(m) = models
            .iter()
            .find(|m| m.id.to_lowercase().contains(&q) || m.name.to_lowercase().contains(&q))
        {
            return Some(m);
        }

        None
    }

    pub fn default_model(&self, config: &NervConfig) -> Option<&Model> {
        let reg = self.provider_registry.read().unwrap();
        if let Some(ref id) = config.default_model
            && let Some(m) = self.find_model(id)
            && reg.get(&m.provider_name).is_some()
        {
            return Some(m);
        }
        drop(reg);
        self.available_models().into_iter().next()
    }

    pub fn add_custom_provider(
        &mut self,
        cfg: CustomProviderConfig,
        nerv_dir: &std::path::Path,
    ) -> anyhow::Result<()> {
        let provider =
            OpenAICompatProvider::new(cfg.name.clone(), cfg.base_url.clone(), cfg.api_key.clone());
        self.provider_registry.write().unwrap().register(&cfg.name, Arc::new(provider));

        for model_cfg in &cfg.models {
            self.custom.push(Model {
                id: model_cfg.id.clone(),
                name: model_cfg.name.clone().unwrap_or_else(|| model_cfg.id.clone()),
                provider_name: cfg.name.clone(),
                context_window: model_cfg.context_window.unwrap_or(128_000),
                max_output_tokens: 32_000,
                reasoning: model_cfg.reasoning.unwrap_or(false),
                supports_adaptive_thinking: false,
                supports_xhigh: false,
                pricing: ModelPricing::default_custom(),
            });
        }

        // Update config file
        let mut config = NervConfig::load(nerv_dir);
        config.custom_providers.push(cfg);
        config.save(nerv_dir)?;

        Ok(())
    }
}

fn builtin_anthropic_models() -> Vec<Model> {
    vec![
        Model {
            id: "claude-opus-4-6".into(),
            name: "Claude Opus 4.6".into(),
            provider_name: "anthropic".into(),
            context_window: 200_000,
            max_output_tokens: 32_000,
            reasoning: true,
            supports_adaptive_thinking: true,
            supports_xhigh: true,
            pricing: ModelPricing {
                input: 15.0,
                output: 75.0,
                cache_read: 1.5,
                cache_write: 18.75,
            },
        },
        Model {
            id: "claude-sonnet-4-6".into(),
            name: "Claude Sonnet 4.6".into(),
            provider_name: "anthropic".into(),
            context_window: 200_000,
            max_output_tokens: 32_000,
            reasoning: true,
            supports_adaptive_thinking: true,
            supports_xhigh: false,
            pricing: ModelPricing { input: 3.0, output: 15.0, cache_read: 0.3, cache_write: 3.75 },
        },
        Model {
            id: "claude-haiku-4-5".into(),
            name: "Claude Haiku 4.5".into(),
            provider_name: "anthropic".into(),
            context_window: 200_000,
            max_output_tokens: 8_192,
            reasoning: false,
            supports_adaptive_thinking: false,
            supports_xhigh: false,
            pricing: ModelPricing { input: 0.80, output: 4.0, cache_read: 0.08, cache_write: 1.0 },
        },
    ]
}

fn builtin_codex_models() -> Vec<Model> {
    fn m(id: &str, name: &str, ctx: u32, max_out: u32, reasoning: bool, inp: f64, out: f64, cr: f64) -> Model {
        Model {
            id: id.into(),
            name: name.into(),
            provider_name: "codex".into(),
            context_window: ctx,
            max_output_tokens: max_out,
            reasoning,
            supports_adaptive_thinking: reasoning,
            supports_xhigh: reasoning,
            pricing: ModelPricing { input: inp, output: out, cache_read: cr, cache_write: 0.0 },
        }
    }
    vec![
        m("gpt-5",               "GPT-5",               272_000, 128_000, true,  3.0,  12.0, 0.3),
        m("gpt-5.1",             "GPT-5.1",             272_000, 128_000, true,  1.25, 10.0, 0.125),
        m("gpt-5.1-codex-max",   "GPT-5.1 Codex Max",   272_000, 128_000, true,  1.25, 10.0, 0.125),
        m("gpt-5.1-codex-mini",  "GPT-5.1 Codex Mini",  272_000, 128_000, true,  0.25,  2.0, 0.025),
        m("gpt-5.2",             "GPT-5.2",             272_000, 128_000, true,  1.75, 14.0, 0.175),
        m("gpt-5.2-codex",       "GPT-5.2 Codex",       272_000, 128_000, true,  1.75, 14.0, 0.175),
        m("gpt-5.3-codex",       "GPT-5.3 Codex",       272_000, 128_000, true,  1.75, 14.0, 0.175),
        m("gpt-5.3-codex-spark", "GPT-5.3 Codex Spark", 128_000, 128_000, true,  0.0,   0.0, 0.0),
        m("gpt-5.4",             "GPT-5.4",             272_000, 128_000, true,  2.5,  15.0, 0.25),
        m("gpt-5.4-mini",        "GPT-5.4 Mini",        272_000, 128_000, true,  0.75,  4.5, 0.075),
    ]
}

impl ModelPricing {
    fn default_custom() -> Self {
        Self { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0 }
    }
}
