//! Background recovery and disk reconciliation; callbacks resolve stable IDs.
use super::Document;
use crate::{file::FileState, recovery};
use rxui::{Element, prelude::*};
use std::time::Duration;

#[derive(PartialEq)]
struct DocumentKey {
    id: u64,
    revision: u64,
    selection: velk_core::Selection,
    scroll: [f32; 2],
    file: Option<FileState>,
    path: std::path::PathBuf,
}
#[derive(PartialEq)]
pub(super) struct RecoveryKey {
    documents: Vec<DocumentKey>,
    active: u64,
    next_id: u64,
    root: Option<std::path::PathBuf>,
}

impl Document {
    pub fn enable_recovery(&mut self, recovery: recovery::Handle, cx: &mut Context<'_, Self>) {
        self.recovery = Some(recovery);
        self.schedule_recovery(cx);
        self.arm_maintenance(cx);
    }
    pub(super) fn schedule_recovery(&mut self, cx: &mut Context<'_, Self>) {
        if self.recovery.is_none() || self.recovery_task.is_some() || self.reloading() {
            return;
        }
        match cx.try_spawn(rxui::sleep(Duration::from_millis(200)), |this, _, cx| {
            this.recovery_task = None;
            this.capture_recovery(cx);
        }) {
            Ok(task) => self.recovery_task = Some(task),
            Err(error) => self.notice(Some(format!("Cannot schedule recovery: {error}"))),
        }
    }
    fn capture_recovery(&mut self, cx: &impl rxui::ReadContext) {
        let Some(recovery) = &self.recovery else {
            return;
        };
        if self.reloading() {
            return;
        }
        let key = RecoveryKey {
            active: self.buffer.as_ref().map_or(0, |b| b.id),
            next_id: self.next_buffer,
            root: self.workspace.root().map(std::path::Path::to_path_buf),
            documents: self
                .ids()
                .into_iter()
                .map(|id| {
                    let b = if id == self.buffer.id {
                        &self.buffer
                    } else {
                        &self.inactive[&id]
                    };
                    let editor = b.editor.read(cx);
                    DocumentKey {
                        id,
                        revision: editor.buffer().revision(),
                        selection: editor.buffer().selection(),
                        scroll: editor.scroll_offset(),
                        file: b.file.clone(),
                        path: b.path.clone(),
                    }
                })
                .collect(),
        };
        if self.recovery_key.as_ref() == Some(&key) && recovery.error().is_none() {
            return;
        }
        self.recovery_key = Some(key);
        let documents = self
            .ids()
            .into_iter()
            .map(|id| {
                let b = if id == self.buffer.id {
                    &self.buffer
                } else {
                    &self.inactive[&id]
                };
                let editor = b.editor.read(cx);
                recovery::PendingDocument {
                    id,
                    path: b.path.clone(),
                    file: b.file.clone(),
                    buffer: editor.buffer().snapshot(),
                    scroll: editor.scroll_offset(),
                }
            })
            .collect();
        recovery.update(recovery::Pending {
            next_id: self.next_buffer,
            documents,
            active: self.buffer.as_ref().map_or(0, |b| b.id),
            workspace: self.workspace.clone(),
        });
    }
    pub fn start_document_maintenance(&mut self, cx: &mut Context<'_, Self>) {
        self.reload_keybindings(cx);
        self.arm_maintenance(cx);
    }
    pub fn recovery_notice(&mut self, notice: String) {
        self.notice(Some(notice));
    }
    fn arm_maintenance(&mut self, cx: &mut Context<'_, Self>) {
        if self.maintenance.is_some() {
            return;
        }
        if let Ok(task) = cx.try_spawn(rxui::sleep(Duration::from_secs(2)), |this, _, cx| {
            this.maintenance = None;
            if this.reloading() {
                this.arm_maintenance(cx);
                return;
            }
            this.schedule_recovery(cx);
            if let Some(error) = this.recovery.as_ref().and_then(|r| r.error()) {
                this.notice(Some(format!("Recovery unavailable: {error}")));
            }
            this.refresh_keybindings(cx);
            this.check_disk(cx);
            this.arm_maintenance(cx);
        }) {
            self.maintenance = Some(task);
        }
    }
    fn check_disk(&mut self, cx: &mut Context<'_, Self>) {
        if self.disk_check.is_some()
            || self.dialog.is_some()
            || self.lifecycle.close_intent().is_some()
        {
            return;
        }
        let files: Vec<_> = self
            .buffer
            .iter()
            .chain(self.inactive.values())
            .filter(|b| b.saving.is_none())
            .filter_map(|b| {
                b.file
                    .clone()
                    .map(|file| (b.id, file, b.disk_stamp.clone()))
            })
            .collect();
        let plugins = self.plugins.as_ref().map(|p| p.client.clone());
        if files.is_empty() && plugins.is_none() {
            return;
        }
        if let Ok(task) = cx.try_spawn_blocking(
            move || {
                if let Some(plugins) = plugins {
                    plugins.reconcile_diagnostics();
                }
                files
                    .into_iter()
                    .map(|(id, file, previous)| {
                        let stamp = file.disk_stamp().ok();
                        let changed = if stamp.is_some() && stamp == previous {
                            None
                        } else {
                            Some(file.changed_on_disk().unwrap_or(true))
                        };
                        (id, file, stamp, changed)
                    })
                    .collect::<Vec<_>>()
            },
            |this, result, cx| {
                this.disk_check = None;
                if let Ok(results) = result {
                    for (id, baseline, stamp, changed) in results {
                        this.with_buffer(id, cx, |this, cx| {
                            if this.buffer.saving.is_some()
                                || this.buffer.file.as_ref() != Some(&baseline)
                            {
                                return;
                            }
                            let automatic = stamp.as_ref().is_some_and(|stamp| {
                                stamp.length() <= velk_plugin_contracts::host::MAX_FILE_BYTES as u64
                            });
                            this.buffer.disk_stamp = stamp;
                            if let Some(changed) = changed {
                                this.buffer.disk_changed = changed;
                            }
                            if this.buffer.disk_changed
                                && automatic
                                && !this.buffer.editor.read(cx).buffer().is_dirty()
                                && !this.buffer.editor.read(cx).has_composition()
                                && !this.busy()
                                && this.workspace_edit.is_none()
                            {
                                this.reload_from_disk(cx);
                            }
                        });
                    }
                }
            },
        ) {
            self.disk_check = Some(task);
        }
    }
    fn disk_notice_visible(&self) -> bool {
        self.buffer
            .as_ref()
            .is_some_and(|buffer| buffer.disk_changed && !buffer.disk_notice_dismissed)
    }
    fn dismiss_disk_notice(&mut self, id: u64, cx: &mut Context<'_, Self>) {
        self.with_buffer(id, cx, |this, _| this.buffer.disk_notice_dismissed = true);
    }
    pub(super) fn disk_notice(&self, cx: &mut ViewContext<'_, Self>) -> Option<Element> {
        if !self.disk_notice_visible() {
            return None;
        }
        let id = self.buffer.id;
        let busy = self.busy() || self.lifecycle.close_intent().is_some();
        Some(column().key("disk-change-notice").fill_width().padding(10.).gap(6.)
            .background(ThemeColor::Raised)
            .child(label("This buffer is out of sync with disk, or the file is unavailable. Your editor contents have been kept.").font_size(12.5))
            .child(row().gap(8.)
                .child(button("Reload from disk…").key("disk-reload").disabled(busy)
                    .on_click(cx.listener(|this, _, cx| this.confirm_reload(cx))))
                .child(button("Open disk version").key("disk-compare").disabled(busy)
                    .on_click(cx.listener(|this, _, cx| this.open_disk_version(cx))))
                .child(button("Overwrite disk…").key("disk-overwrite").disabled(busy)
                    .on_click(cx.listener(|this, _, cx| this.confirm_overwrite(cx))))
                .child(button("Dismiss").key("disk-dismiss")
                    .on_click(cx.listener(move |this, _, cx| this.dismiss_disk_notice(id, cx))))))
    }
    fn confirm_reload(&mut self, cx: &mut Context<'_, Self>) {
        if self.busy() {
            return;
        }
        if !self.buffer.editor.read(cx).buffer().is_dirty() {
            self.reload_from_disk(cx);
            return;
        }
        let Some(window) = &self.window else {
            return;
        };
        let id = self.buffer.id;
        let revision = self.buffer.editor.read(cx).buffer().revision();
        let options = MessageDialog::new()
            .parent(window)
            .title("Reload from disk?")
            .description(
                "Discard this document’s unsaved changes and load the current disk version?",
            )
            .level(MessageLevel::Warning)
            .buttons(MessageButtons::YesNo);
        if let Ok(task) = cx.show_message(options, move |this, result, cx| {
            this.dialog = None;
            if matches!(result, Ok(MessageResponse::Yes)) {
                this.with_buffer(id, cx, |this, cx| {
                    if this.buffer.editor.read(cx).buffer().revision() == revision {
                        this.reload_from_disk(cx);
                    } else {
                        this.notice(Some(
                            "Document changed while confirming reload; please try again.".into(),
                        ));
                    }
                });
            }
        }) {
            self.dialog = Some(task);
        }
    }
    fn reload_from_disk(&mut self, cx: &mut Context<'_, Self>) {
        let Some(file) = self.buffer.file.clone() else {
            return;
        };
        if self.busy() {
            return;
        }
        let baseline = file.clone();
        let id = self.buffer.id;
        let revision = self.buffer.editor.read(cx).buffer().revision();
        self.notice(Some("Reloading from disk…".into()));
        match cx.try_spawn_blocking(
            move || FileState::read(file.path()),
            move |this, result, cx| {
                this.with_buffer(id, cx, |this, cx| {
                    this.buffer.saving = None;
                    match result {
                        Ok(Ok((file, buffer)))
                            if this.buffer.file.as_ref() == Some(&baseline)
                                && this.buffer.editor.read(cx).buffer().revision() == revision
                                && !this.buffer.editor.read(cx).has_composition() =>
                        {
                            let applied = this.buffer.editor.update(cx, |editor, _| {
                                let model = editor.buffer();
                                if model.text() != buffer.text() {
                                    let length = buffer.text().len_chars();
                                    let selection = model.selection();
                                    editor.apply_transaction(velk_core::Transaction {
                                        revision: model.revision(),
                                        edits: vec![velk_core::TextEdit {
                                            range: 0..model.text().len_chars(),
                                            text: buffer.text().to_string(),
                                        }],
                                        selection: Some(velk_core::Selection {
                                            anchor: selection.anchor.min(length),
                                            head: selection.head.min(length),
                                        }),
                                    })?;
                                }
                                editor.mark_saved(editor.buffer().checkpoint());
                                Ok::<_, velk_core::TransactionError>(())
                            });
                            if let Err(error) = applied {
                                this.notice(Some(format!("Reload failed: {error}")));
                                return;
                            }
                            this.buffer.file = Some(file);
                            this.buffer.disk_stamp = None;
                            this.buffer.disk_changed = false;
                            this.buffer.dirty = false;
                            this.buffer.status = None;
                            this.sync_semantics(cx);
                            if let Some(state) = &this.buffer.semantic {
                                state.service.saved(this.semantic_snapshot(cx));
                            }
                            this.restart_search(cx);
                            let buffer = this.buffer_info(&this.buffer, cx);
                            let saved_revision = buffer.revision;
                            this.publish_buffer_event(
                                velk_plugin_contracts::buffers::Event::Saved {
                                    buffer,
                                    saved_revision,
                                },
                                cx,
                            );
                        }
                        Ok(Ok(_)) => this.notice(Some(
                            "Document changed while reloading; local edits were kept.".into(),
                        )),
                        Ok(Err(error)) => this.notice(Some(format!("Reload failed: {error}"))),
                        Err(error) => this.notice(Some(format!("Reload failed: {error}"))),
                    }
                });
                this.title();
                this.schedule_recovery(cx);
            },
        ) {
            Ok(task) => self.buffer.saving = Some(task),
            Err(error) => self.notice(Some(format!("Reload failed: {error}"))),
        }
    }
    fn open_disk_version(&mut self, cx: &mut Context<'_, Self>) {
        let Some(file) = self.buffer.file.clone() else {
            return;
        };
        let name = format!(
            "Disk version — {}",
            self.buffer
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        );
        let id = self.buffer.id;
        match cx.try_spawn_blocking(
            move || {
                let (_, buffer) = FileState::read(file.path())?;
                let mut detached = velk_core::Buffer::new("");
                detached.insert(&buffer.text().to_string());
                Ok::<_, std::io::Error>(detached)
            },
            move |this, result, cx| {
                this.opening = None;
                match result {
                    Ok(Ok(buffer)) => this.add_buffer(
                        None,
                        velk_editor_widget::Editor::from_buffer(buffer),
                        name.into(),
                        cx,
                    ),
                    Ok(Err(error)) => this.with_buffer(id, cx, |this, _| {
                        this.notice(Some(format!("Cannot read disk version: {error}")))
                    }),
                    Err(error) => this.with_buffer(id, cx, |this, _| {
                        this.notice(Some(format!("Cannot read disk version: {error}")))
                    }),
                }
            },
        ) {
            Ok(task) => self.opening = Some(task),
            Err(error) => self.notice(Some(format!("Cannot read disk version: {error}"))),
        }
    }

