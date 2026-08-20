//! Trowser-owned interactive state machine for the BMUX TUI runtime.

use bmux_keyboard::{KeyCode, KeyStroke};
use bmux_tui::event::{Event, MouseEventKind};
use bmux_tui::geometry::Size;
use bmux_tui::hit::HitMap;
use bmux_tui::interaction::InteractionRouter;
use bmux_tui_runtime::{Command, Program, RuntimeEvent, Subscription, SubscriptionKey, Update};

use crate::{
    ControlHitboxAction, RenderMode, RenderedDocument, TerminalRenderOptions, render_document_mode,
};

/// CLI-owned asynchronous effect factory used by the terminal state machine.
pub type BrowserEffect = std::sync::Arc<
    dyn Fn(BrowserAction, u64, Size, RenderMode) -> Command<BrowserMessage> + Send + Sync,
>;

/// Application messages produced by asynchronous browser/daemon commands.
#[derive(Debug, Clone)]
pub enum BrowserMessage {
    /// Begin one browser action after the runtime starts admitting terminal input.
    StartAction(BrowserAction),
    /// A page and its freshly projected terminal document are ready.
    PageLoaded {
        /// Sanitized browser page.
        page: Box<trowser_render::SemanticPage>,
        /// Browser-specific terminal projection.
        document: Box<RenderedDocument>,
        /// Current daemon-owned tab summaries after the operation.
        tabs: Vec<trowser_session_models::TabSummary>,
        /// Monotonic request generation used to reject stale completions.
        generation: u64,
    },
    /// Advance the loading indicator through normal runtime invalidation.
    LoadingTick {
        /// Request generation whose loading state should advance.
        generation: u64,
    },
    /// A browser command failed.
    Failed {
        /// Human-readable failure text.
        message: String,
        /// Monotonic request generation used to reject stale completions.
        generation: u64,
    },
}

/// Product actions requested by terminal input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserAction {
    /// Open a URL entered through the local prompt.
    OpenUrl(String),
    /// Search within the current rendered document.
    Search(String),
    /// Navigate backward.
    Back,
    /// Navigate forward.
    Forward,
    /// Reload the active page.
    Reload,
    /// Activate the committed browser interaction at a stable BMUX hit id.
    Activate(String),
    /// Activate a rendered reader-mode link.
    FollowLink {
        /// One-based rendered link id.
        link_id: u32,
        /// Stable browser action id when available.
        action_id: Option<trowser_session_models::ActionId>,
    },
    /// Activate a rendered form control.
    ClickControl {
        /// One-based rendered control id.
        control_id: u32,
        /// Stable browser action id when available.
        action_id: Option<trowser_session_models::ActionId>,
    },
    /// Input text into a rendered form control.
    InputControl {
        /// One-based rendered control id.
        control_id: u32,
        /// Stable browser action id when available.
        action_id: Option<trowser_session_models::ActionId>,
        /// Submitted text.
        text: String,
    },
    /// Synchronize the terminal viewport with an absolute browser scroll offset.
    ScrollTo(u32),
    /// Switch to a daemon-owned browser tab.
    SwitchTab(trowser_session_models::TabId),
    /// Switch to the next daemon-owned browser tab.
    NextTab,
    /// Switch to the previous daemon-owned browser tab.
    PreviousTab,
    /// Close the active daemon-owned browser tab.
    CloseTab,
    /// Close every daemon-owned browser tab.
    CloseAllTabs,
}

/// Prompt currently owned by the interactive trowser UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptState {
    /// URL entry prompt.
    OpenUrl { input: String },
    /// In-document search prompt.
    Search { input: String },
    /// Text entry for one rendered form control.
    ControlInput {
        control_id: u32,
        action_id: Option<trowser_session_models::ActionId>,
        input: String,
    },
    /// Confirmation before closing every browser tab.
    ConfirmCloseAll,
}

impl PromptState {
    fn input_mut(&mut self) -> &mut String {
        match self {
            Self::OpenUrl { input } | Self::Search { input } | Self::ControlInput { input, .. } => {
                input
            }
            Self::ConfirmCloseAll => unreachable!("confirmation prompt has no editable input"),
        }
    }

