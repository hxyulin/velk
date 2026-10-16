//! Close and Save All policy. Frontends provide current document facts and carry
//! out returned actions; this module owns no windows, dialogs, tasks, or files.
use std::collections::{BTreeSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseIntent {
    Buffer,
    Close,
    Quit,
}

/// The active document comes first; remaining documents follow the frontend's
/// stable prompt order. Dirty state must be read directly, not from a UI cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentState {
    pub id: u64,
    pub dirty: bool,
    pub composing: bool,
    pub saving: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Activity {
    /// A modal operation, reload, or save of the active document blocks actions.
    pub busy: bool,
    pub opening: bool,
    /// Any document has an in-flight save, including an inactive document.
    pub saving: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseBlock {
    Composition,
    Saving,
    FileOperations,
    NewerEdits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseAction {
    Ignore,
    Blocked(CloseBlock),
    Prompt(u64),
    Save { id: u64, then: CloseIntent },
    Remove(u64),
    Finish(CloseIntent),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseResponse {
    Save,
    SaveAll,
    Discard,
    DiscardAll,
    Cancel,
}

#[derive(Default)]
pub struct Lifecycle {
    closing: Option<CloseIntent>,
    prompt: Option<u64>,
    approved: BTreeSet<u64>,
    save_remaining: bool,
    save_queue: VecDeque<u64>,
}

impl Lifecycle {
    pub fn close_intent(&self) -> Option<CloseIntent> {
        self.closing
    }

    pub fn cancel_close(&mut self) {
        self.closing = None;
        self.prompt = None;
        self.approved.clear();
        self.save_remaining = false;
    }

    pub fn prompted_document(&self) -> Option<u64> {
        self.prompt
    }

    pub fn remaining_dirty(&self, documents: &[DocumentState]) -> usize {
        documents
            .iter()
            .filter(|d| d.dirty && !self.approved.contains(&d.id))
            .count()
    }

    pub fn cancel_save_all(&mut self) {
        self.save_queue.clear();
    }

    pub fn save_all_pending(&self) -> bool {
        !self.save_queue.is_empty()
    }

    pub fn begin_save_all(
        &mut self,
        ids: impl IntoIterator<Item = u64>,
        activity: Activity,
    ) -> bool {
        if activity.busy
            || activity.opening
            || activity.saving
            || self.closing.is_some()
            || self.save_all_pending()
        {
            return false;
        }
        self.save_queue = ids.into_iter().collect();
        true
    }

    /// Closed documents and clean documents are skipped without changing selection.
    pub fn next_save(&mut self, documents: &[DocumentState]) -> Option<u64> {
        while let Some(id) = self.save_queue.pop_front() {
            if documents.iter().any(|d| d.id == id && d.dirty) {
                return Some(id);
            }
        }
        None
    }

    pub fn request_close(
        &mut self,
        intent: CloseIntent,
        documents: &[DocumentState],
        activity: Activity,
    ) -> CloseAction {
        if self.closing.is_some() {
            return CloseAction::Ignore;
        }
        let Some(active) = documents.first() else {
            if intent == CloseIntent::Buffer || activity.busy || activity.opening {
                return CloseAction::Ignore;
            }
            return self.approve_close(intent, documents.first().map(|d| d.id), documents);
        };
        if active.composing {
            return CloseAction::Blocked(CloseBlock::Composition);
        }
        if activity.busy {
            return if active.saving {
                CloseAction::Blocked(CloseBlock::Saving)
            } else {
                CloseAction::Ignore
            };
        }
        if activity.saving || activity.opening || self.save_all_pending() {
            return CloseAction::Blocked(CloseBlock::FileOperations);
        }
        self.approved.clear();
        self.save_remaining = false;
        self.closing = Some(intent);
        let target = if intent != CloseIntent::Buffer && !active.dirty {
            documents.iter().find(|d| d.dirty).unwrap_or(active)
        } else {
            active
        };
        if target.dirty {
            self.prompt(target)
        } else {
            self.approve_close(intent, documents.first().map(|d| d.id), documents)
        }
    }

    /// Only the outstanding prompt's document ID can resolve that prompt.
    pub fn respond(
        &mut self,
        id: u64,
        response: CloseResponse,
        documents: &[DocumentState],
    ) -> CloseAction {
        let Some(intent) = self.closing else {
            return CloseAction::Ignore;
        };
        if self.prompt != Some(id) {
            return CloseAction::Ignore;
        }
        self.prompt = None;
        if !documents.iter().any(|d| d.id == id) {
            self.cancel_close();
            return CloseAction::Ignore;
        }
        match response {
            CloseResponse::Save | CloseResponse::SaveAll => {
                self.save_remaining =
                    response == CloseResponse::SaveAll && intent != CloseIntent::Buffer;
                CloseAction::Save { id, then: intent }
            }
            CloseResponse::Discard => self.approve_close(intent, Some(id), documents),
            CloseResponse::DiscardAll => {
                if intent != CloseIntent::Buffer {
                    if documents.iter().any(|d| d.composing) {
                        self.cancel_close();
                        return CloseAction::Blocked(CloseBlock::Composition);
                    }
                    self.approved.extend(documents.iter().map(|d| d.id));
                }
                self.approve_close(intent, Some(id), documents)
            }
            CloseResponse::Cancel => {
                self.cancel_close();
                CloseAction::Ignore
            }
        }
    }

    /// A successful save may persist an older snapshot. Newer edits must keep the
    /// document open; a failed save should cancel closing without calling this.
    /// The first document must be the one whose save completed, resolved by ID.
    pub fn saved(&mut self, intent: CloseIntent, documents: &[DocumentState]) -> CloseAction {
        self.prompt = None;
        if documents.first().is_some_and(|d| d.dirty) {
            self.cancel_close();
            CloseAction::Blocked(CloseBlock::NewerEdits)
        } else {
            self.approve_close(intent, documents.first().map(|d| d.id), documents)
        }
    }

    fn prompt(&mut self, target: &DocumentState) -> CloseAction {
        if target.composing {
            self.cancel_close();
            CloseAction::Blocked(CloseBlock::Composition)
        } else {
            self.prompt = Some(target.id);
            CloseAction::Prompt(target.id)
        }
    }

    fn approve_close(
        &mut self,
        intent: CloseIntent,
        id: Option<u64>,
        documents: &[DocumentState],
    ) -> CloseAction {
        if intent == CloseIntent::Buffer {
            self.cancel_close();
            return id.map_or(CloseAction::Ignore, CloseAction::Remove);
        }
        // Approval is provisional: the frontend keeps every document until the
        // complete close succeeds, so cancelling a later prompt loses no text.
        // Only discards waive dirty checks. A saved document can acquire new
        // edits while later documents are still being written.
        if let Some(id) = id
            && documents.iter().any(|d| d.id == id && d.dirty)
        {
            self.approved.insert(id);
        }
        self.closing = Some(intent);
        if let Some(next) = documents
            .iter()
            .find(|d| d.dirty && !self.approved.contains(&d.id))
        {
            if self.save_remaining && !next.composing {
                return CloseAction::Save {
                    id: next.id,
                    then: intent,
                };
            }
            return self.prompt(next);
        }
        self.cancel_close();
        CloseAction::Finish(intent)
    }
}
