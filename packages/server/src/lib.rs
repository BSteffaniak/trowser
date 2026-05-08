#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Daemon runtime for trowser.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, Notify};
use trowser_browser_cdp::{CdpBackendConfig, CdpBrowser, CdpResourcePolicy};
use trowser_config::{ConfigPaths, TROWSER_DAEMON_READY_FILE_ENV, TrowserConfig};
use trowser_ipc::{
    ClientHello, PROTOCOL_VERSION, ProtocolCapabilities, Request, Response, ServerHello,
    ServerStatus, ViewportHint,
};
use trowser_render::{
    SemanticBlock, SemanticPage, SemanticRegion, SemanticViewport, reader_blocks, reader_links,
};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabLifecycle {
    Awake,
    Sleeping,
}

impl TabLifecycle {
    const fn is_sleeping(self) -> bool {
        matches!(self, Self::Sleeping)
    }
}

#[derive(Debug, Clone)]
struct TabEntry {
    url: String,
    title: String,
    cached_page: Option<SemanticPage>,
    lifecycle: TabLifecycle,
    last_active_seq: u64,
}

impl TabEntry {
    fn awake_from_page(page: SemanticPage, last_active_seq: u64) -> Self {
        Self {
            url: page.url.clone(),
            title: page.title.clone(),
            cached_page: Some(page),
            lifecycle: TabLifecycle::Awake,
            last_active_seq,
        }
    }

    fn sleeping_from_summary(tab: &TabSummary, last_active_seq: u64) -> Self {
        Self {
            url: tab.url.clone(),
            title: tab.title.clone(),
            cached_page: None,
            lifecycle: TabLifecycle::Sleeping,
            last_active_seq,
        }
    }

    fn update_page(&mut self, page: SemanticPage, last_active_seq: u64) {
        self.url.clone_from(&page.url);
        self.title.clone_from(&page.title);
        self.cached_page = Some(page);
        self.lifecycle = TabLifecycle::Awake;
        self.last_active_seq = last_active_seq;
    }
}

#[derive(Debug, Clone, Default)]
struct ServerState {
    tabs: BTreeMap<TabId, TabEntry>,
    active_tab: Option<TabId>,
    next_tab_id: u64,
    activity_seq: u64,
    canceled_requests: BTreeSet<String>,
}

impl ServerState {
    const fn next_activity_seq(&mut self) -> u64 {
        let seq = self.activity_seq;
        self.activity_seq = self.activity_seq.saturating_add(1);
        seq
    }
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
            let _ = self.restore_session_from_disk().await;
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
            Request::Open { url, viewport } => self.handle_open(url, viewport).await,
            Request::Navigate {
                request_id,
                url,
                viewport,
            } => self.handle_navigate(request_id, url, viewport).await,
            Request::Cancel { request_id } => self.handle_cancel(request_id).await,
            Request::SwitchTab { tab_id } => self.handle_switch_tab(tab_id).await,
            Request::CloseTab { tab_id } => self.handle_close_tab(tab_id).await,
            Request::NextTab => self.handle_next_tab().await,
            Request::PreviousTab => self.handle_previous_tab().await,
            Request::ActiveSnapshot { viewport } => self.handle_active_snapshot(viewport).await,
            Request::Reload { viewport } => self.handle_reload(viewport).await,
            Request::Back { viewport } => self.handle_back(viewport).await,
            Request::Forward { viewport } => self.handle_forward(viewport).await,
            Request::ActivateAction {
                action_id,
                viewport,
            } => self.handle_activate_action(action_id, viewport).await,
            Request::InputTextAction {
                action_id,
                text,
                viewport,
            } => {
                self.handle_input_text_action(action_id, text, viewport)
                    .await
            }
            Request::ClickControl { control_id } => self.handle_click_control(control_id).await,
            Request::InputText { control_id, text } => {
                self.handle_input_text(control_id, text).await
            }
            Request::ListTabs => Response::Tabs(self.snapshot().await),
            Request::SaveSession => self.handle_save_session().await,
            Request::RestoreSession => self.handle_restore_session().await,
            Request::ClearSession => self.handle_clear_session().await,
            Request::FollowLink { link_id, viewport } => {
                self.handle_follow_link(link_id, viewport).await
            }
            Request::Snapshot { tab_id, viewport } => self.handle_snapshot(&tab_id, viewport).await,
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

