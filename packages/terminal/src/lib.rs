#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Terminal rendering primitives for trowser.

use trowser_render::{SemanticBlock, SemanticPage, SemanticRegion, reader_blocks, sanitize_page};
use trowser_terminal_image::TerminalImageCapabilities;

const CSS_PX_PER_CELL_X: f64 = 9.0;
const MIN_BLOCK_WIDTH: usize = 20;
const MAX_READER_INDENT: usize = 6;

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

/// A rendered terminal document with optional hit-test metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderedDocument {
    /// Rendered terminal lines.
    pub lines: Vec<RenderedLine>,
}

impl RenderedDocument {
    /// Converts the document to newline-terminated plain text.
    #[must_use]
    pub fn to_plain_text(&self) -> String {
        let mut output = String::new();
        for line in &self.lines {
            output.push_str(&line.text);
            output.push('\n');
        }
        output
    }
}

/// One rendered terminal line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderedLine {
    /// Visible line text.
    pub text: String,
    /// Link hitboxes on this line.
    pub links: Vec<LinkHitbox>,
}

/// Clickable link range within a rendered line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkHitbox {
    /// One-based link id shown in terminal output.
    pub link_id: u32,
    /// Inclusive start column.
    pub start_col: u16,
    /// Exclusive end column.
    pub end_col: u16,
}

impl LinkHitbox {
    /// Returns true if a terminal column is inside this hitbox.
    #[must_use]
    pub const fn contains(self, column: u16) -> bool {
        self.start_col <= column && column < self.end_col
    }
}

/// Renders a semantic page into terminal-safe reader-mode text.
#[must_use]
pub fn render_plain_text(page: &SemanticPage, options: &TerminalRenderOptions) -> String {
    render_document(page, options).to_plain_text()
}

/// Renders a semantic page into terminal lines plus link hit-test metadata.
#[must_use]
pub fn render_document(page: &SemanticPage, options: &TerminalRenderOptions) -> RenderedDocument {
    let page = sanitize_page(page);
    let width = usize::from(options.width.max(40));
    let mut document = RenderedDocument::default();

    push_header_lines(&page, width, &mut document);
    push_nav_summary_lines(&page, width, &mut document);

    let blocks = reader_blocks(&page.blocks);
    let mut next_link_id = 1usize;
    for block in blocks {
        push_reader_block_lines(block, &mut next_link_id, width, options, &mut document);
    }

    document
}

fn push_header_lines(page: &SemanticPage, width: usize, document: &mut RenderedDocument) {
    let mut output = String::new();
    render_header(page, width, &mut output);
    push_text_lines(document, &output, None);
}

fn push_nav_summary_lines(page: &SemanticPage, width: usize, document: &mut RenderedDocument) {
    let mut output = String::new();
    render_nav_summary(page, width, &mut output);
    push_text_lines(document, &output, None);
}

fn push_reader_block_lines(
    block: &SemanticBlock,
    next_link_id: &mut usize,
    terminal_width: usize,
    options: &TerminalRenderOptions,
    document: &mut RenderedDocument,
) {
    let placement = ReaderPlacement::from_block(block, terminal_width);
    match block {
        SemanticBlock::Heading { level, text, .. } => {
            document.lines.push(RenderedLine::default());
            document.lines.push(RenderedLine {
                text: format!(
                    "{}{} {text}",
                    " ".repeat(placement.indent),
                    "#".repeat(usize::from((*level).clamp(1, 6)))
                ),
                links: Vec::new(),
            });
            document.lines.push(RenderedLine::default());
        }
        SemanticBlock::Paragraph { text, .. } => {
            push_wrapped_lines(document, text, placement.indent, placement.width, None);
            document.lines.push(RenderedLine::default());
        }
        SemanticBlock::ListItem { text, .. } => {
            push_wrapped_lines(
                document,
                &format!("• {text}"),
                placement.indent,
                placement.width,
                None,
            );
        }
        SemanticBlock::Link { text, url, .. } => {
            let label = if text.is_empty() {
                url.as_str()
            } else {
                text.as_str()
            };
            let link_id = u32::try_from(*next_link_id).unwrap_or(u32::MAX);
            *next_link_id += 1;
            let value = format!("[{link_id}] {label} <{url}>");
            push_wrapped_lines(
                document,
                &value,
                placement.indent,
                placement.width,
                Some(link_id),
            );
            document.lines.push(RenderedLine::default());
        }
        SemanticBlock::Image {
            alt,
            url,
            width,
            height,
            ..
        } => {
            let mut output = String::new();
            render_image_box(alt, url, *width, *height, placement, options, &mut output);
            push_text_lines(document, &output, None);
        }
        SemanticBlock::FormControl {
            label,
            control_type,
            value,
            ..
        } => {
            let text = if value.is_empty() {
                format!("[{control_type}] {label}")
            } else {
                format!("[{control_type}] {label}: {value}")
            };
            push_wrapped_lines(document, &text, placement.indent, placement.width, None);
            document.lines.push(RenderedLine::default());
        }
    }
}

