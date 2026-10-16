//! Keymap policy state is owned by each editor buffer and reset on provider replacement.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    /// Stable provider-defined mode ID for keybinding contexts; empty means unspecified.
    pub mode: String,
    pub label: String,
    /// Opaque provider data; never shared between buffers or persisted across restarts.
    pub data: String,
    /// Suppress composition and keyboard text if the provider fails or declines.
    pub capture_text: bool,
    /// Keep the caret on a character rather than the insertion point after line text.
    pub clamp_caret: bool,
    /// Up to eight Ctrl+character bindings owned by the focused editor.
    pub control_keys: Vec<String>,
}
impl State {
    pub fn validate(&self) -> Result<(), String> {
        if self.mode.len() > 64
            || self
                .mode
                .chars()
                .any(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '_' && c != '-')
            || self.label.len() > 64
            || self.label.chars().any(char::is_control)
            || self.data.len() > 4096
            || self.control_keys.len() > 8
            || self
                .control_keys
                .iter()
                .any(|key| key.chars().count() != 1 || key.chars().any(char::is_control))
            || self
                .control_keys
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.control_keys.len()
        {
            return Err("invalid keymap state".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    /// Committed keyboard text, a named key (Escape, ArrowLeft, ...), or empty to initialize.
    pub key: String,
    /// True for committed text; false for named keys and initialization.
    pub text: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
    pub state: State,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBackward,
    WordEnd,
    LineStart,
    FirstNonBlank,
    LineEnd,
    DocumentStart,
    DocumentEnd,
    /// One-based logical line number supplied by the action count.
    Line,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionKind {
    Block,
    Character,
    Line,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Select { mode: Option<SelectionKind> },
    SwapSelectionEnds,
    DeleteSelection,
    ChangeSelection,
    InsertAtSelections { after: bool },
    Move { motion: Motion, count: usize },
    DeleteCharacters { count: usize },
    DeleteLines { count: usize },
    InsertLine { below: bool },
    Undo { count: usize },
    Redo { count: usize },
}
impl Action {
    pub fn validate(&self) -> Result<(), String> {
        let count = match self {
            Self::Move { count, .. }
            | Self::DeleteCharacters { count }
            | Self::DeleteLines { count }
            | Self::Undo { count }
            | Self::Redo { count } => *count,
            Self::InsertLine { .. }
            | Self::Select { .. }
            | Self::SwapSelectionEnds
            | Self::DeleteSelection
            | Self::ChangeSelection
            | Self::InsertAtSelections { .. } => 1,
        };
        if !(1..=10_000).contains(&count) {
            return Err("keymap count must be 1..=10000".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    /// False preserves state but lets ordinary input handling continue.
    pub handled: bool,
    pub state: State,
    pub actions: Vec<Action>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Decision, Intent, Request, Selection};
    #[test]
    fn keymaps_validate_state_actions_and_pass_through_without_document_text() {
        let request = Request {
            revision: 3,
            text: String::new(),
            offset: 0,
            document_length: 500_000,
            selection: Selection::caret(400_000),
            intent: Intent::Key(Key {
                key: "x".into(),
                text: true,
                control: false,
                alt: false,
                meta: false,
                state: State::default(),
            }),
            settings: Default::default(),
            pairs: vec![],
        };
        request.validate().unwrap();
        let valid = Response {
            handled: true,
            state: State::default(),
            actions: vec![Action::DeleteCharacters { count: 1 }],
        };
        Decision::Input(valid.clone()).validate(&request).unwrap();
        for response in [
            Response {
                handled: false,
                ..valid.clone()
            },
            Response {
                actions: vec![Action::DeleteLines { count: 10_001 }],
                ..valid.clone()
            },
            Response {
                actions: vec![Action::Undo { count: 0 }],
                ..valid.clone()
            },
            Response {
                state: State {
                    data: "x".repeat(4097),
                    ..Default::default()
                },
                ..valid.clone()
            },
            Response {
                state: State {
                    label: "bad\nmode".into(),
                    ..Default::default()
                },
                ..valid.clone()
            },
        ] {
            assert!(Decision::Input(response).validate(&request).is_err());
        }
        for keys in [
            vec!["v".into(); 2],
            vec!["long".into()],
            vec!["\n".into()],
            (0..9).map(|n| n.to_string()).collect(),
        ] {
            assert!(
                State {
                    control_keys: keys,
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
        let mut other = request.clone();
        other.intent = Intent::Type("x".into());
        other.text = "x".into();
        other.document_length = 1;
        other.selection = Selection::caret(0);
        assert!(Decision::Input(valid).validate(&other).is_err());
    }
}
