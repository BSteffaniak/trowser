#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Terminal image protocol capability detection and small protocol encoders for trowser.
//!
//! The shape mirrors the generic parts of `bmux_image`: detect host terminal
//! capabilities, prefer protocols that can place PNG payloads efficiently, and
//! keep protocol-specific escape generation outside the semantic renderer.

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
    /// Terminal cell width in pixels, when known.
    pub cell_pixel_width: u16,
    /// Terminal cell height in pixels, when known.
    pub cell_pixel_height: u16,
    /// Maximum image width in pixels, when known.
    pub max_pixel_width: Option<u32>,
    /// Maximum image height in pixels, when known.
    pub max_pixel_height: Option<u32>,
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

    /// Returns the preferred protocol that accepts PNG/file payloads directly.
    #[must_use]
    pub fn preferred_png_protocol(&self) -> Option<ImageProtocol> {
        self.protocols
            .iter()
            .copied()
            .find(|protocol| matches!(protocol, ImageProtocol::Kitty | ImageProtocol::Iterm2))
    }
}

/// A terminal image placement request for a PNG payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PngImagePlacement<'a> {
    /// PNG bytes.
    pub data: &'a [u8],
    /// Absolute terminal row where the image should appear.
    pub row: u16,
    /// Absolute terminal column where the image should appear.
    pub col: u16,
    /// Pixel width of the PNG payload.
    pub pixel_width: u32,
    /// Pixel height of the PNG payload.
    pub pixel_height: u32,
    /// Desired width in terminal cells.
    pub cell_width: u16,
    /// Desired height in terminal cells.
    pub cell_height: u16,
    /// Stable per-render image id for Kitty transmit/place.
    pub image_id: u32,
}

/// Encodes a PNG image for the best directly-supported terminal protocol.
#[must_use]
pub fn encode_png_image(
    capabilities: &TerminalImageCapabilities,
    placement: &PngImagePlacement<'_>,
) -> Option<Vec<u8>> {
    match capabilities.preferred_png_protocol()? {
        ImageProtocol::Kitty => Some(encode_kitty_png(placement)),
        ImageProtocol::Iterm2 => Some(encode_iterm2_png(placement)),
        ImageProtocol::Sixel => None,
    }
}

/// Encodes a Kitty graphics transmit+place sequence for PNG bytes.
#[must_use]
pub fn encode_kitty_png(placement: &PngImagePlacement<'_>) -> Vec<u8> {
    let payload = base64_encode(placement.data);
    let mut output = Vec::new();
    output.extend_from_slice(b"\x1b_");
    output.extend_from_slice(
        format!(
            "Ga=T,i={},f=100,s={},v={},c={},r={};{}",
            placement.image_id,
            placement.pixel_width,
            placement.pixel_height,
            placement.cell_width.max(1),
            placement.cell_height.max(1),
            payload
        )
        .as_bytes(),
    );
    output.extend_from_slice(b"\x1b\\");
    output.extend_from_slice(b"\x1b_");
    output.extend_from_slice(
        format!(
            "Ga=p,i={},p={},C={},R={},c={},r={}",
            placement.image_id,
            placement.image_id,
            placement.col,
            placement.row,
            placement.cell_width.max(1),
            placement.cell_height.max(1)
        )
        .as_bytes(),
    );
    output.extend_from_slice(b"\x1b\\");
    output
}

/// Encodes an iTerm2 inline image sequence for PNG bytes.
#[must_use]
pub fn encode_iterm2_png(placement: &PngImagePlacement<'_>) -> Vec<u8> {
    let payload = base64_encode(placement.data);
    format!(
        "\x1b]1337;File=inline=1;width={}px;height={}px;preserveAspectRatio=1:{}\x07",
        placement.pixel_width, placement.pixel_height, payload
    )
    .into_bytes()
}

