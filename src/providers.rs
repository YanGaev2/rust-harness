use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Proxy for this provider's requests: an `http://[user:pass@]host:port`
    /// URL, `"env"` (use HTTP_PROXY/HTTPS_PROXY from the environment), or
    /// `"none"` (force direct even when a global proxy is configured).
    /// `None` inherits the config-level proxy, defaulting to direct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proxy: Option<String>,
    /// Provider-specific JSON object merged into the top level of every
    /// request body (OpenAI-compatible format only), e.g. OpenRouter's
    /// `{"provider": {"only": ["wandb"]}}` routing or DashScope's
    /// `{"enable_thinking": false}`. Core fields (model, messages, tools,
    /// stream) always win on key collision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extra_body: Option<serde_json::Value>,
    /// What the provider's `/models` list reported per model (context
    /// window, prices) when the provider was added.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    model_info: BTreeMap<String, ModelInfo>,
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
            proxy: None,
            extra_body: None,
            model_info: BTreeMap::new(),
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
            proxy: None,
            extra_body: None,
            model_info: BTreeMap::new(),
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

    pub fn with_proxy(mut self, proxy: impl Into<String>) -> Self {
        self.proxy = Some(proxy.into());
        self
    }

    /// Proxy directive for this provider (URL, "env", or "none"); `None`
    /// means "inherit the global config proxy, default direct".
    pub fn proxy(&self) -> Option<&str> {
        self.proxy.as_deref()
    }

    pub fn with_extra_body(mut self, extra_body: serde_json::Value) -> Self {
        self.extra_body = Some(extra_body);
        self
    }

    /// Extra JSON merged into the top level of OpenAI-compatible request
    /// bodies; core fields win on key collision.
    pub fn extra_body(&self) -> Option<&serde_json::Value> {
        self.extra_body.as_ref()
    }

    pub fn key_env(&self) -> Option<&str> {
        self.key_env.as_deref()
    }

    /// Add `models` together with what `discovery` reported about them.
    pub fn with_discovered_models(mut self, discovery: &ModelDiscovery, models: &[String]) -> Self {
        for model in models {
            let info = discovery.model_info(model);
            if info != ModelInfo::default() {
                self.model_info.insert(model.clone(), info);
            }
            self = self.with_model(model.clone());
        }
        self
    }

    /// What `/models` reported for `model` when the provider was added.
    pub fn model_info(&self, model: &str) -> Option<&ModelInfo> {
        self.model_info.get(model)
    }

    /// Prices for `model`: the saved `/models` prices first, then the
    /// built-in price list.
    pub fn pricing(&self, model: &str) -> Option<ModelPricing> {
        self.model_info
            .get(model)
            .and_then(|info| info.pricing.clone())
            .or_else(|| builtin_pricing(&self.name, model))
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
    /// Every builtin family, in the order UIs list them. The single roster —
    /// the setup TUI and `provider subscriptions` derive from it.
    pub const ALL: &[Self] = &[
        Self::Codex,
        Self::Xiaomi,
        Self::Glm,
        Self::Kimi,
        Self::Claude,
        Self::DeepSeek,
    ];

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
                base_url: None,
            },
            Self::Xiaomi => ProviderProfile {
                name: self.name(),
                key_env: "XIAOMI_API_KEY",
                model_hints: &["xiaomi-lm"],
                auth_scheme: AuthScheme::Subscription,
                cache_policy: CachePolicy::default(),
                chat_api: ChatApiFormat::OpenAiCompatible,
                base_url: None,
            },
            Self::Glm => ProviderProfile {
                name: self.name(),
                key_env: "GLM_API_KEY",
                model_hints: &["glm-5.2"],
                auth_scheme: AuthScheme::Subscription,
                cache_policy: CachePolicy::default(),
                chat_api: ChatApiFormat::OpenAiCompatible,
                base_url: Some("https://api.z.ai/api/paas/v4"),
            },
            Self::Kimi => ProviderProfile {
                name: self.name(),
                key_env: "KIMI_API_KEY",
                model_hints: &["kimi-k2", "moonshot-v1-128k"],
                auth_scheme: AuthScheme::Subscription,
                cache_policy: CachePolicy::default(),
                chat_api: ChatApiFormat::OpenAiCompatible,
                base_url: None,
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
                base_url: None,
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
                base_url: Some("https://api.deepseek.com/v1"),
            },
        }
    }
}

