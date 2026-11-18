//! BMUX frame composition for the interactive trowser application.

use std::cell::RefCell;

use bmux_tui::component::{Component, Constraints, LayoutCx};
use bmux_tui::geometry::{Insets, Rect, Size};
use bmux_tui::hit::{HitRegion, HitRole};
use bmux_tui::paint::{LocalRect, PaintCx};
use bmux_tui::style::{Color, Modifier, Style};
use bmux_tui::text::{Line, Span};
use bmux_tui::text_width::display_width;
use bmux_tui_components::dialog::{Dialog, DialogComponent};
use bmux_tui_components::key_hint_bar::{KeyHint, KeyHintBarComponent, KeyHintBarPolicy};
use bmux_tui_components::modal_frame::{ModalSizing, ModalTheme};
use bmux_tui_components::scrollbar::{Scrollbar, ScrollbarPolicy};
use bmux_tui_components::status_bar::{
    StatusBarComponent, StatusBarPolicy, StatusBarStyles, StatusSegment, StatusSeverity,
};
use bmux_tui_components::tab_bar::{TabBarComponent, TabBarPolicy, TabBarState, TabItem};
use bmux_tui_components::text_input_box::{TextInputBoxComponent, TextInputBoxPolicy};

use crate::bmux::render_document_frame;
use crate::layout::BrowserLayout;
use crate::runtime::{
    BrowserProgram, CONFIRM_DIALOG_ID, PROMPT_POLICY, PromptState, confirm_close_all_actions,
};

/// Retained component state used while rendering the interactive application.
#[derive(Default)]
pub struct BrowserApplicationRenderer {
    tabs: TabBarState,
}

impl BrowserApplicationRenderer {
    /// Create empty retained component state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Render one application frame.
    pub fn render(&mut self, program: &BrowserProgram, frame: &mut PaintCx<'_, '_>) {
        render_browser_frame(program, &mut self.tabs, frame);
    }

    /// Commit the exact interaction scene after successful presentation.
    pub fn commit(program: &mut BrowserProgram, hits: &bmux_tui::hit::HitMap) {
        program.commit_interactions(hits.clone());
    }
}

