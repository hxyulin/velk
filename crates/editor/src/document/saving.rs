//! File destination changes preserve document identity and edit history.
use super::{Document, Intent};
use rxui::prelude::*;

impl Document {
    pub(super) fn save_as(&mut self, then: Option<Intent>, cx: &mut Context<'_, Self>) {
        if !self.has_buffers() || self.busy() || self.any_saving() || self.opening.is_some() {
            return;
        }
        let Some(window) = &self.window else {
            self.lifecycle.cancel_close();
            self.lifecycle.cancel_save_all();
            self.buffer.status = Some("Save As requires a document window.".into());
            return;
        };
        let id = self.buffer.id;
        let mut options = FileDialog::new().parent(window).title("Save As").file_name(
            self.buffer
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
        );
        if let Some(file) = &self.buffer.file
            && let Some(parent) = file.path().parent()
        {
            options = options.directory(parent);
        }
        match cx.save_file(options, move |this, result, cx| {
            this.dialog = None;
            match result {
                Ok(Some(path)) => {
                    this.with_buffer(id, cx, |this, cx| this.save_to(Some(path), then, cx))
                }
                Ok(None) => {
                    this.lifecycle.cancel_close();
                    this.lifecycle.cancel_save_all();
                }
                Err(error) => {
                    this.lifecycle.cancel_close();
                    this.lifecycle.cancel_save_all();
                    this.with_buffer(id, cx, |this, _| {
                        this.buffer.status = Some(format!("Save As failed: {error}"))
                    });
                }
            }
        }) {
            Ok(task) => self.dialog = Some(task),
            Err(error) => {
                self.lifecycle.cancel_close();
                self.lifecycle.cancel_save_all();
                self.buffer.status = Some(format!("Save As failed: {error}"));
            }
        }
    }
    pub(super) fn buffer_save_error(&self, id: u64) -> Option<String> {
        let b = if self.buffer.as_ref().is_some_and(|b| b.id == id) {
            self.buffer.as_ref()
        } else {
            self.inactive.get(&id)
        };
        b.and_then(|b| {
            b.status
                .as_ref()
                .filter(|s| s.starts_with("Save failed:"))
                .map(|s| format!("{}: {s}", b.path.display()))
        })
    }
    pub(super) fn save_all(&mut self, cx: &mut Context<'_, Self>) {
        let ids = self.ids();
        let activity = self.lifecycle_activity();
        if self.lifecycle.begin_save_all(ids, activity) {
            self.continue_save_all(cx);
        }
    }
    pub(super) fn continue_save_all(&mut self, cx: &mut Context<'_, Self>) {
        let documents = self.lifecycle_documents(cx);
        if let Some(id) = self.lifecycle.next_save(&documents) {
            self.with_buffer(id, cx, |this, cx| this.save(None, cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{document::tests::QueuedExecutor, file::FileState};
    use std::sync::Arc;
    use velk_editor_widget::Editor;
    #[test]
    fn recovered_documents_keep_tab_order_and_do_not_reuse_closed_ids() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "saved").unwrap();
        let (file, editor) = FileState::open(&path).unwrap();
        let mut workspace = crate::workspace::Workspace::default();
        workspace.set_root(directory.path().into());
        let mut runtime = Runtime::new();
        runtime.update(|cx| {
            let active = cx.new(|_| Editor::new("scratch"));
            let root =
                cx.new(|cx| Document::new_with_id(11, active, None, "Untitled 11".into(), cx));
            root.update(cx, |d, cx| {
                d.restore_recovered(
                    vec![crate::recovery::RestoredDocument {
                        id: 7,
                        path,
                        file: Some(file),
                        editor,
                    }],
                    workspace,
                    vec![7, 11],
                    20,
                    cx,
                );
                assert_eq!(d.ids(), vec![7, 11]);
                assert_eq!(d.buffer.id, 11);
                assert_eq!(d.workspace.root(), Some(directory.path()));
                d.new_buffer(cx);
                assert_eq!(d.buffer.id, 20);
                assert_eq!(d.ids(), vec![7, 11, 20]);
            });
        });
    }
    #[test]
    fn untitled_save_as_preserves_identity_history_and_edits_made_during_save() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("new.rs");
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let root = runtime.update(|cx| {
            let editor = cx.new(|_| Editor::new(""));
            cx.new(|cx| Document::new(editor, None, "Untitled 1".into(), cx))
        });
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                d.buffer.editor.update(cx, |e, _| e.insert("first"));
                d.save_to(Some(destination.clone()), None, cx);
                d.buffer.editor.update(cx, |e, _| e.insert("newer"));
            })
        });
        executor.complete(&mut runtime);
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "first");
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                assert_eq!(d.buffer.id, 1);
                assert_eq!(
                    d.buffer.file.as_ref().unwrap().path(),
                    std::fs::canonicalize(&destination).unwrap()
                );
                assert!(d.buffer.editor.read(cx).buffer().is_dirty());
                let mut history = velk_core::Buffer::restore(
                    &d.buffer
                        .editor
                        .read(cx)
                        .buffer()
                        .snapshot()
                        .encode()
                        .unwrap(),
                )
                .unwrap();
                history.undo();
                assert!(!history.is_dirty());
                assert_eq!(history.text().to_string(), "first");
                history.undo();
                assert!(history.is_dirty());
                d.new_buffer(cx);
                assert_eq!(d.buffer.id, 2);
                assert!(d.buffer.file.is_none());
                d.save(Some(Intent::Buffer), cx); // no window: cancelled safely
                assert!(d.lifecycle.close_intent().is_none());
                assert!(d.buffer.saving.is_none());
            })
        });
    }
    #[test]
    fn save_all_keeps_selection_and_stops_on_conflicts_without_touching_later_documents() {
        let directory = tempfile::tempdir().unwrap();
        let a = directory.path().join("a.txt");
        let b = directory.path().join("b.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let root = runtime.update(|cx| {
            let (file, editor) = FileState::open(&a).unwrap();
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, a.clone(), cx))
        });
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                d.buffer.editor.update(cx, |e, _| e.insert("A"));
                let (file, mut editor) = FileState::open(&b).unwrap();
                editor.insert("B");
                d.add_buffer(file, editor, b.clone(), cx);
                d.save_all(cx);
                assert_eq!(d.buffer.id, 2);
            })
        });
        executor.complete(&mut runtime);
        executor.complete(&mut runtime);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "Aa");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "Bb");
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                assert_eq!(d.buffer.id, 2);
                assert!(!d.lifecycle.save_all_pending());
                for id in d.ids() {
                    d.with_buffer(id, cx, |d, cx| {
                        d.buffer.editor.update(cx, |e, _| e.insert("more"))
                    });
                }
                std::fs::write(&a, "external").unwrap();
                d.save_all(cx);
            })
        });
        executor.complete(&mut runtime);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "external");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "Bb");
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                assert!(!d.lifecycle.save_all_pending());
                assert!(d.buffer.editor.read(cx).buffer().is_dirty());
                assert!(d.inactive[&1].editor.read(cx).buffer().is_dirty());
            })
        });
    }
}
