#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Terminal rendering primitives for trowser.

use std::collections::BTreeMap;

use trowser_render::{
    NAV_SUMMARY_LINK_LIMIT, SemanticBlock, SemanticLayoutNode, SemanticPage, SemanticRegion,
    TextRun, TextRunStyle, reader_blocks, sanitize_page,
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
    /// Emulate browser layout using DOM geometry and regions.
    Browser,
    /// Show a compact headings/actions overview.
    Outline,
}

impl RenderMode {
    /// Returns the next render mode in the interactive cycle.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Reader | Self::Page => Self::Browser,
            Self::Browser => Self::Outline,
            Self::Outline => Self::Reader,
        }
    }

    /// Returns a short label for status bars.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Page => "page",
            Self::Browser => "browser",
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
    /// Inline or block code text.
    Code,
    /// Quoted text.
    Quote,
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
            let blocks = reader_blocks(&page.blocks);
            push_blocks_with_layout(
                &blocks,
                mode,
                width,
                options,
                &mut next_link_id,
                &mut next_control_id,
                &mut document,
            );
        }
        RenderMode::Page => {
            let blocks = page.blocks.iter().collect::<Vec<_>>();
            push_blocks_with_layout(
                &blocks,
                mode,
                width,
                options,
                &mut next_link_id,
                &mut next_control_id,
                &mut document,
            );
        }
        RenderMode::Browser => push_browser_layout_lines(
            &page,
            width,
            options,
            &mut next_link_id,
            &mut next_control_id,
            &mut document,
        ),
        RenderMode::Outline => push_outline_lines(
            &page,
            &mut next_link_id,
            &mut next_control_id,
            width,
            &mut document,
        ),
    }

    normalize_document_spacing(&mut document);
    document
}

fn push_header_lines(page: &SemanticPage, width: usize, document: &mut RenderedDocument) {
    let mut output = String::new();
    render_header(page, width, &mut output);
    push_text_lines(document, &output, None);
}

fn push_blocks_with_layout(
    blocks: &[&SemanticBlock],
    mode: RenderMode,
    width: usize,
    options: &TerminalRenderOptions,
    next_link_id: &mut usize,
    next_control_id: &mut usize,
    document: &mut RenderedDocument,
) {
    let mut index = 0usize;
    let mut current_region = None;
    while let Some(block) = blocks.get(index) {
        if mode == RenderMode::Page && current_region != Some(block.region()) {
            current_region = Some(block.region());
            push_region_heading(document, block.region(), width);
        }

        if block.region() == SemanticRegion::Navigation
            && mode == RenderMode::Page
            && !block_link_targets(block).is_empty()
        {
            index = push_navigation_group(blocks, index, width, next_link_id, document);
            continue;
        }

        if matches!(block, SemanticBlock::FormControl { .. }) {
            index = push_control_group(blocks, index, width, next_control_id, document);
            continue;
        }

        push_reader_block_lines(
            block,
            next_link_id,
            next_control_id,
            width,
            options,
            document,
        );
        index += 1;
    }
}

fn push_navigation_group(
    blocks: &[&SemanticBlock],
    start: usize,
    width: usize,
    next_link_id: &mut usize,
    document: &mut RenderedDocument,
) -> usize {
    let mut index = start;
    let mut targets = Vec::new();
    while let Some(block) = blocks.get(index) {
        if block.region() != SemanticRegion::Navigation {
            break;
        }
        let block_targets = block_link_targets(block);
        if block_targets.is_empty() {
            break;
        }
        targets.extend(block_targets);
        index += 1;
    }
    push_nav_chip_lines(document, targets, width, next_link_id);
    document.lines.push(RenderedLine::default());
    index
}

fn push_control_group(
    blocks: &[&SemanticBlock],
    start: usize,
    width: usize,
    next_control_id: &mut usize,
    document: &mut RenderedDocument,
) -> usize {
    push_section_heading(document, "Actions", width);
    let mut index = start;
    while let Some(SemanticBlock::FormControl {
        action_id,
        label,
        control_type,
        value,
        ..
    }) = blocks.get(index)
    {
        push_form_control_line(
            document,
            action_id.as_ref(),
            label,
            control_type,
            value,
            ReaderPlacement::full_width(width),
            next_control_id,
        );
        index += 1;
    }
    document.lines.push(RenderedLine::default());
    index
}

