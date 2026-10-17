mod buffer_events;
mod buffers;
mod closing;
mod collections;
mod diagnostics;
mod file_operations;
mod grid;
mod host_services;
mod icons;
mod keybindings;
mod maintenance;
mod navigation;
mod palette;
mod panels;
mod picker;
mod plugin_commands;
mod plugins;
mod plugins_page;
pub(crate) mod reload;
mod saving;
mod search;
mod semantic;
mod settings_page;
mod shell;
mod views;
mod workspace_edits;
use crate::file::FileState;
use crate::settings;
use rxui::prelude::*;
use std::path::PathBuf;
use velk_editor_widget::Editor;

struct Save;
impl Command for Save {}
struct SaveAs;
impl Command for SaveAs {}
struct SaveAll;
impl Command for SaveAll {}
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
pub use velk_core::lifecycle::CloseIntent as Intent;

pub struct Document {
    buffer: buffers::ActiveBuffer,
    views: views::State,
    window_status: Option<String>,
    inactive: std::collections::BTreeMap<u64, buffers::BufferState>,
    documents: velk_editor_model::workspace::Documents,
    workspace: crate::workspace::Workspace,
    background_buffer: bool,
    next_search: u64,
    lifecycle: velk_core::lifecycle::Lifecycle,
    overwrite_approved: bool,
    saving_as: bool,
    opening: Option<Task>,
    pending_opens: std::collections::VecDeque<buffers::OpenRequest>,
    command_task: Option<Task>,
    next_picker: u64,
    collection: Option<collections::State>,
    next_collection: u64,
    automatic_completion: Option<Task>,
    deferred_choice: Option<(velk_commands::ChoiceAction, workspace_edits::Guard)>,
    host_requests: Option<Task>,
    buffer_events: buffer_events::Delivery,
    workspace_edit: Option<Task>,
    file_operation: Option<Task>,
    navigation: velk_editor_model::navigation::History,
    command_result: Option<plugin_commands::ResultView>,
    pub window: Option<WindowHandle>,
    reload: Option<Task>,
    diagnostics: Option<Task>,
    recovery: Option<crate::recovery::Handle>,
    recovery_task: Option<Task>,
    recovery_key: Option<maintenance::RecoveryKey>,
    maintenance: Option<Task>,
    disk_check: Option<Task>,
    keybindings: keybindings::State,
    restore_focus_pending: bool,
    restore_focus_action: std::cell::RefCell<Option<rxui::CommandAction<reload::RestoreFocus>>>,
    dialog: Option<DialogTask>,
    plugins: Option<plugins::State>,
    plugin_menu_open: bool,
    plugin_page: plugins_page::Page,
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
            .menu(
                NativeMenu::new("Velk")
                    .command::<ToggleSettings>("Settings…")
                    .separator()
                    .command::<commands::Quit>("Quit Velk"),
            )
            .menu(
                NativeMenu::new("File")
                    .command::<buffers::New>("New File")
                    .command::<buffers::Open>("Open…")
                    .command::<Save>("Save")
                    .command::<SaveAs>("Save As…")
                    .command::<SaveAll>("Save All")
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
            .menu(
                NativeMenu::new("View")
                    .command::<views::SplitRight>("Split editor right")
                    .command::<views::SplitDown>("Split editor down")
                    .command::<views::CloseView>("Close editor view")
                    .command::<views::NextView>("Focus next editor view")
                    .separator()
                    .command::<panels::Toggle>("Toggle panels")
                    .command::<panels::ToggleBottom>("Toggle terminal")
                    .separator()
                    .submenu(
                        NativeMenu::new("Layout")
                            .command::<shell::LayoutQuiet>("Quiet")
                            .command::<shell::LayoutRail>("Rail")
                            .command::<shell::LayoutTiled>("Tiled"),
                    )
                    .separator()
                    .command::<TogglePlugins>("Plugins")
                    .command::<ToggleSettings>("Settings"),
            )
    }
    #[cfg(test)]
    pub fn new(
        editor: Entity<Editor>,
        file: impl Into<Option<FileState>>,
        path: PathBuf,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        Self::new_with_id(1, editor, file.into(), path, cx)
    }
    pub fn new_with_id(
        id: u64,
        editor: Entity<Editor>,
        file: Option<FileState>,
        path: PathBuf,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let panel_source = file
            .as_ref()
            .map(|f| velk_panels::NativePath::new(f.path()));
        let buffer = buffers::BufferState::new(id, editor, file, path, cx);
        Self::window_state(Some(buffer), id + 1, panel_source)
    }
    pub fn empty(next_id: u64) -> Self {
        Self::window_state(None, next_id.max(1), None)
    }
    fn window_state(
        buffer: Option<buffers::BufferState>,
        next_id: u64,
        panel_source: Option<velk_panels::NativePath>,
    ) -> Self {
        let mut documents = velk_editor_model::workspace::Documents::new(next_id);
        if let Some(buffer) = &buffer {
            documents
                .restore(
                    buffer.id,
                    buffer.file.as_ref().map(|file| file.path().to_path_buf()),
                )
                .expect("initial document has a valid unique identity");
        }
        let editor_focus = rxui::FocusHandle::new();
        let views = views::State::new(buffer.as_ref(), editor_focus.clone());
        Self {
            views,
            buffer: buffer.into(),
            window_status: None,
            inactive: Default::default(),
            documents,
            workspace: Default::default(),
            background_buffer: false,
            next_search: 0,
            lifecycle: Default::default(),
            overwrite_approved: false,
            saving_as: false,
            opening: None,
            pending_opens: Default::default(),
            command_task: None,
            next_picker: 1,
            collection: None,
            next_collection: 1,
            automatic_completion: None,
            deferred_choice: None,
            host_requests: None,
            buffer_events: Default::default(),
            workspace_edit: None,
            file_operation: None,
            navigation: Default::default(),
            command_result: None,
            window: None,
            reload: None,
            diagnostics: None,
            recovery: None,
            recovery_task: None,
            recovery_key: None,
            maintenance: None,
            disk_check: None,
            keybindings: Default::default(),
            restore_focus_pending: false,
            restore_focus_action: Default::default(),
            dialog: None,
            plugins: None,
            plugin_menu_open: false,
            plugin_page: Default::default(),
            editor_focus,
            shell: shell::State::default(),
            palette: palette::State::default(),
            panels: panels::State::new(panel_source),
        }
    }
    pub(super) fn has_buffers(&self) -> bool {
        self.buffer.is_some()
    }
    pub(super) fn notice(&mut self, message: Option<String>) {
        if let Some(buffer) = self.buffer.as_mut() {
            buffer.status = message;
        } else {
            self.window_status = message;
        }
    }
    pub fn title(&self) {
        if self.background_buffer {
            return;
        }
        if let Some(window) = self.window.as_ref().and_then(WindowHandle::native_window) {
            if !self.has_buffers() {
                window.set_title("Velk");
                return;
            }
            window.set_title(&format!(
                "Velk — {}{}",
                self.buffer.path.display(),
                if self.buffer.dirty { " *" } else { "" }
            ));
        }
    }
    fn busy(&self) -> bool {
        self.workspace_edit.is_some()
            || self.file_operation.is_some()
            || self.lifecycle.prompted_document().is_some()
            || self.buffer.as_ref().is_some_and(|b| b.saving.is_some())
            || self.dialog.is_some()
            || self.reloading()
    }
    #[tracing::instrument(level = "debug", skip_all)]
    fn save(&mut self, then: Option<Intent>, cx: &mut Context<'_, Self>) {
        if self.busy() || !self.has_buffers() {
            return;
        }
        if self.buffer.file.is_none() {
            self.save_as(then, cx);
            return;
        }
        self.save_to(None, then, cx);
    }
    fn save_to(
        &mut self,
        destination: Option<PathBuf>,
        then: Option<Intent>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.busy() || !self.has_buffers() {
            return;
        }
        let (text, checkpoint) = {
            let editor = self.buffer.editor.read(cx);
            (editor.buffer().text(), editor.buffer().checkpoint())
        };
        self.buffer
            .editor
            .update(cx, |editor, _| editor.break_undo_group());
        let undo_snapshot = crate::undo::Store::from_env()
            .map(|store| (store, self.buffer.editor.read(cx).buffer().snapshot()));
        let saved_revision = self.buffer.editor.read(cx).buffer().revision();
        let buffer_id = self.buffer.id;
        let saved_snapshot = self
            .buffer
            .semantic
            .as_ref()
            .map(|_| self.semantic_snapshot(cx));
        let file = self.buffer.file.clone();
        let occupied: Vec<_> = self
            .buffer
            .iter()
            .chain(self.inactive.values())
            .filter(|b| b.id != buffer_id)
            .filter_map(|b| b.file.as_ref().map(|f| f.path().to_path_buf()))
            .collect();
        let changing_path = destination.is_some();
        let overwrite = self.overwrite_approved;
        let save_trace = tracing::debug_span!(
            "save.transaction",
            async_work = true,
            revision = self.buffer.editor.read(cx).buffer().revision()
        );
        let worker_trace = save_trace.clone();
        let queued = (!worker_trace.is_disabled()).then(std::time::Instant::now);
        self.notice(Some("Saving…".into()));
        if changing_path {
            self.saving_as = true;
        }
        match cx.try_spawn_blocking(
            move || {
                worker_trace.in_scope(|| {
                    if let Some(start) = queued {
                        tracing::debug!(
                            queue_wait_ms = start.elapsed().as_secs_f64() * 1000.,
                            "save dequeued"
                        );
                    }
                    let saved = match destination {
                        Some(path) => FileState::save_as(file, path, text, &occupied),
                        None => {
                            let file =
                                file.ok_or_else(|| std::io::Error::other("document has no file"))?;
                            if overwrite {
                                file.overwrite(text)
                            } else {
                                file.save(text)
                            }
                        }
                    }?;
                    if let Some((store, snapshot)) = undo_snapshot
                        && let Err(error) = store.save(saved.path(), snapshot)
                    {
                        tracing::warn!(%error, "file saved but undo archive could not be written");
                    }
                    Ok::<_, std::io::Error>(saved)
                })
            },
            move |this, result, cx| {
                if changing_path {
                    this.saving_as = false;
                }
                this.with_buffer(buffer_id, cx, |this, cx| {
                    let _entered = save_trace.enter();
                    this.buffer.saving = None;
                    match result {
                        Ok(Ok(file)) => {
                            this.documents
                                .rename(buffer_id, Some(file.path().to_path_buf()))
                                .expect("save destination was checked against open documents");
                            this.buffer.path = file.path().to_path_buf();
                            this.buffer.file = Some(file);
                            if changing_path {
                                if let Some(state) = this.buffer.semantic.take() {
                                    state.service.shutdown();
                                }
                                this.buffer.refresh_view_languages(cx);
                                this.start_buffer_semantics(cx);
                            }
                            if let (Some(state), Some(snapshot)) = (
                                &this.buffer.semantic,
                                saved_snapshot.filter(|_| !changing_path),
                            ) {
                                state.service.saved(snapshot);
                            }
                            this.buffer.editor.update(cx, |editor, _| {
                                editor.mark_saved(checkpoint);
                            });
                            let dirty = this.buffer.editor.read(cx).buffer().is_dirty();
                            this.buffer.dirty = dirty;
                            // A saved checkpoint can change even when the on-disk
                            // bytes and current document revision stay identical.
                            this.recovery_key = None;
                            this.title();
                            this.buffer.status = None;
                            this.buffer.disk_changed = false;
                            this.buffer.disk_stamp = None;
                            let buffer = this.buffer_info(&this.buffer, cx);
                            this.publish_buffer_event(
                                velk_plugin_contracts::buffers::Event::Saved {
                                    buffer,
                                    saved_revision,
                                },
                                cx,
                            );
                            if let Some(intent) = then {
                                let documents = this.lifecycle_documents(cx);
                                let action = this.lifecycle.saved(intent, &documents);
                                this.close_action(action, cx);
                            }
                        }
                        Ok(Err(error)) => {
                            this.lifecycle.cancel_close();
                            this.notice(Some(format!("Save failed: {error}")));
                        }
                        Err(error) => {
                            this.lifecycle.cancel_close();
                            this.notice(Some(format!("Save failed: {error}")));
                        }
                    }
                });
                this.title();
                if changing_path && this.panels.ui.source.is_none() {
                    let file = if this.buffer.as_ref().is_some_and(|b| b.id == buffer_id) {
                        this.buffer.file.as_ref()
                    } else {
                        this.inactive.get(&buffer_id).and_then(|b| b.file.as_ref())
                    };
                    if let Some(file) = file {
                        this.panels.ui.source = Some(velk_panels::NativePath::new(file.path()));
                        this.refresh_panels(cx);
                    }
                }
                this.schedule_recovery(cx);
                if let Some(error) = this.buffer_save_error(buffer_id) {
                    this.lifecycle.cancel_save_all();
                    if this.buffer.as_ref().is_none_or(|b| b.id != buffer_id) {
                        this.notice(Some(error));
                    }
                } else {
                    this.continue_save_all(cx);
                }
            },
        ) {
            Ok(task) => self.buffer.saving = Some(task),
            Err(error) => {
                if changing_path {
                    self.saving_as = false;
                }
                self.lifecycle.cancel_close();
                self.lifecycle.cancel_save_all();
                self.notice(Some(format!("Save failed: {error}")));
            }
        }
    }
    fn lifecycle_documents(
        &self,
        cx: &impl rxui::ReadContext,
    ) -> Vec<velk_core::lifecycle::DocumentState> {
        self.buffer
            .iter()
            .chain(self.inactive.values())
            .map(|b| {
                let editor = b.editor.read(cx);
                velk_core::lifecycle::DocumentState {
                    id: b.id,
                    dirty: editor.buffer().is_dirty(),
                    composing: b
                        .views
                        .iter()
                        .any(|view| view.editor.read(cx).has_composition()),
                    saving: b.saving.is_some(),
                }
            })
            .collect()
    }

