//! UI-independent document state for the default Velk engine.
//! This linked Rust implementation is not a stable plugin or binary interface.
mod buffer;
mod change;
pub mod graphemes;

pub use buffer::{
    Buffer, BufferSnapshot, Checkpoint, Selection, TextEdit, Transaction, TransactionError,
};
pub use change::{Change, TextPoint};
