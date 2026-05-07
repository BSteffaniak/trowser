#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Chromium CDP browser backend for trowser.
//!
//! This crate is intentionally the only place where Chromium/CDP-specific
//! implementation details should live.

use std::path::PathBuf;

use trowser_browser::{
    BrowserBackend, BrowserCapabilities, BrowserError, BrowserResult, ExtractedPage,
    NavigateRequest,
};
use trowser_render::SemanticPage;

/// Configuration for the Chromium/CDP backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdpBackendConfig {
    /// Optional path to the Chromium-compatible browser binary.
    pub browser_binary: Option<PathBuf>,
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

/// Browser binary discovery result for `CDP` diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserDiscovery {
    /// First matching browser executable.
    pub binary: Option<PathBuf>,
    /// Candidate executable names that were checked.
    pub candidates: Vec<String>,
}

impl BrowserDiscovery {
    /// Returns true when a Chromium-compatible browser binary was found.
    #[must_use]
    pub const fn found(&self) -> bool {
        self.binary.is_some()
    }
}

/// Finds a Chromium-compatible browser binary on the current platform.
#[must_use]
pub fn discover_browser_binary(configured: Option<&PathBuf>) -> BrowserDiscovery {
    if let Some(path) = configured
        && path.exists()
    {
        return BrowserDiscovery {
            binary: Some(path.clone()),
            candidates: vec![path.to_string_lossy().into_owned()],
        };
    }

    let candidates = default_browser_candidates();
    let binary = candidates
        .iter()
        .find_map(|candidate| find_on_path(candidate));
    BrowserDiscovery { binary, candidates }
}

fn default_browser_candidates() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        vec![
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".to_string(),
            "/Applications/Chromium.app/Contents/MacOS/Chromium".to_string(),
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser".to_string(),
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".to_string(),
            "google-chrome".to_string(),
            "chromium".to_string(),
            "brave".to_string(),
            "microsoft-edge".to_string(),
        ]
    }
    #[cfg(target_os = "windows")]
    {
        vec![
            "chrome.exe".to_string(),
            "chromium.exe".to_string(),
            "brave.exe".to_string(),
            "msedge.exe".to_string(),
        ]
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        vec![
            "google-chrome".to_string(),
            "google-chrome-stable".to_string(),
            "chromium".to_string(),
            "chromium-browser".to_string(),
            "brave-browser".to_string(),
            "microsoft-edge".to_string(),
        ]
    }
}

fn find_on_path(candidate: &str) -> Option<PathBuf> {
    let path = PathBuf::from(candidate);
    if path.is_absolute() && path.exists() {
        return Some(path);
    }

    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        let path = dir.join(candidate);
        path.exists().then_some(path)
    })
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
