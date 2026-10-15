mod buffers;
mod diagnostics;
mod grid;
mod palette;
mod panels;
mod plugins;
pub(crate) mod reload;
mod search;
mod semantic;
mod shell;
use crate::file::FileState;
use rxui::prelude::*;
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
    Buffer,
}

pub struct Document {
    buffer: buffers::BufferState,
    inactive: std::collections::BTreeMap<u64, buffers::BufferState>,
    next_buffer: u64,
    buffer_order: Vec<u64>,
    background_buffer: bool,
    next_search: u64,
    opening: Option<Task>,
    pending_opens: std::collections::VecDeque<PathBuf>,
    closing: Option<Intent>,
    close_approved: std::collections::BTreeSet<u64>,
    pub window: Option<WindowHandle>,
    reload: Option<Task>,
    diagnostics: Option<Task>,
    restore_focus_pending: bool,
    restore_focus_action: std::cell::RefCell<Option<rxui::CommandAction<reload::RestoreFocus>>>,
    dialog: Option<DialogTask>,
    plugins: Option<plugins::State>,
    plugin_menu_open: bool,
    editor_focus: rxui::FocusHandle,
    shell: shell::State,
    palette: palette::State,
    panels: panels::State,
}
impl Document {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn menus() -> rxui::NativeMenuBar {
        use rxui::{NativeMenu, NativeMenuBar, standard_commands as commands};
        NativeMenuBar::new()
            .menu(NativeMenu::new("Velk").command::<commands::Quit>("Quit Velk"))
            .menu(
                NativeMenu::new("File")
                    .command::<buffers::Open>("Open…")
                    .command::<Save>("Save")
                    .separator()
                    .command::<buffers::Close>("Close buffer")
                    .command::<commands::CloseWindow>("Close window"),
            )
            .menu(
                NativeMenu::new("Edit")
                    .command::<commands::Undo>("Undo")
                    .command::<commands::Redo>("Redo")
                    .separator()
                    .command::<commands::Cut>("Cut")
                    .command::<commands::Copy>("Copy")
                    .command::<commands::Paste>("Paste")
                    .command::<commands::SelectAll>("Select all"),
            )
    }
    pub fn new(
        editor: Entity<Editor>,
        file: FileState,
        path: PathBuf,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let panel_source = velk_panels::NativePath::new(file.path());
        let buffer = buffers::BufferState::new(1, editor, file, path, cx);
        Self {
            buffer,
            inactive: Default::default(),
            next_buffer: 2,
            buffer_order: vec![1],
            background_buffer: false,
            next_search: 0,
            opening: None,
            pending_opens: Default::default(),
            closing: None,
            close_approved: Default::default(),
            window: None,
            reload: None,
            diagnostics: None,
            restore_focus_pending: false,
            restore_focus_action: Default::default(),
            dialog: None,
            plugins: None,
            plugin_menu_open: false,
            editor_focus: rxui::FocusHandle::new(),
            shell: shell::State::default(),
            palette: palette::State::default(),
            panels: panels::State::new(panel_source),
        }
    }
    pub fn title(&self) {
        if self.background_buffer {
            return;
        }
        if let Some(window) = self.window.as_ref().and_then(WindowHandle::native_window) {
            window.set_title(&format!(
                "Velk — {}{}",
                self.buffer.path.display(),
                if self.buffer.dirty { " *" } else { "" }
            ));
        }
    }
    fn busy(&self) -> bool {
        self.buffer.saving.is_some() || self.dialog.is_some() || self.reloading()
    }
    #[tracing::instrument(level = "debug", skip_all)]
    fn save(&mut self, then: Option<Intent>, cx: &mut Context<'_, Self>) {
        if self.busy() {
            return;
        }
        let (text, checkpoint) = {
            let editor = self.buffer.editor.read(cx);
            (editor.buffer().text().clone(), editor.buffer().checkpoint())
        };
        let buffer_id = self.buffer.id;
        let saved_snapshot = self
            .buffer
            .semantic
            .as_ref()
            .map(|_| self.semantic_snapshot(cx));
        let file = self.buffer.file.clone();
        let save_trace = tracing::debug_span!(
            "save.transaction",
            async_work = true,
            revision = self.buffer.editor.read(cx).buffer().revision()
        );
        let worker_trace = save_trace.clone();
        let queued = (!worker_trace.is_disabled()).then(std::time::Instant::now);
        self.buffer.status = Some("Saving…".into());
        match cx.try_spawn_blocking(
            move || {
                worker_trace.in_scope(|| {
                    if let Some(start) = queued {
                        tracing::debug!(
                            queue_wait_ms = start.elapsed().as_secs_f64() * 1000.,
                            "save dequeued"
                        );
                    }
                    file.save(text)
                })
            },
            move |this, result, cx| {
                this.with_buffer(buffer_id, cx, |this, cx| {
                    if then.is_some() {
                        this.closing = None;
                    }
                    let _entered = save_trace.enter();
                    this.buffer.saving = None;
                    match result {
                        Ok(Ok(file)) => {
                            this.buffer.file = file;
                            if let (Some(state), Some(snapshot)) =
                                (&this.buffer.semantic, saved_snapshot)
                            {
                                state.service.saved(snapshot);
                            }
                            this.buffer.editor.update(cx, |editor, _| {
                                editor.mark_saved(checkpoint);
                            });
                            this.buffer.dirty = this.buffer.editor.read(cx).buffer().is_dirty();
                            this.title();
                            this.buffer.status = None;
                            if let Some(intent) = then {
                                if this.buffer.dirty {
                                    this.buffer.status = Some(
                                        "Saved the snapshot; newer edits are still unsaved.".into(),
                                    );
                                } else {
                                    this.closing = Some(intent);
                                    this.finish(intent, cx);
                                }
                            }
                        }
                        Ok(Err(error)) => {
                            this.buffer.status = Some(format!("Save failed: {error}"))
                        }
                        Err(error) => this.buffer.status = Some(format!("Save failed: {error}")),
                    }
                });
                this.title();
            },
        ) {
            Ok(task) => self.buffer.saving = Some(task),
            Err(error) => {
                self.closing = None;
                self.buffer.status = Some(format!("Save failed: {error}"));
            }
        }
    }
    pub fn intent(&mut self, intent: Intent, cx: &mut Context<'_, Self>) {
        if self.buffer.editor.read(cx).has_composition() {
            self.buffer.status = Some("Finish text composition before closing.".into());
            return;
        }
        if self.busy() {
            if self.buffer.saving.is_some() {
                self.buffer.status = Some("Saving… Please wait before closing.".into());
            }
            return;
        }
        if self.any_saving() || self.opening.is_some() {
            self.buffer.status =
                Some("Please wait for file operations to finish before closing.".into());
            return;
        }
        if self.closing.is_none() {
            self.close_approved.clear();
        }
        self.closing = Some(intent);
        if !matches!(intent, Intent::Buffer)
            && !self.buffer.editor.read(cx).buffer().is_dirty()
            && let Some(id) = self
                .inactive
                .values()
                .find(|b| b.editor.read(cx).buffer().is_dirty())
                .map(|b| b.id)
        {
            let mut buffer = self.inactive.remove(&id).unwrap();
            std::mem::swap(&mut self.buffer, &mut buffer);
            self.inactive.insert(buffer.id, buffer);
            self.title();
        }
        // Read the buffer directly: its observer may not yet have run.
        if !self.buffer.editor.read(cx).buffer().is_dirty() {
            self.finish(intent, cx);
            return;
        }
        let options = MessageDialog::new()
            .parent(self.window.as_ref().expect("native document window"))
            .title("Save changes?")
            .description(format!(
                "Save changes to ‘{}’ before closing?",
                self.buffer.path.display()
            ))
            .level(MessageLevel::Warning)
            .buttons(MessageButtons::YesNoCancel);
        match cx.show_message(options, move |this, result, cx| {
            this.dialog = None;
            match result {
                Ok(MessageResponse::Yes) => this.save(Some(intent), cx),
                Ok(MessageResponse::No) => this.finish(intent, cx),
                Ok(_) => {
                    this.closing = None;
                }
                Err(error) => {
                    this.closing = None;
                    this.buffer.status = Some(format!("Cannot show save dialog: {error}"));
                }
            }
        }) {
            Ok(task) => self.dialog = Some(task),
            Err(error) => {
                self.closing = None;
                self.buffer.status = Some(format!("Cannot show save dialog: {error}"));
            }
        }
    }
    fn finish(&mut self, intent: Intent, cx: &mut Context<'_, Self>) {
        if matches!(intent, Intent::Buffer) {
            if self.inactive.is_empty() {
                self.finish(Intent::Close, cx);
            } else {
                self.closing = None;
                self.remove_current_buffer(cx);
            }
            return;
        }
        // Retain every buffer until all prompts succeed. Cancelling a later
        // prompt must not lose an earlier buffer whose discard was approved.
        self.close_approved.insert(self.buffer.id);
        self.closing = Some(intent);
        if let Some(id) = self
            .inactive
            .values()
            .find(|b| !self.close_approved.contains(&b.id) && b.editor.read(cx).buffer().is_dirty())
            .map(|b| b.id)
        {
            let mut next = self.inactive.remove(&id).unwrap();
            std::mem::swap(&mut self.buffer, &mut next);
            self.inactive.insert(next.id, next);
            self.title();
            self.intent(intent, cx);
            return;
        }
        self.closing = None;
        let result = match intent {
            Intent::Close => cx.close_window(self.window.as_ref().expect("native document window")),
            Intent::Quit => cx.exit(),
            Intent::Buffer => unreachable!(),
        };
        if let Err(error) = result {
            self.buffer.status = Some(error.to_string());
        }
    }
}
impl View for Document {
    fn view(&self, cx: &mut ViewContext<'_, Self>) -> impl IntoElement {
        if self.restore_focus_pending {
            *self.restore_focus_action.borrow_mut() =
                Some(cx.command(reload::RestoreFocus, |this, _, cx| this.restore_focus(cx)));
            cx.request_animation_frame();
        }
        let open = cx
            .command(buffers::Open, |this, _, cx| this.open_file(cx))
            .label("Open file…")
            .enabled(!self.busy() && self.opening.is_none() && self.closing.is_none())
            .shortcut(Shortcut::primary("o"));
        let close_buffer = cx
            .command(buffers::Close, |this, _, cx| {
                this.intent(Intent::Buffer, cx)
            })
            .label("Close buffer")
            .enabled(
                !self.busy()
                    && !self.any_saving()
                    && self.opening.is_none()
                    && self.closing.is_none(),
            )
            .shortcut(Shortcut::primary("w"));
        let next_buffer = cx
            .command(buffers::Next, |this, _, cx| this.cycle_buffer(false, cx))
            .label("Next buffer")
            .shortcut(Shortcut::new(
                KeyboardKey::Tab,
                rxui::Modifiers {
                    control: true,
                    ..Default::default()
                },
            ));
        let previous_buffer = cx
            .command(buffers::Previous, |this, _, cx| this.cycle_buffer(true, cx))
            .label("Previous buffer")
            .shortcut(Shortcut::new(
                KeyboardKey::Tab,
                rxui::Modifiers {
                    control: true,
                    shift: true,
                    ..Default::default()
                },
            ));
        let quit = cx
            .command(rxui::standard_commands::Quit, |this, _, cx| {
                this.intent(Intent::Quit, cx)
            })
            .label("Quit Velk")
            .shortcut(Shortcut::primary("q"))
            .allow_in_modal(true);
        let close_window = cx
            .command(rxui::standard_commands::CloseWindow, |this, _, cx| {
                this.intent(Intent::Close, cx)
            })
            .label("Close window")
            .shortcut(Shortcut::primary("w").shift());
        let save = cx
            .command(Save, |this, _, cx| this.save(None, cx))
            .label("Save")
            .enabled(!self.busy() && self.buffer.dirty)
            .shortcut(Shortcut::primary("s"));
        let hover = cx
            .command(Hover, |this, _, cx| this.request_hover(cx))
            .label("Show hover")
            .enabled(self.buffer.semantic.as_ref().is_some_and(
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
        let (load_plugins, unload_plugins) = self.plugin_actions(cx);
        let plugin_menu = self.plugin_menu(
            cx,
            toggle_plugins.clone(),
            close_plugins.clone(),
            load_plugins.clone(),
            unload_plugins.clone(),
        );
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
        let status = self
            .buffer
            .status
            .as_deref()
            .unwrap_or(if self.buffer.dirty {
                "Unsaved changes"
            } else {
                "Saved"
            });
        let status = match self.semantic_status() {
            Some(semantic) => format!("{status} · {semantic}"),
            None => status.to_owned(),
        };
        let toggle_panels = self.panels_action(cx);
        let toggle_terminal = cx
            .command(panels::ToggleBottom, |this, _, cx| this.toggle_bottom(cx))
            .label("Terminal: Toggle bottom panel")
            .enabled(self.bottom_available() && !self.reloading())
            .shortcut(Shortcut::new(
                KeyboardKey::Character("`".into()),
                rxui::Modifiers {
                    control: true,
                    ..Default::default()
                },
            ));
        let refresh_panels = cx
            .command(panels::Refresh, |this, _, cx| this.refresh_panels(cx))
            .label("Panels: Refresh")
            .enabled(!self.plugins_busy() && !self.reloading() && !self.panels_refresh_busy());
        let open_folder = cx
            .command(panels::OpenFolder, |this, _, cx| this.choose_folder(cx))
            .label("Open folder…")
            .enabled(!self.busy() && !self.reloading())
            .shortcut(Shortcut::primary("o").shift());
        let activity = self.activity_bar(
            toggle_settings.clone(),
            toggle_plugins.clone(),
            toggle_panels.clone(),
        );
        let (find, search_bar) = self.search_bar(cx);
        let status_bar = self.status_bar(
            status,
            toggle_settings.clone(),
            toggle_plugins.clone(),
            toggle_panels.clone(),
        );
        let restart_plugins = cx
            .command(reload::RestartPlugins, |this, _, cx| {
                this.plugin_operation(plugins::Operation::Restart(None), cx)
            })
            .label("Plugins: Restart active plugins")
            .enabled(!self.plugins_busy() && !self.reloading());
        let reload_window = cx
            .command(reload::ReloadWindow, |this, _, cx| this.reload_window(cx))
            .label("Reload window")
            .enabled(self.can_reload(cx))
            .allow_in_modal(true)
            .shortcut(Shortcut::primary("r").alt());
        let trace_available =
            velk_diagnostics::available() && self.diagnostics.is_none() && !self.reloading();
        let trace_start = cx
            .command(diagnostics::Start, |this, _, cx| {
                this.trace_capture(true, cx)
            })
            .label("Diagnostics: Start trace capture")
            .enabled(trace_available && !velk_diagnostics::recording());
        let trace_stop = cx
            .command(diagnostics::Stop, |this, _, cx| {
                this.trace_capture(false, cx)
            })
            .label("Diagnostics: Stop trace capture")
            .enabled(trace_available && velk_diagnostics::status().is_some());
        let trace_marker = cx
            .command(diagnostics::Marker, |this, _, _| {
                velk_diagnostics::marker("User marker");
                this.buffer.status = Some("Trace marker added".into());
            })
            .label("Diagnostics: Add trace marker")
            .enabled(trace_available && velk_diagnostics::recording());
        let (toggle_palette, palette) = self.command_palette(
            cx,
            if self.palette.open {
                vec![
                    palette::Entry::new("velk.file.save", save.clone(), palette::primary("S")),
                    palette::Entry::new("velk.file.open", open.clone(), palette::primary("O")),
                    palette::Entry::new(
                        "velk.buffer.close",
                        close_buffer.clone(),
                        palette::primary("W"),
                    ),
                    palette::Entry::new("velk.buffer.next", next_buffer.clone(), "Ctrl+Tab"),
                    palette::Entry::new(
                        "velk.buffer.previous",
                        previous_buffer.clone(),
                        "Ctrl+Shift+Tab",
                    ),
                    palette::Entry::new("velk.search.find", find.clone(), palette::primary("F")),
                    palette::Entry::new(
                        "velk.settings.open",
                        toggle_settings.clone(),
                        palette::primary(","),
                    ),
                    palette::Entry::new(
                        "velk.plugins.open",
                        toggle_plugins.clone().label("Plugins"),
                        "F2",
                    ),
                    palette::Entry::new("velk.editor.hover", hover.clone(), "F1"),
                    palette::Entry::new(
                        "velk.plugins.load",
                        load_plugins.clone().label("Plugins: Load from disk"),
                        "",
                    ),
                    palette::Entry::new(
                        "velk.plugins.unload-all",
                        unload_plugins.clone().label("Plugins: Unload all"),
                        "",
                    ),
                    palette::Entry::new("velk.window.close", close_window.clone(), ""),
                    palette::Entry::new("velk.app.quit", quit.clone(), palette::primary("Q")),
                    palette::Entry::new("velk.plugins.restart", restart_plugins.clone(), ""),
                    palette::Entry::new(
                        "velk.window.reload",
                        reload_window.clone(),
                        if cfg!(target_os = "macos") {
                            "⌘⌥R"
                        } else {
                            "Ctrl+Alt+R"
                        },
                    ),
                    palette::Entry::new(
                        "velk.panels.toggle",
                        toggle_panels.clone(),
                        palette::primary("B"),
                    ),
                    palette::Entry::new(
                        "velk.folder.open",
                        open_folder.clone(),
                        palette::primary("Shift+O"),
                    ),
                    palette::Entry::new("velk.panels.refresh", refresh_panels.clone(), ""),
                    palette::Entry::new("velk.terminal.toggle", toggle_terminal.clone(), "Ctrl+`"),
                    palette::Entry::new("velk.diagnostics.start", trace_start.clone(), ""),
                    palette::Entry::new("velk.diagnostics.stop", trace_stop.clone(), ""),
                    palette::Entry::new("velk.diagnostics.marker", trace_marker.clone(), ""),
                ]
            } else {
                Vec::new()
            },
        );
        column()
            .fill_width()
            .fill_height()
            .on_command(toggle_panels)
            .on_command(toggle_terminal)
            .on_command(trace_start)
            .on_command(trace_stop)
            .on_command(trace_marker)
            .on_command(restart_plugins)
            .on_command(reload_window)
            .on_command(toggle_palette)
            .on_command(load_plugins)
            .on_command(unload_plugins)
            .on_command(quit)
            .on_command(close_window)
            .on_command(open.clone())
            .on_command(open_folder)
            .on_command(refresh_panels)
            .on_command(close_buffer.clone())
            .on_command(next_buffer)
            .on_command(previous_buffer)
            .on_command(save)
            .on_command(hover)
            .on_command(toggle_plugins)
            .on_command(toggle_settings)
            .on_command(find)
            .children(palette)
            .children(plugin_menu)
            .children(settings_menu)
            .children(self.reloading().then(|| {
                modal(label("Reloading window…").padding(20.))
                    .key("reload-progress")
                    .width(320.)
                    .accessibility_label("Reloading window")
                    .into_element()
            }))
            .child(self.buffer_bar(cx, open.clone(), close_buffer.clone()))
            .child(search_bar)
            .child(
                row()
                    .fill_width()
                    .fill_height()
                    .flex_shrink(1.)
                    .min_height(0.)
                    .children(activity)
                    .children(self.panel_slot(velk_panels::Slot::Left, cx))
                    .child(
                        self.buffer
                            .editor
                            .clone()
                            .into_element()
                            .focus_handle(self.editor_focus.clone())
                            .fill_width()
                            .fill_height()
                            .flex_shrink(1.)
                            .min_width(0.)
                            .min_height(0.),
                    )
                    .children(self.panel_slot(velk_panels::Slot::Right, cx)),
            )
            .children(self.panel_slot(velk_panels::Slot::Bottom, cx))
            .children(
                (!self.shell.store.preferences.status_bar)
                    .then_some(self.buffer.status.as_ref())
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
    pub(super) struct QueuedExecutor(Mutex<Vec<BlockingJob>>);
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
        pub(super) fn complete(&self, runtime: &mut Runtime) {
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
            assert!(document.read(cx).buffer.dirty);
            document.update(cx, |d, cx| d.save(None, cx));
            editor.update(cx, |e, _| e.insert("two"));
            // A second save must not race the first one's rename/baseline.
            document.update(cx, |d, cx| d.save(None, cx));
            assert!(document.read(cx).buffer.saving.is_some());
        });
        assert_eq!(executor.0.lock().unwrap().len(), 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        executor.complete(&mut runtime);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "oneoriginal");
        runtime.update(|cx| {
            assert!(editor.read(cx).buffer().is_dirty());
            assert!(document.read(cx).buffer.dirty);
            assert!(document.read(cx).buffer.saving.is_none());
            assert!(document.read(cx).buffer.status.is_none());
            assert_eq!(
                editor.read(cx).buffer().text().to_string(),
                "onetwooriginal"
            );
            document.update(cx, |d, cx| d.save(None, cx));
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            assert!(!editor.read(cx).buffer().is_dirty());
            assert!(!document.read(cx).buffer.dirty);
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
                    .buffer
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
                bounds.height >= size[1] - 74.,
                "editor unexpectedly collapsed: {bounds:?}"
            );
            assert!(
                bounds.height <= size[1] - 52.,
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
                    .buffer
                    .status
                    .as_ref()
                    .unwrap()
                    .contains("not configured")
            );
            assert!(document.read(cx).buffer.saving.is_none());
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    }
}
