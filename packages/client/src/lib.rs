#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Client API for talking to the trowser daemon.

use thiserror::Error;
use trowser_config::ConfigPaths;
use trowser_ipc::{Request, Response};

/// Client-side errors.
#[derive(Debug, Error)]
pub enum ClientError {
    /// The daemon transport is not implemented yet.
    #[error("daemon transport is not implemented yet for endpoint {endpoint}")]
    TransportUnavailable {
        /// Endpoint that would have been used.
        endpoint: String,
    },
}

/// Result alias for client operations.
pub type ClientResult<T> = Result<T, ClientError>;

/// Client handle configured for a local trowser daemon.
#[derive(Debug, Clone)]
pub struct TrowserClient {
    paths: ConfigPaths,
}

impl TrowserClient {
    /// Creates a client handle using resolved config paths.
    #[must_use]
    pub const fn new(paths: ConfigPaths) -> Self {
        Self { paths }
    }

    /// Returns the resolved config paths used by this client.
    #[must_use]
    pub const fn paths(&self) -> &ConfigPaths {
        &self.paths
    }

    /// Sends a request to the daemon.
    ///
    /// # Errors
    ///
    /// Returns an error until daemon transport is implemented.
    pub fn send(&self, _request: Request) -> ClientResult<Response> {
        Err(ClientError::TransportUnavailable {
            endpoint: self
                .paths
                .server_socket_path()
                .to_string_lossy()
                .into_owned(),
        })
    }
}