    /// Return the prompt label shown in application chrome.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::OpenUrl { .. } => "Open URL: ",
            Self::Search { .. } => "Search: ",
            Self::ControlInput { .. } => "Text: ",
            Self::ConfirmCloseAll => "Close all tabs? [y/N] ",
        }
    }

    /// Return current prompt input.
    #[must_use]
    pub const fn input(&self) -> &str {
        match self {
            Self::OpenUrl { input } | Self::Search { input } | Self::ControlInput { input, .. } => {
                input.as_str()
            }
            Self::ConfirmCloseAll => "",
        }
    }
}

/// Interactive trowser state owned above BMUX's generic runtime.
pub struct BrowserProgram {
    /// Current sanitized browser page.
    pub page: trowser_render::SemanticPage,
    /// Current browser-specific terminal projection.
    pub document: RenderedDocument,
    /// First visible document line.
    pub offset: usize,
    /// Active semantic projection mode.
    pub render_mode: RenderMode,
    /// Current status text.
    pub status: String,
    /// Current in-document search query.
    pub search_query: String,
    /// Compact tab status rendered in application chrome.
    pub tab_status: String,
    /// Current daemon tab summaries.
    pub tabs: Vec<trowser_session_models::TabSummary>,
    /// Prompt state, when the application is collecting local input.
    pub prompt: Option<PromptState>,
    /// Whether an asynchronous browser operation is pending.
    pub loading: bool,
    /// Current loading animation frame.
    pub loading_frame: usize,
    /// Stable committed focus identifier.
    pub focused: Option<String>,
    interaction_router: InteractionRouter,
    /// Last known terminal size.
    pub terminal_size: Size,
    /// Product action awaiting execution by the CLI integration layer.
    pub pending_action: Option<BrowserAction>,
    effect: Option<BrowserEffect>,
    request_generation: u64,
}

impl BrowserProgram {
    /// Create interactive state for one page and rendered document.
    #[must_use]
    pub fn new(
        page: trowser_render::SemanticPage,
        document: RenderedDocument,
        terminal_size: Size,
    ) -> Self {
        Self {
            page,
            document,
            offset: 0,
            render_mode: RenderMode::default(),
            status: String::new(),
            search_query: String::new(),
            tab_status: String::new(),
            tabs: Vec::new(),
            prompt: None,
            loading: false,
            loading_frame: 0,
            focused: None,
            interaction_router: InteractionRouter::new(),
            terminal_size,
            pending_action: None,
            effect: None,
            request_generation: 0,
        }
    }

    /// Install the CLI-owned browser/daemon effect factory.
    pub fn set_effect(&mut self, effect: BrowserEffect) {
        self.effect = Some(effect);
    }

    /// Replace daemon tab state and its compact status label.
    pub fn set_tabs(&mut self, tabs: Vec<trowser_session_models::TabSummary>) {
        let active = tabs
            .iter()
            .position(|tab| tab.active)
            .map_or(0, |index| index + 1);
        self.tab_status = if tabs.is_empty() {
            "no tabs".to_string()
        } else {
            format!("tab {active}/{}", tabs.len())
        };
        self.tabs = tabs;
    }

    /// Start a replaceable browser request and return its generation.
    pub fn begin_request(&mut self, status: impl Into<String>) -> u64 {
        self.request_generation = self.request_generation.saturating_add(1);
        self.status = status.into();
        self.loading = true;
        self.loading_frame = 0;
        self.request_generation
    }

    /// Commit the exact interaction scene from one successful presentation.
    pub fn commit_interactions(&mut self, scene: HitMap) {
        self.interaction_router.commit_scene(scene, None);
        self.focused = self
            .interaction_router
            .focused()
            .map(|id| id.as_str().to_owned());
    }

    fn max_offset(&self) -> usize {
        self.document.lines.len().saturating_sub(usize::from(
            self.terminal_size.height.saturating_sub(2).max(1),
        ))
    }