/// Provider seeded from the builtin profile when the name matches a family,
/// plain otherwise.
pub fn provider_from_profile(name: String, url: String, key: String) -> ProviderConfig {
    if let Some(builtin) = BuiltinProvider::from_name(&name) {
        let profile = builtin.profile();
        ProviderConfig::new(profile.name, url, key)
            .with_auth_scheme(profile.auth_scheme)
            .with_cache_policy(profile.cache_policy)
            .with_chat_api(profile.chat_api)
            .with_key_env(profile.key_env)
    } else {
        ProviderConfig::new(name, url, key)
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
    /// Endpoint for the preset flow (pick provider → paste key → done).
    /// Only bench-verified families carry one; the rest require --url.
    pub base_url: Option<&'static str>,
}

/// USD per 1M tokens, from the provider's official price page or its
/// `/models` list on `as_of`. Estimates only — prices change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub cached_input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub as_of: String,
}

/// What a provider's `/models` entry reports about one model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricing>,
}

impl ModelPricing {
    /// Cache-aware cost estimate: cached prompt tokens bill at the cached
    /// rate, the remainder at the full input rate.
    pub fn estimate_usd(
        &self,
        prompt_tokens: u64,
        cached_tokens: u64,
        completion_tokens: u64,
    ) -> f64 {
        let cached = cached_tokens.min(prompt_tokens) as f64;
        let fresh = prompt_tokens.saturating_sub(cached_tokens) as f64;
        (fresh * self.input_per_mtok
            + cached * self.cached_input_per_mtok
            + completion_tokens as f64 * self.output_per_mtok)
            / 1_000_000.0
    }
}

