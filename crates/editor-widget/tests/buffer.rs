#[test]
fn external_selection_encloses_graphemes_without_edits_or_undo() {
    let mut editor = velk_editor_widget::Editor::new("é 👩‍💻");
    assert!(editor.select_range(0..1));
    assert_eq!(editor.buffer().selection().range(), 0..2);
    assert!(editor.select_range(4..5));
    assert_eq!(editor.buffer().selection().range(), 3..6);
    assert!(editor.select_range(4..4));
    assert_eq!(editor.buffer().selection().range(), 3..3);
    let selected = editor.buffer().selection();
    assert!(!editor.select_range(0..99));
    assert_eq!(editor.buffer().selection(), selected);
    assert!(!editor.buffer().is_dirty());
    assert!(!editor.buffer().can_undo());
    assert_eq!(editor.buffer().revision(), 0);
}
