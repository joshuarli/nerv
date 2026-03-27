use std::sync::Arc;

use super::auth::AuthStorage;
use super::config::{CustomProviderConfig, NervConfig};
use crate::agent::provider::ProviderRegistry;
use crate::agent::types::{Model, ModelPricing};
use crate::agent::{AnthropicProvider, OpenAICompatProvider};

pub struct ModelRegistry {
    built_in: Vec<Model>,
    custom: Vec<Model>,
    pub provider_registry: ProviderRegistry,
}

impl ModelRegistry {
    pub fn empty() -> Self {
        Self {
            built_in: Vec::new(),
            custom: Vec::new(),
            provider_registry: ProviderRegistry::new(),
        }
    }

    pub fn new(config: &NervConfig, auth: &mut AuthStorage) -> Self {
        let mut registry = ProviderRegistry::new();

        // Always include built-in models; available_models() filters by registered providers
        let built_in = builtin_anthropic_models();

        // Register Anthropic provider if auth is available
        let is_oauth = auth.is_oauth("anthropic");
        let extra_headers: Vec<(String, String)> = config
            .headers
            .get("anthropic")
            .map(|h| h.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        if let Some(api_key) = auth.api_key("anthropic") {
            let provider = if is_oauth {
                AnthropicProvider::new_oauth(api_key)
            } else {
                AnthropicProvider::new(api_key)
            }
            .with_headers(extra_headers);
            registry.register("anthropic", Arc::new(provider));
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
                    name: model_cfg
                        .name
                        .clone()
                        .unwrap_or_else(|| model_cfg.id.clone()),
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

        Self {
            built_in,
            custom,
            provider_registry: registry,
        }
    }

    pub fn all_models(&self) -> Vec<&Model> {
        self.built_in.iter().chain(self.custom.iter()).collect()
    }

    pub fn available_models(&self) -> Vec<&Model> {
        self.all_models()
            .into_iter()
            .filter(|m| self.provider_registry.get(&m.provider_name).is_some())
            .collect()
    }

    pub fn get_model(&self, provider: &str, id: &str) -> Option<&Model> {
        self.all_models()
            .into_iter()
            .find(|m| m.provider_name == provider && m.id == id)
    }

    /// Find a model by partial/fuzzy match. Checks id, name, and common aliases.
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
        if let Some(ref id) = config.default_model
            && let Some(m) = self.find_model(id)
            && self.provider_registry.get(&m.provider_name).is_some()
        {
            return Some(m);
        }
        self.available_models().into_iter().next()
    }

    pub fn add_custom_provider(
        &mut self,
        cfg: CustomProviderConfig,
        nerv_dir: &std::path::Path,
    ) -> anyhow::Result<()> {
        let provider =
            OpenAICompatProvider::new(cfg.name.clone(), cfg.base_url.clone(), cfg.api_key.clone());
        self.provider_registry
            .register(&cfg.name, Arc::new(provider));

        for model_cfg in &cfg.models {
            self.custom.push(Model {
                id: model_cfg.id.clone(),
                name: model_cfg
                    .name
                    .clone()
                    .unwrap_or_else(|| model_cfg.id.clone()),
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
            pricing: ModelPricing {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
            },
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
            pricing: ModelPricing {
                input: 0.80,
                output: 4.0,
                cache_read: 0.08,
                cache_write: 1.0,
            },
        },
    ]
}

impl ModelPricing {
    fn default_custom() -> Self {
        Self {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        }
    }
}