fn push_region_heading(document: &mut RenderedDocument, region: SemanticRegion, width: usize) {
    let Some(title) = region_title(region) else {
        return;
    };
    push_section_heading(document, title, width);
}

const fn region_title(region: SemanticRegion) -> Option<&'static str> {
    match region {
        SemanticRegion::Header => Some("Header"),
        SemanticRegion::Navigation => Some("Navigation"),
        SemanticRegion::Aside => Some("Aside"),
        SemanticRegion::Footer => Some("Footer"),
        SemanticRegion::Unknown => Some("Page"),
        SemanticRegion::Main => None,
    }
}

fn push_section_heading(document: &mut RenderedDocument, title: &str, width: usize) {
    let prefix = format!("── {title} ");
    let fill = width.saturating_sub(prefix.chars().count()).min(80);
    document.lines.push(RenderedLine::default());
    document.lines.push(line_with_style(
        format!("{prefix}{}", "─".repeat(fill)),
        SpanStyle::Chrome,
    ));
}

fn normalize_document_spacing(document: &mut RenderedDocument) {
    let mut lines = Vec::with_capacity(document.lines.len());
    let mut previous_blank = true;
    for line in document.lines.drain(..) {
        let blank = line.text.trim().is_empty();
        if blank && previous_blank {
            continue;
        }
        previous_blank = blank;
        lines.push(line);
    }
    while lines.last().is_some_and(|line| line.text.trim().is_empty()) {
        lines.pop();
    }
    document.lines = lines;
}

fn push_browser_layout_lines(
    page: &SemanticPage,
    width: usize,
    options: &TerminalRenderOptions,
    next_link_id: &mut usize,
    next_control_id: &mut usize,
    document: &mut RenderedDocument,
) {
    if !page
        .layout_tree
        .as_ref()
        .is_some_and(layout_tree_has_geometry)
    {
        let blocks = page.blocks.iter().collect::<Vec<_>>();
        push_blocks_with_layout(
            &blocks,
            RenderMode::Page,
            width,
            options,
            next_link_id,
            next_control_id,
            document,
        );
        return;
    }

    let regions = BrowserRegions::from_blocks(&page.blocks);
    push_browser_full_width_panel(
        "Top bar",
        &regions.top,
        width,
        options,
        next_link_id,
        next_control_id,
        document,
    );
    push_browser_content_panels(
        &regions.main,
        &regions.aside,
        width,
        options,
        next_link_id,
        next_control_id,
        document,
    );
    push_browser_full_width_panel(
        "Footer",
        &regions.footer,
        width,
        options,
        next_link_id,
        next_control_id,
        document,
    );
}

fn layout_tree_has_geometry(node: &SemanticLayoutNode) -> bool {
    node.has_layout() || node.children.iter().any(layout_tree_has_geometry)
}

#[derive(Debug, Default)]
struct BrowserRegions<'a> {
    top: Vec<&'a SemanticBlock>,
    main: Vec<&'a SemanticBlock>,
    aside: Vec<&'a SemanticBlock>,
    footer: Vec<&'a SemanticBlock>,
}

impl<'a> BrowserRegions<'a> {
    fn from_blocks(blocks: &'a [SemanticBlock]) -> Self {
        let mut regions = Self::default();
        for block in blocks {
            match block.region() {
                SemanticRegion::Header | SemanticRegion::Navigation => regions.top.push(block),
                SemanticRegion::Aside => regions.aside.push(block),
                SemanticRegion::Footer => regions.footer.push(block),
                SemanticRegion::Main | SemanticRegion::Unknown => regions.main.push(block),
            }
        }
        regions
    }
}

