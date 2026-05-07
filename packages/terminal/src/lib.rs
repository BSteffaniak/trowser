#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Terminal rendering primitives for trowser.

use trowser_render::{SemanticBlock, SemanticPage, sanitize_page};
use trowser_terminal_image::TerminalImageCapabilities;

/// Terminal rendering options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRenderOptions {
    /// Maximum terminal width in cells.
    pub width: u16,
    /// Current terminal image capabilities.
    pub image_capabilities: TerminalImageCapabilities,
}

impl Default for TerminalRenderOptions {
    fn default() -> Self {
        Self {
            width: 80,
            image_capabilities: TerminalImageCapabilities::default(),
        }
    }
}

/// Renders a semantic page into plain terminal-safe text.
#[must_use]
pub fn render_plain_text(page: &SemanticPage, options: &TerminalRenderOptions) -> String {
    let page = sanitize_page(page);
    let mut output = String::new();

    if !page.title.is_empty() {
        output.push_str(&page.title);
        output.push_str("\n\n");
    }

    for block in &page.blocks {
        render_block(block, options, &mut output);
        output.push('\n');
    }

    output
}

fn render_block(block: &SemanticBlock, options: &TerminalRenderOptions, output: &mut String) {
    match block {
        SemanticBlock::Heading { level, text } => {
            output.push_str(&"#".repeat(usize::from((*level).clamp(1, 6))));
            output.push(' ');
            output.push_str(text);
            output.push('\n');
        }
        SemanticBlock::Paragraph { text } => output.push_str(&wrap_text(text, options.width)),
        SemanticBlock::Link { text, url } => {
            output.push_str(text);
            output.push_str(" <");
            output.push_str(url);
            output.push('>');
        }
        SemanticBlock::Image {
            alt,
            url,
            width,
            height,
        } => render_image_placeholder(alt, url, *width, *height, options, output),
        SemanticBlock::FormControl {
            label,
            control_type,
            value,
        } => {
            output.push('[');
            output.push_str(control_type);
            output.push_str("] ");
            output.push_str(label);
            if !value.is_empty() {
                output.push_str(": ");
                output.push_str(value);
            }
        }
    }
}

fn render_image_placeholder(
    alt: &str,
    url: &str,
    width: Option<u32>,
    height: Option<u32>,
    options: &TerminalRenderOptions,
    output: &mut String,
) {
    if options.image_capabilities.supports_images() {
        output.push_str("[image available");
    } else {
        output.push_str("[image");
    }

    if let (Some(width), Some(height)) = (width, height) {
        output.push(' ');
        output.push_str(&width.to_string());
        output.push('x');
        output.push_str(&height.to_string());
    }

    if !alt.is_empty() {
        output.push_str(": ");
        output.push_str(alt);
    }

    if !url.is_empty() {
        output.push_str(" <");
        output.push_str(url);
        output.push('>');
    }

    output.push(']');
}

fn wrap_text(text: &str, width: u16) -> String {
    let width = usize::from(width.max(20));
    let mut line_len = 0usize;
    let mut output = String::new();

    for word in text.split_whitespace() {
        let word_len = word.len();
        if line_len != 0 && line_len + 1 + word_len > width {
            output.push('\n');
            line_len = 0;
        } else if line_len != 0 {
            output.push(' ');
            line_len += 1;
        }
        output.push_str(word);
        line_len += word_len;
    }

    output
}

#[cfg(test)]
mod tests {
    use super::{TerminalRenderOptions, render_plain_text};
    use trowser_render::{SemanticBlock, SemanticPage};

    #[test]
    fn renders_semantic_text() {
        let page = SemanticPage {
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            blocks: vec![SemanticBlock::Heading {
                level: 1,
                text: "Hello".to_string(),
            }],
        };

        let rendered = render_plain_text(&page, &TerminalRenderOptions::default());
        assert!(rendered.contains("Example"));
        assert!(rendered.contains("# Hello"));
    }
}
