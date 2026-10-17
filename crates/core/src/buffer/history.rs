use super::*;
pub use crate::history::{Commit, Limits as HistoryLimits, Patch, RedoBranch, Request};
pub(super) struct Owned {
    pub provider: Arc<dyn crate::history::Provider>,
    pub session: Box<dyn crate::history::Session>,
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
