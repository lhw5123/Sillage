//! NeoVim-inspired interaction: insert, command, and f-hint (Vimium) modes.
//!
//! Double Escape within [`DOUBLE_ESC`] enters command mode.
//! From command mode, `f` labels every clickable control.

use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{App, Div, KeyBinding, ParentElement as _, SharedString, Styled, actions, div, px};
use gpui_component::ActiveTheme as _;

actions!(
    sillage,
    [
        HandleEscape,
        EnterInsert,
        ShowHints,
        SubmitTask,
        CancelTask,
        StartNewTask,
        FocusSearch,
        FindNext,
        FindPrevious,
        ToggleSidebar,
        TogglePreview,
        PickWorkspace
    ]
);

const WORKSPACE: &str = "SillageWorkspace";
const COMMAND: &str = "SillageCommand";
const HINT: &str = "SillageHint";

/// Two Escape presses within this window enter command mode.
pub const DOUBLE_ESC: Duration = Duration::from_millis(400);

/// GPUI key-context name for the current mode.
pub fn key_context(mode: &Mode) -> &'static str {
    match mode {
        Mode::Insert => WORKSPACE,
        Mode::Command => COMMAND,
        Mode::Hint { .. } => HINT,
    }
}

/// Bind insert, command, and hint chords used by the workspace window.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", HandleEscape, Some(WORKSPACE)),
        KeyBinding::new("escape", HandleEscape, Some(COMMAND)),
        KeyBinding::new("escape", HandleEscape, Some(HINT)),
        KeyBinding::new("cmd-enter", SubmitTask, Some(WORKSPACE)),
        KeyBinding::new("cmd-.", CancelTask, Some(WORKSPACE)),
        KeyBinding::new("cmd-.", CancelTask, Some(COMMAND)),
        KeyBinding::new("cmd-n", StartNewTask, Some(WORKSPACE)),
        KeyBinding::new("cmd-f", FocusSearch, Some(WORKSPACE)),
        KeyBinding::new("cmd-f", FocusSearch, Some(COMMAND)),
        KeyBinding::new("cmd-f", FocusSearch, Some(HINT)),
        KeyBinding::new("cmd-g", FindNext, Some(WORKSPACE)),
        KeyBinding::new("cmd-g", FindNext, Some(COMMAND)),
        KeyBinding::new("cmd-shift-g", FindPrevious, Some(WORKSPACE)),
        KeyBinding::new("cmd-shift-g", FindPrevious, Some(COMMAND)),
        KeyBinding::new("shift-enter", FindPrevious, Some(WORKSPACE)),
        KeyBinding::new("cmd-b", ToggleSidebar, Some(WORKSPACE)),
        KeyBinding::new("cmd-r", TogglePreview, Some(WORKSPACE)),
        KeyBinding::new("cmd-r", TogglePreview, Some(COMMAND)),
        KeyBinding::new("cmd-o", PickWorkspace, Some(WORKSPACE)),
        KeyBinding::new("i", EnterInsert, Some(COMMAND)),
        KeyBinding::new("f", ShowHints, Some(COMMAND)),
        KeyBinding::new("/", FocusSearch, Some(COMMAND)),
        KeyBinding::new("n", StartNewTask, Some(COMMAND)),
        KeyBinding::new("b", ToggleSidebar, Some(COMMAND)),
        KeyBinding::new("r", TogglePreview, Some(COMMAND)),
        KeyBinding::new("o", PickWorkspace, Some(COMMAND)),
    ]);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Insert,
    Command,
    Hint { typed: String },
}

