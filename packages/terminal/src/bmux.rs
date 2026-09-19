//! BMUX frame adapter for trowser's browser-specific rendered document.

use super::{RenderedDocument, SpanStyle, image_contribution};
use bmux_tui::geometry::Rect;
use bmux_tui::hit::{HitRegion, HitRole};
use bmux_tui::paint::{LocalRect, PaintCx};
use bmux_tui::style::{Color, Modifier, Style};
use bmux_tui::text::{Line, Span};

/// Paint a rendered browser document into a BMUX frame viewport.
///
/// `offset` is the first document line visible in `viewport`. Text, style, and
/// protocol-neutral image placement are clipped to that viewport. Browser
/// actions remain represented by trowser's document hitboxes; stable BMUX hit
/// registration is added by the interactive application layer.
pub fn render_document_frame(
    frame: &mut PaintCx<'_, '_>,
    document: &RenderedDocument,
    offset: usize,
    viewport: Rect,
) {
    frame.fill(LocalRect::terminal(viewport), " ", Style::new());
    for (visible_row, line) in document
        .lines
        .iter()
        .skip(offset)
        .take(usize::from(viewport.height))
        .enumerate()
    {
        let row = viewport
            .y
            .saturating_add(u16::try_from(visible_row).unwrap_or(u16::MAX));
        let spans = if line.spans.is_empty() {
            vec![Span::raw(line.text.clone())]
        } else {
            line.spans
                .iter()
                .map(|span| Span::styled(span.text.clone(), span_style(span.style)))
                .collect()
        };
        frame.write_line(
            LocalRect::terminal(Rect::new(viewport.x, row, viewport.width, 1)),
            &Line::from_spans(spans),
        );
        for link in &line.links {
            frame.push_hit(
                HitRegion::new(
                    format!("browser.link.{}", link.link_id),
                    clipped_hit_area(viewport, row, link.start_col, link.end_col),
                )
                .focusable(true)
                .tab_order(i32::try_from(link.link_id).unwrap_or(i32::MAX)),
            );
        }
        for control in &line.controls {
            frame.push_hit(
                HitRegion::new(
                    format!("browser.control.{}", control.control_id),
                    clipped_hit_area(viewport, row, control.start_col, control.end_col),
                )
                .role(HitRole::TextInput)
                .focusable(true)
                .tab_order(i32::try_from(control.control_id).unwrap_or(i32::MAX)),
            );
        }
        for image in &line.images {
            frame.push_image(image_contribution(image, row, viewport));
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
    use bmux_tui::buffer::Buffer;
    use bmux_tui::frame::Frame;
    use bmux_tui::geometry::Rect;
    use bmux_tui::paint::PaintCx;

    use super::render_document_frame;
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
                    images: vec![RenderedImage {
                        key: "preview:test".to_string(),
                        mime_type: "image/png".to_string(),
                        data: vec![1, 2, 3],
                        pixel_width: 1,
                        pixel_height: 1,
                        cell_width: 2,
                        cell_height: 1,
                        start_col: 3,
                        row_offset: 0,
                    }],
                    ..RenderedLine::default()
                },
            ],
        };
        let viewport = Rect::new(0, 0, 20, 1);
        let mut buffer = Buffer::empty(viewport);
        let mut frame = Frame::new(&mut buffer);

        render_document_frame(&mut PaintCx::new(&mut frame), &document, 1, viewport);

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
}
