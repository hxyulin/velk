use super::{Document, LoadPlugins, UnloadPlugins};
use rxui::prelude::*;
use velk_plugin_host::{Client, Event};
pub(super) enum Operation {
    Load,
    UnloadAll,
    Unload(String),
    Restart(Option<String>),
    LoadSelected(Vec<String>, Vec<(String, Vec<u8>)>),
}

pub(super) struct State {
    pub(super) client: Client,
    markdown_provider: String,
    pub(super) task: Option<Task>,
    pub status: String,
    pub(super) loaded: Vec<velk_plugin_host::PluginInfo>,
    /// Last info seen per ID this session, so unloaded plugins keep a description.
    pub(super) known: std::collections::BTreeMap<String, velk_plugin_host::PluginInfo>,
    /// Rendered README copies remain usable after a plugin unloads.
    pub(super) readmes: std::collections::BTreeMap<String, velk_markup::Document>,
    /// Manifest IDs from the latest lifecycle report.
    pub(super) available: Vec<String>,
    /// Latest rejection per ID, cleared when that plugin loads.
    pub(super) failed: std::collections::BTreeMap<String, String>,
}
impl State {
    pub(super) fn new(client: Client, markdown_provider: String) -> Self {
        Self {
            client,
            markdown_provider,
            task: None,
            status: String::new(),
            loaded: Vec::new(),
            known: Default::default(),
            readmes: Default::default(),
            available: Vec::new(),
            failed: Default::default(),
        }
    }
}
impl Document {
    pub fn enable_plugins(&mut self, client: Client, cx: &mut Context<'_, Self>) {
        self.enable_selected_plugins(client, None, Vec::new(), cx);
    }
    pub fn enable_selected_plugins(
        &mut self,
        client: Client,
        selected: Option<Vec<String>>,
        states: Vec<(String, Vec<u8>)>,
        cx: &mut Context<'_, Self>,
    ) {
        self.plugins = Some(State::new(
            client,
            self.shell.store.preferences.markdown_provider.clone(),
        ));
        self.plugin_operation(
            selected.map_or(Operation::Load, |ids| Operation::LoadSelected(ids, states)),
            cx,
        );
    }
    pub(super) fn markup_renderer(&self) -> std::sync::Arc<dyn velk_markup::Renderer> {
        self.plugins.as_ref().map_or_else(
            || {
                std::sync::Arc::new(velk_markup::PlainText)
                    as std::sync::Arc<dyn velk_markup::Renderer>
            },
            |state| {
                std::sync::Arc::new(
                    state
                        .client
                        .markdown_renderer(state.markdown_provider.clone()),
                ) as std::sync::Arc<dyn velk_markup::Renderer>
            },
        )
    }
    pub(super) fn plugins_busy(&self) -> bool {
        self.plugins.as_ref().is_none_or(|s| s.task.is_some())
    }
    pub(super) fn change_plugins(&mut self, load: bool, cx: &mut Context<'_, Self>) {
        self.plugin_operation(
            if load {
                Operation::Load
            } else {
                Operation::UnloadAll
            },
            cx,
        );
    }
    pub(super) fn plugin_operation(&mut self, operation: Operation, cx: &mut Context<'_, Self>) {
        let Some(state) = &mut self.plugins else {
            return;
        };
        if state.task.is_some() {
            cx.unchanged();
            return;
        }
        self.suspend_panels();
        let disabled = self.shell.store.preferences.disabled_plugins.clone();
        let state = self.plugins.as_mut().expect("plugin state");
        let receiver = state
            .client
            .set_settings(self.shell.store.preferences.plugins.clone())
            .and_then(|_| match &operation {
                Operation::Load => state.client.load_except(disabled),
                Operation::LoadSelected(ids, states) => {
                    state.client.load_restored(ids.clone(), states.clone())
                }
                Operation::Restart(id) => state.client.restart(id.clone()),
                Operation::UnloadAll => state.client.unload_all(),
                Operation::Unload(id) => state.client.unload(id.clone()),
            });
        let receiver = match receiver {
            Ok(r) => r,
            Err(e) => {
                state.status = format!("Plugins: {e}");
                self.refresh_panels(cx);
                return;
            }
        };
        state.status = if matches!(operation, Operation::Load | Operation::LoadSelected(..)) {
            "Loading plugins…"
        } else if matches!(operation, Operation::Restart(_)) {
            "Restarting plugins…"
        } else {
            "Unloading plugins…"
        }
        .into();
        let renderer = state
            .client
            .markdown_renderer(state.markdown_provider.clone());
        match cx.try_spawn(
            async move {
                use velk_markup::Renderer;
                let result = receiver.recv().await;
                let mut readmes = std::collections::BTreeMap::new();
                if let Ok(Ok(report)) = &result {
                    for plugin in &report.plugins {
                        let source = plugin.metadata.readme.clone();
                        if !source.is_empty() {
                            let document = renderer
                                .render(source.clone())
                                .await
                                .unwrap_or_else(|_| velk_markup::Document::plain(source));
                            readmes.insert(plugin.id.clone(), document);
                        }
                    }
                }
                (result, readmes)
            },
            |this, result, cx| {
                let Some(state) = &mut this.plugins else {
                    return;
                };
                state.task = None;
                let result = result.map(|(result, readmes)| {
                    for (id, readme) in readmes {
                        state.readmes.insert(id, readme);
                    }
                    result
                });
                state.status = match result {
                    Ok(Ok(Ok(report))) => {
                        for event in &report.events {
                            if let Event::Rejected { id, message } = event {
                                state.failed.insert(id.clone(), message.clone());
                            }
                        }
                        for plugin in &report.plugins {
                            state.failed.remove(&plugin.id);
                            if plugin.metadata.readme.is_empty() {
                                state.readmes.remove(&plugin.id);
                            }
                            state.known.insert(plugin.id.clone(), plugin.clone());
                        }
                        state.available = report.available;
                        state.loaded = report.plugins;
                        let errors: Vec<_> = report
                            .events
                            .iter()
                            .filter_map(|e| match e {
                                Event::Rejected { id, message } => Some(format!("{id}: {message}")),
                                _ => None,
                            })
                            .collect();
                        if errors.is_empty() {
                            format!("Plugins: {} loaded", report.loaded)
                        } else {
                            format!("Plugins: {} loaded · {}", report.loaded, errors.join("; "))
                        }
                    }
                    Ok(Ok(Err(error))) => format!("Plugins: {error}"),
                    _ => "Plugin worker unavailable".into(),
                };
                this.refresh_panels(cx);
                if this.has_buffers() && this.buffer.search.open {
                    this.buffer.search.preserve_selection = true;
                    this.restart_search(cx);
                }
            },
        ) {
            Ok(task) => state.task = Some(task),
            Err(error) => {
                state.status = format!("Plugins: {error}");
                self.refresh_panels(cx);
            }
        }
    }
}

