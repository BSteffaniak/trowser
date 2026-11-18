//! Configurable interactive key bindings built on `bmux_keyboard`.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use bmux_keyboard::{KeyCode, KeyStroke, Modifiers, parse_key_stroke};

/// Browser commands that can be bound to keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyAction {
    /// Exit trowser.
    Quit,
    /// Scroll down one line.
    ScrollDown,
    /// Scroll up one line.
    ScrollUp,
    /// Scroll down one viewport.
    PageDown,
    /// Scroll up one viewport.
    PageUp,
    /// Jump to the top of the document.
    Top,
    /// Jump to the bottom of the document.
    Bottom,
    /// Cycle the semantic render mode.
    CycleRenderMode,
    /// Open the URL prompt.
    OpenUrl,
    /// Open the in-document search prompt.
    Search,
    /// Jump to the next search match.
    SearchNext,
    /// Jump to the previous search match.
    SearchPrevious,
    /// Focus the next link or control.
    NextAction,
    /// Focus the previous link or control.
    PreviousAction,
    /// Activate the focused link or control.
    Activate,
    /// Close the active tab.
    CloseTab,
    /// Ask to close every tab.
    CloseAllTabs,
    /// Navigate back.
    Back,
    /// Navigate forward.
    Forward,
    /// Reload the page.
    Reload,
    /// Switch to the next tab.
    NextTab,
    /// Switch to the previous tab.
    PreviousTab,
    /// Show or hide the key hint bar.
    ToggleHelp,
}

impl KeyAction {
    /// Every bindable action.
    pub const ALL: [Self; 23] = [
        Self::Quit,
        Self::ScrollDown,
        Self::ScrollUp,
        Self::PageDown,
        Self::PageUp,
        Self::Top,
        Self::Bottom,
        Self::CycleRenderMode,
        Self::OpenUrl,
        Self::Search,
        Self::SearchNext,
        Self::SearchPrevious,
        Self::NextAction,
        Self::PreviousAction,
        Self::Activate,
        Self::CloseTab,
        Self::CloseAllTabs,
        Self::Back,
        Self::Forward,
        Self::Reload,
        Self::NextTab,
        Self::PreviousTab,
        Self::ToggleHelp,
    ];

    /// Config name used in the `[keys]` table.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Quit => "quit",
            Self::ScrollDown => "scroll_down",
            Self::ScrollUp => "scroll_up",
            Self::PageDown => "page_down",
            Self::PageUp => "page_up",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::CycleRenderMode => "cycle_render_mode",
            Self::OpenUrl => "open_url",
            Self::Search => "search",
            Self::SearchNext => "search_next",
            Self::SearchPrevious => "search_previous",
            Self::NextAction => "next_action",
            Self::PreviousAction => "previous_action",
            Self::Activate => "activate",
            Self::CloseTab => "close_tab",
            Self::CloseAllTabs => "close_all_tabs",
            Self::Back => "back",
            Self::Forward => "forward",
            Self::Reload => "reload",
            Self::NextTab => "next_tab",
            Self::PreviousTab => "previous_tab",
            Self::ToggleHelp => "toggle_help",
        }
    }

    /// Short label shown in the key hint bar.
    #[must_use]
    pub const fn hint_label(self) -> &'static str {
        match self {
            Self::Quit => "quit",
            Self::ScrollDown => "down",
            Self::ScrollUp => "up",
            Self::PageDown => "page down",
            Self::PageUp => "page up",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::CycleRenderMode => "mode",
            Self::OpenUrl => "open",
            Self::Search => "search",
            Self::SearchNext => "next match",
            Self::SearchPrevious => "prev match",
            Self::NextAction => "next link",
            Self::PreviousAction => "prev link",
            Self::Activate => "activate",
            Self::CloseTab => "close tab",
            Self::CloseAllTabs => "close all",
            Self::Back => "back",
            Self::Forward => "forward",
            Self::Reload => "reload",
            Self::NextTab => "next tab",
            Self::PreviousTab => "prev tab",
            Self::ToggleHelp => "help",
        }
    }

    const fn default_keys(self) -> &'static [&'static str] {
        match self {
            Self::Quit => &["q"],
            Self::ScrollDown => &["j", "down"],
            Self::ScrollUp => &["k", "up"],
            Self::PageDown => &["pagedown"],
            Self::PageUp => &["pageup"],
            Self::Top => &["g", "home"],
            Self::Bottom => &["shift+g", "end"],
            Self::CycleRenderMode => &["m"],
            Self::OpenUrl => &["o", "t"],
            Self::Search => &["/"],
            Self::SearchNext => &["n"],
            Self::SearchPrevious => &["shift+n"],
            Self::NextAction => &["a"],
            Self::PreviousAction => &["shift+a"],
            Self::Activate => &["enter"],
            Self::CloseTab => &["x"],
            Self::CloseAllTabs => &["shift+x"],
            Self::Back => &["b", "left"],
            Self::Forward => &["f", "shift+f", "right"],
            Self::Reload => &["r"],
            Self::NextTab => &["tab"],
            Self::PreviousTab => &["shift+tab"],
            Self::ToggleHelp => &["?"],
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.name() == name)
    }
}

