use ropey::Rope;
use std::{collections::VecDeque, ops::Range};
use unicode_segmentation::UnicodeSegmentation;

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
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new("")
    }
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        Self {
            current: Snapshot {
                text: Rope::from_str(&normalize(text)),
                selection: Selection::default(),
            },
            undo: VecDeque::new(),
            redo: Vec::new(),
            revision: 0,
            goal_column: None,
        }
    }

    pub fn text(&self) -> &Rope {
        &self.current.text
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
        let boundaries = self.line_boundaries(line);
        self.set_caret(
            if end {
                *boundaries.last().unwrap()
            } else {
                boundaries[0]
            },
            extend,
        );
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
        self.current.text.remove(range.clone());
        self.current.text.insert(range.start, text);
        let end = range.start + text.chars().count();
        // Insertions can join neighboring combining marks/ZWJ sequences. Advance
        // to the end of that grapheme rather than leaving a caret inside it.
        let snapped = self.snap(end);
        let head = if snapped == end {
            end
        } else {
            self.next(snapped)
        };
        self.current.selection = Selection { anchor: head, head };
        self.changed();
    }

    fn changed(&mut self) {
        self.revision += 1;
        self.goal_column = None;
    }

    // Only the current logical line is materialized for segmentation, never the
    // full document. This also handles graphemes spanning Ropey's chunk edges.
    fn line_boundaries(&self, line: usize) -> Vec<usize> {
        let start = self.text().line_to_char(line);
        let text = self.text().line(line).to_string();
        let text = text.strip_suffix('\n').unwrap_or(&text);
        let mut offset = start;
        let mut result = vec![start];
        for grapheme in text.graphemes(true) {
            offset += grapheme.chars().count();
            result.push(offset);
        }
        result
    }

    fn snap(&self, position: usize) -> usize {
        let boundaries = self.line_boundaries(self.text().char_to_line(position));
        boundaries[boundaries
            .partition_point(|&p| p <= position)
            .saturating_sub(1)]
    }

    fn previous(&self, position: usize) -> usize {
        let line = self.text().char_to_line(position);
        if position == self.text().line_to_char(line) {
            return position.saturating_sub(1);
        }
        let boundaries = self.line_boundaries(line);
        boundaries[boundaries
            .partition_point(|&p| p < position)
            .saturating_sub(1)]
    }

    fn next(&self, position: usize) -> usize {
        let boundaries = self.line_boundaries(self.text().char_to_line(position));
        boundaries
            .get(boundaries.partition_point(|&p| p <= position))
            .copied()
            .unwrap_or_else(|| (position + 1).min(self.text().len_chars()))
    }
}

pub(crate) fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}