fn push_browser_full_width_panel(
    title: &str,
    blocks: &[&SemanticBlock],
    width: usize,
    options: &TerminalRenderOptions,
    next_link_id: &mut usize,
    next_control_id: &mut usize,
    document: &mut RenderedDocument,
) {
    if blocks.is_empty() {
        return;
    }
    let panel = render_browser_panel(title, blocks, width, options, next_link_id, next_control_id);
    document.lines.extend(panel.lines);
    document.lines.push(RenderedLine::default());
}

fn push_browser_content_panels(
    main_blocks: &[&SemanticBlock],
    aside_blocks: &[&SemanticBlock],
    width: usize,
    options: &TerminalRenderOptions,
    next_link_id: &mut usize,
    next_control_id: &mut usize,
    document: &mut RenderedDocument,
) {
    if main_blocks.is_empty() && aside_blocks.is_empty() {
        return;
    }
    if aside_blocks.is_empty() || width < 96 {
        push_browser_full_width_panel(
            "Main",
            main_blocks,
            width,
            options,
            next_link_id,
            next_control_id,
            document,
        );
        push_browser_full_width_panel(
            "Aside",
            aside_blocks,
            width,
            options,
            next_link_id,
            next_control_id,
            document,
        );
        return;
    }

    let aside_width = (width / 3).clamp(28, 44);
    let main_width = width.saturating_sub(aside_width + 2).max(40);
    let main = render_browser_panel(
        "Main",
        main_blocks,
        main_width,
        options,
        next_link_id,
        next_control_id,
    );
    let aside = render_browser_panel(
        "Aside",
        aside_blocks,
        aside_width,
        options,
        next_link_id,
        next_control_id,
    );
    document
        .lines
        .extend(combine_documents_side_by_side(&main, &aside, 2));
    document.lines.push(RenderedLine::default());
}

fn render_browser_panel(
    title: &str,
    blocks: &[&SemanticBlock],
    width: usize,
    options: &TerminalRenderOptions,
    next_link_id: &mut usize,
    next_control_id: &mut usize,
) -> RenderedDocument {
    let inner_width = width.saturating_sub(4).max(MIN_BLOCK_WIDTH);
    let mut content = RenderedDocument::default();
    push_blocks_with_layout(
        blocks,
        RenderMode::Reader,
        inner_width,
        options,
        next_link_id,
        next_control_id,
        &mut content,
    );
    normalize_document_spacing(&mut content);
    box_document(title, &content, width)
}

fn box_document(title: &str, content: &RenderedDocument, width: usize) -> RenderedDocument {
    let mut document = RenderedDocument::default();
    document.lines.push(line_with_style(
        box_top_line(title, width),
        SpanStyle::Chrome,
    ));
    for line in &content.lines {
        document.lines.push(box_content_line(line, width));
    }
    document.lines.push(line_with_style(
        format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
        SpanStyle::Chrome,
    ));
    document
}

fn box_top_line(title: &str, width: usize) -> String {
    let label = format!(" {title} ");
    let fill = width.saturating_sub(label.chars().count() + 2);
    format!("╭{label}{}╮", "─".repeat(fill))
}

fn box_content_line(line: &RenderedLine, width: usize) -> RenderedLine {
    let inner_width = width.saturating_sub(4);
    let clipped = truncate_for_cells(&line.text, inner_width);
    let pad = inner_width.saturating_sub(clipped.chars().count());
    let text = format!("│ {clipped}{} │", " ".repeat(pad));
    RenderedLine {
        spans: boxed_spans(line, &clipped, pad),
        links: offset_links(&line.links, 2),
        controls: offset_controls(&line.controls, 2),
        text,
    }
}

fn boxed_spans(line: &RenderedLine, clipped: &str, pad: usize) -> Vec<StyledSpan> {
    let style = line
        .spans
        .first()
        .map_or(SpanStyle::Normal, |span| span.style);
    vec![
        StyledSpan {
            text: "│ ".to_string(),
            style: SpanStyle::Chrome,
        },
        StyledSpan {
            text: clipped.to_string(),
            style,
        },
        StyledSpan {
            text: format!("{} │", " ".repeat(pad)),
            style: SpanStyle::Chrome,
        },
    ]
}

