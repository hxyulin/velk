//! Host-authenticated caller identity for reusable byte-request backends.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Consumer {
    pub plugin: String,
    pub generation: u64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Call {
    pub consumer: Consumer,
    pub input: Vec<u8>,
}

pub const RELEASE_HOOK: &str = "velk.backend.release";
