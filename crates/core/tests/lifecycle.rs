use velk_core::lifecycle::{
    Activity, CloseAction, CloseBlock, CloseIntent, CloseResponse, DocumentState, Lifecycle,
};
fn document(id: u64, dirty: bool) -> DocumentState {
    DocumentState {
        id,
        dirty,
        composing: false,
        saving: false,
    }
}

#[test]
fn cancelling_later_window_prompt_retains_all_documents_and_resets_approvals_on_retry() {
    let documents = [document(1, true), document(2, true)];
    let mut lifecycle = Lifecycle::default();
    assert_eq!(
        lifecycle.request_close(CloseIntent::Quit, &documents, Activity::default()),
        CloseAction::Prompt(1)
    );
    assert_eq!(
        lifecycle.respond(1, CloseResponse::Discard, &documents),
        CloseAction::Prompt(2)
    );
    assert_eq!(
        lifecycle.respond(2, CloseResponse::Cancel, &documents),
        CloseAction::Ignore
    );
    assert!(lifecycle.close_intent().is_none());
    assert_eq!(
        lifecycle.request_close(CloseIntent::Quit, &documents, Activity::default()),
        CloseAction::Prompt(1)
    );
    assert_eq!(
        lifecycle.respond(1, CloseResponse::Discard, &documents),
        CloseAction::Prompt(2)
    );
    assert_eq!(
        lifecycle.respond(2, CloseResponse::Discard, &documents),
        CloseAction::Finish(CloseIntent::Quit)
    );
    assert!(documents.iter().all(|d| d.dirty));
}

#[test]
fn responses_are_bound_to_prompted_document_and_duplicates_are_ignored() {
    let mut lifecycle = Lifecycle::default();
    let documents = [document(1, false), document(2, true)];
    assert_eq!(
        lifecycle.request_close(CloseIntent::Close, &documents, Activity::default()),
        CloseAction::Prompt(2)
    );
    assert_eq!(
        lifecycle.respond(1, CloseResponse::Discard, &documents),
        CloseAction::Ignore
    );
    assert_eq!(
        lifecycle.respond(2, CloseResponse::Save, &documents),
        CloseAction::Save {
            id: 2,
            then: CloseIntent::Close
        }
    );
    assert_eq!(
        lifecycle.respond(2, CloseResponse::Discard, &documents),
        CloseAction::Ignore
    );
}

#[test]
fn saving_an_older_snapshot_keeps_newer_edits_open() {
    let mut lifecycle = Lifecycle::default();
    let mut documents = [document(7, true)];
    assert_eq!(
        lifecycle.request_close(CloseIntent::Buffer, &documents, Activity::default()),
        CloseAction::Prompt(7)
    );
    assert_eq!(
        lifecycle.respond(7, CloseResponse::Save, &documents),
        CloseAction::Save {
            id: 7,
            then: CloseIntent::Buffer
        }
    );
    assert_eq!(
        lifecycle.saved(CloseIntent::Buffer, &documents),
        CloseAction::Blocked(CloseBlock::NewerEdits)
    );
    assert!(lifecycle.close_intent().is_none());
    documents[0].dirty = false;
    assert_eq!(
        lifecycle.saved(CloseIntent::Buffer, &documents),
        CloseAction::Remove(7)
    );
}

#[test]
fn closing_last_buffer_does_not_close_the_window_and_empty_workspace_can_exit() {
    let mut lifecycle = Lifecycle::default();
    assert_eq!(
        lifecycle.request_close(
            CloseIntent::Buffer,
            &[document(1, false)],
            Activity::default()
        ),
        CloseAction::Remove(1)
    );
    assert_eq!(
        lifecycle.request_close(CloseIntent::Buffer, &[], Activity::default()),
        CloseAction::Ignore
    );
    assert_eq!(
        lifecycle.request_close(CloseIntent::Close, &[], Activity::default()),
        CloseAction::Finish(CloseIntent::Close)
    );
    assert_eq!(
        lifecycle.request_close(
            CloseIntent::Quit,
            &[],
            Activity {
                opening: true,
                ..Activity::default()
            }
        ),
        CloseAction::Ignore
    );
}