fn combine_documents_side_by_side(
    left: &RenderedDocument,
    right: &RenderedDocument,
    gap: usize,
) -> Vec<RenderedLine> {
    let left_width = document_width(left);
    let rows = left.lines.len().max(right.lines.len());
    (0..rows)
        .map(|row| combine_document_row(left, right, row, left_width, gap))
        .collect()
}

fn combine_document_row(
    left: &RenderedDocument,
    right: &RenderedDocument,
    row: usize,
    left_width: usize,
    gap: usize,
) -> RenderedLine {
    let left_line = left.lines.get(row).cloned().unwrap_or_default();
    let right_line = right.lines.get(row).cloned().unwrap_or_default();
    let filler = left_width.saturating_sub(left_line.text.chars().count()) + gap;
    let text = format!(
        "{}{}{}",
        left_line.text,
        " ".repeat(filler),
        right_line.text
    );
    RenderedLine {
        spans: combined_spans(&left_line, &right_line, filler),
        links: combine_links(&left_line, &right_line, left_width + gap),
        controls: combine_controls(&left_line, &right_line, left_width + gap),
        text,
    }
}

fn combined_spans(left: &RenderedLine, right: &RenderedLine, filler: usize) -> Vec<StyledSpan> {
    let mut spans = if left.spans.is_empty() {
        vec![StyledSpan {
            text: left.text.clone(),
            style: SpanStyle::Normal,
        }]
    } else {
        left.spans.clone()
    };
    spans.push(StyledSpan {
        text: " ".repeat(filler),
        style: SpanStyle::Normal,
    });
    spans.extend(if right.spans.is_empty() {
        vec![StyledSpan {
            text: right.text.clone(),
            style: SpanStyle::Normal,
        }]
    } else {
        right.spans.clone()
    });
    spans
}

fn combine_links(left: &RenderedLine, right: &RenderedLine, offset: usize) -> Vec<LinkHitbox> {
    left.links
        .iter()
        .cloned()
        .chain(offset_links(&right.links, offset))
        .collect()
}

fn combine_controls(
    left: &RenderedLine,
    right: &RenderedLine,
    offset: usize,
) -> Vec<ControlHitbox> {
    left.controls
        .iter()
        .cloned()
        .chain(offset_controls(&right.controls, offset))
        .collect()
}

fn offset_links(links: &[LinkHitbox], offset: usize) -> Vec<LinkHitbox> {
    links
        .iter()
        .map(|link| LinkHitbox {
            link_id: link.link_id,
            action_id: link.action_id.clone(),
            start_col: offset_col(link.start_col, offset),
            end_col: offset_col(link.end_col, offset),
        })
        .collect()
}

fn offset_controls(controls: &[ControlHitbox], offset: usize) -> Vec<ControlHitbox> {
    controls
        .iter()
        .map(|control| ControlHitbox {
            control_id: control.control_id,
            action_id: control.action_id.clone(),
            action: control.action,
            start_col: offset_col(control.start_col, offset),
            end_col: offset_col(control.end_col, offset),
        })
        .collect()
}

fn offset_col(column: u16, offset: usize) -> u16 {
    column.saturating_add(u16::try_from(offset).unwrap_or(u16::MAX))
}

fn document_width(document: &RenderedDocument) -> usize {
    document
        .lines
        .iter()
        .map(|line| line.text.chars().count())
        .max()
        .unwrap_or_default()
}

fn truncate_for_cells(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
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
        .filter(|block| {
            matches!(
                block.region(),
                SemanticRegion::Header | SemanticRegion::Navigation
            )
        })
        .flat_map(block_link_targets)
        .take(NAV_SUMMARY_LINK_LIMIT)
        .collect::<Vec<_>>();

    if nav_links.is_empty() {
        return;
    }

    push_nav_chip_lines(document, nav_links, width, next_link_id);
    document.lines.push(RenderedLine::default());
}

