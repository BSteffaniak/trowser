//! BMUX frame adapter for trowser's browser-specific rendered document.

use std::collections::BTreeMap;

use super::{MAX_IMAGE_CELL_HEIGHT, RenderedDocument, SpanStyle, image_contribution};
use bmux_tui::geometry::Rect;
use bmux_tui::hit::{HitId, HitRegion, HitRole};
use bmux_tui::image::ImageKey;
use bmux_tui::paint::{LocalRect, PaintCx};
use bmux_tui::style::{Color, Modifier, Style};
use bmux_tui::text::{Line, Span};

/// A rendered document with its BMUX lines and hit identities prepared once.
///
/// Building this when the document changes keeps per-frame painting to
/// borrowing prepared lines instead of re-styling spans and formatting hit
/// ids for every visible line on every frame.
#[derive(Debug, Clone, Default)]
pub struct PaintedDocument {
    document: RenderedDocument,
    lines: Vec<PaintedLine>,
}

#[derive(Debug, Clone)]
struct PaintedLine {
    line: Line,
    hits: Vec<PaintedHit>,
}

#[derive(Debug, Clone)]
struct PaintedHit {
    id: HitId,
    start_col: u16,
    end_col: u16,
    tab_order: i32,
    role: Option<HitRole>,
}

impl PaintedDocument {
    /// Prepare `document` for repeated painting.
    #[must_use]
    pub fn new(document: RenderedDocument) -> Self {
        let lines = document.lines.iter().map(paint_line).collect();
        Self { document, lines }
    }

    /// The underlying rendered document.
    #[must_use]
    pub const fn document(&self) -> &RenderedDocument {
        &self.document
    }
}

impl From<RenderedDocument> for PaintedDocument {
    fn from(document: RenderedDocument) -> Self {
        Self::new(document)
    }
}

fn paint_line(line: &super::RenderedLine) -> PaintedLine {
    let spans = if line.spans.is_empty() {
        vec![Span::raw(line.text.clone())]
    } else {
        line.spans
            .iter()
            .map(|span| Span::styled(span.text.clone(), span_style(span.style)))
            .collect()
    };
    let links = line.links.iter().map(|link| PaintedHit {
        id: HitId::new(format!("browser.link.{}", link.link_id)),
        start_col: link.start_col,
        end_col: link.end_col,
        tab_order: i32::try_from(link.link_id).unwrap_or(i32::MAX),
        role: None,
    });
    let controls = line.controls.iter().map(|control| PaintedHit {
        id: HitId::new(format!("browser.control.{}", control.control_id)),
        start_col: control.start_col,
        end_col: control.end_col,
        tab_order: i32::try_from(control.control_id).unwrap_or(i32::MAX),
        role: Some(HitRole::TextInput),
    });
    PaintedLine {
        line: Line::from_spans(spans),
        hits: links.chain(controls).collect(),
    }
}

/// Paint a prepared browser document into a BMUX frame viewport.
///
/// `offset` is the first document line visible in `viewport`. Text, style,
/// hit regions, and protocol-neutral image placement are clipped to that
/// viewport.
pub fn render_document_frame(
    frame: &mut PaintCx<'_, '_>,
    painted: &PaintedDocument,
    offset: usize,
    viewport: Rect,
) {
    frame.fill(LocalRect::terminal(viewport), " ", Style::new());
    for (visible_row, line) in painted
        .lines
        .iter()
        .skip(offset)
        .take(usize::from(viewport.height))
        .enumerate()
    {
        let row = viewport
            .y
            .saturating_add(u16::try_from(visible_row).unwrap_or(u16::MAX));
        frame.write_line(
            LocalRect::terminal(Rect::new(viewport.x, row, viewport.width, 1)),
            &line.line,
        );
        for hit in &line.hits {
            let mut region = HitRegion::new(
                hit.id.clone(),
                clipped_hit_area(viewport, row, hit.start_col, hit.end_col),
            )
            .focusable(true)
            .tab_order(hit.tab_order);
            if let Some(role) = hit.role {
                region = region.role(role);
            }
            frame.push_hit(region);
        }
    }
    paint_document_images(frame, &painted.document, offset, viewport);
}

