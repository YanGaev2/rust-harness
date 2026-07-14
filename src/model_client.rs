use std::error::Error;
use std::fmt;
use std::time::Duration;

use crate::providers::{ModelDiscovery, ModelDiscoveryError, ProviderConfig};

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
        let url = format!("{}/models", provider.base_url().trim_end_matches('/'));
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

        let response = request.call()?;
        let raw = response.into_body().read_to_string()?;
        Ok(ModelDiscovery::from_openai_compatible_response(
            provider, &raw,
        )?)
    }
}

#[derive(Debug)]
pub enum ModelClientError {
    Http(Box<ureq::Error>),
    Io(std::io::Error),
    Discovery(ModelDiscoveryError),
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