fn push_wrapped_lines(
    document: &mut RenderedDocument,
    text: &str,
    indent: usize,
    width: usize,
    link_id: Option<u32>,
) {
    for line in wrap_lines(text, width) {
        let text = format!("{}{}", " ".repeat(indent), line);
        let links = link_id.map_or_else(Vec::new, |link_id| {
            vec![LinkHitbox {
                link_id,
                start_col: u16::try_from(indent).unwrap_or(u16::MAX),
                end_col: u16::try_from(text.chars().count()).unwrap_or(u16::MAX),
            }]
        });
        document.lines.push(RenderedLine { text, links });
    }
}

fn push_text_lines(document: &mut RenderedDocument, text: &str, link_id: Option<u32>) {
    for line in text.lines() {
        let end_col = u16::try_from(line.chars().count()).unwrap_or(u16::MAX);
        let links = link_id.map_or_else(Vec::new, |link_id| {
            vec![LinkHitbox {
                link_id,
                start_col: 0,
                end_col,
            }]
        });
        document.lines.push(RenderedLine {
            text: line.to_string(),
            links,
        });
    }
}

fn render_header(page: &SemanticPage, width: usize, output: &mut String) {
    let title = if page.title.is_empty() {
        page.url.as_str()
    } else {
        page.title.as_str()
    };

    if title.is_empty() {
        return;
    }

    output.push('┌');
    output.push_str(&"─".repeat(width.saturating_sub(2)));
    output.push('┐');
    output.push('\n');
    output.push('│');
    output.push(' ');
    push_truncated(output, title, width.saturating_sub(4));
    pad_to_width(
        output,
        width.saturating_sub(2),
        title.chars().count().min(width.saturating_sub(4)) + 1,
    );
    output.push('│');
    output.push('\n');
    output.push('└');
    output.push_str(&"─".repeat(width.saturating_sub(2)));
    output.push('┘');
    output.push_str("\n\n");
}

fn render_nav_summary(page: &SemanticPage, width: usize, output: &mut String) {
    let nav_links = page
        .blocks
        .iter()
        .filter_map(nav_link_label)
        .take(6)
        .collect::<Vec<_>>();

    if nav_links.is_empty() {
        return;
    }

    let nav = format!("nav: {}", nav_links.join(" | "));
    render_wrapped(&nav, 0, width, output);
    output.push('\n');
}

fn nav_link_label(block: &SemanticBlock) -> Option<String> {
    let SemanticBlock::Link {
        text, url, region, ..
    } = block
    else {
        return None;
    };

    if !matches!(region, SemanticRegion::Header | SemanticRegion::Navigation) {
        return None;
    }

    Some(if text.is_empty() {
        url.clone()
    } else {
        text.clone()
    })
}

#[derive(Debug, Clone, Copy)]
struct ReaderPlacement {
    indent: usize,
    width: usize,
}

impl ReaderPlacement {
    fn from_block(block: &SemanticBlock, terminal_width: usize) -> Self {
        let raw_indent = block
            .layout()
            .filter(|layout| layout.is_non_empty())
            .map_or(0, |layout| {
                css_pixels_to_cells(layout.x, CSS_PX_PER_CELL_X, 0.0)
            });
        let indent = raw_indent.min(MAX_READER_INDENT);
        Self {
            indent,
            width: terminal_width.saturating_sub(indent).max(MIN_BLOCK_WIDTH),
        }
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "CSS pixel geometry is finite, non-negative, and intentionally quantized to terminal cells"
)]
fn css_pixels_to_cells(value: f64, cell_size: f64, minimum: f64) -> usize {
    if !value.is_finite() {
        return minimum as usize;
    }
    (value / cell_size).round().max(minimum) as usize
}