    fn handle_terminal(&mut self, event: &Event) -> Update<BrowserMessage> {
        match event {
            Event::Resize(size) => {
                self.terminal_size = *size;
                self.document = render_document_mode(
                    &self.page,
                    &TerminalRenderOptions { width: size.width },
                    self.render_mode,
                );
                self.offset = page_scroll_offset(&self.page, &self.document);
                Update::reset()
            }
            Event::Key(stroke) if self.prompt.is_some() => self.handle_prompt_key(*stroke),
            Event::Key(stroke) => self.handle_unprompted_key(*stroke),
            Event::Mouse(mouse) => {
                let route = self.interaction_router.route(Event::Mouse(*mouse));
                if let Some(focus) = route.focus_changed {
                    self.focused = Some(focus.as_str().to_owned());
                }
                if matches!(
                    mouse.kind,
                    MouseEventKind::Up(bmux_tui::event::MouseButton::Left)
                ) && let Some(target) = route.target
                {
                    let target = target.as_str();
                    if let Some(tab_id) = target.strip_prefix("chrome.tabs.") {
                        return self.action(BrowserAction::SwitchTab(
                            trowser_session_models::TabId::new(tab_id),
                        ));
                    }
                    return self.activate_target(target);
                }
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        self.offset = self.offset.saturating_sub(3);
                        self.scroll_update()
                    }
                    MouseEventKind::ScrollDown => {
                        self.offset = self.offset.saturating_add(3).min(self.max_offset());
                        self.scroll_update()
                    }
                    MouseEventKind::Down(_)
                    | MouseEventKind::Up(_)
                    | MouseEventKind::Drag(_)
                    | MouseEventKind::Move
                    | MouseEventKind::ScrollLeft
                    | MouseEventKind::ScrollRight => Update::none(),
                }
            }
            Event::Paste(_) | Event::Focus(_) | Event::Tick | Event::User(_) => Update::none(),
        }
    }

    fn handle_unprompted_key(&mut self, stroke: KeyStroke) -> Update<BrowserMessage> {
        if stroke.key == KeyCode::Tab {
            return self.handle_key(stroke);
        }
        let route = self.interaction_router.route(Event::Key(stroke));
        if let Some(focus) = route.focus_changed {
            self.focused = Some(focus.as_str().to_owned());
            Update::redraw()
        } else {
            self.handle_key(stroke)
        }
    }

    fn handle_key(&mut self, stroke: KeyStroke) -> Update<BrowserMessage> {
        let KeyStroke { key, modifiers } = stroke;
        if modifiers.ctrl && key == KeyCode::Char('c') {
            return Update::exit();
        }
        match key {
            KeyCode::Char('q') => Update::exit(),
            KeyCode::Tab if modifiers.shift => self.action(BrowserAction::PreviousTab),
            KeyCode::Tab => self.action(BrowserAction::NextTab),
            KeyCode::Char('j') | KeyCode::Down => {
                self.offset = self.offset.saturating_add(1).min(self.max_offset());
                self.scroll_update()
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.offset = self.offset.saturating_sub(1);
                self.scroll_update()
            }
            KeyCode::PageDown => {
                self.offset = self
                    .offset
                    .saturating_add(usize::from(
                        self.terminal_size.height.saturating_sub(2).max(1),
                    ))
                    .min(self.max_offset());
                self.scroll_update()
            }
            KeyCode::PageUp => {
                self.offset = self.offset.saturating_sub(usize::from(
                    self.terminal_size.height.saturating_sub(2).max(1),
                ));
                self.scroll_update()
            }
            KeyCode::Char('g') if modifiers.shift => {
                self.offset = self.max_offset();
                self.scroll_update()
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.offset = 0;
                self.scroll_update()
            }
            KeyCode::End => {
                self.offset = self.max_offset();
                self.scroll_update()
            }
            KeyCode::Char('m') => {
                self.render_mode = self.render_mode.next();
                self.document = render_document_mode(
                    &self.page,
                    &TerminalRenderOptions {
                        width: self.terminal_size.width,
                    },
                    self.render_mode,
                );
                self.offset = page_scroll_offset(&self.page, &self.document);
                self.status = format!("render mode: {}", self.render_mode.label());
                Update::reset()
            }
            KeyCode::Char('o' | 't') => {
                self.prompt = Some(PromptState::OpenUrl {
                    input: String::new(),
                });
                Update::redraw()
            }
            KeyCode::Char('/') => {
                self.prompt = Some(PromptState::Search {
                    input: self.search_query.clone(),
                });
                Update::redraw()
            }
            KeyCode::Char('n') if modifiers.shift => self.repeat_search(true),
            KeyCode::Char('n') => self.repeat_search(false),
            KeyCode::Char('a') if modifiers.shift => self.focus_action(false),
            KeyCode::Char('a') => self.focus_action(true),
            KeyCode::Enter => self.activate_focused_action(),
            KeyCode::Char('x') if modifiers.shift => {
                self.prompt = Some(PromptState::ConfirmCloseAll);
                Update::redraw()
            }
            KeyCode::Char('x') => self.action(BrowserAction::CloseTab),
            KeyCode::Char('b') | KeyCode::Left => self.action(BrowserAction::Back),
            KeyCode::Char('f') if modifiers.shift => self.action(BrowserAction::Forward),
            KeyCode::Char('f') | KeyCode::Right => self.action(BrowserAction::Forward),
            KeyCode::Char('r') => self.action(BrowserAction::Reload),
            _ => Update::none(),
        }
    }

    fn focus_action(&mut self, forward: bool) -> Update<BrowserMessage> {
        let found = next_action_line(&self.document, self.offset, forward);
        if let Some((row, target)) = found {
            self.offset = row;
            self.focused = Some(target);
            self.status = if forward {
                "focused next action".to_string()
            } else {
                "focused previous action".to_string()
            };
        } else {
            self.status = "no actionable element".to_string();
        }
        Update::redraw()
    }

    fn activate_focused_action(&mut self) -> Update<BrowserMessage> {
        let target = self
            .focused
            .clone()
            .or_else(|| first_action_on_line(&self.document, self.offset));
        target.map_or_else(Update::none, |target| self.activate_target(&target))
    }

    fn activate_target(&mut self, target: &str) -> Update<BrowserMessage> {
        if let Some(link_id) = target
            .strip_prefix("browser.link.")
            .and_then(|value| value.parse::<u32>().ok())
        {
            let action_id = self
                .document
                .lines
                .iter()
                .flat_map(|line| &line.links)
                .find(|link| link.link_id == link_id)
                .and_then(|link| link.action_id.clone());
            return self.action(BrowserAction::FollowLink { link_id, action_id });
        }
        if let Some(control_id) = target
            .strip_prefix("browser.control.")
            .and_then(|value| value.parse::<u32>().ok())
        {
            let control = self
                .document
                .lines
                .iter()
                .flat_map(|line| &line.controls)
                .find(|control| control.control_id == control_id);
            let action = control.map_or(ControlHitboxAction::Click, |control| control.action);
            let action_id = control.and_then(|control| control.action_id.clone());
            return match action {
                ControlHitboxAction::Click => self.action(BrowserAction::ClickControl {
                    control_id,
                    action_id,
                }),
                ControlHitboxAction::InputText => {
                    self.prompt = Some(PromptState::ControlInput {
                        control_id,
                        action_id,
                        input: String::new(),
                    });
                    Update::redraw()
                }
            };
        }
        self.action(BrowserAction::Activate(target.to_owned()))
    }

    fn repeat_search(&mut self, reverse: bool) -> Update<BrowserMessage> {
        if self.search_query.is_empty() {
            self.status = "no search query".to_string();
            return Update::redraw();
        }
        let found = if reverse {
            find_document_line_reverse(&self.document, &self.search_query, self.offset)
        } else {
            find_document_line(
                &self.document,
                &self.search_query,
                self.offset.saturating_add(1),
            )
        };
        if let Some(found) = found {
            self.offset = found;
            self.status = format!("found: {}", self.search_query);
        } else {
            self.status = format!("not found: {}", self.search_query);
        }
        Update::redraw()
    }

    fn handle_prompt_key(&mut self, stroke: KeyStroke) -> Update<BrowserMessage> {
        if matches!(self.prompt, Some(PromptState::ConfirmCloseAll)) {
            self.prompt = None;
            return if stroke.key == KeyCode::Char('y') {
                self.action(BrowserAction::CloseAllTabs)
            } else {
                self.status = "close all canceled".to_string();
                Update::redraw()
            };
        }
        let KeyStroke { key, modifiers } = stroke;
        match key {
            KeyCode::Escape => {
                self.prompt = None;
                Update::redraw()
            }
            KeyCode::Enter => {
                let prompt = self.prompt.take();
                match prompt {
                    Some(PromptState::OpenUrl { input }) if !input.is_empty() => {
                        self.status.clone_from(&input);
                        self.action(BrowserAction::OpenUrl(input))
                    }
                    Some(PromptState::Search { input }) => {
                        self.search_query.clone_from(&input);
                        if let Some(found) = find_document_line(&self.document, &input, 0) {
                            self.offset = found;
                            self.status = format!("found: {input}");
                        } else {
                            self.status = format!("not found: {input}");
                        }
                        Update::redraw()
                    }
                    Some(PromptState::ControlInput {
                        control_id,
                        action_id,
                        input,
                    }) => self.action(BrowserAction::InputControl {
                        control_id,
                        action_id,
                        text: input,
                    }),
                    Some(PromptState::OpenUrl { .. } | PromptState::ConfirmCloseAll) | None => {
                        Update::redraw()
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.input_mut().pop();
                }
                Update::redraw()
            }
            KeyCode::Char(character) if !modifiers.ctrl && !modifiers.alt => {
                if let Some(prompt) = self.prompt.as_mut() {
                    let character = if modifiers.shift {
                        character.to_ascii_uppercase()
                    } else {
                        character
                    };
                    prompt.input_mut().push(character);
                }
                Update::redraw()
            }
            _ => Update::none(),
        }
    }

    fn scroll_update(&mut self) -> Update<BrowserMessage> {
        let y_px = self
            .document
            .lines
            .get(self.offset)
            .and_then(|line| line.source_y_px)
            .unwrap_or(0);
        self.action(BrowserAction::ScrollTo(y_px))
    }

    fn action(&mut self, action: BrowserAction) -> Update<BrowserMessage> {
        self.pending_action = Some(action.clone());
        let Some(effect) = self.effect.clone() else {
            return Update::redraw();
        };
        let generation = self.begin_request(action_status(&action));
        Update::redraw()
            .with_command(effect(
                action,
                generation,
                self.terminal_size,
                self.render_mode,
            ))
            .with_subscription(loading_subscription(generation))
    }
}

