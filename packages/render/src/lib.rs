#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Semantic render model and sanitization helpers for trowser.

use serde::{Deserialize, Serialize};
use trowser_session_models::ActionId;

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
    /// Optional browser-derived DOM/layout tree for layout-preserving rendering.
    pub layout_tree: Option<SemanticLayoutNode>,
    /// Viewport used by the browser when extracting this page.
    pub viewport: Option<SemanticViewport>,
    /// Browser vertical scroll offset in CSS pixels at extraction time.
    pub scroll_y_px: u32,
}

/// Browser viewport used for responsive layout extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SemanticViewport {
    /// Viewport width in CSS pixels.
    pub width_px: u32,
    /// Viewport height in CSS pixels.
    pub height_px: u32,
}

impl SemanticViewport {
    /// Creates a viewport from terminal cells and approximate cell size.
    #[must_use]
    pub fn from_terminal_cells(width_cells: u16, height_cells: u16) -> Self {
        Self {
            width_px: u32::from(width_cells).saturating_mul(9).max(1),
            height_px: u32::from(height_cells).saturating_mul(18).max(1),
        }
    }
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
    /// Stable DOM action id when this link maps to an actionable element.
    pub action_id: Option<ActionId>,
    /// Link text.
    pub text: String,
    /// Link target URL.
    pub url: String,
}

/// Inline text style hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TextRunStyle {
    /// Regular inline text.
    #[default]
    Normal,
    /// Strong/bold inline text.
    Strong,
    /// Emphasized inline text.
    Emphasis,
    /// Inline code text.
    Code,
}

/// Terminal-renderable inline text run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TextRun {
    /// Run text.
    pub text: String,
    /// Inline style hint.
    pub style: TextRunStyle,
    /// Link target URL when this run is clickable.
    pub url: Option<String>,
    /// Stable DOM action id when this run maps to an action.
    pub action_id: Option<ActionId>,
}

impl TextRun {
    /// Returns true when this run is a clickable link.
    #[must_use]
    pub fn is_link(&self) -> bool {
        self.url.as_ref().is_some_and(|url| !url.is_empty()) || self.action_id.is_some()
    }
}

/// Browser-derived layout tree node used by layout-preserving renderers.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SemanticLayoutNode {
    /// Lowercase DOM tag name or synthetic node name.
    pub tag: String,
    /// ARIA role when present.
    pub role: String,
    /// Browser semantic region for this node.
    pub region: SemanticRegion,
    /// Browser-derived geometry for this node.
    pub layout: Option<BlockLayout>,
    /// CSS display hint such as block/flex/grid.
    pub display: String,
    /// Direct visible text label for compact nodes.
    pub text: String,
    /// Inline text runs for text-bearing nodes.
    pub runs: Vec<TextRun>,
    /// Stable DOM action id when this node is actionable.
    pub action_id: Option<ActionId>,
    /// Link target URL when this node is a link.
    pub url: String,
    /// Control type when this node is a control.
    pub control_type: String,
    /// Current control value when available.
    pub value: String,
    /// Child layout nodes.
    pub children: Vec<Self>,
}