fn push_nav_chip_lines(
    document: &mut RenderedDocument,
    nav_links: Vec<(Option<ActionId>, String, String)>,
    width: usize,
    next_link_id: &mut usize,
) {
    let max_width = width.max(MIN_BLOCK_WIDTH);
    let mut current = inline_cells("nav: ", SpanStyle::Chrome, None);
    for (action_id, label, url) in nav_links {
        let link_id = u32::try_from(*next_link_id).unwrap_or(u32::MAX);
        *next_link_id += 1;
        let label = if label.is_empty() {
            url.as_str()
        } else {
            label.as_str()
        };
        let chip = format!("[{link_id}] {label}");
        let mut cells = inline_cells(&chip, SpanStyle::Link, Some(&(link_id, action_id)));
        cells.extend(inline_cells("  ", SpanStyle::Normal, None));
        if current.len() > 5 && current.len().saturating_add(cells.len()) > max_width {
            push_inline_cells_line(document, &current);
            current = inline_cells("nav: ", SpanStyle::Chrome, None);
        }
        current.extend(cells);
    }
    if !current.is_empty() {
        push_inline_cells_line(document, &current);
    }
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
            push_heading_lines(document, *level, text, placement);
        }
        SemanticBlock::Paragraph { text, .. } => {
            push_wrapped_lines(document, text, placement.indent, placement.width, None);
            document.lines.push(RenderedLine::default());
        }
        SemanticBlock::RichParagraph { runs, .. } => {
            push_rich_text_lines(
                document,
                runs,
                "",
                placement.indent,
                placement.width,
                next_link_id,
                SpanStyle::Normal,
            );
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
        SemanticBlock::RichListItem { runs, .. } => {
            push_rich_text_lines(
                document,
                runs,
                "• ",
                placement.indent,
                placement.width,
                next_link_id,
                SpanStyle::Normal,
            );
        }
        SemanticBlock::Quote { runs, .. } => {
            push_rich_text_lines(
                document,
                runs,
                "│ ",
                placement.indent,
                placement.width,
                next_link_id,
                SpanStyle::Quote,
            );
            document.lines.push(RenderedLine::default());
        }
        SemanticBlock::CodeBlock { text, language, .. } => {
            push_code_block_lines(document, text, language.as_deref(), placement);
            document.lines.push(RenderedLine::default());
        }
        SemanticBlock::Table { headers, rows, .. } => {
            push_table_lines(document, headers, rows, placement.width, placement.indent);
            document.lines.push(RenderedLine::default());
        }
        SemanticBlock::HorizontalRule { .. } => push_horizontal_rule_lines(document, placement),
        SemanticBlock::Link {
            action_id,
            text,
            url,
            ..
        } => push_link_block_lines(
            document,
            action_id.as_ref(),
            text,
            url,
            placement,
            next_link_id,
        ),
        SemanticBlock::Image {
            alt,
            url,
            width,
            height,
            ..
        } => push_image_block_lines(document, alt, url, *width, *height, placement, options),
        SemanticBlock::FormControl {
            action_id,
            label,
            control_type,
            value,
            ..
        } => push_form_control_block_lines(
            document,
            action_id.as_ref(),
            label,
            control_type,
            value,
            placement,
            next_control_id,
        ),
    }
}

fn push_heading_lines(
    document: &mut RenderedDocument,
    level: u8,
    text: &str,
    placement: ReaderPlacement,
) {
    document.lines.push(RenderedLine::default());
    document.lines.push(line_with_style(
        format!(
            "{}{} {text}",
            " ".repeat(placement.indent),
            "#".repeat(usize::from(level.clamp(1, 6)))
        ),
        SpanStyle::Heading,
    ));
    document.lines.push(RenderedLine::default());
}

fn push_horizontal_rule_lines(document: &mut RenderedDocument, placement: ReaderPlacement) {
    document.lines.push(line_with_style(
        format!(
            "{}{}",
            " ".repeat(placement.indent),
            "─".repeat(placement.width.min(80))
        ),
        SpanStyle::Chrome,
    ));
    document.lines.push(RenderedLine::default());
}

