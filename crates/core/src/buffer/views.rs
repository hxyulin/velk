use super::*;
use std::sync::Mutex;

#[derive(Clone, Copy, Default)]
pub(super) struct State {
    pub selection: Selection,
    pub goal_column: Option<usize>,
}

/// Authoritative text, revision, save identity, and history shared by its views.
/// Snapshot reads clone the persistent Rope, never a full document string.
#[derive(Clone)]
pub struct TextDocument(Arc<Mutex<DocumentState>>);
impl TextDocument {
    pub fn new(text: &str) -> Self {
        Self(Arc::new(Mutex::new(DocumentState::new(text))))
    }
    /// Create a view at the start of the document, without copying its history.
    pub fn new_view(&self) -> Buffer {
        self.view(State::default())
    }
    fn view(&self, state: State) -> Buffer {
        let mut document = self.0.lock().unwrap();
        let id = document.next_view;
        document.next_view += 1;
        document.views.insert(id, state);
        Buffer {
            document: self.clone(),
            id,
        }
    }
    pub fn text(&self) -> Rope {
        self.0.lock().unwrap().current.text.clone()
    }
    pub fn same_document(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A document view with its own directed selection and preferred grapheme column.
/// New views share text, undo/redo, revisions, and the saved checkpoint. Selection
/// anchors in other views follow every edit, including undo and redo.
pub struct Buffer {
    document: TextDocument,
    id: u64,
}
impl Default for Buffer {
    fn default() -> Self {
        Self::new("")
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        self.document.0.lock().unwrap().views.remove(&self.id);
    }
}
impl Buffer {
    /// Create a headless buffer without history. Use `with_history` to choose
    /// an undo implementation explicitly.
    pub fn new(text: &str) -> Self {
        TextDocument::new(text).new_view()
    }
    fn from_state(document: DocumentState) -> Self {
        let state = State {
            selection: document.current.selection,
            goal_column: document.goal_column,
        };
        TextDocument(Arc::new(Mutex::new(document))).view(state)
    }
    pub fn from_reader(reader: impl Read) -> io::Result<Self> {
        DocumentState::from_reader(reader).map(Self::from_state)
    }
    pub fn from_reader_with_history(
        reader: impl Read,
        provider: Arc<dyn crate::history::Provider>,
    ) -> io::Result<Self> {
        let mut buffer = Self::from_reader(reader)?;
        buffer.attach_history(provider).map_err(io::Error::other)?;
        Ok(buffer)
    }
    /// Blocking archive restore with a provider and optional exact file baseline.
    pub fn read_archive_with_history(
        path: &std::path::Path,
        expected: Option<&Rope>,
        provider: Arc<dyn crate::history::Provider>,
    ) -> Result<Self, String> {
        let buffer =
            Self::restore_with_history(&std::fs::read(path).map_err(|e| e.to_string())?, provider)?;
        if expected.is_some_and(|text| *text != buffer.text()) {
            return Err("undo archive does not match the current file".into());
        }
        Ok(buffer)
    }
    /// Restore provider-free state. Archives containing history require
    /// `restore_with_history` and their declared provider.
    pub fn restore(bytes: &[u8]) -> Result<Self, String> {
        DocumentState::restore(bytes, None).map(Self::from_state)
    }
    /// Begin one input operation. Pair with `end_undo_group`; selection movement,
    /// saves, undo, and redo end a typing run. The inactivity limit is one second.
    pub fn begin_undo_group(&mut self, kind: UndoGroup) {
        let mut document = self.document.0.lock().unwrap();
        let selection = document.views[&self.id].selection;
        let time = std::time::Instant::now();
        document.merge_group = document.previous_group.as_ref().is_some_and(|last| {
            last.view == self.id
                && last.kind == kind
                && last.state == document.current.state
                && last.selection == selection
                && time.duration_since(last.time) < std::time::Duration::from_secs(1)
        });
        document.group = Some(history::Group {
            view: self.id,
            kind,
            state: document.current.state,
            selection,
            time,
        });
    }
    pub fn end_undo_group(&mut self) {
        let mut document = self.document.0.lock().unwrap();
        if document
            .group
            .as_ref()
            .is_some_and(|group| group.view != self.id)
        {
            return;
        }
        if let Some(mut group) = document.group.take()
            && group.state != document.current.state
        {
            group.state = document.current.state;
            group.selection = document.views[&self.id].selection;
            group.time = std::time::Instant::now();
            document.previous_group = Some(group);
        }
        document.merge_group = false;
    }
    pub fn break_undo_group(&mut self) {
        let mut document = self.document.0.lock().unwrap();
        document.group = None;
        document.previous_group = None;
        document.merge_group = false;
    }
    pub fn with_history(text: &str, provider: Arc<dyn crate::history::Provider>) -> Self {
        let buffer = Self::new(text);
        buffer.document.0.lock().unwrap().history = Some(history::Owned {
            session: provider.open(0),
            provider,
        });
        buffer
    }
    /// Attach only to a document without a history session. Existing providers
    /// remain document-owned until all views close; replacing one is explicit.
    pub fn attach_history(
        &mut self,
        provider: Arc<dyn crate::history::Provider>,
    ) -> Result<(), String> {
        self.update(|document| {
            if document.history.is_some() {
                return Err("document already has an undo provider".into());
            }
            document.history = Some(history::Owned {
                session: provider.open(document.current.state),
                provider,
            });
            Ok(())
        })
    }
    pub fn archive_history_provider(bytes: &[u8]) -> Result<Option<String>, String> {
        #[derive(serde::Deserialize)]
        struct Header {
            history: Option<HistoryHeader>,
        }
        #[derive(serde::Deserialize)]
        struct HistoryHeader {
            provider: String,
        }
        serde_json::from_slice::<Header>(bytes)
            .map(|header| header.history.map(|h| h.provider))
            .map_err(|e| e.to_string())
    }
    pub fn restore_with_history(
        bytes: &[u8],
        provider: Arc<dyn crate::history::Provider>,
    ) -> Result<Self, String> {
        DocumentState::restore(bytes, Some(provider)).map(Self::from_state)
    }
    pub fn history_provider(&self) -> Option<String> {
        self.read(|d| d.history.as_ref().map(|h| h.provider.id().to_owned()))
    }
    pub fn history_limits(&self) -> HistoryLimits {
        self.read(|d| {
            d.history
                .as_ref()
                .map(|h| h.session.status().limits)
                .unwrap_or_default()
        })
    }
    pub fn set_history_limits(&mut self, limits: HistoryLimits) {
        self.update(|d| {
            if let Some(history) = &mut d.history
                && let Err(error) = history.session.request(history::Request::Limits {
                    current: d.current.state,
                    limits,
                })
            {
                tracing::error!(%error, "cannot set undo limits");
            }
        });
    }
    pub fn history_bytes(&self) -> usize {
        self.read(|d| d.history.as_ref().map_or(0, |h| h.session.status().bytes))
    }
    pub fn redo_branches(&self) -> Vec<RedoBranch> {
        self.read(|d| {
            d.history
                .as_ref()
                .map_or_else(Vec::new, |h| h.session.status().branches)
        })
    }
    pub fn redo_branch(&mut self, state: u64) -> bool {
        self.break_undo_group();
        self.update(|d| d.redo_branch(state))
    }
    pub fn document(&self) -> TextDocument {
        self.document.clone()
    }
    /// Duplicate this view's caret and preferred column, sharing its document.
    pub fn new_view(&self) -> Self {
        let state = self.document.0.lock().unwrap().views[&self.id];
        self.document.view(state)
    }
    /// An immutable, cheap snapshot of the current document. No lock is retained.
    pub fn text(&self) -> Rope {
        self.document.text()
    }
    pub fn selection(&self) -> Selection {
        self.document.0.lock().unwrap().views[&self.id].selection
    }
    fn read<R>(&self, run: impl FnOnce(&DocumentState) -> R) -> R {
        run(&self.document.0.lock().unwrap())
    }
    fn update<R>(&self, run: impl FnOnce(&mut DocumentState) -> R) -> R {
        let mut document = self.document.0.lock().unwrap();
        if document
            .group
            .as_ref()
            .is_some_and(|group| group.view != self.id)
        {
            document.group = None;
            document.previous_group = None;
            document.merge_group = false;
        }
        let view = document.views[&self.id];
        document.current.selection = view.selection;
        document.goal_column = view.goal_column;
        let revision = document.revision;
        let result = run(&mut document);
        if document.revision != revision {
            let text = document.current.text.clone();
            for view in document.views.values_mut() {
                let snap = |position: usize| {
                    let position = position.min(text.len_chars());
                    if graphemes::is_boundary(&text, position) {
                        position
                    } else {
                        graphemes::next(&text, position)
                    }
                };
                view.selection = Selection {
                    anchor: snap(view.selection.anchor),
                    head: snap(view.selection.head),
                };
            }
        }
        let state = State {
            selection: document.current.selection,
            goal_column: document.goal_column,
        };
        document.views.insert(self.id, state);
        result
    }
    pub fn snapshot(&self) -> BufferSnapshot {
        self.update(|document| document.snapshot())
    }
    pub fn revision(&self) -> u64 {
        self.read(DocumentState::revision)
    }
    pub fn checkpoint(&self) -> Checkpoint {
        self.read(DocumentState::checkpoint)
    }
    pub fn is_dirty(&self) -> bool {
        self.read(DocumentState::is_dirty)
    }
    pub fn can_undo(&self) -> bool {
        self.read(DocumentState::can_undo)
    }
    pub fn can_redo(&self) -> bool {
        self.read(DocumentState::can_redo)
    }
    pub fn mark_saved(&mut self, checkpoint: Checkpoint) -> bool {
        self.break_undo_group();
        self.update(|d| d.mark_saved(checkpoint))
    }
    pub fn is_grapheme_boundary(&self, position: usize) -> bool {
        self.read(|d| d.is_grapheme_boundary(position))
    }
    pub fn grapheme_range(&self, range: Range<usize>) -> Option<Range<usize>> {
        self.read(|d| d.grapheme_range(range))
    }
    pub fn previous_grapheme_boundary(&self, position: usize) -> usize {
        self.read(|d| d.previous_grapheme_boundary(position))
    }
    pub fn next_grapheme_boundary(&self, position: usize) -> usize {
        self.read(|d| d.next_grapheme_boundary(position))
    }
    pub fn selected_text(&self) -> String {
        let document = self.document.0.lock().unwrap();
        document
            .current
            .text
            .slice(document.views[&self.id].selection.range())
            .to_string()
    }
    pub fn edits_since(&self, revision: u64) -> Option<Vec<Change>> {
        self.read(|d| d.edits_since(revision))
    }
    pub fn set_caret(&mut self, position: usize, extend: bool) {
        self.break_undo_group();
        self.update(|d| d.set_caret(position, extend));
    }
    pub fn set_selection_preserving_column(&mut self, selection: Selection) {
        self.update(|d| d.set_selection_preserving_column(selection));
    }
    pub fn select_all(&mut self) {
        self.break_undo_group();
        self.update(DocumentState::select_all);
    }
    pub fn insert(&mut self, text: &str) {
        self.update(|d| d.insert(text));
    }
    pub fn backspace(&mut self) {
        self.update(DocumentState::backspace);
    }
    pub fn delete(&mut self) {
        self.update(DocumentState::delete);
    }
    pub fn undo(&mut self) {
        self.break_undo_group();
        self.update(DocumentState::undo);
    }
    pub fn redo(&mut self) {
        self.break_undo_group();
        self.update(DocumentState::redo);
    }
    pub fn clamp_caret_to_line_character(&mut self) {
        self.update(DocumentState::clamp_caret_to_line_character);
    }
    pub fn horizontal(&mut self, right: bool, extend: bool) {
        self.break_undo_group();
        self.update(|d| d.horizontal(right, extend));
    }
    pub fn vertical(&mut self, down: bool, extend: bool) {
        self.break_undo_group();
        self.update(|d| d.vertical(down, extend));
    }
    pub fn line_edge(&mut self, end: bool, extend: bool) {
        self.break_undo_group();
        self.update(|d| d.line_edge(end, extend));
    }
    pub fn document_edge(&mut self, end: bool, extend: bool) {
        self.break_undo_group();
        self.update(|d| d.document_edge(end, extend));
    }
    pub fn apply_transaction(
        &mut self,
        transaction: Transaction,
    ) -> Result<bool, TransactionError> {
        self.update(|d| d.apply_transaction(transaction))
    }
    #[cfg(test)]
    pub(super) fn line_boundaries(&self, line: usize) -> Arc<Vec<usize>> {
        self.read(|d| d.line_boundaries(line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_input_ends_a_typing_group_without_sleeping() {
        let mut buffer = crate::Buffer::with_history("", std::sync::Arc::new(velk_undo_tree::Tree));
        buffer.begin_undo_group(UndoGroup::Typing);
        buffer.insert("a");
        buffer.end_undo_group();
        buffer
            .document
            .0
            .lock()
            .unwrap()
            .previous_group
            .as_mut()
            .unwrap()
            .time = std::time::Instant::now() - std::time::Duration::from_secs(2);
        buffer.begin_undo_group(UndoGroup::Typing);
        buffer.insert("b");
        buffer.end_undo_group();
        buffer.undo();
        assert_eq!(buffer.text().to_string(), "a");
    }
    #[test]
    fn an_interleaved_view_cannot_join_another_views_open_group() {
        let mut first = crate::Buffer::with_history("", std::sync::Arc::new(velk_undo_tree::Tree));
        let mut second = first.new_view();
        first.begin_undo_group(UndoGroup::Typing);
        first.insert("a");
        second.insert("b");
        first.end_undo_group();
        first.undo();
        assert_eq!(first.text().to_string(), "a");
    }
    #[test]
    fn views_share_text_history_and_checkpoints_but_keep_independent_carets() {
        let mut first = crate::Buffer::with_history(
            "first\nsecond\n",
            std::sync::Arc::new(velk_undo_tree::Tree),
        );
        first.set_caret(6, false);
        let mut second = first.new_view();
        second.set_caret(0, false);
        let original = first.checkpoint();
        second.insert("new ");
        assert_eq!(first.text().to_string(), "new first\nsecond\n");
        assert_eq!(first.selection().head, 10);
        assert_eq!(second.selection().head, 4);
        assert_eq!(first.revision(), second.revision());
        first.undo();
        assert_eq!(second.text().to_string(), "first\nsecond\n");
        assert_eq!(first.checkpoint(), original);
        assert!(!second.is_dirty());
        second.redo();
        let saved = second.checkpoint();
        assert!(first.mark_saved(saved));
        assert!(!second.is_dirty());
        first.insert("x");
        assert!(second.is_dirty());
        assert!(
            !crate::Buffer::with_history("", std::sync::Arc::new(velk_undo_tree::Tree))
                .mark_saved(saved)
        );
    }
    #[test]
    fn transaction_rebases_every_view_even_when_it_exceeds_the_change_log() {
        let mut first = crate::Buffer::with_history(
            &"x ".repeat(300),
            std::sync::Arc::new(velk_undo_tree::Tree),
        );
        let mut second = first.new_view();
        second.set_caret(600, false);
        first
            .apply_transaction(Transaction {
                revision: 0,
                edits: (0..300)
                    .map(|i| TextEdit {
                        range: i * 2..i * 2 + 1,
                        text: "long".into(),
                    })
                    .collect(),
                selection: Some(Selection::default()),
            })
            .unwrap();
        assert_eq!(second.selection().head, 1500);
        assert!(first.edits_since(0).is_none());
        second.undo();
        assert_eq!(first.text().len_chars(), 600);
        assert_eq!(first.selection().head, 1);
        first.redo();
        assert_eq!(second.text().len_chars(), 1500);
    }
    #[test]
    fn snapshots_and_lifetimes_are_independent_of_view_ownership() {
        let mut first =
            crate::Buffer::with_history("old", std::sync::Arc::new(velk_undo_tree::Tree));
        let snapshot = first.text();
        first.set_caret(3, false);
        let second = first.new_view();
        first.insert(" text");
        let bytes = second.snapshot().encode().unwrap();
        drop(first);
        assert_eq!(snapshot.to_string(), "old");
        assert_eq!(second.text().to_string(), "old text");
        let restored =
            crate::Buffer::restore_with_history(&bytes, std::sync::Arc::new(velk_undo_tree::Tree))
                .unwrap();
        assert!(!restored.document().same_document(&second.document()));
        assert_eq!(restored.selection(), second.selection());
        assert_eq!(restored.text(), second.text());
    }
    #[test]
    fn preferred_columns_and_grapheme_anchors_are_view_local() {
        let mut first =
            crate::Buffer::with_history("abcd\nx\nabcd", std::sync::Arc::new(velk_undo_tree::Tree));
        first.set_caret(3, false);
        first.vertical(true, false);
        let mut second = first.new_view();
        second.set_caret(0, false);
        second.vertical(true, false);
        first.vertical(true, false);
        assert_eq!(first.selection().head, 10);
        assert_eq!(second.selection().head, 5);
        let mut first = crate::Buffer::with_history("e", std::sync::Arc::new(velk_undo_tree::Tree));
        first.set_caret(1, false);
        let mut second = first.new_view();
        second.insert("\u{301}");
        assert_eq!(first.selection().head, 2);
        assert!(first.is_grapheme_boundary(first.selection().head));
    }
}
