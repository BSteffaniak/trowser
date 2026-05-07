#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Daemon runtime for trowser.

use trowser_config::ConfigPaths;
use trowser_ipc::{ProtocolCapabilities, ServerStatus};

/// Daemon runtime state.
#[derive(Debug, Clone)]
pub struct TrowserServer {
    paths: ConfigPaths,
}

impl TrowserServer {
    /// Creates a daemon runtime placeholder.
    #[must_use]
    pub const fn new(paths: ConfigPaths) -> Self {
        Self { paths }
    }

    /// Returns current daemon status.
    #[must_use]
    pub fn status(&self) -> ServerStatus {
        ServerStatus {
            version: env!("CARGO_PKG_VERSION").to_string(),
            runtime_name: self.paths.runtime_name().to_string(),
        }
    }

    /// Returns currently supported protocol capabilities.
    #[must_use]
    pub const fn capabilities() -> ProtocolCapabilities {
        ProtocolCapabilities {
            semantic_snapshots: true,
            tabs: true,
            navigation: true,
        }
    }

    /// Returns daemon paths.
    #[must_use]
    pub const fn paths(&self) -> &ConfigPaths {
        &self.paths
    }
}