fn push_link_block_lines(
    document: &mut RenderedDocument,
    action_id: Option<&ActionId>,
    text: &str,
    url: &str,
    placement: ReaderPlacement,
    next_link_id: &mut usize,
) {
    let label = if text.is_empty() { url } else { text };
    let link_id = u32::try_from(*next_link_id).unwrap_or(u32::MAX);
    *next_link_id += 1;
    let value = format!("[{link_id}] {label} <{url}>");
    push_wrapped_lines(
        document,
        &value,
        placement.indent,
        placement.width,
        Some((link_id, action_id.cloned())),
    );
    document.lines.push(RenderedLine::default());
}

fn push_image_block_lines(
    document: &mut RenderedDocument,
    alt: &str,
    url: &str,
    width: Option<u32>,
    height: Option<u32>,
    placement: ReaderPlacement,
    options: &TerminalRenderOptions,
) {
    let mut output = String::new();
    render_image_box(alt, url, width, height, placement, options, &mut output);
    push_text_lines(document, &output, None);
}

fn push_form_control_block_lines(
    document: &mut RenderedDocument,
    action_id: Option<&ActionId>,
    label: &str,
    control_type: &str,
    value: &str,
    placement: ReaderPlacement,
    next_control_id: &mut usize,
) {
    push_form_control_line(
        document,
        action_id,
        label,
        control_type,
        value,
        placement,
        next_control_id,
    );
    document.lines.push(RenderedLine::default());
}

fn push_form_control_line(
    document: &mut RenderedDocument,
    action_id: Option<&ActionId>,
    label: &str,
    control_type: &str,
    value: &str,
    placement: ReaderPlacement,
    next_control_id: &mut usize,
) {
    let control_id = u32::try_from(*next_control_id).unwrap_or(u32::MAX);
    *next_control_id += 1;
    let text = format_control_text(control_id, label, control_type, value);
    push_wrapped_control_lines(
        document,
        &text,
        placement.indent,
        placement.width,
        control_id,
        action_id.cloned(),
        control_action(control_type),
    );
}

fn format_control_text(control_id: u32, label: &str, control_type: &str, value: &str) -> String {
    let label = if label.is_empty() { "unnamed" } else { label };
    if value.is_empty() {
        format!("[control {control_id}] {control_type:<8} {label}")
    } else {
        format!("[control {control_id}] {control_type:<8} {label}: {value}")
    }
}

#[derive(Debug, Clone)]
struct InlineCell {
    ch: char,
    style: SpanStyle,
    link: Option<(u32, Option<ActionId>)>,
}

fn push_rich_text_lines(
    document: &mut RenderedDocument,
    runs: &[TextRun],
    prefix: &str,
    indent: usize,
    width: usize,
    next_link_id: &mut usize,
    default_style: SpanStyle,
) {
    let max_width = indent.saturating_add(width).max(MIN_BLOCK_WIDTH);
    let prefix_cells = inline_cells(&" ".repeat(indent), SpanStyle::Normal, None)
        .into_iter()
        .chain(inline_cells(prefix, default_style, None))
        .collect::<Vec<_>>();
    let mut current = prefix_cells.clone();
    let mut needs_space = false;

    for run in rich_display_runs(runs, next_link_id, default_style) {
        for word in run.text.split_whitespace() {
            let mut word_cells = inline_cells(word, run.style, run.link.as_ref());
            if word_cells.is_empty() {
                continue;
            }
            let add_space = needs_space && current.len() > prefix_cells.len();
            let needed = word_cells.len() + usize::from(add_space);
            if current.len() > prefix_cells.len()
                && current.len().saturating_add(needed) > max_width
            {
                push_inline_cells_line(document, &current);
                current.clone_from(&prefix_cells);
            }
            if add_space && current.len() > prefix_cells.len() {
                current.push(InlineCell {
                    ch: ' ',
                    style: default_style,
                    link: None,
                });
            }
            while current.len().saturating_add(word_cells.len()) > max_width
                && current.len() == prefix_cells.len()
                && !word_cells.is_empty()
            {
                let take = max_width.saturating_sub(current.len()).max(1);
                let rest = word_cells.split_off(take.min(word_cells.len()));
                current.extend(word_cells);
                push_inline_cells_line(document, &current);
                current.clone_from(&prefix_cells);
                word_cells = rest;
            }
            current.extend(word_cells);
            needs_space = true;
        }
    }

    if current.len() > prefix_cells.len() || !prefix.is_empty() {
        push_inline_cells_line(document, &current);
    }
}

