#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Terminal rendering primitives for trowser.

use trowser_render::{
    NAV_SUMMARY_LINK_LIMIT, SemanticBlock, SemanticPage, SemanticRegion, reader_blocks,
    sanitize_page,
};
use trowser_session_models::ActionId;
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

/// Terminal presentation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderMode {
    /// Prioritize readable main/article content.
    #[default]
    Reader,
    /// Preserve more page chrome such as header/nav/footer.
    Page,
    /// Show a compact headings/actions overview.
    Outline,
}

impl RenderMode {
    /// Returns the next render mode in the interactive cycle.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Reader => Self::Page,
            Self::Page => Self::Outline,
            Self::Outline => Self::Reader,
        }
    }

    /// Returns a short label for status bars.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Page => "page",
            Self::Outline => "outline",
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
    /// Styled spans for terminal presentation.
    pub spans: Vec<StyledSpan>,
    /// Link hitboxes on this line.
    pub links: Vec<LinkHitbox>,
    /// Form-control hitboxes on this line.
    pub controls: Vec<ControlHitbox>,
}

/// A styled text span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    /// Span text.
    pub text: String,
    /// Span style class.
    pub style: SpanStyle,
}

/// Semantic style classes used by terminal clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpanStyle {
    /// Regular text.
    #[default]
    Normal,
    /// Browser chrome / borders.
    Chrome,
    /// Heading text.
    Heading,
    /// Link/action text.
    Link,
    /// Form-control/action text.
    Control,
    /// Less important surrounding text.
    Dim,
    /// Error/warning text.
    Error,
}

/// Clickable link range within a rendered line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkHitbox {
    /// One-based link id shown in terminal output.
    pub link_id: u32,
    /// Stable DOM action id for this link when available.
    pub action_id: Option<ActionId>,
    /// Inclusive start column.
    pub start_col: u16,
    /// Exclusive end column.
    pub end_col: u16,
}

impl LinkHitbox {
    /// Returns true if a terminal column is inside this hitbox.
    #[must_use]
    pub const fn contains(&self, column: u16) -> bool {
        self.start_col <= column && column < self.end_col
    }
}

/// Clickable form-control range within a rendered line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlHitbox {
    /// One-based control id shown in terminal output.
    pub control_id: u32,
    /// Stable DOM action id for this control when available.
    pub action_id: Option<ActionId>,
    /// Default click behavior for this control.
    pub action: ControlHitboxAction,
    /// Inclusive start column.
    pub start_col: u16,
    /// Exclusive end column.
    pub end_col: u16,
}

impl ControlHitbox {
    /// Returns true if a terminal column is inside this hitbox.
    #[must_use]
    pub const fn contains(&self, column: u16) -> bool {
        self.start_col <= column && column < self.end_col
    }
}

/// Suggested behavior when clicking a rendered form control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlHitboxAction {
    /// Prompt for text and fill the control.
    InputText,
    /// Activate/click the control.
    Click,
}

/// Renders a semantic page into terminal-safe reader-mode text.
#[must_use]
pub fn render_plain_text(page: &SemanticPage, options: &TerminalRenderOptions) -> String {
    render_document(page, options).to_plain_text()
}

/// Renders a semantic page into terminal lines plus link hit-test metadata.
#[must_use]
pub fn render_document(page: &SemanticPage, options: &TerminalRenderOptions) -> RenderedDocument {
    render_document_mode(page, options, RenderMode::Reader)
}

/// Renders a semantic page in the requested presentation mode.
#[must_use]
pub fn render_document_mode(
    page: &SemanticPage,
    options: &TerminalRenderOptions,
    mode: RenderMode,
) -> RenderedDocument {
    let page = sanitize_page(page);
    let width = usize::from(options.width.max(40));
    let mut document = RenderedDocument::default();

    push_header_lines(&page, width, &mut document);

    let mut next_link_id = 1usize;
    let mut next_control_id = 1usize;
    match mode {
        RenderMode::Reader => {
            push_nav_summary_lines(&page, width, &mut next_link_id, &mut document);
            for block in reader_blocks(&page.blocks) {
                push_reader_block_lines(
                    block,
                    &mut next_link_id,
                    &mut next_control_id,
                    width,
                    options,
                    &mut document,
                );
            }
        }
        RenderMode::Page => {
            for block in &page.blocks {
                push_reader_block_lines(
                    block,
                    &mut next_link_id,
                    &mut next_control_id,
                    width,
                    options,
                    &mut document,
                );
            }
        }
        RenderMode::Outline => push_outline_lines(
            &page,
            &mut next_link_id,
            &mut next_control_id,
            width,
            &mut document,
        ),
    }

    document
}