fn page_scroll_offset(page: &trowser_render::SemanticPage, document: &RenderedDocument) -> usize {
    if page.scroll_y_px == 0 {
        return 0;
    }
    document
        .lines
        .iter()
        .position(|line| {
            line.source_y_px
                .is_some_and(|source_y_px| source_y_px >= page.scroll_y_px)
        })
        .unwrap_or_else(|| document.lines.len().saturating_sub(1))
}

fn find_document_line(document: &RenderedDocument, query: &str, start: usize) -> Option<usize> {
    if query.is_empty() {
        return None;
    }
    let query = query.to_lowercase();
    document
        .lines
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, line)| line.text.to_lowercase().contains(&query))
        .map(|(index, _)| index)
}

fn loading_subscription(generation: u64) -> Subscription<BrowserMessage> {
    Subscription::new(
        SubscriptionKey::new("browser.loading"),
        move |sender| async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                if sender
                    .send(BrowserMessage::LoadingTick { generation })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        },
    )
}

fn first_action_on_line(document: &RenderedDocument, row: usize) -> Option<String> {
    let line = document.lines.get(row)?;
    line.links
        .first()
        .map(|link| format!("browser.link.{}", link.link_id))
        .or_else(|| {
            line.controls
                .first()
                .map(|control| format!("browser.control.{}", control.control_id))
        })
}