#[derive(Debug, Clone)]
struct RichDisplayRun {
    text: String,
    style: SpanStyle,
    link: Option<(u32, Option<ActionId>)>,
}

fn rich_display_runs(
    runs: &[TextRun],
    next_link_id: &mut usize,
    default_style: SpanStyle,
) -> Vec<RichDisplayRun> {
    runs.iter()
        .filter_map(|run| {
            let text = run.text.trim();
            if text.is_empty() {
                return None;
            }
            if run.is_link() {
                let link_id = u32::try_from(*next_link_id).unwrap_or(u32::MAX);
                *next_link_id += 1;
                Some(RichDisplayRun {
                    text: format!("[{link_id}] {text}"),
                    style: SpanStyle::Link,
                    link: Some((link_id, run.action_id.clone())),
                })
            } else {
                Some(RichDisplayRun {
                    text: text.to_string(),
                    style: text_run_span_style(run.style, default_style),
                    link: None,
                })
            }
        })
        .collect()
}

const fn text_run_span_style(style: TextRunStyle, default_style: SpanStyle) -> SpanStyle {
    match style {
        TextRunStyle::Normal => default_style,
        TextRunStyle::Strong | TextRunStyle::Emphasis => SpanStyle::Heading,
        TextRunStyle::Code => SpanStyle::Code,
    }
}

fn inline_cells(
    text: &str,
    style: SpanStyle,
    link: Option<&(u32, Option<ActionId>)>,
) -> Vec<InlineCell> {
    text.chars()
        .map(|ch| InlineCell {
            ch,
            style,
            link: link.cloned(),
        })
        .collect()
}

fn push_inline_cells_line(document: &mut RenderedDocument, cells: &[InlineCell]) {
    let text = cells.iter().map(|cell| cell.ch).collect::<String>();
    let spans = inline_spans(cells);
    let links = inline_links(cells);
    document.lines.push(RenderedLine {
        text,
        spans,
        links,
        controls: Vec::new(),
    });
}

fn inline_spans(cells: &[InlineCell]) -> Vec<StyledSpan> {
    let mut spans: Vec<StyledSpan> = Vec::new();
    for cell in cells {
        if let Some(span) = spans.last_mut()
            && span.style == cell.style
        {
            span.text.push(cell.ch);
            continue;
        }
        spans.push(StyledSpan {
            text: cell.ch.to_string(),
            style: cell.style,
        });
    }
    spans
}

fn inline_links(cells: &[InlineCell]) -> Vec<LinkHitbox> {
    let mut ranges: BTreeMap<u32, (Option<ActionId>, usize, usize)> = BTreeMap::new();
    for (index, cell) in cells.iter().enumerate() {
        if let Some((link_id, action_id)) = &cell.link {
            ranges
                .entry(*link_id)
                .and_modify(|(_, _, end)| *end = index + 1)
                .or_insert_with(|| (action_id.clone(), index, index + 1));
        }
    }
    ranges
        .into_iter()
        .map(|(link_id, (action_id, start, end))| LinkHitbox {
            link_id,
            action_id,
            start_col: u16::try_from(start).unwrap_or(u16::MAX),
            end_col: u16::try_from(end).unwrap_or(u16::MAX),
        })
        .collect()
}