/// Paint the complete interactive trowser surface into one BMUX frame.
///
/// The application surface has a one-row header, an optional tab row, a
/// responsive browser-content viewport with a scrollbar, an optional key hint
/// row, and a one-row status/prompt surface (see [`BrowserLayout`]). Browser
/// document content and images are delegated to the semantic painter.
pub fn render_browser_frame(
    program: &BrowserProgram,
    tab_state: &mut TabBarState,
    frame: &mut PaintCx<'_, '_>,
) {
    let area = frame.clip();
    frame.fill(LocalRect::terminal(area), " ", Style::new());
    if area.is_empty() {
        return;
    }

    let layout = BrowserLayout::new(area, !program.tabs.is_empty(), program.show_help);
    let header = layout.header;

    let title = if program.page.title.is_empty() {
        program.page.url.as_str()
    } else {
        program.page.title.as_str()
    };
    frame.write_line_with_fallback_style(
        LocalRect::terminal(header),
        &Line::from_spans([
            Span::styled(" trowser ", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{title} · {:?}", program.render_mode)),
        ]),
        Style::new().fg(Color::Black).bg(Color::Cyan),
    );
    frame.push_hit(
        HitRegion::new("chrome.header", header)
            .role(HitRole::Background)
            .pointer_events(false),
    );

    if let Some(tabs_area) = layout.tabs {
        let items = program
            .tabs
            .iter()
            .map(|tab| {
                let label = if tab.title.is_empty() {
                    tab.url.as_str()
                } else {
                    tab.title.as_str()
                };
                TabItem::new(tab.id.as_str(), label).disabled(tab.sleeping)
            })
            .collect::<Vec<_>>();
        tab_state.set_selected(program.tabs.iter().position(|tab| tab.active));
        let state = RefCell::new(std::mem::take(tab_state));
        paint_component(
            &TabBarComponent::new("chrome.tabs", &items, &state)
                .policy(TabBarPolicy::interactive()),
            tabs_area,
            frame,
        );
        *tab_state = state.into_inner();
    }

    render_document_frame(
        frame,
        program.painted_document(),
        program.offset,
        layout.body,
    );
    render_scrollbar(program, layout, frame);
    if let Some(hints) = layout.hints {
        render_key_hints(program, hints, frame);
    }

    render_status(program, layout.status, frame);

    if matches!(program.prompt, Some(PromptState::ConfirmCloseAll)) {
        render_confirm_dialog(program, area, frame);
    }
}

/// Paint the scrollbar only when the document is taller than its viewport.
fn render_scrollbar(program: &BrowserProgram, layout: BrowserLayout, frame: &mut PaintCx<'_, '_>) {
    let mut state = program.scrollbar_state();
    state.viewport_len = layout.body.height;
    if layout.scrollbar.is_empty() || state.max_offset() == 0 {
        return;
    }
    state.normalize();
    Scrollbar::new()
        .policy(ScrollbarPolicy::vertical())
        .paint(layout.scrollbar, &state, frame);
}

fn render_key_hints(program: &BrowserProgram, area: Rect, frame: &mut PaintCx<'_, '_>) {
    let hints = program
        .key_hints()
        .iter()
        .map(|(key, label)| KeyHint::new(key, label))
        .collect::<Vec<_>>();
    paint_component(
        &KeyHintBarComponent::new("chrome.hints", &hints).policy(KeyHintBarPolicy::compact()),
        area,
        frame,
    );
}

fn render_confirm_dialog(program: &BrowserProgram, area: Rect, frame: &mut PaintCx<'_, '_>) {
    let body = [Line::raw("Close every open tab?")];
    let actions = confirm_close_all_actions();
    let dialog = Dialog::new(&body, &actions, ModalTheme::dark(Color::Cyan))
        .title("Close all tabs")
        .sizing(ModalSizing::new(
            Size::new(24, 6),
            Size::new(48, 8),
            Insets::all(1),
        ));
    paint_component(
        &DialogComponent::new(CONFIRM_DIALOG_ID, dialog, &program.confirm_actions),
        area,
        frame,
    );
}

fn render_status(program: &BrowserProgram, status: Rect, frame: &mut PaintCx<'_, '_>) {
    if let Some(prompt) = &program.prompt {
        frame.write_line(
            LocalRect::terminal(status),
            &Line::from_spans([Span::styled(prompt.label(), Style::new().fg(Color::Yellow))]),
        );
        let label_width = u16::try_from(display_width(prompt.label())).unwrap_or(u16::MAX);
        let input_area = Rect::new(
            status.x.saturating_add(label_width),
            status.y,
            status.width.saturating_sub(label_width),
            1,
        );
        if prompt.is_editable() {
            paint_component(
                &TextInputBoxComponent::new("chrome.status", PROMPT_POLICY, &program.prompt_input)
                    .policy(TextInputBoxPolicy::bare().focused(true)),
                input_area,
                frame,
            );
        }
    } else {
        let severity = if program.loading {
            StatusSeverity::Warning
        } else {
            StatusSeverity::Default
        };
        let mut loading_text = String::new();
        let left_text = status_text(program, &mut loading_text);
        let left = [StatusSegment::new(left_text).severity(severity)];
        let right = [StatusSegment::new(&program.tab_status).severity(StatusSeverity::Muted)];
        paint_component(
            &StatusBarComponent::new("chrome.status")
                .left(&left)
                .right(&right)
                .policy(StatusBarPolicy {
                    separator: " · ",
                    background: true,
                    truncate: true,
                })
                .styles(StatusBarStyles::default()),
            status,
            frame,
        );
    }
    if program.prompt.is_none() {
        frame.push_hit(
            HitRegion::new("chrome.status", status)
                .role(HitRole::Background)
                .pointer_events(false),
        );
    }
}

fn paint_component(component: &impl Component, area: Rect, frame: &mut PaintCx<'_, '_>) {
    let layout = component.layout(
        Constraints::tight(Size::new(area.width, area.height)),
        &mut LayoutCx::new(),
    );
    frame.with_child(
        i32::from(area.x),
        i64::from(area.y),
        LocalRect::new(0, 0, area.width, area.height),
        |cx| component.paint(&layout, cx),
    );
}

fn status_text<'a>(program: &'a BrowserProgram, loading_text: &'a mut String) -> &'a str {
    if !program.loading {
        return program.status.as_str();
    }
    let label = if program.status.is_empty() {
        "loading"
    } else {
        program.status.as_str()
    };
    let frame = program.spinner.frame(program.loading_started.elapsed());
    *loading_text = format!("{frame} {label}");
    loading_text.as_str()
}

#[cfg(test)]
mod tests {
    use bmux_tui::buffer::Buffer;
    use bmux_tui::frame::Frame;
    use bmux_tui::geometry::{Rect, Size};
    use bmux_tui::paint::PaintCx;

    use super::{BrowserApplicationRenderer, render_browser_frame};
    use crate::runtime::{BrowserProgram, PromptState};
    use crate::{RenderedDocument, RenderedLine};

    #[test]
    fn renderer_commit_publishes_only_presented_hits() {
        let mut program = BrowserProgram::new(
            trowser_render::SemanticPage::default(),
            RenderedDocument::default(),
            Size::new(20, 3),
        );
        let mut hits = bmux_tui::hit::HitMap::new();
        hits.push(bmux_tui::hit::HitRegion::new(
            "browser.link.1",
            Rect::new(0, 1, 3, 1),
        ));

        BrowserApplicationRenderer::commit(&mut program, &hits);

        assert!(program.focused.is_none());
    }

    fn render_rows(program: &BrowserProgram, size: Size) -> Vec<String> {
        let area = Rect::new(0, 0, size.width, size.height);
        let mut buffer = Buffer::empty(area);
        let mut frame = Frame::new(&mut buffer);
        let mut tab_state = bmux_tui_components::tab_bar::TabBarState::default();
        render_browser_frame(program, &mut tab_state, &mut PaintCx::new(&mut frame));
        drop(frame);
        (0..size.height)
            .map(|y| {
                (0..size.width)
                    .filter_map(|x| buffer.get(bmux_tui::geometry::Point::new(x, y)))
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn narrow_and_short_layouts_preserve_useful_content() {
        let program = BrowserProgram::new(
            trowser_render::SemanticPage {
                title: "A long example title".to_string(),
                ..trowser_render::SemanticPage::default()
            },
            RenderedDocument {
                lines: vec![RenderedLine {
                    text: "semantic body".to_string(),
                    ..RenderedLine::default()
                }],
            },
            Size::new(12, 3),
        );

        let narrow = render_rows(&program, Size::new(12, 3));
        assert!(narrow[0].contains("trowser"));
        // One column is reserved for the scrollbar.
        assert!(narrow[1].starts_with("semantic bo"));

        let short = render_rows(&program, Size::new(20, 2));
        assert!(short[0].contains("trowser"));
        assert!(short[1].contains("semantic body") || short[1].trim().is_empty());
    }

    #[test]
    fn composes_header_body_status_and_prompt_hits() {
        let mut program = BrowserProgram::new(
            trowser_render::SemanticPage {
                title: "Example".to_string(),
                ..trowser_render::SemanticPage::default()
            },
            RenderedDocument {
                lines: vec![RenderedLine {
                    text: "body".to_string(),
                    ..RenderedLine::default()
                }],
            },
            Size::new(40, 5),
        );
        program.prompt = Some(PromptState::Search);
        *program.prompt_input.get_mut() = bmux_tui_components::text_input::TextInputState::new(
            bmux_text_edit::TextEditBuffer::from_text("term"),
        );
        let area = Rect::new(0, 0, 40, 5);
        let mut buffer = Buffer::empty(area);
        let mut frame = Frame::new(&mut buffer);
        let mut tab_state = bmux_tui_components::tab_bar::TabBarState::default();

        render_browser_frame(&program, &mut tab_state, &mut PaintCx::new(&mut frame));

        assert_eq!(frame.hits().regions()[0].id.as_str(), "chrome.header");
        assert_eq!(frame.hits().regions()[1].id.as_str(), "chrome.status.input");
        assert!(frame.hits().regions()[1].focusable);
        drop(frame);
        let rows = (0..5)
            .map(|y| {
                (0..40)
                    .filter_map(|x| buffer.get(bmux_tui::geometry::Point::new(x, y)))
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(rows[0].contains("trowser"));
        assert!(rows[1].starts_with("body"));
        assert!(rows[4].starts_with("Search: term"));
    }

    fn long_program(size: Size) -> BrowserProgram {
        BrowserProgram::new(
            trowser_render::SemanticPage::default(),
            RenderedDocument {
                lines: (0..40)
                    .map(|index| RenderedLine {
                        text: format!("line {index}"),
                        ..RenderedLine::default()
                    })
                    .collect(),
            },
            size,
        )
    }

    #[test]
    fn draws_scrollbar_only_for_scrollable_documents() {
        let size = Size::new(30, 10);
        let rows = render_rows(&long_program(size), size);
        assert!(rows[1].ends_with('█'), "thumb at the top: {:?}", rows[1]);

        let short = BrowserProgram::new(
            trowser_render::SemanticPage::default(),
            RenderedDocument {
                lines: vec![RenderedLine {
                    text: "short".to_string(),
                    ..RenderedLine::default()
                }],
            },
            size,
        );
        let rows = render_rows(&short, size);
        assert!(rows.iter().all(|row| !row.contains('█')));
    }

    #[test]
    fn help_toggle_shows_key_hints_from_the_keymap() {
        let size = Size::new(60, 10);
        let mut program = long_program(size);
        program.show_help = true;

        let rows = render_rows(&program, size);

        assert!(rows[8].contains("open"), "hint row: {:?}", rows[8]);
        assert!(rows[8].contains('?'));
    }

    #[test]
    fn close_all_confirmation_renders_as_dialog_with_button_hits() {
        let size = Size::new(60, 16);
        let mut program = long_program(size);
        program.prompt = Some(PromptState::ConfirmCloseAll);
        let area = Rect::new(0, 0, size.width, size.height);
        let mut buffer = Buffer::empty(area);
        let mut frame = Frame::new(&mut buffer);
        let mut tab_state = bmux_tui_components::tab_bar::TabBarState::default();

        render_browser_frame(&program, &mut tab_state, &mut PaintCx::new(&mut frame));

        let ids = frame
            .hits()
            .regions()
            .iter()
            .map(|region| region.id.as_str().to_owned())
            .collect::<Vec<_>>();
        assert!(
            ids.iter()
                .any(|id| id == "chrome.confirm.actions.close-all")
        );
        assert!(ids.iter().any(|id| id == "chrome.confirm.actions.cancel"));
        drop(frame);
        let text = (0..size.height)
            .flat_map(|y| (0..size.width).map(move |x| (x, y)).collect::<Vec<_>>())
            .filter_map(|(x, y)| buffer.get(bmux_tui::geometry::Point::new(x, y)))
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert!(text.contains("Close every open tab?"));
    }
}
