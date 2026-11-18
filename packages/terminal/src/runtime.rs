//! Trowser-owned interactive state machine for the BMUX TUI runtime.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use bmux_keyboard::{KeyCode, KeyStroke};
use bmux_text_edit::TextEditBuffer;
use bmux_tui::event::{Event, MouseEventKind};
use bmux_tui::geometry::{Rect, Size};
use bmux_tui::hit::HitMap;
use bmux_tui::interaction::InteractionRouter;
use bmux_tui_components::action_row::{
    ActionButton, ActionRow, ActionRowOutcome, ActionRowPolicy, ActionRowState,
};
use bmux_tui_components::scrollbar::{Scrollbar, ScrollbarOutcome, ScrollbarState};
use bmux_tui_components::spinner::Spinner;
use bmux_tui_components::text_input::{
    EdgePolicy, KeyboardPolicy, TextInputControl, TextInputOutcome, TextInputPolicy,
    TextInputState, ViewportPolicy,
};
use bmux_tui_runtime::{Command, Program, RuntimeEvent, Subscription, SubscriptionKey, Update};

use crate::bmux::PaintedDocument;
use crate::keymap::{KeyAction, Keymap};
use crate::layout::{BrowserLayout, document_width};
use crate::{
    ControlHitboxAction, RenderMode, RenderedDocument, TerminalRenderOptions, render_document_mode,
};

/// Hit-id prefix of the close-all confirmation dialog's buttons.
pub const CONFIRM_DIALOG_ID: &str = "chrome.confirm";
const CONFIRM_ACTIONS_PREFIX: &str = "chrome.confirm.actions.";
const CONFIRM_CANCEL: &str = "cancel";
const CONFIRM_CLOSE_ALL: &str = "close-all";

/// Buttons of the close-all confirmation dialog, in display order.
#[must_use]
pub fn confirm_close_all_actions() -> [ActionButton; 2] {
    [
        ActionButton::new(CONFIRM_CANCEL, "Cancel"),
        ActionButton::new(CONFIRM_CLOSE_ALL, "Close all"),
    ]
}

/// Single-row prompt editing: Enter submits; cursor movement, word editing,
/// paste, selection and undo use BMUX's standard readline keymap.
pub const PROMPT_POLICY: TextInputPolicy = TextInputPolicy {
    keyboard: KeyboardPolicy {
        shift_enter: None,
        ..KeyboardPolicy::chat_composer()
    },
    viewport: ViewportPolicy {
        auto_scroll_to_cursor: true,
        min_rows: 1,
        max_rows: Some(1),
    },
    edge: EdgePolicy::disabled(),
    ..TextInputPolicy::chat_composer()
};

/// Idle time after local scrolling before the browser scroll position is synced.
pub const SCROLL_SYNC_DELAY: Duration = Duration::from_millis(250);

/// CLI-owned asynchronous effect factory used by the terminal state machine.
///
/// [`BrowserAction::ScrollTo`] is a background sync: the effect should wait
/// [`SCROLL_SYNC_DELAY`] under its own replaceable command key and answer with
/// [`BrowserMessage::ScrollSynced`] rather than [`BrowserMessage::PageLoaded`].
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
    /// A debounced background scroll sync refreshed the page after local scrolling.
    ScrollSynced {
        /// Sanitized browser page at the synced scroll position.
        page: Box<trowser_render::SemanticPage>,
        /// Browser-specific terminal projection.
        document: Box<RenderedDocument>,
        /// Request generation current when the sync was issued.
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
    /// Load the daemon's active tab snapshot during application startup.
    LoadActiveSnapshot,
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
    /// Sync the browser to an absolute scroll offset after local scrolling.
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
///
/// Editable text lives in [`BrowserProgram::prompt_input`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptState {
    /// URL entry prompt.
    OpenUrl,
    /// In-document search prompt.
    Search,
    /// Text entry for one rendered form control.
    ControlInput {
        control_id: u32,
        action_id: Option<trowser_session_models::ActionId>,
    },
    /// Confirmation before closing every browser tab.
    ConfirmCloseAll,
}

