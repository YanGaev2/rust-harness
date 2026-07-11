use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    name: String,
    base_url: String,
    api_key: String,
    models: Vec<String>,
    #[serde(default)]
    auth_scheme: AuthScheme,
    #[serde(default)]
    cache_policy: CachePolicy,
    #[serde(default)]
    chat_api: ChatApiFormat,
    #[serde(default)]
    key_env: Option<String>,
}

impl ProviderConfig {
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            models: Vec::new(),
            auth_scheme: AuthScheme::Bearer,
            cache_policy: CachePolicy::default(),
            chat_api: ChatApiFormat::default(),
            key_env: None,
        }
    }

    pub fn subscription(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base_url: String::new(),
            api_key: String::new(),
            models: Vec::new(),
            auth_scheme: AuthScheme::Subscription,
            cache_policy: CachePolicy::default(),
            chat_api: ChatApiFormat::default(),
            key_env: None,
        }
    }

    pub fn from_profile(profile: ProviderProfile) -> Self {
        let mut provider = Self::subscription(profile.name)
            .with_auth_scheme(profile.auth_scheme)
            .with_cache_policy(profile.cache_policy)
            .with_chat_api(profile.chat_api)
            .with_key_env(profile.key_env);
        for model in profile.model_hints {
            provider = provider.with_model(*model);
        }
        provider
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.models.push(model.into());
        self.models.sort();
        self.models.dedup();
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = api_key.into();
        self
    }

    pub fn with_auth_scheme(mut self, auth_scheme: AuthScheme) -> Self {
        self.auth_scheme = auth_scheme;
        self
    }

    pub fn with_cache_policy(mut self, cache_policy: CachePolicy) -> Self {
        self.cache_policy = cache_policy;
        self
    }

    pub fn with_chat_api(mut self, chat_api: ChatApiFormat) -> Self {
        self.chat_api = chat_api;
        self
    }

    pub fn with_key_env(mut self, key_env: impl Into<String>) -> Self {
        self.key_env = Some(key_env.into());
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn models(&self) -> &[String] {
        &self.models
    }

    pub fn auth_scheme(&self) -> AuthScheme {
        self.auth_scheme.clone()
    }

    pub fn cache_policy(&self) -> CachePolicy {
        self.cache_policy.clone()
    }

    pub fn chat_api(&self) -> ChatApiFormat {
        self.chat_api
    }

    pub fn key_env(&self) -> Option<&str> {
        self.key_env.as_deref()
    }

    pub fn auth_header(&self) -> Option<(String, String)> {
        self.auth_header_with_lookup(|name| std::env::var(name).ok())
    }

    pub fn auth_header_with_lookup<F>(&self, lookup: F) -> Option<(String, String)>
    where
        F: FnOnce(&str) -> Option<String>,
    {
        let api_key = self.resolved_api_key(lookup)?;

        match &self.auth_scheme {
            AuthScheme::Bearer | AuthScheme::Subscription => {
                Some(("Authorization".to_string(), format!("Bearer {api_key}")))
            }
            AuthScheme::Header { name } => Some((name.clone(), api_key)),
        }
    }

    fn resolved_api_key<F>(&self, lookup: F) -> Option<String>
    where
        F: FnOnce(&str) -> Option<String>,
    {
        if !self.api_key.trim().is_empty() {
            return Some(self.api_key.clone());
        }

        self.key_env
            .as_deref()
            .and_then(lookup)
            .filter(|value| !value.trim().is_empty())
    }

    pub fn cache_header(&self, cache_key: &str) -> Option<(String, String)> {
        match &self.cache_policy {
            CachePolicy::Disabled => None,
            CachePolicy::Automatic { .. } => None,
            CachePolicy::AnthropicAutomatic { .. } => None,
            CachePolicy::BodyCacheControl { .. } => None,
            CachePolicy::Header { name } => Some((name.clone(), cache_key.to_string())),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthScheme {
    #[default]
    Bearer,
    Header {
        name: String,
    },
    Subscription,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CachePolicy {
    Disabled,
    Header {
        name: String,
    },
    Automatic {
        hit_tokens_field: String,
        miss_tokens_field: String,
    },
    AnthropicAutomatic {
        ttl: Option<String>,
    },
    BodyCacheControl {
        ttl: Option<String>,
    },
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self::Header {
            name: "X-Harness-Cache-Key".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatApiFormat {
    #[default]
    OpenAiCompatible,
    OpenAiResponses,
    OpenAiCodexResponses,
    AnthropicMessages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinProvider {
    Codex,
    Xiaomi,
    Glm,
    Kimi,
    Claude,
    DeepSeek,
}

impl BuiltinProvider {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "codex" => Some(Self::Codex),
            "xiaomi" => Some(Self::Xiaomi),
            "glm" => Some(Self::Glm),
            "kimi" => Some(Self::Kimi),
            "claude" => Some(Self::Claude),
            "deepseek" => Some(Self::DeepSeek),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Xiaomi => "xiaomi",
            Self::Glm => "glm",
            Self::Kimi => "kimi",
            Self::Claude => "claude",
            Self::DeepSeek => "deepseek",
        }
    }

    pub fn profile(self) -> ProviderProfile {
        match self {
            Self::Codex => ProviderProfile {
                name: self.name(),
                key_env: "OPENAI_API_KEY",
                model_hints: &["gpt-5-codex"],
                auth_scheme: AuthScheme::Subscription,
                cache_policy: CachePolicy::default(),
                chat_api: ChatApiFormat::OpenAiCodexResponses,
            },
            Self::Xiaomi => ProviderProfile {
                name: self.name(),
                key_env: "XIAOMI_API_KEY",
                model_hints: &["xiaomi-lm"],
                auth_scheme: AuthScheme::Subscription,
                cache_policy: CachePolicy::default(),
                chat_api: ChatApiFormat::OpenAiCompatible,
            },
            Self::Glm => ProviderProfile {
                name: self.name(),
                key_env: "GLM_API_KEY",
                model_hints: &["glm-4.5", "glm-4.5-air"],
                auth_scheme: AuthScheme::Subscription,
                cache_policy: CachePolicy::default(),
                chat_api: ChatApiFormat::OpenAiCompatible,
            },
            Self::Kimi => ProviderProfile {
                name: self.name(),
                key_env: "KIMI_API_KEY",
                model_hints: &["kimi-k2", "moonshot-v1-128k"],
                auth_scheme: AuthScheme::Subscription,
                cache_policy: CachePolicy::default(),
                chat_api: ChatApiFormat::OpenAiCompatible,
            },
            Self::Claude => ProviderProfile {
                name: self.name(),
                key_env: "ANTHROPIC_API_KEY",
                model_hints: &["claude-sonnet-4.5"],
                auth_scheme: AuthScheme::Header {
                    name: "x-api-key".to_string(),
                },
                cache_policy: CachePolicy::AnthropicAutomatic { ttl: None },
                chat_api: ChatApiFormat::AnthropicMessages,
            },
            Self::DeepSeek => ProviderProfile {
                name: self.name(),
                key_env: "DEEPSEEK_API_KEY",
                model_hints: &["deepseek-v4-pro", "deepseek-v4-flash"],
                auth_scheme: AuthScheme::Subscription,
                cache_policy: CachePolicy::Automatic {
                    hit_tokens_field: "prompt_cache_hit_tokens".to_string(),
                    miss_tokens_field: "prompt_cache_miss_tokens".to_string(),
                },
                chat_api: ChatApiFormat::OpenAiCompatible,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProfile {
    pub name: &'static str,
    pub key_env: &'static str,
    pub model_hints: &'static [&'static str],
    pub auth_scheme: AuthScheme,
    pub cache_policy: CachePolicy,
    pub chat_api: ChatApiFormat,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, ProviderConfig>,
}

impl ProviderRegistry {
    pub fn with_builtin_subscriptions() -> Self {
        let mut registry = Self::default();
        for provider in [
            BuiltinProvider::Codex,
            BuiltinProvider::Xiaomi,
            BuiltinProvider::Glm,
            BuiltinProvider::Kimi,
            BuiltinProvider::Claude,
            BuiltinProvider::DeepSeek,
        ] {
            registry.add_provider(ProviderConfig::from_profile(provider.profile()));
        }
        registry
    }

    pub fn add_provider(&mut self, provider: ProviderConfig) {
        self.providers.insert(provider.name.clone(), provider);
    }

    pub fn get(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChoice {
    AddAll,
    Model(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDiscovery {
    provider_name: String,
    model_ids: Vec<String>,
    choices: Vec<ModelChoice>,
}

impl ModelDiscovery {
    pub fn from_openai_compatible_response(
        provider: &ProviderConfig,
        raw_json: &str,
    ) -> Result<Self, ModelDiscoveryError> {
        let parsed: OpenAiModelsResponse = serde_json::from_str(raw_json)?;
        let mut model_ids = parsed
            .data
            .into_iter()
            .map(|entry| entry.id)
            .filter(|id| !id.trim().is_empty())
            .collect::<Vec<_>>();
        model_ids.sort();
        model_ids.dedup();

        let mut choices = Vec::with_capacity(model_ids.len() + 1);
        choices.push(ModelChoice::AddAll);
        choices.extend(model_ids.iter().cloned().map(ModelChoice::Model));

        Ok(Self {
            provider_name: provider.name().to_string(),
            model_ids,
            choices,
        })
    }

    pub fn provider_name(&self) -> &str {
        &self.provider_name
    }

    pub fn choices(&self) -> &[ModelChoice] {
        &self.choices
    }

    pub fn add_all_model_ids(&self) -> Vec<String> {
        self.model_ids.clone()
    }
}

#[derive(Debug)]
pub enum ModelDiscoveryError {
    InvalidJson(serde_json::Error),
}

impl fmt::Display for ModelDiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(err) => write!(f, "invalid model list response: {err}"),
        }
    }
}

impl Error for ModelDiscoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidJson(err) => Some(err),
        }
    }
}

impl From<serde_json::Error> for ModelDiscoveryError {
    fn from(value: serde_json::Error) -> Self {
        Self::InvalidJson(value)
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModelEntry>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelEntry {
    id: String,
}
