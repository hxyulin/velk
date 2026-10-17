//! Renderer-independent open-document identity and tab selection.
//!
//! Paths must already be resolved by the filesystem adapter. The model performs
//! no I/O, so tests and other frontends can supply deterministic path identities.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub const MAX_DOCUMENTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    Existing(u64),
    Created(u64),
}
impl Open {
    pub fn id(&self) -> u64 {
        match self {
            Self::Existing(id) | Self::Created(id) => *id,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Documents {
    paths: BTreeMap<u64, Option<PathBuf>>,
    order: Vec<u64>,
    active: Option<u64>,
    next: u64,
}
impl Default for Documents {
    fn default() -> Self {
        Self::new(1)
    }
}
impl Documents {
    pub fn new(next: u64) -> Self {
        Self {
            paths: BTreeMap::new(),
            order: Vec::new(),
            active: None,
            next: next.max(1),
        }
    }
    pub fn ids(&self) -> &[u64] {
        &self.order
    }
    pub fn active(&self) -> Option<u64> {
        self.active
    }
    pub fn next_id(&self) -> u64 {
        self.next
    }
    pub fn len(&self) -> usize {
        self.order.len()
    }
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
    pub fn contains(&self, id: u64) -> bool {
        self.paths.contains_key(&id)
    }
    pub fn path(&self, id: u64) -> Option<&Path> {
        self.paths.get(&id).and_then(|path| path.as_deref())
    }
    pub fn find_path(&self, path: &Path) -> Option<u64> {
        self.paths
            .iter()
            .find_map(|(id, candidate)| (candidate.as_deref() == Some(path)).then_some(*id))
    }
    pub fn open(&mut self, path: Option<PathBuf>) -> Result<Open, String> {
        if let Some(id) = path.as_deref().and_then(|path| self.find_path(path)) {
            self.active = Some(id);
            return Ok(Open::Existing(id));
        }
        let id = self.next;
        self.restore(id, path)?;
        self.active = Some(id);
        Ok(Open::Created(id))
    }
    /// Register an already validated recovered document without changing focus.
    pub fn restore(&mut self, id: u64, path: Option<PathBuf>) -> Result<(), String> {
        if id == 0 || id == u64::MAX || self.contains(id) {
            return Err("Invalid or duplicate document identity".into());
        }
        if self.len() >= MAX_DOCUMENTS {
            return Err("This workspace already has 256 open documents.".into());
        }
        if path
            .as_deref()
            .is_some_and(|path| self.find_path(path).is_some())
        {
            return Err("A document with this path is already open".into());
        }
        self.next = self.next.max(id + 1);
        self.paths.insert(id, path);
        self.order.push(id);
        if self.active.is_none() {
            self.active = Some(id);
        }
        Ok(())
    }
    pub fn reserve_ids(&mut self, next: u64) {
        self.next = self.next.max(next);
    }
    pub fn select(&mut self, id: u64) -> bool {
        if !self.contains(id) {
            return false;
        }
        self.active = Some(id);
        true
    }
    pub fn adjacent(&self, backwards: bool) -> Option<u64> {
        let index = self.order.iter().position(|id| Some(*id) == self.active)?;
        let offset = if backwards { self.len() - 1 } else { 1 };
        Some(self.order[(index + offset) % self.len()])
    }
    /// Close identity and select its right neighbor, or left neighbor at the end.
    pub fn close(&mut self, id: u64) -> bool {
        let Some(index) = self.order.iter().position(|other| *other == id) else {
            return false;
        };
        self.paths.remove(&id);
        self.order.remove(index);
        if self.active == Some(id) {
            self.active = self
                .order
                .get(index.min(self.len().saturating_sub(1)))
                .copied();
        }
        true
    }
    pub fn rename(&mut self, id: u64, path: Option<PathBuf>) -> Result<(), String> {
        if !self.contains(id) {
            return Err("Document was closed".into());
        }
        if path
            .as_deref()
            .and_then(|path| self.find_path(path))
            .is_some_and(|other| other != id)
        {
            return Err("A document with this path is already open".into());
        }
        self.paths.insert(id, path);
        Ok(())
    }
    /// Apply an exact permutation, rejecting malformed session data atomically.
    pub fn reorder(&mut self, order: Vec<u64>) -> Result<(), String> {
        let identities: std::collections::BTreeSet<_> = order.iter().copied().collect();
        if order.len() != self.len()
            || identities.len() != self.len()
            || identities.iter().any(|id| !self.contains(*id))
        {
            return Err("Invalid document tab order".into());
        }
        self.order = order;
        Ok(())
    }
    pub fn validate_layout(&self, layout: &crate::views::Layout) -> Result<(), String> {
        layout.validate(self.ids())?;
        let focused = layout
            .views()
            .into_iter()
            .find(|(id, _)| *id == layout.active)
            .map(|(_, document)| document);
        if focused != self.active {
            return Err("Active view and document differ".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headless_open_dedup_close_and_reopen_retires_ids() {
        let mut documents = Documents::default();
        assert_eq!(
            documents.open(Some("/project/a".into())).unwrap(),
            Open::Created(1)
        );
        assert_eq!(documents.open(None).unwrap(), Open::Created(2));
        assert_eq!(
            documents.open(Some("/project/a".into())).unwrap(),
            Open::Existing(1)
        );
        assert_eq!(documents.ids(), [1, 2]);
        assert_eq!(documents.adjacent(false), Some(2));
        documents.close(1);
        assert_eq!(documents.active(), Some(2));
        documents.close(2);
        assert_eq!(documents.active(), None);
        assert_eq!(documents.adjacent(false), None);
        assert_eq!(documents.open(None).unwrap(), Open::Created(3));
        assert!(!documents.select(1));
    }
    #[test]
    fn rename_collision_is_atomic_and_restoration_preserves_identity_and_order() {
        let mut documents = Documents::new(10);
        documents.restore(5, Some("/a".into())).unwrap();
        documents.restore(8, Some("/b".into())).unwrap();
        assert!(documents.rename(5, Some("/b".into())).is_err());
        assert_eq!(documents.path(5), Some(Path::new("/a")));
        documents.rename(5, Some("/c".into())).unwrap();
        assert_eq!(documents.find_path(Path::new("/a")), None);
        assert_eq!(documents.find_path(Path::new("/c")), Some(5));
        assert!(documents.reorder(vec![5, 5]).is_err());
        assert_eq!(documents.ids(), [5, 8]);
        documents.reorder(vec![8, 5]).unwrap();
        assert_eq!(documents.open(None).unwrap(), Open::Created(10));
    }
    #[test]
    fn invalid_restore_and_capacity_fail_without_mutation() {
        let mut documents = Documents::default();
        assert!(documents.restore(0, None).is_err());
        assert!(documents.restore(u64::MAX, None).is_err());
        assert!(documents.is_empty());
        for _ in 0..MAX_DOCUMENTS {
            documents.open(None).unwrap();
        }
        let active = documents.active();
        let next = documents.next_id();
        assert!(documents.open(None).is_err());
        assert_eq!(documents.active(), active);
        assert_eq!(documents.next_id(), next);
        documents.close(1);
        assert_eq!(documents.open(None).unwrap(), Open::Created(next));
        let mut exhausted = Documents::new(u64::MAX);
        assert!(exhausted.open(None).is_err());
        assert!(exhausted.is_empty());
    }
    #[test]
    fn view_relationships_are_testable_without_widgets_or_a_filesystem() {
        let mut documents = Documents::default();
        let first = documents.open(None).unwrap().id();
        let mut layout = crate::views::Layout::default();
        layout.show(first);
        layout.split(crate::views::Axis::Horizontal).unwrap();
        documents.validate_layout(&layout).unwrap();
        let second = documents.open(None).unwrap().id();
        assert!(documents.validate_layout(&layout).is_err());
        layout.show(second);
        documents.validate_layout(&layout).unwrap();
        documents.close(first);
        assert!(documents.validate_layout(&layout).is_err());
        let old_view = layout
            .views()
            .into_iter()
            .find(|(_, document)| *document == first)
            .unwrap()
            .0;
        layout.remove(old_view);
        documents.validate_layout(&layout).unwrap();
    }
}
