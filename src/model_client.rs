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
        let agent = ureq::AgentBuilder::new().timeout(self.timeout).build();
        let mut request = agent.get(&url).set("Accept", "application/json");
        if let Some((name, value)) = provider.auth_header() {
            request = request.set(&name, &value);
        }

        let response = request.call()?;
        let raw = response.into_string().map_err(ModelClientError::Io)?;
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
