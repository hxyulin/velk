mod history;
mod session;
pub use history::{HistoryLimits, RedoBranch, UndoGroup};
mod views;
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
pub use views::{Buffer, TextDocument};

use crate::{Change, graphemes};

const CHANGE_LOG_LIMIT: usize = 128;

pub use velk_core_contracts::Selection;

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

impl Transaction {
    /// Validate ranges against the original document without mutating it.
    /// Explicit final selection is checked when applying the transaction.
    pub fn validate_ranges(&self, revision: u64, length: usize) -> Result<(), TransactionError> {
        if self.revision != revision {
            return Err(TransactionError::StaleRevision);
        }
        let mut edits: Vec<_> = self.edits.iter().collect();
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        if edits
            .iter()
            .any(|edit| edit.range.start > edit.range.end || edit.range.end > length)
        {
            return Err(TransactionError::InvalidRange);
        }
        for pair in edits.windows(2) {
            if pair[0].range.end > pair[1].range.start || pair[0].range.start == pair[1].range.start
            {
                return Err(TransactionError::OverlappingEdits);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionError {
    StaleRevision,
    InvalidRange,
    OverlappingEdits,
    InvalidSelection,
    HistoryUnavailable,
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
            Self::HistoryUnavailable => "undo provider rejected the transaction",
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

/// Rope document and editing state, independent of RXUI and the graphics host.
/// Newlines are normalized to LF. All caret positions are grapheme boundaries.
struct DocumentState {
    views: std::collections::BTreeMap<u64, views::State>,
    next_view: u64,
    current: Snapshot,
    history: Option<history::Owned>,
    group: Option<history::Group>,
    previous_group: Option<history::Group>,
    merge_group: bool,
    revision: u64,
    document: u64,
    next_state: u64,
    saved: Checkpoint,
    edits: VecDeque<Change>,
    goal_column: Option<usize>,
    boundaries: RefCell<VecDeque<(usize, Arc<Vec<usize>>)>>,
}

impl Default for DocumentState {
    fn default() -> Self {
        Self::new("")
    }
}

impl DocumentState {
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
            views: Default::default(),
            next_view: 1,
            current: Snapshot {
                text,
                selection: Selection::default(),
                state: 0,
            },
            history: None,
            group: None,
            previous_group: None,
            merge_group: false,
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
            self.next_grapheme_boundary(range.end)
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

    pub fn can_undo(&self) -> bool {
        self.history
            .as_ref()
            .is_some_and(|history| history.session.status().can_undo)
    }

    pub fn can_redo(&self) -> bool {
        self.history
            .as_ref()
            .is_some_and(|history| !history.session.status().branches.is_empty())
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

    /// Project a directed selection without changing the preferred grapheme column.
    pub fn set_selection_preserving_column(&mut self, selection: Selection) {
        let length = self.text().len_chars();
        self.current.selection = Selection {
            anchor: self.snap(selection.anchor.min(length)),
            head: self.snap(selection.head.min(length)),
        };
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
            self.previous_grapheme_boundary(range.start)..range.end
        } else {
            range
        };
        self.replace(range, "");
    }

    pub fn delete(&mut self) {
        let range = self.selection().range();
        let range = if range.is_empty() {
            range.start..self.next_grapheme_boundary(range.end)
        } else {
            range
        };
        self.replace(range, "");
    }

    /// Restrict the caret to line characters, retaining the preferred
    /// grapheme column for subsequent vertical movement. Empty lines keep column zero.
    pub fn clamp_caret_to_line_character(&mut self) {
        let head = self.selection().head;
        let text = self.text();
        let line = text.char_to_line(head);
        let start = text.line_to_char(line);
        let mut end = start + text.line(line).len_chars();
        if end > start && text.char(end - 1) == '\n' {
            end -= 1;
        }
        if head < end {
            return;
        }
        let last = if end > start {
            self.previous_grapheme_boundary(end)
        } else {
            start
        };
        if head > last {
            self.current.selection = Selection {
                anchor: last,
                head: last,
            };
        }
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
            self.next_grapheme_boundary(selection.head)
        } else {
            self.previous_grapheme_boundary(selection.head)
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
        self.travel(history::Request::Undo(self.current.state));
    }
    pub fn redo(&mut self) {
        self.travel(history::Request::Redo {
            current: self.current.state,
            branch: None,
        });
    }
    fn redo_branch(&mut self, state: u64) -> bool {
        self.travel(history::Request::Redo {
            current: self.current.state,
            branch: Some(state),
        })
    }
    fn inspect_history(&mut self) -> Result<crate::history::Inspection, String> {
        let tree = match &mut self.history {
            Some(history) => history.session.request(history::Request::Inspect)?.tree,
            None => None,
        };
        if let Some(tree) = &tree {
            let states: std::collections::BTreeSet<_> =
                tree.nodes.iter().map(|n| n.state).collect();
            if tree.nodes.len() > 4097
                || states.len() != tree.nodes.len()
                || !states.contains(&self.current.state)
                || tree.nodes.iter().filter(|n| n.parent.is_none()).count() != 1
                || tree.nodes.iter().any(|n| {
                    n.summary.len() > 256
                        || n.parent
                            .is_some_and(|parent| parent >= n.state || !states.contains(&parent))
                })
            {
                return Err("invalid undo inspection tree".into());
            }
        }
        Ok(crate::history::Inspection {
            current: self.current.state,
            saved: self.saved.state,
            tree,
        })
    }
    fn travel(&mut self, request: history::Request) -> bool {
        let result = (|| {
            let Some(history) = self.history.as_mut() else {
                return Ok(false);
            };
            let Some(step) = history.session.request(request)?.step else {
                return Ok(false);
            };
            if step.source != self.current.state
                || step.target >= self.next_state
                || step.target == step.source
                || step.patches.is_empty()
            {
                return Err("invalid undo target".into());
            }
            let mut text = self.current.text.clone();
            self.revision
                .checked_add(step.patches.len() as u64)
                .ok_or("undo revision overflow")?;
            for patch in &step.patches {
                crate::history::apply_patch(&mut text, patch)?;
            }
            if [step.selection.anchor, step.selection.head]
                .iter()
                .any(|&p| p > text.len_chars() || !graphemes::is_boundary(&text, p))
            {
                return Err("invalid undo selection".into());
            }
            history.session.request(history::Request::Checkout {
                current: step.source,
                target: step.target,
            })?;
            self.current.text = text;
            for patch in step.patches {
                self.revision += 1;
                let mut change = patch.change;
                change.revision = self.revision;
                self.record(change);
            }
            self.current.state = step.target;
            self.current.selection = step.selection;
            self.changed();
            Ok::<_, String>(true)
        })();
        match result {
            Ok(changed) => changed,
            Err(error) => {
                tracing::error!(%error, "undo provider failed");
                false
            }
        }
    }

    fn record(&mut self, edit: Change) {
        for view in self.views.values_mut() {
            view.selection = edit.map_selection(view.selection, crate::Affinity::After);
            view.goal_column = None;
        }
        self.edits.push_back(edit);
        if self.edits.len() > CHANGE_LOG_LIMIT {
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
        transaction.validate_ranges(self.revision, self.text().len_chars())?;
        transaction
            .edits
            .sort_by_key(|edit| (edit.range.start, edit.range.end));
        let mut next = self.current.clone();
        let mut edits = Vec::with_capacity(transaction.edits.len());
        let mut patches = Vec::with_capacity(transaction.edits.len());
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
            let removed: Arc<str> = before.slice(edit.range.clone()).to_string().into();
            edits.push(Change::between(
                self.revision + edits.len() as u64 + 1,
                &before,
                &next.text,
                edit.range,
                inserted,
            ));
            next.selection = edits
                .last()
                .unwrap()
                .map_selection(next.selection, crate::Affinity::After);
            patches.push(history::Patch {
                change: edits.last().unwrap().clone(),
                removed,
                inserted: text.into(),
            });
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
        if let Some(history) = &mut self.history {
            history
                .session
                .request(history::Request::Record(history::Commit {
                    state: next.state,
                    parent: self.current.state,
                    before: self.current.selection,
                    after: next.selection,
                    patches,
                    merge: self.group.is_some()
                        && self.merge_group
                        && self.current.state != self.saved.state,
                }))
                .map_err(|error| {
                    tracing::error!(%error, "undo provider rejected edit");
                    TransactionError::HistoryUnavailable
                })?;
        }
        self.next_state += 1;
        self.current = next;
        for edit in &edits {
            self.record(edit.clone());
        }
        self.revision += edits.len() as u64;
        if self.group.is_some() {
            self.merge_group = true;
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
        .unwrap_or_else(|error| {
            tracing::error!(%error, "text replacement rejected");
            false
        });
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
            self.previous_grapheme_boundary(position)
        }
    }

    /// Previous extended grapheme boundary, clamped at the document edges.
    pub fn previous_grapheme_boundary(&self, position: usize) -> usize {
        graphemes::previous(self.text(), position.min(self.text().len_chars()))
    }

    /// Next extended grapheme boundary, clamped at the document edges.
    pub fn next_grapheme_boundary(&self, position: usize) -> usize {
        graphemes::next(self.text(), position.min(self.text().len_chars()))
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
        let mut buffer = crate::Buffer::with_history(
            "éabc\n👩‍💻xy\nlast",
            std::sync::Arc::new(velk_undo_tree::Tree),
        );
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
