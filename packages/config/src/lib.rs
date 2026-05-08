#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Configuration and path handling for trowser.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

/// Environment variable that points at an explicit config file.
pub const TROWSER_CONFIG_ENV: &str = "TROWSER_CONFIG";
/// Environment variable that overrides the config directory.
pub const TROWSER_CONFIG_DIR_ENV: &str = "TROWSER_CONFIG_DIR";
/// Environment variable that overrides the runtime directory.
pub const TROWSER_RUNTIME_DIR_ENV: &str = "TROWSER_RUNTIME_DIR";
/// Environment variable that selects an isolated runtime name.
pub const TROWSER_RUNTIME_NAME_ENV: &str = "TROWSER_RUNTIME_NAME";
/// Environment variable that overrides the data directory.
pub const TROWSER_DATA_DIR_ENV: &str = "TROWSER_DATA_DIR";
/// Environment variable that overrides the state directory.
pub const TROWSER_STATE_DIR_ENV: &str = "TROWSER_STATE_DIR";
/// Environment variable that overrides the log directory.
pub const TROWSER_LOG_DIR_ENV: &str = "TROWSER_LOG_DIR";
/// Internal environment variable used by the parent CLI to request daemon readiness signaling.
pub const TROWSER_DAEMON_READY_FILE_ENV: &str = "TROWSER_DAEMON_READY_FILE";

/// Config loading error.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Config file could not be read.
    #[error("failed to read config file {}: {source}", path.display())]
    Read {
        /// File path.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// Config file could not be parsed.
    #[error("failed to parse config file {}: {source}", path.display())]
    Parse {
        /// File path.
        path: PathBuf,
        /// TOML parse error.
        source: toml::de::Error,
    },
}

/// Result alias for config operations.
pub type ConfigResult<T> = Result<T, ConfigError>;

/// Browser backend configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserConfig {
    /// Backend id. The MVP value is `chromium-cdp`.
    pub backend: String,
    /// Optional browser binary path.
    pub binary: Option<PathBuf>,
    /// Launch backend in headless mode when supported.
    pub headless: bool,
    /// Browser profile mode/name: `default`, `temporary`, `private`, or a custom persistent profile name.
    pub profile: String,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            backend: "chromium-cdp".to_string(),
            binary: None,
            headless: true,
            profile: "default".to_string(),
        }
    }
}

/// Rendering configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderConfig {
    /// Prefer semantic/accessibility extraction before visual fallbacks.
    pub semantic_first: bool,
    /// Enable terminal image protocols when supported.
    pub images: bool,
    /// Maximum width used for plain text wrapping.
    pub max_width: u16,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            semantic_first: true,
            images: true,
            max_width: 100,
        }
    }
}

/// Resource and memory policy configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "config booleans map directly to independent resource classes"
)]
pub struct ResourcePolicyConfig {
    /// Optional maximum active tabs before the daemon should reject new tabs.
    ///
    /// `None` means unlimited tabs.
    pub max_tabs: Option<usize>,
    /// Block image requests by default.
    pub block_images: bool,
    /// Block video/audio requests by default.
    pub block_video: bool,
    /// Block web font requests by default.
    pub block_fonts: bool,
    /// Block common ad URL patterns by default.
    pub block_ads: bool,
    /// Block common tracker URL patterns by default.
    pub block_trackers: bool,
}

impl Default for ResourcePolicyConfig {
    fn default() -> Self {
        Self {
            max_tabs: None,
            block_images: false,
            block_video: true,
            block_fonts: false,
            block_ads: true,
            block_trackers: true,
        }
    }
}

/// Top-level trowser config.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TrowserConfig {
    /// Browser backend configuration.
    pub browser: BrowserConfig,
    /// Terminal rendering configuration.
    pub render: RenderConfig,
    /// Resource policy configuration.
    pub resources: ResourcePolicyConfig,
}

