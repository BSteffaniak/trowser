#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Chromium CDP browser backend for trowser.
//!
//! This crate is intentionally the only place where Chromium/CDP-specific
//! implementation details should live.

use trowser_browser::{
    BrowserBackend, BrowserCapabilities, BrowserError, BrowserResult, ExtractedPage,
    NavigateRequest,
};
use trowser_render::SemanticPage;

/// Configuration for the Chromium/CDP backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdpBackendConfig {
    /// Optional path to the Chromium-compatible browser binary.
    pub browser_binary: Option<String>,
    /// Whether to launch the browser in headless mode.
    pub headless: bool,
}

impl Default for CdpBackendConfig {
    fn default() -> Self {
        Self {
            browser_binary: None,
            headless: true,
        }
    }
}

/// Placeholder Chromium/CDP backend.
#[derive(Debug, Clone)]
pub struct CdpBackend {
    config: CdpBackendConfig,
}

impl CdpBackend {
    /// Creates a Chromium/CDP backend placeholder.
    #[must_use]
    pub const fn new(config: CdpBackendConfig) -> Self {
        Self { config }
    }

    /// Returns the backend config.
    #[must_use]
    pub const fn config(&self) -> &CdpBackendConfig {
        &self.config
    }
}

impl BrowserBackend for CdpBackend {
    fn capabilities(&self) -> BrowserCapabilities {
        BrowserCapabilities::default()
    }

    fn navigate(&mut self, _request: NavigateRequest) -> BrowserResult<()> {
        Err(BrowserError::NotConnected)
    }

    fn extract_page(
        &mut self,
        tab_id: &trowser_session_models::TabId,
    ) -> BrowserResult<ExtractedPage> {
        Ok(ExtractedPage {
            tab_id: tab_id.clone(),
            page: SemanticPage {
                url: String::new(),
                title: "CDP backend not connected".to_string(),
                blocks: Vec::new(),
            },
        })
    }
}
