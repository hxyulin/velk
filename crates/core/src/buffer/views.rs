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
    pub fn restore(bytes: &[u8]) -> Result<Self, String> {
        DocumentState::restore(bytes).map(Self::from_state)
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
        self.update(|d| d.set_caret(position, extend));
    }
    pub fn set_selection_preserving_column(&mut self, selection: Selection) {
        self.update(|d| d.set_selection_preserving_column(selection));
    }
    pub fn select_all(&mut self) {
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
        self.update(DocumentState::undo);
    }
    pub fn redo(&mut self) {
        self.update(DocumentState::redo);
    }
    pub fn clamp_caret_to_line_character(&mut self) {
        self.update(DocumentState::clamp_caret_to_line_character);
    }
    pub fn horizontal(&mut self, right: bool, extend: bool) {
        self.update(|d| d.horizontal(right, extend));
    }
    pub fn vertical(&mut self, down: bool, extend: bool) {
        self.update(|d| d.vertical(down, extend));
    }
    pub fn line_edge(&mut self, end: bool, extend: bool) {
        self.update(|d| d.line_edge(end, extend));
    }
    pub fn document_edge(&mut self, end: bool, extend: bool) {
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
    fn views_share_text_history_and_checkpoints_but_keep_independent_carets() {
        let mut first = Buffer::new("first\nsecond\n");
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
        assert!(!Buffer::new("").mark_saved(saved));
    }
    #[test]
    fn transaction_rebases_every_view_even_when_it_exceeds_the_change_log() {
        let mut first = Buffer::new(&"x ".repeat(300));
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
        let mut first = Buffer::new("old");
        let snapshot = first.text();
        first.set_caret(3, false);
        let second = first.new_view();
        first.insert(" text");
        let bytes = second.snapshot().encode().unwrap();
        drop(first);
        assert_eq!(snapshot.to_string(), "old");
        assert_eq!(second.text().to_string(), "old text");
        let restored = Buffer::restore(&bytes).unwrap();
        assert!(!restored.document().same_document(&second.document()));
        assert_eq!(restored.selection(), second.selection());
        assert_eq!(restored.text(), second.text());
    }
    #[test]
    fn preferred_columns_and_grapheme_anchors_are_view_local() {
        let mut first = Buffer::new("abcd\nx\nabcd");
        first.set_caret(3, false);
        first.vertical(true, false);
        let mut second = first.new_view();
        second.set_caret(0, false);
        second.vertical(true, false);
        first.vertical(true, false);
        assert_eq!(first.selection().head, 10);
        assert_eq!(second.selection().head, 5);
        let mut first = Buffer::new("e");
        first.set_caret(1, false);
        let mut second = first.new_view();
        second.insert("\u{301}");
        assert_eq!(first.selection().head, 2);
        assert!(first.is_grapheme_boundary(first.selection().head));
    }
}
