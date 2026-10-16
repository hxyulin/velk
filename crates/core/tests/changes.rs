use velk_core::{Buffer, TextPoint};

#[test]
fn unicode_multiline_changes_keep_byte_coordinates_through_undo_and_redo() {
    let mut buffer = Buffer::new("é\nabc");
    buffer.set_caret(3, false);
    buffer.insert("λ\n中");
    let change = buffer.edits_since(0).unwrap().remove(0);
    assert_eq!(change.replaced, 3..3);
    assert_eq!(change.inserted, 3);
    assert_eq!(
        change.start,
        TextPoint {
            byte: 4,
            line: 1,
            column: 1,
        }
    );
    assert_eq!(change.old_end, change.start);
    assert_eq!(
        change.new_end,
        TextPoint {
            byte: 10,
            line: 2,
            column: 3,
        }
    );
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "é\nabc");
    assert_eq!(buffer.edits_since(1).unwrap(), vec![change.inverse(2)]);
    buffer.redo();
    let mut expected = change;
    expected.revision = 3;
    assert_eq!(buffer.edits_since(2).unwrap(), vec![expected]);
}

#[test]
fn restores_existing_v1_sessions_without_a_view_or_parser() {
    let bytes = br#"{
        "version": 1,
        "text": "a",
        "selection": [1, 1],
        "state": 1,
        "revision": 1,
        "next_state": 2,
        "saved_state": 0,
        "goal_column": null,
        "undo": [{
            "start": 0, "removed": 1, "text": "",
            "selection": [0, 0], "state": 0
        }],
        "redo": []
    }"#;
    let mut buffer = Buffer::restore(bytes).unwrap();
    assert!(buffer.is_dirty());
    buffer.undo();
    assert_eq!(buffer.text().len_chars(), 0);
    assert!(!buffer.is_dirty());
    buffer.redo();
    assert_eq!(buffer.text().to_string(), "a");
    assert!(buffer.is_dirty());
    let restored = Buffer::restore(&buffer.snapshot().encode().unwrap()).unwrap();
    assert_eq!(restored.text(), buffer.text());
    assert_eq!(restored.selection(), buffer.selection());
}