    fn confirm_overwrite(&mut self, cx: &mut Context<'_, Self>) {
        if self.busy() {
            return;
        }
        let Some(window) = &self.window else {
            return;
        };
        let id = self.buffer.id;
        let options = MessageDialog::new().parent(window).title("Overwrite disk changes?")
            .description("Replace the current disk file with this document’s contents? Use Save As to keep both versions instead.")
            .level(MessageLevel::Warning).buttons(MessageButtons::YesNo);
        if let Ok(task) = cx.show_message(options, move |this, result, cx| {
            this.dialog = None;
            if matches!(result, Ok(MessageResponse::Yes)) {
                this.with_buffer(id, cx, |this, cx| this.save_overwriting_disk(cx));
            }
        }) {
            self.dialog = Some(task);
        }
    }
    fn save_overwriting_disk(&mut self, cx: &mut Context<'_, Self>) {
        // A separate save mode establishes the approved disk baseline on the
        // worker, then uses the same conflict-checked atomic write path.
        self.overwrite_approved = true;
        self.save(None, cx);
        self.overwrite_approved = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::tests::QueuedExecutor;
    use std::sync::Arc;
    #[test]
    fn dismissing_disk_notice_is_local_to_the_buffer_and_survives_later_changes() {
        let mut runtime = Runtime::new();
        runtime
            .configure_tasks(Arc::new(QueuedExecutor::default()), || {})
            .unwrap();
        let root = runtime.update(|cx| cx.new(|_| Document::empty(1)));
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                assert!(!d.disk_notice_visible());
                d.new_buffer(cx);
                let first = d.buffer.id;
                d.buffer.disk_changed = true;
                assert!(d.disk_notice_visible());
                d.dismiss_disk_notice(first, cx);
                assert!(!d.disk_notice_visible());
                assert!(
                    d.buffer.disk_changed,
                    "dismissal cannot clear the disk conflict"
                );
                d.buffer.disk_changed = false;
                d.buffer.disk_changed = true;
                assert!(
                    !d.disk_notice_visible(),
                    "later disk changes do not revive a dismissed notice"
                );
                d.new_buffer(cx);
                let second = d.buffer.id;
                d.buffer.disk_changed = true;
                assert!(
                    d.disk_notice_visible(),
                    "another buffer keeps its own warning"
                );
                d.dismiss_disk_notice(first, cx);
                assert_eq!(d.buffer.id, second);
                assert!(d.disk_notice_visible());
                d.select_buffer(first, cx);
                assert!(!d.disk_notice_visible());
                d.close_buffer(first, cx);
                d.new_buffer(cx);
                d.buffer.disk_changed = true;
                assert!(
                    d.disk_notice_visible(),
                    "new buffers start with warnings enabled"
                );
            })
        });
    }
    #[test]
    fn large_external_replacements_require_explicit_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let root = runtime.update(|cx| {
            let (file, editor) = FileState::open(&path).unwrap();
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, path.clone(), cx))
        });
        std::fs::write(
            &path,
            "x".repeat(velk_plugin_contracts::host::MAX_FILE_BYTES + 1),
        )
        .unwrap();
        runtime.update(|cx| root.update(cx, |d, cx| d.check_disk(cx)));
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            let d = root.read(cx);
            assert!(d.buffer.disk_changed);
            assert!(d.buffer.saving.is_none());
            assert_eq!(
                d.buffer.editor.read(cx).buffer().text().to_string(),
                "original"
            );
        });
    }
    #[test]
    fn clean_buffers_reconcile_external_changes_without_reusing_revisions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let root = runtime.update(|cx| {
            let (file, editor) = FileState::open(&path).unwrap();
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, path.clone(), cx))
        });
        let revision =
            runtime.update(|cx| root.read(cx).buffer.editor.read(cx).buffer().revision());
        std::fs::write(&path, "external").unwrap();
        runtime.update(|cx| root.update(cx, |d, cx| d.check_disk(cx)));
        executor.complete(&mut runtime);
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "external"
                );
                assert!(d.buffer.editor.read(cx).buffer().revision() > revision);
                assert!(!d.buffer.editor.read(cx).buffer().is_dirty());
                assert!(!d.buffer.disk_changed);
                d.buffer.editor.update(cx, |editor, _| {
                    editor.execute(velk_editor_widget::EditCommand::Undo)
                });
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "original"
                );
                assert!(
                    d.buffer.editor.read(cx).buffer().is_dirty(),
                    "undoing a disk reload is an unsaved local change"
                );
            })
        });
    }
    #[test]
    fn external_checks_and_reloads_never_discard_edits_made_while_io_is_pending() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let root = runtime.update(|cx| {
            let (file, editor) = FileState::open(&path).unwrap();
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, path.clone(), cx))
        });
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                d.buffer.editor.update(cx, |e, _| e.insert("dirty"))
            })
        });
        std::fs::write(&path, "external").unwrap();
        runtime.update(|cx| root.update(cx, |d, cx| d.check_disk(cx)));
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                assert!(d.buffer.disk_changed);
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "dirtyoriginal"
                );
                d.reload_from_disk(cx);
                d.buffer.editor.update(cx, |e, _| e.insert("local"));
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "dirtylocaloriginal"
                );
                assert!(d.buffer.disk_changed);
                d.reload_from_disk(cx); // confirmed explicit reload, now no intervening edit
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                assert_eq!(d.buffer.id, 1);
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "external"
                );
                assert!(!d.buffer.editor.read(cx).buffer().is_dirty());
                assert!(!d.buffer.disk_changed);
            })
        });
    }
}
