//! Host-rendered panel contributions carried by a named byte request. No UI,
//! callbacks, plugin-owned memory, or filesystem assumptions cross this boundary.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub const HOOK: &str = "velk.ui.panels.v1";
pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_PANELS: usize = 16;
pub const MAX_NODES: usize = 10_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    Left,
    Right,
    Bottom,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Panel {
    /// Local to the provider; the host qualifies this with plugin ID.
    pub id: String,
    pub title: String,
    pub slot: Slot,
    pub revision: u64,
    pub content: Content,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Content {
    Tree(Vec<Node>),
    Text(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub label: String,
    /// Preorder depth. Every parent must precede its children and be a branch.
    pub depth: u8,
    pub branch: bool,
    /// Initial expansion only; subsequent snapshots preserve host view state.
    #[serde(default)]
    pub expanded: bool,
    /// Optional opaque action. Selection and expansion are always host-local.
    #[serde(default)]
    pub action: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Describe,
    Invoke {
        panel: String,
        revision: u64,
        node: String,
        action: String,
    },
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub panels: Vec<Panel>,
}
fn short(s: &str, limit: usize) -> bool {
    !s.is_empty() && s.len() <= limit && !s.chars().any(char::is_control)
}
impl Snapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.panels.len() > MAX_PANELS {
            return Err("too many panels".into());
        }
        let mut ids = BTreeSet::new();
        let mut count = 0;
        for p in &self.panels {
            if !short(&p.id, 128) || !short(&p.title, 128) || !ids.insert(&p.id) {
                return Err("invalid or duplicate panel identity".into());
            }
            match &p.content {
                Content::Text(text) if text.len() > 64 * 1024 => {
                    return Err("panel text too large".into());
                }
                Content::Text(_) => {}
                Content::Tree(nodes) => {
                    count += nodes.len();
                    if count > MAX_NODES {
                        return Err("too many panel rows".into());
                    }
                    let mut ids = BTreeSet::new();
                    let mut previous: Option<&Node> = None;
                    for n in nodes {
                        if !short(&n.id, 128)
                            || !short(&n.label, 256)
                            || !ids.insert(&n.id)
                            || n.depth > 32
                            || n.action.as_ref().is_some_and(|s| !short(s, 128))
                            || previous.is_none_or(|p| {
                                n.depth > p.depth && (!p.branch || n.depth != p.depth + 1)
                            }) && n.depth != 0
                        {
                            return Err("invalid panel tree".into());
                        }
                        previous = Some(n);
                    }
                }
            }
        }
        Ok(())
    }
}
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("panel payload exceeds 1 MiB".into());
    }
    Ok(bytes)
}
pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    if bytes.len() > MAX_BYTES {
        return Err("panel payload exceeds 1 MiB".into());
    }
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn node(id: &str, depth: u8, branch: bool) -> Node {
        Node {
            id: id.into(),
            label: id.into(),
            depth,
            branch,
            expanded: false,
            action: None,
        }
    }
    #[test]
    fn validates_tree_structure_unique_ids_and_bounds() {
        let mut s = Snapshot {
            panels: vec![Panel {
                id: "files".into(),
                title: "Files".into(),
                slot: Slot::Left,
                revision: 1,
                content: Content::Tree(vec![node("root", 0, true), node("child", 1, false)]),
            }],
        };
        assert!(s.validate().is_ok());
        for nodes in [
            vec![node("orphan", 1, false)],
            vec![node("root", 0, false), node("child", 1, false)],
            vec![node("root", 0, true), node("skip", 2, false)],
            vec![node("same", 0, true), node("same", 1, false)],
            vec![node("deep", 33, false)],
        ] {
            s.panels[0].content = Content::Tree(nodes);
            assert!(s.validate().is_err());
        }
        s.panels[0].content = Content::Text("x".repeat(65537));
        assert!(s.validate().is_err());
        s.panels[0].content = Content::Tree(vec![node("root", 0, true)]);
        s.panels.push(s.panels[0].clone());
        assert!(s.validate().is_err());
        assert!(decode::<Snapshot>(&vec![b' '; MAX_BYTES + 1]).is_err());
    }
}