    fn lifecycle_activity(&self) -> velk_core::lifecycle::Activity {
        velk_core::lifecycle::Activity {
            busy: self.busy(),
            opening: self.opening.is_some(),
            saving: self.any_saving(),
        }
    }

    pub fn intent(&mut self, intent: Intent, cx: &mut Context<'_, Self>) {
        let documents = self.lifecycle_documents(cx);
        let activity = self.lifecycle_activity();
        let action = self.lifecycle.request_close(intent, &documents, activity);
        self.close_action(action, cx);
    }

    fn close_action(
        &mut self,
        action: velk_core::lifecycle::CloseAction,
        cx: &mut Context<'_, Self>,
    ) {
        use velk_core::lifecycle::{CloseAction, CloseBlock};
        match action {
            CloseAction::Ignore => {}
            CloseAction::Blocked(reason) => self.notice(Some(
                match reason {
                    CloseBlock::Composition => "Finish text composition before closing.",
                    CloseBlock::Saving => "Saving… Please wait before closing.",
                    CloseBlock::FileOperations => {
                        "Please wait for file operations to finish before closing."
                    }
                    CloseBlock::NewerEdits => "Saved the snapshot; newer edits are still unsaved.",
                }
                .into(),
            )),
            CloseAction::Prompt(id) => {
                if self.buffer.as_ref().is_none_or(|b| b.id != id) {
                    let Some(mut next) = self.inactive.remove(&id) else {
                        self.lifecycle.cancel_close();
                        return;
                    };
                    std::mem::swap(self.buffer.as_mut().unwrap(), &mut next);
                    self.inactive.insert(next.id, next);
                    self.documents.select(id);
                    self.show_active_document(cx);
                    self.title();
                }
            }
            CloseAction::Save { id, then } => {
                self.with_buffer(id, cx, |this, cx| this.save(Some(then), cx))
            }
            CloseAction::Remove(id) => {
                if self.buffer.as_ref().is_some_and(|b| b.id == id) {
                    self.remove_current_buffer(cx);
                }
            }
            CloseAction::Finish(intent) => {
                let result = match intent {
                    Intent::Close => self
                        .window
                        .as_ref()
                        .map_or(Err(rxui::ApplicationError::NoSourceWindow), |window| {
                            cx.close_window(window)
                        }),
                    Intent::Quit => cx.exit(),
                    Intent::Buffer => unreachable!(),
                };
                if let Err(error) = result {
                    self.notice(Some(error.to_string()));
                } else if let Some(recovery) = &self.recovery {
                    recovery.discard();
                }
            }
        }
    }
}
impl View for Document {
    fn view(&self, cx: &mut ViewContext<'_, Self>) -> impl IntoElement {
        let layout = self.layout();
        if self.restore_focus_pending {
            *self.restore_focus_action.borrow_mut() =
                Some(cx.command(reload::RestoreFocus, |this, _, cx| this.restore_focus(cx)));
            cx.request_animation_frame();
        }
        let new_file = cx
            .command(buffers::New, |this, _, cx| this.new_buffer(cx))
            .label("New File")
            .enabled(!self.busy() && self.lifecycle.close_intent().is_none())
            .shortcut(Shortcut::primary("n"));
        let save_as = cx
            .command(SaveAs, |this, _, cx| this.save_as(None, cx))
            .label("Save As…")
            .enabled(self.has_buffers() && !self.busy() && self.lifecycle.close_intent().is_none())
            .shortcut(Shortcut::primary("s").shift());
        let save_all = cx
            .command(SaveAll, |this, _, cx| this.save_all(cx))
            .label("Save All")
            .enabled(
                self.has_buffers()
                    && !self.busy()
                    && !self.any_saving()
                    && self.lifecycle.close_intent().is_none(),
            )
            .shortcut(Shortcut::primary("s").alt());
        let open = cx
            .command(buffers::Open, |this, _, cx| this.open_file(cx))
            .label("Open file…")
            .enabled(
                !self.busy() && self.opening.is_none() && self.lifecycle.close_intent().is_none(),
            )
            .shortcut(Shortcut::primary("o"));
        let close_buffer = cx
            .command(buffers::Close, |this, _, cx| {
                this.intent(Intent::Buffer, cx)
            })
            .label("Close buffer")
            .enabled(
                self.has_buffers()
                    && !self.busy()
                    && !self.any_saving()
                    && self.opening.is_none()
                    && self.lifecycle.close_intent().is_none(),
            )
            .shortcut(Shortcut::primary("w"));
        let next_buffer = cx
            .command(buffers::Next, |this, _, cx| this.cycle_buffer(false, cx))
            .label("Next buffer")
            .enabled(self.documents.len() > 1)
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
            .enabled(self.documents.len() > 1)
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
            .enabled(
                self.has_buffers()
                    && !self.busy()
                    && (self.buffer.dirty || self.buffer.file.is_none()),
            )
            .shortcut(Shortcut::primary("s"));
        let hover = cx
            .command(Hover, |this, _, cx| this.request_hover(cx))
            .label("Show hover")
            .enabled(
                self.has_buffers()
                    && self.buffer.semantic.as_ref().is_some_and(
                        |state| matches!(state.status, velk_semantic::Status::Ready(c) if c.hover),
                    ),
            )
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
            .label("Plugins (F4)")
            .shortcut(Shortcut::new(
                KeyboardKey::Other("F4".into()),
                Default::default(),
            ))
            .shortcut(Shortcut::new(
                KeyboardKey::Other("Named(F4)".into()),
                Default::default(),
            ));
        let close_plugins = cx
            .command(ClosePlugins, |this, _, cx| this.set_plugin_menu(false, cx))
            .label("Close plugins")
            .enabled(self.plugin_menu_open)
            .shortcut(Shortcut::new(KeyboardKey::Escape, Default::default()));
        let (load_plugins, unload_plugins) = self.plugin_actions(cx);
        let plugin_menu = self.plugins_modal(
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
        let settings_menu =
            self.settings_modal(cx, toggle_settings.clone(), close_settings.clone());
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
        let (layout_quiet, layout_rail, layout_tiled) = self.layout_actions(cx);
        let (find, search_bar) = self.search_bar(cx);
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
                this.notice(Some("Trace marker added".into()));
            })
            .label("Diagnostics: Add trace marker")
            .enabled(trace_available && velk_diagnostics::recording());
        let back = cx
            .command(navigation::Back, |this, _, cx| {
                this.navigate_history(false, cx)
            })
            .label("Navigate back")
            .enabled(self.navigation.can_back() && !self.busy())
            .shortcut(Shortcut::new(
                KeyboardKey::ArrowLeft,
                rxui::Modifiers {
                    alt: true,
                    ..Default::default()
                },
            ));
        let forward = cx
            .command(navigation::Forward, |this, _, cx| {
                this.navigate_history(true, cx)
            })
            .label("Navigate forward")
            .enabled(self.navigation.can_forward() && !self.busy())
            .shortcut(Shortcut::new(
                KeyboardKey::ArrowRight,
                rxui::Modifiers {
                    alt: true,
                    ..Default::default()
                },
            ));
        let mut entries = vec![
            palette::Entry::new("velk.navigation.back", back.clone(), "Alt+Left"),
            palette::Entry::new("velk.navigation.forward", forward.clone(), "Alt+Right"),
            palette::Entry::new("velk.file.save", save.clone(), palette::primary("S")),
            palette::Entry::new("velk.file.new", new_file.clone(), palette::primary("N")),
            palette::Entry::new(
                "velk.file.save-as",
                save_as.clone(),
                palette::primary("Shift+S"),
            ),
            palette::Entry::new(
                "velk.file.save-all",
                save_all.clone(),
                palette::primary("Alt+S"),
            ),
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
                "F4",
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
            palette::Entry::new("velk.layout.quiet", layout_quiet.clone(), ""),
            palette::Entry::new("velk.layout.rail", layout_rail.clone(), ""),
            palette::Entry::new("velk.layout.tiled", layout_tiled.clone(), ""),
        ];
        let (view_entries, view_actions) = self.view_commands(cx);
        entries.extend(view_entries);
        entries.extend(self.plugin_command_entries(cx));
        entries.extend(self.editing_command_entries(cx));
        let (open_bindings, reload_bindings, show_commands) = self.keybinding_actions(cx);
        entries.extend([
            palette::Entry::new("velk.keybindings.open", open_bindings.clone(), ""),
            palette::Entry::new("velk.keybindings.reload", reload_bindings.clone(), ""),
            palette::Entry::new("velk.keybindings.commands", show_commands.clone(), ""),
        ]);
        self.decorate_binding_entries(&mut entries, cx);
        let (toggle_palette, palette) = self.command_palette(cx, entries.clone());
        entries.push(palette::Entry::new(
            "velk.commands.open",
            toggle_palette.clone(),
            palette::primary("Shift+P"),
        ));
        self.keybindings.entries.replace(entries);
        let shortcuts = self.plugin_shortcuts(cx);
        let mut content = column()
            .fill_width()
            .fill_height()
            .on_command(self.window_key_reservation(cx))
            .children(palette)
            .children(self.command_result_modal(cx))
            .children(plugin_menu)
            .children(settings_menu)
            .children(self.close_confirmation(cx))
            .children(self.reloading().then(|| {
                modal(label("Reloading window…").padding(20.))
                    .key("reload-progress")
                    .width(320.)
                    .accessibility_label("Reloading window")
                    .into_element()
            }))
            .children(self.title_bar(layout, cx))
            .children(self.keybinding_notice())
            .child(self.body(layout, search_bar, cx))
            .children(self.status_line(layout, cx));
        content = view_actions(content);
        content = column()
            .fill_width()
            .fill_height()
            .on_command(layout_quiet)
            .on_command(layout_rail)
            .on_command(layout_tiled)
            .on_command(open_bindings)
            .on_command(reload_bindings)
            .on_command(show_commands)
            .on_command(back)
            .on_command(forward)
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
            .on_command(new_file)
            .on_command(save_as)
            .on_command(save_all)
            .on_command(save)
            .on_command(hover)
            .on_command(toggle_plugins)
            .on_command(toggle_settings)
            .on_command(find)
            .child(content);
        // RXUI command identity is a Rust type. Dynamic bindings use separate
        // outer scopes; the inner host scope retains shortcut precedence.
        for action in shortcuts {
            content = column()
                .fill_width()
                .fill_height()
                .on_command(action)
                .child(content);
        }
        content
            .on_key_down_capture(cx.listener(|this, event: &KeyInput, cx| {
                this.bound_key(event, velk_commands::keybindings::Scope::Window, cx)
            }))
            .on_pointer_down_capture(cx.listener(|this, _, _| this.cancel_key_chord()))
    }
}
impl Document {
    /// Rail, sidebar, editor, bottom and right panels, arranged by the layout.
    fn body(
        &self,
        layout: settings::Layout,
        search_bar: rxui::Element,
        cx: &mut ViewContext<'_, Self>,
    ) -> rxui::Element {
        use velk_panels::Slot;
        let content: rxui::Element = if self.has_buffers() {
            self.editor_views(cx)
        } else {
            column()
                .key("empty-workspace")
                .fill_width()
                .fill_height()
                .gap(12.)
                .align_items(rxui::taffy::prelude::AlignItems::CENTER)
                .justify_content(rxui::taffy::prelude::JustifyContent::CENTER)
                .child(
                    label("No documents open")
                        .font_size(20.)
                        .color(ThemeColor::TextMuted),
                )
                .child(
                    row()
                        .gap(10.)
                        .child(
                            button("New File")
                                .key("empty-new")
                                .focus_handle(self.editor_focus.clone())
                                .on_click(cx.listener(|this, _, cx| this.new_buffer(cx))),
                        )
                        .child(
                            button("Open File…")
                                .key("empty-open")
                                .on_click(cx.listener(|this, _, cx| this.open_file(cx))),
                        ),
                )
                .into_element()
        };
        let editor = column()
            .key("editor-column")
            .fill_width()
            .fill_height()
            .flex_shrink(1.)
            .min_width(0.)
            .min_height(0.)
            .children(self.editor_tabs(layout, cx))
            .children(self.disk_notice(cx))
            .child(search_bar)
            .child(content);
        let body = row()
            .key("body")
            .fill_width()
            .fill_height()
            .flex_shrink(1.)
            .min_height(0.)
            .children(self.rail(layout, cx));
        match layout.panels {
            settings::PanelMode::Tiled => body.child(self.tiled(editor.into_element(), cx)),
            settings::PanelMode::Docked => body
                .children(self.panel_slot(Slot::Left, layout, cx))
                .child(editor.children(self.panel_slot(Slot::Bottom, layout, cx)))
                .children(self.panel_slot(Slot::Right, layout, cx)),
        }
        .into_element()
    }
}

#[cfg(test)]
mod tests;