fn next_action_line(
    document: &RenderedDocument,
    start: usize,
    forward: bool,
) -> Option<(usize, String)> {
    if forward {
        document
            .lines
            .iter()
            .enumerate()
            .skip(start.saturating_add(1))
            .find_map(|(row, _)| first_action_on_line(document, row).map(|target| (row, target)))
    } else {
        document
            .lines
            .iter()
            .enumerate()
            .take(start)
            .rev()
            .find_map(|(row, _)| first_action_on_line(document, row).map(|target| (row, target)))
    }
}

fn find_document_line_reverse(
    document: &RenderedDocument,
    query: &str,
    before: usize,
) -> Option<usize> {
    if query.is_empty() {
        return None;
    }
    let query = query.to_lowercase();
    document
        .lines
        .iter()
        .enumerate()
        .take(before)
        .rev()
        .find(|(_, line)| line.text.to_lowercase().contains(&query))
        .map(|(index, _)| index)
}

const fn action_status(action: &BrowserAction) -> &'static str {
    match action {
        BrowserAction::OpenUrl(_) => "opening URL",
        BrowserAction::Search(_) => "searching",
        BrowserAction::Back => "back",
        BrowserAction::Forward => "forward",
        BrowserAction::Reload => "reloading",
        BrowserAction::Activate(_) => "activating",
        BrowserAction::FollowLink { .. } => "following link",
        BrowserAction::ClickControl { .. } => "clicking control",
        BrowserAction::InputControl { .. } => "inputting text",
        BrowserAction::ScrollTo(_) => "scrolling",
        BrowserAction::SwitchTab(_) | BrowserAction::NextTab | BrowserAction::PreviousTab => {
            "switching tab"
        }
        BrowserAction::CloseTab => "closing tab",
        BrowserAction::CloseAllTabs => "closing all tabs",
    }
}

