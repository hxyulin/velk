use velk_core::Buffer;
use velk_editor_model::{
    EditCommand,
    commands::{Key, Modifiers, Platform, key_command},
};

#[test]
fn platform_navigation_and_selection_do_not_edit_text_or_history() {
    for (platform, modifiers, key, expected) in [
        (
            Platform::Mac,
            Modifiers {
                meta: true,
                shift: true,
                ..Modifiers::default()
            },
            Key::Right,
            8,
        ),
        (
            Platform::Other,
            Modifiers {
                control: true,
                shift: true,
                ..Modifiers::default()
            },
            Key::End,
            13,
        ),
    ] {
        let mut buffer = Buffer::new("one\nword\nlast");
        buffer.set_caret(5, false);
        key_command(key, modifiers, platform)
            .unwrap()
            .apply(&mut buffer);
        assert_eq!(buffer.selection().anchor, 5);
        assert_eq!(buffer.selection().head, expected);
        assert_eq!(buffer.revision(), 0);
        assert!(!buffer.can_undo());
    }
    for platform in [Platform::Mac, Platform::Other] {
        assert_eq!(
            key_command(
                Key::Tab,
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
                platform
            ),
            None
        );
        assert_eq!(
            key_command(
                Key::Left,
                Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
                platform
            ),
            None
        );
        assert_eq!(
            key_command(Key::Other, Modifiers::default(), platform),
            None
        );
    }
    assert_eq!(
        key_command(
            Key::Left,
            Modifiers {
                control: true,
                ..Modifiers::default()
            },
            Platform::Other
        ),
        None
    );
    assert_eq!(
        key_command(
            Key::Left,
            Modifiers {
                control: true,
                ..Modifiers::default()
            },
            Platform::Mac
        ),
        None
    );
}

#[test]
fn text_commands_preserve_unicode_history_and_normalization() {
    let mut buffer = Buffer::new("e\u{301}λ");
    EditCommand::DocumentEdge {
        end: true,
        extend: false,
    }
    .apply(&mut buffer);
    EditCommand::Backspace.apply(&mut buffer);
    EditCommand::Backspace.apply(&mut buffer);
    assert_eq!(buffer.text().len_chars(), 0);
    EditCommand::Undo.apply(&mut buffer);
    assert_eq!(buffer.text().to_string(), "e\u{301}");
    EditCommand::Redo.apply(&mut buffer);
    EditCommand::Insert("a\r\nb").apply(&mut buffer);
    assert_eq!(buffer.text().to_string(), "a\nb");
    EditCommand::ReplaceAll("new").apply(&mut buffer);
    EditCommand::Undo.apply(&mut buffer);
    assert_eq!(buffer.text().to_string(), "a\nb");
}

#[test]
fn external_ranges_enclose_graphemes_and_reject_invalid_offsets_atomically() {
    let mut buffer = Buffer::new("e\u{301}λ");
    assert!(EditCommand::SelectRange(0..1).apply(&mut buffer));
    assert_eq!(buffer.selection().range(), 0..2);
    let selection = buffer.selection();
    assert!(!EditCommand::SelectRange(0..99).apply(&mut buffer));
    assert_eq!(buffer.selection(), selection);
    assert_eq!(buffer.revision(), 0);
    assert!(EditCommand::Delete.apply(&mut buffer));
    assert_eq!(buffer.text().to_string(), "λ");
    EditCommand::Undo.apply(&mut buffer);
    assert_eq!(buffer.text().to_string(), "e\u{301}λ");
}
