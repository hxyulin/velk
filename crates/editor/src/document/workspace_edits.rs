//! Prepare all text changes away from the UI, then commit without yielding.
use super::{Document, buffers::BufferState};
use crate::file::FileState;
use rxui::prelude::*;
use std::{collections::BTreeMap, path::PathBuf};
use velk_commands::{Context as CommandContext, WorkspaceEdit};
use velk_core::{Buffer, Transaction};
use velk_editor_widget::{Editor, Language};

#[derive(Clone)]
pub(super) struct Guard {
    pub source: Option<u64>,
    view: u64,
    context: CommandContext,
    pub owner: Option<(String, u64)>,
    buffers: BTreeMap<PathBuf, (u64, u64)>,
}
#[derive(Clone)]
struct Snapshot {
    id: u64,
    revision: u64,
    text: ropey::Rope,
    dirty: bool,
}
enum Prepared {
    Open {
        path: PathBuf,
        id: u64,
        transaction: Transaction,
    },
    Closed {
        file: FileState,
        buffer: Buffer,
    },
}
impl Document {
    pub(super) fn edit_guard(&self, cx: &impl rxui::ReadContext) -> Guard {
        Guard {
            view: self.views.layout.active,
            source: self.buffer.as_ref().map(|b| b.id),
            context: self.command_context(cx),
            owner: None,
            buffers: self
                .buffer
                .iter()
                .chain(self.inactive.values())
                .filter_map(|b| {
                    b.file.as_ref().map(|f| {
                        (
                            f.path().to_path_buf(),
                            (b.id, b.editor.read(cx).buffer().revision()),
                        )
                    })
                })
                .collect(),
        }
    }
    pub(super) fn guard_valid(&self, guard: &Guard, cx: &impl rxui::ReadContext) -> bool {
        self.views.layout.active == guard.view
            && self.buffer.as_ref().map(|b| b.id) == guard.source
            && self.command_source_matches(&guard.context, cx)
            && guard
                .owner
                .as_ref()
                .is_none_or(|(plugin, generation)| self.command_owner_loaded(plugin, *generation))
    }
    pub(super) fn apply_workspace_edit(
        &mut self,
        edit: WorkspaceEdit,
        guard: Guard,
        cx: &mut Context<'_, Self>,
    ) {
        if self.workspace_edit.is_some() || !self.guard_valid(&guard, cx) {
            return;
        }
        if let Err(error) = edit.validate() {
            self.notice(Some(error));
            return;
        }
        let snapshots: BTreeMap<_, _> = self
            .buffer
            .iter()
            .chain(self.inactive.values())
            .filter_map(|b| {
                b.file.as_ref().map(|file| {
                    let buffer = b.editor.read(cx);
                    (
                        file.path().to_path_buf(),
                        Snapshot {
                            id: b.id,
                            revision: buffer.buffer().revision(),
                            text: buffer.buffer().text(),
                            dirty: buffer.buffer().is_dirty(),
                        },
                    )
                })
            })
            .collect();
        let worker_guard = guard.clone();
        match cx.try_spawn_blocking(
            move || prepare(edit, &snapshots, &worker_guard),
            move |this, result, cx| {
                this.workspace_edit = None;
                if !this.guard_valid(&guard, cx) {
                    this.notice(Some("Edit cancelled because its source changed".into()));
                    return;
                }
                match result {
                    Ok(Ok(prepared)) => {
                        if let Err(error) = this.commit_workspace_edit(prepared, cx) {
                            this.notice(Some(error));
                        }
                    }
                    Ok(Err(error)) => this.notice(Some(error)),
                    Err(error) => this.notice(Some(error.to_string())),
                }
            },
        ) {
            Ok(task) => self.workspace_edit = Some(task),
            Err(error) => self.notice(Some(error.to_string())),
        }
    }
    fn commit_workspace_edit(
        &mut self,
        prepared: Vec<Prepared>,
        cx: &mut Context<'_, Self>,
    ) -> Result<(), String> {
        if self.busy() || self.lifecycle.close_intent().is_some() || self.reloading() {
            return Err("Editor is busy; edit cancelled".into());
        }
        let mut closed = Vec::new();
        let mut open = Vec::new();
        for target in prepared {
            match target {
                Prepared::Open {
                    path,
                    id,
                    transaction,
                } => {
                    let buffer = self
                        .buffer
                        .iter()
                        .chain(self.inactive.values())
                        .find(|b| b.id == id)
                        .ok_or("Edit target was closed")?;
                    let editor = buffer.editor.read(cx);
                    if buffer.file.as_ref().is_none_or(|f| f.path() != path)
                        || editor.buffer().revision() != transaction.revision
                        || buffer.saving.is_some()
                        || editor.has_composition()
                    {
                        return Err("Edit target changed during preparation".into());
                    }
                    transaction
                        .validate_ranges(
                            editor.buffer().revision(),
                            editor.buffer().text().len_chars(),
                        )
                        .map_err(|e| e.to_string())?;
                    open.push((id, transaction));
                }
                Prepared::Closed { file, buffer } => {
                    if self
                        .buffer
                        .iter()
                        .chain(self.inactive.values())
                        .any(|b| b.file.as_ref().is_some_and(|f| f.path() == file.path()))
                    {
                        return Err("Edit target was opened during preparation".into());
                    }
                    let path = file.path().to_path_buf();
                    let mut editor = Editor::from_buffer(buffer);
                    editor
                        .set_language(Language::for_path(&path))
                        .map_err(|e| e.to_string())?;
                    closed.push((file, editor, path));
                }
            }
        }
        if self.buffer_order.len() + closed.len() > 256 {
            return Err("Workspace edit would exceed 256 open documents".into());
        }
        let mut count = closed.len();
        for (id, transaction) in open {
            self.with_buffer(id, cx, |this, cx| {
                // All revisions/ranges were checked above. No UI event can run
                // between preflight and these commits; selections are implicit.
                if this.buffer.editor.update(cx, |editor, _| {
                    editor
                        .apply_transaction(transaction)
                        .expect("preflighted workspace transaction")
                }) {
                    count += 1;
                }
            });
        }
        for (file, editor, path) in closed {
            let editor = cx.new(|_| editor);
            let id = self.next_buffer;
            self.next_buffer += 1;
            let buffer = BufferState::new(id, editor, file, path, cx);
            self.buffer_order.push(id);
            if self.buffer.as_ref().is_none() {
                self.buffer.replace(buffer);
                self.restore_focus_pending = true;
            } else {
                self.inactive.insert(id, buffer);
            }
            self.with_buffer(id, cx, |this, cx| this.start_buffer_semantics(cx));
        }
        self.schedule_recovery(cx);
        self.notice(Some(if count == 0 {
            "No changes needed".into()
        } else {
            format!(
                "Updated {count} {}; changes are unsaved",
                if count == 1 { "document" } else { "documents" }
            )
        }));
        self.title();
        Ok(())
    }
}
fn prepare(
    edit: WorkspaceEdit,
    snapshots: &BTreeMap<PathBuf, Snapshot>,
    guard: &Guard,
) -> Result<Vec<Prepared>, String> {
    let mut paths = std::collections::BTreeSet::new();
    let mut prepared = Vec::new();
    for mut document in edit.documents {
        if document.edits.is_empty() {
            continue;
        }
        document.path = std::fs::canonicalize(&document.path).map_err(|e| e.to_string())?;
        if !paths.insert(document.path.clone()) {
            return Err("Multiple edit targets resolve to the same file".into());
        }
        if let Some(snapshot) = snapshots.get(&document.path) {
            if guard.buffers.get(&document.path) != Some(&(snapshot.id, snapshot.revision)) {
                return Err("Edit target changed while the provider was working".into());
            }
            if snapshot.dirty && guard.source != Some(snapshot.id) && document.revision.is_none() {
                return Err("Unversioned edit targets another unsaved document".into());
            }
            let transaction = velk_editor_model::prepare_document_edit(
                &document,
                &snapshot.text,
                snapshot.revision,
            )?;
            prepared.push(Prepared::Open {
                path: document.path,
                id: snapshot.id,
                transaction,
            });
        } else {
            if guard.buffers.contains_key(&document.path) {
                return Err("Edit target was closed while the provider was working".into());
            }
            if document.revision.is_some() {
                return Err("Versioned edit targets a document that is not open".into());
            }
            let (file, mut buffer) = FileState::read(&document.path).map_err(|e| e.to_string())?;
            let transaction = velk_editor_model::prepare_document_edit(
                &document,
                &buffer.text(),
                buffer.revision(),
            )?;
            if !buffer
                .apply_transaction(transaction)
                .map_err(|e| e.to_string())?
            {
                continue;
            }
            prepared.push(Prepared::Closed { file, buffer });
        }
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use velk_commands::{DocumentEdit, Encoding, Position, TextEdit};
    fn replacement(
        path: PathBuf,
        revision: Option<u64>,
        start: u32,
        end: u32,
        text: &str,
    ) -> DocumentEdit {
        DocumentEdit {
            path,
            revision,
            edits: vec![TextEdit {
                start: Position {
                    line: 0,
                    column: start,
                    encoding: Encoding::Utf16,
                },
                end: Position {
                    line: 0,
                    column: end,
                    encoding: Encoding::Utf16,
                },
                text: text.into(),
            }],
        }
    }
    #[test]
    fn workspace_edits_preflight_every_file_preserve_dirty_text_and_group_undo() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let other = dir.path().join("other.txt");
        std::fs::write(&source, "a𐐀 old").unwrap();
        std::fs::write(&other, "old()").unwrap();
        let mut runtime = rxui::Runtime::new();
        let executor = std::sync::Arc::new(super::super::tests::QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = runtime.update(|cx| {
            let (file, editor) = FileState::open(&source).unwrap();
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, source.clone(), cx))
        });
        let submit = |runtime: &mut rxui::Runtime, documents: Vec<DocumentEdit>| {
            runtime.update(|cx| {
                document.update(cx, |d, cx| {
                    let guard = d.edit_guard(cx);
                    d.apply_workspace_edit(WorkspaceEdit { documents }, guard, cx);
                })
            })
        };
        submit(
            &mut runtime,
            vec![
                replacement(source.clone(), Some(0), 4, 7, "new"),
                replacement(other.clone(), None, 0, 999, "invalid"),
            ],
        );
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            let d = document.read(cx);
            assert_eq!(
                d.buffer.editor.read(cx).buffer().text().to_string(),
                "a𐐀 old"
            );
            assert_eq!(d.ids().len(), 1);
        });
        submit(
            &mut runtime,
            vec![
                replacement(source.clone(), Some(0), 4, 7, "new"),
                replacement(other.clone(), None, 0, 3, "new"),
            ],
        );
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "a𐐀 new"
                );
                assert!(d.buffer.editor.read(cx).buffer().is_dirty());
                assert_eq!(d.ids().len(), 2);
                d.buffer.editor.update(cx, |e, _| {
                    e.execute(velk_editor_model::EditCommand::Undo);
                });
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "a𐐀 old"
                );
                let id = d.inactive.keys().copied().next().unwrap();
                assert_eq!(
                    d.inactive[&id].editor.read(cx).buffer().text().to_string(),
                    "new()"
                );
                d.with_buffer(id, cx, |d, cx| {
                    d.buffer.editor.update(cx, |e, _| {
                        e.execute(velk_editor_model::EditCommand::Undo);
                    });
                    assert_eq!(
                        d.buffer.editor.read(cx).buffer().text().to_string(),
                        "old()"
                    );
                });
            })
        });
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "a𐐀 old");
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "old()");
    }
    #[test]
    fn stale_and_overlapping_targets_reject_the_complete_batch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "abcdef").unwrap();
        let path = std::fs::canonicalize(path).unwrap();
        let snapshot = Snapshot {
            id: 1,
            revision: 7,
            text: ropey::Rope::from_str("abcdef"),
            dirty: true,
        };
        let snapshots = BTreeMap::from([(path.clone(), snapshot)]);
        let guard = Guard {
            view: 0,
            source: Some(1),
            context: CommandContext {
                input: None,
                path: Some(path.clone()),
                language: None,
                revision: Some(7),
                position: Some(0),
            },
            owner: None,
            buffers: BTreeMap::from([(path.clone(), (1, 7))]),
        };
        assert!(
            prepare(
                WorkspaceEdit {
                    documents: vec![replacement(path.clone(), Some(6), 0, 1, "x")]
                },
                &snapshots,
                &guard
            )
            .is_err()
        );
        let mut edit = replacement(path.clone(), Some(7), 0, 3, "x");
        edit.edits.push(
            replacement(path.clone(), Some(7), 2, 4, "y")
                .edits
                .remove(0),
        );
        assert!(
            prepare(
                WorkspaceEdit {
                    documents: vec![edit]
                },
                &snapshots,
                &guard
            )
            .is_err()
        );
        let guard = Guard {
            view: 0,
            source: Some(2),
            ..guard
        };
        assert!(
            prepare(
                WorkspaceEdit {
                    documents: vec![replacement(path, None, 0, 1, "x")]
                },
                &snapshots,
                &guard
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod empty_workspace_tests {
    use super::*;
    #[test]
    fn global_plugin_edits_open_a_visible_dirty_document_from_an_empty_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "before").unwrap();
        let mut runtime = rxui::Runtime::new();
        let executor = std::sync::Arc::new(super::super::tests::QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = runtime.update(|cx| cx.new(|_| Document::empty(1)));
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                let position = |column| velk_commands::Position {
                    line: 0,
                    column,
                    encoding: velk_commands::Encoding::Utf16,
                };
                let guard = d.edit_guard(cx);
                d.apply_workspace_edit(
                    WorkspaceEdit {
                        documents: vec![velk_commands::DocumentEdit {
                            path: path.clone(),
                            revision: None,
                            edits: vec![velk_commands::TextEdit {
                                start: position(0),
                                end: position(6),
                                text: "after".into(),
                            }],
                        }],
                    },
                    guard,
                    cx,
                );
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            let d = document.read(cx);
            assert!(d.has_buffers());
            assert_eq!(
                d.buffer.editor.read(cx).buffer().text().to_string(),
                "after"
            );
            assert!(d.buffer.editor.read(cx).buffer().is_dirty());
        });
    }
}
