//! Host-owned workspace and document operations. Buffer IDs last one window session.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const PLUGIN_CALL: &str = "plugin.call";
pub const WORKSPACE_READ: &str = "workspace.read";
pub const PROJECT_READ: &str = "filesystem.project.read";
pub const DIAGNOSTICS_PUBLISH: &str = "editor.diagnostics.publish";
pub const DIAGNOSTICS_READ: &str = "editor.diagnostics.read";
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
    /// Call a declared dependency's byte-request hook from a background worker.
    CallPlugin {
        provider: String,
        hook: String,
        input: Vec<u8>,
    },
    Workspace,
    /// Relative paths resolve against the selected project root; no implicit cwd fallback.
    ReadFile {
        path: PathBuf,
    },
    /// Sorted pages of at most MAX_DIRECTORY_ENTRIES. An empty page ends enumeration.
    ReadDirectory {
        path: PathBuf,
        /// Project-relative path from the last entry of the preceding page.
        after: Option<PathBuf>,
    },
    Buffers,
    /// Replace one client's publication for a file. Empty diagnostics clear it.
    PublishDiagnostics {
        client: u64,
        publication: DiagnosticPublication,
    },
    ClearDiagnostics {
        client: u64,
    },
    /// Current workspace diagnostics, omitting stale open revisions and disk content.
    Diagnostics,
    InspectHistory {
        id: u64,
    },
    CheckoutHistory {
        id: u64,
        revision: u64,
        state: u64,
    },
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
            Self::CallPlugin { .. } => PLUGIN_CALL,
            Self::Workspace => WORKSPACE_READ,
            Self::Diagnostics => DIAGNOSTICS_READ,
            Self::PublishDiagnostics { .. } | Self::ClearDiagnostics { .. } => DIAGNOSTICS_PUBLISH,
            Self::ReadFile { .. } | Self::ReadDirectory { .. } => PROJECT_READ,
            Self::Buffers | Self::ReadBuffer { .. } | Self::InspectHistory { .. } => BUFFER_READ,
            Self::ApplyEdit { .. } | Self::EditBuffer { .. } | Self::CheckoutHistory { .. } => {
                BUFFER_WRITE
            }
            _ => BUFFER_MANAGE,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::CallPlugin {
                provider,
                hook,
                input,
            } => {
                if crate::parse_plugin_id(provider).is_none()
                    || crate::parse_plugin_id(hook).is_none()
                    || input.len() > 256 * 1024
                {
                    return Err("invalid or oversized plugin call".into());
                }
            }
            Self::PublishDiagnostics { publication, .. } => {
                let length = publication.text.chars().count();
                if publication.collection.is_empty()
                    || publication.collection.len() > 128
                    || publication.collection.chars().any(char::is_control)
                    || !publication.path.is_absolute()
                    || publication.text.len() > MAX_FILE_BYTES
                    || publication.diagnostics.len() > 1000
                    || publication
                        .diagnostics
                        .iter()
                        .any(|d| d.start > d.end || d.end > length || d.message.len() > 4096)
                {
                    return Err("invalid or oversized diagnostic publication".into());
                }
            }
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
    Plugin {
        output: Vec<u8>,
    },
    Diagnostics {
        documents: Vec<DiagnosticDocument>,
        truncated: bool,
    },
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
    History {
        buffer: Buffer,
        history: velk_core_contracts::history::Inspection,
    },
    Created {
        id: u64,
    },
    /// Dispatched through the normal host flow. Opens, saves, edits, and dirty
    /// closes may finish later or be cancelled by the user; this is not completion.
    Accepted,
}

/// One provider/client publication, independent of a buffer's lifetime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticPublication {
    /// Independent result set within a client, such as push or pull diagnostics.
    pub collection: String,
    pub path: PathBuf,
    /// Host revision for versioned open-document results; None means disk text.
    pub revision: Option<u64>,
    pub text: String,
    /// Whether the publication also describes the saved file.
    pub disk_matches: bool,
    pub truncated: bool,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticDocument {
    pub target: crate::commands::DiagnosticTarget,
    pub title: String,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub start: usize,
    pub end: usize,
    /// Zero-based logical line and Unicode-scalar column for display.
    pub line: usize,
    pub column: usize,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}