#[test]
fn close_waits_for_composition_and_file_operations() {
    let mut lifecycle = Lifecycle::default();
    let mut documents = [document(1, true)];
    documents[0].composing = true;
    assert_eq!(
        lifecycle.request_close(CloseIntent::Buffer, &documents, Activity::default()),
        CloseAction::Blocked(CloseBlock::Composition)
    );
    documents[0].composing = false;
    documents[0].saving = true;
    assert_eq!(
        lifecycle.request_close(
            CloseIntent::Buffer,
            &documents,
            Activity {
                busy: true,
                saving: true,
                ..Activity::default()
            }
        ),
        CloseAction::Blocked(CloseBlock::Saving)
    );
    documents[0].saving = false;
    assert_eq!(
        lifecycle.request_close(
            CloseIntent::Close,
            &documents,
            Activity {
                opening: true,
                ..Activity::default()
            }
        ),
        CloseAction::Blocked(CloseBlock::FileOperations)
    );
    assert!(lifecycle.close_intent().is_none());
}

#[test]
fn save_all_keeps_order_skips_clean_and_closed_documents_and_stops_when_cancelled() {
    let mut lifecycle = Lifecycle::default();
    let documents = [document(3, true), document(1, false), document(4, true)];
    assert!(lifecycle.begin_save_all([1, 2, 3, 4], Activity::default()));
    assert!(!lifecycle.begin_save_all([9], Activity::default()));
    assert_eq!(lifecycle.next_save(&documents), Some(3));
    assert_eq!(
        lifecycle.request_close(CloseIntent::Close, &documents, Activity::default()),
        CloseAction::Blocked(CloseBlock::FileOperations)
    );
    lifecycle.cancel_save_all();
    assert_eq!(lifecycle.next_save(&documents), None);
    assert!(!lifecycle.begin_save_all(
        [3],
        Activity {
            saving: true,
            ..Activity::default()
        }
    ));
    assert!(lifecycle.begin_save_all([4, 3], Activity::default()));
    assert_eq!(lifecycle.next_save(&documents), Some(4));
    assert_eq!(lifecycle.next_save(&documents), Some(3));
    assert_eq!(lifecycle.next_save(&documents), None);
}

#[test]
fn composition_in_a_later_document_cannot_leave_the_workspace_stuck_closing() {
    let mut documents = [document(1, true), document(2, true)];
    documents[1].composing = true;
    let mut lifecycle = Lifecycle::default();
    assert_eq!(
        lifecycle.request_close(CloseIntent::Close, &documents, Activity::default()),
        CloseAction::Prompt(1)
    );
    assert_eq!(
        lifecycle.respond(1, CloseResponse::Discard, &documents),
        CloseAction::Blocked(CloseBlock::Composition)
    );
    assert!(lifecycle.close_intent().is_none());
    documents[1].composing = false;
    assert_eq!(
        lifecycle.request_close(CloseIntent::Close, &documents, Activity::default()),
        CloseAction::Prompt(1)
    );
}