/// Contribute each image that is at least partly visible exactly once.
///
/// Images are attached to their top line only, so lines up to
/// [`MAX_IMAGE_CELL_HEIGHT`] above the viewport are scanned for images that
/// extend into it. Identical content appearing more than once in a frame gets
/// a distinct key per occurrence, since BMUX treats a reused key as one image.
fn paint_document_images(
    frame: &mut PaintCx<'_, '_>,
    document: &RenderedDocument,
    offset: usize,
    viewport: Rect,
) {
    let first = offset.saturating_sub(usize::from(MAX_IMAGE_CELL_HEIGHT));
    let end = offset.saturating_add(usize::from(viewport.height));
    let mut occurrences = BTreeMap::<&ImageKey, usize>::new();
    for (index, line) in document
        .lines
        .iter()
        .enumerate()
        .take(end)
        .skip(first)
        .filter(|(_, line)| !line.images.is_empty())
    {
        let relative = i64::try_from(index)
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(offset).unwrap_or(i64::MAX));
        let anchor_row = i64::from(viewport.y).saturating_add(relative);
        for image in &line.images {
            let seen = occurrences.entry(&image.key).or_default();
            let key = if *seen == 0 {
                image.key.clone()
            } else {
                ImageKey::new(format!("{}#{seen}", image.key.as_str()))
            };
            *seen += 1;
            if let Some(contribution) = image_contribution(image, key, anchor_row, viewport) {
                frame.push_image(contribution);
            }
        }
    }
}

fn clipped_hit_area(viewport: Rect, row: u16, start_col: u16, end_col: u16) -> Rect {
    let start = viewport.x.saturating_add(start_col).min(viewport.right());
    let end = viewport.x.saturating_add(end_col).min(viewport.right());
    Rect::new(start, row, end.saturating_sub(start), 1)
}

