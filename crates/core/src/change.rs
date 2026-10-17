use ropey::Rope;
use std::ops::Range;

/// A UTF-8 byte offset and its zero-based logical line and byte column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Change {
    /// The document revision after the replacement.
    pub revision: u64,
    pub replaced: Range<usize>,
    pub inserted: usize,
    pub start: TextPoint,
    pub old_end: TextPoint,
    pub new_end: TextPoint,
}

/// Where an anchor lands when text is inserted or replaces its position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affinity {
    Before,
    After,
}

impl Change {
    pub fn map_position(&self, position: usize, affinity: Affinity) -> usize {
        if position < self.replaced.start {
            position
        } else if position > self.replaced.end {
            position - self.replaced.len() + self.inserted
        } else if position == self.replaced.end && !self.replaced.is_empty() {
            self.replaced.start + self.inserted
        } else {
            self.replaced.start
                + if affinity == Affinity::After {
                    self.inserted
                } else {
                    0
                }
        }
    }

    pub fn map_selection(
        &self,
        selected: crate::Selection,
        affinity: Affinity,
    ) -> crate::Selection {
        crate::Selection {
            anchor: self.map_position(selected.anchor, affinity),
            head: self.map_position(selected.head, affinity),
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anchors_choose_insertion_affinity_and_replacement_end_remains_after_text() {
        let before = Rope::from_str("abc");
        let after = Rope::from_str("aXYbc");
        let insert = Change::between(1, &before, &after, 1..1, 2);
        assert_eq!(insert.map_position(1, Affinity::Before), 1);
        assert_eq!(insert.map_position(1, Affinity::After), 3);
        assert_eq!(insert.map_position(3, Affinity::After), 5);
        let replace = Change::between(2, &before, &Rope::from_str("Xc"), 0..2, 1);
        assert_eq!(replace.map_position(1, Affinity::Before), 0);
        assert_eq!(replace.map_position(1, Affinity::After), 1);
        assert_eq!(replace.map_position(2, Affinity::Before), 1);
        let selected = crate::Selection { anchor: 3, head: 0 };
        assert_eq!(
            replace.map_selection(selected, Affinity::After),
            crate::Selection { anchor: 2, head: 1 }
        );
    }
}
