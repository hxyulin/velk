//! Lazy file-tree data, independent of panel layout and rendering.
use serde::{Deserialize, Serialize};
pub use velk_commands::NativePath;
pub const PROVIDER: &str = "velk.files.backend";
pub const HOOK: &str = "velk.files.tree";
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Context {
    pub document: Option<NativePath>,
    pub root: Option<NativePath>,
    pub initialized: bool,
    pub expanded: Vec<String>,
}
impl Context {
    pub fn validate(&self) -> Result<(), String> {
        for path in self.document.iter().chain(self.root.iter()) {
            path.to_path()?;
        }
        if self.expanded.len() > 10_000
            || self
                .expanded
                .iter()
                .any(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
        {
            return Err("invalid tree expansion context".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Changes,
    Describe {
        context: Context,
    },
    Expand {
        revision: u64,
        node: String,
        expanded: Vec<String>,
    },
    Resolve {
        revision: u64,
        node: String,
    },
    Poll {
        id: u64,
    },
    Cancel {
        id: u64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub path: Option<NativePath>,
    pub label: String,
    pub depth: u8,
    pub directory: bool,
    pub expanded: bool,
    pub lazy: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tree {
    pub revision: u64,
    pub entries: Vec<Entry>,
    pub notice: Option<String>,
    pub open: Option<NativePath>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Reply {
    Unchanged,
    Invalidated,
    Ready { tree: Tree },
    Pending { id: u64 },
    Cancelled,
}
impl Tree {
    pub fn validate(&self) -> Result<(), String> {
        if self.entries.len() > 10_000 || self.notice.as_ref().is_some_and(|n| n.len() > 64 * 1024)
        {
            return Err("tree exceeds limits".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for entry in &self.entries {
            if entry.id.is_empty()
                || entry.id.len() > 128
                || !ids.insert(&entry.id)
                || entry.label.len() > 256
                || entry.depth > 32
                || entry.lazy && !entry.directory
            {
                return Err("invalid tree entry".into());
            }
            if let Some(path) = &entry.path {
                path.to_path()?;
            }
        }
        if let Some(path) = &self.open {
            path.to_path()?;
        }
        Ok(())
    }
}

pub const INTERFACE: &str = "velk.files.tree";
