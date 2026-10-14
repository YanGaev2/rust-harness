use std::error::Error;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::providers::{
    AuthScheme, BuiltinProvider, CachePolicy, ChatApiFormat, ModelDiscovery, ModelDiscoveryError,
    ProviderConfig,
};

/// The Anthropic Models API requires a version header.
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleModelClient {
    timeout: Duration,
}

impl OpenAiCompatibleModelClient {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    pub fn list_models(
        &self,
        provider: &ProviderConfig,
    ) -> Result<ModelDiscovery, ModelClientError> {
        let anthropic = provider.chat_api() == ChatApiFormat::AnthropicMessages;
        let mut url = format!("{}/models", provider.base_url().trim_end_matches('/'));
        if anthropic {
            // The Anthropic list pages 20 models by default; 1000 is its maximum.
            url.push_str("?limit=1000");
        }
        // Same proxy policy as the chat client: config opt-in only, ambient
        // env proxies are ignored.
        let proxy = match provider.proxy().map(str::trim) {
            None | Some("") | Some("none") => None,
            Some("env") => ureq::Proxy::try_from_env(),
            Some(proxy_url) => ureq::Proxy::new(proxy_url).ok(),
        };
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .proxy(proxy)
            .build()
            .into();
        let mut request = agent.get(&url).header("Accept", "application/json");
        if let Some((name, value)) = provider.auth_header() {
            request = request.header(&name, &value);
        }
        if anthropic {
            request = request.header("anthropic-version", ANTHROPIC_VERSION);
        }

        let response = request.call()?;
        let raw = response.into_body().read_to_string()?;
        Ok(ModelDiscovery::from_openai_compatible_response(
            provider, &raw,
        )?)
    }

    /// Fetch the model list for a provider being added and settle its API
    /// format. A builtin family keeps its profile. Any other provider is
    /// asked OpenAI-style first; when the key is rejected (401/403) the
    /// request is repeated Anthropic-style (`x-api-key`). A list with the
    /// Anthropic shape switches the provider to the Messages API. Prices
    /// are stamped with today's date.
    pub fn discover(
        &self,
        provider: ProviderConfig,
    ) -> Result<(ProviderConfig, ModelDiscovery), ModelClientError> {
        let pinned = BuiltinProvider::from_name(provider.name()).is_some()
            || provider.chat_api() != ChatApiFormat::OpenAiCompatible;
        let (provider, discovery) = match self.list_models(&provider) {
            Ok(discovery) if !pinned && discovery.is_anthropic_shape() => {
                (anthropic_format(provider), discovery)
            }
            Ok(discovery) => (provider, discovery),
            Err(err) if !pinned && err.is_auth_rejection() => {
                let anthropic = anthropic_format(provider).with_auth_scheme(AuthScheme::Header {
                    name: "x-api-key".to_string(),
                });
                match self.list_models(&anthropic) {
                    Ok(discovery) => (anthropic, discovery),
                    Err(_) => return Err(err),
                }
            }
            Err(err) => return Err(err),
        };
        Ok((provider, discovery.with_pricing_date(&today())))
    }
}

fn anthropic_format(provider: ProviderConfig) -> ProviderConfig {
    provider
        .with_chat_api(ChatApiFormat::AnthropicMessages)
        .with_cache_policy(CachePolicy::AnthropicAutomatic { ttl: None })
}

/// Today's UTC date, `YYYY-MM-DD`.
fn today() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    crate::session::format_utc_timestamp(secs)[..10].to_string()
}

#[derive(Debug)]
pub enum ModelClientError {
    Http(Box<ureq::Error>),
    Io(std::io::Error),
    Discovery(ModelDiscoveryError),
}

impl ModelClientError {
    /// The server rejected the credentials (HTTP 401 or 403).
    pub fn is_auth_rejection(&self) -> bool {
        matches!(self, Self::Http(err) if matches!(**err, ureq::Error::StatusCode(401 | 403)))
    }
}

impl fmt::Display for ModelClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(err) => write!(f, "model list request failed: {err}"),
            Self::Io(err) => write!(f, "model list response read failed: {err}"),
            Self::Discovery(err) => write!(f, "{err}"),
        }
    }
}

impl Error for ModelClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Http(err) => Some(err.as_ref()),
            Self::Io(err) => Some(err),
            Self::Discovery(err) => Some(err),
        }
    }
}

impl From<ureq::Error> for ModelClientError {
    fn from(value: ureq::Error) -> Self {
        Self::Http(Box::new(value))
    }
}

impl From<ModelDiscoveryError> for ModelClientError {
    fn from(value: ModelDiscoveryError) -> Self {
        Self::Discovery(value)
    }
}