/// Detects image support from common terminal environment variables.
#[must_use]
pub fn detect_from_env() -> TerminalImageCapabilities {
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let lc_terminal = std::env::var("LC_TERMINAL").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();
    let mut protocols = Vec::new();

    if std::env::var_os("KITTY_WINDOW_ID").is_some()
        || term.contains("kitty")
        || term_program.eq_ignore_ascii_case("WezTerm")
        || term_program.eq_ignore_ascii_case("ghostty")
    {
        push_protocol(&mut protocols, ImageProtocol::Kitty);
    }

    if term.contains("sixel")
        || std::env::var_os("TROWSER_FORCE_SIXEL").is_some()
        || term_program.eq_ignore_ascii_case("WezTerm")
        || term_program.eq_ignore_ascii_case("foot")
        || term.starts_with("foot")
        || term_program.eq_ignore_ascii_case("Alacritty")
        || term.starts_with("alacritty")
        || term_program.eq_ignore_ascii_case("Windows_Terminal")
    {
        push_protocol(&mut protocols, ImageProtocol::Sixel);
    }

    if term_program.eq_ignore_ascii_case("iTerm.app")
        || lc_terminal.eq_ignore_ascii_case("iTerm2")
        || term_program.eq_ignore_ascii_case("WezTerm")
    {
        push_protocol(&mut protocols, ImageProtocol::Iterm2);
    }

    let (cell_pixel_width, cell_pixel_height) = query_cell_pixel_size();

    TerminalImageCapabilities {
        protocols,
        cell_pixel_width,
        cell_pixel_height,
        max_pixel_width: None,
        max_pixel_height: None,
    }
}

fn push_protocol(protocols: &mut Vec<ImageProtocol>, protocol: ImageProtocol) {
    if !protocols.contains(&protocol) {
        protocols.push(protocol);
    }
}

/// Query the host terminal for cell pixel dimensions.
#[must_use]
#[cfg(unix)]
pub fn query_cell_pixel_size() -> (u16, u16) {
    use std::mem::MaybeUninit;
    use std::os::unix::io::AsRawFd;

    let fd = std::io::stdout().as_raw_fd();
    let mut ws = MaybeUninit::<Winsize>::uninit();
    let ret = unsafe { ioctl_tiocgwinsz(fd, ws.as_mut_ptr()) };
    if ret != 0 {
        return (0, 0);
    }
    let ws = unsafe { ws.assume_init() };
    if ws.ws_col == 0 || ws.ws_row == 0 || ws.ws_xpixel == 0 || ws.ws_ypixel == 0 {
        return (0, 0);
    }
    (ws.ws_xpixel / ws.ws_col, ws.ws_ypixel / ws.ws_row)
}

/// Query the host terminal for cell pixel dimensions.
#[must_use]
#[cfg(not(unix))]
pub const fn query_cell_pixel_size() -> (u16, u16) {
    (0, 0)
}

#[cfg(unix)]
#[repr(C)]
#[allow(
    clippy::struct_field_names,
    reason = "field names mirror the platform winsize ABI"
)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

#[cfg(target_os = "macos")]
const TIOCGWINSZ: u64 = 0x4008_7468;

#[cfg(target_os = "linux")]
const TIOCGWINSZ: u64 = 0x5413;

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
const TIOCGWINSZ: u64 = 0x5413;

#[cfg(unix)]
unsafe fn ioctl_tiocgwinsz(fd: i32, ws: *mut Winsize) -> i32 {
    unsafe extern "C" {
        fn ioctl(fd: i32, request: u64, ...) -> i32;
    }
    unsafe { ioctl(fd, TIOCGWINSZ, ws) }
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let triple = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        output.push(char::from(TABLE[((triple >> 18) & 0x3f) as usize]));
        output.push(char::from(TABLE[((triple >> 12) & 0x3f) as usize]));
        if chunk.len() > 1 {
            output.push(char::from(TABLE[((triple >> 6) & 0x3f) as usize]));
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(char::from(TABLE[(triple & 0x3f) as usize]));
        } else {
            output.push('=');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{ImageProtocol, PngImagePlacement, TerminalImageCapabilities, encode_png_image};

    #[test]
    fn prefers_png_capable_protocol_over_sixel() {
        let caps = TerminalImageCapabilities {
            protocols: vec![ImageProtocol::Sixel, ImageProtocol::Iterm2],
            ..TerminalImageCapabilities::default()
        };

        assert_eq!(caps.preferred_png_protocol(), Some(ImageProtocol::Iterm2));
    }

    #[test]
    fn encodes_kitty_png_escape() {
        let caps = TerminalImageCapabilities {
            protocols: vec![ImageProtocol::Kitty],
            ..TerminalImageCapabilities::default()
        };
        let placement = PngImagePlacement {
            data: b"png",
            row: 2,
            col: 3,
            pixel_width: 10,
            pixel_height: 20,
            cell_width: 4,
            cell_height: 5,
            image_id: 9,
        };

        let output = encode_png_image(&caps, &placement).expect("kitty should encode PNG");
        let output = String::from_utf8(output).expect("kitty escape is UTF-8");
        assert!(output.contains("Ga=T,i=9,f=100"));
        assert!(output.contains("Ga=p,i=9,p=9,C=3,R=2"));
    }
}
