//! Host-owned workspace and document operations. Buffer IDs last one window session.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const WORKSPACE_READ: &str = "workspace.read";
pub const PROJECT_READ: &str = "filesystem.project.read";
pub const BUFFER_READ: &str = "editor.buffer.read";
pub const BUFFER_WRITE: &str = "editor.buffer.write";
pub const BUFFER_MANAGE: &str = "editor.buffer.manage";
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_DIRECTORY_ENTRIES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub current_directory: PathBuf,
    /// The folder selected in the editor, which can differ from the process cwd.
    pub project_root: Option<PathBuf>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Buffer {
    pub id: u64,
    pub title: String,
    pub path: Option<PathBuf>,
    pub revision: u64,
    pub dirty: bool,
    pub active: bool,
    /// Unicode scalar offsets, matching the document model.
    pub anchor: usize,
    pub head: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryEntry {
    /// Relative to the project root. Symlinks are reported, not followed by listings.
    pub path: PathBuf,
    pub kind: FileKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Workspace,
    /// Relative paths resolve against the selected project root; no implicit cwd fallback.
    ReadFile {
        path: PathBuf,
    },
    ReadDirectory {
        path: PathBuf,
    },
    Buffers,
    ReadBuffer {
        id: u64,
    },
    /// Absolute paths only. Uses the normal asynchronous open/deduplication flow.
    OpenBuffer {
        path: PathBuf,
    },
    NewBuffer {
        text: String,
    },
    SelectBuffer {
        id: u64,
    },
    CloseBuffer {
        id: u64,
    },
    SaveBuffer {
        id: u64,
        /// Conditional background saves require a file and this exact revision.
        expected_revision: Option<u64>,
    },
    /// Versioned edits also support untitled buffers. Uses the shared position encoding.
    EditBuffer {
        id: u64,
        revision: u64,
        edits: Vec<crate::commands::TextEdit>,
    },
    ApplyEdit {
        edit: crate::commands::WorkspaceEdit,
    },
}
impl Request {
    pub fn capability(&self) -> &'static str {
        match self {
            Self::Workspace => WORKSPACE_READ,
            Self::ReadFile { .. } | Self::ReadDirectory { .. } => PROJECT_READ,
            Self::Buffers | Self::ReadBuffer { .. } => BUFFER_READ,
            Self::ApplyEdit { .. } | Self::EditBuffer { .. } => BUFFER_WRITE,
            _ => BUFFER_MANAGE,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::OpenBuffer { path } if !path.is_absolute() => {
                return Err("opening a buffer requires an absolute path".into());
            }
            Self::NewBuffer { text } if text.len() > MAX_FILE_BYTES => {
                return Err("new buffer text exceeds 1 MiB".into());
            }
            Self::EditBuffer { edits, .. } if edits.len() > 10_000 => {
                return Err("buffer edit exceeds 10000 replacements".into());
            }
            Self::ApplyEdit { edit } => edit.validate()?,
            _ => {}
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Workspace {
        workspace: Workspace,
    },
    File {
        bytes: Vec<u8>,
    },
    Directory {
        entries: Vec<DirectoryEntry>,
    },
    Buffers {
        buffers: Vec<Buffer>,
    },
    Buffer {
        buffer: Buffer,
        text: String,
    },
    Created {
        id: u64,
    },
    /// Dispatched through the normal host flow. Opens, saves, edits, and dirty
    /// closes may finish later or be cancelled by the user; this is not completion.
    Accepted,
}
