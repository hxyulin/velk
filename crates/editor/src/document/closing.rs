use super::{Document, Intent};
use rxui::prelude::*;
use velk_core::lifecycle::CloseResponse;

struct Save;
impl Command for Save {}
struct SaveAll;
impl Command for SaveAll {}
struct Discard;
impl Command for Discard {}
struct DiscardAll;
impl Command for DiscardAll {}
struct Cancel;
impl Command for Cancel {}

impl Document {
    fn respond_to_close(&mut self, id: u64, response: CloseResponse, cx: &mut Context<'_, Self>) {
        let documents = self.lifecycle_documents(cx);
        let action = self.lifecycle.respond(id, response, &documents);
        self.close_action(action, cx);
        self.restore_focus_pending = self.lifecycle.close_intent().is_none();
    }

    pub(super) fn close_confirmation(
        &self,
        cx: &mut ViewContext<'_, Self>,
    ) -> Option<rxui::Element> {
        let id = self.lifecycle.prompted_document()?;
        let buffer = self
            .buffer
            .iter()
            .chain(self.inactive.values())
            .find(|b| b.id == id)?;
        let save = cx
            .command(Save, move |this, _, cx| {
                this.respond_to_close(id, CloseResponse::Save, cx)
            })
            .label("Save")
            .shortcut(Shortcut::new(KeyboardKey::Enter, Default::default()));
        let discard = cx
            .command(Discard, move |this, _, cx| {
                this.respond_to_close(id, CloseResponse::Discard, cx)
            })
            .label("Don't Save");
        let cancel = cx
            .command(Cancel, move |this, _, cx| {
                this.respond_to_close(id, CloseResponse::Cancel, cx)
            })
            .label("Cancel")
            .shortcut(Shortcut::new(KeyboardKey::Escape, Default::default()));
        let documents = self.lifecycle_documents(cx);
        let remaining = self.lifecycle.remaining_dirty(&documents);
        let mut actions = row()
            .gap(8.)
            .child(save.button().key("close-save"))
            .on_command(save)
            .child(discard.button().key("close-discard"))
            .on_command(discard);
        if self.lifecycle.close_intent() != Some(Intent::Buffer) && remaining > 1 {
            let discard_all = cx
                .command(DiscardAll, move |this, _, cx| {
                    this.respond_to_close(id, CloseResponse::DiscardAll, cx)
                })
                .label("Don't Save All");
            let save_all = cx
                .command(SaveAll, move |this, _, cx| {
                    this.respond_to_close(id, CloseResponse::SaveAll, cx)
                })
                .label("Save All");
            actions = actions
                .child(discard_all.button().key("close-discard-all"))
                .child(save_all.button().key("close-save-all"))
                .on_command(discard_all)
                .on_command(save_all);
        }
        actions = actions
            .child(cancel.button().key("close-cancel"))
            .on_command(cancel);
        Some(
            modal(
                column()
                    .gap(12.)
                    .padding(16.)
                    .child(label("Save changes?"))
                    .child(label(format!(
                        "Save changes to ‘{}’ before closing?",
                        buffer.path.display()
                    )))
                    .children(
                        (remaining > 1 && self.lifecycle.close_intent() != Some(Intent::Buffer))
                            .then(|| {
                                label(format!("{remaining} documents still have unsaved changes."))
                            }),
                    )
                    .child(actions),
            )
            .key("save-confirmation")
            .width(640.)
            .accessibility_label("Save changes before closing")
            .on_dismiss(cx.listener(move |this, _: &DismissEvent, cx| {
                this.respond_to_close(id, CloseResponse::Cancel, cx)
            }))
            .into_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::{buffers, tests::QueuedExecutor},
        file::FileState,
    };
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };

    use rxui::{ElementId, TextMeasure, TextRequest, UiError};

