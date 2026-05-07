#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Chromium `CDP` browser backend for trowser.
//!
//! This crate is intentionally the only place where Chromium/CDP-specific
//! implementation details should live.

use std::collections::BTreeMap;
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
use trowser_render::{BlockLayout, SemanticBlock, SemanticPage, SemanticRegion};
use trowser_session_models::{ActionId, TabId};

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
    _profile: tempfile::TempDir,
    port: u16,
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
        let profile = tempfile::Builder::new()
            .prefix("trowser-cdp-")
            .tempdir()
            .map_err(CdpError::TempDir)?;
        let child = launch_browser(&browser_binary, profile.path(), port, config.headless)?;
        wait_for_devtools(port).await?;
        Ok(Self {
            child,
            _profile: profile,
            port,
            pages: BTreeMap::new(),
        })
    }

    /// Creates or replaces a `CDP` page for a trowser tab, navigates it, and extracts content.
    ///
    /// # Errors
    ///
    /// Returns an error if target creation, navigation, or extraction fails.
    pub async fn open_tab(&mut self, tab_id: TabId, url: &str) -> CdpResult<SemanticPage> {
        if self.pages.contains_key(&tab_id) {
            self.close_tab(&tab_id).await?;
        }
        let mut page = CdpPage::create(self.port).await?;
        let semantic_page = page.navigate(url).await?;
        self.pages.insert(tab_id, page);
        Ok(semantic_page)
    }

    /// Navigates an existing tab, creating the page target if necessary.
    ///
    /// # Errors
    ///
    /// Returns an error if target creation, navigation, or extraction fails.
    pub async fn navigate_tab(&mut self, tab_id: &TabId, url: &str) -> CdpResult<SemanticPage> {
        if !self.pages.contains_key(tab_id) {
            return self.open_tab(tab_id.clone(), url).await;
        }
        self.page_mut(tab_id)?.navigate(url).await
    }

    /// Reloads a tab and extracts semantic content.
    ///
    /// # Errors
    ///
    /// Returns an error if the page target is missing or reload/extraction fails.
    pub async fn reload_tab(&mut self, tab_id: &TabId) -> CdpResult<SemanticPage> {
        self.page_mut(tab_id)?.reload().await
    }

    /// Navigates a tab backward in history.
    ///
    /// # Errors
    ///
    /// Returns an error if no back entry exists or `CDP` navigation/extraction fails.
    pub async fn back_tab(&mut self, tab_id: &TabId) -> CdpResult<SemanticPage> {
        self.page_mut(tab_id)?.back().await
    }

    /// Navigates a tab forward in history.
    ///
    /// # Errors
    ///
    /// Returns an error if no forward entry exists or `CDP` navigation/extraction fails.
    pub async fn forward_tab(&mut self, tab_id: &TabId) -> CdpResult<SemanticPage> {
        self.page_mut(tab_id)?.forward().await
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
    ) -> CdpResult<SemanticPage> {
        self.page_mut(tab_id)?.activate_action(action_id).await
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
    ) -> CdpResult<SemanticPage> {
        self.page_mut(tab_id)?
            .input_text_action(action_id, text)
            .await
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
}

impl CdpPage {
    async fn create(port: u16) -> CdpResult<Self> {
        let target = create_page_target(port).await?;
        let mut cdp = CdpConnection::connect(&target.web_socket_debugger_url).await?;
        cdp.call("Page.enable", json!({})).await?;
        cdp.call("Runtime.enable", json!({})).await?;
        Ok(Self {
            target_id: target.id,
            cdp,
        })
    }

    async fn navigate(&mut self, url: &str) -> CdpResult<SemanticPage> {
        self.cdp
            .call("Page.navigate", json!({ "url": url }))
            .await?;
        self.wait_for_page_load(Duration::from_secs(15)).await;
        self.cdp.extract_semantic_page().await
    }

    async fn reload(&mut self) -> CdpResult<SemanticPage> {
        self.cdp.call("Page.reload", json!({})).await?;
        self.wait_for_page_load(Duration::from_secs(15)).await;
        self.cdp.extract_semantic_page().await
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
        self.run_action(&activate_action_js(action_id)?).await?;
        self.wait_for_page_load(Duration::from_secs(2)).await;
        self.cdp.extract_semantic_page().await
    }

    async fn input_text_action(
        &mut self,
        action_id: &ActionId,
        text: &str,
    ) -> CdpResult<SemanticPage> {
        self.run_action(&input_text_action_js(action_id, text)?)
            .await?;
        self.cdp.extract_semantic_page().await
    }

    async fn run_action(&mut self, expression: &str) -> CdpResult<()> {
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
        let value = result
            .get("result")
            .and_then(|value| value.get("value"))
            .ok_or(CdpError::MissingField("Runtime.evaluate.result.value"))?;
        if value.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            return Ok(());
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
        self.cdp.extract_semantic_page().await
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
    let result = browser.open_tab(TabId::new("tab-0"), url).await;
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

    async fn extract_semantic_page(&mut self) -> CdpResult<SemanticPage> {
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
        let page_json = result
            .get("result")
            .and_then(|value| value.get("value"))
            .and_then(Value::as_str)
            .ok_or(CdpError::MissingField("Runtime.evaluate.result.value"))?;
        let page: CdpSemanticPage = serde_json::from_str(page_json)?;
        let mut page = page.into_semantic_page();
        if page.blocks.is_empty() {
            page.blocks.push(SemanticBlock::Paragraph {
                text: "No semantic text content was found on this page.".to_string(),
                layout: None,
                region: SemanticRegion::Main,
            });
        }
        Ok(page)
    }
}

#[derive(Debug, Deserialize)]
struct CdpMessage {
    id: Option<u64>,
    method: Option<String>,
    result: Option<Value>,
    error: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct CdpSemanticPage {
    url: String,
    title: String,
    blocks: Vec<CdpSemanticBlock>,
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
    ListItem {
        text: String,
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
            Self::ListItem {
                text,
                layout,
                region,
            } => SemanticBlock::ListItem {
                text,
                layout,
                region,
            },
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
    if (!container) return 'Main';
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
  const pushText = (kind, el, extra = {}) => {
    const text = clean(el.innerText || el.textContent || el.getAttribute('aria-label'));
    if (text) blocks.push({ kind, text, layout: layout(el), region: region(el), ...extra });
  };
  const controlSelector = 'input,textarea,button,select,[role="button"],[role="menuitem"],[aria-haspopup],[aria-expanded],[onclick]';
  const isControl = (el) => el.matches(controlSelector) && !el.matches('a[href]');
  const controlType = (el) => clean(el.getAttribute('role') || el.type || el.tagName.toLowerCase());
  const controlLabel = (el) => clean(el.labels && el.labels[0] ? el.labels[0].innerText : (el.getAttribute('aria-label') || el.getAttribute('title') || el.name || el.placeholder || el.innerText || el.textContent));
  window.trowserVisibleControls = () => Array.from(document.body.querySelectorAll(controlSelector)).filter((el) => visible(el) && isControl(el));

  for (const el of document.body.querySelectorAll(`h1,h2,h3,h4,h5,h6,p,a,img,li,${controlSelector}`)) {
    if (!visible(el)) continue;
    const tag = el.tagName.toLowerCase();
    if (/^h[1-6]$/.test(tag)) {
      pushText('heading', el, { level: Number(tag.slice(1)) });
    } else if (tag === 'li') {
      pushText('list_item', el);
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
    } else {
      pushText('paragraph', el);
    }
  }

  return JSON.stringify({
    url: location.href,
    title: document.title,
    blocks,
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
            },
        })
    }
}
