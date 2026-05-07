#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! IPC protocol models for trowser.

use serde::{Deserialize, Serialize};
use trowser_render::SemanticPage;
use trowser_session_models::{SessionSnapshot, TabId};

/// Current trowser IPC protocol version.
pub const PROTOCOL_VERSION: u16 = 1;

/// Initial client hello.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    /// Client-supported protocol version.
    pub protocol_version: u16,
    /// Human-readable client name.
    pub client_name: String,
}

impl Default for ClientHello {
    fn default() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            client_name: "trowser-cli".to_string(),
        }
    }
}

/// Daemon hello response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    /// Selected protocol version.
    pub protocol_version: u16,
    /// Server capabilities.
    pub capabilities: ProtocolCapabilities,
}

/// IPC protocol capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtocolCapabilities {
    /// Server supports semantic page snapshots.
    pub semantic_snapshots: bool,
    /// Server supports tab listing.
    pub tabs: bool,
    /// Server supports navigation.
    pub navigation: bool,
}

/// Client request sent to the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Protocol handshake.
    Hello(ClientHello),
    /// Returns daemon status.
    Status,
    /// Opens a new tab to a URL.
    Open { url: String },
    /// Switches to a tab.
    SwitchTab { tab_id: TabId },
    /// Closes a tab.
    CloseTab { tab_id: TabId },
    /// Switches to the next tab.
    NextTab,
    /// Switches to the previous tab.
    PreviousTab,
    /// Returns a semantic snapshot for the active tab.
    ActiveSnapshot,
    /// Reloads the active tab.
    Reload,
    /// Navigates the active tab backward in browser history.
    Back,
    /// Navigates the active tab forward in browser history.
    Forward,
    /// Activates/clicks a form control in the active tab.
    ClickControl { control_id: u32 },
    /// Inputs text into a form control in the active tab.
    InputText { control_id: u32, text: String },
    /// Lists known tabs.
    ListTabs,
    /// Follows a reader-mode link in the active tab.
    FollowLink { link_id: u32 },
    /// Returns a semantic snapshot for a tab.
    Snapshot { tab_id: TabId },
    /// Requests graceful daemon shutdown.
    Shutdown,
}

/// Server response sent to a client request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    /// Protocol handshake response.
    Hello(ServerHello),
    /// Daemon status response.
    Status(ServerStatus),
    /// Opened/navigated tab response.
    Opened { tab_id: TabId },
    /// Session tab listing response.
    Tabs(SessionSnapshot),
    /// Semantic page snapshot response.
    Snapshot { page: SemanticPage },
    /// Active tab semantic page snapshot response.
    ActiveSnapshot { tab_id: TabId, page: SemanticPage },
    /// Empty success response.
    Ok,
    /// Error response.
    Error { message: String },
}

/// Daemon status information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerStatus {
    /// Daemon version string.
    pub version: String,
    /// Active runtime name.
    pub runtime_name: String,
}

/// Encodes a request using the shared bmux binary codec.
///
/// # Errors
///
/// Returns an error if serialization fails.
pub fn encode_request(request: &Request) -> Result<Vec<u8>, bmux_codec::Error> {
    bmux_codec::to_vec(request)
}

/// Decodes a request using the shared bmux binary codec.
///
/// # Errors
///
/// Returns an error if deserialization fails.
pub fn decode_request(bytes: &[u8]) -> Result<Request, bmux_codec::Error> {
    bmux_codec::from_bytes(bytes)
}

/// Encodes a response using the shared bmux binary codec.
///
/// # Errors
///
/// Returns an error if serialization fails.
pub fn encode_response(response: &Response) -> Result<Vec<u8>, bmux_codec::Error> {
    bmux_codec::to_vec(response)
}

/// Decodes a response using the shared bmux binary codec.
///
/// # Errors
///
/// Returns an error if deserialization fails.
pub fn decode_response(bytes: &[u8]) -> Result<Response, bmux_codec::Error> {
    bmux_codec::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::{
        ClientHello, Request, Response, ServerStatus, decode_request, decode_response,
        encode_request, encode_response,
    };

    #[test]
    fn request_round_trips_with_bmux_codec() {
        let request = Request::Hello(ClientHello::default());
        let encoded = encode_request(&request).expect("encode request");
        let decoded = decode_request(&encoded).expect("decode request");
        assert_eq!(decoded, request);
    }

    #[test]
    fn response_round_trips_with_bmux_codec() {
        let response = Response::Status(ServerStatus {
            version: "0.0.1-alpha.0".to_string(),
            runtime_name: "default".to_string(),
        });
        let encoded = encode_response(&response).expect("encode response");
        let decoded = decode_response(&encoded).expect("decode response");
        assert_eq!(decoded, response);
    }
}