    async fn handle_open(&self, url: String, viewport: Option<ViewportHint>) -> Response {
        self.open_url(url, None, viewport).await
    }

    async fn handle_navigate(
        &self,
        request_id: String,
        url: String,
        viewport: Option<ViewportHint>,
    ) -> Response {
        self.open_url(url, Some(request_id), viewport).await
    }

    async fn open_url(
        &self,
        url: String,
        request_id: Option<String>,
        viewport: Option<ViewportHint>,
    ) -> Response {
        if let Some(request_id) = request_id.as_ref()
            && self.is_request_canceled(request_id).await
        {
            return Response::Error {
                message: format!("request {request_id} canceled"),
            };
        }
        if let Some(max_tabs) = self.config.resources.max_tabs
            && self.open_tab_count().await >= max_tabs
        {
            return Response::Error {
                message: format!("maximum tab count ({max_tabs}) reached"),
            };
        }
        let tab_id = self.allocate_tab_id().await;
        let page = match tokio::time::timeout(
            Duration::from_secs(45),
            self.open_browser_tab(
                tab_id.clone(),
                &url,
                viewport.map(ViewportHint::semantic_viewport),
            ),
        )
        .await
        {
            Ok(Ok(page)) => page,
            Ok(Err(error)) => error_page(&url, &error.to_string()),
            Err(_) => error_page(&url, "timed out while loading/extracting page"),
        };
        if let Some(request_id) = request_id.as_ref()
            && self.take_canceled_request(request_id).await
        {
            return Response::Error {
                message: format!("request {request_id} canceled"),
            };
        }
        self.store_page(tab_id, page).await
    }

    async fn open_tab_count(&self) -> usize {
        let state = self.state.lock().await;
        let count = state.tabs.len();
        drop(state);
        count
    }

    async fn handle_cancel(&self, request_id: String) -> Response {
        let mut state = self.state.lock().await;
        state.canceled_requests.insert(request_id);
        drop(state);
        Response::Ok
    }

    async fn is_request_canceled(&self, request_id: &str) -> bool {
        let state = self.state.lock().await;
        let canceled = state.canceled_requests.contains(request_id);
        drop(state);
        canceled
    }

    async fn take_canceled_request(&self, request_id: &str) -> bool {
        let mut state = self.state.lock().await;
        let canceled = state.canceled_requests.remove(request_id);
        drop(state);
        canceled
    }

    async fn allocate_tab_id(&self) -> TabId {
        let mut state = self.state.lock().await;
        let tab_id = TabId::new(format!("tab-{}", state.next_tab_id));
        state.next_tab_id += 1;
        drop(state);
        tab_id
    }

    async fn handle_follow_link(&self, link_id: u32, viewport: Option<ViewportHint>) -> Response {
        let mut target = self.follow_link_target(link_id).await;
        if target.is_none() {
            let _ = self.handle_active_snapshot(viewport).await;
            target = self.follow_link_target(link_id).await;
        }
        let Some((tab_id, action_id, url)) = target else {
            return Response::Error {
                message: format!("unknown link id {link_id}"),
            };
        };

        if let Some(action_id) = action_id {
            return self.handle_activate_action(action_id, viewport).await;
        }

        let page = self
            .navigate_browser_tab(&tab_id, &url, viewport.map(ViewportHint::semantic_viewport))
            .await
            .unwrap_or_else(|error| error_page(&url, &error.to_string()));
        self.store_page(tab_id, page).await
    }

    async fn follow_link_target(&self, link_id: u32) -> Option<(TabId, Option<ActionId>, String)> {
        let state = self.state.lock().await;
        let tab_id = state.active_tab.clone()?;
        let page = state.tabs.get(&tab_id)?.cached_page.clone()?;
        drop(state);
        let target = reader_links(&page)
            .into_iter()
            .find(|link| link.id == link_id)?;
        Some((tab_id, target.action_id, target.url))
    }

