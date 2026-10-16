//! Command data shared by providers, semantic services, and presentation adapters.
pub mod keybindings;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub id: String,
    pub title: String,
    pub language: Option<String>,
    pub handler: Handler,
    pub input: Option<String>,
    pub shortcut: Option<Shortcut>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shortcut {
    pub key: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub control: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Handler {
    Request {
        hook: String,
    },
    Picker {
        hook: String,
        mode: PickerMode,
    },
    /// Opaque backend-owned data. The host does not interpret protocol methods.
    Semantic {
        payload: serde_json::Value,
    },
}
impl Declaration {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id)
            || self.title.trim().is_empty()
            || self.title.len() > 256
            || self.title.chars().any(char::is_control)
            || self
                .language
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 64 || s.chars().any(char::is_control))
        {
            return Err("invalid command declaration".into());
        }
        if self
            .input
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err("invalid command input prompt".into());
        }
        if self.shortcut.as_ref().is_some_and(|s| {
            let key = &s.key;
            !(key.chars().count() == 1 && !key.chars().any(char::is_control)
                || key == "Space"
                || key
                    .strip_prefix('F')
                    .and_then(|s| s.parse::<u8>().ok())
                    .is_some_and(|n| (1..=24).contains(&n)))
        }) {
            return Err("invalid command shortcut".into());
        }
        match &self.handler {
            Handler::Request { hook } | Handler::Picker { hook, .. } if !valid_id(hook) => {
                return Err("invalid command hook".into());
            }
            Handler::Semantic { payload }
                if serde_json::to_vec(payload)
                    .map_err(|e| e.to_string())?
                    .len()
                    > 16 * 1024 =>
            {
                return Err("command payload exceeds 16 KiB".into());
            }
            _ => {}
        }
        Ok(())
    }
}
fn valid_id(id: &str) -> bool {
    id.len() < 128
        && id.contains('.')
        && id.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
        })
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub input: Option<String>,
    pub path: Option<PathBuf>,
    pub language: Option<String>,
    pub revision: Option<u64>,
    /// Unicode scalar offset in the source document.
    pub position: Option<usize>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    Utf8,
    Utf16,
    Utf32,
}
/// A zero-based line and column, measured in the specified encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub line: u32,
    pub column: u32,
    pub encoding: Encoding,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub path: PathBuf,
    pub position: Position,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextEdit {
    pub start: Position,
    pub end: Position,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentEdit {
    pub path: PathBuf,
    /// Host document revision, never an LSP document version. An unversioned
    /// edit cannot modify a different dirty buffer whose text the provider may not know.
    pub revision: Option<u64>,
    pub edits: Vec<TextEdit>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceEdit {
    pub documents: Vec<DocumentEdit>,
}
impl WorkspaceEdit {
    pub fn validate(&self) -> Result<(), String> {
        if self.documents.len() > 256 {
            return Err("workspace edit exceeds 256 documents".into());
        }
        let mut paths = std::collections::BTreeSet::new();
        let mut count = 0usize;
        for document in &self.documents {
            if !document.path.is_absolute() || !paths.insert(&document.path) {
                return Err("workspace edit contains invalid or duplicate paths".into());
            }
            count += document.edits.len();
            if count > 10_000 {
                return Err("workspace edit exceeds 10000 replacements".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChoiceAction {
    Location { location: Location },
    Edit { edit: WorkspaceEdit },
    Semantic { payload: serde_json::Value },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub title: String,
    pub detail: Option<String>,
    pub action: ChoiceAction,
}
/// Revision-checked navigation into an existing buffer. Offsets are Unicode scalars.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferLocation {
    pub id: u64,
    pub revision: u64,
    pub position: usize,
}
/// Diagnostics from an exact open revision or a checked disk snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagnosticTarget {
    Buffer { id: u64, revision: u64 },
    File { path: PathBuf, text_hash: u64 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticLocation {
    pub target: DiagnosticTarget,
    /// Unicode scalar offset in the published text.
    pub position: usize,
}
/// Stable content fingerprint for stale navigation checks, not a security boundary.
pub fn text_hash(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    RevealDiagnostic {
        location: DiagnosticLocation,
    },
    RevealBuffer {
        location: BufferLocation,
    },
    /// Select a panel belonging to the command provider.
    ShowPanel {
        panel: String,
    },
    Edit {
        edit: WorkspaceEdit,
    },
    Choices {
        title: String,
        items: Vec<Choice>,
    },
    Message {
        text: String,
    },
    Locations {
        locations: Vec<Location>,
    },
    Document {
        title: String,
        text: String,
        language: Option<String>,
    },
}
impl Outcome {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::ShowPanel { panel }
                if panel.is_empty() || panel.len() > 128 || panel.chars().any(char::is_control) =>
            {
                return Err("invalid panel identity".into());
            }
            Self::Edit { edit } => edit.validate()?,
            Self::Choices { title, items } => {
                if title.is_empty() || title.len() > 256 || items.len() > 1000 {
                    return Err("invalid command choices".into());
                }
                for item in items {
                    if item.title.trim().is_empty()
                        || item.title.len() > 1024
                        || item.title.chars().any(char::is_control)
                    {
                        return Err("invalid choice title".into());
                    }
                    match &item.action {
                        ChoiceAction::Location { location } if !location.path.is_absolute() => {
                            return Err("choice location must be absolute".into());
                        }
                        ChoiceAction::Edit { edit } => edit.validate()?,
                        ChoiceAction::Semantic { payload }
                            if serde_json::to_vec(payload)
                                .map_err(|e| e.to_string())?
                                .len()
                                > 16 * 1024 =>
                        {
                            return Err("choice payload exceeds 16 KiB".into());
                        }
                        _ => {}
                    }
                }
            }
            Self::Locations { locations }
                if locations.len() > 10_000 || locations.iter().any(|l| !l.path.is_absolute()) =>
            {
                return Err("invalid command locations".into());
            }
            Self::Document { title, .. }
                if title.trim().is_empty()
                    || title.len() > 256
                    || title.chars().any(char::is_control) =>
            {
                return Err("invalid command document title".into());
            }
            _ => {}
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
            return Err("command output exceeds 4 MiB".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod declaration_tests {
    use super::*;
    #[test]
    fn declarations_reject_invalid_ids_labels_hooks_and_unbounded_backend_payloads() {
        let valid = Declaration {
            input: None,
            shortcut: None,
            id: "test.action".into(),
            title: "Action".into(),
            language: None,
            handler: Handler::Request {
                hook: "test.invoke".into(),
            },
        };
        valid.validate().unwrap();
        for id in ["", "unscoped", "test..action", "test.Action"] {
            let mut d = valid.clone();
            d.id = id.into();
            assert!(d.validate().is_err());
        }
        let mut d = valid.clone();
        d.title = "bad\ntitle".into();
        assert!(d.validate().is_err());
        let mut d = valid.clone();
        d.handler = Handler::Request {
            hook: "invalid".into(),
        };
        assert!(d.validate().is_err());
        let mut d = valid;
        d.handler = Handler::Semantic {
            payload: serde_json::json!("x".repeat(16 * 1024)),
        };
        assert!(d.validate().is_err());
        assert!(
            Outcome::Locations {
                locations: vec![Location {
                    path: "relative.rs".into(),
                    position: Position {
                        line: 0,
                        column: 0,
                        encoding: Encoding::Utf32
                    }
                }]
            }
            .validate()
            .is_err()
        );
    }
}

/// Interactive pickers have cancellable background jobs and host-rendered controls.
pub const PICKER_CANCEL_HOOK: &str = "velk.picker.cancel.v1";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickerMode {
    Files,
    Content,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PickerRequest {
    Begin {
        /// Stable across queries in one picker; a new session refreshes its file index.
        session: u64,
        id: u64,
        context: Context,
        query: velk_search::Query,
    },
    Poll {
        id: u64,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PickerReply {
    Pending,
    Ready { items: Vec<Choice>, notice: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickerCancel {
    pub plugin: String,
    pub id: u64,
}
