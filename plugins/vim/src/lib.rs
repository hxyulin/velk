//! A small Vim keymap; document operations stay in the editor model.
use serde::{Deserialize, Serialize};
use velk_plugin_sdk::{
    CapabilityDeclaration, EditingBehavior, Plugin, Registrar,
    editing::{
        input::{Action, Motion},
        *,
    },
};

#[derive(Default)]
pub struct Vim;
impl Plugin for Vim {
    const ID: &'static str = "velk.vim";
    const NAME: &'static str = "Vim";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const README: &'static str = include_str!("../README.md");
    const CAPABILITIES: &'static [CapabilityDeclaration] = &[
        CapabilityDeclaration::required(
            "editor.buffer.read",
            "Read caret and document context for modal input.",
        ),
        CapabilityDeclaration::required(
            "editor.buffer.write",
            "Request movements, edits, undo and redo through the editor model.",
        ),
    ];
    fn describe(&mut self) -> String {
        "Normal, Insert and Visual modes with basic Vim motions.".into()
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.editing::<Self>()
    }
}
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    mode: Mode,
    count: usize,
    pending: Option<char>,
    operator_count: usize,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
    VisualBlock,
}
impl Mode {
    fn visual(self) -> bool {
        matches!(self, Self::Visual | Self::VisualLine | Self::VisualBlock)
    }
    fn id(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Insert => "insert",
            Self::Visual => "visual",
            Self::VisualLine => "visual_line",
            Self::VisualBlock => "visual_block",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Visual => "VISUAL",
            Self::VisualLine => "V-LINE",
            Self::VisualBlock => "V-BLOCK",
        }
    }
}
impl EditingBehavior for Vim {
    const ROLE: Role = Role::Keymap;
    fn evaluate(request: &Request) -> Result<Decision, String> {
        request.validate()?;
        let Intent::Key(key) = &request.intent else {
            return Ok(Decision::Decline);
        };
        let mut state: State = if key.state.data.is_empty() {
            State::default()
        } else {
            serde_json::from_str(&key.state.data).map_err(|e| e.to_string())?
        };
        let mut actions = Vec::new();
        let mut handled = true;
        if key.key.is_empty() {
            // Initialization preserves an existing buffer's mode.
        } else if key.meta || key.alt {
            handled = false;
        } else if !key.text && (key.key == "Escape" || key.control && key.key == "[") {
            if state.mode == Mode::Insert {
                actions.push(Action::Select { mode: None });
                actions.push(Action::Move {
                    motion: Motion::Left,
                    count: 1,
                });
            }
            if state.mode.visual() {
                actions.push(Action::Select { mode: None });
            }
            state = State::default();
        } else if state.mode == Mode::Insert {
            handled = false;
        } else if key.control {
            state.pending = None;
            if matches!(key.key.as_str(), "v" | "q") {
                state.mode = if state.mode == Mode::VisualBlock {
                    Mode::Normal
                } else {
                    Mode::VisualBlock
                };
                actions.push(Action::Select {
                    mode: (state.mode == Mode::VisualBlock).then_some(input::SelectionKind::Block),
                });
            }
            if key.key == "r" && !state.mode.visual() {
                actions.push(Action::Redo {
                    count: state.count.max(1),
                });
            }
            state.count = 0;
        } else if let Some(digit) = key
            .key
            .chars()
            .next()
            .filter(|_| key.key.len() == 1)
            .and_then(|ch| ch.to_digit(10))
            .filter(|digit| *digit != 0 || state.count != 0)
        {
            state.count = state
                .count
                .saturating_mul(10)
                .saturating_add(digit as usize)
                .min(10_000);
        } else {
            let count = state.count.max(1);
            let pending = state.pending.take();
            if pending == Some('g') && key.key == "g" {
                actions.push(Action::Move {
                    motion: Motion::Line,
                    count,
                });
            } else if pending == Some('d') && key.key == "d" {
                actions.push(Action::DeleteLines {
                    count: count
                        .saturating_mul(state.operator_count.max(1))
                        .min(10_000),
                });
            } else if pending.is_none() {
                let motion = match key.key.as_str() {
                    "h" | "ArrowLeft" | "Backspace" => Some(Motion::Left),
                    "j" | "ArrowDown" => Some(Motion::Down),
                    "k" | "ArrowUp" => Some(Motion::Up),
                    "l" | "ArrowRight" | " " => Some(Motion::Right),
                    "w" => Some(Motion::WordForward),
                    "b" => Some(Motion::WordBackward),
                    "e" => Some(Motion::WordEnd),
                    "0" | "Home" => Some(Motion::LineStart),
                    "^" => Some(Motion::FirstNonBlank),
                    "$" | "End" => Some(Motion::LineEnd),
                    "G" => Some(if state.count == 0 {
                        Motion::DocumentEnd
                    } else {
                        Motion::Line
                    }),
                    _ => None,
                };
                if let Some(motion) = motion {
                    actions.push(Action::Move { motion, count });
                } else {
                    match key.key.as_str() {
                        "v" | "V" => {
                            let target = if key.key == "v" {
                                Mode::Visual
                            } else {
                                Mode::VisualLine
                            };
                            state.mode = if state.mode == target {
                                Mode::Normal
                            } else {
                                target
                            };
                            actions.push(Action::Select {
                                mode: match state.mode {
                                    Mode::Visual => Some(input::SelectionKind::Character),
                                    Mode::VisualLine => Some(input::SelectionKind::Line),
                                    Mode::VisualBlock => Some(input::SelectionKind::Block),
                                    _ => None,
                                },
                            });
                        }
                        "d" | "x" | "Delete" if state.mode.visual() => {
                            actions.push(Action::DeleteSelection);
                            state.mode = Mode::Normal;
                        }
                        "c" if state.mode.visual() => {
                            actions.push(Action::ChangeSelection);
                            state.mode = Mode::Insert;
                        }
                        "I" | "A" if state.mode == Mode::VisualBlock => {
                            actions.push(Action::InsertAtSelections {
                                after: key.key == "A",
                            });
                            state.mode = Mode::Insert;
                        }
                        "o" if state.mode.visual() => actions.push(Action::SwapSelectionEnds),
                        "g" => state.pending = Some('g'),
                        _ if state.mode.visual() => {}
                        "d" => {
                            state.pending = Some('d');
                            state.operator_count = count;
                            state.count = 0;
                        }
                        "i" => state.mode = Mode::Insert,
                        "a" => {
                            state.mode = Mode::Insert;
                            actions.push(Action::Move {
                                motion: Motion::Right,
                                count: 1,
                            });
                        }
                        "I" => {
                            state.mode = Mode::Insert;
                            actions.push(Action::Move {
                                motion: Motion::FirstNonBlank,
                                count: 1,
                            });
                        }
                        "A" => {
                            state.mode = Mode::Insert;
                            actions.push(Action::Move {
                                motion: Motion::LineEnd,
                                count: 1,
                            });
                        }
                        "o" | "O" => {
                            state.mode = Mode::Insert;
                            actions.push(Action::InsertLine {
                                below: key.key == "o",
                            });
                        }
                        "x" | "Delete" => actions.push(Action::DeleteCharacters { count }),
                        "u" => actions.push(Action::Undo { count }),
                        _ => {}
                    }
                }
            }
            if state.pending.is_none() {
                state.count = 0;
                state.operator_count = 0;
            }
        }
        let prefix = if state.pending == Some('d') {
            format!(
                " {}d{}",
                if state.operator_count > 1 {
                    state.operator_count.to_string()
                } else {
                    String::new()
                },
                if state.count > 0 {
                    state.count.to_string()
                } else {
                    String::new()
                }
            )
        } else {
            format!(
                "{}{}",
                if state.count > 0 {
                    format!(" {}", state.count)
                } else {
                    String::new()
                },
                state
                    .pending
                    .map_or_else(String::new, |ch| format!(" {ch}"))
            )
        };
        let response = input::Response {
            handled,
            state: input::State {
                mode: state.mode.id().into(),
                label: format!("{}{prefix}", state.mode.label()),
                data: serde_json::to_string(&state).map_err(|e| e.to_string())?,
                capture_text: state.mode != Mode::Insert,
                clamp_caret: state.mode != Mode::Insert,
                control_keys: if state.mode == Mode::Insert {
                    vec![]
                } else {
                    vec!["v".into(), "q".into()]
                },
            },
            actions,
        };
        let decision = Decision::Input(response);
        decision.validate(request)?;
        Ok(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Arc};
    use velk_core::Buffer;
    use velk_editor_model::editing::Session;
    #[derive(Debug)]
    struct Provider;
    impl velk_plugin_sdk::editing::Provider for Provider {
        fn evaluate(&self, request: &Request) -> Result<Decision, String> {
            Vim::evaluate(request)
        }
    }
    fn session(buffer: &mut Buffer) -> Session {
        let mut session = Session::default();
        session.set_providers(BTreeMap::from([(
            Role::Keymap,
            Arc::new(Provider) as Arc<dyn velk_plugin_sdk::editing::Provider>,
        )]));
        press(&mut session, buffer, "", false);
        session
    }
    fn press(session: &mut Session, buffer: &mut Buffer, key: &str, control: bool) -> bool {
        session
            .evaluate(
                buffer,
                Intent::Key(input::Key {
                    key: key.into(),
                    text: !key.is_empty() && key != "Escape" && !control,
                    control,
                    alt: false,
                    meta: false,
                    state: Default::default(),
                }),
            )
            .unwrap()
    }
    fn keys(session: &mut Session, buffer: &mut Buffer, keys: &str) {
        for key in keys.chars() {
            assert!(press(session, buffer, &key.to_string(), false));
        }
    }
    #[test]
    fn normal_motions_counts_prefixes_and_line_targets() {
        let mut buffer = Buffer::new("one two three\n  four\nlast");
        let mut session = session(&mut buffer);
        keys(&mut session, &mut buffer, "2w");
        assert_eq!(buffer.selection().head, 8);
        keys(&mut session, &mut buffer, "b");
        assert_eq!(buffer.selection().head, 4);
        keys(&mut session, &mut buffer, "e");
        assert_eq!(buffer.selection().head, 6);
        keys(&mut session, &mut buffer, "0");
        assert_eq!(buffer.selection().head, 0);
        keys(&mut session, &mut buffer, "$llllllll");
        assert_eq!(buffer.selection().head, 12);
        keys(&mut session, &mut buffer, "2gg");
        assert_eq!(buffer.selection().head, 16);
        keys(&mut session, &mut buffer, "0^");
        assert_eq!(buffer.selection().head, 16);
        keys(&mut session, &mut buffer, "G");
        assert_eq!(buffer.selection().head, 21);
        keys(&mut session, &mut buffer, "gg");
        assert_eq!(buffer.selection().head, 0);
        keys(&mut session, &mut buffer, "2G");
        assert_eq!(buffer.selection().head, 16);
        assert_eq!(session.input_state().label, "NORMAL");
        assert!(!buffer.can_undo());
    }
    #[test]
    fn vertical_movement_retains_column_across_short_lines() {
        let mut buffer = Buffer::new("abcdef\nx\nabcdef");
        buffer.set_caret(5, false);
        let mut session = session(&mut buffer);
        keys(&mut session, &mut buffer, "j");
        assert_eq!(buffer.selection().head, 7);
        keys(&mut session, &mut buffer, "j");
        assert_eq!(buffer.selection().head, 14);
        keys(&mut session, &mut buffer, "kk");
        assert_eq!(buffer.selection().head, 5);
    }
    #[test]
    fn insertion_entry_points_escape_and_per_buffer_state() {
        for (key, initial, expected) in [
            ("i", 3, 3),
            ("a", 3, 4),
            ("I", 3, 2),
            ("A", 3, 5),
            ("o", 3, 6),
            ("O", 3, 0),
        ] {
            let mut buffer = Buffer::new("  abc\ndef");
            buffer.set_caret(initial, false);
            let mut session = session(&mut buffer);
            keys(&mut session, &mut buffer, key);
            assert_eq!(session.input_state().label, "INSERT", "{key}");
            assert_eq!(buffer.selection().head, expected, "{key}");
            assert!(!press(&mut session, &mut buffer, "X", false));
            buffer.insert("X");
            assert_eq!(session.input_state().label, "INSERT");
            press(&mut session, &mut buffer, "Escape", false);
            assert_eq!(session.input_state().label, "NORMAL");
            assert_eq!(buffer.selection().head, expected);
        }
        let mut a = Buffer::new("a");
        let mut b = Buffer::new("b");
        let mut sa = session(&mut a);
        let sb = session(&mut b);
        keys(&mut sa, &mut a, "i");
        assert_eq!(sa.input_state().label, "INSERT");
        assert_eq!(sb.input_state().label, "NORMAL");
        sa.set_providers(Default::default());
        assert!(sa.input_state().label.is_empty());
        assert!(!press(&mut sa, &mut a, "x", false));
    }
    #[test]
    fn edits_use_graphemes_stay_on_lines_and_undo_atomically() {
        let mut buffer = Buffer::new("e\u{301}界z\nnext");
        let mut session = session(&mut buffer);
        keys(&mut session, &mut buffer, "lh");
        assert_eq!(buffer.selection().head, 0);
        keys(&mut session, &mut buffer, "2x");
        assert_eq!(buffer.text().to_string(), "z\nnext");
        keys(&mut session, &mut buffer, "u");
        assert_eq!(buffer.text().to_string(), "e\u{301}界z\nnext");
        assert_eq!(buffer.selection().head, 0);
        press(&mut session, &mut buffer, "r", true);
        assert_eq!(buffer.text().to_string(), "z\nnext");
        keys(&mut session, &mut buffer, "999x");
        assert_eq!(buffer.text().to_string(), "\nnext");
        let mut buffer = Buffer::new("first\nsecond\nthird");
        buffer.set_caret(6, false);
        let mut session = self::session(&mut buffer);
        keys(&mut session, &mut buffer, "2dd");
        assert_eq!(buffer.text().to_string(), "first");
        keys(&mut session, &mut buffer, "u");
        assert_eq!(buffer.text().to_string(), "first\nsecond\nthird");
        assert!(!buffer.can_undo());
    }
    #[test]
    fn doubled_operator_counts_multiply_and_meta_keys_preserve_insert_mode() {
        let mut buffer = Buffer::new("1\n2\n3\n4\n5\n6\n7");
        let mut session = session(&mut buffer);
        keys(&mut session, &mut buffer, "2d");
        assert_eq!(session.input_state().label, "NORMAL 2d");
        assert_eq!(session.input_state().mode, "normal");
        keys(&mut session, &mut buffer, "3d");
        assert_eq!(buffer.text().to_string(), "7");
        keys(&mut session, &mut buffer, "ui");
        let mut event = input::Key {
            key: "p".into(),
            text: false,
            control: false,
            alt: false,
            meta: true,
            state: Default::default(),
        };
        assert!(
            !session
                .evaluate(&mut buffer, Intent::Key(event.clone()))
                .unwrap()
        );
        assert_eq!(session.input_state().label, "INSERT");
        event.key = "Escape".into();
        event.meta = false;
        event.text = true;
        assert!(!session.evaluate(&mut buffer, Intent::Key(event)).unwrap());
        assert_eq!(session.input_state().label, "INSERT");
    }
    #[test]
    fn word_motions_advance_past_combining_and_joined_graphemes() {
        let mut buffer = Buffer::new("e\u{301} x\u{301} \u{1f469}\u{200d}\u{1f4bb} z");
        let mut session = session(&mut buffer);
        keys(&mut session, &mut buffer, "e");
        assert_eq!(buffer.selection().head, 3);
        keys(&mut session, &mut buffer, "e");
        assert_eq!(buffer.selection().head, 6);
        keys(&mut session, &mut buffer, "w");
        assert_eq!(buffer.selection().head, 10);
        keys(&mut session, &mut buffer, "b");
        assert_eq!(buffer.selection().head, 6);
    }
    #[test]
    fn visual_selection_is_inclusive_and_preserves_the_logical_cursor() {
        let mut buffer = Buffer::new("abcde");
        buffer.set_caret(2, false);
        let mut session = session(&mut buffer);
        keys(&mut session, &mut buffer, "v");
        assert_eq!(session.input_state().label, "VISUAL");
        assert_eq!(buffer.selected_text(), "c");
        assert_eq!(session.caret_position(&buffer), 2);
        keys(&mut session, &mut buffer, "2h");
        assert_eq!(buffer.selected_text(), "abc");
        assert_eq!(session.caret_position(&buffer), 0);
        assert!(buffer.selection().anchor > buffer.selection().head);
        keys(&mut session, &mut buffer, "4l");
        assert_eq!(buffer.selected_text(), "cde");
        assert_eq!(session.caret_position(&buffer), 4);
        keys(&mut session, &mut buffer, "o");
        assert_eq!(buffer.selected_text(), "cde");
        assert_eq!(session.caret_position(&buffer), 2);
        press(&mut session, &mut buffer, "Escape", false);
        assert!(buffer.selection().is_empty());
        assert_eq!(buffer.selection().head, 2);
        assert_eq!(session.input_state().label, "NORMAL");
        assert!(!buffer.can_undo());
    }
    #[test]
    fn visual_graphemes_delete_and_change_form_single_undo_steps() {
        let mut buffer = Buffer::new("e\u{301}界z");
        let mut session = session(&mut buffer);
        keys(&mut session, &mut buffer, "v");
        assert_eq!(buffer.selected_text(), "e\u{301}");
        keys(&mut session, &mut buffer, "ld");
        assert_eq!(buffer.text().to_string(), "z");
        assert_eq!(session.input_state().label, "NORMAL");
        keys(&mut session, &mut buffer, "u");
        assert_eq!(buffer.text().to_string(), "e\u{301}界z");
        assert!(buffer.selection().is_empty());
        assert!(!buffer.can_undo());
        keys(&mut session, &mut buffer, "v$c");
        assert_eq!(buffer.text().to_string(), "");
        assert_eq!(session.input_state().label, "INSERT");
        assert!(!press(&mut session, &mut buffer, "replacement", false));
        buffer.insert("replacement");
        assert_eq!(buffer.text().to_string(), "replacement");
    }
    #[test]
    fn linewise_visual_switches_kind_extends_and_deletes_eof_without_a_blank_line() {
        let mut buffer = Buffer::new("one\ntwo\nthree");
        buffer.set_caret(5, false);
        let mut session = session(&mut buffer);
        keys(&mut session, &mut buffer, "V");
        assert_eq!(buffer.selected_text(), "two\n");
        assert_eq!(session.input_state().label, "V-LINE");
        assert_eq!(session.caret_position(&buffer), 5);
        keys(&mut session, &mut buffer, "j");
        assert_eq!(buffer.selected_text(), "two\nthree");
        keys(&mut session, &mut buffer, "v");
        assert_eq!(buffer.selected_text(), "wo\nth");
        keys(&mut session, &mut buffer, "Vd");
        assert_eq!(buffer.text().to_string(), "one");
        keys(&mut session, &mut buffer, "u");
        assert_eq!(buffer.text().to_string(), "one\ntwo\nthree");
        assert!(!buffer.can_undo());
    }
    #[test]
    fn linewise_change_keeps_a_line_and_visual_toggles_do_not_edit() {
        let mut buffer = Buffer::new("first\nsecond\nthird");
        buffer.set_caret(8, false);
        let mut session = session(&mut buffer);
        keys(&mut session, &mut buffer, "VV");
        assert!(buffer.selection().is_empty());
        keys(&mut session, &mut buffer, "vv");
        assert!(buffer.selection().is_empty());
        assert!(!buffer.can_undo());
        keys(&mut session, &mut buffer, "Vc");
        assert_eq!(buffer.text().to_string(), "first\n\nthird");
        assert_eq!(buffer.selection().head, 6);
        assert_eq!(session.input_state().label, "INSERT");
    }
    #[test]
    fn visual_vertical_motions_keep_preferred_column_and_empty_lines_are_safe() {
        let mut buffer = Buffer::new("abcdef\nx\nabcdef");
        buffer.set_caret(5, false);
        let mut session = session(&mut buffer);
        keys(&mut session, &mut buffer, "vjj");
        assert_eq!(session.caret_position(&buffer), 14);
        keys(&mut session, &mut buffer, "kk");
        assert_eq!(session.caret_position(&buffer), 5);
        let mut empty = Buffer::new("");
        let mut se = self::session(&mut empty);
        keys(&mut se, &mut empty, "vjlkd");
        assert_eq!(empty.text().to_string(), "");
        assert_eq!(se.input_state().label, "NORMAL");
    }
    #[test]
    fn linewise_selection_includes_the_final_empty_line_when_requested() {
        for (keys_to_apply, expected) in [("Vd", "one\n"), ("Vjd", "one"), ("Vjc", "one\n")] {
            let mut buffer = Buffer::new("one\ntwo\n");
            buffer.set_caret(4, false);
            let mut session = session(&mut buffer);
            keys(&mut session, &mut buffer, keys_to_apply);
            assert_eq!(buffer.text().to_string(), expected, "{keys_to_apply}");
            buffer.undo();
            assert_eq!(buffer.text().to_string(), "one\ntwo\n");
        }
    }
    #[test]
    fn external_edits_and_selections_invalidate_visual_anchors() {
        let mut buffer = Buffer::new("abc");
        let mut session = session(&mut buffer);
        keys(&mut session, &mut buffer, "vl");
        buffer.set_caret(2, false);
        session.synchronize(&buffer);
        assert!(!session.has_visual_selection());
        press(&mut session, &mut buffer, "", false);
        assert_eq!(session.input_state().label, "NORMAL");
        keys(&mut session, &mut buffer, "v");
        buffer.insert("X");
        session.synchronize(&buffer);
        assert!(!session.has_visual_selection());
        assert_eq!(session.caret_position(&buffer), buffer.selection().head);
    }
    #[test]
    fn visual_block_projects_disjoint_rows_and_deletes_atomically() {
        let mut buffer = Buffer::new("abcd\nefgh\nijkl");
        buffer.set_caret(1, false);
        let mut session = session(&mut buffer);
        press(&mut session, &mut buffer, "v", true);
        keys(&mut session, &mut buffer, "l2j");
        assert_eq!(session.input_state().label, "V-BLOCK");
        assert_eq!(session.selected_text(&buffer), "bc\nfg\njk");
        let regions = session.selection_regions().unwrap();
        assert_eq!(regions.len(), 3);
        assert_eq!(session.caret_position(&buffer), 12);
        assert!(
            regions
                .iter()
                .all(|region| region.selection.range().len() == 2)
        );
        keys(&mut session, &mut buffer, "d");
        assert_eq!(buffer.text().to_string(), "ad\neh\nil");
        keys(&mut session, &mut buffer, "u");
        assert_eq!(buffer.text().to_string(), "abcd\nefgh\nijkl");
        assert!(!buffer.can_undo());
    }
    #[test]
    fn block_columns_count_graphemes_clip_short_lines_and_treat_tabs_as_one_column() {
        let mut buffer = Buffer::new("a\u{301}bc\nx\n\t界z");
        buffer.set_caret(2, false);
        let mut session = session(&mut buffer);
        press(&mut session, &mut buffer, "q", true);
        keys(&mut session, &mut buffer, "l2j");
        assert_eq!(session.selected_text(&buffer), "bc\n\n界z");
        let regions = session.selection_regions().unwrap();
        assert!(regions[1].selection.is_empty());
        keys(&mut session, &mut buffer, "o");
        assert_eq!(session.selected_text(&buffer), "bc\n\n界z");
        keys(&mut session, &mut buffer, "x");
        assert_eq!(buffer.text().to_string(), "a\u{301}\nx\n\t");
    }
    #[test]
    fn block_change_and_insert_apply_to_every_caret_and_escape_keeps_one() {
        use velk_editor_model::EditCommand;
        for (entry, expected) in [
            ("c", "a!d\ne!h"),
            ("I", "a!bcd\ne!fgh"),
            ("A", "abc!d\nefg!h"),
        ] {
            let mut buffer = Buffer::new("abcd\nefgh");
            buffer.set_caret(1, false);
            let mut session = session(&mut buffer);
            press(&mut session, &mut buffer, "v", true);
            keys(&mut session, &mut buffer, "lj");
            keys(&mut session, &mut buffer, entry);
            assert_eq!(session.input_state().label, "INSERT");
            assert!(session.has_multiple_carets());
            assert!(!press(&mut session, &mut buffer, "!", false));
            assert!(
                session
                    .edit_command(&mut buffer, &EditCommand::Insert("!"))
                    .unwrap()
            );
            assert_eq!(buffer.text().to_string(), expected, "{entry}");
            assert!(
                session
                    .edit_command(&mut buffer, &EditCommand::Backspace)
                    .unwrap()
            );
            assert!(!buffer.text().to_string().contains('!'));
            press(&mut session, &mut buffer, "Escape", false);
            assert!(!session.has_multiple_carets());
            assert!(session.selection_regions().is_none());
            assert_eq!(session.input_state().label, "NORMAL");
            keys(&mut session, &mut buffer, "u");
            assert_eq!(buffer.text().to_string(), expected);
        }
    }
    #[test]
    fn block_paste_distributes_matching_rows_and_undo_restores_the_document() {
        use velk_editor_model::EditCommand;
        let mut buffer = Buffer::new("abcd\nefgh");
        buffer.set_caret(1, false);
        let mut session = session(&mut buffer);
        press(&mut session, &mut buffer, "v", true);
        keys(&mut session, &mut buffer, "lj");
        assert!(
            session
                .edit_command(&mut buffer, &EditCommand::Paste("12\n34"))
                .unwrap()
        );
        assert_eq!(buffer.text().to_string(), "a12d\ne34h");
        buffer.undo();
        assert_eq!(buffer.text().to_string(), "abcd\nefgh");
        assert!(!buffer.can_undo());
    }
    #[test]
    fn block_switching_cancellation_and_row_limits_preserve_the_original_selection() {
        let mut buffer = Buffer::new("abc\ndef\nghi");
        let mut session = session(&mut buffer);
        press(&mut session, &mut buffer, "v", true);
        keys(&mut session, &mut buffer, "lj");
        keys(&mut session, &mut buffer, "v");
        assert_eq!(session.input_state().label, "VISUAL");
        assert!(session.selection_regions().is_none());
        press(&mut session, &mut buffer, "q", true);
        assert!(session.selection_regions().is_some());
        press(&mut session, &mut buffer, "q", true);
        assert!(buffer.selection().is_empty());
        assert_eq!(session.input_state().label, "NORMAL");
        let mut large = Buffer::new(&"x\n".repeat(4100));
        let mut sl = self::session(&mut large);
        press(&mut sl, &mut large, "v", true);
        let before = large.selection();
        let result = sl.evaluate(
            &mut large,
            Intent::Key(input::Key {
                key: "G".into(),
                text: true,
                control: false,
                alt: false,
                meta: false,
                state: Default::default(),
            }),
        );
        assert!(result.is_err());
        assert_eq!(large.selection(), before);
        assert_eq!(sl.selection_regions().unwrap().len(), 1);
        assert_eq!(sl.input_state().label, "V-BLOCK");
    }
    #[test]
    fn unknown_keys_and_escape_cancel_prefixes_and_large_lines_remain_navigable() {
        let mut buffer = Buffer::new(&"x".repeat(MAX_TEXT + 1));
        let mut session = session(&mut buffer);
        keys(&mut session, &mut buffer, "12g");
        assert_eq!(session.input_state().label, "NORMAL 12 g");
        press(&mut session, &mut buffer, "Escape", false);
        assert_eq!(session.input_state().label, "NORMAL");
        keys(&mut session, &mut buffer, "dz");
        assert_eq!(session.input_state().label, "NORMAL");
        keys(&mut session, &mut buffer, "w");
        assert_eq!(buffer.selection().head, MAX_TEXT);
        keys(&mut session, &mut buffer, "99999999999999999");
        assert!(session.input_state().label.contains("10000"));
        assert!(!buffer.can_undo());
    }
}
