#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Client API for talking to the trowser daemon.

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use trowser_config::ConfigPaths;
use trowser_ipc::{Request, Response};

/// Client-side errors.
#[derive(Debug, Error)]
pub enum ClientError {
    /// The daemon transport is not implemented on this platform.
    #[error("daemon transport is not implemented yet for endpoint {endpoint}")]
    TransportUnavailable {
        /// Endpoint that would have been used.
        endpoint: String,
    },
    /// Local daemon connection failed.
    #[error("failed to connect to trowser daemon at {endpoint}: {source}")]
    Connect {
        /// Endpoint that failed.
        endpoint: String,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// Local daemon I/O failed.
    #[error("daemon I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Request encoding failed.
    #[error("failed to encode daemon request: {0}")]
    Encode(#[from] bmux_codec::Error),
    /// Server returned an error response.
    #[error("daemon returned an error: {0}")]
    Server(String),
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
    /// Returns an error when connecting, encoding, writing, reading, or decoding fails.
    pub async fn send(&self, request: &Request) -> ClientResult<Response> {
        let endpoint = self
            .paths
            .server_socket_path()
            .to_string_lossy()
            .into_owned();
        #[cfg(unix)]
        {
            let mut stream = tokio::net::UnixStream::connect(self.paths.server_socket_path())
                .await
                .map_err(|source| ClientError::Connect {
                    endpoint: endpoint.clone(),
                    source,
                })?;
            write_message(&mut stream, request).await?;
            let response: Response = read_message(&mut stream).await?;
            if let Response::Error { message } = response {
                return Err(ClientError::Server(message));
            }
            Ok(response)
        }
        #[cfg(not(unix))]
        {
            let _ = request;
            Err(ClientError::TransportUnavailable { endpoint })
        }
    }
}

#[cfg(unix)]
async fn write_message<T>(stream: &mut tokio::net::UnixStream, value: &T) -> ClientResult<()>
where
    T: serde::Serialize + Sync,
{
    let bytes = bmux_codec::to_vec(value)?;
    let len = u32::try_from(bytes.len()).map_err(|_| {
        ClientError::Io(std::io::Error::new(
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
async fn read_message<T>(stream: &mut tokio::net::UnixStream) -> ClientResult<T>
where
    T: serde::de::DeserializeOwned,
{
    let mut len = [0_u8; 4];
    stream.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    let mut bytes = vec![0_u8; len];
    stream.read_exact(&mut bytes).await?;
    bmux_codec::from_bytes(&bytes).map_err(ClientError::from)
}