fn push_header_lines(page: &SemanticPage, width: usize, document: &mut RenderedDocument) {
    let mut output = String::new();
    render_header(page, width, &mut output);
    push_text_lines(document, &output, None);
}

fn line_with_style(text: String, style: SpanStyle) -> RenderedLine {
    RenderedLine {
        spans: vec![StyledSpan {
            text: text.clone(),
            style,
        }],
        text,
        links: Vec::new(),
        controls: Vec::new(),
    }
}

fn push_nav_summary_lines(
    page: &SemanticPage,
    width: usize,
    next_link_id: &mut usize,
    document: &mut RenderedDocument,
) {
    let nav_links = page
        .blocks
        .iter()
        .filter_map(nav_link_target)
        .take(NAV_SUMMARY_LINK_LIMIT)
        .collect::<Vec<_>>();

    if nav_links.is_empty() {
        return;
    }

    document
        .lines
        .push(line_with_style("nav:".to_string(), SpanStyle::Chrome));
    for (action_id, label, url) in nav_links {
        let link_id = u32::try_from(*next_link_id).unwrap_or(u32::MAX);
        *next_link_id += 1;
        let value = format!("[{link_id}] {label} <{url}>");
        push_wrapped_lines(document, &value, 0, width, Some((link_id, action_id)));
    }
    document.lines.push(RenderedLine::default());
}

fn push_outline_lines(
    page: &SemanticPage,
    next_link_id: &mut usize,
    next_control_id: &mut usize,
    width: usize,
    document: &mut RenderedDocument,
) {
    document
        .lines
        .push(line_with_style("outline:".to_string(), SpanStyle::Chrome));
    for block in &page.blocks {
        match block {
            SemanticBlock::Heading { level, text, .. } => document.lines.push(line_with_style(
                format!(
                    "{}{} {text}",
                    "  ".repeat(usize::from(level.saturating_sub(1))),
                    "#"
                ),
                SpanStyle::Heading,
            )),
            SemanticBlock::Link {
                action_id,
                text,
                url,
                ..
            } => {
                let label = if text.is_empty() { url } else { text };
                let link_id = u32::try_from(*next_link_id).unwrap_or(u32::MAX);
                *next_link_id += 1;
                push_wrapped_lines(
                    document,
                    &format!("[{link_id}] link {label}"),
                    0,
                    width,
                    Some((link_id, action_id.clone())),
                );
            }
            SemanticBlock::FormControl {
                action_id,
                label,
                control_type,
                ..
            } => {
                let control_id = u32::try_from(*next_control_id).unwrap_or(u32::MAX);
                *next_control_id += 1;
                push_wrapped_control_lines(
                    document,
                    &format!("[control {control_id}] {control_type} {label}"),
                    0,
                    width,
                    control_id,
                    action_id.clone(),
                    control_action(control_type),
                );
            }
            _ => {}
        }
    }
    document.lines.push(RenderedLine::default());
}