impl PromptState {
    /// Return the prompt label shown in application chrome.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::OpenUrl => "Open URL: ",
            Self::Search => "Search: ",
            Self::ControlInput { .. } => "Text: ",
            Self::ConfirmCloseAll => "Close all tabs? [y/N] ",
        }
    }

    /// Whether this prompt collects editable text.
    #[must_use]
    pub const fn is_editable(&self) -> bool {
        !matches!(self, Self::ConfirmCloseAll)
    }
}

/// Interactive trowser state owned above BMUX's generic runtime.
pub struct BrowserProgram {
    /// Current sanitized browser page.
    pub page: trowser_render::SemanticPage,
    /// Current terminal projection, prepared for painting.
    document: PaintedDocument,
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
    /// Editable prompt text, cursor, selection and undo history.
    ///
    /// Shared with the prompt component during painting, hence the `RefCell`.
    pub prompt_input: RefCell<TextInputState>,
    /// Whether an asynchronous browser operation is pending.
    pub loading: bool,
    /// When the pending browser operation started, for the loading spinner.
    pub loading_started: Instant,
    /// Loading indicator animation.
    pub spinner: Spinner,
    /// Stable committed focus identifier.
    pub focused: Option<String>,
    /// Whether the key hint bar is shown.
    pub show_help: bool,
    /// Focus state of the close-all confirmation dialog buttons.
    pub confirm_actions: Cell<ActionRowState>,
    keymap: Keymap,
    key_hints: Vec<(String, &'static str)>,
    scrollbar_drag: ScrollbarState,
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
        let keymap = Keymap::defaults();
        Self {
            page,
            document: PaintedDocument::new(document),
            offset: 0,
            render_mode: RenderMode::default(),
            status: String::new(),
            search_query: String::new(),
            tab_status: String::new(),
            tabs: Vec::new(),
            prompt: None,
            prompt_input: RefCell::new(TextInputState::default()),
            loading: false,
            loading_started: Instant::now(),
            spinner: Spinner::braille(),
            focused: None,
            show_help: false,
            confirm_actions: Cell::new(ActionRowState::default()),
            key_hints: keymap.hints(),
            keymap,
            scrollbar_drag: ScrollbarState::default(),
            interaction_router: InteractionRouter::new(),
            terminal_size,
            pending_action: None,
            effect: None,
            request_generation: 0,
        }
    }

    /// Current browser-specific terminal projection.
    #[must_use]
    pub const fn document(&self) -> &RenderedDocument {
        self.document.document()
    }

    /// Current projection with BMUX lines and hit ids prepared for painting.
    #[must_use]
    pub const fn painted_document(&self) -> &PaintedDocument {
        &self.document
    }

    /// Replace the terminal projection, preparing it for painting once.
    pub fn set_document(&mut self, document: RenderedDocument) {
        self.document = PaintedDocument::new(document);
    }

    /// Install key bindings (for example built from config overrides).
    pub fn set_keymap(&mut self, keymap: Keymap) {
        self.key_hints = keymap.hints();
        self.keymap = keymap;
    }