impl Program for BrowserProgram {
    type Message = BrowserMessage;
    type Error = std::convert::Infallible;

    fn update(
        &mut self,
        event: RuntimeEvent<Self::Message>,
    ) -> Result<Update<Self::Message>, Self::Error> {
        let update = match event {
            RuntimeEvent::Terminal(event) => self.handle_terminal(&event),
            RuntimeEvent::Message(BrowserMessage::StartAction(action)) => self.action(action),
            RuntimeEvent::Message(BrowserMessage::PageLoaded {
                page,
                document,
                tabs,
                generation,
            }) if generation == self.request_generation => {
                self.page = *page;
                self.document = *document;
                self.set_tabs(tabs);
                self.offset = self.offset.min(self.max_offset());
                self.pending_action = None;
                self.status.clear();
                self.loading = false;
                Update::redraw().cancel_subscription(SubscriptionKey::new("browser.loading"))
            }
            RuntimeEvent::Message(BrowserMessage::LoadingTick { generation })
                if generation == self.request_generation && self.loading =>
            {
                self.loading_frame = self.loading_frame.wrapping_add(1);
                Update::redraw()
            }
            RuntimeEvent::Message(BrowserMessage::Failed {
                message,
                generation,
            }) if generation == self.request_generation => {
                self.pending_action = None;
                self.status = message;
                self.loading = false;
                Update::redraw().cancel_subscription(SubscriptionKey::new("browser.loading"))
            }
            RuntimeEvent::Message(_) | RuntimeEvent::Timer(_) => Update::none(),
        };
        Ok(update)
    }
}

#[cfg(test)]
mod tests {
    use bmux_keyboard::{KeyCode, KeyStroke, Modifiers};
    use bmux_tui::event::{Event, MouseButton, MouseEvent, MouseEventKind};
    use bmux_tui::geometry::{Point, Rect, Size};
    use bmux_tui::hit::HitRegion;
    use bmux_tui_runtime::{Invalidation, Lifecycle, Program, RuntimeEvent};

    use super::{BrowserMessage, BrowserProgram};
    use crate::{RenderedDocument, RenderedLine};

    fn program() -> BrowserProgram {
        BrowserProgram::new(
            trowser_render::SemanticPage::default(),
            RenderedDocument {
                lines: (0..20)
                    .map(|index| RenderedLine {
                        text: format!("line {index}"),
                        ..RenderedLine::default()
                    })
                    .collect(),
            },
            Size::new(80, 6),
        )
    }

    #[test]
    fn rejects_zero_terminal_dimensions() {
        let size = bmux_tui::geometry::Size::new(0, 0);
        assert!(size.width < 2 || size.height < 2);
    }

    #[tokio::test]
    async fn complete_state_machine_runs_through_headless_presenter() {
        let (runtime, handle) = bmux_tui_runtime::Runtime::new(
            program(),
            bmux_tui_runtime::HeadlessPresenter::default(),
            bmux_tui_runtime::RuntimeConfig::default(),
        );
        handle
            .try_send_terminal(Event::Key(KeyStroke::simple(KeyCode::Char('j'))))
            .expect("scroll event admitted");
        handle
            .try_send_terminal(Event::Key(KeyStroke::simple(KeyCode::Char('q'))))
            .expect("exit event admitted");

        let output = runtime
            .run()
            .await
            .unwrap_or_else(|_| panic!("infallible headless runtime failed"));

        assert_eq!(output.program.offset, 1);
        assert!(output.presenter.presentations() >= 1);
        assert!(output.stats.updates_completed >= 2);
    }