/// Built-in price list for the bench-verified models. Unknown models get
/// an honest `None` rather than a guess.
pub fn builtin_pricing(provider: &str, model: &str) -> Option<ModelPricing> {
    let family = BuiltinProvider::from_name(provider)?;
    match (family, model) {
        (BuiltinProvider::Glm, "glm-5.2") => Some(ModelPricing {
            input_per_mtok: 1.40,
            cached_input_per_mtok: 0.26,
            output_per_mtok: 4.40,
            as_of: "2026-07-13".to_string(),
        }),
        (BuiltinProvider::DeepSeek, "deepseek-v4-pro") => Some(ModelPricing {
            input_per_mtok: 0.435,
            cached_input_per_mtok: 0.003625,
            output_per_mtok: 0.87,
            as_of: "2026-07-13".to_string(),
        }),
        (BuiltinProvider::DeepSeek, "deepseek-v4-flash" | "deepseek-chat") => Some(ModelPricing {
            input_per_mtok: 0.14,
            cached_input_per_mtok: 0.0028,
            output_per_mtok: 0.28,
            as_of: "2026-07-13".to_string(),
        }),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChoice {
    AddAll,
    Model(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelDiscovery {
    provider_name: String,
    model_ids: Vec<String>,
    choices: Vec<ModelChoice>,
    /// Context window and prices per model, for the entries that report them.
    info: BTreeMap<String, ModelInfo>,
    /// The list has the Anthropic Models API shape (`has_more` paging).
    anthropic_shape: bool,
}

impl ModelDiscovery {
    pub fn from_openai_compatible_response(
        provider: &ProviderConfig,
        raw_json: &str,
    ) -> Result<Self, ModelDiscoveryError> {
        let parsed: OpenAiModelsResponse = serde_json::from_str(raw_json)?;
        let info = parsed
            .data
            .iter()
            .filter_map(|entry| {
                let info = ModelInfo {
                    context_window: entry
                        .context_length
                        .or(entry.context_window)
                        .or(entry.max_model_len)
                        .or(entry.max_input_tokens),
                    pricing: entry.pricing.as_ref().and_then(per_token_pricing),
                };
                (info != ModelInfo::default()).then(|| (entry.id.clone(), info))
            })
            .collect();
        let anthropic_shape = parsed.has_more.is_some();
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
            info,
            anthropic_shape,
        })
    }

    /// The model's context window in tokens, when its `/models` entry
    /// reports one.
    pub fn context_window(&self, model: &str) -> Option<u64> {
        self.info.get(model).and_then(|info| info.context_window)
    }

    /// Everything the model's `/models` entry reported (empty when nothing).
    pub fn model_info(&self, model: &str) -> ModelInfo {
        self.info.get(model).cloned().unwrap_or_default()
    }

    /// True when the list came from an Anthropic Models API endpoint.
    pub fn is_anthropic_shape(&self) -> bool {
        self.anthropic_shape
    }

    /// Stamp the prices read from this list with the day it was fetched.
    pub fn with_pricing_date(mut self, date: &str) -> Self {
        for pricing in self
            .info
            .values_mut()
            .filter_map(|info| info.pricing.as_mut())
        {
            pricing.as_of = date.to_string();
        }
        self
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

/// Resolve a single `/model ARG` token against a `(provider, models)`
/// catalog. Priority: exact provider name (→ its first model), then a model
/// owned by exactly one provider, then a brand-new model on the active
/// provider. Ambiguous model names fall through to the active provider so a
/// duplicate name never silently switches providers.
/// Resolve a `/model` argument shared by both front ends: a number is a
/// 1-based index into the flattened (provider, model) menu, anything else
/// goes through [`resolve_model_shorthand`].
pub fn resolve_model_arg(
    catalog: &[(String, Vec<String>)],
    active_provider: &str,
    arg: &str,
) -> Result<(String, String), String> {
    if let Ok(number) = arg.parse::<usize>() {
        return number
            .checked_sub(1)
            .and_then(|index| {
                catalog
                    .iter()
                    .flat_map(|(provider, models)| {
                        models
                            .iter()
                            .map(move |model| (provider.clone(), model.clone()))
                    })
                    .nth(index)
            })
            .ok_or_else(|| format!("no menu entry {number} (run /model to see the list)"));
    }
    resolve_model_shorthand(catalog, active_provider, arg)
}

pub fn resolve_model_shorthand(
    catalog: &[(String, Vec<String>)],
    active_provider: &str,
    arg: &str,
) -> Result<(String, String), String> {
    if let Some((name, models)) = catalog.iter().find(|(name, _)| name == arg) {
        return match models.first() {
            Some(model) => Ok((name.clone(), model.clone())),
            None => Err(format!("provider {arg} has no configured models")),
        };
    }

    let owners: Vec<&String> = catalog
        .iter()
        .filter(|(_, models)| models.iter().any(|model| model == arg))
        .map(|(name, _)| name)
        .collect();
    if owners.len() == 1 {
        return Ok((owners[0].clone(), arg.to_string()));
    }

    if catalog.iter().any(|(name, _)| name == active_provider) {
        return Ok((active_provider.to_string(), arg.to_string()));
    }
    Err(format!(
        "unknown model {arg} and no active provider to attach it to"
    ))
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
    /// Only the Anthropic Models API pages with `has_more`.
    #[serde(default)]
    has_more: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModelEntry {
    id: String,
    /// The context window under the names providers use: baseten and
    /// OpenRouter `context_length`, Groq-style `context_window`, vLLM
    /// `max_model_len`.
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    max_model_len: Option<u64>,
    /// Anthropic Models API name for the context window.
    #[serde(default)]
    max_input_tokens: Option<u64>,
    /// OpenRouter prices: USD per token, as decimal strings.
    #[serde(default)]
    pricing: Option<serde_json::Value>,
}

/// Convert OpenRouter-style per-token prices (`prompt`, `completion`,
/// optional `input_cache_read`; strings or numbers) to USD per 1M tokens.
/// Negative prices (OpenRouter's `-1` for routers) mean "unknown".
fn per_token_pricing(pricing: &serde_json::Value) -> Option<ModelPricing> {
    let price = |field: &str| {
        let per_token = match pricing.get(field)? {
            serde_json::Value::String(text) => text.trim().parse::<f64>().ok()?,
            other => other.as_f64()?,
        };
        (per_token >= 0.0).then_some(per_token * 1_000_000.0)
    };
    let input = price("prompt")?;
    let output = price("completion")?;
    Some(ModelPricing {
        input_per_mtok: input,
        cached_input_per_mtok: price("input_cache_read").unwrap_or(input),
        output_per_mtok: output,
        as_of: String::new(),
    })
}