/// Actions listed in the key hint bar, in display order.
///
/// Help comes first so the key that hides the bar survives truncation on
/// narrow terminals.
pub const HINT_ACTIONS: [KeyAction; 11] = [
    KeyAction::ToggleHelp,
    KeyAction::OpenUrl,
    KeyAction::Search,
    KeyAction::Back,
    KeyAction::Forward,
    KeyAction::Reload,
    KeyAction::NextAction,
    KeyAction::Activate,
    KeyAction::CycleRenderMode,
    KeyAction::CloseTab,
    KeyAction::Quit,
];

/// Invalid `[keys]` configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapError(String);

impl fmt::Display for KeymapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid key binding: {}", self.0)
    }
}

impl std::error::Error for KeymapError {}

/// Resolved key bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    bindings: BTreeMap<KeyStroke, KeyAction>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::defaults()
    }
}

impl Keymap {
    /// Built-in bindings.
    ///
    /// # Panics
    ///
    /// Never in practice: built-in key strings are constants covered by tests.
    #[must_use]
    pub fn defaults() -> Self {
        let mut bindings = BTreeMap::new();
        for action in KeyAction::ALL {
            for key in action.default_keys() {
                let stroke = parse_key_stroke(key).expect("built-in key binding parses");
                bindings.insert(normalize(stroke), action);
            }
        }
        Self { bindings }
    }