impl SemanticLayoutNode {
    /// Returns true when this node has useful geometry.
    #[must_use]
    pub fn has_layout(&self) -> bool {
        self.layout.is_some_and(BlockLayout::is_non_empty)
    }
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
    /// Paragraph/body text with inline links and styles.
    RichParagraph {
        runs: Vec<TextRun>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// List item text.
    ListItem {
        text: String,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// List item text with inline links and styles.
    RichListItem {
        runs: Vec<TextRun>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// Block quote text.
    Quote {
        runs: Vec<TextRun>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// Preformatted/code block text.
    CodeBlock {
        text: String,
        language: Option<String>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// Simple table data.
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// Horizontal rule / thematic break.
    HorizontalRule {
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// Link text plus target URL.
    Link {
        action_id: Option<ActionId>,
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
        action_id: Option<ActionId>,
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
            | Self::RichParagraph { layout, .. }
            | Self::ListItem { layout, .. }
            | Self::RichListItem { layout, .. }
            | Self::Quote { layout, .. }
            | Self::CodeBlock { layout, .. }
            | Self::Table { layout, .. }
            | Self::HorizontalRule { layout, .. }
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
            | Self::RichParagraph { region, .. }
            | Self::ListItem { region, .. }
            | Self::RichListItem { region, .. }
            | Self::Quote { region, .. }
            | Self::CodeBlock { region, .. }
            | Self::Table { region, .. }
            | Self::HorizontalRule { region, .. }
            | Self::Link { region, .. }
            | Self::Image { region, .. }
            | Self::FormControl { region, .. } => *region,
        }
    }
}

/// Returns reader-mode link targets in the same order used by the terminal renderer.
#[must_use]
pub fn reader_links(page: &SemanticPage) -> Vec<ReaderLink> {
    nav_link_targets(&page.blocks)
        .into_iter()
        .chain(
            reader_blocks(&page.blocks)
                .into_iter()
                .flat_map(link_targets_from_block),
        )
        .enumerate()
        .map(|(index, (action_id, text, url))| ReaderLink {
            id: u32::try_from(index + 1).unwrap_or(u32::MAX),
            action_id,
            text,
            url,
        })
        .collect()
}

fn nav_link_targets(blocks: &[SemanticBlock]) -> Vec<(Option<ActionId>, String, String)> {
    blocks
        .iter()
        .filter(|block| {
            matches!(
                block.region(),
                SemanticRegion::Header | SemanticRegion::Navigation
            )
        })
        .filter(|block| !is_noise_block(block))
        .flat_map(link_targets_from_block)
        .take(NAV_SUMMARY_LINK_LIMIT)
        .collect()
}

fn link_targets_from_block(block: &SemanticBlock) -> Vec<(Option<ActionId>, String, String)> {
    match block {
        SemanticBlock::Link {
            action_id,
            text,
            url,
            ..
        } => vec![(action_id.clone(), text.clone(), url.clone())],
        SemanticBlock::RichParagraph { runs, .. }
        | SemanticBlock::RichListItem { runs, .. }
        | SemanticBlock::Quote { runs, .. } => runs
            .iter()
            .filter_map(|run| {
                let url = run.url.clone()?;
                (!url.is_empty()).then(|| (run.action_id.clone(), run.text.clone(), url))
            })
            .collect(),
        _ => Vec::new(),
    }
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
        SemanticBlock::RichParagraph { runs, .. }
        | SemanticBlock::RichListItem { runs, .. }
        | SemanticBlock::Quote { runs, .. } => runs.iter().all(|run| run.text.trim().is_empty()),
        SemanticBlock::CodeBlock { text, .. } => text.trim().is_empty(),
        SemanticBlock::Table { headers, rows, .. } => headers.is_empty() && rows.is_empty(),
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
        layout_tree: page.layout_tree.as_ref().map(sanitize_layout_node),
        viewport: page.viewport,
        scroll_y_px: page.scroll_y_px,
    }
}

fn sanitize_layout_node(node: &SemanticLayoutNode) -> SemanticLayoutNode {
    SemanticLayoutNode {
        tag: sanitize_terminal_text(&node.tag),
        role: sanitize_terminal_text(&node.role),
        region: node.region,
        layout: node.layout,
        display: sanitize_terminal_text(&node.display),
        text: sanitize_terminal_text(&node.text),
        runs: node.runs.iter().map(sanitize_text_run).collect(),
        action_id: node.action_id.clone(),
        url: sanitize_terminal_text(&node.url),
        control_type: sanitize_terminal_text(&node.control_type),
        value: sanitize_terminal_text(&node.value),
        children: node.children.iter().map(sanitize_layout_node).collect(),
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
        SemanticBlock::RichParagraph {
            runs,
            layout,
            region,
        } => sanitize_rich_paragraph(runs, *layout, *region),
        SemanticBlock::ListItem {
            text,
            layout,
            region,
        } => SemanticBlock::ListItem {
            text: sanitize_terminal_text(text),
            layout: *layout,
            region: *region,
        },
        SemanticBlock::RichListItem {
            runs,
            layout,
            region,
        } => sanitize_rich_list_item(runs, *layout, *region),
        SemanticBlock::Quote {
            runs,
            layout,
            region,
        } => sanitize_quote(runs, *layout, *region),
        SemanticBlock::CodeBlock {
            text,
            language,
            layout,
            region,
        } => sanitize_code_block(text, language.as_deref(), *layout, *region),
        SemanticBlock::Table {
            headers,
            rows,
            layout,
            region,
        } => sanitize_table(headers, rows, *layout, *region),
        SemanticBlock::HorizontalRule { layout, region } => SemanticBlock::HorizontalRule {
            layout: *layout,
            region: *region,
        },
        SemanticBlock::Link {
            action_id,
            text,
            url,
            layout,
            region,
        } => SemanticBlock::Link {
            action_id: action_id.clone(),
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
        } => sanitize_image(alt, url, *width, *height, *layout, *region),
        SemanticBlock::FormControl {
            action_id,
            label,
            control_type,
            value,
            layout,
            region,
        } => sanitize_form_control(
            action_id.as_ref(),
            label,
            control_type,
            value,
            *layout,
            *region,
        ),
    }
}

fn sanitize_image(
    alt: &str,
    url: &str,
    width: Option<u32>,
    height: Option<u32>,
    layout: Option<BlockLayout>,
    region: SemanticRegion,
) -> SemanticBlock {
    SemanticBlock::Image {
        alt: sanitize_terminal_text(alt),
        url: sanitize_terminal_text(url),
        width,
        height,
        layout,
        region,
    }
}

fn sanitize_form_control(
    action_id: Option<&ActionId>,
    label: &str,
    control_type: &str,
    value: &str,
    layout: Option<BlockLayout>,
    region: SemanticRegion,
) -> SemanticBlock {
    SemanticBlock::FormControl {
        action_id: action_id.cloned(),
        label: sanitize_terminal_text(label),
        control_type: sanitize_terminal_text(control_type),
        value: sanitize_terminal_text(value),
        layout,
        region,
    }
}

fn sanitize_rich_paragraph(
    runs: &[TextRun],
    layout: Option<BlockLayout>,
    region: SemanticRegion,
) -> SemanticBlock {
    SemanticBlock::RichParagraph {
        runs: runs.iter().map(sanitize_text_run).collect(),
        layout,
        region,
    }
}

fn sanitize_rich_list_item(
    runs: &[TextRun],
    layout: Option<BlockLayout>,
    region: SemanticRegion,
) -> SemanticBlock {
    SemanticBlock::RichListItem {
        runs: runs.iter().map(sanitize_text_run).collect(),
        layout,
        region,
    }
}

fn sanitize_quote(
    runs: &[TextRun],
    layout: Option<BlockLayout>,
    region: SemanticRegion,
) -> SemanticBlock {
    SemanticBlock::Quote {
        runs: runs.iter().map(sanitize_text_run).collect(),
        layout,
        region,
    }
}

fn sanitize_code_block(
    text: &str,
    language: Option<&str>,
    layout: Option<BlockLayout>,
    region: SemanticRegion,
) -> SemanticBlock {
    SemanticBlock::CodeBlock {
        text: sanitize_terminal_text(text),
        language: language.map(sanitize_terminal_text),
        layout,
        region,
    }
}

fn sanitize_table(
    headers: &[String],
    rows: &[Vec<String>],
    layout: Option<BlockLayout>,
    region: SemanticRegion,
) -> SemanticBlock {
    SemanticBlock::Table {
        headers: headers
            .iter()
            .map(|value| sanitize_terminal_text(value))
            .collect(),
        rows: rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|value| sanitize_terminal_text(value))
                    .collect()
            })
            .collect(),
        layout,
        region,
    }
}

fn sanitize_text_run(run: &TextRun) -> TextRun {
    TextRun {
        text: sanitize_terminal_text(&run.text),
        style: run.style,
        url: run.url.as_ref().map(|value| sanitize_terminal_text(value)),
        action_id: run.action_id.clone(),
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
                action_id: None,
                text: "Example".to_string(),
                url: "https://example.com".to_string(),
                layout: None,
                region: SemanticRegion::Main,
            }],
            layout_tree: None,
            viewport: None,
            scroll_y_px: 0,
        };

        let links = reader_links(&page);
        assert_eq!(links[0].id, 1);
        assert_eq!(links[0].url, "https://example.com");
    }
}
