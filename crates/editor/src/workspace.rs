//! Host-owned workspace context shared by recovery and provider contexts.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Workspace {
    #[serde(default, with = "crate::session::optional_path")]
    root: Option<PathBuf>,
}
impl Workspace {
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }
    pub fn set_root(&mut self, root: PathBuf) {
        self.root = Some(root);
    }
}