    #[test]
    fn handles_scroll_mode_and_exit_without_effects() {
        let mut program = program();
        let down = program
            .update(RuntimeEvent::Terminal(Event::Key(KeyStroke::simple(
                KeyCode::Char('j'),
            ))))
            .unwrap();
        assert_eq!(program.offset, 1);
        assert_eq!(down.invalidation, Invalidation::Redraw);

        program
            .update(RuntimeEvent::Terminal(Event::Key(KeyStroke::simple(
                KeyCode::Char('m'),
            ))))
            .unwrap();
        assert_ne!(program.render_mode, crate::RenderMode::default());

        let exit = program
            .update(RuntimeEvent::Terminal(Event::Key(KeyStroke::simple(
                KeyCode::Char('q'),
            ))))
            .unwrap();
        assert_eq!(exit.lifecycle, Lifecycle::Exit);
    }

    #[test]
    fn prompt_state_collects_and_commits_local_input() {
        let mut program = program();
        for key in [KeyCode::Char('o'), KeyCode::Char('h'), KeyCode::Char('i')] {
            program
                .update(RuntimeEvent::Terminal(Event::Key(KeyStroke::simple(key))))
                .unwrap();
        }
        program
            .update(RuntimeEvent::Terminal(Event::Key(KeyStroke::simple(
                KeyCode::Enter,
            ))))
            .unwrap();

        assert!(program.prompt.is_none());
        assert_eq!(program.status, "hi");
        assert_eq!(
            program.pending_action,
            Some(super::BrowserAction::OpenUrl("hi".to_string()))
        );
    }

    #[test]
    fn routes_mouse_activation_against_committed_scene() {
        let mut program = program();
        let mut scene = bmux_tui::hit::HitMap::new();
        scene.push(HitRegion::new("browser.link.7", Rect::new(1, 1, 4, 1)).focusable(true));
        program.commit_interactions(scene);
        program
            .update(RuntimeEvent::Terminal(Event::Mouse(MouseEvent::new(
                MouseEventKind::Down(MouseButton::Left),
                Point::new(2, 1),
            ))))
            .unwrap();
        program
            .update(RuntimeEvent::Terminal(Event::Mouse(MouseEvent::new(
                MouseEventKind::Up(MouseButton::Left),
                Point::new(2, 1),
            ))))
            .unwrap();

        assert_eq!(program.focused.as_deref(), Some("browser.link.7"));
        assert_eq!(
            program.pending_action,
            Some(super::BrowserAction::FollowLink {
                link_id: 7,
                action_id: None,
            })
        );
    }

    #[test]
    fn routes_committed_tab_item_to_switch_action() {
        let mut program = program();
        let mut scene = bmux_tui::hit::HitMap::new();
        scene.push(HitRegion::new("chrome.tabs.tab-2", Rect::new(1, 1, 4, 1)));
        program.commit_interactions(scene);
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            program
                .update(RuntimeEvent::Terminal(Event::Mouse(MouseEvent::new(
                    kind,
                    Point::new(2, 1),
                ))))
                .unwrap();
        }

