use rxui::{
    ElementId, Entity, KeyEvent, Modifiers, Runtime, TextInputEvent, TextMeasure, TextRequest, Ui,
    UiError, prelude::*,
};
use velk_editor_widget::Editor;

struct Measure;
impl TextMeasure for Measure {
    fn measure(&mut self, _: ElementId, _: TextRequest<'_>) -> Result<[f32; 2], UiError> {
        unreachable!("editor is a custom leaf")
    }
}

fn setup() -> (Runtime, Entity<Editor>, Ui<Editor>) {
    let mut runtime = Runtime::new();
    let editor = runtime.update(|cx| cx.new(|_| Editor::default()));
    let mut ui = Ui::new(&mut runtime, editor.clone()).unwrap();
    ui.prepare(&mut runtime, [640., 480.], &mut Measure)
        .unwrap();
    let id = ui
        .elements()
        .find(|element| element.key == Some(&Key::from("editor-surface")))
        .unwrap()
        .id;
    assert!(ui.focus(id));
    (runtime, editor, ui)
}

fn text(runtime: &mut Runtime, editor: &Entity<Editor>) -> String {
    runtime.update(|cx| editor.read(cx).buffer().text().to_string())
}

fn input(runtime: &mut Runtime, ui: &mut Ui<Editor>, event: TextInputEvent) {
    assert!(ui.text_input(runtime, event, &mut Measure).unwrap());
}

fn key(runtime: &mut Runtime, ui: &mut Ui<Editor>, key: KeyboardKey, modifiers: Modifiers) {
    ui.prepare(runtime, [640., 480.], &mut Measure).unwrap();
    assert!(
        ui.key(
            runtime,
            KeyEvent {
                key,
                modifiers,
                pressed: true,
                repeat: false
            }
        )
        .unwrap()
        .default_prevented
    );
}

fn primary() -> Modifiers {
    Modifiers {
        meta: cfg!(target_os = "macos"),
        control: !cfg!(target_os = "macos"),
        ..Default::default()
    }
}

#[test]
fn successive_native_text_paste_and_edit_keys_use_current_rope() {
    let (mut runtime, editor, mut ui) = setup();
    input(&mut runtime, &mut ui, TextInputEvent::Insert("é".into()));
    input(
        &mut runtime,
        &mut ui,
        TextInputEvent::Paste("\r\n👩‍💻".into()),
    );
    assert_eq!(text(&mut runtime, &editor), "é\n👩‍💻");
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Backspace,
        Modifiers::default(),
    );
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Tab,
        Modifiers::default(),
    );
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Enter,
        Modifiers::default(),
    );
    assert_eq!(text(&mut runtime, &editor), "é\n    \n");
}

#[test]
fn select_all_undo_and_redo_route_to_widget_commands() {
    let (mut runtime, editor, mut ui) = setup();
    input(&mut runtime, &mut ui, TextInputEvent::Insert("old".into()));
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Character("a".into()),
        primary(),
    );
    input(&mut runtime, &mut ui, TextInputEvent::Insert("new".into()));
    assert_eq!(text(&mut runtime, &editor), "new");
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Character("z".into()),
        primary(),
    );
    assert_eq!(text(&mut runtime, &editor), "old");
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Character("z".into()),
        Modifiers {
            shift: true,
            ..primary()
        },
    );
    assert_eq!(text(&mut runtime, &editor), "new");
}

#[test]
fn composition_is_transient_and_commit_replaces_selection_once() {
    let (mut runtime, editor, mut ui) = setup();
    input(&mut runtime, &mut ui, TextInputEvent::Insert("old".into()));
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Character("a".into()),
        primary(),
    );
    input(
        &mut runtime,
        &mut ui,
        TextInputEvent::Preedit {
            text: "ni".into(),
            cursor: Some((2, 2)),
        },
    );
    assert_eq!(text(&mut runtime, &editor), "old");
    input(&mut runtime, &mut ui, TextInputEvent::Commit("你".into()));
    assert_eq!(text(&mut runtime, &editor), "你");
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Character("z".into()),
        primary(),
    );
    assert_eq!(text(&mut runtime, &editor), "old");
}

