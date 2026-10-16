mod session;
use ropey::{Rope, RopeBuilder};
pub use session::BufferSnapshot;
use std::{
    cell::RefCell,
    collections::VecDeque,
    fmt,
    io::{self, Read},
    ops::Range,
    sync::Arc,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{Change, graphemes};

const HISTORY_LIMIT: usize = 128;

/// A directed selection, in Unicode scalar (Ropey character) offsets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn range(self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }
}

/// A replacement in Unicode scalar offsets in the transaction's original document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub text: String,
}

/// Non-overlapping edits against one document revision, committed as one undo step.
/// Input order is immaterial. Equal starting offsets are rejected as ambiguous.
/// Optional selection coordinates refer to the final document; otherwise both
/// selection endpoints follow edits with right affinity.
#[derive(Clone, Debug)]
pub struct Transaction {
    pub revision: u64,
    pub edits: Vec<TextEdit>,
    pub selection: Option<Selection>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionError {
    StaleRevision,
    InvalidRange,
    OverlappingEdits,
    InvalidSelection,
}

impl fmt::Display for TransactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::StaleRevision => "transaction targets an obsolete document revision",
            Self::InvalidRange => "transaction range is outside the document or reversed",
            Self::OverlappingEdits => {
                "transaction edits overlap or have ambiguous starting offsets"
            }
            Self::InvalidSelection => "transaction selection is outside the final document",
        })
    }
}
impl std::error::Error for TransactionError {}

/// Identity of a document content state. Survives undo/redo and can be retained
/// alongside an asynchronous save without retaining the document itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    document: u64,
    state: u64,
}

#[derive(Clone)]
struct Snapshot {
    text: Rope,
    selection: Selection,
    state: u64,
}

#[derive(Clone)]
struct History {
    snapshot: Snapshot,
    edits: Vec<Change>,
}

