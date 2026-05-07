#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Daemon runtime for trowser.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, Notify};
use trowser_browser_cdp::{CdpBackendConfig, CdpBrowser};
use trowser_config::{ConfigPaths, TROWSER_DAEMON_READY_FILE_ENV, TrowserConfig};
use trowser_ipc::{
    ClientHello, PROTOCOL_VERSION, ProtocolCapabilities, Request, Response, ServerHello,
    ServerStatus,
};
use trowser_render::{SemanticBlock, SemanticPage, SemanticRegion, reader_blocks, reader_links};
use trowser_session_models::{ActionId, SessionSnapshot, TabId, TabSummary};

/// Daemon errors.
#[derive(Debug, Error)]
pub enum ServerError {
    /// I/O operation failed.
    #[error("daemon I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// IPC encoding or decoding failed.
    #[error("daemon codec failed: {0}")]
    Codec(#[from] bmux_codec::Error),
}

/// Result alias for daemon operations.
pub type ServerResult<T> = Result<T, ServerError>;

#[derive(Debug, Clone, Default)]
struct ServerState {
    tabs: BTreeMap<TabId, SemanticPage>,
    active_tab: Option<TabId>,
    next_tab_id: u64,
}

/// Daemon runtime state.
#[derive(Debug, Clone)]
pub struct TrowserServer {
    paths: ConfigPaths,
    config: TrowserConfig,
    state: Arc<Mutex<ServerState>>,
    browser: Arc<Mutex<Option<CdpBrowser>>>,
    shutdown: Arc<Notify>,
}

impl TrowserServer {
    /// Creates a daemon runtime.
    #[must_use]
    pub fn new(paths: ConfigPaths, config: TrowserConfig) -> Self {
        Self {
            paths,
            config,
            state: Arc::new(Mutex::new(ServerState::default())),
            browser: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(Notify::new()),
        }
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

    /// Runs the daemon in the foreground until interrupted.
    ///
    /// # Errors
    ///
    /// Returns an error if the local IPC listener cannot be created or accepts fail.
    pub async fn run_foreground(&self) -> ServerResult<()> {
        #[cfg(unix)]
        {
            let socket_path = self.paths.server_socket_path();
            if let Some(parent) = socket_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            if socket_path.exists() {
                tokio::fs::remove_file(&socket_path).await?;
            }
            let listener = tokio::net::UnixListener::bind(&socket_path)?;
            let pid_path = self.paths.daemon_pid_path();
            write_pid_file(&pid_path).await?;
            write_ready_file_if_requested(&socket_path).await?;
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

            loop {
                tokio::select! {
                    signal = tokio::signal::ctrl_c() => {
                        signal?;
                        break;
                    }
                    _ = terminate.recv() => {
                        break;
                    }
                    () = self.shutdown.notified() => {
                        break;
                    }
                    accepted = listener.accept() => {
                        let (stream, _) = accepted?;
                        let server = self.clone();
                        tokio::spawn(async move {
                            if let Err(error) = server.handle_stream(stream).await {
                                eprintln!("trowser daemon connection error: {error}");
                            }
                        });
                    }
                }
            }

            self.shutdown_browser().await;
            if socket_path.exists() {
                tokio::fs::remove_file(&socket_path).await?;
            }
            remove_pid_file_if_current(&pid_path).await?;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Err(ServerError::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "foreground daemon transport is only implemented for Unix sockets so far",
            )))
        }
    }

    #[cfg(unix)]
    async fn handle_stream(&self, mut stream: tokio::net::UnixStream) -> ServerResult<()> {
        let request: Request = read_message(&mut stream).await?;
        let response = self.handle_request(request).await;
        write_message(&mut stream, &response).await
    }