#[test]
fn cancelled_composition_leaves_document_and_selection_untouched() {
    let (mut runtime, editor, mut ui) = setup();
    input(&mut runtime, &mut ui, TextInputEvent::Insert("old".into()));
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Character("a".into()),
        primary(),
    );
    input(
        &mut runtime,
        &mut ui,
        TextInputEvent::Preedit {
            text: "ni".into(),
            cursor: None,
        },
    );
    input(&mut runtime, &mut ui, TextInputEvent::CancelComposition);
    assert_eq!(text(&mut runtime, &editor), "old");
    input(&mut runtime, &mut ui, TextInputEvent::Insert("x".into()));
    assert_eq!(text(&mut runtime, &editor), "x");
}

#[test]
fn pointer_focus_accepts_text_after_key_dispatch() {
    let mut runtime = Runtime::new();
    let editor = runtime.update(|cx| cx.new(|_| Editor::default()));
    let mut ui = Ui::new(&mut runtime, editor.clone()).unwrap();
    ui.prepare(&mut runtime, [640., 480.], &mut Measure)
        .unwrap();
    ui.pointer(&mut runtime, rxui::PointerEvent::Pressed([100., 100.]))
        .unwrap();
    assert!(ui.focused_text_client().is_some());
    ui.pointer(&mut runtime, rxui::PointerEvent::Released([100., 100.]))
        .unwrap();
    ui.prepare(&mut runtime, [640., 480.], &mut Measure)
        .unwrap();
    let result = ui
        .key(
            &mut runtime,
            KeyEvent {
                key: KeyboardKey::Character("x".into()),
                modifiers: Modifiers::default(),
                pressed: true,
                repeat: false,
            },
        )
        .unwrap();
    assert!(!result.default_prevented);
    assert!(ui.focused_text_client().is_some());
    input(&mut runtime, &mut ui, TextInputEvent::Insert("x".into()));
    assert_eq!(text(&mut runtime, &editor), "x");
}

#[derive(Debug)]
struct IndentationProvider;
impl velk_editing::Provider for IndentationProvider {
    fn evaluate(&self, request: &velk_editing::Request) -> Result<velk_editing::Decision, String> {
        <velk_plugin_indentation::Indentation as velk_plugin_sdk::EditingBehavior>::evaluate(
            request,
        )
    }
}
#[test]
fn tab_and_shift_tab_use_the_indentation_plugin_inside_the_editor() {
    let (mut runtime, editor, mut ui) = setup();
    runtime.update(|cx| {
        editor.update(cx, |editor, _| {
            editor.set_editing_providers(std::collections::BTreeMap::from([(
                velk_editing::Role::Indentation,
                std::sync::Arc::new(IndentationProvider)
                    as std::sync::Arc<dyn velk_editing::Provider>,
            )]))
        })
    });
    input(
        &mut runtime,
        &mut ui,
        TextInputEvent::Insert("one\ntwo\nthree".into()),
    );
    runtime.update(|cx| {
        editor.update(cx, |editor, _| {
            editor.execute(velk_editor_widget::EditCommand::SelectRange(0..8))
        })
    });
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Tab,
        Modifiers::default(),
    );
    assert_eq!(text(&mut runtime, &editor), "    one\n    two\nthree");
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Tab,
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    assert_eq!(text(&mut runtime, &editor), "one\ntwo\nthree");
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Character("z".into()),
        primary(),
    );
    assert_eq!(text(&mut runtime, &editor), "    one\n    two\nthree");
}

#[test]
fn enter_uses_indentation_and_keeps_the_caret_inside_expanded_brackets() {
    let (mut runtime, editor, mut ui) = setup();
    runtime.update(|cx| {
        editor.update(cx, |editor, _| {
            editor.set_editing_providers(std::collections::BTreeMap::from([(
                velk_editing::Role::Indentation,
                std::sync::Arc::new(IndentationProvider)
                    as std::sync::Arc<dyn velk_editing::Provider>,
            )]));
            editor.set_text("{}");
            editor.execute(velk_editor_widget::EditCommand::SetCaret {
                position: 1,
                extend: false,
            });
        })
    });
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Enter,
        Modifiers::default(),
    );
    assert_eq!(text(&mut runtime, &editor), "{\n    \n}");
    input(
        &mut runtime,
        &mut ui,
        TextInputEvent::Insert("inside".into()),
    );
    assert_eq!(text(&mut runtime, &editor), "{\n    inside\n}");
}

