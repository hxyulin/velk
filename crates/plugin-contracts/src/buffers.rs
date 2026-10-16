//! Post-commit buffer notifications. Consumers must accept authoritative snapshots.
use crate::host::Buffer;
use serde::{Deserialize, Serialize};
pub const HOOK: &str = "velk.editor.buffers.v1";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Snapshot { buffers: Vec<Buffer> },
    Opened { buffer: Buffer },
    Changed { buffer: Buffer },
    Saved { buffer: Buffer, saved_revision: u64 },
    Closed { id: u64 },
}
impl Event {
    pub fn id(&self) -> Option<u64> {
        match self {
            Self::Snapshot { .. } => None,
            Self::Opened { buffer } | Self::Changed { buffer } | Self::Saved { buffer, .. } => {
                Some(buffer.id)
            }
            Self::Closed { id } => Some(*id),
        }
    }
}