impl Document {
    pub(super) fn set_plugin_menu(&mut self, open: bool, cx: &mut Context<'_, Self>) {
        if open {
            self.shell.open = false;
            self.palette.open = false;
        }
        self.plugin_menu_open = open;
        if !open {
            let _ = self.editor_focus.focus(cx);
        }
    }
    pub(super) fn plugin_actions(
        &self,
        cx: &ViewContext<'_, Self>,
    ) -> (
        rxui::CommandAction<LoadPlugins>,
        rxui::CommandAction<UnloadPlugins>,
    ) {
        let load = cx
            .command(LoadPlugins, |this, _, cx| this.change_plugins(true, cx))
            .label("Load from disk")
            .enabled(!self.plugins_busy());
        let unload = cx
            .command(UnloadPlugins, |this, _, cx| this.change_plugins(false, cx))
            .label("Unload all")
            .enabled(
                !self.plugins_busy() && self.plugins.as_ref().is_some_and(|s| !s.loaded.is_empty()),
            );
        (load, unload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ClosePlugins;
    use rxui::{KeyEvent, Runtime, Ui};

    #[test]
    fn plugin_modal_preserves_editor_layout_contains_focus_and_restores_editing() {
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
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let editor = runtime.update(|cx| cx.new(|_| editor));
        let document =
            runtime.update(|cx| cx.new(|cx| Document::new(editor.clone(), file, path, cx)));
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        let prepare = |ui: &mut Ui<Document>, runtime: &mut Runtime| {
            ui.prepare(runtime, [640., 400.], &mut Measure).unwrap()
        };
        prepare(&mut ui, &mut runtime);
        let source = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        let source_bounds = ui.elements().find(|e| e.id == source).unwrap().bounds;
        assert!(ui.focus(source));
        for spelling in ["F4", "Named(F4)"] {
            for open in [true, false, true] {
                ui.key(
                    &mut runtime,
                    KeyEvent {
                        key: KeyboardKey::Other(spelling.into()),
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    },
                )
                .unwrap();
                prepare(&mut ui, &mut runtime);
                assert_eq!(
                    ui.elements().find(|e| e.id == source).unwrap().bounds,
                    source_bounds,
                    "opening or closing the modal must not resize the editor"
                );
                if open {
                    assert!(!ui.focus(source), "modal must block focus on the editor");
                    assert_ne!(ui.focused_element(), Some(source));
                }
                assert_eq!(
                    ui.elements()
                        .any(|e| e.key == Some(&rxui::Key::from("plugin-menu"))),
                    open
                );
            }
            // Focus the close button as a pointer/tab interaction would, then
            // dispatch its action. Closing must restore the editing surface.
            let close = ui
                .elements()
                .find(|e| e.key == Some(&rxui::Key::from("plugin-menu-close")))
                .unwrap()
                .id;
            ui.focus(close);
            assert_eq!(ui.focused_element(), Some(close));
            assert_eq!(
                ui.dispatch_command::<ClosePlugins>(&mut runtime).unwrap(),
                rxui::CommandStatus::Handled
            );
            prepare(&mut ui, &mut runtime);
            assert_eq!(ui.focused_element(), Some(source));
            ui.key(
                &mut runtime,
                KeyEvent {
                    key: KeyboardKey::Other(spelling.into()),
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            prepare(&mut ui, &mut runtime);
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
            prepare(&mut ui, &mut runtime);
            assert!(
                !ui.elements()
                    .any(|e| e.key == Some(&rxui::Key::from("plugin-menu")))
            );
        }
        runtime.update(|cx| {
            assert_eq!(editor.read(cx).buffer().text().to_string(), "original");
            assert!(!editor.read(cx).buffer().is_dirty());
        });
    }
}
