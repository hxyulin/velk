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
    pub(super) editing: velk_editing::Providers,
    pub(super) completion_frontends: velk_commands::completion::frontend::Providers,
    pub(super) snippets: velk_commands::snippet::Providers,
    pub(super) matching: velk_commands::completion::matching::Policies,
    syntax: std::collections::BTreeMap<String, std::sync::Arc<dyn velk_syntax::Provider>>,
    pub(super) task: Option<Task>,
    pub status: String,
    pub(super) loaded: Vec<velk_plugin_host::PluginInfo>,
    /// Last info seen per ID this session, so unloaded plugins keep a description.
    pub(super) known: std::collections::BTreeMap<String, velk_plugin_host::PluginInfo>,
    /// Rendered README copies remain usable after a plugin unloads.
    pub(super) readmes: std::collections::BTreeMap<String, velk_markup::Document>,
    /// Manifest IDs from the latest lifecycle report.
    pub(super) available: Vec<String>,
    pub(super) inventory: Vec<velk_plugin_host::PluginSpec>,
    /// Latest rejection per ID, cleared when that plugin loads.
    pub(super) failed: std::collections::BTreeMap<String, String>,
}
impl State {
    pub(super) fn new(client: Client, markdown_provider: String) -> Self {
        Self {
            client,
            markdown_provider,
            editing: Default::default(),
            snippets: Default::default(),
            matching: Default::default(),
            completion_frontends: Default::default(),
            syntax: Default::default(),
            task: None,
            status: String::new(),
            loaded: Vec::new(),
            known: Default::default(),
            readmes: Default::default(),
            available: Vec::new(),
            inventory: Vec::new(),
            failed: Default::default(),
        }
    }
}
fn retain_editing_generations(
    previous: &velk_editing::Providers,
    current: velk_editing::Providers,
    before: &[velk_plugin_host::PluginInfo],
    after: &[velk_plugin_host::PluginInfo],
) -> velk_editing::Providers {
    current
        .into_iter()
        .map(|(key, provider)| {
            let unchanged = before.iter().any(|old| {
                old.id == key.0
                    && after
                        .iter()
                        .any(|new| new.id == old.id && new.generation == old.generation)
            });
            let provider = if unchanged {
                previous.get(&key).cloned().unwrap_or(provider)
            } else {
                provider
            };
            (key, provider)
        })
        .collect()
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
        self.publish_plugin_workspace(&client);
        self.listen_host_requests(client.host_requests(), cx);
        self.plugins = Some(State::new(
            client,
            self.shell.store.preferences.markdown_provider.clone(),
        ));
        self.plugin_operation(
            selected.map_or(Operation::Load, |ids| Operation::LoadSelected(ids, states)),
            cx,
        );
    }
    pub(super) fn selected_editing(
        &self,
    ) -> std::collections::BTreeMap<velk_editing::Role, std::sync::Arc<dyn velk_editing::Provider>>
    {
        let preferences = &self.shell.store.preferences;
        let Some(state) = &self.plugins else {
            return Default::default();
        };
        [
            (
                velk_editing::Role::Indentation,
                &preferences.indentation_provider,
            ),
            (velk_editing::Role::Pairing, &preferences.pairing_provider),
            (velk_editing::Role::Keymap, &preferences.keymap_provider),
        ]
        .into_iter()
        .filter_map(|(role, id)| {
            state
                .editing
                .get(&(id.clone(), role))
                .map(|provider| (role, provider.clone()))
        })
        .collect()
    }
    pub(super) fn refresh_editing(&mut self, cx: &mut Context<'_, Self>) {
        let providers = self.selected_editing();
        for buffer in self.buffer.iter().chain(self.inactive.values()) {
            for view in &buffer.views {
                view.editor.update(cx, |editor, _| {
                    editor.set_editing_providers(providers.clone())
                });
            }
        }
    }
    pub(super) fn file_type(
        &self,
        path: &std::path::Path,
    ) -> Option<velk_plugin_contracts::file_types::FileType> {
        velk_plugin_contracts::file_types::resolve(
            path,
            self.plugins
                .iter()
                .flat_map(|s| &s.loaded)
                .flat_map(|p| &p.metadata.file_types),
            &self.shell.store.preferences.file_associations,
        )
    }
    pub(super) fn language_for_path(
        &self,
        path: &std::path::Path,
    ) -> Option<velk_syntax::Language> {
        self.file_type(path)
            .map(|t| velk_syntax::Language::new(t.id))
    }
    pub(super) fn refresh_syntax(&mut self, cx: &mut Context<'_, Self>) {
        let provider = self
            .plugins
            .as_ref()
            .and_then(|state| {
                state
                    .syntax
                    .get(&self.shell.store.preferences.syntax_provider)
            })
            .cloned();
        for buffer in self.buffer.iter().chain(self.inactive.values()) {
            buffer.refresh_view_languages(self.language_for_path(&buffer.path), cx);
            for view in &buffer.views {
                view.editor.update(cx, |editor, _| {
                    if let Err(error) = editor.set_syntax_provider(provider.clone()) {
                        tracing::warn!(%error, "Cannot start syntax service");
                    }
                });
            }
        }
    }
    fn apply_plugin_report(
        &mut self,
        report: velk_plugin_host::Report,
        cx: &mut Context<'_, Self>,
    ) -> String {
        crate::undo::configure(&self.shell.store.preferences, Some(report.undo));
        let state = self.plugins.as_mut().expect("plugin state");
        let language_types = |plugins: &[velk_plugin_host::PluginInfo]| {
            plugins
                .iter()
                .flat_map(|p| p.metadata.file_types.clone())
                .collect::<Vec<_>>()
        };
        let languages_changed = language_types(&state.loaded) != language_types(&report.plugins);
        let status = {
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
            state.syntax = report
                .syntax
                .into_iter()
                .map(|(id, provider)| {
                    let unchanged = state.loaded.iter().any(|old| {
                        old.id == id
                            && report
                                .plugins
                                .iter()
                                .any(|new| new.id == id && new.generation == old.generation)
                    });
                    let provider = if unchanged {
                        state.syntax.get(&id).cloned().unwrap_or(provider)
                    } else {
                        provider
                    };
                    (id, provider)
                })
                .collect();
            state.editing = retain_editing_generations(
                &state.editing,
                report.editing,
                &state.loaded,
                &report.plugins,
            );
            state.snippets = report.snippets;
            state.matching = report.matching;
            state.completion_frontends = report.completion_frontends;
            state.available = report.available;
            state.inventory = report.inventory;
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
        };
        self.refresh_editing(cx);
        self.refresh_syntax(cx);
        if languages_changed {
            self.start_all_semantics(cx);
        }
        status
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
        self.cancel_collection(true, cx);
        self.deferred_choice = None;
        self.suspend_panels();
        let disabled = self.shell.store.preferences.disabled_plugins.clone();
        let state = self.plugins.as_mut().expect("plugin state");
        let receiver = state
            .client
            .set_settings(self.shell.store.preferences.plugins.clone())
            .and_then(|_| {
                state
                    .client
                    .set_interface_bindings(self.shell.store.preferences.interface_bindings.clone())
            })
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
                let status = match result {
                    Ok(Ok(Ok(report))) => this.apply_plugin_report(report, cx),
                    Ok(Ok(Err(error))) => format!("Plugins: {error}"),
                    _ => "Plugin worker unavailable".into(),
                };
                this.plugins.as_mut().expect("plugin state").status = status;
                this.publish_buffer_snapshot(cx);
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
    #[allow(unsafe_code)]
    fn file_type_declarations_and_overrides_refresh_existing_views_and_retire_on_unload() {
        use velk_plugin_contracts::file_types::FileType;
        let directory = tempfile::tempdir().unwrap();
        let host = unsafe { velk_plugin_host::Runtime::start(directory.path().into()) }.unwrap();
        let mut harness = super::super::tests::Harness::new();
        let path = harness.write_file("source.custom", "text");
        let report = |plugins: Vec<velk_plugin_host::PluginInfo>| velk_plugin_host::Report {
            loaded: plugins.len(),
            plugins,
            events: vec![],
            available: vec![],
            inventory: vec![],
            editing: Default::default(),
            syntax: Default::default(),
            undo: Default::default(),
            snippets: Default::default(),
            matching: Default::default(),
            completion_frontends: Default::default(),
        };
        harness.runtime.update(|cx| {
            harness.document.update(cx, |this, cx| {
                let (file, editor) = crate::file::FileState::open(&path).unwrap();
                this.add_buffer(file, editor, path.clone(), cx);
                let mut state = State::new(host.client(), String::new());
                state.loaded.push(velk_plugin_host::PluginInfo {
                    id: "test.language".into(),
                    version: "1".into(),
                    generation: 1,
                    loaded_path: Default::default(),
                    description: String::new(),
                    metadata: velk_plugin_host::PluginMetadata {
                        name: "Language".into(),
                        file_types: vec![
                            FileType {
                                id: "custom".into(),
                                name: "Custom".into(),
                                extensions: vec!["custom".into()],
                                filenames: vec![],
                            },
                            FileType {
                                id: "other".into(),
                                name: "Other".into(),
                                extensions: vec![],
                                filenames: vec![],
                            },
                        ],
                        ..Default::default()
                    },
                    settings_values: Default::default(),
                    settings_error: None,
                    resident_code: false,
                    hooks: vec![],
                    dependencies: vec![],
                });
                let declarations = std::mem::take(&mut state.loaded);
                this.plugins = Some(state);
                assert!(this.buffer.semantic.is_none());
                this.apply_plugin_report(report(declarations), cx);
                assert!(this.buffer.semantic.is_some());
                assert_eq!(
                    this.buffer.editor.read(cx).language().unwrap().as_str(),
                    "custom"
                );
                assert_eq!(this.command_context(cx).language.as_deref(), Some("custom"));
                this.shell
                    .store
                    .preferences
                    .file_associations
                    .insert("*.custom".into(), "other".into());
                this.refresh_syntax(cx);
                assert_eq!(
                    this.buffer.editor.read(cx).language().unwrap().as_str(),
                    "other"
                );
                this.apply_plugin_report(report(vec![]), cx);
                assert!(this.buffer.editor.read(cx).language().is_none());
                assert!(this.buffer.semantic.is_none());
            })
        });
    }
    #[test]
    #[allow(unsafe_code)]
    fn editing_providers_refresh_initial_and_inactive_buffers_after_loading() {
        use std::{collections::BTreeMap, sync::Arc};
        use velk_editing::{Decision, Intent, Request, Role, input};
        #[derive(Debug)]
        struct Keymap;
        impl velk_editing::Provider for Keymap {
            fn evaluate(&self, request: &Request) -> Result<Decision, String> {
                let Intent::Key(key) = &request.intent else {
                    return Ok(Decision::Decline);
                };
                Ok(Decision::Input(input::Response {
                    handled: true,
                    state: input::State {
                        mode: if key.state.mode.is_empty() {
                            "normal".into()
                        } else {
                            key.state.mode.clone()
                        },
                        capture_text: true,
                        ..Default::default()
                    },
                    actions: vec![],
                }))
            }
        }
        let directory = tempfile::tempdir().unwrap();
        // SAFETY: this test-owned directory contains no native libraries.
        let host = unsafe { velk_plugin_host::Runtime::start(directory.path().into()) }.unwrap();
        let mut runtime = Runtime::new();
        let initial = runtime.update(|cx| cx.new(|_| velk_editor_widget::Editor::new("initial")));
        let document = runtime.update(|cx| {
            cx.new(|cx| Document::new(initial.clone(), None, "initial.txt".into(), cx))
        });
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                this.shell.store.preferences.keymap_provider = "test.keymap".into();
                this.plugins = Some(State::new(host.client(), String::new()));
                this.start_all_semantics(cx);
                this.new_buffer(cx);
                assert_eq!(initial.read(cx).input_mode_id(), "insert");
                this.apply_plugin_report(
                    velk_plugin_host::Report {
                        snippets: Default::default(),
                        matching: Default::default(),
                        completion_frontends: Default::default(),
                        inventory: Vec::new(),
                        syntax: Default::default(),
                        undo: Default::default(),
                        editing: BTreeMap::from([(
                            ("test.keymap".into(), Role::Keymap),
                            Arc::new(Keymap) as Arc<dyn velk_editing::Provider>,
                        )]),
                        loaded: 1,
                        plugins: vec![],
                        events: vec![],
                        available: vec![],
                    },
                    cx,
                );
                for buffer in this.buffer.iter().chain(this.inactive.values()) {
                    assert_eq!(buffer.editor.read(cx).input_mode_id(), "normal");
                    assert!(!buffer.editor.read(cx).buffer().is_dirty());
                }
                this.apply_plugin_report(
                    velk_plugin_host::Report {
                        snippets: Default::default(),
                        matching: Default::default(),
                        completion_frontends: Default::default(),
                        inventory: Vec::new(),
                        syntax: Default::default(),
                        undo: Default::default(),
                        editing: BTreeMap::new(),
                        loaded: 0,
                        plugins: vec![],
                        events: vec![],
                        available: vec![],
                    },
                    cx,
                );
                for buffer in this.buffer.iter().chain(this.inactive.values()) {
                    assert_eq!(buffer.editor.read(cx).input_mode_id(), "insert");
                }
            })
        });
    }
    #[test]
    fn unchanged_provider_generations_keep_session_identity_across_plugin_reports() {
        #[derive(Debug)]
        struct Provider;
        impl velk_editing::Provider for Provider {
            fn evaluate(
                &self,
                _: &velk_editing::Request,
            ) -> Result<velk_editing::Decision, String> {
                Ok(velk_editing::Decision::Decline)
            }
        }
        let info = velk_plugin_host::PluginInfo {
            dependencies: Vec::new(),
            id: "velk.vim".into(),
            version: "0.0.0-dev".into(),
            generation: 1,
            loaded_path: Default::default(),
            description: String::new(),
            metadata: Default::default(),
            settings_values: Default::default(),
            settings_error: None,
            resident_code: false,
            hooks: vec![],
        };
        let key = (info.id.clone(), velk_editing::Role::Keymap);
        let old = std::sync::Arc::new(Provider) as std::sync::Arc<dyn velk_editing::Provider>;
        let fresh = std::sync::Arc::new(Provider) as std::sync::Arc<dyn velk_editing::Provider>;
        let previous = std::collections::BTreeMap::from([(key.clone(), old.clone())]);
        let current = std::collections::BTreeMap::from([(key.clone(), fresh.clone())]);
        let same = retain_editing_generations(
            &previous,
            current.clone(),
            std::slice::from_ref(&info),
            std::slice::from_ref(&info),
        );
        assert!(std::sync::Arc::ptr_eq(&same[&key], &old));
        let mut replaced = info.clone();
        replaced.generation += 1;
        let changed = retain_editing_generations(&previous, current, &[info], &[replaced]);
        assert!(std::sync::Arc::ptr_eq(&changed[&key], &fresh));
    }
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