/// Rope document and editing state, independent of RXUI and the graphics host.
/// Newlines are normalized to LF. All caret positions are grapheme boundaries.
pub struct Buffer {
    current: Snapshot,
    undo: VecDeque<History>,
    redo: Vec<History>,
    revision: u64,
    document: u64,
    next_state: u64,
    saved: Checkpoint,
    edits: VecDeque<Change>,
    goal_column: Option<usize>,
    boundaries: RefCell<VecDeque<(usize, Arc<Vec<usize>>)>>,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new("")
    }
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        Self::from_rope(Rope::from_str(&normalize(text)))
    }

    /// Read UTF-8 directly into a Rope, normalizing CRLF and CR to LF.
    /// The loaded document starts with an empty undo history and a caret at zero.
    pub fn from_reader(reader: impl Read) -> io::Result<Self> {
        let rope = Rope::from_reader(reader)?;
        if !rope.chunks().any(|chunk| chunk.contains('\r')) {
            return Ok(Self::from_rope(rope));
        }
        let mut normalized = RopeBuilder::new();
        let mut after_cr = false;
        for chunk in rope.chunks() {
            for piece in chunk.split_inclusive('\r') {
                let piece = if after_cr {
                    piece.strip_prefix('\n').unwrap_or(piece)
                } else {
                    piece
                };
                if let Some(before) = piece.strip_suffix('\r') {
                    normalized.append(before);
                    normalized.append("\n");
                    after_cr = true;
                } else {
                    normalized.append(piece);
                    after_cr = false;
                }
            }
        }
        Ok(Self::from_rope(normalized.finish()))
    }

    fn from_rope(text: Rope) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_DOCUMENT: AtomicU64 = AtomicU64::new(1);
        let document = NEXT_DOCUMENT.fetch_add(1, Ordering::Relaxed);
        Self {
            current: Snapshot {
                text,
                selection: Selection::default(),
                state: 0,
            },
            undo: VecDeque::new(),
            redo: Vec::new(),
            revision: 0,
            document,
            next_state: 1,
            saved: Checkpoint { document, state: 0 },
            edits: VecDeque::new(),
            goal_column: None,
            boundaries: RefCell::new(VecDeque::new()),
        }
    }

    pub fn text(&self) -> &Rope {
        &self.current.text
    }

    pub fn is_grapheme_boundary(&self, position: usize) -> bool {
        position <= self.text().len_chars() && graphemes::is_boundary(self.text(), position)
    }

    /// Expand a semantic scalar range to complete graphemes for decoration.
    /// Invalid offsets are rejected, rather than clamped onto unrelated text.
    pub fn grapheme_range(&self, range: Range<usize>) -> Option<Range<usize>> {
        if range.start > range.end || range.end > self.text().len_chars() {
            return None;
        }
        let start = self.snap(range.start);
        let end = if self.is_grapheme_boundary(range.end) {
            range.end
        } else {
            self.next(range.end)
        };
        Some(start..end)
    }

    pub fn selection(&self) -> Selection {
        self.current.selection
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            document: self.document,
            state: self.current.state,
        }
    }

    /// Constant-time history identity comparison, including returning to a saved
    /// state through undo/redo. Independently retyping identical content is dirty.
    pub fn is_dirty(&self) -> bool {
        self.checkpoint() != self.saved
    }

    /// Mark the successfully persisted snapshot, even if editing has continued.
    /// Checkpoints from another document are rejected.
    pub fn mark_saved(&mut self, checkpoint: Checkpoint) -> bool {
        if checkpoint.document != self.document {
            return false;
        }
        self.saved = checkpoint;
        true
    }

    pub fn selected_text(&self) -> String {
        self.text().slice(self.selection().range()).to_string()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Clamp and snap an external character offset to the preceding grapheme edge.
    pub fn set_caret(&mut self, position: usize, extend: bool) {
        let position = self.snap(position.min(self.text().len_chars()));
        self.current.selection.head = position;
        if !extend {
            self.current.selection.anchor = position;
        }
        self.goal_column = None;
    }

    pub fn select_all(&mut self) {
        self.current.selection = Selection {
            anchor: 0,
            head: self.text().len_chars(),
        };
        self.goal_column = None;
    }

    pub fn insert(&mut self, text: &str) {
        self.replace(self.selection().range(), &normalize(text));
    }

    pub fn backspace(&mut self) {
        let range = self.selection().range();
        let range = if range.is_empty() {
            self.previous(range.start)..range.end
        } else {
            range
        };
        self.replace(range, "");
    }

    pub fn delete(&mut self) {
        let range = self.selection().range();
        let range = if range.is_empty() {
            range.start..self.next(range.end)
        } else {
            range
        };
        self.replace(range, "");
    }

    pub fn horizontal(&mut self, right: bool, extend: bool) {
        let selection = self.selection();
        let position = if !extend && !selection.is_empty() {
            if right {
                selection.range().end
            } else {
                selection.range().start
            }
        } else if right {
            self.next(selection.head)
        } else {
            self.previous(selection.head)
        };
        self.set_caret(position, extend);
    }

    /// Move by logical lines while retaining the preferred grapheme column.
    pub fn vertical(&mut self, down: bool, extend: bool) {
        let head = self.selection().head;
        let line = self.text().char_to_line(head);
        let boundaries = self.line_boundaries(line);
        let column = self
            .goal_column
            .unwrap_or_else(|| boundaries.partition_point(|&p| p < head));
        let target = if down {
            (line + 1).min(self.text().len_lines() - 1)
        } else {
            line.saturating_sub(1)
        };
        // Up/Down at the document edge leaves the preferred column intact.
        if target != line {
            let boundaries = self.line_boundaries(target);
            self.set_caret(boundaries[column.min(boundaries.len() - 1)], extend);
        }
        self.goal_column = Some(column);
    }

    pub fn line_edge(&mut self, end: bool, extend: bool) {
        let line = self.text().char_to_line(self.selection().head);
        let start = self.text().line_to_char(line);
        let content = self.text().line(line);
        let length = content.len_chars();
        let newline = usize::from(length > 0 && content.char(length - 1) == '\n');
        self.set_caret(if end { start + length - newline } else { start }, extend);
    }

    pub fn document_edge(&mut self, end: bool, extend: bool) {
        self.set_caret(if end { self.text().len_chars() } else { 0 }, extend);
    }

    /// Changes after a revision, or None if the bounded change log has expired.
    /// Each change uses coordinates in the immediately preceding document state.
    pub fn edits_since(&self, revision: u64) -> Option<Vec<Change>> {
        let edits: Vec<_> = self
            .edits
            .iter()
            .filter(|e| e.revision > revision)
            .cloned()
            .collect();
        (revision + edits.len() as u64 == self.revision).then_some(edits)
    }

    pub fn undo(&mut self) {
        if let Some(previous) = self.undo.pop_back() {
            for edit in previous.edits.iter().rev() {
                self.revision += 1;
                self.record(edit.inverse(self.revision));
            }
            self.redo.push(History {
                snapshot: std::mem::replace(&mut self.current, previous.snapshot),
                edits: previous.edits,
            });
            self.changed();
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            for edit in &next.edits {
                self.revision += 1;
                let mut edit = edit.clone();
                edit.revision = self.revision;
                self.record(edit);
            }
            self.undo.push_back(History {
                snapshot: std::mem::replace(&mut self.current, next.snapshot),
                edits: next.edits,
            });
            self.changed();
        }
    }

    fn record(&mut self, edit: Change) {
        self.edits.push_back(edit);
        if self.edits.len() > HISTORY_LIMIT {
            self.edits.pop_front();
        }
    }

    /// Apply atomically. Ranges are scalar offsets, so protocol edits may split
    /// an existing grapheme; only the final caret/selection is snapped to edges.
    /// Each effective replacement advances the monotonic document revision; the
    /// entire transaction occupies one history entry. Errors leave state intact.
    #[tracing::instrument(level = "debug", skip_all, fields(revision = self.revision, edits = transaction.edits.len()))]
    pub fn apply_transaction(
        &mut self,
        mut transaction: Transaction,
    ) -> Result<bool, TransactionError> {
        if transaction.revision != self.revision {
            return Err(TransactionError::StaleRevision);
        }
        transaction
            .edits
            .sort_by_key(|edit| (edit.range.start, edit.range.end));
        for edit in &transaction.edits {
            if edit.range.start > edit.range.end || edit.range.end > self.text().len_chars() {
                return Err(TransactionError::InvalidRange);
            }
        }
        for pair in transaction.edits.windows(2) {
            if pair[0].range.end > pair[1].range.start || pair[0].range.start == pair[1].range.start
            {
                return Err(TransactionError::OverlappingEdits);
            }
        }
        let mut next = self.current.clone();
        let mut edits = Vec::with_capacity(transaction.edits.len());
        for edit in transaction.edits.into_iter().rev() {
            let text = if edit.text.contains('\r') {
                normalize(&edit.text)
            } else {
                edit.text
            };
            if next.text.slice(edit.range.clone()) == text {
                continue;
            }
            let before = next.text.clone();
            next.text.remove(edit.range.clone());
            next.text.insert(edit.range.start, &text);
            let inserted = text.chars().count();
            let map = |position: usize| {
                if position < edit.range.start {
                    position
                } else if position <= edit.range.end {
                    edit.range.start + inserted
                } else {
                    position - edit.range.len() + inserted
                }
            };
            next.selection = Selection {
                anchor: map(next.selection.anchor),
                head: map(next.selection.head),
            };
            edits.push(Change::between(
                self.revision + edits.len() as u64 + 1,
                &before,
                &next.text,
                edit.range,
                inserted,
            ));
        }
        if let Some(selection) = transaction.selection {
            if selection.anchor > next.text.len_chars() || selection.head > next.text.len_chars() {
                return Err(TransactionError::InvalidSelection);
            }
            next.selection = selection;
        }
        let snap = |position| {
            if graphemes::is_boundary(&next.text, position) {
                position
            } else {
                graphemes::next(&next.text, position)
            }
        };
        next.selection = Selection {
            anchor: snap(next.selection.anchor),
            head: snap(next.selection.head),
        };
        if edits.is_empty() {
            self.current.selection = next.selection;
            self.goal_column = None;
            return Ok(false);
        }
        let first_line = self
            .text()
            .char_to_line(edits.last().unwrap().replaced.start);
        self.boundaries
            .get_mut()
            .retain(|(line, _)| *line < first_line);
        next.state = self.next_state;
        self.next_state += 1;
        let previous = std::mem::replace(&mut self.current, next);
        self.redo.clear();
        for edit in &edits {
            self.record(edit.clone());
        }
        self.revision += edits.len() as u64;
        self.undo.push_back(History {
            snapshot: previous,
            edits,
        });
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.pop_front();
        }
        self.goal_column = None;
        Ok(true)
    }

    fn replace(&mut self, range: Range<usize>, text: &str) {
        let head = range.start + text.chars().count();
        self.apply_transaction(Transaction {
            revision: self.revision,
            edits: vec![TextEdit {
                range,
                text: text.to_owned(),
            }],
            selection: Some(Selection { anchor: head, head }),
        })
        .expect("internal editing range is valid");
    }

    fn changed(&mut self) {
        self.boundaries.get_mut().clear();
        self.goal_column = None;
    }

    // Up/Down needs grapheme-column indexing. Only those operations materialize
    // logical lines; point queries use borrowed Rope chunks instead.
    fn line_boundaries(&self, line: usize) -> Arc<Vec<usize>> {
        let mut cache = self.boundaries.borrow_mut();
        if let Some(index) = cache.iter().position(|(cached, _)| *cached == line) {
            let entry = cache.remove(index).unwrap();
            let result = entry.1.clone();
            cache.push_back(entry);
            return result;
        }
        let start = self.text().line_to_char(line);
        let text = self.text().line(line).to_string();
        let text = text.strip_suffix('\n').unwrap_or(&text);
        let mut offset = start;
        let mut result = vec![start];
        for grapheme in text.graphemes(true) {
            offset += grapheme.chars().count();
            result.push(offset);
        }
        let result = Arc::new(result);
        if cache.len() == 8 {
            cache.pop_front();
        }
        cache.push_back((line, result.clone()));
        result
    }

    fn snap(&self, position: usize) -> usize {
        if graphemes::is_boundary(self.text(), position) {
            position
        } else {
            self.previous(position)
        }
    }

    fn previous(&self, position: usize) -> usize {
        graphemes::previous(self.text(), position)
    }

    fn next(&self, position: usize) -> usize {
        graphemes::next(self.text(), position)
    }
}

