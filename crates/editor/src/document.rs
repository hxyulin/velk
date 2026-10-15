mod plugins;
mod semantic;
mod shell;
use crate::file::FileState;
use rxui::{Subscription, prelude::*};
use std::path::PathBuf;
use velk_editor_widget::Editor;

struct Save;
impl Command for Save {}
struct LoadPlugins;
impl Command for LoadPlugins {}
struct UnloadPlugins;
impl Command for UnloadPlugins {}
struct TogglePlugins;
impl Command for TogglePlugins {}
struct ClosePlugins;
impl Command for ClosePlugins {}
struct Hover;
impl Command for Hover {}
struct ToggleSettings;
impl Command for ToggleSettings {}
struct CloseSettings;
impl Command for CloseSettings {}
#[derive(Clone, Copy)]
pub enum Intent {
    Close,
    Quit,
}

pub struct Document {
    editor: Entity<Editor>,
    file: FileState,
    path: PathBuf,
    pub window: Option<WindowHandle>,
    dirty: bool,
    status: Option<String>,
    saving: Option<Task>,
    dialog: Option<DialogTask>,
    _subscription: Subscription,
    semantic: Option<semantic::State>,
    plugins: Option<plugins::State>,
    plugin_menu_open: bool,
    editor_focus: rxui::FocusHandle,
    shell: shell::State,
}
impl Document {
    pub fn new(
        editor: Entity<Editor>,
        file: FileState,
        path: PathBuf,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let dirty = editor.read(cx).buffer().is_dirty();
        let subscription = cx
            .observe(&editor, |this, editor, cx| {
                let dirty = editor.read(cx).buffer().is_dirty();
                let semantic_changed = this.sync_semantics(cx);
                if this.dirty != dirty {
                    this.dirty = dirty;
                    this.title();
                } else if !semantic_changed {
                    cx.unchanged();
                }
            })
            .expect("editor entity is live");
        Self {
            editor,
            file,
            path,
            window: None,
            dirty,
            status: None,
            saving: None,
            dialog: None,
            _subscription: subscription,
            semantic: None,
            plugins: None,
            plugin_menu_open: false,
            editor_focus: rxui::FocusHandle::new(),
            shell: shell::State::default(),
        }
    }
    pub fn title(&self) {
        if let Some(window) = self.window.as_ref().and_then(WindowHandle::native_window) {
            window.set_title(&format!(
                "Velk — {}{}",
                self.path.display(),
                if self.dirty { " *" } else { "" }
            ));
        }
    }
    fn busy(&self) -> bool {
        self.saving.is_some() || self.dialog.is_some()
    }
    fn save(&mut self, then: Option<Intent>, cx: &mut Context<'_, Self>) {
        if self.busy() {
            return;
        }
        let (text, checkpoint) = {
            let editor = self.editor.read(cx);
            (editor.buffer().text().clone(), editor.buffer().checkpoint())
        };
        let saved_snapshot = self.semantic.as_ref().map(|_| self.semantic_snapshot(cx));
        let file = self.file.clone();
        self.status = Some("Saving…".into());
        match cx.try_spawn_blocking(
            move || file.save(text),
            move |this, result, cx| {
                this.saving = None;
                match result {
                    Ok(Ok(file)) => {
                        this.file = file;
                        if let (Some(state), Some(snapshot)) = (&this.semantic, saved_snapshot) {
                            state.service.saved(snapshot);
                        }
                        this.editor.update(cx, |editor, _| {
                            editor.mark_saved(checkpoint);
                        });
                        this.dirty = this.editor.read(cx).buffer().is_dirty();
                        this.title();
                        this.status = None;
                        if let Some(intent) = then {
                            if this.dirty {
                                this.status = Some(
                                    "Saved the snapshot; newer edits are still unsaved.".into(),
                                );
                            } else {
                                this.finish(intent, cx);
                            }
                        }
                    }
                    Ok(Err(error)) => this.status = Some(format!("Save failed: {error}")),
                    Err(error) => this.status = Some(format!("Save failed: {error}")),
                }
            },
        ) {
            Ok(task) => self.saving = Some(task),
            Err(error) => self.status = Some(format!("Save failed: {error}")),
        }
    }
    pub fn intent(&mut self, intent: Intent, cx: &mut Context<'_, Self>) {
        if self.busy() {
            if self.saving.is_some() {
                self.status = Some("Saving… Please wait before closing.".into());
            }
            return;
        }
        // Read the buffer directly: its observer may not yet have run.
        if !self.editor.read(cx).buffer().is_dirty() {
            self.finish(intent, cx);
            return;
        }
        let options = MessageDialog::new()
            .parent(self.window.as_ref().expect("native document window"))
            .title("Save changes?")
            .description(format!(
                "Save changes to ‘{}’ before closing?",
                self.path.display()
            ))
            .level(MessageLevel::Warning)
            .buttons(MessageButtons::YesNoCancel);
        match cx.show_message(options, move |this, result, cx| {
            this.dialog = None;
            match result {
                Ok(MessageResponse::Yes) => this.save(Some(intent), cx),
                Ok(MessageResponse::No) => this.finish(intent, cx),
                Ok(_) => {}
                Err(error) => this.status = Some(format!("Cannot show save dialog: {error}")),
            }
        }) {
            Ok(task) => self.dialog = Some(task),
            Err(error) => self.status = Some(format!("Cannot show save dialog: {error}")),
        }
    }
    fn finish(&mut self, intent: Intent, cx: &mut Context<'_, Self>) {
        if let Some(state) = &self.semantic {
            state.service.shutdown();
        }
        let result = match intent {
            Intent::Close => cx.close_window(self.window.as_ref().expect("native document window")),
            Intent::Quit => cx.exit(),
        };
        if let Err(error) = result {
            self.status = Some(error.to_string());
        }
    }
}
impl View for Document {
    fn view(&self, cx: &mut ViewContext<'_, Self>) -> impl IntoElement {
        let save = cx
            .command(Save, |this, _, cx| this.save(None, cx))
            .label("Save")
            .enabled(!self.busy() && self.dirty)
            .shortcut(Shortcut::primary("s"));
        let hover = cx
            .command(Hover, |this, _, cx| this.request_hover(cx))
            .label("Show hover")
            .enabled(self.semantic.as_ref().is_some_and(
                |state| matches!(state.status, velk_semantic::Status::Ready(c) if c.hover),
            ))
            .shortcut(Shortcut::new(
                KeyboardKey::Other("F1".into()),
                Default::default(),
            ))
            // The pinned native host preserves Winit's debug spelling for
            // otherwise-unmapped named keys. Custom hosts may use plain F1.
            .shortcut(Shortcut::new(
                KeyboardKey::Other("Named(F1)".into()),
                Default::default(),
            ));
        let toggle_plugins = cx
            .command(TogglePlugins, |this, _, cx| {
                this.set_plugin_menu(!this.plugin_menu_open, cx);
            })
            .label("Plugins (F2)")
            .shortcut(Shortcut::new(
                KeyboardKey::Other("F2".into()),
                Default::default(),
            ))
            .shortcut(Shortcut::new(
                KeyboardKey::Other("Named(F2)".into()),
                Default::default(),
            ));
        let close_plugins = cx
            .command(ClosePlugins, |this, _, cx| this.set_plugin_menu(false, cx))
            .label("Close plugins")
            .enabled(self.plugin_menu_open)
            .shortcut(Shortcut::new(KeyboardKey::Escape, Default::default()));
        let plugin_menu = self.plugin_menu(cx, toggle_plugins.clone(), close_plugins.clone());
        let toggle_settings = cx
            .command(ToggleSettings, |this, _, cx| {
                this.set_settings(!this.shell.open, cx);
            })
            .label("Settings")
            .shortcut(Shortcut::primary(","));
        let close_settings = cx
            .command(CloseSettings, |this, _, cx| this.set_settings(false, cx))
            .enabled(self.shell.open)
            .shortcut(Shortcut::new(KeyboardKey::Escape, Default::default()));
        let settings_menu = self.settings_menu(cx, toggle_settings.clone(), close_settings.clone());
        let status = self.status.as_deref().unwrap_or(if self.dirty {
            "Unsaved changes"
        } else {
            "Saved"
        });
        let status = match self.semantic_status() {
            Some(semantic) => format!("{status} · {semantic}"),
            None => status.to_owned(),
        };
        let activity = self.activity_bar(toggle_settings.clone(), toggle_plugins.clone());
        let status_bar = self.status_bar(status, toggle_settings.clone(), toggle_plugins.clone());
        column()
            .fill_width()
            .fill_height()
            .on_command(save)
            .on_command(hover)
            .on_command(toggle_plugins)
            .on_command(toggle_settings)
            .children(plugin_menu)
            .children(settings_menu)
            .child(
                row()
                    .fill_width()
                    .fill_height()
                    .flex_shrink(1.)
                    .min_height(0.)
                    .children(activity)
                    .child(
                        self.editor
                            .clone()
                            .into_element()
                            .focus_handle(self.editor_focus.clone())
                            .fill_width()
                            .fill_height()
                            .flex_shrink(1.)
                            .min_width(0.)
                            .min_height(0.),
                    ),
            )
            .children(
                (!self.shell.store.preferences.status_bar)
                    .then_some(self.status.as_ref())
                    .flatten()
                    .map(|message| {
                        label(message.clone())
                            .key("document-notice")
                            .font_size(12.)
                            .padding(6.)
                            .color(ThemeColor::TextMuted)
                    }),
            )
            .children(status_bar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxui::{BackgroundFuture, BlockingJob, Runtime, SpawnError, TaskExecutor};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct QueuedExecutor(Mutex<Vec<BlockingJob>>);
    impl TaskExecutor for QueuedExecutor {
        fn spawn(&self, _: BackgroundFuture) -> Result<(), SpawnError> {
            Err(SpawnError::Unavailable)
        }
        fn spawn_blocking(&self, job: BlockingJob) -> Result<(), SpawnError> {
            self.0.lock().unwrap().push(job);
            Ok(())
        }
    }
    impl QueuedExecutor {
        fn complete(&self, runtime: &mut Runtime) {
            let jobs = std::mem::take(&mut *self.0.lock().unwrap());
            std::thread::spawn(move || {
                for job in jobs {
                    job();
                }
            })
            .join()
            .unwrap();
            runtime.poll_tasks();
            runtime.flush().unwrap();
        }
    }
    #[test]
    fn async_save_persists_requested_snapshot_keeps_newer_edits_dirty_and_serializes_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (file, editor) = FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let editor = runtime.update(|cx| cx.new(|_| editor));
        let document =
            runtime.update(|cx| cx.new(|cx| Document::new(editor.clone(), file, path.clone(), cx)));
        runtime.update(|cx| editor.update(cx, |e, _| e.insert("one")));
        runtime.flush().unwrap();
        runtime.update(|cx| {
            assert!(document.read(cx).dirty);
            document.update(cx, |d, cx| d.save(None, cx));
            editor.update(cx, |e, _| e.insert("two"));
            // A second save must not race the first one's rename/baseline.
            document.update(cx, |d, cx| d.save(None, cx));
            assert!(document.read(cx).saving.is_some());
        });
        assert_eq!(executor.0.lock().unwrap().len(), 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        executor.complete(&mut runtime);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "oneoriginal");
        runtime.update(|cx| {
            assert!(editor.read(cx).buffer().is_dirty());
            assert!(document.read(cx).dirty);
            assert!(document.read(cx).saving.is_none());
            assert!(document.read(cx).status.is_none());
            assert_eq!(
                editor.read(cx).buffer().text().to_string(),
                "onetwooriginal"
            );
            document.update(cx, |d, cx| d.save(None, cx));
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            assert!(!editor.read(cx).buffer().is_dirty());
            assert!(!document.read(cx).dirty);
            assert!(editor.read(cx).buffer().can_undo());
            // A failed subsequent save retains the last successful checkpoint.
            editor.update(cx, |e, _| e.insert("three"));
            std::fs::write(&path, "external").unwrap();
            document.update(cx, |d, cx| d.save(None, cx));
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            assert!(editor.read(cx).buffer().is_dirty());
            assert!(
                document
                    .read(cx)
                    .status
                    .as_ref()
                    .unwrap()
                    .contains("changed on disk")
            );
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external");
    }
    #[test]
    fn document_layout_reserves_status_height_and_keeps_editor_visible_on_resize() {
        struct Measure;
        impl rxui::TextMeasure for Measure {
            fn measure(
                &mut self,
                _: rxui::ElementId,
                request: rxui::TextRequest<'_>,
            ) -> Result<[f32; 2], rxui::UiError> {
                Ok([request.text.len() as f32 * 7., 18.])
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (file, editor) = FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let document = runtime.update(|cx| {
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, path, cx))
        });
        let mut ui = rxui::Ui::new(&mut runtime, document).unwrap();
        for size in [[640., 400.], [320., 160.]] {
            ui.prepare(&mut runtime, size, &mut Measure).unwrap();
            let bounds = ui
                .elements()
                .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
                .unwrap()
                .bounds;
            assert_eq!(bounds.width, size[0]);
            assert!(
                bounds.height >= size[1] - 40.,
                "editor unexpectedly collapsed: {bounds:?}"
            );
            assert!(
                bounds.height <= size[1] - 18.,
                "editor obscures status: {bounds:?}"
            );
        }
    }

    #[test]
    fn failed_task_submission_keeps_edits_and_reports_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (file, mut editor) = FileState::open(&path).unwrap();
        editor.insert("x");
        let mut runtime = Runtime::new();
        runtime.update(|cx| {
            let editor = cx.new(|_| editor);
            let document = cx.new(|cx| Document::new(editor, file, path.clone(), cx));
            document.update(cx, |d, cx| d.save(None, cx));
            assert!(
                document
                    .read(cx)
                    .status
                    .as_ref()
                    .unwrap()
                    .contains("not configured")
            );
            assert!(document.read(cx).saving.is_none());
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    }
}