    async fn handle_request(&self, request: Request) -> Response {
        match request {
            Request::Hello(hello) => Self::handle_hello(&hello),
            Request::Status => Response::Status(self.status()),
            Request::Open { url } => self.handle_open(url).await,
            Request::SwitchTab { tab_id } => self.handle_switch_tab(tab_id).await,
            Request::CloseTab { tab_id } => self.handle_close_tab(tab_id).await,
            Request::NextTab => self.handle_next_tab().await,
            Request::PreviousTab => self.handle_previous_tab().await,
            Request::ActiveSnapshot => self.handle_active_snapshot().await,
            Request::Reload => self.handle_reload().await,
            Request::Back => self.handle_back().await,
            Request::Forward => self.handle_forward().await,
            Request::ActivateAction { action_id } => self.handle_activate_action(action_id).await,
            Request::InputTextAction { action_id, text } => {
                self.handle_input_text_action(action_id, text).await
            }
            Request::ClickControl { control_id } => self.handle_click_control(control_id).await,
            Request::InputText { control_id, text } => {
                self.handle_input_text(control_id, text).await
            }
            Request::ListTabs => Response::Tabs(self.snapshot().await),
            Request::FollowLink { link_id } => self.handle_follow_link(link_id).await,
            Request::Snapshot { tab_id } => self.handle_snapshot(&tab_id).await,
            Request::Shutdown => {
                self.shutdown.notify_waiters();
                Response::Ok
            }
        }
    }

    fn handle_hello(hello: &ClientHello) -> Response {
        if hello.protocol_version != PROTOCOL_VERSION {
            return Response::Error {
                message: format!(
                    "unsupported protocol version {} (server supports {PROTOCOL_VERSION})",
                    hello.protocol_version
                ),
            };
        }

        Response::Hello(ServerHello {
            protocol_version: PROTOCOL_VERSION,
            capabilities: Self::capabilities(),
        })
    }

    async fn handle_open(&self, url: String) -> Response {
        let tab_id = self.allocate_tab_id().await;
        let page = self
            .open_browser_tab(tab_id.clone(), &url)
            .await
            .unwrap_or_else(|error| error_page(&url, &error.to_string()));
        self.store_page(tab_id, page).await
    }

    async fn allocate_tab_id(&self) -> TabId {
        let mut state = self.state.lock().await;
        let tab_id = TabId::new(format!("tab-{}", state.next_tab_id));
        state.next_tab_id += 1;
        drop(state);
        tab_id
    }

    async fn handle_follow_link(&self, link_id: u32) -> Response {
        let Some((tab_id, action_id, url)) = self.follow_link_target(link_id).await else {
            return Response::Error {
                message: format!("unknown link id {link_id}"),
            };
        };

        if let Some(action_id) = action_id {
            return self.handle_activate_action(action_id).await;
        }

        let page = self
            .navigate_browser_tab(&tab_id, &url)
            .await
            .unwrap_or_else(|error| error_page(&url, &error.to_string()));
        self.store_page(tab_id, page).await
    }

    async fn follow_link_target(&self, link_id: u32) -> Option<(TabId, Option<ActionId>, String)> {
        let state = self.state.lock().await;
        let tab_id = state.active_tab.clone()?;
        let page = state.tabs.get(&tab_id)?.clone();
        drop(state);
        let target = reader_links(&page)
            .into_iter()
            .find(|link| link.id == link_id)?;
        Some((tab_id, target.action_id, target.url))
    }

    async fn control_action_id(&self, control_id: u32) -> Option<ActionId> {
        let state = self.state.lock().await;
        let tab_id = state.active_tab.clone()?;
        let page = state.tabs.get(&tab_id)?.clone();
        drop(state);
        reader_blocks(&page.blocks)
            .into_iter()
            .filter_map(|block| {
                let SemanticBlock::FormControl { action_id, .. } = block else {
                    return None;
                };
                action_id.clone()
            })
            .nth(usize::try_from(control_id.saturating_sub(1)).ok()?)
    }

    async fn handle_switch_tab(&self, tab_id: TabId) -> Response {
        let mut state = self.state.lock().await;
        if !state.tabs.contains_key(&tab_id) {
            return Response::Error {
                message: format!("unknown tab {}", tab_id.as_str()),
            };
        }
        state.active_tab = Some(tab_id.clone());
        drop(state);
        Response::Opened { tab_id }
    }