pub(crate) fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_keep_prefix_column_cache_and_invalidate_shifted_lines_and_history() {
        let mut buffer = Buffer::new("éabc\n👩‍💻xy\nlast");
        let prefix = buffer.line_boundaries(0);
        let edited = buffer.line_boundaries(1);
        let suffix = buffer.line_boundaries(2);
        buffer.set_caret(buffer.text().line_to_char(1), false);
        buffer.insert("new\n");
        assert!(Arc::ptr_eq(&prefix, &buffer.line_boundaries(0)));
        assert!(!Arc::ptr_eq(&edited, &buffer.line_boundaries(1)));
        assert!(!Arc::ptr_eq(&suffix, &buffer.line_boundaries(2)));
        assert_eq!(&*buffer.line_boundaries(1), &[6, 7, 8, 9]);
        assert_eq!(&*buffer.line_boundaries(2), &[10, 13, 14, 15]);
        buffer.undo();
        assert_eq!(&*buffer.line_boundaries(1), &[6, 9, 10, 11]);
        buffer.redo();
        assert_eq!(&*buffer.line_boundaries(2), &[10, 13, 14, 15]);
        buffer.document_edge(false, false);
        buffer.delete();
        assert_eq!(&*buffer.line_boundaries(0), &[0, 1, 2, 3]);
    }
}