    /// Key hint pairs (key label, action label) for the hint bar.
    #[must_use]
    pub fn key_hints(&self) -> &[(String, &'static str)] {
        &self.key_hints
    }

    /// Frame layout for the current terminal size and visible chrome.
    #[must_use]
    pub const fn layout(&self) -> BrowserLayout {
        BrowserLayout::new(
            Rect::new(0, 0, self.terminal_size.width, self.terminal_size.height),
            !self.tabs.is_empty(),
            self.show_help,
        )
    }

    /// Scrollbar geometry for the current document and viewport.
    ///
    /// Line counts beyond `u16::MAX` are scaled down so long documents still
    /// get a proportional thumb.
    #[must_use]
    pub fn scrollbar_state(&self) -> ScrollbarState {
        let scale = self.scroll_scale();
        let to_track = |lines: usize| u16::try_from(lines / scale).unwrap_or(u16::MAX);
        ScrollbarState::new(
            to_track(self.document().lines.len()),
            to_track(self.viewport_height()),
        )
        .offset(to_track(self.offset))
    }

    fn scroll_scale(&self) -> usize {
        self.document()
            .lines
            .len()
            .div_ceil(usize::from(u16::MAX))
            .max(1)
    }

    fn viewport_height(&self) -> usize {
        usize::from(self.layout().body.height.max(1))
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
        self.loading_started = Instant::now();
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

    /// Current prompt text, empty when no prompt is open.
    #[must_use]
    pub fn prompt_text(&self) -> String {
        self.prompt_input.borrow().buffer().text().to_owned()
    }

    fn open_prompt(&mut self, prompt: PromptState, initial: &str) -> Update<BrowserMessage> {
        self.prompt = Some(prompt);
        *self.prompt_input.get_mut() = TextInputState::new(TextEditBuffer::from_text(initial));
        Update::redraw()
    }

    fn reproject_document(&mut self) {
        self.set_document(render_document_mode(
            &self.page,
            &TerminalRenderOptions {
                width: document_width(self.terminal_size.width),
            },
            self.render_mode,
        ));
        self.offset = page_scroll_offset(&self.page, self.document());
    }

    fn max_offset(&self) -> usize {
        self.document()
            .lines
            .len()
            .saturating_sub(self.viewport_height())
    }

    fn scroll_to(&mut self, offset: usize) -> Update<BrowserMessage> {
        self.offset = offset.min(self.max_offset());
        self.scroll_update()
    }

    /// Route scrollbar clicks and drags; `None` when the event is not for it.
    fn handle_scrollbar_mouse(&mut self, event: &Event) -> Option<Update<BrowserMessage>> {
        let area = self.layout().scrollbar;
        // Keep BMUX's private drag flag; refresh geometry from the document.
        let current = self.scrollbar_state();
        self.scrollbar_drag.content_len = current.content_len;
        self.scrollbar_drag.viewport_len = current.viewport_len;
        self.scrollbar_drag.offset = current.offset;
        match Scrollbar::new().handle_event(area, &mut self.scrollbar_drag, event) {
            ScrollbarOutcome::Changed { offset } => {
                Some(self.scroll_to(usize::from(offset) * self.scroll_scale()))
            }
            ScrollbarOutcome::Redraw => Some(Update::redraw()),
            ScrollbarOutcome::Ignored => None,
        }
    }

    fn handle_terminal(&mut self, event: &Event) -> Update<BrowserMessage> {
        match event {
            Event::Resize(size) => {
                // The runtime already resizes and resets the presenter.
                self.terminal_size = *size;
                self.reproject_document();
                Update::redraw()
            }
            Event::Key(_) | Event::Paste(_) if self.prompt.is_some() => {
                self.handle_prompt_event(event)
            }
            Event::Key(stroke) => self.handle_unprompted_key(*stroke),
            Event::Mouse(_) if matches!(self.prompt, Some(PromptState::ConfirmCloseAll)) => {
                self.handle_confirm_event(event)
            }
            Event::Mouse(mouse) => {
                if let Some(update) = self.handle_scrollbar_mouse(event) {
                    return update;
                }
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
                    MouseEventKind::ScrollUp => self.scroll_to(self.offset.saturating_sub(3)),
                    MouseEventKind::ScrollDown => self.scroll_to(self.offset.saturating_add(3)),
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
        if stroke.modifiers.ctrl && stroke.key == KeyCode::Char('c') {
            return Update::exit();
        }
        let action = self.keymap.resolve(stroke);
        // Tab-switch bindings bypass BMUX focus traversal of the committed scene.
        if matches!(action, Some(KeyAction::NextTab | KeyAction::PreviousTab)) {
            return self.key_action(action);
        }
        let route = self.interaction_router.route(Event::Key(stroke));
        if let Some(focus) = route.focus_changed {
            self.focused = Some(focus.as_str().to_owned());
            Update::redraw()
        } else {
            self.key_action(action)
        }
    }

    fn key_action(&mut self, action: Option<KeyAction>) -> Update<BrowserMessage> {
        let Some(action) = action else {
            return Update::none();
        };
        let page = self.viewport_height();
        match action {
            KeyAction::Quit => Update::exit(),
            KeyAction::NextTab => self.action(BrowserAction::NextTab),
            KeyAction::PreviousTab => self.action(BrowserAction::PreviousTab),
            KeyAction::ScrollDown => self.scroll_to(self.offset.saturating_add(1)),
            KeyAction::ScrollUp => self.scroll_to(self.offset.saturating_sub(1)),
            KeyAction::PageDown => self.scroll_to(self.offset.saturating_add(page)),
            KeyAction::PageUp => self.scroll_to(self.offset.saturating_sub(page)),
            KeyAction::Top => self.scroll_to(0),
            KeyAction::Bottom => self.scroll_to(usize::MAX),
            KeyAction::CycleRenderMode => {
                // A plain redraw keeps retained terminal images that are still valid.
                self.render_mode = self.render_mode.next();
                self.reproject_document();
                self.status = format!("render mode: {}", self.render_mode.label());
                Update::redraw()
            }
            KeyAction::OpenUrl => self.open_prompt(PromptState::OpenUrl, ""),
            KeyAction::Search => {
                let query = self.search_query.clone();
                self.open_prompt(PromptState::Search, &query)
            }
            KeyAction::SearchNext => self.repeat_search(false),
            KeyAction::SearchPrevious => self.repeat_search(true),
            KeyAction::NextAction => self.focus_action(true),
            KeyAction::PreviousAction => self.focus_action(false),
            KeyAction::Activate => self.activate_focused_action(),
            KeyAction::CloseAllTabs => {
                let mut actions = ActionRowState::default();
                actions.set_focused(Some(0));
                self.confirm_actions.set(actions);
                self.open_prompt(PromptState::ConfirmCloseAll, "")
            }
            KeyAction::CloseTab => self.action(BrowserAction::CloseTab),
            KeyAction::Back => self.action(BrowserAction::Back),
            KeyAction::Forward => self.action(BrowserAction::Forward),
            KeyAction::Reload => self.action(BrowserAction::Reload),
            KeyAction::ToggleHelp => {
                self.show_help = !self.show_help;
                self.offset = self.offset.min(self.max_offset());
                Update::redraw()
            }
        }
    }

    fn focus_action(&mut self, forward: bool) -> Update<BrowserMessage> {
        let found = next_action_line(self.document(), self.offset, forward);
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
            .or_else(|| first_action_on_line(self.document(), self.offset));
        target.map_or_else(Update::none, |target| self.activate_target(&target))
    }

    fn activate_target(&mut self, target: &str) -> Update<BrowserMessage> {
        if let Some(id) = target.strip_prefix(CONFIRM_ACTIONS_PREFIX) {
            return self.confirm_close_all(id == CONFIRM_CLOSE_ALL);
        }
        if let Some(link_id) = target
            .strip_prefix("browser.link.")
            .and_then(|value| value.parse::<u32>().ok())
        {
            let action_id = self
                .document()
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
                .document()
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
                ControlHitboxAction::InputText => self.open_prompt(
                    PromptState::ControlInput {
                        control_id,
                        action_id,
                    },
                    "",
                ),
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
            find_document_line_reverse(self.document(), &self.search_query, self.offset)
        } else {
            find_document_line(
                self.document(),
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

    fn handle_prompt_event(&mut self, event: &Event) -> Update<BrowserMessage> {
        let Some(prompt) = &self.prompt else {
            return Update::none();
        };
        if !prompt.is_editable() {
            return self.handle_confirm_event(event);
        }
        if matches!(event, Event::Key(stroke) if stroke.key == KeyCode::Escape) {
            self.prompt = None;
            return Update::redraw();
        }
        match TextInputControl::new(&PROMPT_POLICY).handle_event(self.prompt_input.get_mut(), event)
        {
            TextInputOutcome::Submitted => self.submit_prompt(),
            TextInputOutcome::Edited | TextInputOutcome::Redraw => Update::redraw(),
            TextInputOutcome::Ignored | TextInputOutcome::EdgeUp | TextInputOutcome::EdgeDown => {
                Update::none()
            }
        }
    }

    /// Close-all confirmation: `y`/`n`/Escape shortcuts, arrow focus and
    /// Enter/Space activation through BMUX's action row, and button clicks.
    fn handle_confirm_event(&mut self, event: &Event) -> Update<BrowserMessage> {
        if let Event::Key(stroke) = event {
            match stroke.key {
                KeyCode::Char('y') => return self.confirm_close_all(true),
                KeyCode::Char('n') | KeyCode::Escape => return self.confirm_close_all(false),
                _ => {}
            }
        }
        if let Event::Mouse(mouse) = event {
            let route = self.interaction_router.route(Event::Mouse(*mouse));
            if matches!(
                mouse.kind,
                MouseEventKind::Up(bmux_tui::event::MouseButton::Left)
            ) && let Some(target) = route.target
                && target.as_str().starts_with(CONFIRM_ACTIONS_PREFIX)
            {
                return self.activate_target(target.as_str());
            }
            return Update::none();
        }
        let actions = confirm_close_all_actions();
        let mut state = self.confirm_actions.get();
        let outcome = ActionRow::new(&actions)
            .policy(ActionRowPolicy::roving())
            .handle_event(Rect::default(), &mut state, event);
        self.confirm_actions.set(state);
        match outcome {
            ActionRowOutcome::Activated { id, .. } => {
                self.confirm_close_all(id == CONFIRM_CLOSE_ALL)
            }
            outcome if outcome.needs_redraw() => Update::redraw(),
            _ => Update::none(),
        }
    }

    fn confirm_close_all(&mut self, confirmed: bool) -> Update<BrowserMessage> {
        self.prompt = None;
        if confirmed {
            self.action(BrowserAction::CloseAllTabs)
        } else {
            self.status = "close all canceled".to_string();
            Update::redraw()
        }
    }

    fn submit_prompt(&mut self) -> Update<BrowserMessage> {
        let input = self.prompt_text();
        match self.prompt.take() {
            Some(PromptState::OpenUrl) if !input.is_empty() => {
                self.status.clone_from(&input);
                self.action(BrowserAction::OpenUrl(input))
            }
            Some(PromptState::Search) => {
                if let Some(found) = find_document_line(self.document(), &input, 0) {
                    self.offset = found;
                    self.status = format!("found: {input}");
                } else {
                    self.status = format!("not found: {input}");
                }
                self.search_query = input;
                Update::redraw()
            }
            Some(PromptState::ControlInput {
                control_id,
                action_id,
            }) => self.action(BrowserAction::InputControl {
                control_id,
                action_id,
                text: input,
            }),
            Some(PromptState::OpenUrl | PromptState::ConfirmCloseAll) | None => Update::redraw(),
        }
    }

    /// Scrolling is local: redraw immediately and schedule a debounced,
    /// replaceable browser sync without loading state.
    fn scroll_update(&mut self) -> Update<BrowserMessage> {
        let y_px = self
            .document()
            .lines
            .get(self.offset)
            .and_then(|line| line.source_y_px)
            .unwrap_or(0);
        if y_px == self.page.scroll_y_px {
            return Update::redraw();
        }
        self.page.scroll_y_px = y_px;
        let Some(effect) = self.effect.as_ref() else {
            return Update::redraw();
        };
        Update::redraw().with_command(effect(
            BrowserAction::ScrollTo(y_px),
            self.request_generation,
            self.terminal_size,
            self.render_mode,
        ))
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
            .with_subscription(loading_subscription(generation, self.spinner.interval()))
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

fn loading_subscription(generation: u64, interval: Duration) -> Subscription<BrowserMessage> {
    Subscription::new(
        SubscriptionKey::new("browser.loading"),
        move |sender| async move {
            loop {
                tokio::time::sleep(interval).await;
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
        BrowserAction::LoadActiveSnapshot => "loading active tab",
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
                self.set_document(*document);
                self.set_tabs(tabs);
                self.offset = self.offset.min(self.max_offset());
                self.pending_action = None;
                self.status.clear();
                self.loading = false;
                Update::redraw().cancel_subscription(SubscriptionKey::new("browser.loading"))
            }
            RuntimeEvent::Message(BrowserMessage::ScrollSynced {
                mut page,
                document,
                generation,
            }) if generation == self.request_generation && !self.loading => {
                // Keep the user's latest local position, not the one the sync started at.
                page.scroll_y_px = self.page.scroll_y_px;
                self.page = *page;
                self.set_document(*document);
                self.offset = page_scroll_offset(&self.page, self.document());
                Update::redraw()
            }
            // The spinner frame derives from elapsed time; ticks only request redraws.
            RuntimeEvent::Message(BrowserMessage::LoadingTick { generation })
                if generation == self.request_generation && self.loading =>
            {
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
        assert_eq!(program.prompt_text(), "A");
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
            program.document().lines[program.offset]
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

    type Recorded = std::sync::Arc<std::sync::Mutex<Vec<super::BrowserAction>>>;

    fn scrollable_program() -> (BrowserProgram, Recorded) {
        let mut program = BrowserProgram::new(
            trowser_render::SemanticPage::default(),
            RenderedDocument {
                lines: (0..20_u32)
                    .map(|index| RenderedLine {
                        text: format!("line {index}"),
                        source_y_px: Some(index * 20),
                        ..RenderedLine::default()
                    })
                    .collect(),
            },
            Size::new(80, 6),
        );
        let recorded = Recorded::default();
        let sink = std::sync::Arc::clone(&recorded);
        program.set_effect(std::sync::Arc::new(move |action, _, _, _| {
            sink.lock().unwrap().push(action);
            bmux_tui_runtime::Command::concurrent(async { None })
        }));
        (program, recorded)
    }

    fn key(
        program: &mut BrowserProgram,
        code: KeyCode,
    ) -> bmux_tui_runtime::Update<BrowserMessage> {
        program
            .update(RuntimeEvent::Terminal(Event::Key(KeyStroke::simple(code))))
            .unwrap()
    }

    #[test]
    fn scrolling_is_local_and_only_schedules_background_sync() {
        let (mut program, recorded) = scrollable_program();

        let update = key(&mut program, KeyCode::Char('j'));

        assert_eq!(program.offset, 1);
        assert_eq!(update.invalidation, Invalidation::Redraw);
        assert!(!program.loading, "scrolling must not show loading state");
        assert!(program.status.is_empty());
        assert!(program.pending_action.is_none());
        assert_eq!(program.page.scroll_y_px, 20);
        assert_eq!(
            *recorded.lock().unwrap(),
            [super::BrowserAction::ScrollTo(20)]
        );

        // Scrolling back to an already-synced position schedules nothing new.
        key(&mut program, KeyCode::Char('k'));
        key(&mut program, KeyCode::Char('k'));
        assert_eq!(
            *recorded.lock().unwrap(),
            [
                super::BrowserAction::ScrollTo(20),
                super::BrowserAction::ScrollTo(0)
            ]
        );
    }

    #[test]
    fn scroll_sync_keeps_latest_local_position_and_yields_to_navigation() {
        let (mut program, _) = scrollable_program();
        for _ in 0..3 {
            key(&mut program, KeyCode::Char('j'));
        }
        let synced_document = program.document().clone();
        let generation = program.begin_request("idle");
        program.loading = false;

        program
            .update(RuntimeEvent::Message(BrowserMessage::ScrollSynced {
                page: Box::new(trowser_render::SemanticPage {
                    title: "synced".to_string(),
                    scroll_y_px: 20,
                    ..trowser_render::SemanticPage::default()
                }),
                document: Box::new(synced_document.clone()),
                generation,
            }))
            .unwrap();
        assert_eq!(program.page.title, "synced");
        assert_eq!(program.offset, 3, "sync must not move the view back");

        let navigation = program.begin_request("navigating");
        program
            .update(RuntimeEvent::Message(BrowserMessage::ScrollSynced {
                page: Box::new(trowser_render::SemanticPage {
                    title: "late sync".to_string(),
                    ..trowser_render::SemanticPage::default()
                }),
                document: Box::new(synced_document),
                generation: navigation,
            }))
            .unwrap();
        assert_eq!(program.page.title, "synced", "sync ignored while loading");
    }

    #[test]
    fn render_mode_switch_redraws_without_resetting_images() {
        let mut program = program();

        let update = key(&mut program, KeyCode::Char('m'));

        assert_eq!(update.invalidation, Invalidation::Redraw);
    }

    #[test]
    fn prompt_uses_bmux_text_editing() {
        let mut program = program();
        for code in [
            KeyCode::Char('o'),
            KeyCode::Char('a'),
            KeyCode::Char('c'),
            KeyCode::Left,
            KeyCode::Char('b'),
            KeyCode::End,
            KeyCode::Space,
        ] {
            key(&mut program, code);
        }
        program
            .update(RuntimeEvent::Terminal(Event::Paste(
                "d.example".to_string(),
            )))
            .unwrap();
        assert_eq!(program.prompt_text(), "abc d.example");

        key(&mut program, KeyCode::Escape);
        assert!(program.prompt.is_none());
        assert!(program.pending_action.is_none());
    }

    #[test]
    fn search_prompt_starts_with_previous_query() {
        let mut program = program();
        program.search_query = "line 4".to_string();

        key(&mut program, KeyCode::Char('/'));
        assert_eq!(program.prompt_text(), "line 4");
        key(&mut program, KeyCode::Enter);

        assert_eq!(program.offset, 4);
        assert_eq!(program.status, "found: line 4");
    }

    fn shifted(code: KeyCode) -> KeyStroke {
        KeyStroke::with_modifiers(
            code,
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
        )
    }

    #[test]
    fn configured_keymap_drives_actions() {
        let mut program = program();
        let overrides = std::collections::BTreeMap::from([(
            "scroll_down".to_string(),
            vec!["ctrl+e".to_string()],
        )]);
        program.set_keymap(crate::keymap::Keymap::with_overrides(&overrides).unwrap());

        key(&mut program, KeyCode::Char('j'));
        assert_eq!(program.offset, 0, "default binding was replaced");
        program
            .update(RuntimeEvent::Terminal(Event::Key(
                KeyStroke::with_modifiers(
                    KeyCode::Char('e'),
                    Modifiers {
                        ctrl: true,
                        ..Modifiers::NONE
                    },
                ),
            )))
            .unwrap();
        assert_eq!(program.offset, 1);
    }

    #[test]
    fn help_toggle_shrinks_the_scrollable_viewport() {
        let mut program = program();
        let before = program.layout().body.height;

        key(&mut program, KeyCode::Char('?'));

        assert!(program.show_help);
        assert_eq!(program.layout().body.height, before - 1);
        key(&mut program, KeyCode::Char('?'));
        assert!(!program.show_help);
    }

    #[test]
    fn close_all_dialog_supports_arrows_enter_escape_and_clicks() {
        let mut program = program();
        let open = |program: &mut BrowserProgram| {
            program
                .update(RuntimeEvent::Terminal(Event::Key(shifted(KeyCode::Char(
                    'x',
                )))))
                .unwrap();
            assert_eq!(program.prompt, Some(super::PromptState::ConfirmCloseAll));
        };

        open(&mut program);
        key(&mut program, KeyCode::Enter);
        assert!(program.prompt.is_none(), "Enter on focused Cancel closes");
        assert_eq!(program.status, "close all canceled");

        open(&mut program);
        key(&mut program, KeyCode::Right);
        key(&mut program, KeyCode::Enter);
        assert_eq!(
            program.pending_action,
            Some(super::BrowserAction::CloseAllTabs)
        );

        program.pending_action = None;
        open(&mut program);
        key(&mut program, KeyCode::Escape);
        assert!(program.prompt.is_none());
        assert!(program.pending_action.is_none());

        open(&mut program);
        let mut scene = bmux_tui::hit::HitMap::new();
        scene.push(HitRegion::new(
            "chrome.confirm.actions.close-all",
            Rect::new(10, 2, 9, 1),
        ));
        program.commit_interactions(scene);
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            program
                .update(RuntimeEvent::Terminal(Event::Mouse(MouseEvent::new(
                    kind,
                    Point::new(12, 2),
                ))))
                .unwrap();
        }
        assert_eq!(
            program.pending_action,
            Some(super::BrowserAction::CloseAllTabs)
        );
    }

    #[test]
    fn dragging_the_scrollbar_scrolls_the_document() {
        let mut program = program();
        let scrollbar = program.layout().scrollbar;
        let top = Point::new(scrollbar.x, scrollbar.y);
        let bottom = Point::new(scrollbar.x, scrollbar.bottom() - 1);

        for (kind, point) in [
            (MouseEventKind::Down(MouseButton::Left), top),
            (MouseEventKind::Drag(MouseButton::Left), bottom),
            (MouseEventKind::Up(MouseButton::Left), bottom),
        ] {
            program
                .update(RuntimeEvent::Terminal(Event::Mouse(MouseEvent::new(
                    kind, point,
                ))))
                .unwrap();
        }

        assert!(program.offset > 0, "offset {}", program.offset);
    }
}
