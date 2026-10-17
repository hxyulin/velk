use velk_core::{Buffer, HistoryLimits, Selection, TextEdit, Transaction, UndoGroup};

fn type_text(buffer: &mut Buffer, text: &str) {
    buffer.begin_undo_group(UndoGroup::Typing);
    buffer.insert(text);
    buffer.end_undo_group();
}
#[test]
fn recovery_preserves_disjoint_edit_boundaries_for_peer_carets_in_both_directions() {
    let mut buffer =
        velk_core::Buffer::with_history("abcdef", std::sync::Arc::new(velk_undo_tree::Tree));
    buffer
        .apply_transaction(Transaction {
            revision: 0,
            edits: vec![
                TextEdit {
                    range: 0..1,
                    text: "A".into(),
                },
                TextEdit {
                    range: 5..6,
                    text: "F".into(),
                },
            ],
            selection: None,
        })
        .unwrap();
    let mut restored = velk_core::Buffer::restore_with_history(
        &buffer.snapshot().encode().unwrap(),
        std::sync::Arc::new(velk_undo_tree::Tree),
    )
    .unwrap();
    let mut peer = restored.new_view();
    peer.set_caret(3, false);
    restored.undo();
    assert_eq!(restored.text().to_string(), "abcdef");
    assert_eq!(peer.selection().head, 3);
    assert_eq!(restored.edits_since(2).unwrap().len(), 2);
    let mut restored = velk_core::Buffer::restore_with_history(
        &restored.snapshot().encode().unwrap(),
        std::sync::Arc::new(velk_undo_tree::Tree),
    )
    .unwrap();
    let mut peer = restored.new_view();
    peer.set_caret(3, false);
    restored.redo();
    assert_eq!(restored.text().to_string(), "AbcdeF");
    assert_eq!(peer.selection().head, 3);
}
#[test]
fn undo_tree_keeps_alternative_futures_and_round_trips_through_atomic_disk_archives() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("undo.json");
    let mut buffer =
        velk_core::Buffer::with_history("a", std::sync::Arc::new(velk_undo_tree::Tree));
    buffer.document_edge(true, false);
    buffer.insert("b");
    let branch_b = buffer.redo_branches();
    assert!(branch_b.is_empty());
    buffer.undo();
    let b = buffer.redo_branches()[0].state;
    buffer.insert("c");
    buffer.undo();
    let branches = buffer.redo_branches();
    assert_eq!(branches.len(), 2);
    assert!(
        !branches
            .iter()
            .find(|branch| branch.state == b)
            .unwrap()
            .preferred
    );
    buffer.snapshot().write_archive(&path).unwrap();
    let expected = buffer.text();
    let mut restored = velk_core::Buffer::read_archive_with_history(
        &path,
        Some(&expected),
        std::sync::Arc::new(velk_undo_tree::Tree),
    )
    .unwrap();
    assert_eq!(restored.redo_branches(), branches);
    restored.redo();
    assert_eq!(restored.text().to_string(), "ac");
    restored.undo();
    assert!(restored.redo_branch(b));
    assert_eq!(restored.text().to_string(), "ab");
    assert!(!restored.redo_branch(u64::MAX));
    assert!(
        velk_core::Buffer::read_archive_with_history(
            &path,
            Some(&restored.text()),
            std::sync::Arc::new(velk_undo_tree::Tree)
        )
        .is_err()
    );
    restored.snapshot().write_archive(&path).unwrap();
    assert_eq!(
        velk_core::Buffer::read_archive_with_history(
            &path,
            None,
            std::sync::Arc::new(velk_undo_tree::Tree)
        )
        .unwrap()
        .text(),
        restored.text()
    );
}
#[test]
fn history_limits_prune_side_branches_then_old_ancestors_and_release_large_patches() {
    let mut buffer = velk_core::Buffer::with_history("", std::sync::Arc::new(velk_undo_tree::Tree));
    buffer.set_history_limits(HistoryLimits {
        entries: 2,
        bytes: 2048,
    });
    buffer.insert("a");
    buffer.insert("b");
    buffer.undo();
    buffer.insert("c");
    buffer.insert("d");
    buffer.undo();
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "a");
    assert!(!buffer.can_undo());
    buffer.redo();
    assert_eq!(buffer.text().to_string(), "ac");
    assert_eq!(buffer.redo_branches().len(), 1);
    buffer.insert(&"x".repeat(4096));
    assert!(buffer.history_bytes() <= 2048);
    assert!(!buffer.can_undo());
    assert!(
        velk_core::Buffer::restore_with_history(
            &buffer.snapshot().encode().unwrap(),
            std::sync::Arc::new(velk_undo_tree::Tree)
        )
        .is_ok()
    );
}
#[test]
fn typing_groups_stop_at_caret_changes_other_views_saves_and_explicit_boundaries() {
    let mut buffer = velk_core::Buffer::with_history("", std::sync::Arc::new(velk_undo_tree::Tree));
    type_text(&mut buffer, "a");
    type_text(&mut buffer, "b");
    type_text(&mut buffer, "c");
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "");
    buffer.redo();
    let saved = buffer.checkpoint();
    buffer.mark_saved(saved);
    type_text(&mut buffer, "d");
    buffer.undo();
    assert!(!buffer.is_dirty());
    buffer.redo();
    buffer.set_caret(0, false);
    type_text(&mut buffer, "X");
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "abcd");
    buffer.document_edge(true, false);
    type_text(&mut buffer, "e");
    let mut peer = buffer.new_view();
    type_text(&mut peer, "f");
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "abcde");
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "abcd");
    type_text(&mut buffer, "g");
    buffer.break_undo_group();
    type_text(&mut buffer, "h");
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "abcdg");
}
#[test]
fn malformed_history_patches_and_disconnected_branches_are_rejected() {
    let mut buffer =
        velk_core::Buffer::with_history("abc", std::sync::Arc::new(velk_undo_tree::Tree));
    buffer.insert("x");
    let bytes = buffer.snapshot().encode().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for field in ["removed", "inserted"] {
        let mut value = value.clone();
        value["history"]["data"]["entries"]["1"]["patches"][0][field] = "incorrect".into();
        assert!(
            velk_core::Buffer::restore_with_history(
                &serde_json::to_vec(&value).unwrap(),
                std::sync::Arc::new(velk_undo_tree::Tree)
            )
            .is_err()
        );
    }
    let mut oversized = value.clone();
    oversized["history"]["data"]["entries"]["1"]["patches"][0]["change"]["inserted"] =
        serde_json::json!(u64::MAX);
    assert!(
        velk_core::Buffer::restore_with_history(
            &serde_json::to_vec(&oversized).unwrap(),
            std::sync::Arc::new(velk_undo_tree::Tree)
        )
        .is_err()
    );
    let mut value = value.clone();
    value["history"]["data"]["entries"]["1"]["parent"] = 1.into();
    assert!(
        velk_core::Buffer::restore_with_history(
            &serde_json::to_vec(&value).unwrap(),
            std::sync::Arc::new(velk_undo_tree::Tree)
        )
        .is_err()
    );
}
#[test]
fn grouped_history_round_trip_retains_exact_edits_and_saved_checkpoint() {
    let mut buffer = velk_core::Buffer::with_history("", std::sync::Arc::new(velk_undo_tree::Tree));
    type_text(&mut buffer, "a");
    type_text(&mut buffer, "b");
    let mut restored = velk_core::Buffer::restore_with_history(
        &buffer.snapshot().encode().unwrap(),
        std::sync::Arc::new(velk_undo_tree::Tree),
    )
    .unwrap();
    assert_eq!(restored.selection(), Selection { anchor: 2, head: 2 });
    restored.undo();
    assert_eq!(restored.text().to_string(), "");
    assert!(!restored.is_dirty());
    restored.redo();
    assert_eq!(restored.text().to_string(), "ab");
}

