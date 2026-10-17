//! UI-independent document state for the default Velk engine.
//! This linked Rust implementation is not a stable plugin or binary interface.
mod buffer;

pub use velk_core_contracts::graphemes;
pub mod history;
pub mod lifecycle;

pub use buffer::{
    Buffer, BufferSnapshot, Checkpoint, HistoryLimits, RedoBranch, Selection, TextDocument,
    TextEdit, Transaction, TransactionError, UndoGroup,
};
pub use velk_core_contracts::{Affinity, Change, TextPoint};
