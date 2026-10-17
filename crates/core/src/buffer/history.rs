use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Limits retained undo data, excluding live text and snapshots held by callers.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryLimits {
    pub entries: usize,
    pub bytes: usize,
}
impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            entries: 128,
            bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RedoBranch {
    pub state: u64,
    pub preferred: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Patch {
    pub change: Change,
    pub removed: Arc<str>,
    pub inserted: Arc<str>,
}
impl Patch {
    pub fn bytes(&self) -> usize {
        self.removed
            .len()
            .saturating_add(self.inserted.len())
            .saturating_add(std::mem::size_of::<Self>())
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Entry {
    pub parent: u64,
    pub before: Selection,
    pub after: Selection,
    pub patches: Vec<Patch>,
    #[serde(skip)]
    bytes: usize,
}
impl Entry {
    fn measure(&self) -> usize {
        self.patches
            .iter()
            .fold(std::mem::size_of::<Self>(), |sum, patch| {
                sum.saturating_add(patch.bytes())
            })
    }
}
/// Reversible patches form a tree. Only the current document retains a Rope.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct RamHistory {
    pub root: u64,
    pub entries: BTreeMap<u64, Arc<Entry>>,
    pub preferred: BTreeMap<u64, u64>,
    pub limits: HistoryLimits,
    #[serde(skip)]
    bytes: usize,
}
impl RamHistory {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn recount(&mut self) {
        self.bytes = 0;
        for entry in self.entries.values_mut() {
            let entry = Arc::make_mut(entry);
            entry.bytes = entry.measure();
            self.bytes = self.bytes.saturating_add(entry.bytes);
        }
    }
    pub fn branches(&self, state: u64) -> Vec<RedoBranch> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.parent == state)
            .map(|(&id, _)| RedoBranch {
                state: id,
                preferred: self.preferred.get(&state) == Some(&id),
            })
            .collect()
    }
    pub fn commit(
        &mut self,
        state: u64,
        parent: u64,
        before: Selection,
        after: Selection,
        patches: Vec<Patch>,
        merge: bool,
    ) {
        let patch_bytes = patches
            .iter()
            .fold(0usize, |sum, patch| sum.saturating_add(patch.bytes()));
        let entry = if merge && self.branches(parent).is_empty() {
            self.entries.remove(&parent).map(|old| {
                self.preferred.remove(&old.parent);
                self.bytes = self.bytes.saturating_sub(old.bytes);
                let mut entry = Arc::unwrap_or_clone(old);
                entry.bytes = entry.bytes.saturating_add(patch_bytes);
                entry.after = after;
                entry.patches.extend(patches.iter().cloned());
                entry
            })
        } else {
            None
        };
        let entry = entry.unwrap_or(Entry {
            parent,
            before,
            after,
            patches,
            bytes: patch_bytes.saturating_add(std::mem::size_of::<Entry>()),
        });
        self.bytes = self.bytes.saturating_add(entry.bytes);
        self.preferred.insert(entry.parent, state);
        self.entries.insert(state, Arc::new(entry));
        self.prune(state);
    }
    pub fn prune(&mut self, current: u64) {
        while self.entries.len() > self.limits.entries || self.bytes() > self.limits.bytes {
            let mut path = std::collections::BTreeSet::new();
            let mut cursor = current;
            while let Some(entry) = self.entries.get(&cursor) {
                path.insert(cursor);
                cursor = entry.parent;
            }
            let leaf = self.entries.keys().copied().find(|id| {
                !path.contains(id) && !self.entries.values().any(|entry| entry.parent == *id)
            });
            if let Some(leaf) = leaf {
                if let Some(entry) = self.entries.remove(&leaf) {
                    self.bytes = self.bytes.saturating_sub(entry.bytes);
                    if self.preferred.get(&entry.parent) == Some(&leaf) {
                        if let Some((&child, _)) = self
                            .entries
                            .iter()
                            .rev()
                            .find(|(_, remaining)| remaining.parent == entry.parent)
                        {
                            self.preferred.insert(entry.parent, child);
                        } else {
                            self.preferred.remove(&entry.parent);
                        }
                    }
                }
            } else if let Some((&id, _)) = self
                .entries
                .iter()
                .find(|(_, entry)| entry.parent == self.root)
            {
                self.preferred.remove(&self.root);
                self.root = id;
                if let Some(entry) = self.entries.remove(&id) {
                    self.bytes = self.bytes.saturating_sub(entry.bytes);
                }
            } else {
                break;
            }
        }
    }
}

/// Consecutive compatible input can share one undo step. Commands outside an
/// explicit group always remain independent transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoGroup {
    Typing,
    Backspace,
    Delete,
}
pub(super) struct Group {
    pub view: u64,
    pub kind: UndoGroup,
    pub state: u64,
    pub selection: Selection,
    pub time: std::time::Instant,
}
