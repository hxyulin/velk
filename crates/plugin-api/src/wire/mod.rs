//! Shared native codecs. Borrowed C views stay within a call; decoded Rust values
//! are copied into the receiver's allocator. Author lifecycle lives in the SDK.
pub mod language_server;
pub mod semantic;