#[test]
fn bulk_save_continues_without_prompts_and_preserves_prior_discard_choices() {
    let mut documents = vec![document(1, true), document(2, true), document(3, true)];
    let mut lifecycle = Lifecycle::default();
    assert_eq!(
        lifecycle.request_close(CloseIntent::Close, &documents, Activity::default()),
        CloseAction::Prompt(1)
    );
    assert_eq!(
        lifecycle.respond(1, CloseResponse::Discard, &documents),
        CloseAction::Prompt(2)
    );
    assert_eq!(lifecycle.remaining_dirty(&documents), 2);
    assert_eq!(
        lifecycle.respond(2, CloseResponse::SaveAll, &documents),
        CloseAction::Save {
            id: 2,
            then: CloseIntent::Close
        }
    );
    documents[1].dirty = false;
    documents.swap(0, 1);
    assert_eq!(
        lifecycle.saved(CloseIntent::Close, &documents),
        CloseAction::Save {
            id: 3,
            then: CloseIntent::Close
        }
    );
    documents[2].dirty = false;
    documents.swap(0, 2);
    assert_eq!(
        lifecycle.saved(CloseIntent::Close, &documents),
        CloseAction::Finish(CloseIntent::Close)
    );
    assert!(lifecycle.close_intent().is_none());
    assert!(documents.iter().find(|d| d.id == 1).unwrap().dirty);
}

#[test]
fn bulk_discard_finishes_without_mutating_documents_and_is_scoped_to_the_close_intent() {
    let documents = [document(1, true), document(2, true), document(3, true)];
    let mut lifecycle = Lifecycle::default();
    assert_eq!(
        lifecycle.request_close(CloseIntent::Quit, &documents, Activity::default()),
        CloseAction::Prompt(1)
    );
    assert_eq!(
        lifecycle.respond(1, CloseResponse::DiscardAll, &documents),
        CloseAction::Finish(CloseIntent::Quit)
    );
    assert_eq!(lifecycle.prompted_document(), None);
    assert!(lifecycle.close_intent().is_none());
    assert!(documents.iter().all(|d| d.dirty));
    assert_eq!(
        lifecycle.request_close(CloseIntent::Buffer, &documents, Activity::default()),
        CloseAction::Prompt(1)
    );
    assert_eq!(
        lifecycle.respond(1, CloseResponse::DiscardAll, &documents),
        CloseAction::Remove(1)
    );
}

#[test]
fn failed_or_outdated_bulk_save_clears_bulk_mode_and_retry_prompts_again() {
    let documents = [document(1, true), document(2, true)];
    let mut lifecycle = Lifecycle::default();
    for failure in [false, true] {
        assert_eq!(
            lifecycle.request_close(CloseIntent::Close, &documents, Activity::default()),
            CloseAction::Prompt(1)
        );
        assert_eq!(
            lifecycle.respond(1, CloseResponse::SaveAll, &documents),
            CloseAction::Save {
                id: 1,
                then: CloseIntent::Close
            }
        );
        if failure {
            lifecycle.cancel_close();
        } else {
            assert_eq!(
                lifecycle.saved(CloseIntent::Close, &documents),
                CloseAction::Blocked(CloseBlock::NewerEdits)
            );
        }
        assert!(lifecycle.close_intent().is_none());
    }
}

#[test]
fn bulk_save_rechecks_documents_saved_earlier_before_finishing() {
    let mut documents = [document(1, true), document(2, true)];
    let mut lifecycle = Lifecycle::default();
    assert_eq!(
        lifecycle.request_close(CloseIntent::Close, &documents, Activity::default()),
        CloseAction::Prompt(1)
    );
    assert_eq!(
        lifecycle.respond(1, CloseResponse::SaveAll, &documents),
        CloseAction::Save {
            id: 1,
            then: CloseIntent::Close
        }
    );
    documents[0].dirty = false;
    assert_eq!(
        lifecycle.saved(CloseIntent::Close, &documents),
        CloseAction::Save {
            id: 2,
            then: CloseIntent::Close
        }
    );
    documents[0].dirty = true;
    documents[1].dirty = false;
    documents.swap(0, 1);
    assert_eq!(
        lifecycle.saved(CloseIntent::Close, &documents),
        CloseAction::Save {
            id: 1,
            then: CloseIntent::Close
        }
    );
    documents[1].dirty = false;
    documents.swap(0, 1);
    assert_eq!(
        lifecycle.saved(CloseIntent::Close, &documents),
        CloseAction::Finish(CloseIntent::Close)
    );
}
