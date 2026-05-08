#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Chromium `CDP` browser backend for trowser.
//!
//! This crate is intentionally the only place where Chromium/CDP-specific
//! implementation details should live.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::Message;
use trowser_browser::{
    BrowserBackend, BrowserCapabilities, BrowserError, BrowserResult, ExtractedPage,
    NavigateRequest,
};
use trowser_render::{
    BlockLayout, SemanticBlock, SemanticLayoutNode, SemanticPage, SemanticRegion, SemanticViewport,
    TextRun,
};
use trowser_session_models::{ActionId, TabId};

/// Configuration for the Chromium/CDP backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdpBackendConfig {
    /// Optional path to the Chromium-compatible browser binary.
    pub browser_binary: Option<PathBuf>,
    /// Whether to launch the browser in headless mode.
    pub headless: bool,
    /// Resource blocking policy applied to new page targets.
    pub resource_policy: CdpResourcePolicy,
    /// Optional persistent Chromium user-data-dir.
    pub profile_dir: Option<PathBuf>,
}

impl Default for CdpBackendConfig {
    fn default() -> Self {
        Self {
            browser_binary: None,
            headless: true,
            resource_policy: CdpResourcePolicy::default(),
            profile_dir: None,
        }
    }
}

/// Resource blocking policy for Chromium page targets.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "resource policy booleans map directly to independent CDP URL pattern groups"
)]
pub struct CdpResourcePolicy {
    /// Block image requests.
    pub block_images: bool,
    /// Block media requests.
    pub block_media: bool,
    /// Block web font requests.
    pub block_fonts: bool,
    /// Block common ad URL patterns.
    pub block_ads: bool,
    /// Block common tracker URL patterns.
    pub block_trackers: bool,
}