#[derive(Debug)]
struct PairingProvider;
impl velk_editing::Provider for PairingProvider {
    fn evaluate(&self, request: &velk_editing::Request) -> Result<velk_editing::Decision, String> {
        <velk_plugin_autopairs::Autopairs as velk_plugin_sdk::EditingBehavior>::evaluate(request)
    }
}
#[test]
fn typing_uses_autopairs_but_pasting_an_opener_remains_literal() {
    let (mut runtime, editor, mut ui) = setup();
    runtime.update(|cx| {
        editor.update(cx, |editor, _| {
            editor.set_editing_providers(std::collections::BTreeMap::from([(
                velk_editing::Role::Pairing,
                std::sync::Arc::new(PairingProvider) as std::sync::Arc<dyn velk_editing::Provider>,
            )]))
        })
    });
    input(&mut runtime, &mut ui, TextInputEvent::Insert("(".into()));
    assert_eq!(text(&mut runtime, &editor), "()");
    input(&mut runtime, &mut ui, TextInputEvent::Insert("界".into()));
    assert_eq!(text(&mut runtime, &editor), "(界)");
    input(&mut runtime, &mut ui, TextInputEvent::Paste("[".into()));
    assert_eq!(text(&mut runtime, &editor), "(界[)");
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Character("z".into()),
        primary(),
    );
    assert_eq!(text(&mut runtime, &editor), "(界)");
}

#[test]
fn tracked_closers_skip_and_empty_pairs_delete_but_undo_and_existing_brackets_are_literal() {
    let (mut runtime, editor, mut ui) = setup();
    runtime.update(|cx| {
        editor.update(cx, |editor, _| {
            editor.set_editing_providers(std::collections::BTreeMap::from([(
                velk_editing::Role::Pairing,
                std::sync::Arc::new(PairingProvider) as std::sync::Arc<dyn velk_editing::Provider>,
            )]))
        })
    });
    input(&mut runtime, &mut ui, TextInputEvent::Insert("(".into()));
    input(&mut runtime, &mut ui, TextInputEvent::Insert(")".into()));
    assert_eq!(text(&mut runtime, &editor), "()");
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::ArrowLeft,
        Modifiers::default(),
    );
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Backspace,
        Modifiers::default(),
    );
    assert_eq!(text(&mut runtime, &editor), "");
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Character("z".into()),
        primary(),
    );
    assert_eq!(text(&mut runtime, &editor), "()");
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Backspace,
        Modifiers::default(),
    );
    assert_eq!(text(&mut runtime, &editor), ")");
    runtime.update(|cx| {
        editor.update(cx, |editor, _| {
            editor.set_text(")");
            editor.execute(velk_editor_widget::EditCommand::SetCaret {
                position: 0,
                extend: false,
            });
        })
    });
    input(&mut runtime, &mut ui, TextInputEvent::Insert(")".into()));
    assert_eq!(text(&mut runtime, &editor), "))");
}