    async fn control_action_id(&self, control_id: u32) -> Option<ActionId> {
        let state = self.state.lock().await;
        let tab_id = state.active_tab.clone()?;
        let page = state.tabs.get(&tab_id)?.cached_page.clone()?;
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
        let seq = state.next_activity_seq();
        let Some(tab) = state.tabs.get_mut(&tab_id) else {
            return Response::Error {
                message: format!("unknown tab {}", tab_id.as_str()),
            };
        };
        tab.last_active_seq = seq;
        state.active_tab = Some(tab_id.clone());
        drop(state);
        self.sleep_excess_tabs().await;
        let _ = self.save_session_to_disk().await;
        Response::Opened { tab_id }
    }

    async fn handle_save_session(&self) -> Response {
        self.save_session_to_disk().await.map_or_else(
            |error| Response::Error {
                message: error.to_string(),
            },
            |()| Response::Ok,
        )
    }

    async fn handle_restore_session(&self) -> Response {
        match self.restore_session_from_disk().await {
            Ok(Some(tab_id)) => Response::Opened { tab_id },
            Ok(None) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        }
    }

    async fn handle_clear_session(&self) -> Response {
        let path = self.paths.session_file();
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Response::Ok,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        }
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
        let _ = self.save_session_to_disk().await;
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
        let seq = state.next_activity_seq();
        if let Some(tab) = state.tabs.get_mut(&tab_id) {
            tab.last_active_seq = seq;
        }
        state.active_tab = Some(tab_id.clone());
        drop(state);
        self.sleep_excess_tabs().await;
        let _ = self.save_session_to_disk().await;
        Response::Opened { tab_id }
    }

    async fn handle_active_snapshot(&self, viewport: Option<ViewportHint>) -> Response {
        let state = self.state.lock().await;
        let Some(tab_id) = state.active_tab.clone() else {
            return Response::Error {
                message: "no active tab".to_string(),
            };
        };
        let Some(tab) = state.tabs.get(&tab_id) else {
            return Response::Error {
                message: format!("unknown active tab {}", tab_id.as_str()),
            };
        };
        let semantic_viewport = viewport.map(ViewportHint::semantic_viewport);
        let cached_page = tab.cached_page.clone();
        let needs_refresh = cached_page.as_ref().is_none_or(|page| {
            tab.lifecycle.is_sleeping()
                || semantic_viewport.is_some_and(|viewport| page.viewport != Some(viewport))
        });
        let url = tab.url.clone();
        drop(state);
        if needs_refresh {
            let page = self
                .refresh_tab_page(tab_id.clone(), &url, semantic_viewport)
                .await;
            return Response::ActiveSnapshot { tab_id, page };
        }
        let Some(page) = cached_page else {
            unreachable!("refresh required when active cached page is missing")
        };
        Response::ActiveSnapshot { tab_id, page }
    }

    async fn handle_reload(&self, viewport: Option<ViewportHint>) -> Response {
        let Some((tab_id, url)) = self.active_tab_url().await else {
            return Response::Error {
                message: "no active tab".to_string(),
            };
        };
        let page = self
            .reload_page(&tab_id, &url, viewport.map(ViewportHint::semantic_viewport))
            .await
            .unwrap_or_else(|error| error_page(&url, &error.to_string()));
        self.store_page(tab_id, page).await
    }

    async fn handle_back(&self, viewport: Option<ViewportHint>) -> Response {
        let Some(tab_id) = self.active_tab_id().await else {
            return Response::Error {
                message: "no active tab".to_string(),
            };
        };
        match self
            .back_page(&tab_id, viewport.map(ViewportHint::semantic_viewport))
            .await
        {
            Ok(page) => self.store_page(tab_id, page).await,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        }
    }

    async fn handle_forward(&self, viewport: Option<ViewportHint>) -> Response {
        let Some(tab_id) = self.active_tab_id().await else {
            return Response::Error {
                message: "no active tab".to_string(),
            };
        };
        match self
            .forward_page(&tab_id, viewport.map(ViewportHint::semantic_viewport))
            .await
        {
            Ok(page) => self.store_page(tab_id, page).await,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        }
    }

    async fn handle_activate_action(
        &self,
        action_id: ActionId,
        viewport: Option<ViewportHint>,
    ) -> Response {
        let Some(tab_id) = self.active_tab_id().await else {
            return Response::Error {
                message: "no active tab".to_string(),
            };
        };
        match self
            .activate_action(
                &tab_id,
                &action_id,
                viewport.map(ViewportHint::semantic_viewport),
            )
            .await
        {
            Ok(page) => self.store_page(tab_id, page).await,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        }
    }

    async fn handle_input_text_action(
        &self,
        action_id: ActionId,
        text: String,
        viewport: Option<ViewportHint>,
    ) -> Response {
        let Some(tab_id) = self.active_tab_id().await else {
            return Response::Error {
                message: "no active tab".to_string(),
            };
        };
        match self
            .input_text_action(
                &tab_id,
                &action_id,
                &text,
                viewport.map(ViewportHint::semantic_viewport),
            )
            .await
        {
            Ok(page) => self.store_page(tab_id, page).await,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        }
    }

    async fn handle_click_control(&self, control_id: u32) -> Response {
        let Some(action_id) = self.control_action_id_after_wake(control_id).await else {
            return Response::Error {
                message: format!("unknown control id {control_id}"),
            };
        };
        self.handle_activate_action(action_id, None).await
    }

    async fn handle_input_text(&self, control_id: u32, text: String) -> Response {
        let Some(action_id) = self.control_action_id_after_wake(control_id).await else {
            return Response::Error {
                message: format!("unknown control id {control_id}"),
            };
        };
        self.handle_input_text_action(action_id, text, None).await
    }

    async fn control_action_id_after_wake(&self, control_id: u32) -> Option<ActionId> {
        let mut action_id = self.control_action_id(control_id).await;
        if action_id.is_none() {
            let _ = self.handle_active_snapshot(None).await;
            action_id = self.control_action_id(control_id).await;
        }
        action_id
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

    async fn refresh_tab_page(
        &self,
        tab_id: TabId,
        url: &str,
        viewport: Option<SemanticViewport>,
    ) -> SemanticPage {
        let page = self
            .reload_page(&tab_id, url, viewport)
            .await
            .unwrap_or_else(|error| error_page(url, &error.to_string()));
        self.store_page_state(tab_id, page.clone()).await;
        self.sleep_excess_tabs().await;
        let _ = self.save_session_to_disk().await;
        page
    }

    async fn store_page(&self, tab_id: TabId, page: SemanticPage) -> Response {
        self.store_page_state(tab_id.clone(), page).await;
        self.sleep_excess_tabs().await;
        let _ = self.save_session_to_disk().await;
        Response::Opened { tab_id }
    }

    async fn store_page_state(&self, tab_id: TabId, page: SemanticPage) {
        let mut state = self.state.lock().await;
        let seq = state.next_activity_seq();
        if let Some(tab) = state.tabs.get_mut(&tab_id) {
            tab.update_page(page, seq);
        } else {
            state
                .tabs
                .insert(tab_id.clone(), TabEntry::awake_from_page(page, seq));
        }
        state.active_tab = Some(tab_id);
    }

    async fn handle_snapshot(&self, tab_id: &TabId, viewport: Option<ViewportHint>) -> Response {
        let state = self.state.lock().await;
        let Some(tab) = state.tabs.get(tab_id) else {
            return Response::Error {
                message: format!("unknown tab {}", tab_id.as_str()),
            };
        };
        let semantic_viewport = viewport.map(ViewportHint::semantic_viewport);
        let cached_page = tab.cached_page.clone();
        let needs_refresh = cached_page.as_ref().is_none_or(|page| {
            tab.lifecycle.is_sleeping()
                || semantic_viewport.is_some_and(|viewport| page.viewport != Some(viewport))
        });
        let url = tab.url.clone();
        drop(state);
        if needs_refresh {
            let page = self
                .refresh_tab_page(tab_id.clone(), &url, semantic_viewport)
                .await;
            return Response::Snapshot { page };
        }
        let Some(page) = cached_page else {
            unreachable!("refresh required when cached page is missing")
        };
        Response::Snapshot { page }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn open_browser_tab(
        &self,
        tab_id: TabId,
        url: &str,
        viewport: Option<SemanticViewport>,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        let mut browser = self.browser.lock().await;
        if browser.is_none() {
            let config = self.cdp_config();
            *browser = Some(CdpBrowser::launch(&config).await?);
        }
        let Some(browser) = browser.as_mut() else {
            unreachable!("browser initialized above")
        };
        browser.open_tab(tab_id, url, viewport).await
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn navigate_browser_tab(
        &self,
        tab_id: &TabId,
        url: &str,
        viewport: Option<SemanticViewport>,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        let mut browser = self.browser.lock().await;
        if browser.is_none() {
            let config = self.cdp_config();
            *browser = Some(CdpBrowser::launch(&config).await?);
        }
        let Some(browser) = browser.as_mut() else {
            unreachable!("browser initialized above")
        };
        browser.navigate_tab(tab_id, url, viewport).await
    }

    async fn ensure_browser_tab_awake(
        &self,
        tab_id: &TabId,
        viewport: Option<SemanticViewport>,
    ) -> Result<(), trowser_browser_cdp::CdpError> {
        let state = self.state.lock().await;
        let Some(tab) = state.tabs.get(tab_id) else {
            return Err(trowser_browser_cdp::CdpError::PageAction(format!(
                "unknown tab {}",
                tab_id.as_str()
            )));
        };
        let needs_wake = tab.lifecycle.is_sleeping() || tab.cached_page.is_none();
        let url = tab.url.clone();
        drop(state);

        if needs_wake {
            let page = self.reload_page(tab_id, &url, viewport).await?;
            self.store_page_state(tab_id.clone(), page).await;
        }
        Ok(())
    }

    async fn sleep_excess_tabs(&self) {
        let Some(max_awake_tabs) = self.config.resources.max_awake_tabs else {
            return;
        };

        let tabs_to_sleep = {
            let state = self.state.lock().await;
            let awake_count = state
                .tabs
                .values()
                .filter(|tab| tab.lifecycle == TabLifecycle::Awake)
                .count();
            let sleep_count = awake_count.saturating_sub(max_awake_tabs);
            if sleep_count == 0 {
                return;
            }

            let mut candidates = state
                .tabs
                .iter()
                .filter(|(tab_id, tab)| {
                    state.active_tab.as_ref() != Some(*tab_id)
                        && tab.lifecycle == TabLifecycle::Awake
                })
                .map(|(tab_id, tab)| (tab.last_active_seq, tab_id.clone()))
                .collect::<Vec<_>>();
            drop(state);
            candidates.sort_by_key(|(last_active_seq, tab_id)| (*last_active_seq, tab_id.clone()));
            candidates
                .into_iter()
                .take(sleep_count)
                .map(|(_, tab_id)| tab_id)
                .collect::<Vec<_>>()
        };

        if tabs_to_sleep.is_empty() {
            return;
        }

        {
            let mut browser = self.browser.lock().await;
            if let Some(browser) = browser.as_mut() {
                for tab_id in &tabs_to_sleep {
                    let _ = browser.close_tab(tab_id).await;
                }
            }
        }

        let mut state = self.state.lock().await;
        let active_tab = state.active_tab.clone();
        for tab_id in tabs_to_sleep {
            if active_tab.as_ref() != Some(&tab_id)
                && let Some(tab) = state.tabs.get_mut(&tab_id)
            {
                tab.lifecycle = TabLifecycle::Sleeping;
            }
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn reload_page(
        &self,
        tab_id: &TabId,
        fallback_url: &str,
        viewport: Option<SemanticViewport>,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        let mut browser = self.browser.lock().await;
        if browser.is_none() {
            let config = self.cdp_config();
            *browser = Some(CdpBrowser::launch(&config).await?);
            let Some(browser) = browser.as_mut() else {
                unreachable!("browser initialized above")
            };
            return browser
                .open_tab(tab_id.clone(), fallback_url, viewport)
                .await;
        }
        let Some(browser) = browser.as_mut() else {
            unreachable!("browser checked above")
        };
        match browser.reload_tab(tab_id, viewport).await {
            Ok(page) => Ok(page),
            Err(_) => {
                browser
                    .open_tab(tab_id.clone(), fallback_url, viewport)
                    .await
            }
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn back_page(
        &self,
        tab_id: &TabId,
        viewport: Option<SemanticViewport>,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        self.ensure_browser_tab_awake(tab_id, viewport).await?;
        let mut browser = self.browser.lock().await;
        let Some(browser) = browser.as_mut() else {
            return Err(trowser_browser_cdp::CdpError::NavigationHistory);
        };
        browser.back_tab(tab_id, viewport).await
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn forward_page(
        &self,
        tab_id: &TabId,
        viewport: Option<SemanticViewport>,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        self.ensure_browser_tab_awake(tab_id, viewport).await?;
        let mut browser = self.browser.lock().await;
        let Some(browser) = browser.as_mut() else {
            return Err(trowser_browser_cdp::CdpError::NavigationHistory);
        };
        browser.forward_tab(tab_id, viewport).await
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "browser mutex intentionally serializes access to persistent CDP page targets"
    )]
    async fn activate_action(
        &self,
        tab_id: &TabId,
        action_id: &ActionId,
        viewport: Option<SemanticViewport>,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        self.ensure_browser_tab_awake(tab_id, viewport).await?;
        let mut browser = self.browser.lock().await;
        let Some(browser) = browser.as_mut() else {
            return Err(trowser_browser_cdp::CdpError::PageAction(
                "browser is not connected".to_string(),
            ));
        };
        browser.activate_action(tab_id, action_id, viewport).await
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
        viewport: Option<SemanticViewport>,
    ) -> Result<SemanticPage, trowser_browser_cdp::CdpError> {
        self.ensure_browser_tab_awake(tab_id, viewport).await?;
        let mut browser = self.browser.lock().await;
        let Some(browser) = browser.as_mut() else {
            return Err(trowser_browser_cdp::CdpError::PageAction(
                "browser is not connected".to_string(),
            ));
        };
        browser
            .input_text_action(tab_id, action_id, text, viewport)
            .await
    }

    fn cdp_config(&self) -> CdpBackendConfig {
        CdpBackendConfig {
            browser_binary: self.config.browser.binary.clone(),
            headless: self.config.browser.headless,
            resource_policy: self.resource_policy(),
            profile_dir: self.profile_dir(),
        }
    }

    fn profile_dir(&self) -> Option<std::path::PathBuf> {
        match self.config.browser.profile.as_str() {
            "temporary" | "private" => None,
            profile => Some(self.paths.profiles_dir().join(profile)),
        }
    }

    const fn resource_policy(&self) -> CdpResourcePolicy {
        CdpResourcePolicy {
            block_images: self.config.resources.block_images,
            block_media: self.config.resources.block_video,
            block_fonts: self.config.resources.block_fonts,
            block_ads: self.config.resources.block_ads,
            block_trackers: self.config.resources.block_trackers,
        }
    }

    async fn shutdown_browser(&self) {
        let mut browser = self.browser.lock().await;
        if let Some(browser) = browser.as_mut() {
            let _ = browser.shutdown().await;
        }
        *browser = None;
    }

    async fn save_session_to_disk(&self) -> std::io::Result<()> {
        let snapshot = self.snapshot().await;
        let path = self.paths.session_file();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_vec_pretty(&snapshot).map_err(std::io::Error::other)?;
        tokio::fs::write(path, json).await
    }

    async fn restore_session_from_disk(&self) -> std::io::Result<Option<TabId>> {
        let path = self.paths.session_file();
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let snapshot =
            serde_json::from_slice::<SessionSnapshot>(&bytes).map_err(std::io::Error::other)?;
        self.shutdown_browser().await;
        {
            let mut state = self.state.lock().await;
            state.tabs.clear();
            state.active_tab = None;
            state.next_tab_id = next_tab_id_after(&snapshot);
            state.activity_seq = 0;
        }

        {
            let mut state = self.state.lock().await;
            for tab in &snapshot.tabs {
                let seq = state.next_activity_seq();
                state
                    .tabs
                    .insert(tab.id.clone(), TabEntry::sleeping_from_summary(tab, seq));
            }
        }
        let active_tab = snapshot
            .active_tab
            .filter(|tab_id| snapshot.tabs.iter().any(|tab| &tab.id == tab_id))
            .or_else(|| snapshot.tabs.first().map(|tab| tab.id.clone()));
        let mut state = self.state.lock().await;
        if let Some(tab_id) = &active_tab {
            let seq = state.next_activity_seq();
            if let Some(tab) = state.tabs.get_mut(tab_id) {
                tab.last_active_seq = seq;
            }
        }
        state.active_tab.clone_from(&active_tab);
        drop(state);
        Ok(active_tab)
    }

    async fn snapshot(&self) -> SessionSnapshot {
        let state = self.state.lock().await;
        let tabs = state
            .tabs
            .iter()
            .map(|(id, tab)| TabSummary {
                id: id.clone(),
                url: tab.url.clone(),
                title: tab.title.clone(),
                active: state.active_tab.as_ref() == Some(id),
                sleeping: tab.lifecycle.is_sleeping(),
            })
            .collect();

        SessionSnapshot {
            tabs,
            active_tab: state.active_tab.clone(),
        }
    }
}

fn next_tab_id_after(snapshot: &SessionSnapshot) -> u64 {
    snapshot
        .tabs
        .iter()
        .filter_map(|tab| tab.id.as_str().strip_prefix("tab-"))
        .filter_map(|suffix| suffix.parse::<u64>().ok())
        .max()
        .map_or(0, |value| value.saturating_add(1))
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
                text: "Could not load page".to_string(),
                layout: None,
                region: SemanticRegion::Main,
            },
            SemanticBlock::Paragraph {
                text: format!("URL: {url}"),
                layout: None,
                region: SemanticRegion::Main,
            },
            SemanticBlock::Paragraph {
                text: format!("Reason: {error}"),
                layout: None,
                region: SemanticRegion::Main,
            },
            SemanticBlock::Paragraph {
                text: "Try: r reload · b back · o open a different URL".to_string(),
                layout: None,
                region: SemanticRegion::Main,
            },
        ],
        layout_tree: None,
        viewport: None,
        scroll_y_px: 0,
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

#[cfg(test)]
mod tests {
    use super::{TabEntry, TabLifecycle, TrowserServer};
    use trowser_config::{ConfigPaths, TrowserConfig};
    use trowser_render::SemanticPage;
    use trowser_session_models::TabId;

    #[tokio::test]
    async fn sleep_excess_tabs_sleeps_lru_inactive_tabs() {
        let mut config = TrowserConfig::default();
        config.resources.max_awake_tabs = Some(1);
        let server = TrowserServer::new(ConfigPaths::resolve(), config);
        let active_tab = TabId::new("tab-2");

        {
            let mut state = server.state.lock().await;
            for index in 0..3 {
                let tab_id = TabId::new(format!("tab-{index}"));
                state.tabs.insert(
                    tab_id,
                    TabEntry::awake_from_page(
                        SemanticPage {
                            url: format!("https://example.com/{index}"),
                            title: format!("tab {index}"),
                            ..SemanticPage::default()
                        },
                        index,
                    ),
                );
            }
            state.active_tab = Some(active_tab.clone());
        }

        server.sleep_excess_tabs().await;

        let state = server.state.lock().await;
        let sleeping_tabs = ["tab-0", "tab-1"]
            .into_iter()
            .map(|tab_id| state.tabs.get(&TabId::new(tab_id)).map(|tab| tab.lifecycle))
            .collect::<Vec<_>>();
        let active_lifecycle = state.tabs.get(&active_tab).map(|tab| tab.lifecycle);
        drop(state);

        assert_eq!(
            sleeping_tabs,
            vec![Some(TabLifecycle::Sleeping), Some(TabLifecycle::Sleeping)]
        );
        assert_eq!(active_lifecycle, Some(TabLifecycle::Awake));
    }
}
