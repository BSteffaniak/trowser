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

/// Maximum number of header/navigation links rendered in the reader nav summary.
pub const NAV_SUMMARY_LINK_LIMIT: usize = 6;

/// Link target exposed by reader-mode rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReaderLink {
    /// One-based link id shown in terminal output.
    pub id: u32,
    /// Link text.
    pub text: String,
    /// Link target URL.
    pub url: String,
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

/// Returns reader-mode link targets in the same order used by the terminal renderer.
#[must_use]
pub fn reader_links(page: &SemanticPage) -> Vec<ReaderLink> {
    nav_link_blocks(&page.blocks)
        .into_iter()
        .chain(
            reader_blocks(&page.blocks)
                .into_iter()
                .filter(|block| matches!(block, SemanticBlock::Link { .. })),
        )
        .enumerate()
        .filter_map(|(index, block)| {
            let SemanticBlock::Link { text, url, .. } = block else {
                return None;
            };
            Some(ReaderLink {
                id: u32::try_from(index + 1).unwrap_or(u32::MAX),
                text: text.clone(),
                url: url.clone(),
            })
        })
        .collect()
}

fn nav_link_blocks(blocks: &[SemanticBlock]) -> Vec<&SemanticBlock> {
    blocks
        .iter()
        .filter(|block| {
            matches!(
                block.region(),
                SemanticRegion::Header | SemanticRegion::Navigation
            )
        })
        .filter(|block| matches!(block, SemanticBlock::Link { .. }))
        .filter(|block| !is_noise_block(block))
        .take(NAV_SUMMARY_LINK_LIMIT)
        .collect()
}

/// Returns semantic blocks prioritized for reader-mode rendering.
#[must_use]
pub fn reader_blocks(blocks: &[SemanticBlock]) -> Vec<&SemanticBlock> {
    let main_blocks = blocks
        .iter()
        .filter(|block| block.region() == SemanticRegion::Main)
        .filter(|block| !is_noise_block(block))
        .collect::<Vec<_>>();

    if main_blocks.is_empty() {
        blocks
            .iter()
            .filter(|block| !is_noise_block(block))
            .collect()
    } else {
        main_blocks
    }
}

fn is_noise_block(block: &SemanticBlock) -> bool {
    match block {
        SemanticBlock::Image {
            alt, width, height, ..
        } => {
            alt.is_empty()
                && width
                    .zip(*height)
                    .is_some_and(|(width, height)| width <= 140 && height <= 40)
        }
        SemanticBlock::Link { text, url, .. } => text.trim().is_empty() && url.trim().is_empty(),
        _ => false,
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
    use super::{
        SemanticBlock, SemanticPage, SemanticRegion, reader_links, sanitize_terminal_text,
    };

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

    #[test]
    fn reader_links_are_numbered() {
        let page = SemanticPage {
            url: String::new(),
            title: String::new(),
            blocks: vec![SemanticBlock::Link {
                text: "Example".to_string(),
                url: "https://example.com".to_string(),
                layout: None,
                region: SemanticRegion::Main,
            }],
        };

        let links = reader_links(&page);
        assert_eq!(links[0].id, 1);
        assert_eq!(links[0].url, "https://example.com");
    }
}
