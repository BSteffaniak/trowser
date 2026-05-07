#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Chromium `CDP` browser backend for trowser.
//!
//! This crate is intentionally the only place where Chromium/CDP-specific
//! implementation details should live.

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
use trowser_render::{SemanticBlock, SemanticPage};

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

/// Launches Chromium, navigates to a URL, and extracts a semantic page.
///
/// # Errors
///
/// Returns an error if Chromium cannot be launched, `CDP` cannot be reached,
/// navigation fails, or semantic extraction fails.
pub async fn fetch_semantic_page(config: &CdpBackendConfig, url: &str) -> CdpResult<SemanticPage> {
    let discovery = discover_browser_binary(config.browser_binary.as_ref());
    let browser_binary = discovery.binary.ok_or(CdpError::BrowserNotFound)?;
    let port = allocate_local_port().map_err(CdpError::Port)?;
    let profile = tempfile::Builder::new()
        .prefix("trowser-cdp-")
        .tempdir()
        .map_err(CdpError::TempDir)?;
    let mut browser = launch_browser(&browser_binary, profile.path(), port, config.headless)?;

    let result = async {
        let ws_url = wait_for_page_ws_url(port).await?;
        let mut cdp = CdpConnection::connect(&ws_url).await?;
        cdp.call("Page.enable", json!({})).await?;
        cdp.call("Runtime.enable", json!({})).await?;
        cdp.call("Page.navigate", json!({ "url": url })).await?;
        let _ = tokio::time::timeout(
            Duration::from_secs(15),
            cdp.wait_for_event("Page.loadEventFired"),
        )
        .await;
        cdp.extract_semantic_page().await
    }
    .await;

    let _ = browser.kill().await;
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
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: String,
}

async fn wait_for_page_ws_url(port: u16) -> CdpResult<String> {
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/json/new?about:blank");
    for _ in 0..100 {
        if let Ok(response) = client.put(&url).send().await
            && response.status().is_success()
        {
            let target = response.json::<DevToolsTarget>().await?;
            return Ok(target.web_socket_debugger_url);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(CdpError::DevToolsTimeout)
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
    },
    Paragraph {
        text: String,
    },
    Link {
        text: String,
        url: String,
    },
    Image {
        alt: String,
        url: String,
        width: Option<u32>,
        height: Option<u32>,
    },
    FormControl {
        label: String,
        control_type: String,
        value: String,
    },
}

impl CdpSemanticBlock {
    fn into_semantic_block(self) -> SemanticBlock {
        match self {
            Self::Heading { level, text } => SemanticBlock::Heading { level, text },
            Self::Paragraph { text } => SemanticBlock::Paragraph { text },
            Self::Link { text, url } => SemanticBlock::Link { text, url },
            Self::Image {
                alt,
                url,
                width,
                height,
            } => SemanticBlock::Image {
                alt,
                url,
                width,
                height,
            },
            Self::FormControl {
                label,
                control_type,
                value,
            } => SemanticBlock::FormControl {
                label,
                control_type,
                value,
            },
        }
    }
}

const DOM_EXTRACTION_JS: &str = r"
(() => {
  const blocks = [];
  const clean = (value) => (value || '').replace(/\s+/g, ' ').trim();
  const visible = (el) => {
    const style = window.getComputedStyle(el);
    if (!style || style.display === 'none' || style.visibility === 'hidden') return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  };
  const pushText = (kind, el, extra = {}) => {
    const text = clean(el.innerText || el.textContent || el.getAttribute('aria-label'));
    if (text) blocks.push({ kind, text, ...extra });
  };

  for (const el of document.body.querySelectorAll('h1,h2,h3,h4,h5,h6,p,a,img,input,textarea,button,select,li')) {
    if (!visible(el)) continue;
    const tag = el.tagName.toLowerCase();
    if (/^h[1-6]$/.test(tag)) {
      pushText('heading', el, { level: Number(tag.slice(1)) });
    } else if (tag === 'a') {
      const text = clean(el.innerText || el.textContent || el.href);
      if (text || el.href) blocks.push({ kind: 'link', text, url: el.href || '' });
    } else if (tag === 'img') {
      blocks.push({
        kind: 'image',
        alt: clean(el.alt || el.getAttribute('aria-label')),
        url: el.currentSrc || el.src || '',
        width: el.naturalWidth || null,
        height: el.naturalHeight || null,
      });
    } else if (tag === 'input' || tag === 'textarea' || tag === 'button' || tag === 'select') {
      blocks.push({
        kind: 'form_control',
        label: clean(el.labels && el.labels[0] ? el.labels[0].innerText : (el.getAttribute('aria-label') || el.name || el.placeholder || el.innerText)),
        control_type: clean(el.type || tag),
        value: clean(el.value || ''),
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
";

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