#[test]
fn pruning_the_preferred_branch_falls_back_to_a_surviving_redo_child() {
    let mut buffer = velk_core::Buffer::with_history("", std::sync::Arc::new(velk_undo_tree::Tree));
    buffer.insert("A");
    buffer.undo();
    let branch_a = buffer.redo_branches()[0].state;
    buffer.insert("B");
    buffer.undo();
    assert!(buffer.redo_branch(branch_a));
    buffer.undo();
    buffer.set_history_limits(HistoryLimits {
        entries: 1,
        bytes: 4096,
    });
    assert!(buffer.can_redo());
    buffer.redo();
    assert_eq!(buffer.text().to_string(), "B");
    assert!(
        velk_core::Buffer::restore_with_history(
            &buffer.snapshot().encode().unwrap(),
            std::sync::Arc::new(velk_undo_tree::Tree)
        )
        .is_ok()
    );
}

#[test]
fn large_document_history_and_encoding_stay_proportional_to_the_edits() {
    let text = "a".repeat(1024 * 1024);
    let mut buffer =
        velk_core::Buffer::with_history(&text, std::sync::Arc::new(velk_undo_tree::Tree));
    for position in 0..128 {
        buffer.set_caret(position * 8192, false);
        buffer.insert("x");
    }
    assert!(buffer.history_bytes() < 64 * 1024);
    let bytes = buffer.snapshot().encode().unwrap();
    assert!(bytes.len() < text.len() + 128 * 1024);
    let mut restored =
        velk_core::Buffer::restore_with_history(&bytes, std::sync::Arc::new(velk_undo_tree::Tree))
            .unwrap();
    for _ in 0..128 {
        restored.undo();
    }
    assert_eq!(restored.text().to_string(), text);
}

#[test]
fn extending_a_typing_group_does_not_mutate_an_in_flight_snapshot() {
    let mut buffer = velk_core::Buffer::with_history("", std::sync::Arc::new(velk_undo_tree::Tree));
    type_text(&mut buffer, "a");
    let snapshot = buffer.snapshot();
    type_text(&mut buffer, "b");
    let mut earlier = velk_core::Buffer::restore_with_history(
        &snapshot.encode().unwrap(),
        std::sync::Arc::new(velk_undo_tree::Tree),
    )
    .unwrap();
    assert_eq!(earlier.text().to_string(), "a");
    earlier.undo();
    assert_eq!(earlier.text().to_string(), "");
    assert_eq!(buffer.text().to_string(), "ab");
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "");
}