impl Mode {
    pub fn status_label(&self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Command => "COMMAND",
            Self::Hint { .. } => "HINT",
        }
    }

    pub fn is_hint(&self) -> bool {
        matches!(self, Self::Hint { .. })
    }

    pub fn typed_hint(&self) -> &str {
        match self {
            Self::Hint { typed } => typed,
            _ => "",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintAction {
    NewTask,
    Search,
    ToggleSidebar,
    TogglePreview,
    CycleAgent,
    ToggleAccess,
    PickWorkspace,
    Submit,
    Project { index: usize },
    Task { project: usize, task: usize },
    ExpandProject { index: usize },
}

#[derive(Debug, Clone)]
pub struct HintTarget {
    pub label: String,
    pub action: HintAction,
}

pub struct CommandState {
    pub mode: Mode,
    last_escape: Option<Instant>,
}

impl CommandState {
    pub fn new() -> Self {
        Self {
            mode: Mode::Insert,
            last_escape: None,
        }
    }

    /// Handle Escape. Returns whether the key was consumed as a mode change.
    pub fn on_escape(&mut self) -> bool {
        match &self.mode {
            Mode::Hint { .. } => {
                self.mode = Mode::Command;
                self.last_escape = None;
                true
            }
            Mode::Command => {
                self.mode = Mode::Insert;
                self.last_escape = None;
                true
            }
            Mode::Insert => {
                let now = Instant::now();
                if let Some(prev) = self.last_escape
                    && now.duration_since(prev) <= DOUBLE_ESC
                {
                    self.mode = Mode::Command;
                    self.last_escape = None;
                    return true;
                }
                self.last_escape = Some(now);
                false
            }
        }
    }

    pub fn enter_hints(&mut self) {
        if matches!(self.mode, Mode::Command | Mode::Hint { .. }) {
            self.mode = Mode::Hint {
                typed: String::new(),
            };
        }
    }

    pub fn enter_insert(&mut self) {
        self.mode = Mode::Insert;
        self.last_escape = None;
    }

    /// Append a typed character while hinting. `matching` is the current label set.
    /// Returns the unique matching target when the typed prefix selects exactly one.
    pub fn type_hint(&mut self, ch: char, labels: &[HintTarget]) -> Option<HintAction> {
        let Mode::Hint { typed } = &mut self.mode else {
            return None;
        };
        if !ch.is_ascii_alphabetic() {
            return None;
        }
        typed.push(ch.to_ascii_lowercase());
        let prefix = typed.clone();
        let matches: Vec<_> = labels
            .iter()
            .filter(|target| target.label.starts_with(&prefix))
            .collect();
        if matches.len() == 1 && matches[0].label == prefix {
            let action = matches[0].action;
            self.mode = Mode::Insert;
            return Some(action);
        }
        if matches.is_empty() {
            typed.clear();
        }
        None
    }
}

/// Home-row first, then remaining letters; two-character codes once the alphabet is exhausted.
pub fn hint_labels(count: usize) -> Vec<String> {
    const CHARS: &[u8] = b"asdfgqwertzxcvb";
    if count == 0 {
        return Vec::new();
    }
    if count <= CHARS.len() {
        return CHARS[..count]
            .iter()
            .map(|ch| (*ch as char).to_string())
            .collect();
    }
    let mut labels = Vec::with_capacity(count);
    for prefix in CHARS {
        for suffix in CHARS {
            labels.push(format!("{}{}", *prefix as char, *suffix as char));
            if labels.len() == count {
                return labels;
            }
        }
    }
    labels
}

/// Badge rendered over a clickable control while hint mode is active.
pub fn hint_badge(label: &str, active: bool, cx: &App) -> Div {
    div()
        .absolute()
        .top(px(-6.))
        .left(px(-6.))
        .px_1()
        .rounded(px(3.))
        .text_xs()
        .font_weight(gpui::FontWeight::BOLD)
        .when(active, |this| {
            this.bg(cx.theme().primary)
                .text_color(cx.theme().primary_foreground)
        })
        .when(!active, |this| {
            this.bg(cx.theme().muted)
                .text_color(cx.theme().muted_foreground)
                .opacity(0.45)
        })
        .child(SharedString::from(label.to_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn double_escape_enters_command_mode() {
        let mut state = CommandState::new();
        assert!(!state.on_escape());
        assert_eq!(state.mode, Mode::Insert);
        thread::sleep(Duration::from_millis(10));
        assert!(state.on_escape());
        assert_eq!(state.mode, Mode::Command);
    }

    #[test]
    fn unique_hint_selects_target() {
        let mut state = CommandState::new();
        state.mode = Mode::Hint {
            typed: String::new(),
        };
        let targets = vec![
            HintTarget {
                label: "a".into(),
                action: HintAction::NewTask,
            },
            HintTarget {
                label: "s".into(),
                action: HintAction::Search,
            },
        ];
        let action = state.type_hint('a', &targets);
        assert_eq!(action, Some(HintAction::NewTask));
        assert_eq!(state.mode, Mode::Insert);
    }

    #[test]
    fn hint_labels_are_unique_and_home_row_first() {
        let labels = hint_labels(4);
        assert_eq!(labels, vec!["a", "s", "d", "f"]);
        let many = hint_labels(20);
        assert_eq!(many.len(), 20);
        let set: std::collections::HashSet<_> = many.iter().collect();
        assert_eq!(set.len(), 20);
    }
}