fn push_code_block_lines(
    document: &mut RenderedDocument,
    text: &str,
    language: Option<&str>,
    placement: ReaderPlacement,
) {
    let title = language.map_or_else(
        || "code".to_string(),
        |language| format!("code: {language}"),
    );
    document.lines.push(line_with_style(
        format!(
            "{}┌─ {title} {}",
            " ".repeat(placement.indent),
            "─".repeat(4)
        ),
        SpanStyle::Chrome,
    ));
    for raw_line in text.lines().take(160) {
        let visible = raw_line
            .chars()
            .take(placement.width.saturating_sub(2))
            .collect::<String>();
        document.lines.push(line_with_style(
            format!("{}│ {visible}", " ".repeat(placement.indent)),
            SpanStyle::Code,
        ));
    }
    document.lines.push(line_with_style(
        format!(
            "{}└{}",
            " ".repeat(placement.indent),
            "─".repeat(placement.width.min(72))
        ),
        SpanStyle::Chrome,
    ));
}

fn push_table_lines(
    document: &mut RenderedDocument,
    headers: &[String],
    rows: &[Vec<String>],
    width: usize,
    indent: usize,
) {
    let column_count = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or_default());
    if column_count == 0 {
        return;
    }
    if width < 50 || column_count > 4 {
        push_stacked_table_lines(document, headers, rows, width, indent);
        return;
    }
    let gap = 3usize;
    let column_width =
        width.saturating_sub(gap.saturating_mul(column_count.saturating_sub(1))) / column_count;
    if !headers.is_empty() {
        let line = table_row_line(headers, column_count, column_width, gap, indent);
        document
            .lines
            .push(line_with_style(line, SpanStyle::Heading));
        document.lines.push(line_with_style(
            format!("{}{}", " ".repeat(indent), "─".repeat(width.min(100))),
            SpanStyle::Chrome,
        ));
    }
    for row in rows.iter().take(80) {
        let line = table_row_line(row, column_count, column_width, gap, indent);
        document
            .lines
            .push(line_with_style(line, SpanStyle::Normal));
    }
}

fn table_row_line(
    cells: &[String],
    column_count: usize,
    column_width: usize,
    gap: usize,
    indent: usize,
) -> String {
    let mut output = " ".repeat(indent);
    for index in 0..column_count {
        if index > 0 {
            output.push_str(&" ".repeat(gap));
        }
        let cell = cells.get(index).map_or("", String::as_str);
        output.push_str(&fit_line(cell, column_width, ' '));
    }
    output
}

fn push_stacked_table_lines(
    document: &mut RenderedDocument,
    headers: &[String],
    rows: &[Vec<String>],
    width: usize,
    indent: usize,
) {
    for (row_index, row) in rows.iter().take(40).enumerate() {
        document.lines.push(line_with_style(
            format!("{}row {}", " ".repeat(indent), row_index + 1),
            SpanStyle::Chrome,
        ));
        for (index, cell) in row.iter().enumerate() {
            let label = headers
                .get(index)
                .map_or_else(|| format!("col {}", index + 1), Clone::clone);
            push_wrapped_lines(
                document,
                &format!("{label}: {cell}"),
                indent.saturating_add(2),
                width.saturating_sub(2),
                None,
            );
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

fn block_link_targets(block: &SemanticBlock) -> Vec<(Option<ActionId>, String, String)> {
    match block {
        SemanticBlock::Link {
            action_id,
            text,
            url,
            ..
        } => vec![(
            action_id.clone(),
            if text.is_empty() {
                url.clone()
            } else {
                text.clone()
            },
            url.clone(),
        )],
        SemanticBlock::RichParagraph { runs, .. }
        | SemanticBlock::RichListItem { runs, .. }
        | SemanticBlock::Quote { runs, .. } => runs
            .iter()
            .filter_map(|run| {
                let url = run.url.clone()?;
                (!url.is_empty()).then(|| {
                    (
                        run.action_id.clone(),
                        if run.text.is_empty() {
                            url.clone()
                        } else {
                            run.text.clone()
                        },
                        url,
                    )
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy)]
struct ReaderPlacement {
    indent: usize,
    width: usize,
}

impl ReaderPlacement {
    const fn full_width(terminal_width: usize) -> Self {
        Self {
            indent: 0,
            width: terminal_width,
        }
    }

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
            layout_tree: None,
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
            layout_tree: None,
        };

        let rendered = render_plain_text(&page, &TerminalRenderOptions::default());
        assert!(rendered.contains("      Indented"));
    }
}
