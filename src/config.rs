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

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(ConfigError::Io)?;
        }

        let raw = serde_json::to_string_pretty(&config)?;
        fs::write(&self.path, raw).map_err(ConfigError::Io)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessConfig {
    providers: BTreeMap<String, ProviderConfig>,
}

impl HarnessConfig {
    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }

    pub fn providers(&self) -> impl Iterator<Item = &ProviderConfig> {
        self.providers.values()
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
