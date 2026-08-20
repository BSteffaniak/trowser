#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Facade crate for trowser.

#[cfg(feature = "browser")]
pub use trowser_browser as browser;
#[cfg(feature = "browser-cdp")]
pub use trowser_browser_cdp as browser_cdp;
#[cfg(feature = "cli")]
pub use trowser_cli as cli;
#[cfg(feature = "client")]
pub use trowser_client as client;
#[cfg(feature = "config")]
pub use trowser_config as config;
#[cfg(feature = "ipc")]
pub use trowser_ipc as ipc;
#[cfg(feature = "render")]
pub use trowser_render as render;
#[cfg(feature = "server")]
pub use trowser_server as server;
#[cfg(feature = "session-models")]
pub use trowser_session_models as session_models;
#[cfg(feature = "terminal")]
pub use trowser_terminal as terminal;
