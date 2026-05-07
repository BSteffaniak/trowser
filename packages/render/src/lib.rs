#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Semantic render model and sanitization helpers for trowser.

use serde::{Deserialize, Serialize};

/// A sanitized page model suitable for terminal rendering.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SemanticPage {
    /// Page URL.
    pub url: String,
    /// Page title.
    pub title: String,
    /// Ordered semantic blocks extracted from the browser backend.
    pub blocks: Vec<SemanticBlock>,
}

/// Browser-derived layout hint for a semantic block.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BlockLayout {
    /// Element x coordinate in CSS pixels.
    pub x: f64,
    /// Element y coordinate in CSS pixels.
    pub y: f64,
    /// Element width in CSS pixels.
    pub width: f64,
    /// Element height in CSS pixels.
    pub height: f64,
}

impl BlockLayout {
    /// Returns true when this layout hint has a useful non-empty rectangle.
    #[must_use]
    pub fn is_non_empty(self) -> bool {
        self.width > 0.0 && self.height > 0.0
    }
}

/// Browser semantic region for a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub enum SemanticRegion {
    /// Main page or article content.
    #[default]
    Main,
    /// Top-level page header.
    Header,
    /// Navigation/header/sidebar links.
    Navigation,
    /// Sidebar/aside content.
    Aside,
    /// Footer content.
    Footer,
    /// Unknown region.
    Unknown,
}

/// Terminal-renderable semantic block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SemanticBlock {
    /// Heading with one-based HTML-like level.
    Heading {
        level: u8,
        text: String,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// Paragraph/body text.
    Paragraph {
        text: String,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// List item text.
    ListItem {
        text: String,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// Link text plus target URL.
    Link {
        text: String,
        url: String,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// Image placeholder or enhanced image candidate.
    Image {
        alt: String,
        url: String,
        width: Option<u32>,
        height: Option<u32>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// Form control placeholder.
    FormControl {
        label: String,
        control_type: String,
        value: String,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
}

impl SemanticBlock {
    /// Returns the browser-derived layout hint for this block, if present.
    #[must_use]
    pub const fn layout(&self) -> Option<BlockLayout> {
        match self {
            Self::Heading { layout, .. }
            | Self::Paragraph { layout, .. }
            | Self::ListItem { layout, .. }
            | Self::Link { layout, .. }
            | Self::Image { layout, .. }
            | Self::FormControl { layout, .. } => *layout,
        }
    }

    /// Returns the semantic region for this block.
    #[must_use]
    pub const fn region(&self) -> SemanticRegion {
        match self {
            Self::Heading { region, .. }
            | Self::Paragraph { region, .. }
            | Self::ListItem { region, .. }
            | Self::Link { region, .. }
            | Self::Image { region, .. }
            | Self::FormControl { region, .. } => *region,
        }
    }
}

/// Removes terminal-dangerous control characters from web content.
#[must_use]
pub fn sanitize_terminal_text(input: &str) -> String {
    input
        .chars()
        .filter(|ch| matches!(ch, '\n' | '\r' | '\t') || !ch.is_control())
        .collect()
}

/// Sanitizes every text-bearing field in a page.
#[must_use]
pub fn sanitize_page(page: &SemanticPage) -> SemanticPage {
    SemanticPage {
        url: sanitize_terminal_text(&page.url),
        title: sanitize_terminal_text(&page.title),
        blocks: page.blocks.iter().map(sanitize_block).collect(),
    }
}

fn sanitize_block(block: &SemanticBlock) -> SemanticBlock {
    match block {
        SemanticBlock::Heading {
            level,
            text,
            layout,
            region,
        } => SemanticBlock::Heading {
            level: *level,
            text: sanitize_terminal_text(text),
            layout: *layout,
            region: *region,
        },
        SemanticBlock::Paragraph {
            text,
            layout,
            region,
        } => SemanticBlock::Paragraph {
            text: sanitize_terminal_text(text),
            layout: *layout,
            region: *region,
        },
        SemanticBlock::ListItem {
            text,
            layout,
            region,
        } => SemanticBlock::ListItem {
            text: sanitize_terminal_text(text),
            layout: *layout,
            region: *region,
        },
        SemanticBlock::Link {
            text,
            url,
            layout,
            region,
        } => SemanticBlock::Link {
            text: sanitize_terminal_text(text),
            url: sanitize_terminal_text(url),
            layout: *layout,
            region: *region,
        },
        SemanticBlock::Image {
            alt,
            url,
            width,
            height,
            layout,
            region,
        } => SemanticBlock::Image {
            alt: sanitize_terminal_text(alt),
            url: sanitize_terminal_text(url),
            width: *width,
            height: *height,
            layout: *layout,
            region: *region,
        },
        SemanticBlock::FormControl {
            label,
            control_type,
            value,
            layout,
            region,
        } => SemanticBlock::FormControl {
            label: sanitize_terminal_text(label),
            control_type: sanitize_terminal_text(control_type),
            value: sanitize_terminal_text(value),
            layout: *layout,
            region: *region,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{SemanticBlock, SemanticPage, SemanticRegion, sanitize_terminal_text};

    #[test]
    fn strips_escape_characters() {
        assert_eq!(sanitize_terminal_text("hello\u{1b}[31m"), "hello[31m");
    }

    #[test]
    fn page_defaults_are_empty() {
        let page = SemanticPage::default();
        assert!(page.blocks.is_empty());
    }

    #[test]
    fn block_is_serializable() {
        let block = SemanticBlock::Paragraph {
            text: "hello".to_string(),
            layout: None,
            region: SemanticRegion::Main,
        };
        let encoded = serde_json::to_string(&block).expect("serialize block");
        assert!(encoded.contains("Paragraph"));
    }
}
