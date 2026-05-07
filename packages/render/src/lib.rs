#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Semantic render model and sanitization helpers for trowser.

use serde::{Deserialize, Serialize};

/// A sanitized page model suitable for terminal rendering.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SemanticPage {
    /// Page URL.
    pub url: String,
    /// Page title.
    pub title: String,
    /// Ordered semantic blocks extracted from the browser backend.
    pub blocks: Vec<SemanticBlock>,
}

/// Terminal-renderable semantic block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SemanticBlock {
    /// Heading with one-based HTML-like level.
    Heading { level: u8, text: String },
    /// Paragraph/body text.
    Paragraph { text: String },
    /// Link text plus target URL.
    Link { text: String, url: String },
    /// Image placeholder or enhanced image candidate.
    Image {
        alt: String,
        url: String,
        width: Option<u32>,
        height: Option<u32>,
    },
    /// Form control placeholder.
    FormControl {
        label: String,
        control_type: String,
        value: String,
    },
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
        SemanticBlock::Heading { level, text } => SemanticBlock::Heading {
            level: *level,
            text: sanitize_terminal_text(text),
        },
        SemanticBlock::Paragraph { text } => SemanticBlock::Paragraph {
            text: sanitize_terminal_text(text),
        },
        SemanticBlock::Link { text, url } => SemanticBlock::Link {
            text: sanitize_terminal_text(text),
            url: sanitize_terminal_text(url),
        },
        SemanticBlock::Image {
            alt,
            url,
            width,
            height,
        } => SemanticBlock::Image {
            alt: sanitize_terminal_text(alt),
            url: sanitize_terminal_text(url),
            width: *width,
            height: *height,
        },
        SemanticBlock::FormControl {
            label,
            control_type,
            value,
        } => SemanticBlock::FormControl {
            label: sanitize_terminal_text(label),
            control_type: sanitize_terminal_text(control_type),
            value: sanitize_terminal_text(value),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{SemanticBlock, SemanticPage, sanitize_terminal_text};

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
        };
        let encoded = serde_json::to_string(&block).expect("serialize block");
        assert!(encoded.contains("paragraph"));
    }
}