impl Default for CdpResourcePolicy {
    fn default() -> Self {
        Self {
            block_images: false,
            block_media: true,
            block_fonts: false,
            block_ads: true,
            block_trackers: true,
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

/// Errors produced by the Chromium `CDP` backend.
#[derive(Debug, Error)]
pub enum CdpError {
    /// No Chromium-compatible browser binary could be found.
    #[error("Chromium-compatible browser binary was not found")]
    BrowserNotFound,
    /// No available local port could be allocated for `CDP`.
    #[error("failed to allocate local CDP port: {0}")]
    Port(std::io::Error),
    /// Browser process failed to spawn.
    #[error("failed to launch browser: {0}")]
    Launch(std::io::Error),
    /// Browser temporary profile directory could not be created.
    #[error("failed to create temporary browser profile: {0}")]
    TempDir(std::io::Error),
    /// Browser did not expose a page target in time.
    #[error("timed out waiting for Chromium DevTools endpoint")]
    DevToolsTimeout,
    /// HTTP request to the `CDP` discovery endpoint failed.
    #[error("CDP HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// WebSocket transport failed.
    #[error("CDP WebSocket failed: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    /// `CDP` JSON serialization failed.
    #[error("CDP JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    /// `CDP` returned an error object.
    #[error("CDP method {method} failed: {message}")]
    Method { method: String, message: String },
    /// Expected `CDP` response field was missing.
    #[error("CDP response missing field: {0}")]
    MissingField(&'static str),
    /// Requested browser history entry does not exist.
    #[error("no matching browser navigation history entry")]
    NavigationHistory,
    /// Requested page action could not be performed.
    #[error("CDP page action failed: {0}")]
    PageAction(String),
    /// Extracted page data had an unexpected shape.
    #[error("CDP semantic extraction failed: {0}")]
    Extraction(String),
}

/// Result alias for Chromium `CDP` operations.
pub type CdpResult<T> = Result<T, CdpError>;

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

/// Persistent Chromium process controlled by `CDP`.
pub struct CdpBrowser {
    child: Child,
    _profile: Option<tempfile::TempDir>,
    port: u16,
    resource_policy: CdpResourcePolicy,
    pages: BTreeMap<TabId, CdpPage>,
}

impl std::fmt::Debug for CdpBrowser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CdpBrowser")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl CdpBrowser {
    /// Launches a persistent Chromium process for repeated navigations.
    ///
    /// # Errors
    ///
    /// Returns an error if a browser binary cannot be found, a debug port cannot
    /// be allocated, a profile directory cannot be created, or Chromium fails to spawn.
    pub async fn launch(config: &CdpBackendConfig) -> CdpResult<Self> {
        let discovery = discover_browser_binary(config.browser_binary.as_ref());
        let browser_binary = discovery.binary.ok_or(CdpError::BrowserNotFound)?;
        let port = allocate_local_port().map_err(CdpError::Port)?;
        let (profile_path, profile) = if let Some(profile_dir) = &config.profile_dir {
            std::fs::create_dir_all(profile_dir).map_err(CdpError::TempDir)?;
            (profile_dir.clone(), None)
        } else {
            let profile = tempfile::Builder::new()
                .prefix("trowser-cdp-")
                .tempdir()
                .map_err(CdpError::TempDir)?;
            (profile.path().to_path_buf(), Some(profile))
        };
        let child = launch_browser(&browser_binary, &profile_path, port, config.headless)?;
        wait_for_devtools(port).await?;
        Ok(Self {
            child,
            _profile: profile,
            port,
            resource_policy: config.resource_policy.clone(),
            pages: BTreeMap::new(),
        })
    }

    /// Creates or replaces a `CDP` page for a trowser tab, navigates it, and extracts content.
    ///
    /// # Errors
    ///
    /// Returns an error if target creation, navigation, or extraction fails.
    pub async fn open_tab(
        &mut self,
        tab_id: TabId,
        url: &str,
        viewport: Option<SemanticViewport>,
    ) -> CdpResult<SemanticPage> {
        if self.pages.contains_key(&tab_id) {
            self.close_tab(&tab_id).await?;
        }
        let mut page = CdpPage::create(self.port, &self.resource_policy).await?;
        page.set_viewport(viewport).await?;
        let semantic_page = page.navigate(url).await?;
        self.pages.insert(tab_id, page);
        Ok(semantic_page)
    }

    /// Navigates an existing tab, creating the page target if necessary.
    ///
    /// # Errors
    ///
    /// Returns an error if target creation, navigation, or extraction fails.
    pub async fn navigate_tab(
        &mut self,
        tab_id: &TabId,
        url: &str,
        viewport: Option<SemanticViewport>,
    ) -> CdpResult<SemanticPage> {
        if !self.pages.contains_key(tab_id) {
            return self.open_tab(tab_id.clone(), url, viewport).await;
        }
        let page = self.page_mut(tab_id)?;
        page.set_viewport(viewport).await?;
        page.navigate(url).await
    }

    /// Reloads a tab and extracts semantic content.
    ///
    /// # Errors
    ///
    /// Returns an error if the page target is missing or reload/extraction fails.
    pub async fn reload_tab(
        &mut self,
        tab_id: &TabId,
        viewport: Option<SemanticViewport>,
    ) -> CdpResult<SemanticPage> {
        let page = self.page_mut(tab_id)?;
        page.set_viewport(viewport).await?;
        page.reload().await
    }

    /// Navigates a tab backward in history.
    ///
    /// # Errors
    ///
    /// Returns an error if no back entry exists or `CDP` navigation/extraction fails.
    pub async fn back_tab(
        &mut self,
        tab_id: &TabId,
        viewport: Option<SemanticViewport>,
    ) -> CdpResult<SemanticPage> {
        let page = self.page_mut(tab_id)?;
        page.set_viewport(viewport).await?;
        page.back().await
    }

    /// Navigates a tab forward in history.
    ///
    /// # Errors
    ///
    /// Returns an error if no forward entry exists or `CDP` navigation/extraction fails.
    pub async fn forward_tab(
        &mut self,
        tab_id: &TabId,
        viewport: Option<SemanticViewport>,
    ) -> CdpResult<SemanticPage> {
        let page = self.page_mut(tab_id)?;
        page.set_viewport(viewport).await?;
        page.forward().await
    }

    /// Activates/clicks a stable DOM action in a tab and extracts semantic content.
    ///
    /// # Errors
    ///
    /// Returns an error if the action does not exist or `CDP` action/extraction fails.
    pub async fn activate_action(
        &mut self,
        tab_id: &TabId,
        action_id: &ActionId,
        viewport: Option<SemanticViewport>,
    ) -> CdpResult<SemanticPage> {
        let page = self.page_mut(tab_id)?;
        page.set_viewport(viewport).await?;
        page.activate_action(action_id).await
    }

    /// Inputs text into a stable DOM action in a tab and extracts semantic content.
    ///
    /// # Errors
    ///
    /// Returns an error if the action does not exist or `CDP` action/extraction fails.
    pub async fn input_text_action(
        &mut self,
        tab_id: &TabId,
        action_id: &ActionId,
        text: &str,
        viewport: Option<SemanticViewport>,
    ) -> CdpResult<SemanticPage> {
        let page = self.page_mut(tab_id)?;
        page.set_viewport(viewport).await?;
        page.input_text_action(action_id, text).await
    }

    /// Closes the browser page target for a tab.
    ///
    /// # Errors
    ///
    /// Returns an error if Chromium rejects the close request.
    pub async fn close_tab(&mut self, tab_id: &TabId) -> CdpResult<()> {
        if let Some(page) = self.pages.remove(tab_id) {
            close_page_target(self.port, &page.target_id).await?;
        }
        Ok(())
    }

    fn page_mut(&mut self, tab_id: &TabId) -> CdpResult<&mut CdpPage> {
        self.pages.get_mut(tab_id).ok_or_else(|| {
            CdpError::PageAction(format!("browser tab {} is not connected", tab_id.as_str()))
        })
    }

    /// Terminates the Chromium child process.
    ///
    /// # Errors
    ///
    /// Returns an error if process termination fails.
    pub async fn shutdown(&mut self) -> CdpResult<()> {
        self.child.kill().await.map_err(CdpError::Launch)
    }
}

impl Drop for CdpBrowser {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

struct CdpPage {
    target_id: String,
    cdp: CdpConnection,
    viewport: Option<SemanticViewport>,
}

impl CdpPage {
    async fn create(port: u16, resource_policy: &CdpResourcePolicy) -> CdpResult<Self> {
        let target = create_page_target(port).await?;
        let mut cdp = CdpConnection::connect(&target.web_socket_debugger_url).await?;
        cdp.call("Page.enable", json!({})).await?;
        cdp.call("Runtime.enable", json!({})).await?;
        cdp.call("Accessibility.enable", json!({})).await?;
        configure_resource_policy(&mut cdp, resource_policy).await?;
        Ok(Self {
            target_id: target.id,
            cdp,
            viewport: None,
        })
    }

    async fn set_viewport(&mut self, viewport: Option<SemanticViewport>) -> CdpResult<()> {
        let Some(viewport) = viewport else {
            return Ok(());
        };
        if self.viewport == Some(viewport) {
            return Ok(());
        }
        self.cdp
            .call(
                "Emulation.setDeviceMetricsOverride",
                json!({
                    "width": viewport.width_px,
                    "height": viewport.height_px,
                    "deviceScaleFactor": 1,
                    "mobile": false,
                    "screenWidth": viewport.width_px,
                    "screenHeight": viewport.height_px,
                }),
            )
            .await?;
        self.viewport = Some(viewport);
        Ok(())
    }

    async fn navigate(&mut self, url: &str) -> CdpResult<SemanticPage> {
        self.cdp
            .call("Page.navigate", json!({ "url": url }))
            .await?;
        self.wait_for_page_load(Duration::from_secs(15)).await;
        self.cdp.extract_semantic_page(self.viewport).await
    }

    async fn reload(&mut self) -> CdpResult<SemanticPage> {
        self.cdp.call("Page.reload", json!({})).await?;
        self.wait_for_page_load(Duration::from_secs(15)).await;
        self.cdp.extract_semantic_page(self.viewport).await
    }

    async fn back(&mut self) -> CdpResult<SemanticPage> {
        let entry_id = self.history_entry(-1).await?;
        self.navigate_to_history_entry(entry_id).await
    }

    async fn forward(&mut self) -> CdpResult<SemanticPage> {
        let entry_id = self.history_entry(1).await?;
        self.navigate_to_history_entry(entry_id).await
    }

    async fn activate_action(&mut self, action_id: &ActionId) -> CdpResult<SemanticPage> {
        match self.run_action(&activate_action_js(action_id)?).await? {
            CdpActionOutcome::Navigate(url) => self.navigate(&url).await,
            CdpActionOutcome::Clicked => {
                self.wait_for_page_load(Duration::from_secs(2)).await;
                self.cdp.extract_semantic_page(self.viewport).await
            }
        }
    }

    async fn input_text_action(
        &mut self,
        action_id: &ActionId,
        text: &str,
    ) -> CdpResult<SemanticPage> {
        let _ = self
            .run_action(&input_text_action_js(action_id, text)?)
            .await?;
        self.cdp.extract_semantic_page(self.viewport).await
    }

    async fn run_action(&mut self, expression: &str) -> CdpResult<CdpActionOutcome> {
        let result = self
            .cdp
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        if let Some(message) = runtime_exception_message(&result) {
            return Err(CdpError::PageAction(message));
        }
        let Some(value) = result.get("result").and_then(|value| value.get("value")) else {
            return Ok(CdpActionOutcome::Clicked);
        };
        if value.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            return Ok(value
                .get("navigate")
                .and_then(Value::as_str)
                .filter(|url| !url.trim().is_empty())
                .map_or(CdpActionOutcome::Clicked, |url| {
                    CdpActionOutcome::Navigate(url.to_string())
                }));
        }
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown page action failure")
            .to_string();
        Err(CdpError::PageAction(message))
    }

    async fn navigate_to_history_entry(&mut self, entry_id: i64) -> CdpResult<SemanticPage> {
        self.cdp
            .call(
                "Page.navigateToHistoryEntry",
                json!({ "entryId": entry_id }),
            )
            .await?;
        self.wait_for_page_load(Duration::from_secs(2)).await;
        self.cdp.extract_semantic_page(self.viewport).await
    }

    async fn history_entry(&mut self, offset: i64) -> CdpResult<i64> {
        let history = self
            .cdp
            .call("Page.getNavigationHistory", json!({}))
            .await?;
        let current_index =
            history
                .get("currentIndex")
                .and_then(Value::as_i64)
                .ok_or(CdpError::MissingField(
                    "Page.getNavigationHistory.currentIndex",
                ))?;
        let target_index = current_index + offset;
        if target_index < 0 {
            return Err(CdpError::NavigationHistory);
        }
        history
            .get("entries")
            .and_then(Value::as_array)
            .and_then(|entries| entries.get(usize::try_from(target_index).ok()?))
            .and_then(|entry| entry.get("id"))
            .and_then(Value::as_i64)
            .ok_or(CdpError::NavigationHistory)
    }

    async fn wait_for_page_load(&mut self, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, self.cdp.wait_for_event("Page.loadEventFired")).await;
    }
}

/// Launches Chromium, navigates to a URL, and extracts a semantic page.
///
/// # Errors
///
/// Returns an error if Chromium cannot be launched, `CDP` cannot be reached,
/// navigation fails, or semantic extraction fails.
pub async fn fetch_semantic_page(config: &CdpBackendConfig, url: &str) -> CdpResult<SemanticPage> {
    let mut browser = CdpBrowser::launch(config).await?;
    let result = browser.open_tab(TabId::new("tab-0"), url, None).await;
    let _ = browser.shutdown().await;
    result
}

fn launch_browser(
    binary: &PathBuf,
    profile_dir: &std::path::Path,
    port: u16,
    headless: bool,
) -> CdpResult<Child> {
    let mut command = Command::new(binary);
    command
        .arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-background-networking")
        .arg("--disable-sync")
        .arg("--disable-extensions")
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if headless {
        command.arg("--headless=new").arg("--disable-gpu");
    }

    command.spawn().map_err(CdpError::Launch)
}

fn allocate_local_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.local_addr().map(|addr| addr.port())
}

#[derive(Debug, Deserialize)]
struct DevToolsTarget {
    id: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: String,
}

async fn wait_for_devtools(port: u16) -> CdpResult<()> {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/json/version");
    for _ in 0..100 {
        if let Ok(response) = client.get(&url).send().await
            && response.status().is_success()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(CdpError::DevToolsTimeout)
}

async fn create_page_target(port: u16) -> CdpResult<DevToolsTarget> {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/json/new?about:blank");
    let response = client.put(&url).send().await?;
    response
        .error_for_status()?
        .json::<DevToolsTarget>()
        .await
        .map_err(CdpError::Http)
}

async fn close_page_target(port: u16, target_id: &str) -> CdpResult<()> {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/json/close/{target_id}");
    let response = client.get(&url).send().await?;
    response.error_for_status()?;
    Ok(())
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn configure_resource_policy(
    cdp: &mut CdpConnection,
    policy: &CdpResourcePolicy,
) -> CdpResult<()> {
    let blocked_urls = blocked_url_patterns(policy);
    if blocked_urls.is_empty() {
        return Ok(());
    }
    cdp.call("Network.enable", json!({})).await?;
    cdp.call("Network.setBlockedURLs", json!({ "urls": blocked_urls }))
        .await?;
    Ok(())
}

fn blocked_url_patterns(policy: &CdpResourcePolicy) -> Vec<&'static str> {
    let mut patterns = Vec::new();
    if policy.block_images {
        patterns.extend([
            "*.png", "*.jpg", "*.jpeg", "*.gif", "*.webp", "*.avif", "*.svg",
        ]);
    }
    if policy.block_media {
        patterns.extend([
            "*.mp4", "*.webm", "*.mov", "*.m4v", "*.mp3", "*.wav", "*.ogg",
        ]);
    }
    if policy.block_fonts {
        patterns.extend(["*.woff", "*.woff2", "*.ttf", "*.otf"]);
    }
    if policy.block_ads {
        patterns.extend([
            "*://*/ads/*",
            "*://*/adservice/*",
            "*doubleclick.net/*",
            "*googlesyndication.com/*",
        ]);
    }
    if policy.block_trackers {
        patterns.extend([
            "*google-analytics.com/*",
            "*googletagmanager.com/*",
            "*facebook.net/*",
            "*hotjar.com/*",
            "*segment.com/*",
        ]);
    }
    patterns
}

enum CdpActionOutcome {
    Clicked,
    Navigate(String),
}

struct CdpConnection {
    socket: WsStream,
    next_id: u64,
}

impl CdpConnection {
    async fn connect(ws_url: &str) -> CdpResult<Self> {
        let (socket, _) = tokio_tungstenite::connect_async(ws_url).await?;
        Ok(Self { socket, next_id: 1 })
    }

    async fn call(&mut self, method: &str, params: Value) -> CdpResult<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        self.socket
            .send(Message::Text(serde_json::to_string(&request)?.into()))
            .await?;

        while let Some(message) = self.socket.next().await {
            let message = message?;
            let Message::Text(text) = message else {
                continue;
            };
            let response: CdpMessage = serde_json::from_str(&text)?;
            if response.id == Some(id) {
                if let Some(error) = response.error {
                    return Err(CdpError::Method {
                        method: method.to_string(),
                        message: error.to_string(),
                    });
                }
                return Ok(response.result.unwrap_or(Value::Null));
            }
        }

        Err(CdpError::MissingField("response"))
    }

    async fn wait_for_event(&mut self, method: &str) -> CdpResult<()> {
        while let Some(message) = self.socket.next().await {
            let message = message?;
            let Message::Text(text) = message else {
                continue;
            };
            let response: CdpMessage = serde_json::from_str(&text)?;
            if response.method.as_deref() == Some(method) {
                return Ok(());
            }
        }
        Err(CdpError::MissingField("event"))
    }

    async fn extract_semantic_page(
        &mut self,
        viewport: Option<SemanticViewport>,
    ) -> CdpResult<SemanticPage> {
        let dom_page = self.extract_dom_page().await?;
        let ax_page = self
            .extract_accessibility_page(&dom_page)
            .await
            .ok()
            .flatten();
        let mut page = merge_accessibility_page(dom_page, ax_page);
        page.viewport = viewport;
        if page.blocks.is_empty() {
            page.blocks.push(SemanticBlock::Paragraph {
                text: "No semantic text content was found on this page.".to_string(),
                layout: None,
                region: SemanticRegion::Main,
            });
        }
        Ok(page)
    }

    async fn extract_dom_page(&mut self) -> CdpResult<SemanticPage> {
        let mut last_error = None;
        for delay in [0_u64, 100, 250, 500] {
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            match self.try_extract_dom_page().await {
                Ok(page) => return Ok(page),
                Err(error) if is_transient_runtime_evaluate_error(&error) => {
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or(CdpError::MissingField("Runtime.evaluate.result.value")))
    }

    async fn try_extract_dom_page(&mut self) -> CdpResult<SemanticPage> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": DOM_EXTRACTION_JS,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        if let Some(message) = runtime_exception_message(&result) {
            return Err(CdpError::Extraction(message));
        }
        let page_json = result
            .get("result")
            .and_then(|value| value.get("value"))
            .and_then(Value::as_str)
            .ok_or(CdpError::MissingField("Runtime.evaluate.result.value"))?;
        let page: CdpSemanticPage = serde_json::from_str(page_json)?;
        Ok(page.into_semantic_page())
    }

    async fn extract_accessibility_page(
        &mut self,
        dom_page: &SemanticPage,
    ) -> CdpResult<Option<SemanticPage>> {
        let result = self.call("Accessibility.getFullAXTree", json!({})).await?;
        let nodes = result
            .get("nodes")
            .cloned()
            .ok_or(CdpError::MissingField("Accessibility.getFullAXTree.nodes"))?;
        let nodes = serde_json::from_value::<Vec<AxNode>>(nodes)?;
        let mut blocks = ax_blocks(&nodes);
        if blocks.len() < 3 {
            return Ok(None);
        }
        merge_dom_action_ids(&mut blocks, &dom_page.blocks);
        Ok(Some(SemanticPage {
            url: dom_page.url.clone(),
            title: dom_page.title.clone(),
            blocks,
            layout_tree: None,
            viewport: None,
        }))
    }
}

#[derive(Debug, Deserialize)]
struct AxNode {
    #[serde(rename = "nodeId")]
    node_id: String,
    #[serde(default)]
    ignored: bool,
    role: Option<AxValue>,
    name: Option<AxValue>,
    value: Option<AxValue>,
    #[serde(default)]
    properties: Vec<AxProperty>,
    #[serde(default, rename = "childIds")]
    child_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AxProperty {
    name: String,
    value: Option<AxValue>,
}

#[derive(Debug, Deserialize)]
struct AxValue {
    value: Option<Value>,
}

impl AxValue {
    fn string(&self) -> Option<String> {
        self.value.as_ref().and_then(|value| match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            Value::Bool(value) => Some(value.to_string()),
            _ => None,
        })
    }
}

#[derive(Debug, Deserialize)]
struct CdpMessage {
    id: Option<u64>,
    method: Option<String>,
    result: Option<Value>,
    error: Option<Value>,
}

fn runtime_exception_message(result: &Value) -> Option<String> {
    let details = result.get("exceptionDetails")?;
    details
        .get("exception")
        .and_then(|exception| exception.get("description"))
        .and_then(Value::as_str)
        .or_else(|| details.get("text").and_then(Value::as_str))
        .or_else(|| {
            details
                .get("exception")
                .and_then(|exception| exception.get("value"))
                .and_then(Value::as_str)
        })
        .map(ToString::to_string)
}

fn is_transient_runtime_evaluate_error(error: &CdpError) -> bool {
    match error {
        CdpError::MissingField("Runtime.evaluate.result.value") => true,
        CdpError::Extraction(message) | CdpError::Method { message, .. } => {
            is_transient_runtime_message(message)
        }
        _ => false,
    }
}

fn is_transient_runtime_message(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("context")
        || message.contains("destroy")
        || message.contains("navigat")
        || message.contains("execution")
}

fn merge_accessibility_page(
    mut dom_page: SemanticPage,
    ax_page: Option<SemanticPage>,
) -> SemanticPage {
    let Some(ax_page) = ax_page else {
        return dom_page;
    };
    if should_use_ax_fallback(&dom_page, &ax_page) {
        return ax_page;
    }
    enrich_dom_blocks_with_ax(&mut dom_page.blocks, &ax_page.blocks);
    dom_page
}

fn should_use_ax_fallback(dom_page: &SemanticPage, ax_page: &SemanticPage) -> bool {
    if dom_page.blocks.is_empty() {
        return true;
    }
    let dom_richness = dom_page
        .blocks
        .iter()
        .filter(|block| is_structured_dom_block(block))
        .count();
    dom_page.blocks.len() < 3 && dom_richness == 0 && ax_page.blocks.len() > dom_page.blocks.len()
}

const fn is_structured_dom_block(block: &SemanticBlock) -> bool {
    matches!(
        block,
        SemanticBlock::RichParagraph { .. }
            | SemanticBlock::RichListItem { .. }
            | SemanticBlock::Quote { .. }
            | SemanticBlock::CodeBlock { .. }
            | SemanticBlock::Table { .. }
    )
}

fn enrich_dom_blocks_with_ax(dom_blocks: &mut [SemanticBlock], ax_blocks: &[SemanticBlock]) {
    for block in dom_blocks {
        match block {
            SemanticBlock::FormControl {
                action_id,
                label,
                control_type,
                value,
                ..
            } => enrich_dom_control(action_id.as_ref(), label, control_type, value, ax_blocks),
            SemanticBlock::Link {
                action_id,
                text,
                url,
                ..
            } => enrich_dom_link(action_id.as_ref(), text, url, ax_blocks),
            SemanticBlock::RichParagraph { runs, .. }
            | SemanticBlock::RichListItem { runs, .. }
            | SemanticBlock::Quote { runs, .. } => {
                for run in runs {
                    if run.is_link() {
                        enrich_dom_run(run, ax_blocks);
                    }
                }
            }
            _ => {}
        }
    }
}

fn enrich_dom_control(
    action_id: Option<&ActionId>,
    label: &mut String,
    control_type: &str,
    value: &mut String,
    ax_blocks: &[SemanticBlock],
) {
    let Some(ax_block) = matching_ax_control(action_id, label, control_type, ax_blocks) else {
        return;
    };
    let SemanticBlock::FormControl {
        label: ax_label,
        value: ax_value,
        ..
    } = ax_block
    else {
        return;
    };
    if is_better_label(ax_label, label) {
        *label = ax_label.clone();
    }
    if value.is_empty() && !ax_value.is_empty() {
        value.clone_from(ax_value);
    }
}

fn matching_ax_control<'a>(
    action_id: Option<&ActionId>,
    label: &str,
    control_type: &str,
    ax_blocks: &'a [SemanticBlock],
) -> Option<&'a SemanticBlock> {
    ax_blocks.iter().find(|block| {
        let SemanticBlock::FormControl {
            action_id: ax_action_id,
            label: ax_label,
            control_type: ax_control_type,
            ..
        } = block
        else {
            return false;
        };
        action_id.is_some_and(|value| ax_action_id.as_ref() == Some(value))
            || normalized_match(label, ax_label)
            || (!control_type.is_empty() && control_type == ax_control_type)
    })
}