fn render_image_box(
    alt: &str,
    url: &str,
    image_width: Option<u32>,
    image_height: Option<u32>,
    placement: ReaderPlacement,
    options: &TerminalRenderOptions,
    output: &mut String,
) {
    let box_width = placement.width.clamp(40, 72);
    let title = if options.image_capabilities.supports_images() {
        "image available"
    } else {
        "image"
    };
    let dimensions = image_width
        .zip(image_height)
        .map(|(width, height)| format!(" {width}x{height}"))
        .unwrap_or_default();
    let label = format!("{title}{dimensions}");

    output.push_str(&" ".repeat(placement.indent));
    output.push('┌');
    output.push_str(&fit_line(&label, box_width.saturating_sub(2), '─'));
    output.push('┐');
    output.push('\n');

    if !alt.is_empty() {
        render_box_line(alt, placement.indent, box_width, output);
    }
    if !url.is_empty() {
        render_box_line(url, placement.indent, box_width, output);
    }

    output.push_str(&" ".repeat(placement.indent));
    output.push('└');
    output.push_str(&"─".repeat(box_width.saturating_sub(2)));
    output.push('┘');
    output.push_str("\n\n");
}

fn fit_line(text: &str, width: usize, fill: char) -> String {
    let mut output = String::new();
    let mut used = 0usize;
    for ch in text.chars().take(width) {
        output.push(ch);
        used += 1;
    }
    if width > used {
        output.push_str(&fill.to_string().repeat(width - used));
    }
    output
}

fn render_box_line(text: &str, indent: usize, width: usize, output: &mut String) {
    for line in wrap_lines(text, width.saturating_sub(4)) {
        output.push_str(&" ".repeat(indent));
        output.push('│');
        output.push(' ');
        output.push_str(&fit_line(&line, width.saturating_sub(4), ' '));
        output.push(' ');
        output.push('│');
        output.push('\n');
    }
}

fn render_wrapped(text: &str, indent: usize, width: usize, output: &mut String) {
    for line in wrap_lines(text, width) {
        output.push_str(&" ".repeat(indent));
        output.push_str(&line);
        output.push('\n');
    }
}

fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(MIN_BLOCK_WIDTH);
    let mut line_len = 0usize;
    let mut current = String::new();
    let mut lines = Vec::new();

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if line_len != 0 && line_len + 1 + word_len > width {
            lines.push(current);
            current = String::new();
            line_len = 0;
        } else if line_len != 0 {
            current.push(' ');
            line_len += 1;
        }
        current.push_str(word);
        line_len += word_len;
    }

    if !current.is_empty() {
        lines.push(current);
    }

    lines
}

fn push_truncated(output: &mut String, text: &str, width: usize) {
    for ch in text.chars().take(width) {
        output.push(ch);
    }
}

fn pad_to_width(output: &mut String, width: usize, used: usize) {
    if width > used {
        output.push_str(&" ".repeat(width - used));
    }
}

#[cfg(test)]
mod tests {
    use super::{TerminalRenderOptions, render_plain_text};
    use trowser_render::{BlockLayout, SemanticBlock, SemanticPage, SemanticRegion};

    #[test]
    fn renders_semantic_text() {
        let page = SemanticPage {
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            blocks: vec![SemanticBlock::Heading {
                level: 1,
                text: "Hello".to_string(),
                layout: None,
                region: SemanticRegion::Main,
            }],
        };

        let rendered = render_plain_text(&page, &TerminalRenderOptions::default());
        assert!(rendered.contains("Example"));
        assert!(rendered.contains("# Hello"));
    }

    #[test]
    fn caps_reader_indent() {
        let page = SemanticPage {
            url: String::new(),
            title: String::new(),
            blocks: vec![SemanticBlock::Paragraph {
                text: "Indented".to_string(),
                layout: Some(BlockLayout {
                    x: 360.0,
                    y: 0.0,
                    width: 300.0,
                    height: 20.0,
                }),
                region: SemanticRegion::Main,
            }],
        };

        let rendered = render_plain_text(&page, &TerminalRenderOptions::default());
        assert!(rendered.contains("      Indented"));
    }
}
