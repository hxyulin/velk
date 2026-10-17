//! Shared document coordinates and undo contracts, independent of any engine.
use std::ops::Range;
mod change;
pub mod graphemes;
pub mod history;
pub use change::{Affinity, Change, TextPoint};
/// A directed selection, in Unicode scalar (Ropey character) offsets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