    /// Built-in bindings with config overrides applied.
    ///
    /// Each listed action replaces all of its default strokes (an empty list
    /// unbinds it). An overridden stroke takes precedence over a default
    /// binding of the same stroke.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown action names, unparsable strokes, or one
    /// stroke assigned to two overridden actions.
    pub fn with_overrides(overrides: &BTreeMap<String, Vec<String>>) -> Result<Self, KeymapError> {
        let overridden = overrides
            .iter()
            .map(|(name, keys)| {
                KeyAction::from_name(name)
                    .map(|action| (action, name, keys))
                    .ok_or_else(|| KeymapError(format!("unknown action '{name}' in [keys]")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut keymap = Self::defaults();
        keymap.bindings.retain(|_, action| {
            !overridden
                .iter()
                .any(|(overridden, _, _)| overridden == action)
        });
        let mut assigned = BTreeMap::<KeyStroke, KeyAction>::new();
        for (action, name, keys) in overridden {
            for key in keys {
                let stroke = parse_key_stroke(key)
                    .map(normalize)
                    .map_err(|error| KeymapError(format!("{name} = '{key}': {error}")))?;
                if let Some(existing) = assigned.insert(stroke, action)
                    && existing != action
                {
                    return Err(KeymapError(format!(
                        "'{key}' is bound to both {} and {name}",
                        existing.name()
                    )));
                }
                keymap.bindings.insert(stroke, action);
            }
        }
        Ok(keymap)
    }

    /// Action bound to a terminal key stroke.
    #[must_use]
    pub fn resolve(&self, stroke: KeyStroke) -> Option<KeyAction> {
        self.bindings.get(&normalize(stroke)).copied()
    }

    /// Display label for the first stroke bound to `action`.
    #[must_use]
    pub fn key_label(&self, action: KeyAction) -> Option<String> {
        self.bindings
            .iter()
            .find(|(_, bound)| **bound == action)
            .map(|(stroke, _)| format_stroke(*stroke))
    }

    /// Key hint pairs (key label, action label) for the hint bar.
    #[must_use]
    pub fn hints(&self) -> Vec<(String, &'static str)> {
        HINT_ACTIONS
            .into_iter()
            .filter_map(|action| {
                self.key_label(action)
                    .map(|label| (label, action.hint_label()))
            })
            .collect()
    }
}

/// Canonical stroke form: letters carry case as `shift`, and shift is implied
/// (dropped) for other printable characters such as `?` or `/`.
const fn normalize(stroke: KeyStroke) -> KeyStroke {
    let KeyStroke { key, mut modifiers } = stroke;
    let key = match key {
        KeyCode::Char(ch) if ch.is_ascii_uppercase() => {
            modifiers.shift = true;
            KeyCode::Char(ch.to_ascii_lowercase())
        }
        KeyCode::Char(ch) if !ch.is_ascii_alphabetic() => {
            modifiers.shift = false;
            key
        }
        other => other,
    };
    KeyStroke::with_modifiers(key, modifiers)
}

fn format_stroke(stroke: KeyStroke) -> String {
    let KeyStroke { key, modifiers } = stroke;
    let mut label = String::new();
    let Modifiers {
        ctrl,
        alt,
        shift,
        super_key,
        hyper,
        meta,
    } = modifiers;
    for (enabled, prefix) in [
        (ctrl, "ctrl+"),
        (alt, "alt+"),
        (super_key, "super+"),
        (hyper, "hyper+"),
        (meta, "meta+"),
    ] {
        if enabled {
            label.push_str(prefix);
        }
    }
    match key {
        KeyCode::Char(ch) if shift && ch.is_ascii_lowercase() => {
            label.push(ch.to_ascii_uppercase());
            return label;
        }
        _ if shift => label.push_str("shift+"),
        _ => {}
    }
    match key {
        KeyCode::Char(ch) => label.push(ch),
        KeyCode::F(number) => {
            let _ = write!(label, "f{number}");
        }
        KeyCode::Enter => label.push_str("enter"),
        KeyCode::Tab => label.push_str("tab"),
        KeyCode::Backspace => label.push_str("bksp"),
        KeyCode::Delete => label.push_str("del"),
        KeyCode::Escape => label.push_str("esc"),
        KeyCode::Space => label.push_str("space"),
        KeyCode::Up => label.push('↑'),
        KeyCode::Down => label.push('↓'),
        KeyCode::Left => label.push('←'),
        KeyCode::Right => label.push('→'),
        KeyCode::Home => label.push_str("home"),
        KeyCode::End => label.push_str("end"),
        KeyCode::PageUp => label.push_str("pgup"),
        KeyCode::PageDown => label.push_str("pgdn"),
        KeyCode::Insert => label.push_str("ins"),
    }
    label
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use bmux_keyboard::{KeyCode, KeyStroke, Modifiers};

    use super::{KeyAction, Keymap};

    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };

    #[test]
    fn defaults_resolve_letters_case_and_shifted_symbols() {
        let keymap = Keymap::defaults();

        assert_eq!(
            keymap.resolve(KeyStroke::simple(KeyCode::Char('j'))),
            Some(KeyAction::ScrollDown)
        );
        // Terminals report shifted letters either as uppercase or with shift.
        assert_eq!(
            keymap.resolve(KeyStroke::simple(KeyCode::Char('G'))),
            Some(KeyAction::Bottom)
        );
        assert_eq!(
            keymap.resolve(KeyStroke::with_modifiers(KeyCode::Char('g'), SHIFT)),
            Some(KeyAction::Bottom)
        );
        assert_eq!(
            keymap.resolve(KeyStroke::with_modifiers(KeyCode::Char('?'), SHIFT)),
            Some(KeyAction::ToggleHelp)
        );
        assert_eq!(
            keymap.resolve(KeyStroke::with_modifiers(KeyCode::Tab, SHIFT)),
            Some(KeyAction::PreviousTab)
        );
    }

    #[test]
    fn overrides_replace_defaults_and_take_precedence() {
        let overrides = BTreeMap::from([
            (
                "reload".to_string(),
                vec!["ctrl+r".to_string(), "j".to_string()],
            ),
            ("quit".to_string(), Vec::new()),
        ]);

        let keymap = Keymap::with_overrides(&overrides).expect("valid overrides");

        assert_eq!(keymap.resolve(KeyStroke::simple(KeyCode::Char('r'))), None);
        assert_eq!(
            keymap.resolve(KeyStroke::simple(KeyCode::Char('j'))),
            Some(KeyAction::Reload)
        );
        assert_eq!(
            keymap.resolve(KeyStroke::simple(KeyCode::Down)),
            Some(KeyAction::ScrollDown)
        );
        assert_eq!(keymap.resolve(KeyStroke::simple(KeyCode::Char('q'))), None);
        assert_eq!(keymap.key_label(KeyAction::Reload).as_deref(), Some("j"));
    }

    #[test]
    fn rejects_unknown_actions_bad_keys_and_override_conflicts() {
        let unknown = BTreeMap::from([("teleport".to_string(), vec!["t".to_string()])]);
        assert!(Keymap::with_overrides(&unknown).is_err());

        let bad = BTreeMap::from([("reload".to_string(), vec!["hyperdrive+r".to_string()])]);
        assert!(Keymap::with_overrides(&bad).is_err());

        let conflict = BTreeMap::from([
            ("reload".to_string(), vec!["ctrl+r".to_string()]),
            ("back".to_string(), vec!["ctrl+r".to_string()]),
        ]);
        let error = Keymap::with_overrides(&conflict).expect_err("conflict rejected");
        assert!(error.to_string().contains("ctrl+r"));
    }

    #[test]
    fn hints_use_bound_key_labels() {
        let hints = Keymap::defaults().hints();

        assert!(hints.contains(&("o".to_string(), "open")));
        assert!(hints.contains(&("?".to_string(), "help")));
    }
}