fn push_reader_block_lines(
    block: &SemanticBlock,
    next_link_id: &mut usize,
    next_control_id: &mut usize,
    terminal_width: usize,
    options: &TerminalRenderOptions,
    document: &mut RenderedDocument,
) {
    let placement = ReaderPlacement::from_block(block, terminal_width);
    match block {
        SemanticBlock::Heading { level, text, .. } => {
            document.lines.push(RenderedLine::default());
            document.lines.push(line_with_style(
                format!(
                    "{}{} {text}",
                    " ".repeat(placement.indent),
                    "#".repeat(usize::from((*level).clamp(1, 6)))
                ),
                SpanStyle::Heading,
            ));
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
        SemanticBlock::Link {
            action_id,
            text,
            url,
            ..
        } => {
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
                Some((link_id, action_id.clone())),
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
            action_id,
            label,
            control_type,
            value,
            ..
        } => {
            let control_id = u32::try_from(*next_control_id).unwrap_or(u32::MAX);
            *next_control_id += 1;
            let text = if value.is_empty() {
                format!("[control {control_id}] {control_type} {label}")
            } else {
                format!("[control {control_id}] {control_type} {label}: {value}")
            };
            push_wrapped_control_lines(
                document,
                &text,
                placement.indent,
                placement.width,
                control_id,
                action_id.clone(),
                control_action(control_type),
            );
            document.lines.push(RenderedLine::default());
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "small render metadata values are cloned into every wrapped hitbox line"
)]
fn push_wrapped_lines(
    document: &mut RenderedDocument,
    text: &str,
    indent: usize,
    width: usize,
    link: Option<(u32, Option<ActionId>)>,
) {
    for line in wrap_lines(text, width) {
        let text = format!("{}{}", " ".repeat(indent), line);
        let links = link.clone().map_or_else(Vec::new, |(link_id, action_id)| {
            vec![LinkHitbox {
                link_id,
                action_id,
                start_col: u16::try_from(indent).unwrap_or(u16::MAX),
                end_col: u16::try_from(text.chars().count()).unwrap_or(u16::MAX),
            }]
        });
        document.lines.push(RenderedLine {
            spans: vec![StyledSpan {
                text: text.clone(),
                style: if links.is_empty() {
                    SpanStyle::Normal
                } else {
                    SpanStyle::Link
                },
            }],
            text,
            links,
            controls: Vec::new(),
        });
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "action ids are cloned into every wrapped hitbox line"
)]
fn push_wrapped_control_lines(
    document: &mut RenderedDocument,
    text: &str,
    indent: usize,
    width: usize,
    control_id: u32,
    action_id: Option<ActionId>,
    action: ControlHitboxAction,
) {
    for line in wrap_lines(text, width) {
        let text = format!("{}{}", " ".repeat(indent), line);
        document.lines.push(RenderedLine {
            spans: vec![StyledSpan {
                text: text.clone(),
                style: SpanStyle::Control,
            }],
            controls: vec![ControlHitbox {
                control_id,
                action_id: action_id.clone(),
                action,
                start_col: u16::try_from(indent).unwrap_or(u16::MAX),
                end_col: u16::try_from(text.chars().count()).unwrap_or(u16::MAX),
            }],
            text,
            links: Vec::new(),
        });
    }
}

fn control_action(control_type: &str) -> ControlHitboxAction {
    match control_type {
        "text" | "search" | "email" | "password" | "tel" | "url" | "number" | "textarea"
        | "select" => ControlHitboxAction::InputText,
        _ => ControlHitboxAction::Click,
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "small render metadata values are cloned into every emitted line"
)]
fn push_text_lines(
    document: &mut RenderedDocument,
    text: &str,
    link: Option<(u32, Option<ActionId>)>,
) {
    for line in text.lines() {
        let end_col = u16::try_from(line.chars().count()).unwrap_or(u16::MAX);
        let links = link.clone().map_or_else(Vec::new, |(link_id, action_id)| {
            vec![LinkHitbox {
                link_id,
                action_id,
                start_col: 0,
                end_col,
            }]
        });
        let text = line.to_string();
        document.lines.push(RenderedLine {
            spans: vec![StyledSpan {
                text: text.clone(),
                style: if links.is_empty() {
                    SpanStyle::Normal
                } else {
                    SpanStyle::Link
                },
            }],
            text,
            links,
            controls: Vec::new(),
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

fn nav_link_target(block: &SemanticBlock) -> Option<(Option<ActionId>, String, String)> {
    let SemanticBlock::Link {
        action_id,
        text,
        url,
        region,
        ..
    } = block
    else {
        return None;
    };

    if !matches!(region, SemanticRegion::Header | SemanticRegion::Navigation) {
        return None;
    }

    Some((
        action_id.clone(),
        if text.is_empty() {
            url.clone()
        } else {
            text.clone()
        },
        url.clone(),
    ))
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
