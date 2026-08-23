//! BMUX frame composition for the interactive trowser application.

use bmux_tui::frame::Frame;
use bmux_tui::geometry::Rect;
use bmux_tui::hit::{HitRegion, HitRole};
use bmux_tui::style::{Color, Modifier, Style};
use bmux_tui::text::{Line, Span};
use bmux_tui_components::status_bar::{
    StatusBar, StatusBarPolicy, StatusBarStyles, StatusSegment, StatusSeverity,
};
use bmux_tui_components::tab_bar::{TabBar, TabBarPolicy, TabBarState, TabItem};
use bmux_tui_components::text_input::{TextInputPolicy, TextInputState};
use bmux_tui_components::text_input_box::{TextInputBox, TextInputBoxPolicy};

use crate::bmux::render_document_frame;
use crate::runtime::BrowserProgram;

/// Retained component state used while rendering the interactive application.
#[derive(Default)]
pub struct BrowserApplicationRenderer {
    prompt: TextInputState,
    tabs: TabBarState,
}

impl BrowserApplicationRenderer {
    /// Create empty retained component state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Render one application frame.
    pub fn render(&mut self, program: &BrowserProgram, frame: &mut Frame<'_>) {
        render_browser_frame(program, &mut self.prompt, &mut self.tabs, frame);
    }

    /// Commit the exact interaction scene after successful presentation.
    pub fn commit(program: &mut BrowserProgram, hits: &bmux_tui::hit::HitMap) {
        program.commit_interactions(hits.clone());
    }
}

/// Paint the complete interactive trowser surface into one BMUX frame.
///
/// The application surface has a one-row header, a responsive browser-content
/// viewport, and a one-row status/prompt surface. Browser document content and
/// images are delegated to the browser-specific semantic painter.
pub fn render_browser_frame(
    program: &BrowserProgram,
    prompt_state: &mut TextInputState,
    tab_state: &mut TabBarState,
    frame: &mut Frame<'_>,
) {
    let area = frame.area();
    frame.fill(area, " ", Style::new());
    if area.is_empty() {
        return;
    }

    let header = Rect::new(area.x, area.y, area.width, 1);
    let has_tabs = !program.tabs.is_empty() && area.height >= 4;
    let tabs_area = has_tabs.then(|| Rect::new(area.x, area.y.saturating_add(1), area.width, 1));
    let status_y = area.bottom().saturating_sub(1);
    let status = Rect::new(area.x, status_y, area.width, 1);
    let body_y = area.y.saturating_add(if has_tabs { 2 } else { 1 });
    let body = Rect::new(area.x, body_y, area.width, status_y.saturating_sub(body_y));

    let title = if program.page.title.is_empty() {
        program.page.url.as_str()
    } else {
        program.page.title.as_str()
    };
    frame.write_line_with_fallback_style(
        header,
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

    if let Some(tabs_area) = tabs_area {
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
        TabBar::new(&items)
            .policy(TabBarPolicy::interactive())
            .render_with_id("chrome.tabs", tabs_area, tab_state, frame);
    }

    render_document_frame(frame, &program.document, program.offset, body);

    if let Some(prompt) = &program.prompt {
        if prompt_state.buffer().text() != prompt.input() {
            *prompt_state =
                TextInputState::new(bmux_text_edit::TextEditBuffer::from_text(prompt.input()));
        }
        frame.write_line(
            Rect::new(status.x, status.y, status.width, 1),
            &Line::from_spans([Span::styled(prompt.label(), Style::new().fg(Color::Yellow))]),
        );
        let label_width = u16::try_from(prompt.label().chars().count()).unwrap_or(u16::MAX);
        let input_area = Rect::new(
            status.x.saturating_add(label_width),
            status.y,
            status.width.saturating_sub(label_width),
            1,
        );
        TextInputBox::new(TextInputPolicy::default())
            .policy(TextInputBoxPolicy::bare().focused(true))
            .render_with_id("chrome.status", input_area, prompt_state, frame);
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
        StatusBar::new()
            .left(&left)
            .right(&right)
            .policy(StatusBarPolicy {
                separator: " · ",
                background: true,
                truncate: true,
            })
            .styles(StatusBarStyles::default())
            .render(status, frame);
    }
    if program.prompt.is_none() {
        frame.push_hit(
            HitRegion::new("chrome.status", status)
                .role(HitRole::Background)
                .pointer_events(false),
        );
    }
}

fn status_text<'a>(program: &'a BrowserProgram, loading_text: &'a mut String) -> &'a str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    if !program.loading {
        return program.status.as_str();
    }
    let label = if program.status.is_empty() {
        "loading"
    } else {
        program.status.as_str()
    };
    *loading_text = format!("{} {label}", FRAMES[program.loading_frame % FRAMES.len()]);
    loading_text.as_str()
}

#[cfg(test)]
mod tests {
    use bmux_tui::buffer::Buffer;
    use bmux_tui::frame::Frame;
    use bmux_tui::geometry::{Rect, Size};

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
        let mut prompt_state = bmux_tui_components::text_input::TextInputState::default();
        let mut tab_state = bmux_tui_components::tab_bar::TabBarState::default();
        render_browser_frame(program, &mut prompt_state, &mut tab_state, &mut frame);
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
        assert!(narrow[1].starts_with("semantic bod"));

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
        program.prompt = Some(PromptState::Search {
            input: "term".to_string(),
        });
        let area = Rect::new(0, 0, 40, 5);
        let mut buffer = Buffer::empty(area);
        let mut frame = Frame::new(&mut buffer);
        let mut prompt_state = bmux_tui_components::text_input::TextInputState::default();
        let mut tab_state = bmux_tui_components::tab_bar::TabBarState::default();

        render_browser_frame(&program, &mut prompt_state, &mut tab_state, &mut frame);

        assert_eq!(frame.hits().regions()[0].id.as_str(), "chrome.header");
        assert_eq!(frame.hits().regions()[1].id.as_str(), "chrome.status");
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
}