    async fn handle_close_tab(&self, tab_id: TabId) -> Response {
        {
            let mut browser = self.browser.lock().await;
            if let Some(browser) = browser.as_mut() {
                let _ = browser.close_tab(&tab_id).await;
            }
        }

        let mut state = self.state.lock().await;
        if state.tabs.remove(&tab_id).is_none() {
            return Response::Error {
                message: format!("unknown tab {}", tab_id.as_str()),
            };
        }
        if state.active_tab.as_ref() == Some(&tab_id) {
            state.active_tab = state.tabs.keys().next().cloned();
        }
        let active_tab = state.active_tab.clone();
        drop(state);
        active_tab.map_or(Response::Ok, |tab_id| Response::Opened { tab_id })
    }

    async fn handle_next_tab(&self) -> Response {
        self.switch_relative_tab(1).await
    }

    async fn handle_previous_tab(&self) -> Response {
        self.switch_relative_tab(-1).await
    }

    async fn switch_relative_tab(&self, offset: isize) -> Response {
        let mut state = self.state.lock().await;
        let tabs = state.tabs.keys().cloned().collect::<Vec<_>>();
        if tabs.is_empty() {
            return Response::Error {
                message: "no tabs".to_string(),
            };
        }
        let current_index = state
            .active_tab
            .as_ref()
            .and_then(|active| tabs.iter().position(|tab| tab == active))
            .unwrap_or(0);
        let target_index = if offset.is_negative() {
            current_index
                .checked_sub(offset.unsigned_abs())
                .unwrap_or(tabs.len() - 1)
        } else {
            (current_index + usize::try_from(offset).unwrap_or(0)) % tabs.len()
        };
        let tab_id = tabs[target_index].clone();
        state.active_tab = Some(tab_id.clone());
        drop(state);
        Response::Opened { tab_id }
    }

    async fn handle_active_snapshot(&self) -> Response {
        let state = self.state.lock().await;
        let Some(tab_id) = state.active_tab.clone() else {
            return Response::Error {
                message: "no active tab".to_string(),
            };
        };
        let Some(page) = state.tabs.get(&tab_id).cloned() else {
            return Response::Error {
                message: format!("unknown active tab {}", tab_id.as_str()),
            };
        };
        drop(state);
        Response::ActiveSnapshot { tab_id, page }
    }

    async fn handle_reload(&self) -> Response {
        let Some((tab_id, url)) = self.active_tab_url().await else {
            return Response::Error {
                message: "no active tab".to_string(),
            };
        };
        let page = self
            .reload_page(&tab_id, &url)
            .await
            .unwrap_or_else(|error| error_page(&url, &error.to_string()));
        self.store_page(tab_id, page).await
    }

    async fn handle_back(&self) -> Response {
        let Some(tab_id) = self.active_tab_id().await else {
            return Response::Error {
                message: "no active tab".to_string(),
            };
        };
        match self.back_page(&tab_id).await {
            Ok(page) => self.store_page(tab_id, page).await,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        }
    }

    async fn handle_forward(&self) -> Response {
        let Some(tab_id) = self.active_tab_id().await else {
            return Response::Error {
                message: "no active tab".to_string(),
            };
        };
        match self.forward_page(&tab_id).await {
            Ok(page) => self.store_page(tab_id, page).await,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        }
    }

    async fn handle_activate_action(&self, action_id: ActionId) -> Response {
        let Some(tab_id) = self.active_tab_id().await else {
            return Response::Error {
                message: "no active tab".to_string(),
            };
        };
        match self.activate_action(&tab_id, &action_id).await {
            Ok(page) => self.store_page(tab_id, page).await,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        }
    }

