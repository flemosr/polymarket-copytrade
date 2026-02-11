use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default config file path.
pub const CONFIG_PATH: &str = "config.toml";

/// Top-level application config deserialized from `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub account: AccountConfig,
    #[serde(default)]
    pub settings: SettingsConfig,
}

/// Account credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Hex-encoded private key (with or without 0x prefix).
    pub private_key: String,
}

/// Runtime settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsConfig {
    /// Polling interval in seconds for trade detection.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
}

fn default_poll_interval() -> u64 {
    10
}

impl Default for SettingsConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval(),
        }
    }
}

impl AppConfig {
    /// Load config from the given TOML file path.
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(config)
    }

    /// Write config to the given TOML file path.
    pub fn save(&self, path: &Path) -> Result<()> {
        let contents = toml::to_string_pretty(self).context("failed to serialize config")?;
        std::fs::write(path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }
}