        assert_eq!(
            program.pending_action,
            Some(super::BrowserAction::SwitchTab(
                trowser_session_models::TabId::new("tab-2")
            ))
        );
    }

    #[test]
    fn preserves_legacy_tab_close_search_and_shifted_prompt_keys() {
        let mut program = program();

        program
            .update(RuntimeEvent::Terminal(Event::Key(KeyStroke::simple(
                KeyCode::Tab,
            ))))
            .unwrap();
        assert_eq!(program.pending_action, Some(super::BrowserAction::NextTab));

        program
            .update(RuntimeEvent::Terminal(Event::Key(
                KeyStroke::with_modifiers(
                    KeyCode::Tab,
                    Modifiers {
                        shift: true,
                        ..Modifiers::NONE
                    },
                ),
            )))
            .unwrap();
        assert_eq!(
            program.pending_action,
            Some(super::BrowserAction::PreviousTab)
        );

        program
            .update(RuntimeEvent::Terminal(Event::Key(
                KeyStroke::with_modifiers(
                    KeyCode::Char('x'),
                    Modifiers {
                        shift: true,
                        ..Modifiers::NONE
                    },
                ),
            )))
            .unwrap();
        assert_eq!(program.prompt, Some(super::PromptState::ConfirmCloseAll));
        program
            .update(RuntimeEvent::Terminal(Event::Key(KeyStroke::simple(
                KeyCode::Char('y'),
            ))))
            .unwrap();
        assert_eq!(
            program.pending_action,
            Some(super::BrowserAction::CloseAllTabs)
        );

        program
            .update(RuntimeEvent::Terminal(Event::Key(KeyStroke::simple(
                KeyCode::Char('o'),
            ))))
            .unwrap();
        program
            .update(RuntimeEvent::Terminal(Event::Key(
                KeyStroke::with_modifiers(
                    KeyCode::Char('a'),
                    Modifiers {
                        shift: true,
                        ..Modifiers::NONE
                    },
                ),
            )))
            .unwrap();
        assert_eq!(
            program.prompt.as_ref().map(super::PromptState::input),
            Some("A")
        );
    }

    #[test]
    fn focused_action_navigation_and_activation_use_stable_document_targets() {
        let mut program = BrowserProgram::new(
            trowser_render::SemanticPage::default(),
            RenderedDocument {
                lines: vec![
                    RenderedLine::default(),
                    RenderedLine {
                        text: "link".to_string(),
                        links: vec![crate::LinkHitbox {
                            link_id: 4,
                            action_id: None,
                            start_col: 0,
                            end_col: 4,
                        }],
                        ..RenderedLine::default()
                    },
                ],
            },
            Size::new(80, 6),
        );

        program
            .update(RuntimeEvent::Terminal(Event::Key(KeyStroke::simple(
                KeyCode::Char('a'),
            ))))
            .unwrap();
        assert_eq!(program.offset, 1);
        assert_eq!(program.focused.as_deref(), Some("browser.link.4"));
        program
            .update(RuntimeEvent::Terminal(Event::Key(KeyStroke::simple(
                KeyCode::Enter,
            ))))
            .unwrap();
        assert_eq!(
            program.pending_action,
            Some(super::BrowserAction::FollowLink {
                link_id: 4,
                action_id: None,
            })
        );
    }

    #[test]
    fn resize_reprojects_document_and_restores_browser_scroll_anchor() {
        let page = trowser_render::SemanticPage {
            scroll_y_px: 100,
            blocks: vec![
                trowser_render::SemanticBlock::Paragraph {
                    text: "before".to_string(),
                    layout: Some(trowser_render::BlockLayout {
                        y: 50.0,
                        width: 100.0,
                        height: 20.0,
                        ..trowser_render::BlockLayout::default()
                    }),
                    region: trowser_render::SemanticRegion::Main,
                },
                trowser_render::SemanticBlock::Paragraph {
                    text: "anchor".to_string(),
                    layout: Some(trowser_render::BlockLayout {
                        y: 100.0,
                        width: 100.0,
                        height: 20.0,
                        ..trowser_render::BlockLayout::default()
                    }),
                    region: trowser_render::SemanticRegion::Main,
                },
            ],
            ..trowser_render::SemanticPage::default()
        };
        let document = crate::render_document_mode(
            &page,
            &crate::TerminalRenderOptions { width: 80 },
            crate::RenderMode::Reader,
        );
        let mut program = BrowserProgram::new(page, document, Size::new(80, 10));

        program
            .update(RuntimeEvent::Terminal(Event::Resize(Size::new(20, 10))))
            .unwrap();

        assert_eq!(program.terminal_size, Size::new(20, 10));
        assert!(
            program.document.lines[program.offset]
                .text
                .contains("anchor")
        );
    }

    #[test]
    fn stale_browser_completion_cannot_replace_newer_state() {
        let mut program = program();
        let stale = program.begin_request("first");
        let current = program.begin_request("second");
        let stale_page = trowser_render::SemanticPage {
            title: "stale".to_string(),
            ..trowser_render::SemanticPage::default()
        };
        program
            .update(RuntimeEvent::Message(BrowserMessage::PageLoaded {
                page: Box::new(stale_page),
                document: Box::new(RenderedDocument::default()),
                tabs: Vec::new(),
                generation: stale,
            }))
            .unwrap();
        assert_ne!(program.page.title, "stale");

        let current_page = trowser_render::SemanticPage {
            title: "current".to_string(),
            ..trowser_render::SemanticPage::default()
        };
        program
            .update(RuntimeEvent::Message(BrowserMessage::PageLoaded {
                page: Box::new(current_page),
                document: Box::new(RenderedDocument::default()),
                tabs: Vec::new(),
                generation: current,
            }))
            .unwrap();
        assert_eq!(program.page.title, "current");
    }
}