    async fn handle_input_text_action(&self, action_id: ActionId, text: String) -> Response {
        let Some(tab_id) = self.active_tab_id().await else {
            return Response::Error {
                message: "no active tab".to_string(),
            };
        };
        match self.input_text_action(&tab_id, &action_id, &text).await {
            Ok(page) => self.store_page(tab_id, page).await,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        }
    }

    async fn handle_click_control(&self, control_id: u32) -> Response {
        let Some(action_id) = self.control_action_id(control_id).await else {
            return Response::Error {
                message: format!("unknown control id {control_id}"),
            };
        };
        self.handle_activate_action(action_id).await
    }

    async fn handle_input_text(&self, control_id: u32, text: String) -> Response {
        let Some(action_id) = self.control_action_id(control_id).await else {
            return Response::Error {
                message: format!("unknown control id {control_id}"),
            };
        };
        self.handle_input_text_action(action_id, text).await
    }

    async fn active_tab_id(&self) -> Option<TabId> {
        let state = self.state.lock().await;
        let tab_id = state.active_tab.clone();
        drop(state);
        tab_id
    }

    async fn active_tab_url(&self) -> Option<(TabId, String)> {
        let state = self.state.lock().await;
        let tab_id = state.active_tab.clone()?;
        let url = state.tabs.get(&tab_id)?.url.clone();
        drop(state);
        Some((tab_id, url))
    }

    async fn store_page(&self, tab_id: TabId, page: SemanticPage) -> Response {
        let mut state = self.state.lock().await;
        state.tabs.insert(tab_id.clone(), page);
        state.active_tab = Some(tab_id.clone());
        drop(state);
        Response::Opened { tab_id }
    }

