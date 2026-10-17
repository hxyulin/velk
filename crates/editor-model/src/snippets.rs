//! View-local snippet navigation; authoritative text remains in Core.
use velk_commands::snippet::{Tabstop, validate_stops};
use velk_core::{Affinity, Buffer, Selection};
pub struct Session {
    stops: Vec<Tabstop>,
    index: u32,
    revision: u64,
}
impl Session {
    pub fn new(buffer: &Buffer, stops: Vec<Tabstop>) -> Result<Self, String> {
        validate_stops(&stops, buffer.text().len_chars())?;
        let index = stops
            .iter()
            .filter(|stop| stop.index != 0)
            .map(|stop| stop.index)
            .min()
            .unwrap_or(0);
        Ok(Self {
            stops,
            index,
            revision: buffer.revision(),
        })
    }
    pub fn selections(&self) -> Vec<Selection> {
        let mut ranges: Vec<_> = self
            .stops
            .iter()
            .filter(|stop| stop.index == self.index)
            .map(|stop| Selection {
                anchor: stop.range.start,
                head: stop.range.end,
            })
            .collect();
        ranges.sort_by_key(|selection| selection.anchor);
        ranges
    }
    pub fn finished(&self) -> bool {
        self.index == 0
    }
    pub fn advance(&mut self, backwards: bool) {
        let mut indices: Vec<_> = self
            .stops
            .iter()
            .map(|stop| stop.index)
            .filter(|index| *index != 0)
            .collect();
        indices.sort_unstable();
        indices.dedup();
        indices.push(0);
        let current = indices
            .iter()
            .position(|index| *index == self.index)
            .unwrap_or(0);
        self.index = indices[if backwards {
            current.saturating_sub(1)
        } else {
            (current + 1).min(indices.len() - 1)
        }];
    }
    pub fn synchronize(&mut self, buffer: &Buffer) -> bool {
        let Some(changes) = buffer.edits_since(self.revision) else {
            return false;
        };
        for change in changes {
            if !self.stops.iter().any(|stop| {
                stop.index == self.index
                    && change.replaced.start >= stop.range.start
                    && change.replaced.end <= stop.range.end
            }) {
                return false;
            }
            // Replacing a parent default removes its nested navigation targets.
            self.stops.retain(|stop| {
                stop.index == self.index
                    || stop.index == 0
                    || !(change.replaced.start <= stop.range.start
                        && change.replaced.end >= stop.range.end
                        && !change.replaced.is_empty())
            });
            for stop in &mut self.stops {
                let active = stop.index == self.index;
                let contains = !stop.range.is_empty()
                    && stop.range.start <= change.replaced.start
                    && stop.range.end >= change.replaced.end;
                stop.range.start = change.map_position(
                    stop.range.start,
                    if active || contains {
                        Affinity::Before
                    } else {
                        Affinity::After
                    },
                );
                stop.range.end = change.map_position(
                    stop.range.end,
                    if !active && !contains && stop.range.end <= change.replaced.start {
                        Affinity::Before
                    } else {
                        Affinity::After
                    },
                );
            }
        }
        self.revision = buffer.revision();
        let selection = buffer.selection().range();
        self.stops.iter().any(|stop| {
            stop.index == self.index
                && selection.start >= stop.range.start
                && selection.end <= stop.range.end
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_tracks_unicode_edits_and_rejects_changes_outside_the_active_stop() {
        let mut buffer = Buffer::new("call(世界, next)");
        let mut session = Session::new(
            &buffer,
            vec![
                Tabstop {
                    index: 1,
                    range: 5..7,
                },
                Tabstop {
                    index: 2,
                    range: 9..13,
                },
                Tabstop {
                    index: 0,
                    range: 14..14,
                },
            ],
        )
        .unwrap();
        buffer.set_selection_preserving_column(session.selections()[0]);
        buffer.insert("longer");
        assert!(session.synchronize(&buffer));
        session.advance(false);
        assert_eq!(session.selections()[0].range(), 13..17);
        session.advance(true);
        assert_eq!(session.selections()[0].range(), 5..11);
        buffer.set_caret(0, false);
        assert!(!session.synchronize(&buffer));
    }
}
