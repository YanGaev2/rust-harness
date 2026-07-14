use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::providers::ProviderConfig;

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<HarnessConfig, ConfigError> {
        match fs::read_to_string(&self.path) {
            Ok(raw) => Ok(serde_json::from_str(&raw)?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(HarnessConfig::default()),
            Err(err) => Err(ConfigError::Io(err)),
        }
    }

    pub fn save_provider(&self, provider: ProviderConfig) -> Result<(), ConfigError> {
        let mut config = self.load()?;
        config
            .providers
            .insert(provider.name().to_string(), provider);
        self.save(&config)
    }

    /// Set (or clear with `None`) the config-wide proxy that providers
    /// without their own `proxy` field inherit.
    pub fn set_proxy(&self, proxy: impl Into<String>) -> Result<(), ConfigError> {
        let mut config = self.load()?;
        let proxy = proxy.into();
        config.proxy = if proxy.trim().is_empty() {
            None
        } else {
            Some(proxy)
        };
        self.save(&config)
    }

    fn save(&self, config: &HarnessConfig) -> Result<(), ConfigError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(ConfigError::Io)?;
        }
        let raw = serde_json::to_string_pretty(config)?;
        fs::write(&self.path, raw).map_err(ConfigError::Io)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessConfig {
    providers: BTreeMap<String, ProviderConfig>,
    /// Config-wide proxy (URL, "env", or "none") inherited by every provider
    /// that does not set its own. Absent = all requests go direct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proxy: Option<String>,
}

impl HarnessConfig {
    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }

    pub fn providers(&self) -> impl Iterator<Item = &ProviderConfig> {
        self.providers.values()
    }

    pub fn proxy(&self) -> Option<&str> {
        self.proxy.as_deref()
    }

    /// The provider with the global proxy folded in: a provider without its
    /// own `proxy` inherits the config-wide one (an explicit per-provider
    /// value, including "none", always wins).
    pub fn resolved_provider(&self, name: &str) -> Option<ProviderConfig> {
        let provider = self.providers.get(name)?.clone();
        Some(match (provider.proxy(), self.proxy.as_deref()) {
            (None, Some(global)) => provider.with_proxy(global),
            _ => provider,
        })
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Json(err) => write!(f, "invalid harness config: {err}"),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
        }
    }
}

impl From<serde_json::Error> for ConfigError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}
