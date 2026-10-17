//! Document identity and editor-view placement are independent of rendering.
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Node {
    View {
        id: u64,
        document: u64,
    },
    Split {
        id: u64,
        axis: Axis,
        fraction: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub root: Option<Node>,
    pub active: u64,
    next: u64,
}
impl Layout {
    pub fn views(&self) -> Vec<(u64, u64)> {
        fn visit(node: &Node, result: &mut Vec<(u64, u64)>) {
            match node {
                Node::View { id, document } => result.push((*id, *document)),
                Node::Split { first, second, .. } => {
                    visit(first, result);
                    visit(second, result);
                }
            }
        }
        let mut result = Vec::new();
        if let Some(root) = &self.root {
            visit(root, &mut result);
        }
        result
    }
    fn id(&mut self) -> u64 {
        self.next = self.next.max(1);
        let id = self.next;
        self.next += 1;
        id
    }
    pub fn show(&mut self, document: u64) -> u64 {
        if self.root.is_none() {
            let id = self.id();
            self.root = Some(Node::View { id, document });
            self.active = id;
        }
        fn set(node: &mut Node, active: u64, document: u64) {
            match node {
                Node::View {
                    id,
                    document: target,
                } if *id == active => *target = document,
                Node::Split { first, second, .. } => {
                    set(first, active, document);
                    set(second, active, document);
                }
                _ => {}
            }
        }
        set(self.root.as_mut().unwrap(), self.active, document);
        self.active
    }
    pub fn split(&mut self, axis: Axis) -> Option<u64> {
        let document = self
            .views()
            .into_iter()
            .find(|(id, _)| *id == self.active)?
            .1;
        if self.views().len() >= 16 {
            return None;
        }
        let split = self.id();
        let view = self.id();
        let active = self.active;
        fn replace(node: &mut Node, active: u64, split: u64, view: u64, document: u64, axis: Axis) {
            match node {
                Node::View { id, .. } if *id == active => {
                    *node = Node::Split {
                        id: split,
                        axis,
                        fraction: 0.5,
                        first: Box::new(node.clone()),
                        second: Box::new(Node::View { id: view, document }),
                    };
                }
                Node::Split { first, second, .. } => {
                    replace(first, active, split, view, document, axis);
                    replace(second, active, split, view, document, axis);
                }
                _ => {}
            }
        }
        replace(
            self.root.as_mut().unwrap(),
            active,
            split,
            view,
            document,
            axis,
        );
        self.active = view;
        Some(view)
    }
    pub fn remove(&mut self, id: u64) {
        fn remove(node: Node, id: u64) -> Option<Node> {
            match node {
                Node::View { id: other, .. } if other == id => None,
                Node::Split {
                    id: split,
                    axis,
                    fraction,
                    first,
                    second,
                } => match (remove(*first, id), remove(*second, id)) {
                    (Some(first), Some(second)) => Some(Node::Split {
                        id: split,
                        axis,
                        fraction,
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                    (node, None) | (None, node) => node,
                },
                node => Some(node),
            }
        }
        self.root = self.root.take().and_then(|root| remove(root, id));
        let views = self.views();
        if !views.iter().any(|(id, _)| *id == self.active) {
            self.active = views.first().map_or(0, |(id, _)| *id);
        }
    }
    pub fn resize(&mut self, id: u64, fraction: f32) {
        if !fraction.is_finite() {
            return;
        }
        fn resize(node: &mut Node, id: u64, fraction: f32) {
            if let Node::Split {
                id: split,
                fraction: position,
                first,
                second,
                ..
            } = node
            {
                if *split == id {
                    *position = fraction.clamp(0.1, 0.9);
                } else {
                    resize(first, id, fraction);
                    resize(second, id, fraction);
                }
            }
        }
        if let Some(root) = &mut self.root {
            resize(root, id, fraction);
        }
    }
    pub fn validate(&self, documents: &[u64]) -> Result<(), String> {
        fn visit(
            node: &Node,
            documents: &[u64],
            ids: &mut std::collections::BTreeSet<u64>,
        ) -> Result<(), String> {
            let id = match node {
                Node::View { id, document } => {
                    if !documents.contains(document) {
                        return Err("view targets an unknown document".into());
                    }
                    *id
                }
                Node::Split {
                    id,
                    fraction,
                    first,
                    second,
                    ..
                } => {
                    if !fraction.is_finite() || !(0.1..=0.9).contains(fraction) {
                        return Err("invalid split position".into());
                    }
                    visit(first, documents, ids)?;
                    visit(second, documents, ids)?;
                    *id
                }
            };
            if id == 0 || !ids.insert(id) {
                return Err("invalid or duplicate view identity".into());
            }
            Ok(())
        }
        let mut ids = std::collections::BTreeSet::new();
        if let Some(root) = &self.root {
            visit(root, documents, &mut ids)?;
        }
        let views = self.views();
        if views.len() > 16
            || self.next == u64::MAX
            || ids.last().is_some_and(|id| *id >= self.next)
            || (!views.is_empty() && !views.iter().any(|(id, _)| *id == self.active))
            || views.is_empty() && self.active != 0
        {
            return Err("invalid editor view layout".into());
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restored_views_reject_unknown_documents_missing_state_and_invalid_scroll() {
        let mut snapshot = Snapshot::default();
        let view = snapshot.layout.show(7);
        assert!(snapshot.validate(&[7]).is_err());
        snapshot.states.insert(
            view,
            ViewState {
                selection: velk_core::Selection::default(),
                scroll: [0., 0.],
            },
        );
        snapshot.validate(&[7]).unwrap();
        assert!(snapshot.validate(&[8]).is_err());
        snapshot.states.get_mut(&view).unwrap().scroll = [f32::NAN, 0.];
        assert!(snapshot.validate(&[7]).is_err());
        snapshot.states.get_mut(&view).unwrap().scroll = [0., -1.];
        assert!(snapshot.validate(&[7]).is_err());
    }
    #[test]
    fn nested_splits_preserve_document_identity_and_collapse_without_closing_it() {
        let mut layout = Layout::default();
        let first = layout.show(7);
        let second = layout.split(Axis::Horizontal).unwrap();
        let third = layout.split(Axis::Vertical).unwrap();
        assert_eq!(layout.views(), [(first, 7), (second, 7), (third, 7)]);
        layout.show(9);
        layout.validate(&[7, 9]).unwrap();
        layout.remove(second);
        assert_eq!(layout.views(), [(first, 7), (third, 9)]);
        layout.remove(third);
        assert_eq!(layout.active, first);
        layout.validate(&[7]).unwrap();
        layout.remove(first);
        layout.validate(&[]).unwrap();
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewState {
    pub selection: velk_core::Selection,
    pub scroll: [f32; 2],
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub layout: Layout,
    pub states: std::collections::BTreeMap<u64, ViewState>,
}
impl Snapshot {
    pub fn validate(&self, documents: &[u64]) -> Result<(), String> {
        self.layout.validate(documents)?;
        let views = self.layout.views();
        if self.states.len() != views.len()
            || views.iter().any(|(id, _)| !self.states.contains_key(id))
            || self.states.values().any(|state| {
                state
                    .scroll
                    .iter()
                    .any(|value| !value.is_finite() || *value < 0.)
            })
        {
            return Err("invalid editor view state".into());
        }
        Ok(())
    }
}
