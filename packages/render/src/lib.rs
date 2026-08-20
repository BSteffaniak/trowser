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

/// Browser-derived media/embed kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MediaKind {
    /// Unknown embedded or replaced content.
    #[default]
    Unknown,
    /// HTML video element.
    Video,
    /// HTML audio element.
    Audio,
    /// Canvas drawing surface.
    Canvas,
    /// Nested browsing context.
    Iframe,
    /// Generic embed element.
    Embed,
    /// Generic object element.
    Object,
}

impl MediaKind {
    /// Returns a compact lowercase label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "media",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Canvas => "canvas",
            Self::Iframe => "iframe",
            Self::Embed => "embed",
            Self::Object => "object",
        }
    }
}

/// Browser-derived media source candidate.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaSource {
    /// Source URL.
    pub url: String,
    /// Declared MIME type, when present.
    pub mime_type: String,
    /// Declared media query, when present.
    pub media: String,
}

/// Inline image/thumbnail payload captured by the browser backend.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ImagePreview {
    /// MIME type for the payload, usually `image/png`.
    pub mime_type: String,
    /// Encoded image bytes.
    pub data: Vec<u8>,
    /// Pixel width of the payload, when known.
    pub width: Option<u32>,
    /// Pixel height of the payload, when known.
    pub height: Option<u32>,
}

impl ImagePreview {
    /// Returns true when this preview contains bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Return dimensions encoded in a supported image payload.
    #[must_use]
    pub fn intrinsic_dimensions(&self) -> Option<(u32, u32)> {
        if self.mime_type != "image/png" {
            return None;
        }
        png_dimensions(&self.data)
    }
}

fn png_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    const PNG_HEADER_LENGTH: usize = 24;
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < PNG_HEADER_LENGTH
        || data.get(..8) != Some(PNG_SIGNATURE)
        || data.get(12..16) != Some(b"IHDR")
    {
        return None;
    }
    let width = u32::from_be_bytes(data.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(data.get(20..24)?.try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
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
    /// Media/embed kind for replaced content nodes.
    pub media_kind: MediaKind,
    /// Primary media source URL for replaced content nodes.
    pub source_url: String,
    /// Poster image URL for video-like content, when present.
    pub poster_url: String,
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
        preview: Option<ImagePreview>,
        layout: Option<BlockLayout>,
        region: SemanticRegion,
    },
    /// Video, audio, canvas, iframe, embed, or object placeholder.
    Media {
        action_id: Option<ActionId>,
        media_kind: MediaKind,
        label: String,
        source_url: String,
        poster_url: String,
        sources: Vec<MediaSource>,
        width: Option<u32>,
        height: Option<u32>,
        duration_seconds: Option<u32>,
        current_time_seconds: Option<u32>,
        paused: bool,
        muted: bool,
        controls: bool,
        autoplay: bool,
        preview: Option<ImagePreview>,
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
            | Self::Media { layout, .. }
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
            | Self::Media { region, .. }
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
        media_kind: node.media_kind,
        source_url: sanitize_terminal_text(&node.source_url),
        poster_url: sanitize_terminal_text(&node.poster_url),
        children: node.children.iter().map(sanitize_layout_node).collect(),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "semantic block sanitization is exhaustive over wire model variants"
)]
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
            preview,
            layout,
            region,
        } => sanitize_image(
            alt,
            url,
            *width,
            *height,
            preview.as_ref(),
            *layout,
            *region,
        ),
        SemanticBlock::Media {
            action_id,
            media_kind,
            label,
            source_url,
            poster_url,
            sources,
            width,
            height,
            duration_seconds,
            current_time_seconds,
            paused,
            muted,
            controls,
            autoplay,
            preview,
            layout,
            region,
        } => sanitize_media(
            action_id.as_ref(),
            *media_kind,
            label,
            source_url,
            poster_url,
            sources,
            *width,
            *height,
            *duration_seconds,
            *current_time_seconds,
            *paused,
            *muted,
            *controls,
            *autoplay,
            preview.as_ref(),
            *layout,
            *region,
        ),
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
    preview: Option<&ImagePreview>,
    layout: Option<BlockLayout>,
    region: SemanticRegion,
) -> SemanticBlock {
    SemanticBlock::Image {
        alt: sanitize_terminal_text(alt),
        url: sanitize_terminal_text(url),
        width,
        height,
        preview: preview.cloned(),
        layout,
        region,
    }
}

#[allow(
    clippy::fn_params_excessive_bools,
    clippy::too_many_arguments,
    reason = "media semantics are a wire model with explicit browser-derived fields"
)]
fn sanitize_media(
    action_id: Option<&ActionId>,
    media_kind: MediaKind,
    label: &str,
    source_url: &str,
    poster_url: &str,
    sources: &[MediaSource],
    width: Option<u32>,
    height: Option<u32>,
    duration_seconds: Option<u32>,
    current_time_seconds: Option<u32>,
    paused: bool,
    muted: bool,
    controls: bool,
    autoplay: bool,
    preview: Option<&ImagePreview>,
    layout: Option<BlockLayout>,
    region: SemanticRegion,
) -> SemanticBlock {
    SemanticBlock::Media {
        action_id: action_id.cloned(),
        media_kind,
        label: sanitize_terminal_text(label),
        source_url: sanitize_terminal_text(source_url),
        poster_url: sanitize_terminal_text(poster_url),
        sources: sources.iter().map(sanitize_media_source).collect(),
        width,
        height,
        duration_seconds,
        current_time_seconds,
        paused,
        muted,
        controls,
        autoplay,
        preview: preview.cloned(),
        layout,
        region,
    }
}

fn sanitize_media_source(source: &MediaSource) -> MediaSource {
    MediaSource {
        url: sanitize_terminal_text(&source.url),
        mime_type: sanitize_terminal_text(&source.mime_type),
        media: sanitize_terminal_text(&source.media),
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
        ImagePreview, SemanticBlock, SemanticPage, SemanticRegion, reader_links,
        sanitize_terminal_text,
    };

    #[test]
    fn reads_intrinsic_png_dimensions() {
        let mut data = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        data.extend_from_slice(&37_u32.to_be_bytes());
        data.extend_from_slice(&19_u32.to_be_bytes());
        let preview = ImagePreview {
            mime_type: "image/png".to_string(),
            data,
            width: Some(1),
            height: Some(1),
        };
        assert_eq!(preview.intrinsic_dimensions(), Some((37, 19)));
    }

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
