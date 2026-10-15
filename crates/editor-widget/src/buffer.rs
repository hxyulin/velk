use ropey::{Rope, RopeBuilder};
use std::{
    cell::RefCell,
    collections::VecDeque,
    io::{self, Read},
    ops::Range,
    sync::Arc,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::graphemes;

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

#[derive(Clone)]
struct Snapshot {
    text: Rope,
    selection: Selection,
}

/// Rope document and editing state, independent of RXUI and the graphics host.
/// Newlines are normalized to LF. All caret positions are grapheme boundaries.
pub struct Buffer {
    current: Snapshot,
    undo: VecDeque<Snapshot>,
    redo: Vec<Snapshot>,
    revision: u64,
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
        Self {
            current: Snapshot {
                text,
                selection: Selection::default(),
            },
            undo: VecDeque::new(),
            redo: Vec::new(),
            revision: 0,
            goal_column: None,
            boundaries: RefCell::new(VecDeque::new()),
        }
    }

    pub fn text(&self) -> &Rope {
        &self.current.text
    }

    pub(crate) fn is_grapheme_boundary(&self, position: usize) -> bool {
        position <= self.text().len_chars() && graphemes::is_boundary(self.text(), position)
    }

    pub fn selection(&self) -> Selection {
        self.current.selection
    }

    pub fn revision(&self) -> u64 {
        self.revision
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

    pub fn undo(&mut self) {
        if let Some(previous) = self.undo.pop_back() {
            self.redo
                .push(std::mem::replace(&mut self.current, previous));
            self.changed();
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo
                .push_back(std::mem::replace(&mut self.current, next));
            self.changed();
        }
    }

    fn replace(&mut self, range: Range<usize>, text: &str) {
        if range.is_empty() && text.is_empty() {
            return;
        }
        self.undo.push_back(self.current.clone());
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.pop_front();
        }
        self.redo.clear();
        // Cache entries use absolute character offsets and logical line numbers.
        // Entries before the first changed line keep both their contents and
        // offsets; the edited line and all later entries must be discarded.
        let first_line = self.text().char_to_line(range.start);
        self.boundaries
            .get_mut()
            .retain(|(line, _)| *line < first_line);
        self.current.text.remove(range.clone());
        self.current.text.insert(range.start, text);
        let end = range.start + text.chars().count();
        // Insertions can join neighboring combining marks/ZWJ sequences. Advance
        // to the end of that grapheme rather than leaving a caret inside it.
        let head = if graphemes::is_boundary(self.text(), end) {
            end
        } else {
            self.next(end)
        };
        self.current.selection = Selection { anchor: head, head };
        self.revision += 1;
        self.goal_column = None;
    }

    fn changed(&mut self) {
        self.boundaries.get_mut().clear();
        self.revision += 1;
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
