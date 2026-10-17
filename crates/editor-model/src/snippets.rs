//! View-local snippet navigation; authoritative text remains in Core.
use velk_commands::snippet::{Tabstop, validate_stops};
use velk_core::{Affinity, Buffer, Selection};
pub struct Session {
    provider: Option<std::sync::Arc<dyn velk_commands::snippet::Provider>>,
    stops: Vec<Tabstop>,
    index: u32,
    revision: u64,
    choices_open: bool,
}
impl Session {
    pub fn new(
        buffer: &Buffer,
        stops: Vec<Tabstop>,
        provider: Option<std::sync::Arc<dyn velk_commands::snippet::Provider>>,
    ) -> Result<Self, String> {
        validate_stops(&stops, buffer.text().len_chars())?;
        let index = stops
            .iter()
            .filter(|stop| stop.index != 0 && stop.transform.is_none())
            .map(|stop| stop.index)
            .min()
            .unwrap_or(0);
        Ok(Self {
            stops,
            provider,
            index,
            revision: buffer.revision(),
            choices_open: true,
        })
    }
    pub fn selections(&self) -> Vec<Selection> {
        let mut ranges: Vec<_> = self
            .stops
            .iter()
            .filter(|stop| stop.index == self.index && stop.transform.is_none())
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
        self.choices_open = true;
        let mut indices: Vec<_> = self
            .stops
            .iter()
            .filter(|stop| stop.transform.is_none())
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
                    && stop.transform.is_none()
                    && change.replaced.start >= stop.range.start
                    && change.replaced.end <= stop.range.end
            }) {
                return false;
            }
            self.map_change(&change, true);
            self.choices_open = false;
        }
        self.revision = buffer.revision();
        let selection = buffer.selection().range();
        self.stops.iter().any(|stop| {
            stop.index == self.index
                && stop.transform.is_none()
                && selection.start >= stop.range.start
                && selection.end <= stop.range.end
        })
    }
    fn map_change(&mut self, change: &velk_core::Change, remove_nested: bool) {
        // Replacing a parent default removes its nested navigation targets.
        if remove_nested {
            self.stops.retain(|stop| {
                stop.index == self.index
                    || stop.index == 0
                    || !(change.replaced.start <= stop.range.start
                        && change.replaced.end >= stop.range.end
                        && !change.replaced.is_empty())
            });
        }
        for stop in &mut self.stops {
            let active = stop.index == self.index && stop.transform.is_none();
            let contains = stop.range == change.replaced
                || !stop.range.is_empty()
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
    pub fn choices(&self, buffer: &Buffer) -> Option<(Vec<String>, usize)> {
        if !self.choices_open {
            return None;
        }
        let stop = self.stops.iter().find(|stop| {
            stop.index == self.index && stop.transform.is_none() && !stop.choices.is_empty()
        })?;
        let value = buffer.text().slice(stop.range.clone()).to_string();
        let selected = stop
            .choices
            .iter()
            .position(|choice| choice == &value)
            .unwrap_or(0);
        Some((stop.choices.clone(), selected))
    }
    pub fn reopen_choices(&mut self) {
        self.choices_open = true;
    }
    pub fn close_choices(&mut self) {
        self.choices_open = false;
    }
    pub fn apply_transforms(&mut self, buffer: &mut Buffer) -> Result<(), String> {
        let Some(source) = self
            .stops
            .iter()
            .find(|stop| stop.index == self.index && stop.transform.is_none())
        else {
            return Ok(());
        };
        let value = buffer.text().slice(source.range.clone()).to_string();
        let mut edits = Vec::new();
        for stop in self.stops.iter().filter(|stop| stop.index == self.index) {
            if let Some(transform) = &stop.transform {
                let text = self
                    .provider
                    .as_ref()
                    .ok_or("snippet provider unavailable")?
                    .transform(transform, &value)?;
                if text.len() > 64 * 1024 || text.contains('\r') {
                    return Err("invalid snippet transform output".into());
                }
                if buffer.text().slice(stop.range.clone()) != text {
                    edits.push(velk_core::TextEdit {
                        range: stop.range.clone(),
                        text,
                    });
                }
            }
        }
        if edits.is_empty() {
            return Ok(());
        }
        buffer
            .apply_transaction(velk_core::Transaction {
                revision: buffer.revision(),
                edits,
                selection: None,
            })
            .map_err(|e| e.to_string())?;
        for change in buffer
            .edits_since(self.revision)
            .ok_or("snippet transform changes expired")?
        {
            self.map_change(&change, false);
        }
        self.revision = buffer.revision();
        Ok(())
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
                    choices: vec![],
                    transform: None,
                    index: 1,
                    range: 5..7,
                },
                Tabstop {
                    choices: vec![],
                    transform: None,
                    index: 2,
                    range: 9..13,
                },
                Tabstop {
                    choices: vec![],
                    transform: None,
                    index: 0,
                    range: 14..14,
                },
            ],
            None,
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