#[derive(Debug)]
struct VimProvider;
impl velk_plugin_sdk::editing::Provider for VimProvider {
    fn evaluate(
        &self,
        request: &velk_plugin_sdk::editing::Request,
    ) -> Result<velk_plugin_sdk::editing::Decision, String> {
        <velk_plugin_vim::Vim as velk_plugin_sdk::EditingBehavior>::evaluate(request)
    }
}
#[test]
fn vim_keyboard_modes_compose_with_text_ime_pairs_and_native_paste() {
    use std::{collections::BTreeMap, sync::Arc};
    use velk_plugin_sdk::editing::{Provider, Role};
    let (mut runtime, editor, mut ui) = setup();
    runtime.update(|cx| {
        editor.update(cx, |editor, _| {
            editor.insert("one two\nnext");
            editor.execute(velk_editor_widget::EditCommand::SetCaret {
                position: 0,
                extend: false,
            });
            editor.set_editing_providers(BTreeMap::from([
                (Role::Keymap, Arc::new(VimProvider) as Arc<dyn Provider>),
                (
                    Role::Pairing,
                    Arc::new(PairingProvider) as Arc<dyn Provider>,
                ),
                (
                    Role::Indentation,
                    Arc::new(IndentationProvider) as Arc<dyn Provider>,
                ),
            ]));
        })
    });
    input(&mut runtime, &mut ui, TextInputEvent::Insert("w".into()));
    runtime.update(|cx| {
        assert_eq!(editor.read(cx).buffer().selection().head, 4);
        assert_eq!(editor.read(cx).input_mode(), Some("NORMAL"));
    });
    input(
        &mut runtime,
        &mut ui,
        TextInputEvent::Preedit {
            text: "漢".into(),
            cursor: None,
        },
    );
    input(&mut runtime, &mut ui, TextInputEvent::Commit("漢".into()));
    assert_eq!(text(&mut runtime, &editor), "one two\nnext");
    input(&mut runtime, &mut ui, TextInputEvent::Insert("$".into()));
    input(&mut runtime, &mut ui, TextInputEvent::Insert("a".into()));
    input(&mut runtime, &mut ui, TextInputEvent::Insert("(".into()));
    assert_eq!(text(&mut runtime, &editor), "one two()\nnext");
    input(
        &mut runtime,
        &mut ui,
        TextInputEvent::Preedit {
            text: "漢".into(),
            cursor: None,
        },
    );
    input(&mut runtime, &mut ui, TextInputEvent::Commit("漢".into()));
    assert_eq!(text(&mut runtime, &editor), "one two(漢)\nnext");
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Escape,
        Modifiers::default(),
    );
    runtime.update(|cx| assert_eq!(editor.read(cx).input_mode(), Some("NORMAL")));
    input(&mut runtime, &mut ui, TextInputEvent::Paste("hello".into()));
    assert!(text(&mut runtime, &editor).contains("hello"));
    key(
        &mut runtime,
        &mut ui,
        KeyboardKey::Character("z".into()),
        primary(),
    );
    assert!(!text(&mut runtime, &editor).contains("hello"));
    runtime.update(|cx| {
        editor.update(cx, |editor, _| {
            editor.set_editing_providers(BTreeMap::new())
        })
    });
    input(
        &mut runtime,
        &mut ui,
        TextInputEvent::Insert("plain".into()),
    );
    assert!(text(&mut runtime, &editor).contains("plain"));
}

#[derive(Debug)]
struct FailingKeymap(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl velk_plugin_sdk::editing::Provider for FailingKeymap {
    fn evaluate(
        &self,
        request: &velk_plugin_sdk::editing::Request,
    ) -> Result<velk_plugin_sdk::editing::Decision, String> {
        if self.0.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("test keymap failure".into());
        }
        <velk_plugin_vim::Vim as velk_plugin_sdk::EditingBehavior>::evaluate(request)
    }
}
#[test]
fn capturing_keymap_failure_does_not_insert_commands_and_deselection_restores_typing() {
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    let (mut runtime, editor, mut ui) = setup();
    let fail = Arc::new(AtomicBool::new(false));
    runtime.update(|cx| {
        editor.update(cx, |editor, _| {
            editor.set_editing_providers(BTreeMap::from([(
                velk_plugin_sdk::editing::Role::Keymap,
                Arc::new(FailingKeymap(fail.clone()))
                    as Arc<dyn velk_plugin_sdk::editing::Provider>,
            )]))
        })
    });
    fail.store(true, Ordering::Relaxed);
    input(&mut runtime, &mut ui, TextInputEvent::Insert("x".into()));
    assert_eq!(text(&mut runtime, &editor), "");
    runtime.update(|cx| {
        editor.update(cx, |editor, _| {
            editor.set_editing_providers(BTreeMap::new())
        })
    });
    input(&mut runtime, &mut ui, TextInputEvent::Insert("x".into()));
    assert_eq!(text(&mut runtime, &editor), "x");
}