impl TrowserConfig {
    /// Loads config from the default path or an explicit env override.
    ///
    /// Missing config files are treated as defaults.
    ///
    /// # Errors
    ///
    /// Returns an error if an existing config file cannot be read or parsed.
    pub fn load(paths: &ConfigPaths) -> ConfigResult<Self> {
        let path =
            std::env::var_os(TROWSER_CONFIG_ENV).map_or_else(|| paths.config_file(), PathBuf::from);

        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        toml::from_str(&contents).map_err(|source| ConfigError::Parse { path, source })
    }
}

#[cfg(test)]
mod tests {
    use super::{ResourcePolicyConfig, TrowserConfig};

    #[test]
    fn resource_policy_defaults_to_unlimited_tabs() {
        assert_eq!(ResourcePolicyConfig::default().max_tabs, None);
    }

    #[test]
    fn config_accepts_explicit_max_tabs() {
        let config = toml::from_str::<TrowserConfig>("[resources]\nmax_tabs = 12\n")
            .expect("config should parse");

        assert_eq!(config.resources.max_tabs, Some(12));
    }
}

/// Platform-aware trowser paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    config_dir: PathBuf,
    runtime_dir: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
    log_dir: PathBuf,
    runtime_name: String,
}

impl ConfigPaths {
    /// Resolves paths from platform defaults and environment overrides.
    #[must_use]
    pub fn resolve() -> Self {
        let runtime_name = std::env::var(TROWSER_RUNTIME_NAME_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "default".to_string());

        let config_dir = env_path(TROWSER_CONFIG_DIR_ENV).unwrap_or_else(default_config_dir);
        let runtime_dir = env_path(TROWSER_RUNTIME_DIR_ENV)
            .unwrap_or_else(default_runtime_dir)
            .join(&runtime_name);
        let data_dir = env_path(TROWSER_DATA_DIR_ENV).unwrap_or_else(default_data_dir);
        let state_dir = env_path(TROWSER_STATE_DIR_ENV).unwrap_or_else(default_state_dir);
        let log_dir = env_path(TROWSER_LOG_DIR_ENV).unwrap_or_else(|| state_dir.join("logs"));

        Self {
            config_dir,
            runtime_dir,
            data_dir,
            state_dir,
            log_dir,
            runtime_name,
        }
    }

    /// Returns the selected runtime name.
    #[must_use]
    pub fn runtime_name(&self) -> &str {
        &self.runtime_name
    }

    /// Returns the config directory.
    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        self.config_dir.clone()
    }

    /// Returns the default config file path.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// Returns the runtime directory.
    #[must_use]
    pub fn runtime_dir(&self) -> PathBuf {
        self.runtime_dir.clone()
    }

    /// Returns the data directory.
    #[must_use]
    pub fn data_dir(&self) -> PathBuf {
        self.data_dir.clone()
    }

    /// Returns the state directory.
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.state_dir.clone()
    }

    /// Returns the log directory.
    #[must_use]
    pub fn log_dir(&self) -> PathBuf {
        self.log_dir.clone()
    }

    /// Returns the local daemon socket path on Unix-like platforms.
    #[must_use]
    pub fn server_socket_path(&self) -> PathBuf {
        self.runtime_dir.join("trowser.sock")
    }

    /// Returns the local daemon pid file path.
    #[must_use]
    pub fn daemon_pid_path(&self) -> PathBuf {
        self.runtime_dir.join("daemon.pid")
    }

    /// Returns the persisted session snapshot path.
    #[must_use]
    pub fn session_file(&self) -> PathBuf {
        self.state_dir.join(&self.runtime_name).join("session.json")
    }

    /// Returns the root directory for persistent Chromium profiles.
    #[must_use]
    pub fn profiles_dir(&self) -> PathBuf {
        self.data_dir.join("profiles")
    }
}

impl Default for ConfigPaths {
    fn default() -> Self {
        Self::resolve()
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

fn default_config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("trowser")
}

fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("trowser")
}

fn default_state_dir() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("trowser")
}

fn default_runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("trowser")
}
