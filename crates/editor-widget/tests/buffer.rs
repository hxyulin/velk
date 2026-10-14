use velk_editor_widget::{Buffer, Selection};

#[test]
fn empty_document_edits_and_boundary_deletes_are_safe() {
    let mut b = Buffer::default();
    b.backspace();
    b.delete();
    b.undo();
    b.redo();
    assert_eq!(b.revision(), 0);
    b.insert("hello\r\n世界\r!");
    assert_eq!(b.text().to_string(), "hello\n世界\n!");
    assert_eq!(b.selection().head, 10);
    b.document_edge(false, false);
    b.delete();
    assert_eq!(b.text().to_string(), "ello\n世界\n!");
}

#[test]
fn movement_and_deletion_preserve_combining_marks_and_emoji() {
    let mut b = Buffer::new("a\u{301}👩‍💻🇭🇰z");
    let end = b.text().len_chars();
    b.document_edge(true, false);
    b.horizontal(false, false);
    assert_eq!(b.selection().head, end - 1);
    b.backspace();
    assert_eq!(b.text().to_string(), "a\u{301}👩‍💻z");
    b.backspace();
    assert_eq!(b.text().to_string(), "a\u{301}z");
    b.backspace();
    assert_eq!(b.text().to_string(), "z");
    b.delete();
    assert_eq!(b.text().len_chars(), 0);
}

#[test]
fn graphemes_spanning_rope_chunks_are_indivisible() {
    let grapheme = format!("a{}", "\u{301}".repeat(3000));
    let mut b = Buffer::new(&format!("{grapheme}z"));
    assert!(b.text().chunks().count() > 1);
    b.horizontal(true, false);
    assert_eq!(b.selection().head, 3001);
    b.backspace();
    assert_eq!(b.text().to_string(), "z");
}

#[test]
fn external_carets_are_clamped_and_snapped() {
    let mut b = Buffer::new("e\u{301}x");
    b.set_caret(1, false);
    assert_eq!(b.selection().head, 0);
    b.set_caret(usize::MAX, false);
    assert_eq!(b.selection().head, 3);
}

#[test]
fn replacing_a_backward_selection_and_undo_restore_direction() {
    let mut b = Buffer::new("abc世界");
    b.document_edge(true, false);
    b.set_caret(2, true);
    assert_eq!(b.selected_text(), "c世界");
    let selection = b.selection();
    b.insert("🐱");
    assert_eq!(b.text().to_string(), "ab🐱");
    b.undo();
    assert_eq!(b.text().to_string(), "abc世界");
    assert_eq!(b.selection(), selection);
    b.redo();
    assert_eq!(b.text().to_string(), "ab🐱");
    assert_eq!(b.selection(), Selection { anchor: 3, head: 3 });
}

#[test]
fn editing_after_undo_discards_the_redo_branch() {
    let mut b = Buffer::default();
    b.insert("a");
    b.insert("b");
    b.undo();
    b.insert("c");
    assert!(!b.can_redo());
    b.redo();
    assert_eq!(b.text().to_string(), "ac");
}

#[test]
fn line_navigation_retains_grapheme_column_through_short_lines() {
    let mut b = Buffer::new("abcde\nx\ne\u{301}12345\n");
    b.set_caret(4, false);
    b.vertical(true, false);
    assert_eq!(b.selection().head, 7);
    b.vertical(true, true);
    assert_eq!(b.selection().head, 13);
    assert_eq!(b.selection().anchor, 7);
    b.vertical(false, false);
    b.vertical(false, false);
    assert_eq!(b.selection().head, 4);
    b.line_edge(true, false);
    assert_eq!(b.selection().head, 5);
}

#[test]
fn newline_navigation_and_joining_lines() {
    let mut b = Buffer::new("a\n\nb\n");
    b.set_caret(2, false);
    b.backspace();
    assert_eq!(b.text().to_string(), "a\nb\n");
    b.horizontal(true, false);
    assert_eq!(b.selection().head, 2);
    b.line_edge(true, false);
    assert_eq!(b.selection().head, 3);
    b.delete();
    assert_eq!(b.text().to_string(), "a\nb");
}

#[test]
fn insertions_and_deletions_that_join_graphemes_keep_valid_carets() {
    let mut b = Buffer::new("\u{301}x");
    b.insert("e");
    assert_eq!(b.selection().head, 2);
    b.backspace();
    assert_eq!(b.text().to_string(), "x");
    let mut b = Buffer::new("e\n\u{301}");
    b.set_caret(1, false);
    b.delete();
    assert_eq!(b.text().to_string(), "e\u{301}");
    assert_eq!(b.selection().head, 2);
}

#[test]
fn collapsing_a_selection_does_not_skip_an_extra_grapheme() {
    let mut b = Buffer::new("abcd");
    b.set_caret(1, false);
    b.set_caret(3, true);
    b.horizontal(false, false);
    assert_eq!(b.selection().head, 1);
    b.set_caret(3, true);
    b.horizontal(true, false);
    assert_eq!(b.selection().head, 3);
}

#[test]
fn history_is_bounded_and_rope_snapshots_remain_unchanged() {
    let mut b = Buffer::new("original");
    let snapshot = b.text().clone();
    b.document_edge(true, false);
    for _ in 0..150 {
        b.insert("x");
    }
    for _ in 0..150 {
        b.undo();
    }
    assert_eq!(b.text().len_chars(), 8 + 22);
    assert_eq!(snapshot.to_string(), "original");
}
