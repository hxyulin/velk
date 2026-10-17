//! Bounded navigation history whose anchors follow document edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub buffer: u64,
    pub position: usize,
}
#[derive(Default)]
pub struct History {
    back: Vec<Anchor>,
    forward: Vec<Anchor>,
}
impl History {
    pub fn record(&mut self, origin: Anchor) {
        if self.back.last() != Some(&origin) {
            self.back.push(origin);
        }
        if self.back.len() > 128 {
            self.back.remove(0);
        }
        self.forward.clear();
    }
    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }
    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }
    pub fn back(&mut self, current: Anchor) -> Option<Anchor> {
        let next = self.back.pop()?;
        self.forward.push(current);
        Some(next)
    }
    pub fn forward(&mut self, current: Anchor) -> Option<Anchor> {
        let next = self.forward.pop()?;
        self.back.push(current);
        Some(next)
    }
    pub fn retain(&mut self, mut exists: impl FnMut(u64) -> bool) {
        self.back.retain(|a| exists(a.buffer));
        self.forward.retain(|a| exists(a.buffer));
    }
    pub fn edited(&mut self, buffer: u64, changes: Option<&[velk_core::Change]>) {
        let Some(changes) = changes else {
            self.back.retain(|a| a.buffer != buffer);
            self.forward.retain(|a| a.buffer != buffer);
            return;
        };
        for anchor in self
            .back
            .iter_mut()
            .chain(&mut self.forward)
            .filter(|a| a.buffer == buffer)
        {
            for edit in changes {
                anchor.position = edit.map_position(anchor.position, velk_core::Affinity::After);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_follows_edits_drops_closed_buffers_and_truncates_forward_branch() {
        let mut h = History::default();
        let a = Anchor {
            buffer: 1,
            position: 4,
        };
        let b = Anchor {
            buffer: 2,
            position: 0,
        };
        h.record(a);
        assert_eq!(h.back(b), Some(a));
        assert_eq!(h.forward(a), Some(b));
        let before = ropey::Rope::from_str("abcdef");
        let after = ropey::Rope::from_str("xxabcdef");
        h.edited(
            1,
            Some(&[velk_core::Change::between(1, &before, &after, 0..0, 2)]),
        );
        assert_eq!(h.back(b), Some(Anchor { position: 6, ..a }));
        h.record(a);
        assert!(!h.can_forward());
        h.retain(|id| id != 1);
        assert!(!h.can_back());
    }
}