const fn span_style(style: SpanStyle) -> Style {
    match style {
        SpanStyle::Normal => Style::new(),
        SpanStyle::Chrome | SpanStyle::Dim => Style::new().fg(Color::BrightBlack),
        SpanStyle::Heading => Style::new().add_modifier(Modifier::BOLD),
        SpanStyle::Link | SpanStyle::Control => Style::new()
            .fg(Color::Cyan)
            .add_modifier(Modifier::UNDERLINE),
        SpanStyle::Code => Style::new().fg(Color::Yellow),
        SpanStyle::Quote => Style::new().fg(Color::Green),
        SpanStyle::Error => Style::new().fg(Color::Red),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bmux_tui::buffer::Buffer;
    use bmux_tui::frame::Frame;
    use bmux_tui::geometry::Rect;
    use bmux_tui::image::{ImageContribution, ImageFit, ImageKey, ImagePayload, ImagePlacement};
    use bmux_tui::paint::PaintCx;

    use super::{PaintedDocument, render_document_frame};
    use crate::{
        ControlHitbox, ControlHitboxAction, LinkHitbox, RenderedDocument, RenderedImage,
        RenderedLine, SpanStyle, StyledSpan,
    };

    #[test]
    fn paints_visible_lines_and_protocol_neutral_images() {
        let document = RenderedDocument {
            lines: vec![
                RenderedLine {
                    text: "hidden".to_string(),
                    ..RenderedLine::default()
                },
                RenderedLine {
                    text: "visible".to_string(),
                    spans: vec![StyledSpan {
                        text: "visible".to_string(),
                        style: SpanStyle::Heading,
                    }],
                    links: vec![LinkHitbox {
                        link_id: 7,
                        action_id: None,
                        start_col: 0,
                        end_col: 3,
                    }],
                    controls: vec![ControlHitbox {
                        control_id: 9,
                        action_id: None,
                        action: ControlHitboxAction::Click,
                        start_col: 4,
                        end_col: 20,
                    }],
                    images: vec![test_image("preview:test", 1)],
                    ..RenderedLine::default()
                },
            ],
        };
        let viewport = Rect::new(0, 0, 20, 1);
        let mut buffer = Buffer::empty(viewport);
        let mut frame = Frame::new(&mut buffer);

        render_document_frame(
            &mut PaintCx::new(&mut frame),
            &PaintedDocument::new(document),
            1,
            viewport,
        );

        assert_eq!(frame.images().len(), 1);
        assert_eq!(frame.hits().regions().len(), 2);
        assert_eq!(frame.hits().regions()[0].id.as_str(), "browser.link.7");
        assert_eq!(frame.hits().regions()[1].id.as_str(), "browser.control.9");
        assert_eq!(frame.hits().regions()[1].area.width, 16);
        drop(frame);
        assert_eq!(
            (0..7)
                .filter_map(|x| buffer.get(bmux_tui::geometry::Point::new(x, 0)))
                .map(|cell| cell.symbol.as_str())
                .collect::<String>(),
            "visible"
        );
    }

    fn test_image(key: &str, cell_height: u16) -> RenderedImage {
        RenderedImage {
            key: ImageKey::new(key),
            payload: ImagePayload::Png {
                bytes: Arc::from([1_u8, 2, 3].as_slice()),
                width: 4,
                height: 2,
            },
            cell_width: 2,
            cell_height,
            start_col: 3,
        }
    }

    fn image_document(images_at: &[(usize, RenderedImage)], len: usize) -> RenderedDocument {
        let mut document = RenderedDocument {
            lines: vec![RenderedLine::default(); len],
        };
        for (line, image) in images_at {
            document.lines[*line].images.push(image.clone());
        }
        document
    }

    fn paint_images(
        document: &RenderedDocument,
        offset: usize,
        viewport: Rect,
    ) -> Vec<ImagePlacement> {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 12));
        let mut frame = Frame::new(&mut buffer);
        render_document_frame(
            &mut PaintCx::new(&mut frame),
            &PaintedDocument::new(document.clone()),
            offset,
            viewport,
        );
        frame
            .images()
            .iter()
            .map(|contribution| match contribution {
                ImageContribution::Present(placement) => placement.clone(),
                ImageContribution::Remove(key) => panic!("unexpected removal of {key:?}"),
            })
            .collect()
    }

    #[test]
    fn crops_image_whose_top_scrolled_above_viewport() {
        let document = image_document(&[(2, test_image("preview:tall", 5))], 10);
        let viewport = Rect::new(0, 1, 20, 6);

        // Lines 0..4 are scrolled away; the image covers lines 2..7.
        let placements = paint_images(&document, 4, viewport);

        assert_eq!(placements.len(), 1);
        let placement = &placements[0];
        let allocation = placement.allocation.expect("full allocation retained");
        assert_eq!((allocation.y, allocation.height), (-1, 5));
        assert_eq!(placement.destination, Rect::new(3, 1, 2, 3));
        assert_eq!(placement.fit, ImageFit::Contain);
        assert!(allocation.covers(placement.destination));
    }

    #[test]
    fn skips_images_entirely_outside_viewport() {
        let document = image_document(
            &[(0, test_image("above", 2)), (9, test_image("below", 2))],
            12,
        );

        assert!(paint_images(&document, 4, Rect::new(0, 0, 20, 4)).is_empty());
    }

    #[test]
    fn contributes_each_visible_image_once_with_shared_bytes() {
        let image = test_image("preview:shared", 4);
        let document = image_document(&[(1, image.clone())], 8);

        let placements = paint_images(&document, 0, Rect::new(0, 0, 20, 8));

        assert_eq!(placements.len(), 1);
        let (ImagePayload::Png { bytes: painted, .. }, ImagePayload::Png { bytes: source, .. }) =
            (&placements[0].payload, &image.payload)
        else {
            panic!("expected PNG payloads");
        };
        assert!(Arc::ptr_eq(painted, source));
    }

    #[test]
    fn distinguishes_repeated_identical_images() {
        let image = test_image("preview:dup", 1);
        let document = image_document(&[(0, image.clone()), (2, image)], 4);

        let placements = paint_images(&document, 0, Rect::new(0, 0, 20, 4));

        let keys = placements
            .iter()
            .map(|placement| placement.key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(keys, ["preview:dup", "preview:dup#1"]);
    }
}
