#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Daemon runtime for trowser.

use std::collections::BTreeMap;
use std::sync::Arc;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, Notify};
use trowser_config::ConfigPaths;
use trowser_ipc::{
    ClientHello, PROTOCOL_VERSION, ProtocolCapabilities, Request, Response, ServerHello,
    ServerStatus,
};
use trowser_render::{SemanticBlock, SemanticPage};
use trowser_session_models::{SessionSnapshot, TabId, TabSummary};

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
    state: Arc<Mutex<ServerState>>,
    shutdown: Arc<Notify>,
}

impl TrowserServer {
    /// Creates a daemon runtime.
    #[must_use]
    pub fn new(paths: ConfigPaths) -> Self {
        Self {
            paths,
            state: Arc::new(Mutex::new(ServerState::default())),
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

            loop {
                tokio::select! {
                    signal = tokio::signal::ctrl_c() => {
                        signal?;
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

            if socket_path.exists() {
                tokio::fs::remove_file(&socket_path).await?;
            }
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
            Request::ListTabs => Response::Tabs(self.snapshot().await),
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
        let mut state = self.state.lock().await;
        let tab_id = TabId::new(format!("tab-{}", state.next_tab_id));
        state.next_tab_id += 1;
        let page = placeholder_page(&url);
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

fn placeholder_page(url: &str) -> SemanticPage {
    SemanticPage {
        url: url.to_string(),
        title: format!("Queued: {url}"),
        blocks: vec![
            SemanticBlock::Heading {
                level: 1,
                text: "trowser daemon is real".to_string(),
            },
            SemanticBlock::Paragraph {
                text: format!(
                    "The daemon accepted navigation to {url}. Chromium/CDP extraction is the next backend step."
                ),
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
