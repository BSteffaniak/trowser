#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Terminal image protocol capability detection for trowser.

use serde::{Deserialize, Serialize};

/// Supported terminal image protocol families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageProtocol {
    /// Kitty graphics protocol.
    Kitty,
    /// Sixel graphics.
    Sixel,
    /// iTerm2 inline images.
    Iterm2,
}

/// Image capabilities detected for the current terminal.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalImageCapabilities {
    /// Supported protocols in preference order.
    pub protocols: Vec<ImageProtocol>,
}

impl TerminalImageCapabilities {
    /// Returns true when at least one terminal image protocol is available.
    #[must_use]
    pub const fn supports_images(&self) -> bool {
        !self.protocols.is_empty()
    }

    /// Returns the preferred terminal image protocol, if any.
    #[must_use]
    pub fn preferred_protocol(&self) -> Option<ImageProtocol> {
        self.protocols.first().copied()
    }
}

/// Detects image support from common terminal environment variables.
#[must_use]
pub fn detect_from_env() -> TerminalImageCapabilities {
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();
    let mut protocols = Vec::new();

    if std::env::var_os("KITTY_WINDOW_ID").is_some() || term_program.eq_ignore_ascii_case("WezTerm")
    {
        protocols.push(ImageProtocol::Kitty);
    }

    if term.contains("sixel") || std::env::var_os("TROWSER_FORCE_SIXEL").is_some() {
        protocols.push(ImageProtocol::Sixel);
    }

    if term_program.eq_ignore_ascii_case("iTerm.app") {
        protocols.push(ImageProtocol::Iterm2);
    }

    TerminalImageCapabilities { protocols }
}
