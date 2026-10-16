use ropey::Rope;
use std::ops::Range;

/// A UTF-8 byte offset and its zero-based logical line and byte column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPoint {
    pub byte: usize,
    pub line: usize,
    pub column: usize,
}

impl TextPoint {
    fn at(text: &Rope, character: usize) -> Self {
        let line = text.char_to_line(character);
        let byte = text.char_to_byte(character);
        Self {
            byte,
            line,
            column: byte - text.line_to_byte(line),
        }
    }
}

/// One replacement in a revision sequence. Scalar ranges and byte coordinates
/// describe the states immediately before and after this replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The document revision after the replacement.
    pub revision: u64,
    pub replaced: Range<usize>,
    pub inserted: usize,
    pub start: TextPoint,
    pub old_end: TextPoint,
    pub new_end: TextPoint,
}

impl Change {
    /// Both documents must reflect the supplied replacement in scalar offsets.
    pub fn between(
        revision: u64,
        before: &Rope,
        after: &Rope,
        range: Range<usize>,
        inserted: usize,
    ) -> Self {
        Self {
            revision,
            start: TextPoint::at(before, range.start),
            old_end: TextPoint::at(before, range.end),
            new_end: TextPoint::at(after, range.start + inserted),
            replaced: range,
            inserted,
        }
    }

    pub fn inverse(&self, revision: u64) -> Self {
        Self {
            revision,
            replaced: self.replaced.start..self.replaced.start + self.inserted,
            inserted: self.replaced.len(),
            start: self.start,
            old_end: self.new_end,
            new_end: self.old_end,
        }
    }
}