fn enrich_dom_link(
    action_id: Option<&ActionId>,
    text: &mut String,
    url: &str,
    ax_blocks: &[SemanticBlock],
) {
    let Some(ax_text) = matching_ax_link_text(action_id, text, url, ax_blocks) else {
        return;
    };
    if is_better_label(ax_text, text) {
        *text = ax_text.to_string();
    }
}

fn enrich_dom_run(run: &mut TextRun, ax_blocks: &[SemanticBlock]) {
    let Some(ax_text) = matching_ax_link_text(
        run.action_id.as_ref(),
        &run.text,
        run.url.as_deref().unwrap_or_default(),
        ax_blocks,
    ) else {
        return;
    };
    if is_better_label(ax_text, &run.text) {
        run.text = ax_text.to_string();
    }
}

fn matching_ax_link_text<'a>(
    action_id: Option<&ActionId>,
    text: &str,
    url: &str,
    ax_blocks: &'a [SemanticBlock],
) -> Option<&'a str> {
    ax_blocks.iter().find_map(|block| {
        let SemanticBlock::Link {
            action_id: ax_action_id,
            text: ax_text,
            url: ax_url,
            ..
        } = block
        else {
            return None;
        };
        (action_id.is_some_and(|value| ax_action_id.as_ref() == Some(value))
            || (!url.is_empty() && url == ax_url)
            || normalized_match(text, ax_text))
        .then_some(ax_text.as_str())
    })
}

