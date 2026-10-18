use super::Document;
use rxui::prelude::*;
use velk_plugin_host::{Client, Event};
pub(super) enum Operation {
    Load,
    #[cfg(test)]
    UnloadAll,
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
        let operation = match selected {
            Some(ids) => Operation::LoadSelected(ids, states),
            None if states.is_empty() => Operation::Load,
            None => Operation::LoadSelected(Vec::new(), states),
        };
        self.plugin_operation(operation, cx);
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
            for plugin in &report.plugins {
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
                format!("Features: {} loaded", report.loaded)
            } else {
                format!("Features: {} loaded · {}", report.loaded, errors.join("; "))
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
        let state = self.plugins.as_mut().expect("plugin state");
        let receiver = state
            .client
            .set_settings(self.shell.store.preferences.plugins.clone())
            .and_then(|_| match &operation {
                Operation::Load => state.client.load(),
                #[cfg(test)]
                Operation::UnloadAll => state.client.unload_all(),
                Operation::LoadSelected(ids, states) => {
                    state.client.load_restored(ids.clone(), states.clone())
                }
                Operation::Restart(id) => state.client.restart(id.clone()),
            });
        let receiver = match receiver {
            Ok(r) => r,
            Err(e) => {
                state.status = format!("Features: {e}");
                self.refresh_panels(cx);
                return;
            }
        };
        state.status = if matches!(operation, Operation::Load | Operation::LoadSelected(..)) {
            "Starting editor services…"
        } else if matches!(operation, Operation::Restart(_)) {
            "Applying feature settings…"
        } else {
            "Stopping editor services…"
        }
        .into();
        match cx.try_spawn(async move { receiver.recv().await }, |this, result, cx| {
            let Some(state) = &mut this.plugins else {
                return;
            };
            state.task = None;
            let status = match result {
                Ok(Ok(Ok(report))) => this.apply_plugin_report(report, cx),
                Ok(Ok(Err(error))) => format!("Features: {error}"),
                _ => "Editor service worker unavailable".into(),
            };
            this.plugins.as_mut().expect("plugin state").status = status;
            this.publish_buffer_snapshot(cx);
            this.refresh_panels(cx);
            if this.has_buffers() && this.buffer.search.open {
                this.buffer.search.preserve_selection = true;
                this.restart_search(cx);
            }
        }) {
            Ok(task) => state.task = Some(task),
            Err(error) => {
                state.status = format!("Features: {error}");
                self.refresh_panels(cx);
            }
        }
    }
}
