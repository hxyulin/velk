//! Bundled linked undo-tree engine, independent of plugin lifecycle and RXUI.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use velk_core_contracts::{Selection, history::*};
type HistoryLimits = Limits;
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub parent: u64,
    pub before: Selection,
    pub after: Selection,
    pub patches: Vec<Patch>,
    #[serde(skip)]
    bytes: usize,
    #[serde(skip)]
    inserted: usize,
    #[serde(skip)]
    removed: usize,
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
pub(crate) struct RamHistory {
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
            entry.inserted = entry
                .patches
                .iter()
                .map(|p| p.inserted.chars().count())
                .sum();
            entry.removed = entry
                .patches
                .iter()
                .map(|p| p.removed.chars().count())
                .sum();
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
        let inserted = patches
            .iter()
            .map(|p| p.inserted.chars().count())
            .sum::<usize>();
        let removed = patches
            .iter()
            .map(|p| p.removed.chars().count())
            .sum::<usize>();
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
                entry.inserted += inserted;
                entry.removed += removed;
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
            inserted,
            removed,
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

fn valid_selection(text: &ropey::Rope, selected: Selection) -> Result<(), String> {
    if [selected.anchor, selected.head]
        .iter()
        .any(|&p| p > text.len_chars() || !velk_core_contracts::graphemes::is_boundary(text, p))
    {
        return Err("invalid history selection".into());
    }
    Ok(())
}
fn validate_history(
    state: &Restore,
    history: &RamHistory,
    text: &ropey::Rope,
) -> Result<(), String> {
    if history.entries.len() > history.limits.entries
        || history.bytes() > history.limits.bytes
        || history.root >= state.next_state
        || history.entries.iter().any(|(&id, entry)| {
            id >= state.next_state
                || !history.preferred.contains_key(&entry.parent)
                || id <= entry.parent
                || entry.parent < history.root
                || entry.patches.is_empty()
                || entry.patches.iter().any(|patch| {
                    patch.change.replaced.start > patch.change.replaced.end
                        || patch.change.replaced.len() != patch.removed.chars().count()
                        || patch.change.inserted != patch.inserted.chars().count()
                        || patch
                            .change
                            .replaced
                            .start
                            .checked_add(patch.change.inserted)
                            .is_none()
                })
        })
        || history.preferred.iter().any(|(&parent, child)| {
            history
                .entries
                .get(child)
                .is_none_or(|entry| entry.parent != parent)
        })
    {
        return Err("invalid history tree".into());
    }
    let changes = history
        .entries
        .values()
        .try_fold(0u64, |sum, entry| {
            sum.checked_add(entry.patches.len() as u64)
        })
        .ok_or("history revision overflow")?;
    state
        .revision
        .checked_add(changes)
        .ok_or("history revision overflow")?;
    let mut children = std::collections::BTreeMap::<u64, Vec<_>>::new();
    for (&id, entry) in &history.entries {
        children.entry(entry.parent).or_default().push((id, entry));
    }
    let mut root = text.clone();
    let mut cursor = state.state;
    while cursor != history.root {
        let entry = history
            .entries
            .get(&cursor)
            .ok_or("history current state is disconnected")?;
        for patch in entry.patches.iter().rev() {
            apply_patch(&mut root, &patch.inverse())?;
        }
        cursor = entry.parent;
    }
    let mut queue = vec![(history.root, root)];
    let mut visited = 0;
    while let Some((parent, text)) = queue.pop() {
        for &(id, entry) in children.get(&parent).map_or(&[][..], Vec::as_slice) {
            valid_selection(&text, entry.before)?;
            let mut next = text.clone();
            for patch in &entry.patches {
                apply_patch(&mut next, patch)?;
            }
            valid_selection(&next, entry.after)?;
            queue.push((id, next));
            visited += 1;
        }
    }
    if visited != history.entries.len() {
        return Err("disconnected history branch".into());
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct Tree;
struct SessionState {
    history: RamHistory,
    current: u64,
}
struct Saved(RamHistory);
impl Snapshot for Saved {
    fn encode(&self) -> Result<serde_json::Value, String> {
        serde_json::to_value(&self.0).map_err(|e| e.to_string())
    }
}
impl Provider for Tree {
    fn id(&self) -> &str {
        DEFAULT_PROVIDER
    }
    fn open(&self, root: u64) -> Box<dyn Session> {
        Box::new(SessionState {
            history: RamHistory {
                root,
                ..Default::default()
            },
            current: root,
        })
    }
    fn restore(&self, archive: Restore) -> Result<Box<dyn Session>, String> {
        let mut history: RamHistory =
            serde_json::from_value(archive.data.clone()).map_err(|e| e.to_string())?;
        history.recount();
        validate_history(&archive, &history, &ropey::Rope::from_str(&archive.text))?;
        Ok(Box::new(SessionState {
            history,
            current: archive.state,
        }))
    }
}
impl SessionState {
    fn path(&self, current: u64, target: u64) -> Result<(Vec<u64>, Vec<u64>), String> {
        if current != self.current
            || (target != self.history.root && !self.history.entries.contains_key(&target))
        {
            return Err("history state changed or was pruned".into());
        }
        let mut ancestors = BTreeMap::new();
        let mut up = Vec::new();
        let mut state = current;
        ancestors.insert(state, 0);
        while let Some(entry) = self.history.entries.get(&state) {
            up.push(state);
            state = entry.parent;
            ancestors.insert(state, up.len());
        }
        let mut down = Vec::new();
        let mut state = target;
        while !ancestors.contains_key(&state) {
            down.push(state);
            state = self
                .history
                .entries
                .get(&state)
                .ok_or("disconnected undo history")?
                .parent;
        }
        up.truncate(ancestors[&state]);
        down.reverse();
        Ok((up, down))
    }
    fn inspect(&self) -> velk_core_contracts::history::Tree {
        let mut nodes = vec![Node {
            state: self.history.root,
            parent: None,
            inserted: 0,
            removed: 0,
            summary: "Retained root".into(),
        }];
        nodes.extend(self.history.entries.iter().map(|(&state, entry)| {
            let summary = entry
                .patches
                .iter()
                .flat_map(|p| {
                    if p.inserted.is_empty() {
                        p.removed.chars()
                    } else {
                        p.inserted.chars()
                    }
                })
                .take(48)
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            Node {
                state,
                parent: Some(entry.parent),
                inserted: entry.inserted,
                removed: entry.removed,
                summary,
            }
        }));
        velk_core_contracts::history::Tree { nodes }
    }
}
impl Session for SessionState {
    fn status(&self) -> Status {
        Status {
            can_undo: self.history.entries.contains_key(&self.current),
            branches: self.history.branches(self.current),
            limits: self.history.limits,
            bytes: self.history.bytes(),
        }
    }
    fn snapshot(&self) -> Result<Arc<dyn Snapshot>, String> {
        Ok(Arc::new(Saved(self.history.clone())))
    }
    fn request(&mut self, request: Request) -> Result<Response, String> {
        let mut step = None;
        let mut tree = None;
        match request {
            Request::Status => {}
            Request::Inspect => {
                if self.history.entries.len() > 4096 {
                    return Err("history inspection exceeds 4096 retained edits".into());
                }
                tree = Some(self.inspect());
            }
            Request::Travel { current, target } => {
                let (up, down) = self.path(current, target)?;
                let mut patches = Vec::new();
                let mut selection = Selection::default();
                for state in up {
                    let entry = &self.history.entries[&state];
                    patches.extend(entry.patches.iter().rev().map(Patch::inverse));
                    selection = entry.before;
                }
                for state in down {
                    let entry = &self.history.entries[&state];
                    patches.extend(entry.patches.clone());
                    selection = entry.after;
                }
                if current != target {
                    step = Some(Step {
                        source: current,
                        target,
                        selection,
                        patches,
                    });
                }
            }
            Request::Record(commit) => {
                if commit.parent != self.current || commit.state <= self.current {
                    return Err("undo commit state mismatch".into());
                }
                self.history.commit(
                    commit.state,
                    commit.parent,
                    commit.before,
                    commit.after,
                    commit.patches,
                    commit.merge,
                );
                self.current = commit.state;
            }
            Request::Undo(current) => {
                if current != self.current {
                    return Err("undo state mismatch".into());
                }
                step = self.history.entries.get(&current).map(|entry| Step {
                    source: current,
                    target: entry.parent,
                    selection: entry.before,
                    patches: entry.patches.iter().rev().map(Patch::inverse).collect(),
                });
            }
            Request::Redo { current, branch } => {
                if current != self.current {
                    return Err("redo state mismatch".into());
                }
                let branch = branch.or_else(|| self.history.preferred.get(&current).copied());
                step = branch.and_then(|branch| {
                    self.history
                        .entries
                        .get(&branch)
                        .filter(|entry| entry.parent == current)
                        .map(|entry| Step {
                            source: current,
                            target: branch,
                            selection: entry.after,
                            patches: entry.patches.clone(),
                        })
                });
            }
            Request::Checkout { current, target } => {
                let (up, down) = self.path(current, target)?;
                for state in up.into_iter().chain(down) {
                    self.history
                        .preferred
                        .insert(self.history.entries[&state].parent, state);
                }
                self.current = target;
            }
            Request::Limits { current, limits } => {
                if current != self.current {
                    return Err("limit state mismatch".into());
                }
                self.history.limits = limits;
                self.history.prune(current);
            }
        }
        Ok(Response {
            status: self.status(),
            step,
            tree,
        })
    }
}
