use super::{Document, LoadPlugins, UnloadPlugins};
use rxui::{Element, prelude::*};
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
    task: Option<Task>,
    pub status: String,
    pub(super) loaded: Vec<velk_plugin_host::PluginInfo>,
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
        self.plugins = Some(State {
            client,
            task: None,
            status: String::new(),
            loaded: Vec::new(),
        });
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
                std::sync::Arc::new(state.client.clone())
                    as std::sync::Arc<dyn velk_markup::Renderer>
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
        let receiver = match &operation {
            Operation::Load => state.client.load(),
            Operation::LoadSelected(ids, states) => {
                state.client.load_restored(ids.clone(), states.clone())
            }
            Operation::Restart(id) => state.client.restart(id.clone()),
            Operation::UnloadAll => state.client.unload_all(),
            Operation::Unload(id) => state.client.unload(id.clone()),
        };
        let receiver = match receiver {
            Ok(r) => r,
            Err(e) => {
                state.status = format!("Plugins: {e}");
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
        match cx.try_spawn(async move { receiver.recv().await }, |this, result, cx| {
            let Some(state) = &mut this.plugins else {
                return;
            };
            state.task = None;
            state.status = match result {
                Ok(Ok(Ok(report))) => {
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
            if this.search.open {
                this.search.preserve_selection = true;
                this.restart_search(cx);
            }
        }) {
            Ok(task) => state.task = Some(task),
            Err(error) => state.status = format!("Plugins: {error}"),
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
    pub(super) fn plugin_menu(
        &self,
        cx: &mut ViewContext<'_, Self>,
        toggle: rxui::CommandAction<super::TogglePlugins>,
        close: rxui::CommandAction<super::ClosePlugins>,
        load: rxui::CommandAction<LoadPlugins>,
        unload: rxui::CommandAction<UnloadPlugins>,
    ) -> Option<Element> {
        if !self.plugin_menu_open {
            return None;
        }
        let close = close.label("Close (F2 / Esc)");
        let menu = column()
            .on_command(toggle)
            .on_command(close.clone())
            .on_command(load.clone())
            .on_command(unload.clone())
            .fill_width()
            .max_height(360.)
            .flex_shrink(0.)
            .padding(10.)
            .gap(6.)
            .background(ThemeColor::Surface)
            .child(label("Plugins").font_size(16.))
            .child(
                row()
                    .gap(8.)
                    .child(load.button())
                    .child(unload.button())
                    .child(close.button().key("plugin-menu-close")),
            );
        let mut entries = column()
            .gap(6.)
            .fill_width()
            .scroll_y()
            .flex_shrink(1.)
            .min_height(0.);
        if let Some(state) = &self.plugins {
            entries = entries.child(label(state.status.clone()).font_size(12.));
            if state.loaded.is_empty() {
                entries = entries.child(label(
                    "No plugins loaded. Load from disk to read plugins.json.",
                ));
            }
            for plugin in &state.loaded {
                let id = plugin.id.clone();
                let description = if plugin.resident_code {
                    format!("{} · Code retained until exit", plugin.description)
                } else {
                    plugin.description.clone()
                };
                entries = entries.child(
                    column()
                        .gap(3.)
                        .child(
                            row()
                                .gap(8.)
                                .child(label(format!(
                                    "{} · {} · generation {}",
                                    plugin.id, plugin.version, plugin.generation
                                )))
                                .child(
                                    button("Restart")
                                        .disabled(self.plugins_busy() || self.reloading())
                                        .on_click(cx.listener({
                                            let id = id.clone();
                                            move |this, _, cx| {
                                                this.plugin_operation(
                                                    Operation::Restart(Some(id.clone())),
                                                    cx,
                                                )
                                            }
                                        })),
                                )
                                .child(button("Unload").disabled(self.plugins_busy()).on_click(
                                    cx.listener(move |this, _, cx| {
                                        this.plugin_operation(Operation::Unload(id.clone()), cx);
                                    }),
                                )),
                        )
                        .child(label(description).font_size(12.)),
                );
            }
        } else {
            entries = entries.child(label("Plugin worker unavailable."));
        }
        Some(
            modal(menu.child(entries))
                .key("plugin-menu")
                .width(560.)
                .accessibility_label("Plugins")
                .on_dismiss(cx.listener(|this, _: &DismissEvent, cx| {
                    this.set_plugin_menu(false, cx);
                }))
                .into_element(),
        )
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
        for spelling in ["F2", "Named(F2)"] {
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
