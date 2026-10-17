//! Replaceable document undo storage. Providers propose changes; Core validates
//! and applies them to the authoritative text and assigns document revisions.
use crate::{Change, Selection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const HOOK: &str = "velk.undo.session";
pub const CONTRACT: &str = "velk.contract.undo-session";
pub const DEFAULT_PROVIDER: &str = "velk.undo.tree";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Limits {
    pub entries: usize,
    pub bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: 128,
            bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedoBranch {
    pub state: u64,
    pub preferred: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Patch {
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
    pub fn inverse(&self) -> Self {
        Self {
            change: self.change.inverse(self.change.revision),
            removed: self.inserted.clone(),
            inserted: self.removed.clone(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Commit {
    pub state: u64,
    pub parent: u64,
    pub before: Selection,
    pub after: Selection,
    pub patches: Vec<Patch>,
    /// True permits coalescing the previous step; false requires an undo boundary.
    /// Providers may decline coalescing, but must preserve boundaries and state IDs.
    pub merge: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Step {
    pub source: u64,
    pub target: u64,
    pub selection: Selection,
    pub patches: Vec<Patch>,
}
#[derive(Clone, Serialize, Deserialize)]
pub enum Request {
    Record(Commit),
    Undo(u64),
    Redo { current: u64, branch: Option<u64> },
    Checkout { current: u64, target: u64 },
    Limits { current: u64, limits: Limits },
    Status,
    Inspect,
    Travel { current: u64, target: u64 },
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub can_undo: bool,
    pub branches: Vec<RedoBranch>,
    pub limits: Limits,
    pub bytes: usize,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Response {
    pub status: Status,
    pub step: Option<Step>,
    pub tree: Option<Tree>,
}
/// Retained state metadata, independent of a provider's archive representation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub state: u64,
    pub parent: Option<u64>,
    pub inserted: usize,
    pub removed: usize,
    /// Bounded single-line excerpt of the change, for history browsers.
    pub summary: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    pub nodes: Vec<Node>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inspection {
    pub current: u64,
    pub saved: u64,
    pub tree: Option<Tree>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Restore {
    pub text: String,
    pub state: u64,
    pub revision: u64,
    pub next_state: u64,
    pub data: serde_json::Value,
}
/// Immutable snapshots must remain encodable after session or plugin retirement.
/// Encoding and restore validation are blocking worker operations.
pub trait Snapshot: Send + Sync {
    fn encode(&self) -> Result<serde_json::Value, String>;
}
/// Sessions are document-owned and used serially. Record/checkout must be atomic
/// on error. Travel only proposes a step; checkout follows Core validation.
/// Existing sessions remain usable after their factory is logically unloaded.
/// Requests and snapshot capture run on the editing thread and must not perform
/// blocking I/O. Status reads should be cheap; encode/restore run on workers.
pub trait Session: Send {
    fn request(&mut self, request: Request) -> Result<Response, String>;
    fn status(&self) -> Status;
    fn snapshot(&self) -> Result<Arc<dyn Snapshot>, String>;
}
pub trait Provider: std::fmt::Debug + Send + Sync + 'static {
    fn id(&self) -> &str;
    fn open(&self, root: u64) -> Box<dyn Session>;
    fn restore(&self, archive: Restore) -> Result<Box<dyn Session>, String>;
}
/// Validate a proposed replacement against its exact source, including Unicode
/// coordinates. The caller supplies a disposable Rope before publishing changes.
pub fn apply_patch(text: &mut ropey::Rope, patch: &Patch) -> Result<(), String> {
    let change = &patch.change;
    if change.replaced.start > change.replaced.end
        || change.replaced.len() != patch.removed.chars().count()
        || change.inserted != patch.inserted.chars().count()
        || change.replaced.end > text.len_chars()
        || patch.inserted.contains('\r')
        || patch.removed.contains('\r')
        || text.slice(change.replaced.clone()) != patch.removed.as_ref()
    {
        return Err("invalid undo patch".into());
    }
    let before = text.clone();
    text.remove(change.replaced.clone());
    text.insert(change.replaced.start, &patch.inserted);
    if Change::between(
        change.revision,
        &before,
        text,
        change.replaced.clone(),
        change.inserted,
    ) != *change
    {
        return Err("invalid undo coordinates".into());
    }
    Ok(())
}
