//! Completion providers contribute normal commands to this topic. They have no
//! runtime dependency on a particular reducer or popup implementation.
pub const TOPIC: &str = "velk.completion.source";
pub const REDUCE: &str = "velk.completion.reduce";
pub const COMMAND: &str = "velk.completion.complete";
pub use velk_commands::contributions::{Collection, Contribution};
