//! Disjoint document selections share one Core transaction and one primary caret.
use std::sync::Arc;
use velk_core::{Buffer, Selection, TextEdit, Transaction};

pub const MAX_SELECTIONS: usize = 4096;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionRegion {
    /// Directed source range, with an exclusive end, in Unicode scalar offsets.
    pub selection: Selection,
    /// Logical caret; may differ from the exclusive selected-range endpoint.
    pub caret: usize,
}
#[derive(Clone)]
pub(crate) struct SelectionSet {
    pub regions: Arc<Vec<SelectionRegion>>,
    pub primary: usize,
    revision: u64,
}
impl SelectionSet {
    pub fn new(
        buffer: &Buffer,
        regions: Vec<SelectionRegion>,
        primary: usize,
    ) -> Result<Self, String> {
        if regions.is_empty()
            || regions.len() > MAX_SELECTIONS
            || primary >= regions.len()
            || regions.iter().any(|region| {
                region.selection.range().end > buffer.text().len_chars()
                    || region.caret > buffer.text().len_chars()
            })
            || regions.windows(2).any(|pair| {
                pair[0].selection.range().end > pair[1].selection.range().start
                    || pair[0].caret >= pair[1].caret
            })
        {
            return Err("invalid or oversized selection set".into());
        }
        Ok(Self {
            regions: Arc::new(regions),
            primary,
            revision: buffer.revision(),
        })
    }
    pub fn synchronize(&mut self, buffer: &Buffer) -> bool {
        if self.revision == buffer.revision() {
            return self.valid(buffer);
        }
        let Some(changes) = buffer.edits_since(self.revision) else {
            return false;
        };
        let text = buffer.text();
        let snap = |position: usize| {
            let position = position.min(text.len_chars());
            if velk_core::graphemes::is_boundary(&text, position) {
                position
            } else {
                buffer.next_grapheme_boundary(position)
            }
        };
        let mut regions = (*self.regions).clone();
        for region in &mut regions {
            for change in &changes {
                region.selection =
                    change.map_selection(region.selection, velk_core::Affinity::After);
                region.caret = change.map_position(region.caret, velk_core::Affinity::After);
            }
            region.selection.anchor = snap(region.selection.anchor);
            region.selection.head = snap(region.selection.head);
            region.caret = snap(region.caret);
        }
        let Ok(next) = Self::new(buffer, regions, self.primary) else {
            return false;
        };
        if !next.valid(buffer) {
            return false;
        }
        *self = next;
        true
    }
    pub fn valid(&self, buffer: &Buffer) -> bool {
        buffer.revision() == self.revision
            && buffer.selection() == self.regions[self.primary].selection
    }
    pub fn project(&self, buffer: &mut Buffer) {
        buffer.set_selection_preserving_column(self.regions[self.primary].selection);
    }
    pub fn collapse(&mut self, buffer: &mut Buffer, after: bool) {
        let regions = Arc::make_mut(&mut self.regions);
        for region in regions {
            let range = region.selection.range();
            let caret = if after { range.end } else { range.start };
            *region = SelectionRegion {
                selection: Selection {
                    anchor: caret,
                    head: caret,
                },
                caret,
            };
        }
        self.project(buffer);
    }
    pub fn selected_text(&self, buffer: &Buffer) -> String {
        self.regions
            .iter()
            .map(|region| buffer.text().slice(region.selection.range()).to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub fn replace(&mut self, buffer: &mut Buffer, text: &str) -> Result<(), String> {
        self.edit(buffer, text, None, false)
    }
    pub fn paste(&mut self, buffer: &mut Buffer, text: &str) -> Result<(), String> {
        self.edit(buffer, text, None, true)
    }
    pub fn erase(&mut self, buffer: &mut Buffer, backwards: bool) -> Result<(), String> {
        self.edit(buffer, "", Some(backwards), false)
    }
    fn edit(
        &mut self,
        buffer: &mut Buffer,
        text: &str,
        erase: Option<bool>,
        paste: bool,
    ) -> Result<(), String> {
        if text.len() > velk_editing::MAX_WIRE {
            return Err("multi-selection insertion exceeds 4 MiB".into());
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let distribute =
            paste && text.bytes().filter(|byte| *byte == b'\n').count() + 1 == self.regions.len();
        let rows: Vec<_> = if distribute {
            text.split('\n').collect()
        } else {
            vec![]
        };
        let bytes = if distribute {
            Some(rows.iter().map(|row| row.len()).sum())
        } else {
            text.len().checked_mul(self.regions.len())
        };
        if bytes.is_none_or(|bytes| bytes > velk_editing::MAX_WIRE) {
            return Err("multi-selection insertion exceeds 4 MiB".into());
        }
        let mut removed = 0;
        let mut inserted = 0;
        let mut edits = Vec::new();
        let mut regions = Vec::with_capacity(self.regions.len());
        for (index, region) in self.regions.iter().enumerate() {
            let text = if distribute {
                rows[index]
            } else {
                text.as_str()
            };
            let length = text.chars().count();
            let mut range = region.selection.range();
            if range.is_empty() {
                if erase == Some(true)
                    && range.start > 0
                    && buffer.text().char(range.start - 1) != '\n'
                {
                    range.start = buffer.previous_grapheme_boundary(range.start);
                } else if erase == Some(false)
                    && range.end < buffer.text().len_chars()
                    && buffer.text().char(range.end) != '\n'
                {
                    range.end = buffer.next_grapheme_boundary(range.end);
                }
            }
            let caret = range.start - removed + inserted + length;
            removed += range.len();
            inserted += length;
            if !range.is_empty() || !text.is_empty() {
                edits.push(TextEdit {
                    range,
                    text: text.into(),
                });
            }
            regions.push(SelectionRegion {
                selection: Selection {
                    anchor: caret,
                    head: caret,
                },
                caret,
            });
        }
        let primary = regions[self.primary].selection;
        let caret = self.regions[self.primary].caret;
        if buffer.selection()
            != (Selection {
                anchor: caret,
                head: caret,
            })
        {
            buffer.set_caret(caret, false);
        }
        buffer
            .apply_transaction(Transaction {
                revision: buffer.revision(),
                edits,
                selection: Some(primary),
            })
            .map_err(|e| e.to_string())?;
        for region in &mut regions {
            if !velk_core::graphemes::is_boundary(&buffer.text(), region.caret) {
                let caret = buffer.next_grapheme_boundary(region.caret);
                *region = SelectionRegion {
                    selection: Selection {
                        anchor: caret,
                        head: caret,
                    },
                    caret,
                };
            }
        }
        self.regions = Arc::new(regions);
        self.revision = buffer.revision();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn caret(position: usize) -> SelectionRegion {
        SelectionRegion {
            selection: Selection {
                anchor: position,
                head: position,
            },
            caret: position,
        }
    }
    #[test]
    fn consecutive_multi_caret_typing_can_share_an_undo_group() {
        let mut buffer = Buffer::new("\n");
        let mut set = SelectionSet::new(&buffer, vec![caret(0), caret(1)], 0).unwrap();
        set.project(&mut buffer);
        for text in ["a", "b"] {
            buffer.begin_undo_group(velk_core::UndoGroup::Typing);
            set.replace(&mut buffer, text).unwrap();
            buffer.end_undo_group();
        }
        assert_eq!(buffer.text().to_string(), "ab\nab");
        buffer.undo();
        assert_eq!(buffer.text().to_string(), "\n");
    }
    #[test]
    fn peer_edits_rebase_secondary_carets_and_expired_changes_invalidate_them() {
        let mut buffer = Buffer::new("a\nb");
        let mut set = SelectionSet::new(&buffer, vec![caret(0), caret(2)], 0).unwrap();
        set.project(&mut buffer);
        let mut peer = buffer.new_view();
        peer.set_caret(0, false);
        peer.insert("prefix\n");
        assert!(set.synchronize(&buffer));
        assert_eq!(set.regions[0].caret, 7);
        assert_eq!(set.regions[1].caret, 9);
        set.replace(&mut buffer, "X").unwrap();
        assert_eq!(buffer.text().to_string(), "prefix\nXa\nXb");
        for _ in 0..129 {
            peer.insert("z");
        }
        assert!(!set.synchronize(&buffer));
    }
    #[test]
    fn overlapping_selections_after_peer_deletion_are_invalidated() {
        let mut buffer = Buffer::new("a\nb");
        let mut set = SelectionSet::new(&buffer, vec![caret(0), caret(2)], 0).unwrap();
        set.project(&mut buffer);
        let mut peer = buffer.new_view();
        peer.select_all();
        peer.insert("");
        assert!(!set.synchronize(&buffer));
    }
    #[test]
    fn multi_selection_edits_snap_each_caret_after_grapheme_joining() {
        let mut buffer = Buffer::new("e\u{301}\nx\u{301}");
        let mut set = SelectionSet::new(&buffer, vec![caret(0), caret(3)], 0).unwrap();
        set.project(&mut buffer);
        set.replace(&mut buffer, "\u{600}").unwrap();
        assert!(set.valid(&buffer));
        assert_eq!(set.regions[0].caret, 3);
        assert_eq!(set.regions[1].caret, 7);
        set.erase(&mut buffer, true).unwrap();
        assert_eq!(buffer.text().to_string(), "\n");
        buffer.undo();
        assert_eq!(
            buffer.text().to_string(),
            "\u{600}e\u{301}\n\u{600}x\u{301}"
        );
    }
    #[test]
    fn invalid_and_oversized_selection_operations_leave_text_and_selection_unchanged() {
        let mut buffer = Buffer::new("a\nb");
        assert!(SelectionSet::new(&buffer, vec![caret(2), caret(0)], 0).is_err());
        assert!(SelectionSet::new(&buffer, vec![caret(0)], 1).is_err());
        let mut set = SelectionSet::new(&buffer, vec![caret(0), caret(2)], 0).unwrap();
        set.project(&mut buffer);
        let selection = buffer.selection();
        assert!(
            set.replace(&mut buffer, &"x".repeat(velk_editing::MAX_WIRE))
                .is_err()
        );
        assert_eq!(buffer.text().to_string(), "a\nb");
        assert_eq!(buffer.selection(), selection);
        assert!(!buffer.can_undo());
        assert!(set.valid(&buffer));
    }
}
