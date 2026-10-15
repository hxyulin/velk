//! Host-rendered panel contributions carried by a named byte request. No UI,
//! callbacks, plugin-owned memory or filesystem I/O cross this boundary.
pub mod grid;
mod path;
pub use path::NativePath;
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
    Grid(grid::Grid),
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
    /// Opening this branch requests children from the provider off the UI thread.
    #[serde(default)]
    pub lazy: bool,
    /// Optional opaque action. Selection and loaded-branch expansion are host-local.
    #[serde(default)]
    pub action: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Context {
    /// The initial window document, independent of the selected buffer.
    pub document: Option<NativePath>,
    /// Explicit workspace folder, overriding document-root inference.
    #[serde(default)]
    pub root: Option<NativePath>,
    /// Whether this provider has existing panel view state, even if all closed.
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub expanded: Vec<String>,
}
impl Context {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(path) = &self.root {
            path.to_path()?;
        }
        if let Some(path) = &self.document {
            path.to_path()?;
        }
        if self.expanded.len() > MAX_NODES || self.expanded.iter().any(|s| !short(s, 128)) {
            return Err("invalid panel expansion context".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Cheap invalidation check; never performs I/O or starts a model job.
    Changes,
    /// Collect a coalesced live snapshot, or Unchanged. Never performs I/O.
    Updates {
        revision: u64,
    },
    /// Ordered input for an interactive panel. Replies with Ack.
    Input {
        panel: String,
        events: Vec<grid::Input>,
    },
    Describe {
        context: Context,
    },
    Expand {
        panel: String,
        revision: u64,
        node: String,
        expanded: Vec<String>,
    },
    Invoke {
        panel: String,
        revision: u64,
        node: String,
        action: String,
    },
    Poll {
        id: u64,
    },
    Cancel {
        id: u64,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Reply {
    Ack,
    Unchanged,
    Invalidated,
    Ready { snapshot: Snapshot },
    Pending { id: u64 },
    Cancelled,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    OpenFile { path: NativePath },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    /// Fast live snapshots via Updates (8 ms), independent of slow invalidations.
    #[serde(default)]
    pub stream: bool,
    /// Provider supports cheap Changes checks for external invalidation.
    #[serde(default)]
    pub watch: bool,
    pub panels: Vec<Panel>,
    /// One-shot host requests. Never retained or replayed during discovery/reload.
    #[serde(default)]
    pub effects: Vec<Effect>,
}

fn short(s: &str, limit: usize) -> bool {
    !s.is_empty() && s.len() <= limit && !s.chars().any(char::is_control)
}
impl Snapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.panels.len() > MAX_PANELS {
            return Err("too many panels".into());
        }
        if self.effects.len() > 16 {
            return Err("too many panel effects".into());
        }
        for effect in &self.effects {
            match effect {
                Effect::OpenFile { path } => {
                    path.to_path()?;
                }
            }
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
                Content::Grid(grid) => grid.validate()?,
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
                            || (n.lazy && !n.branch)
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
            lazy: false,
            action: None,
        }
    }
    #[test]
    fn validates_tree_structure_unique_ids_and_bounds() {
        let mut s = Snapshot {
            watch: false,
            stream: false,
            effects: Vec::new(),
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
            vec![Node {
                lazy: true,
                ..node("leaf", 0, false)
            }],
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
    #[test]
    fn effects_are_bounded_and_validate_native_paths() {
        let path = NativePath::new(&std::env::current_dir().unwrap());
        let effect = Effect::OpenFile { path };
        let mut snapshot = Snapshot {
            watch: false,
            stream: false,
            panels: vec![],
            effects: vec![effect.clone()],
        };
        snapshot.validate().unwrap();
        let restored: Snapshot = decode(&encode(&snapshot).unwrap()).unwrap();
        assert_eq!(restored.effects, snapshot.effects);
        snapshot.effects = vec![effect; 17];
        assert!(snapshot.validate().is_err());
        snapshot.effects = vec![Effect::OpenFile {
            path: NativePath::Unix(b"relative".to_vec()),
        }];
        assert!(snapshot.validate().is_err());
    }
}
