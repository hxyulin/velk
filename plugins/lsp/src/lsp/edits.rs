//! Translate protocol edits to revision-checked host proposals.
use super::{positions::Encoding, *};
use velk_plugin_sdk::commands::{
    DocumentEdit, Encoding as Units, Position, TextEdit, WorkspaceEdit,
};

pub(super) fn position(position: lsp_types::Position, encoding: Encoding) -> Position {
    Position {
        line: position.line,
        column: position.character,
        encoding: match encoding {
            Encoding::Utf8 => Units::Utf8,
            Encoding::Utf16 => Units::Utf16,
            Encoding::Utf32 => Units::Utf32,
        },
    }
}
pub(super) fn text(edit: lsp_types::TextEdit, encoding: Encoding) -> TextEdit {
    TextEdit {
        start: position(edit.range.start, encoding),
        end: position(edit.range.end, encoding),
        text: edit.new_text,
    }
}
pub(super) fn document(
    path: PathBuf,
    revision: Option<u64>,
    edits: Vec<lsp_types::TextEdit>,
    encoding: Encoding,
) -> DocumentEdit {
    DocumentEdit {
        path,
        revision,
        edits: edits.into_iter().map(|e| text(e, encoding)).collect(),
    }
}
pub(super) fn workspace(
    value: Value,
    source: &DocumentSnapshot,
    version: i32,
    encoding: Encoding,
) -> Result<WorkspaceEdit, String> {
    let edit: lsp_types::WorkspaceEdit =
        serde_json::from_value(value).map_err(|e| e.to_string())?;
    if edit.changes.is_some() && edit.document_changes.is_some() {
        return Err("Ambiguous workspace edit representations".into());
    }
    let mut documents = Vec::new();
    if let Some(changes) = edit.changes {
        for (uri, edits) in changes {
            let path = Url::parse(uri.as_str())
                .map_err(|e| e.to_string())?
                .to_file_path()
                .map_err(|_| "Edit target is not a local file")?;
            let revision = (path == source.path).then_some(source.revision);
            documents.push(document(path, revision, edits, encoding));
        }
    }
    if let Some(changes) = edit.document_changes {
        let changes = match changes {
            lsp_types::DocumentChanges::Edits(edits) => edits,
            lsp_types::DocumentChanges::Operations(_) => {
                return Err("File creation, renaming and deletion are not supported".into());
            }
        };
        for change in changes {
            let path = Url::parse(change.text_document.uri.as_str())
                .map_err(|e| e.to_string())?
                .to_file_path()
                .map_err(|_| "Edit target is not a local file")?;
            if let Some(expected) = change.text_document.version
                && (path != source.path || expected != version)
            {
                return Err(
                    "Workspace edit targets an unknown or stale LSP document version".into(),
                );
            }
            let revision = (path == source.path).then_some(source.revision);
            let edits = change
                .edits
                .into_iter()
                .map(|edit| match edit {
                    lsp_types::OneOf::Left(edit) => Ok(edit),
                    lsp_types::OneOf::Right(_) => {
                        Err("Annotated edits require preview support".to_string())
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            documents.push(document(path, revision, edits, encoding));
        }
    }
    let edit = WorkspaceEdit { documents };
    edit.validate()?;
    Ok(edit)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_stale_versions_resource_operations_and_non_file_targets() {
        let source = DocumentSnapshot {
            path: "/tmp/source.rs".into(),
            language: "rust".into(),
            revision: 42,
            text: ropey::Rope::from_str("old"),
        };
        let value = json!({"documentChanges":[{"textDocument":{"uri":"file:///tmp/source.rs","version":8},"edits":[]}]});
        assert!(workspace(value.clone(), &source, 7, Encoding::Utf16).is_err());
        assert!(workspace(value, &source, 8, Encoding::Utf16).is_ok());
        assert!(
            workspace(
                json!({"documentChanges":[{"kind":"create","uri":"file:///tmp/new.rs"}]}),
                &source,
                8,
                Encoding::Utf16
            )
            .is_err()
        );
        assert!(
            workspace(
                json!({"changes":{"untitled:virtual.rs":[]}}),
                &source,
                8,
                Encoding::Utf16
            )
            .is_err()
        );
    }
}
