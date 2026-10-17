//! Host service dispatch stays on the UI; filesystem reads stay on plugin workers.
use super::{Document, buffers::BufferState};
use rxui::prelude::*;
use velk_editor_widget::Editor;
use velk_plugin_contracts::host::{self, Request, Response};
use velk_plugin_host::HostRequest;

impl Document {
    pub(super) fn publish_plugin_workspace(&self, client: &velk_plugin_host::Client) {
        if let Ok(current_directory) = std::env::current_dir() {
            client.set_workspace(host::Workspace {
                current_directory,
                project_root: self.workspace.root().map(std::path::Path::to_path_buf),
            });
        }
    }

    pub(super) fn listen_host_requests(
        &mut self,
        receiver: async_channel::Receiver<HostRequest>,
        cx: &mut Context<'_, Self>,
    ) {
        self.host_requests = None;
        let next = receiver.clone();
        match cx.try_spawn(async move { next.recv().await }, move |this, result, cx| {
            this.host_requests = None;
            if let Ok(Ok(request)) = result {
                let result = if request.is_active() {
                    this.host_request(
                        request.request.clone(),
                        Some((request.plugin.clone(), request.generation)),
                        cx,
                    )
                } else {
                    Err("host service retired".into())
                };
                request.respond(result);
                this.listen_host_requests(receiver, cx);
            }
        }) {
            Ok(task) => self.host_requests = Some(task),
            Err(error) => self.notice(Some(format!(
                "Cannot listen for plugin host requests: {error}"
            ))),
        }
    }
    pub(super) fn host_buffer(&self, id: u64) -> Result<&BufferState, String> {
        self.buffer
            .as_ref()
            .filter(|b| b.id == id)
            .or_else(|| self.inactive.get(&id))
            .ok_or_else(|| format!("buffer {id} is not open"))
    }
    pub(super) fn buffer_info(
        &self,
        buffer: &BufferState,
        cx: &impl rxui::ReadContext,
    ) -> host::Buffer {
        let editor = buffer.editor.read(cx);
        let model = editor.buffer();
        let selection = model.selection();
        host::Buffer {
            id: buffer.id,
            title: buffer
                .path
                .file_name()
                .unwrap_or(buffer.path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            path: buffer.file.as_ref().map(|f| f.path().to_path_buf()),
            revision: model.revision(),
            dirty: model.is_dirty(),
            active: !self.background_buffer
                && self.buffer.as_ref().is_some_and(|b| b.id == buffer.id),
            anchor: selection.anchor,
            head: selection.head,
        }
    }
    fn plugin_diagnostics(&self, cx: &impl rxui::ReadContext) -> Response {
        let mut documents = Vec::new();
        let mut remaining = 1000usize;
        let mut truncated = false;
        let clean = |text: &str, limit: usize| -> String {
            text.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(limit)
                .collect()
        };
        let publications = self
            .plugins
            .as_ref()
            .map(|p| p.client.diagnostic_publications())
            .unwrap_or_default();
        for publication in &publications {
            let open = self.buffer.iter().chain(self.inactive.values()).find(|b| {
                b.file
                    .as_ref()
                    .is_some_and(|f| f.path() == publication.path)
            });
            let target = if let Some(buffer) = open {
                let editor = buffer.editor.read(cx);
                let model = editor.buffer();
                if publication
                    .revision
                    .is_some_and(|revision| revision != model.revision())
                    || !model
                        .text()
                        .chunks()
                        .flat_map(str::bytes)
                        .eq(publication.text.bytes())
                {
                    continue;
                }
                velk_commands::DiagnosticTarget::Buffer {
                    id: buffer.id,
                    revision: model.revision(),
                }
            } else {
                if !publication.disk_matches {
                    continue;
                }
                velk_commands::DiagnosticTarget::File {
                    path: publication.path.clone(),
                    text_hash: velk_commands::text_hash(publication.text.bytes()),
                }
            };
            truncated |= publication.truncated;
            let mut diagnostics = publication.diagnostics.clone();
            if diagnostics.len() > remaining {
                diagnostics.truncate(remaining);
                truncated = true;
            }
            remaining -= diagnostics.len();
            if let Some(buffer) = open {
                let editor = buffer.editor.read(cx);
                let model = editor.buffer();
                diagnostics.retain_mut(|diagnostic| {
                    let Some(range) = model.grapheme_range(diagnostic.start..diagnostic.end) else {
                        return false;
                    };
                    diagnostic.start = range.start;
                    diagnostic.end = range.end;
                    diagnostic.line = model.text().char_to_line(range.start);
                    diagnostic.column = range.start - model.text().line_to_char(diagnostic.line);
                    true
                });
            }
            for diagnostic in &mut diagnostics {
                diagnostic.message = clean(&diagnostic.message, 512);
                diagnostic.source = diagnostic.source.as_deref().map(|s| clean(s, 64));
                diagnostic.code = diagnostic.code.as_deref().map(|s| clean(s, 64));
            }
            if !diagnostics.is_empty() {
                documents.push(host::DiagnosticDocument {
                    target,
                    title: clean(&publication.path.to_string_lossy(), 256),
                    diagnostics,
                });
            }
        }
        for buffer in self.buffer.iter().chain(self.inactive.values()) {
            if buffer
                .file
                .as_ref()
                .is_some_and(|file| publications.iter().any(|p| p.path == file.path()))
            {
                continue;
            }
            let Some(state) = &buffer.semantic else {
                continue;
            };
            let editor = buffer.editor.read(cx);
            let model = editor.buffer();
            if state.revision != model.revision()
                || matches!(
                    state.status,
                    velk_semantic::Status::Failed(_) | velk_semantic::Status::Stopped
                )
            {
                continue;
            }
            let mut diagnostics = Vec::new();
            for diagnostic in &state.diagnostics {
                let Some(range) = model.grapheme_range(diagnostic.range.clone()) else {
                    continue;
                };
                if remaining == 0 {
                    truncated = true;
                    break;
                }
                remaining -= 1;
                let line = model.text().char_to_line(range.start);
                diagnostics.push(host::Diagnostic {
                    start: range.start,
                    end: range.end,
                    line,
                    column: range.start - model.text().line_to_char(line),
                    severity: match diagnostic.severity {
                        velk_semantic::Severity::Error => host::Severity::Error,
                        velk_semantic::Severity::Warning => host::Severity::Warning,
                        velk_semantic::Severity::Information => host::Severity::Information,
                        velk_semantic::Severity::Hint => host::Severity::Hint,
                    },
                    message: clean(&diagnostic.message, 512),
                    source: diagnostic.source.as_deref().map(|s| clean(s, 64)),
                    code: diagnostic.code.as_deref().map(|s| clean(s, 64)),
                });
            }
            if !diagnostics.is_empty() {
                documents.push(host::DiagnosticDocument {
                    target: velk_commands::DiagnosticTarget::Buffer {
                        id: buffer.id,
                        revision: model.revision(),
                    },
                    title: clean(&buffer.path.to_string_lossy(), 256),
                    diagnostics,
                });
            }
        }
        Response::Diagnostics {
            documents,
            truncated,
        }
    }
    pub(super) fn reveal_diagnostic(
        &mut self,
        location: velk_commands::DiagnosticLocation,
        cx: &mut Context<'_, Self>,
    ) {
        match location.target {
            velk_commands::DiagnosticTarget::Buffer { id, revision } => self.reveal_plugin_buffer(
                velk_commands::BufferLocation {
                    id,
                    revision,
                    position: location.position,
                },
                cx,
            ),
            velk_commands::DiagnosticTarget::File { path, text_hash } => {
                self.open_diagnostic(path, text_hash, location.position, cx)
            }
        }
    }
    pub(super) fn reveal_plugin_buffer(
        &mut self,
        location: velk_commands::BufferLocation,
        cx: &mut Context<'_, Self>,
    ) {
        if self.busy()
            || self.opening.is_some()
            || self.lifecycle.close_intent().is_some()
            || self
                .buffer
                .as_ref()
                .is_some_and(|b| b.editor.read(cx).has_composition())
        {
            return;
        }
        let valid = self.host_buffer(location.id).is_ok_and(|b| {
            let editor = b.editor.read(cx);
            editor.buffer().revision() == location.revision
                && location.position <= editor.buffer().text().len_chars()
                && !editor.has_composition()
        });
        if !valid {
            self.notice(Some(
                "Navigation target changed or closed; refresh results and try again".into(),
            ));
            return;
        }
        let origin = self.navigation_anchor(cx);
        self.select_buffer(location.id, cx);
        if let Some(origin) = origin {
            self.navigation.record(origin);
        }
        self.buffer.editor.update(cx, |editor, _| {
            editor.execute(velk_editor_widget::EditCommand::SetCaret {
                position: location.position,
                extend: false,
            })
        });
        self.restore_focus_pending = true;
    }
    pub(super) fn host_request(
        &mut self,
        request: Request,
        owner: Option<(String, u64)>,
        cx: &mut Context<'_, Self>,
    ) -> Result<Response, String> {
        request.validate()?;
        match request {
            Request::CallPlugin { .. } => {
                return Err("plugin calls are handled by the native runtime".into());
            }
            Request::Workspace => {
                return Ok(Response::Workspace {
                    workspace: host::Workspace {
                        current_directory: std::env::current_dir().map_err(|e| e.to_string())?,
                        project_root: self.workspace.root().map(std::path::Path::to_path_buf),
                    },
                });
            }
            Request::Diagnostics => return Ok(self.plugin_diagnostics(cx)),
            Request::Buffers => {
                return Ok(Response::Buffers {
                    buffers: self
                        .ids()
                        .into_iter()
                        .map(|id| self.host_buffer(id).map(|b| self.buffer_info(b, cx)))
                        .collect::<Result<_, _>>()?,
                });
            }
            Request::ReadBuffer { id } => {
                let buffer = self.host_buffer(id)?;
                let editor = buffer.editor.read(cx);
                let text = editor.buffer().text();
                if text.len_bytes() > host::MAX_FILE_BYTES {
                    return Err("buffer text exceeds 1 MiB".into());
                }
                return Ok(Response::Buffer {
                    buffer: self.buffer_info(buffer, cx),
                    text: text.to_string(),
                });
            }
            Request::PublishDiagnostics { .. }
            | Request::ClearDiagnostics { .. }
            | Request::ReadFile { .. }
            | Request::ReadDirectory { .. } => {
                return Err("project I/O must run off the UI".into());
            }
            _ => {}
        }
        if self.busy()
            || self.opening.is_some()
            || self.lifecycle.close_intent().is_some()
            || self.workspace_edit.is_some()
            || self
                .buffer
                .iter()
                .chain(self.inactive.values())
                .any(|b| b.editor.read(cx).has_composition())
        {
            return Err("editor is busy; try the buffer operation again".into());
        }
        match request {
            Request::OpenBuffer { path } => {
                self.open_path(path, cx);
            }
            Request::NewBuffer { text } => {
                if self.ids().len() >= 256 {
                    return Err("workspace already has 256 open documents".into());
                }
                // Populate through the same insertion transaction as a normal edit,
                // so nonempty generated documents are dirty and undoable.
                let mut editor = Editor::new("");
                editor
                    .apply_transaction(velk_core::Transaction {
                        revision: editor.buffer().revision(),
                        edits: vec![velk_core::TextEdit { range: 0..0, text }],
                        selection: None,
                    })
                    .map_err(|e| e.to_string())?;
                self.add_buffer(
                    None,
                    editor,
                    format!("Untitled {}", self.next_buffer).into(),
                    cx,
                );
                return Ok(Response::Created { id: self.buffer.id });
            }
            Request::SelectBuffer { id } => {
                self.host_buffer(id)?;
                self.select_buffer(id, cx);
            }
            Request::CloseBuffer { id } => {
                self.host_buffer(id)?;
                self.close_buffer(id, cx);
            }
            Request::SaveBuffer {
                id,
                expected_revision,
            } => {
                let buffer = self.host_buffer(id)?;
                if let Some(expected) = expected_revision
                    && (buffer.file.is_none()
                        || buffer.editor.read(cx).buffer().revision() != expected)
                {
                    return Err(
                        "conditional save targets an untitled buffer or stale revision".into(),
                    );
                }
                if self.host_buffer(id)?.file.is_some() {
                    self.with_buffer(id, cx, |this, cx| this.save(None, cx));
                } else {
                    if self.window.is_none() {
                        return Err("Save As requires a window".into());
                    }
                    self.select_buffer(id, cx);
                    self.save(None, cx);
                }
            }
            Request::EditBuffer {
                id,
                revision,
                edits,
            } => {
                let buffer = self.host_buffer(id)?;
                let transaction = velk_editor_model::prepare_document_edit(
                    &velk_commands::DocumentEdit {
                        path: buffer.path.clone(),
                        revision: Some(revision),
                        edits,
                    },
                    buffer.editor.read(cx).buffer().text(),
                    buffer.editor.read(cx).buffer().revision(),
                )?;
                buffer
                    .editor
                    .update(cx, |editor, _| editor.apply_transaction(transaction))
                    .map_err(|e| e.to_string())?;
            }
            Request::ApplyEdit { edit } => {
                let mut guard = self.edit_guard(cx);
                guard.owner = owner;
                self.apply_workspace_edit(edit, guard, cx);
            }
            _ => unreachable!("read-only requests returned above"),
        }
        Ok(Response::Accepted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::tests::QueuedExecutor;
    use std::sync::Arc;
    use velk_commands::{Encoding, Position, TextEdit};

    fn edit(id: u64, revision: u64, text: &str) -> Request {
        Request::EditBuffer {
            id,
            revision,
            edits: vec![TextEdit {
                start: Position {
                    line: 0,
                    column: 0,
                    encoding: Encoding::Utf32,
                },
                end: Position {
                    line: 0,
                    column: 1,
                    encoding: Encoding::Utf32,
                },
                text: text.into(),
            }],
        }
    }
    #[test]
    fn disk_diagnostic_navigation_opens_unopened_files_and_rejects_changed_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unopened.txt");
        std::fs::write(&path, "aéx\r\nz").unwrap();
        let hash = velk_commands::text_hash("aéx\nz".bytes());
        let mut runtime = rxui::Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = runtime.update(|cx| cx.new(|_| Document::empty(1)));
        runtime
            .update(|cx| document.update(cx, |d, cx| d.open_diagnostic(path.clone(), hash, 2, cx)));
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                assert_eq!(d.ids().len(), 1);
                assert_eq!(d.buffer.editor.read(cx).caret_position(), 2);
                d.buffer.editor.update(cx, |editor, _| {
                    editor.execute(velk_editor_model::EditCommand::Insert("changed"))
                });
                d.host_request(
                    Request::NewBuffer {
                        text: String::new(),
                    },
                    None,
                    cx,
                )
                .unwrap();
                d.open_diagnostic(path.clone(), hash, 2, cx);
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            document.update(cx, |d, _cx| {
                assert_eq!(
                    d.buffer.id, 2,
                    "stale disk results must not select a dirty target"
                );
                assert!(
                    d.window_status
                        .as_ref()
                        .is_some_and(|s| s.contains("changed"))
                        || d.buffer
                            .status
                            .as_ref()
                            .is_some_and(|s| s.contains("changed"))
                );
            })
        });
        let changed = directory.path().join("changed.txt");
        std::fs::write(&changed, "changed on disk").unwrap();
        runtime.update(|cx| {
            document.update(cx, |d, cx| d.open_diagnostic(changed.clone(), hash, 2, cx))
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            assert_eq!(
                document.read(cx).ids().len(),
                2,
                "stale disk results must not open a buffer"
            )
        });
    }
    #[test]
    fn host_buffers_preserve_unsaved_text_identity_history_and_close_confirmation() {
        let mut runtime = rxui::Runtime::new();
        runtime
            .configure_tasks(Arc::new(QueuedExecutor::default()), || {})
            .unwrap();
        let document = runtime.update(|cx| cx.new(|_| Document::empty(1)));
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                let Response::Buffers { buffers } =
                    d.host_request(Request::Buffers, None, cx).unwrap()
                else {
                    panic!()
                };
                assert!(buffers.is_empty());
                let Response::Created { id } = d
                    .host_request(
                        Request::NewBuffer {
                            text: "世界".into(),
                        },
                        None,
                        cx,
                    )
                    .unwrap()
                else {
                    panic!()
                };
                let Response::Buffer { buffer, text } = d
                    .host_request(Request::ReadBuffer { id }, None, cx)
                    .unwrap()
                else {
                    panic!()
                };
                assert_eq!(text, "世界");
                assert!(buffer.dirty);
                assert!(buffer.path.is_none());
                d.host_request(edit(id, buffer.revision, "hello"), None, cx)
                    .unwrap();
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "hello界"
                );
                assert!(
                    d.host_request(edit(id, buffer.revision, "stale"), None, cx)
                        .is_err()
                );
                d.buffer.editor.update(cx, |editor, _| {
                    editor.execute(velk_editor_model::EditCommand::Undo)
                });
                assert_eq!(d.buffer.editor.read(cx).buffer().text().to_string(), "世界");
                let Response::Created { id: second } = d
                    .host_request(
                        Request::NewBuffer {
                            text: String::new(),
                        },
                        None,
                        cx,
                    )
                    .unwrap()
                else {
                    panic!()
                };
                assert_ne!(id, second);
                d.host_request(Request::SelectBuffer { id }, None, cx)
                    .unwrap();
                assert_eq!(d.buffer.id, id);
                d.host_request(Request::CloseBuffer { id }, None, cx)
                    .unwrap();
                assert_eq!(d.lifecycle.prompted_document(), Some(id));
                assert_eq!(d.ids().len(), 2);
                assert!(
                    d.host_request(
                        Request::NewBuffer {
                            text: String::new()
                        },
                        None,
                        cx
                    )
                    .is_err()
                );
                d.lifecycle.cancel_close();
                d.host_request(Request::CloseBuffer { id: second }, None, cx)
                    .unwrap();
                assert_eq!(d.ids(), vec![id]);
                assert!(
                    d.host_request(Request::ReadBuffer { id: second }, None, cx)
                        .is_err()
                );
                assert!(
                    d.host_request(Request::CloseBuffer { id: second }, None, cx)
                        .is_err()
                );
            })
        });
    }
    #[test]
    fn host_open_deduplicates_and_save_uses_the_existing_disk_conflict_flow() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let mut runtime = rxui::Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = runtime.update(|cx| cx.new(|_| Document::empty(1)));
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.workspace.set_root(directory.path().into());
                let Response::Workspace { workspace } =
                    d.host_request(Request::Workspace, None, cx).unwrap()
                else {
                    panic!()
                };
                assert_eq!(workspace.project_root.as_deref(), Some(directory.path()));
                assert!(workspace.current_directory.is_absolute());
                assert!(
                    d.host_request(
                        Request::OpenBuffer {
                            path: "relative".into()
                        },
                        None,
                        cx
                    )
                    .is_err()
                );
                d.host_request(Request::OpenBuffer { path: path.clone() }, None, cx)
                    .unwrap();
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                assert_eq!(d.ids().len(), 1);
                d.host_request(Request::OpenBuffer { path: path.clone() }, None, cx)
                    .unwrap();
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                assert_eq!(d.ids().len(), 1);
                let id = d.buffer.id;
                let revision = d.buffer.editor.read(cx).buffer().revision();
                assert!(
                    d.host_request(
                        Request::SaveBuffer {
                            id,
                            expected_revision: Some(revision + 1)
                        },
                        None,
                        cx
                    )
                    .is_err()
                );
                assert!(d.buffer.saving.is_none());
                d.host_request(edit(id, revision, "O"), None, cx).unwrap();
                d.host_request(
                    Request::SaveBuffer {
                        id,
                        expected_revision: None,
                    },
                    None,
                    cx,
                )
                .unwrap();
            })
        });
        executor.complete(&mut runtime);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Original");
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                let id = d.buffer.id;
                let revision = d.buffer.editor.read(cx).buffer().revision();
                d.host_request(edit(id, revision, "X"), None, cx).unwrap();
                std::fs::write(&path, "external").unwrap();
                d.host_request(
                    Request::SaveBuffer {
                        id,
                        expected_revision: None,
                    },
                    None,
                    cx,
                )
                .unwrap();
            })
        });
        executor.complete(&mut runtime);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external");
        runtime.update(|cx| {
            assert!(document.read(cx).buffer.editor.read(cx).buffer().is_dirty());
            assert!(
                document
                    .read(cx)
                    .buffer
                    .status
                    .as_ref()
                    .unwrap()
                    .contains("changed on disk")
            );
        });
    }
}