fn is_better_label(candidate: &str, current: &str) -> bool {
    let candidate = candidate.trim();
    let current = current.trim();
    !candidate.is_empty()
        && (current.is_empty()
            || candidate.contains('(') && !current.contains('(')
            || candidate.chars().count() > current.chars().count() + 8)
}

fn ax_blocks(nodes: &[AxNode]) -> Vec<SemanticBlock> {
    let node_by_id = nodes
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    let child_ids = nodes
        .iter()
        .flat_map(|node| node.child_ids.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    let roots = nodes
        .iter()
        .filter(|node| !child_ids.contains(node.node_id.as_str()))
        .collect::<Vec<_>>();
    let mut blocks = Vec::new();
    let mut seen = BTreeSet::new();
    for root in roots {
        push_ax_node_blocks(
            root,
            SemanticRegion::Main,
            &node_by_id,
            &mut seen,
            &mut blocks,
        );
    }
    blocks
}

fn push_ax_node_blocks(
    node: &AxNode,
    inherited_region: SemanticRegion,
    node_by_id: &BTreeMap<&str, &AxNode>,
    seen: &mut BTreeSet<String>,
    blocks: &mut Vec<SemanticBlock>,
) {
    if !seen.insert(node.node_id.clone()) || node.ignored {
        return;
    }
    let role = node_role(node);
    let region = ax_region(&role).unwrap_or(inherited_region);
    if let Some(block) = ax_node_block(node, &role, region) {
        blocks.push(block);
    }
    for child_id in &node.child_ids {
        if let Some(child) = node_by_id.get(child_id.as_str()) {
            push_ax_node_blocks(child, region, node_by_id, seen, blocks);
        }
    }
}

fn node_role(node: &AxNode) -> String {
    node.role
        .as_ref()
        .and_then(AxValue::string)
        .unwrap_or_default()
        .to_lowercase()
}

fn ax_region(role: &str) -> Option<SemanticRegion> {
    match role {
        "navigation" => Some(SemanticRegion::Navigation),
        "banner" => Some(SemanticRegion::Header),
        "contentinfo" => Some(SemanticRegion::Footer),
        "complementary" => Some(SemanticRegion::Aside),
        "main" | "article" => Some(SemanticRegion::Main),
        _ => None,
    }
}

fn ax_node_block(node: &AxNode, role: &str, region: SemanticRegion) -> Option<SemanticBlock> {
    let name = ax_name(node);
    if name.is_empty() {
        return None;
    }
    match role {
        "heading" => Some(SemanticBlock::Heading {
            level: ax_property_u8(node, "level").unwrap_or(2),
            text: name,
            layout: None,
            region,
        }),
        "link" => Some(SemanticBlock::Link {
            action_id: None,
            url: ax_property_string(node, "url").unwrap_or_default(),
            text: name,
            layout: None,
            region,
        }),
        "image" | "img" => Some(SemanticBlock::Image {
            alt: name,
            url: ax_property_string(node, "url").unwrap_or_default(),
            width: None,
            height: None,
            layout: None,
            region,
        }),
        "button" | "menuitem" | "checkbox" | "radio" | "switch" | "tab" | "textbox"
        | "searchbox" | "combobox" | "listbox" | "slider" | "spinbutton" => {
            Some(SemanticBlock::FormControl {
                action_id: None,
                label: append_ax_state(name, node),
                control_type: role.to_string(),
                value: node
                    .value
                    .as_ref()
                    .and_then(AxValue::string)
                    .unwrap_or_default(),
                layout: None,
                region,
            })
        }
        "paragraph" | "listitem" | "text" | "statictext" => Some(SemanticBlock::Paragraph {
            text: name,
            layout: None,
            region,
        }),
        _ => None,
    }
}

fn ax_name(node: &AxNode) -> String {
    node.name
        .as_ref()
        .and_then(AxValue::string)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn append_ax_state(mut label: String, node: &AxNode) -> String {
    let states = [
        ("disabled", "disabled"),
        ("expanded", "expanded"),
        ("checked", "checked"),
        ("selected", "selected"),
        ("focused", "focused"),
        ("required", "required"),
    ]
    .into_iter()
    .filter_map(|(property, label)| {
        ax_property_bool(node, property).and_then(|value| value.then_some(label))
    })
    .collect::<Vec<_>>();
    if !states.is_empty() {
        label.push_str(" (");
        label.push_str(&states.join(", "));
        label.push(')');
    }
    label
}

fn ax_property_string(node: &AxNode, name: &str) -> Option<String> {
    node.properties
        .iter()
        .find(|property| property.name == name)
        .and_then(|property| property.value.as_ref())
        .and_then(AxValue::string)
}

fn ax_property_bool(node: &AxNode, name: &str) -> Option<bool> {
    node.properties
        .iter()
        .find(|property| property.name == name)
        .and_then(|property| property.value.as_ref())
        .and_then(|value| value.value.as_ref())
        .and_then(Value::as_bool)
}

fn ax_property_u8(node: &AxNode, name: &str) -> Option<u8> {
    ax_property_string(node, name).and_then(|value| value.parse().ok())
}

fn merge_dom_action_ids(blocks: &mut [SemanticBlock], dom_blocks: &[SemanticBlock]) {
    let mut used_links = BTreeSet::new();
    let mut used_controls = BTreeSet::new();
    for block in blocks {
        match block {
            SemanticBlock::Link {
                action_id,
                text,
                url,
                ..
            } if action_id.is_none() => {
                *action_id = matching_dom_link_action(text, url, dom_blocks, &mut used_links);
            }
            SemanticBlock::RichParagraph { runs, .. }
            | SemanticBlock::RichListItem { runs, .. }
            | SemanticBlock::Quote { runs, .. } => {
                for run in runs {
                    if run.action_id.is_none()
                        && let Some(url) = &run.url
                    {
                        run.action_id =
                            matching_dom_link_action(&run.text, url, dom_blocks, &mut used_links);
                    }
                }
            }
            SemanticBlock::FormControl {
                action_id,
                label,
                control_type,
                ..
            } if action_id.is_none() => {
                *action_id = matching_dom_control_action(
                    label,
                    control_type,
                    dom_blocks,
                    &mut used_controls,
                );
            }
            _ => {}
        }
    }
}

fn matching_dom_link_action(
    text: &str,
    url: &str,
    dom_blocks: &[SemanticBlock],
    used: &mut BTreeSet<usize>,
) -> Option<ActionId> {
    dom_blocks.iter().enumerate().find_map(|(index, block)| {
        if used.contains(&index) {
            return None;
        }
        let candidates = match block {
            SemanticBlock::Link {
                action_id,
                text: dom_text,
                url: dom_url,
                ..
            } => vec![(action_id.clone(), dom_text.as_str(), dom_url.as_str())],
            SemanticBlock::RichParagraph { runs, .. }
            | SemanticBlock::RichListItem { runs, .. }
            | SemanticBlock::Quote { runs, .. } => runs
                .iter()
                .filter_map(|run| {
                    Some((
                        run.action_id.clone(),
                        run.text.as_str(),
                        run.url.as_deref()?,
                    ))
                })
                .collect(),
            _ => Vec::new(),
        };
        candidates
            .into_iter()
            .find_map(|(action_id, dom_text, dom_url)| {
                let matches =
                    (!url.is_empty() && url == dom_url) || normalized_match(text, dom_text);
                matches.then(|| {
                    used.insert(index);
                    action_id
                })?
            })
    })
}

fn matching_dom_control_action(
    label: &str,
    control_type: &str,
    dom_blocks: &[SemanticBlock],
    used: &mut BTreeSet<usize>,
) -> Option<ActionId> {
    dom_blocks.iter().enumerate().find_map(|(index, block)| {
        if used.contains(&index) {
            return None;
        }
        let SemanticBlock::FormControl {
            action_id,
            label: dom_label,
            control_type: dom_control_type,
            ..
        } = block
        else {
            return None;
        };
        let matches = normalized_match(label, dom_label)
            || (!control_type.is_empty() && control_type == dom_control_type);
        matches.then(|| {
            used.insert(index);
            action_id.clone()
        })?
    })
}

fn normalized_match(left: &str, right: &str) -> bool {
    let left = left.trim().to_lowercase();
    let right = right.trim().to_lowercase();
    !left.is_empty() && (left == right || left.contains(&right) || right.contains(&left))
}

#[derive(Debug, Deserialize)]
struct CdpSemanticPage {
    url: String,
    title: String,
    blocks: Vec<CdpSemanticBlock>,
    layout_tree: Option<SemanticLayoutNode>,
}

impl CdpSemanticPage {
    fn into_semantic_page(self) -> SemanticPage {
        SemanticPage {
            url: self.url,
            title: self.title,
            blocks: self
                .blocks
                .into_iter()
                .map(CdpSemanticBlock::into_semantic_block)
                .collect(),
            layout_tree: self.layout_tree,
            viewport: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CdpSemanticBlock {
    Heading {
        level: u8,
        text: String,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    Paragraph {
        text: String,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    RichParagraph {
        runs: Vec<TextRun>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    ListItem {
        text: String,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    RichListItem {
        runs: Vec<TextRun>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    Quote {
        runs: Vec<TextRun>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    CodeBlock {
        text: String,
        language: Option<String>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    HorizontalRule {
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    Link {
        action_id: Option<ActionId>,
        text: String,
        url: String,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    Image {
        alt: String,
        url: String,
        width: Option<u32>,
        height: Option<u32>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    FormControl {
        action_id: Option<ActionId>,
        label: String,
        control_type: String,
        value: String,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
}

impl CdpSemanticBlock {
    #[allow(
        clippy::too_many_lines,
        reason = "exhaustive wire DTO to backend-neutral semantic model mapping"
    )]
    fn into_semantic_block(self) -> SemanticBlock {
        match self {
            Self::Heading {
                level,
                text,
                layout,
                region,
            } => SemanticBlock::Heading {
                level,
                text,
                layout,
                region,
            },
            Self::Paragraph {
                text,
                layout,
                region,
            } => SemanticBlock::Paragraph {
                text,
                layout,
                region,
            },
            Self::RichParagraph {
                runs,
                layout,
                region,
            } => SemanticBlock::RichParagraph {
                runs,
                layout,
                region,
            },
            Self::ListItem {
                text,
                layout,
                region,
            } => SemanticBlock::ListItem {
                text,
                layout,
                region,
            },
            Self::RichListItem {
                runs,
                layout,
                region,
            } => SemanticBlock::RichListItem {
                runs,
                layout,
                region,
            },
            Self::Quote {
                runs,
                layout,
                region,
            } => SemanticBlock::Quote {
                runs,
                layout,
                region,
            },
            Self::CodeBlock {
                text,
                language,
                layout,
                region,
            } => SemanticBlock::CodeBlock {
                text,
                language,
                layout,
                region,
            },
            Self::Table {
                headers,
                rows,
                layout,
                region,
            } => SemanticBlock::Table {
                headers,
                rows,
                layout,
                region,
            },
            Self::HorizontalRule { layout, region } => {
                SemanticBlock::HorizontalRule { layout, region }
            }
            Self::Link {
                action_id,
                text,
                url,
                layout,
                region,
            } => SemanticBlock::Link {
                action_id,
                text,
                url,
                layout,
                region,
            },
            Self::Image {
                alt,
                url,
                width,
                height,
                layout,
                region,
            } => SemanticBlock::Image {
                alt,
                url,
                width,
                height,
                layout,
                region,
            },
            Self::FormControl {
                action_id,
                label,
                control_type,
                value,
                layout,
                region,
            } => SemanticBlock::FormControl {
                action_id,
                label,
                control_type,
                value,
                layout,
                region,
            },
        }
    }
}

fn activate_action_js(action_id: &ActionId) -> CdpResult<String> {
    let action_id = serde_json::to_string(action_id.as_str())?;
    Ok(format!(
        r"
(() => {{
  const el = window.__trowserActions && window.__trowserActions[{action_id}];
  if (!el) return {{ ok: false, error: 'unknown action id' }};
  const anchor = el.closest && el.closest('a[href]');
  if (anchor && anchor.href) return {{ ok: true, navigate: anchor.href }};
  el.focus();
  el.click();
  return {{ ok: true }};
}})()
"
    ))
}

fn input_text_action_js(action_id: &ActionId, text: &str) -> CdpResult<String> {
    let action_id = serde_json::to_string(action_id.as_str())?;
    let text = serde_json::to_string(text)?;
    Ok(format!(
        r"
(() => {{
  const el = window.__trowserActions && window.__trowserActions[{action_id}];
  if (!el) return {{ ok: false, error: 'unknown action id' }};
  const tag = el.tagName.toLowerCase();
  if (tag === 'select') {{
    const value = {text};
    const option = Array.from(el.options).find((item) => item.value === value || item.text === value);
    if (!option) return {{ ok: false, error: 'no matching select option' }};
    el.value = option.value;
  }} else if ('value' in el) {{
    el.value = {text};
  }} else {{
    return {{ ok: false, error: 'control does not accept text' }};
  }}
  el.focus();
  el.dispatchEvent(new Event('input', {{ bubbles: true }}));
  el.dispatchEvent(new Event('change', {{ bubbles: true }}));
  return {{ ok: true }};
}})()
"
    ))
}

const DOM_EXTRACTION_JS: &str = r#"
(() => {
  const blocks = [];
  const clean = (value) => (value || '').replace(/\s+/g, ' ').trim();
  const visible = (el) => {
    const style = window.getComputedStyle(el);
    if (!style || style.display === 'none' || style.visibility === 'hidden') return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  };
  const layout = (el) => {
    const rect = el.getBoundingClientRect();
    return {
      x: rect.x + window.scrollX,
      y: rect.y + window.scrollY,
      width: rect.width,
      height: rect.height,
    };
  };
  const region = (el) => {
    const container = el.closest('main, article, nav, aside, header, footer, [role="main"], [role="article"], [role="navigation"], [role="complementary"], [role="banner"], [role="contentinfo"]');
    if (!container) return 'Unknown';
    const tag = container.tagName.toLowerCase();
    const role = (container.getAttribute('role') || '').toLowerCase();
    if (tag === 'main' || tag === 'article' || role === 'main' || role === 'article') return 'Main';
    if (tag === 'nav' || role === 'navigation') return 'Navigation';
    if (tag === 'aside' || role === 'complementary') return 'Aside';
    if (tag === 'header' || role === 'banner') return 'Header';
    if (tag === 'footer' || role === 'contentinfo') return 'Footer';
    return 'Main';
  };
  window.__trowserActions = window.__trowserActions || {};
  window.__trowserActionCounter = window.__trowserActionCounter || 1;
  const actionId = (el) => {
    let id = el.getAttribute('data-trowser-action-id');
    if (!id) {
      id = `a-${window.__trowserActionCounter++}`;
      el.setAttribute('data-trowser-action-id', id);
    }
    window.__trowserActions[id] = el;
    return id;
  };
  const controlSelector = 'input,textarea,button,select,[role="button"],[role="menuitem"],[aria-haspopup],[aria-expanded],[onclick]';
  const isControl = (el) => el.matches(controlSelector) && !el.matches('a[href]');
  const controlType = (el) => clean(el.getAttribute('role') || el.type || el.tagName.toLowerCase());
  const controlLabel = (el) => clean(el.labels && el.labels[0] ? el.labels[0].innerText : (el.getAttribute('aria-label') || el.getAttribute('title') || el.name || el.placeholder || el.innerText || el.textContent));
  window.trowserVisibleControls = () => Array.from(document.body.querySelectorAll(controlSelector)).filter((el) => visible(el) && isControl(el));

  const pushText = (kind, el, extra = {}) => {
    const text = clean(el.innerText || el.textContent || el.getAttribute('aria-label'));
    if (text) blocks.push({ kind, text, layout: layout(el), region: region(el), ...extra });
  };
  const mergeRuns = (runs) => {
    const merged = [];
    for (const run of runs) {
      const text = clean(run.text);
      if (!text) continue;
      const last = merged[merged.length - 1];
      if (last && last.style === run.style && last.url === run.url && last.action_id === run.action_id) {
        last.text = `${last.text} ${text}`;
      } else {
        merged.push({ text, style: run.style || 'Normal', url: run.url || null, action_id: run.action_id || null });
      }
    }
    return merged;
  };
  const inlineRuns = (root, inheritedStyle = 'Normal') => {
    const runs = [];
    const walk = (node, style) => {
      if (node.nodeType === Node.TEXT_NODE) {
        runs.push({ text: node.textContent || '', style, url: null, action_id: null });
        return;
      }
      if (node.nodeType !== Node.ELEMENT_NODE) return;
      const el = node;
      if (!visible(el)) return;
      const tag = el.tagName.toLowerCase();
      if (tag === 'br') {
        runs.push({ text: ' ', style, url: null, action_id: null });
        return;
      }
      if (tag === 'a') {
        const text = clean(el.innerText || el.textContent || el.href);
        if (text || el.href) runs.push({ text: text || el.href, style: 'Normal', url: el.href || '', action_id: actionId(el) });
        return;
      }
      if (isControl(el) || tag === 'img' || tag === 'svg') return;
      const childStyle = tag === 'strong' || tag === 'b'
        ? 'Strong'
        : tag === 'em' || tag === 'i'
          ? 'Emphasis'
          : tag === 'code'
            ? 'Code'
            : style;
      for (const child of el.childNodes) walk(child, childStyle);
    };
    for (const child of root.childNodes) walk(child, inheritedStyle);
    return mergeRuns(runs);
  };
  const pushRich = (kind, el, prefix = {}) => {
    const runs = inlineRuns(el);
    if (runs.length) blocks.push({ kind, runs, layout: layout(el), region: region(el), ...prefix });
  };
  const layoutNode = (el, depth = 0) => {
    if (!el || !visible(el) || depth > 8) return null;
    const tag = el.tagName.toLowerCase();
    const style = window.getComputedStyle(el);
    const isLayoutContainer = ['flex','inline-flex','grid','inline-grid','table','table-row','table-cell'].includes(style.display);
    const children = Array.from(el.children)
      .filter((child) => visible(child))
      .filter((child) => {
        const childTag = child.tagName.toLowerCase();
        const childStyle = window.getComputedStyle(child);
        return depth < 2
          || isLayoutContainer
          || ['div','header','nav','main','article','section','aside','footer','form','ul','ol','li','p','blockquote','pre','table','a','button','input','textarea','select'].includes(childTag)
          || ['flex','inline-flex','grid','inline-grid','table','table-row','table-cell'].includes(childStyle.display)
          || child.getAttribute('role');
      })
      .slice(0, 80)
      .map((child) => layoutNode(child, depth + 1))
      .filter(Boolean);
    const actionable = tag === 'a' || isControl(el);
    const runs = ['p','li','blockquote'].includes(tag) ? inlineRuns(el) : [];
    const text = runs.length ? '' : clean(el.getAttribute('aria-label') || el.getAttribute('title') || (actionable ? el.innerText || el.textContent : ''));
    return {
      tag,
      role: clean(el.getAttribute('role') || ''),
      region: region(el),
      layout: layout(el),
      display: style ? style.display || '' : '',
      text,
      runs,
      action_id: actionable ? actionId(el) : null,
      url: tag === 'a' ? el.href || '' : '',
      control_type: isControl(el) ? controlType(el) : '',
      value: isControl(el) ? clean(el.value || '') : '',
      children,
    };
  };

  const tableData = (table) => {
    const headerCells = Array.from(table.querySelectorAll('thead tr:first-child th'));
    const firstRowHeaders = headerCells.length ? headerCells : Array.from(table.querySelectorAll('tr:first-child th'));
    const headers = firstRowHeaders.map((cell) => clean(cell.innerText || cell.textContent)).filter(Boolean);
    const rows = Array.from(table.querySelectorAll('tr')).map((row) => {
      const cells = Array.from(row.querySelectorAll('td,th')).map((cell) => clean(cell.innerText || cell.textContent));
      return cells;
    }).filter((row, index) => row.some(Boolean) && !(headers.length && index === 0 && row.every((cell, cellIndex) => cell === headers[cellIndex])));
    return { headers, rows };
  };

  const selector = `h1,h2,h3,h4,h5,h6,p,li,blockquote,pre,table,hr,a,img,${controlSelector}`;
  for (const el of document.body.querySelectorAll(selector)) {
    if (!visible(el)) continue;
    const tag = el.tagName.toLowerCase();
    if (tag === 'a' && el.closest('p,li,blockquote,table')) continue;
    if (!isControl(el) && tag !== 'a') {
      const parentBlock = el.parentElement && el.parentElement.closest('p,li,blockquote,pre,table');
      if (parentBlock) continue;
    }
    if (/^h[1-6]$/.test(tag)) {
      pushText('heading', el, { level: Number(tag.slice(1)) });
    } else if (tag === 'p') {
      pushRich('rich_paragraph', el);
    } else if (tag === 'li') {
      pushRich('rich_list_item', el);
    } else if (tag === 'blockquote') {
      pushRich('quote', el);
    } else if (tag === 'pre') {
      const code = el.querySelector('code');
      const language = code ? Array.from(code.classList).find((name) => name.startsWith('language-')) : null;
      const text = (el.innerText || el.textContent || '').replace(/\n{4,}/g, '\n\n\n').trimEnd();
      if (text) blocks.push({ kind: 'code_block', text, language: language ? language.replace(/^language-/, '') : null, layout: layout(el), region: region(el) });
    } else if (tag === 'table') {
      const data = tableData(el);
      if (data.headers.length || data.rows.length) blocks.push({ kind: 'table', headers: data.headers, rows: data.rows, layout: layout(el), region: region(el) });
    } else if (tag === 'hr') {
      blocks.push({ kind: 'horizontal_rule', layout: layout(el), region: region(el) });
    } else if (tag === 'a') {
      const text = clean(el.innerText || el.textContent || el.href);
      if (text || el.href) blocks.push({ kind: 'link', action_id: actionId(el), text, url: el.href || '', layout: layout(el), region: region(el) });
    } else if (tag === 'img') {
      blocks.push({
        kind: 'image',
        alt: clean(el.alt || el.getAttribute('aria-label')),
        url: el.currentSrc || el.src || '',
        width: el.naturalWidth || null,
        height: el.naturalHeight || null,
        layout: layout(el),
        region: region(el),
      });
    } else if (isControl(el)) {
      blocks.push({
        kind: 'form_control',
        action_id: actionId(el),
        label: controlLabel(el),
        control_type: controlType(el),
        value: clean(el.value || ''),
        layout: layout(el),
        region: region(el),
      });
    }
  }

  return JSON.stringify({
    url: location.href,
    title: document.title,
    blocks,
    layout_tree: document.body ? layoutNode(document.body) : null,
  });
})()
"#;

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
                title: "CDP backend requires async daemon extraction".to_string(),
                blocks: Vec::new(),
                layout_tree: None,
                viewport: None,
            },
        })
    }
}
