#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Backend-neutral browser abstractions for trowser.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use trowser_render::SemanticPage;
use trowser_session_models::TabId;

/// Browser backend family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserBackendKind {
    /// Chromium-compatible backend controlled via Chrome `DevTools` Protocol.
    ChromiumCdp,
    /// Future `WebDriver` `BiDi` backend.
    WebDriverBidi,
    /// Custom backend implementation.
    Custom,
}

/// Capabilities exposed by a browser backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserCapabilities {
    /// Backend family.
    pub kind: BrowserBackendKind,
    /// Whether the backend can provide accessibility tree snapshots.
    pub accessibility_tree: bool,
    /// Whether the backend can provide DOM snapshots.
    pub dom_snapshot: bool,
    /// Whether the backend can capture screenshots.
    pub screenshot: bool,
}

impl Default for BrowserCapabilities {
    fn default() -> Self {
        Self {
            kind: BrowserBackendKind::ChromiumCdp,
            accessibility_tree: true,
            dom_snapshot: true,
            screenshot: true,
        }
    }
}

/// Browser navigation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavigateRequest {
    /// Target tab id.
    pub tab_id: TabId,
    /// Target URL.
    pub url: String,
}

/// Result of a page extraction operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedPage {
    /// Tab id the page belongs to.
    pub tab_id: TabId,
    /// Renderable semantic page.
    pub page: SemanticPage,
}

/// Browser abstraction error.
#[derive(Debug, Error)]
pub enum BrowserError {
    /// Backend is not connected.
    #[error("browser backend is not connected")]
    NotConnected,
    /// Backend operation failed.
    #[error("browser backend operation failed: {0}")]
    Backend(String),
}

/// Convenient result alias for browser operations.
pub type BrowserResult<T> = Result<T, BrowserError>;

/// Backend-neutral browser control surface.
pub trait BrowserBackend {
    /// Returns backend capabilities.
    fn capabilities(&self) -> BrowserCapabilities;

    /// Navigates a tab to a URL.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend is unavailable or navigation fails.
    fn navigate(&mut self, request: NavigateRequest) -> BrowserResult<()>;

    /// Extracts the current semantic page for a tab.
    ///
    /// # Errors
    ///
    /// Returns an error if extraction fails or the tab does not exist.
    fn extract_page(&mut self, tab_id: &TabId) -> BrowserResult<ExtractedPage>;
}