    async fn handle_snapshot(&self, tab_id: &TabId) -> Response {
        let state = self.state.lock().await;
        state.tabs.get(tab_id).cloned().map_or_else(
            || Response::Error {
                message: format!("unknown tab {}", tab_id.as_str()),
            },
            |page| Response::Snapshot { page },
        )
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn open_browser_tab(
        &self,
        tab_id: TabId,
        url: &str,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        let mut browser = self.browser.lock().await;
        if browser.is_none() {
            let config = CdpBackendConfig {
                browser_binary: self.config.browser.binary.clone(),
                headless: self.config.browser.headless,
            };
            *browser = Some(CdpBrowser::launch(&config).await?);
        }
        let Some(browser) = browser.as_mut() else {
            unreachable!("browser initialized above")
        };
        browser.open_tab(tab_id, url).await
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn navigate_browser_tab(
        &self,
        tab_id: &TabId,
        url: &str,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        let mut browser = self.browser.lock().await;
        if browser.is_none() {
            let config = CdpBackendConfig {
                browser_binary: self.config.browser.binary.clone(),
                headless: self.config.browser.headless,
            };
            *browser = Some(CdpBrowser::launch(&config).await?);
        }
        let Some(browser) = browser.as_mut() else {
            unreachable!("browser initialized above")
        };
        browser.navigate_tab(tab_id, url).await
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn reload_page(
        &self,
        tab_id: &TabId,
        fallback_url: &str,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        let mut browser = self.browser.lock().await;
        if browser.is_none() {
            let config = CdpBackendConfig {
                browser_binary: self.config.browser.binary.clone(),
                headless: self.config.browser.headless,
            };
            *browser = Some(CdpBrowser::launch(&config).await?);
            let Some(browser) = browser.as_mut() else {
                unreachable!("browser initialized above")
            };
            return browser.open_tab(tab_id.clone(), fallback_url).await;
        }
        let Some(browser) = browser.as_mut() else {
            unreachable!("browser checked above")
        };
        match browser.reload_tab(tab_id).await {
            Ok(page) => Ok(page),
            Err(_) => browser.open_tab(tab_id.clone(), fallback_url).await,
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn back_page(
        &self,
        tab_id: &TabId,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        let mut browser = self.browser.lock().await;
        let Some(browser) = browser.as_mut() else {
            return Err(trowser_browser_cdp::CdpError::NavigationHistory);
        };
        browser.back_tab(tab_id).await
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn forward_page(
        &self,
        tab_id: &TabId,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        let mut browser = self.browser.lock().await;
        let Some(browser) = browser.as_mut() else {
            return Err(trowser_browser_cdp::CdpError::NavigationHistory);
        };
        browser.forward_tab(tab_id).await
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn activate_action(
        &self,
        tab_id: &TabId,
        action_id: &ActionId,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        let mut browser = self.browser.lock().await;
        let Some(browser) = browser.as_mut() else {
            return Err(trowser_browser_cdp::CdpError::PageAction(
                "browser is not connected".to_string(),
            ));
        };
        browser.activate_action(tab_id, action_id).await
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn input_text_action(
        &self,
        tab_id: &TabId,
        action_id: &ActionId,
        text: &str,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        let mut browser = self.browser.lock().await;
        let Some(browser) = browser.as_mut() else {
            return Err(trowser_browser_cdp::CdpError::PageAction(
                "browser is not connected".to_string(),
            ));
        };
        browser.input_text_action(tab_id, action_id, text).await
    }

    async fn shutdown_browser(&self) {
        let mut browser = self.browser.lock().await;
        if let Some(browser) = browser.as_mut() {
            let _ = browser.shutdown().await;
        }
        *browser = None;
    }

    async fn snapshot(&self) -> SessionSnapshot {
        let state = self.state.lock().await;
        let tabs = state
            .tabs
            .iter()
            .map(|(id, page)| TabSummary {
                id: id.clone(),
                url: page.url.clone(),
                title: page.title.clone(),
                active: state.active_tab.as_ref() == Some(id),
            })
            .collect();

        SessionSnapshot {
            tabs,
            active_tab: state.active_tab.clone(),
        }
    }
}

async fn write_pid_file(pid_path: &Path) -> ServerResult<()> {
    if let Some(parent) = pid_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(pid_path, std::process::id().to_string()).await?;
    Ok(())
}

async fn remove_pid_file_if_current(pid_path: &Path) -> ServerResult<()> {
    if !pid_path.exists() {
        return Ok(());
    }

    let contents = tokio::fs::read_to_string(pid_path).await?;
    if contents.trim() == std::process::id().to_string() {
        tokio::fs::remove_file(pid_path).await?;
    }
    Ok(())
}

async fn write_ready_file_if_requested(socket_path: &Path) -> ServerResult<()> {
    let Some(ready_path) = std::env::var_os(TROWSER_DAEMON_READY_FILE_ENV) else {
        return Ok(());
    };
    let ready_path = std::path::PathBuf::from(ready_path);
    if let Some(parent) = ready_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&ready_path, socket_path.to_string_lossy().as_bytes()).await?;
    Ok(())
}

fn error_page(url: &str, error: &str) -> SemanticPage {
    SemanticPage {
        url: url.to_string(),
        title: format!("Failed to load: {url}"),
        blocks: vec![
            SemanticBlock::Heading {
                level: 1,
                text: "CDP extraction failed".to_string(),
                layout: None,
                region: SemanticRegion::Main,
            },
            SemanticBlock::Paragraph {
                text: error.to_string(),
                layout: None,
                region: SemanticRegion::Main,
            },
        ],
    }
}

#[cfg(unix)]
async fn write_message<T>(stream: &mut tokio::net::UnixStream, value: &T) -> ServerResult<()>
where
    T: serde::Serialize + Sync,
{
    let bytes = bmux_codec::to_vec(value)?;
    let len = u32::try_from(bytes.len()).map_err(|_| {
        ServerError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message too large",
        ))
    })?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(unix)]
async fn read_message<T>(stream: &mut tokio::net::UnixStream) -> ServerResult<T>
where
    T: serde::de::DeserializeOwned,
{
    let mut len = [0_u8; 4];
    stream.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    let mut bytes = vec![0_u8; len];
    stream.read_exact(&mut bytes).await?;
    bmux_codec::from_bytes(&bytes).map_err(ServerError::from)
}