    struct Measure;
    impl TextMeasure for Measure {
        fn measure(&mut self, _: ElementId, request: TextRequest<'_>) -> Result<[f32; 2], UiError> {
            Ok([request.text.len() as f32 * 7., 18.])
        }
    }
    fn prepare(ui: &mut Ui<Document>, runtime: &mut Runtime) {
        runtime.flush().unwrap();
        ui.prepare(runtime, [900., 600.], &mut Measure).unwrap();
    }
    fn changed_documents(
        runtime: &mut Runtime,
        directory: &Path,
        count: usize,
    ) -> (Entity<Document>, Vec<PathBuf>) {
        let paths: Vec<_> = (0..count)
            .map(|i| directory.join(format!("file-{i}.rs")))
            .collect();
        for path in &paths {
            std::fs::write(path, "symbol").unwrap();
        }
        let root = runtime.update(|cx| {
            let (file, mut editor) = FileState::open(&paths[0]).unwrap();
            editor.insert("renamed_");
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, paths[0].clone(), cx))
        });
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                for path in &paths[1..] {
                    let (file, mut editor) = FileState::open(path).unwrap();
                    editor.insert("renamed_");
                    d.add_buffer(file, editor, path.clone(), cx);
                }
            })
        });
        (root, paths)
    }

    #[test]
    fn close_prompt_selects_the_same_document_in_the_workspace_and_visible_pane() {
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = Runtime::new();
        let (root, _) = changed_documents(&mut runtime, directory.path(), 2);
        runtime.update(|cx| {
            root.update(cx, |this, cx| {
                assert_eq!(this.buffer.id, 2);
                this.intent(Intent::Close, cx);
                assert_eq!(this.lifecycle.prompted_document(), Some(2));
                this.respond_to_close(2, CloseResponse::Discard, cx);
                assert_eq!(this.lifecycle.prompted_document(), Some(1));
                assert_eq!(this.buffer.id, 1);
                assert_eq!(this.documents.active(), Some(1));
                this.documents.validate_layout(&this.views.layout).unwrap();
                this.respond_to_close(1, CloseResponse::Cancel, cx);
                this.cycle_buffer(false, cx);
                assert_eq!(this.buffer.id, 2);
                assert_eq!(this.documents.active(), Some(2));
                this.documents.validate_layout(&this.views.layout).unwrap();
            });
        });
    }
    #[test]
    fn repeated_buffer_discards_keep_confirmation_and_commands_usable() {
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = Runtime::new();
        let (root, paths) = changed_documents(&mut runtime, directory.path(), 5);
        let mut ui = Ui::new(&mut runtime, root.clone()).unwrap();
        for remaining in (1..=5).rev() {
            prepare(&mut ui, &mut runtime);
            assert_eq!(
                ui.dispatch_command::<buffers::Close>(&mut runtime).unwrap(),
                CommandStatus::Handled
            );
            prepare(&mut ui, &mut runtime);
            assert!(
                ui.elements()
                    .any(|e| e.key == Some(&Key::from("save-confirmation")))
            );
            assert!(
                !ui.elements()
                    .any(|e| e.key == Some(&Key::from("close-discard-all")))
            );
            assert_eq!(
                ui.dispatch_command::<Discard>(&mut runtime).unwrap(),
                CommandStatus::Handled
            );
            runtime.update(|cx| {
                let d = root.read(cx);
                assert_eq!(d.ids().len(), remaining - 1);
                assert!(d.lifecycle.close_intent().is_none());
                assert!(d.dialog.is_none());
            });
        }
        prepare(&mut ui, &mut runtime);
        assert_eq!(
            ui.dispatch_command::<buffers::New>(&mut runtime).unwrap(),
            CommandStatus::Handled
        );
        for path in paths {
            assert_eq!(std::fs::read_to_string(path).unwrap(), "symbol");
        }
    }

    #[test]
    fn window_confirmation_survives_multiple_discards_and_cancel_resets_it() {
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = Runtime::new();
        let (root, paths) = changed_documents(&mut runtime, directory.path(), 5);
        let mut ui = Ui::new(&mut runtime, root.clone()).unwrap();
        runtime.update(|cx| root.update(cx, |d, cx| d.intent(Intent::Close, cx)));
        for _ in 0..2 {
            prepare(&mut ui, &mut runtime);
            assert_eq!(
                ui.dispatch_command::<Discard>(&mut runtime).unwrap(),
                CommandStatus::Handled
            );
        }
        prepare(&mut ui, &mut runtime);
        assert!(
            ui.elements()
                .any(|e| e.key == Some(&Key::from("close-discard-all")))
        );
        ui.key(
            &mut runtime,
            KeyEvent {
                key: KeyboardKey::Escape,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
        )
        .unwrap();
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                assert_eq!(d.ids().len(), 5);
                assert!(!d.busy());
                assert!(d.lifecycle.close_intent().is_none());
                d.intent(Intent::Close, cx);
            })
        });
        prepare(&mut ui, &mut runtime);
        assert_eq!(
            ui.dispatch_command::<DiscardAll>(&mut runtime).unwrap(),
            CommandStatus::Handled
        );
        runtime.update(|cx| {
            let d = root.read(cx);
            assert!(d.lifecycle.close_intent().is_none());
            assert!(d.lifecycle_documents(cx).iter().all(|d| d.dirty));
        });
        for path in paths {
            assert_eq!(std::fs::read_to_string(path).unwrap(), "symbol");
        }
    }

    #[test]
    fn enter_saves_and_closes_only_the_prompted_buffer() {
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let (root, paths) = changed_documents(&mut runtime, directory.path(), 2);
        let mut ui = Ui::new(&mut runtime, root.clone()).unwrap();
        runtime.update(|cx| root.update(cx, |d, cx| d.intent(Intent::Buffer, cx)));
        prepare(&mut ui, &mut runtime);
        ui.key(
            &mut runtime,
            KeyEvent {
                key: KeyboardKey::Enter,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
        )
        .unwrap();
        executor.complete(&mut runtime);
        assert_eq!(
            std::fs::read_to_string(&paths[1]).unwrap(),
            "renamed_symbol"
        );
        assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), "symbol");
        runtime.update(|cx| {
            let d = root.read(cx);
            assert_eq!(d.ids(), [1]);
            assert!(d.buffer.editor.read(cx).buffer().is_dirty());
            assert!(d.lifecycle.close_intent().is_none());
        });
    }

    #[test]
    fn save_all_closing_writes_remaining_documents_and_stops_at_conflicts() {
        for conflict in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let mut runtime = Runtime::new();
            let executor = Arc::new(QueuedExecutor::default());
            runtime.configure_tasks(executor.clone(), || {}).unwrap();
            let (root, paths) = changed_documents(&mut runtime, directory.path(), 3);
            let mut ui = Ui::new(&mut runtime, root.clone()).unwrap();
            if conflict {
                std::fs::write(&paths[0], "external").unwrap();
            }
            runtime.update(|cx| root.update(cx, |d, cx| d.intent(Intent::Close, cx)));
            prepare(&mut ui, &mut runtime);
            assert_eq!(
                ui.dispatch_command::<SaveAll>(&mut runtime).unwrap(),
                CommandStatus::Handled
            );
            executor.complete(&mut runtime);
            executor.complete(&mut runtime);
            if !conflict {
                executor.complete(&mut runtime);
            }
            assert_eq!(
                std::fs::read_to_string(&paths[2]).unwrap(),
                "renamed_symbol"
            );
            assert_eq!(
                std::fs::read_to_string(&paths[0]).unwrap(),
                if conflict {
                    "external"
                } else {
                    "renamed_symbol"
                }
            );
            assert_eq!(
                std::fs::read_to_string(&paths[1]).unwrap(),
                if conflict { "symbol" } else { "renamed_symbol" }
            );
            runtime.update(|cx| {
                let d = root.read(cx);
                assert_eq!(d.ids().len(), 3);
                assert!(d.lifecycle.close_intent().is_none());
                assert!(d.lifecycle.prompted_document().is_none());
                assert!(!d.any_saving());
            });
        }
    }
}
