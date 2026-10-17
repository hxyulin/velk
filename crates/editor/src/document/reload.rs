//! Supported host state survives process replacement; service results are rebuilt.
use super::Document;
use crate::{session, settings::Preferences};
use rxui::prelude::*;
use serde::{Deserialize, Serialize};

pub(super) struct RestartPlugins;
impl Command for RestartPlugins {}
pub(crate) struct RestoreFocus;
impl Command for RestoreFocus {}
pub(super) struct ReloadWindow;
impl Command for ReloadWindow {}
#[derive(Default, Serialize, Deserialize)]
pub struct UiState {
    preferences: Preferences,
    #[serde(default)]
    pub(crate) views: velk_editor_model::views::Snapshot,
    settings_open: bool,
    plugins_open: bool,
    palette_open: bool,
    palette_query: String,
    palette_selected: usize,
    search_open: bool,
    search_query: velk_search::Query,
    pub plugins: Vec<String>,
    notice: Option<String>,
    #[serde(default)]
    panels: super::panels::UiState,
    #[serde(default)]
    pub(crate) workspace: crate::workspace::Workspace,
}
impl Document {
    pub(super) fn reloading(&self) -> bool {
        self.reload.is_some()
    }
    pub(super) fn can_reload(&self, cx: &impl rxui::ReadContext) -> bool {
        !self.busy()
            && !self.any_saving()
            && self.opening.is_none()
            && self.lifecycle.close_intent().is_none()
            && self.diagnostics.is_none()
            && !self.plugins.as_ref().is_some_and(|_| self.plugins_busy())
            && self.shell.saving.is_none()
            && self
                .buffer
                .as_ref()
                .is_none_or(|b| !b.editor.read(cx).has_composition())
    }
    pub fn restore_ui(&mut self, state: UiState) {
        let panel_source = self.panels.ui.source.clone();
        self.workspace = state.workspace;
        self.panels.ui = state.panels;
        if self.workspace.root().is_none()
            && let Some(root) = self.panels.ui.root.as_ref().and_then(|p| p.to_path().ok())
        {
            self.workspace.set_root(root);
        }
        if self.panels.ui.source.is_none() {
            self.panels.ui.source = panel_source;
        }
        self.shell.store.preferences = state.preferences;
        self.shell.open = state.settings_open;
        self.plugin_menu_open = state.plugins_open;
        self.palette.open = state.palette_open;
        self.palette.query = state.palette_query;
        self.palette.selected = state.palette_selected;
        if let Some(buffer) = self.buffer.as_mut() {
            buffer.search.open = state.search_open;
            buffer.search.query = state.search_query;
            buffer.search.preserve_selection = true;
        }
        let notice = match velk_diagnostics::status().filter(|status| status.recording) {
            Some(status) => Some(format!(
                "Trace capture continued: {}",
                status.path.display()
            )),
            None => state.notice,
        };
        self.notice(notice);
        self.restore_focus_pending = true;
    }
    pub fn focus_restoration(&self) -> Option<rxui::CommandAction<RestoreFocus>> {
        self.restore_focus_pending
            .then(|| self.restore_focus_action.borrow().clone())
            .flatten()
    }
    pub(super) fn restore_focus(&mut self, cx: &mut Context<'_, Self>) {
        let result = if self.shell.open || self.plugin_menu_open || self.palette.open {
            Ok(()) // RXUI's modal owns autofocus and the focus trap.
        } else if let Some(result) = self.focus_bottom(cx) {
            result
        } else if self.has_buffers() && self.buffer.search.open {
            self.buffer.search.focus.focus(cx)
        } else {
            self.editor_focus.focus(cx)
        };
        if result.is_ok() {
            self.restore_focus_pending = false;
        }
    }
    pub(super) fn reload_window(&mut self, cx: &mut Context<'_, Self>) {
        if !self.can_reload(cx) {
            return;
        }
        self.cancel_search(cx);
        let ui = UiState {
            views: self.capture_editor_views(cx),
            preferences: self.shell.store.preferences.clone(),
            settings_open: self.shell.open,
            plugins_open: self.plugin_menu_open,
            palette_open: self.palette.open,
            palette_query: self.palette.query.clone(),
            palette_selected: self.palette.selected,
            search_open: self.buffer.as_ref().is_some_and(|b| b.search.open),
            search_query: self
                .buffer
                .as_ref()
                .map(|b| b.search.query.clone())
                .unwrap_or_default(),
            plugins: self.plugins.as_ref().map_or_else(Vec::new, |state| {
                state.loaded.iter().map(|p| p.id.clone()).collect()
            }),
            notice: self
                .buffer
                .as_ref()
                .and_then(|b| b.status.clone())
                .or_else(|| self.window_status.clone()),
            panels: self.panels.ui.clone(),
            workspace: self.workspace.clone(),
        };
        let geometry = self
            .window
            .as_ref()
            .and_then(WindowHandle::geometry)
            .unwrap_or_else(|| rxui::WindowGeometry::new(1280., 800.));
        let current = self.buffer.as_ref();
        let active_id = current.map_or(0, |b| b.id);
        let editor = current.map(|b| b.editor.read(cx));
        let pending = session::Pending {
            next_id: self.next_buffer,
            id: active_id,
            buffers: self
                .ids()
                .into_iter()
                .filter(|id| *id != active_id)
                .map(|id| {
                    let b = &self.inactive[&id];
                    let editor = b.editor.read(cx);
                    session::PendingBuffer {
                        id: b.id,
                        path: b.path.clone(),
                        file: b.file.clone(),
                        buffer: editor.buffer().snapshot(),
                        scroll: editor.scroll_offset(),
                        search_open: b.search.open,
                        search_query: b.search.query.clone(),
                    }
                })
                .collect(),
            active_index: self
                .ids()
                .iter()
                .position(|id| *id == active_id)
                .unwrap_or(0),
            path: current.map(|b| b.path.clone()).unwrap_or_default(),
            file: current.and_then(|b| b.file.clone()),
            buffer: editor.as_ref().map(|e| e.buffer().snapshot()),
            scroll: editor.as_ref().map_or([0., 0.], |e| e.scroll_offset()),
            ui,
            size: geometry.inner_size,
            position: geometry.outer_position,
            maximized: geometry.maximized,
            client: self.plugins.as_ref().map(|state| state.client.clone()),
        };
        drop(editor);
        self.notice(Some("Reloading window…".into()));
        match cx.try_spawn_blocking(
            move || session::launch(pending),
            |this, result, cx| {
                this.reload = None;
                match result {
                    Ok(Ok(mut child)) => {
                        if let Err(error) = cx.exit() {
                            let _ = child.kill();
                            // Reap off the UI thread if the host refuses shutdown.
                            let _ = cx.try_spawn_blocking(move || child.wait(), |_, _, _| {});
                            this.notice(Some(format!("Reload failed: {error}")));
                        } else if let Some(recovery) = &this.recovery {
                            recovery.discard();
                        }
                    }
                    Ok(Err(error)) => this.notice(Some(format!("Reload failed: {error}"))),
                    Err(error) => this.notice(Some(format!("Reload failed: {error}"))),
                }
            },
        ) {
            Ok(task) => self.reload = Some(task),
            Err(error) => self.notice(Some(format!("Reload failed: {error}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxui::{Runtime, Ui};
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
    #[test]
    fn restored_search_focus_is_resolved_in_its_live_view_placement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let document = runtime.update(|cx| {
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, path, cx))
        });
        runtime.update(|cx| {
            document.update(cx, |this, _| {
                this.restore_ui(UiState {
                    search_open: true,
                    search_query: velk_search::Query {
                        text: "original".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
            })
        });
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        ui.prepare(&mut runtime, [800., 500.], &mut Measure)
            .unwrap();
        runtime.update(|cx| {
            let action = document.read(cx).focus_restoration().unwrap();
            assert_eq!(action.invoke(cx).unwrap(), rxui::CommandStatus::Handled);
        });
        ui.prepare(&mut runtime, [800., 500.], &mut Measure)
            .unwrap();
        let input = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("search-query")))
            .unwrap()
            .id;
        assert_eq!(ui.focused_element(), Some(input));
        runtime.update(|cx| {
            assert!(document.read(cx).focus_restoration().is_none());
            // Missing executor must leave the live document usable.
            document.update(cx, |this, cx| this.reload_window(cx));
            assert!(!document.read(cx).reloading());
            assert!(
                document
                    .read(cx)
                    .buffer
                    .status
                    .as_ref()
                    .unwrap()
                    .contains("Reload failed")
            );
        });
    }
}
