//! Feature-neutral routing for optional command contributions and reducers.
use super::{Document, plugin_commands::Invoke, workspace_edits::Guard};
use rxui::prelude::*;
use velk_commands::{ChoiceAction, Handler, Outcome, contributions};

pub(super) struct State {
    id: u64,
    invocation: Invoke,
    guard: Guard,
    context: velk_commands::Context,
    view: u64,
    epoch: u64,
    providers: Vec<Invoke>,
    results: Vec<Option<contributions::ContributionResult>>,
    tasks: Vec<Task>,
    reducer: Option<Task>,
    version: u64,
    semantic: std::collections::VecDeque<usize>,
    refresh: bool,
    continuations: std::collections::BTreeMap<(String, u64, String), serde_json::Value>,
}
impl State {
    fn source_input(&self, index: usize) -> String {
        let provider = &self.providers[index];
        contributions::Input {
            activation: self.context.input.clone(),
            continuation: self
                .continuations
                .get(&(
                    provider.plugin.clone(),
                    provider.generation,
                    provider.command.id.clone(),
                ))
                .cloned(),
            refresh: self.refresh,
        }
        .encode()
        .expect("collection input is serializable")
    }
}
#[cfg(test)]
impl State {
    pub(super) fn settled(&self) -> bool {
        self.reducer.is_none() && self.results.iter().all(Option::is_some)
    }
    pub(super) fn has_candidate_from(&self, plugin: &str, filter_text: &str) -> bool {
        self.results.iter().flatten().any(|result| result.plugin == plugin && matches!(&result.outcome, Ok(Outcome::Completions { items, ..}) if items.iter().any(|item| item.filter_text == filter_text)))
    }
    pub(super) fn has_results_from(&self, plugin: &str) -> bool {
        self.results.iter().flatten().any(|result| result.plugin == plugin && matches!(&result.outcome, Ok(Outcome::Completions { items, ..}) if !items.is_empty()))
    }
}

impl Document {
    pub(super) fn schedule_automatic_completion(
        &mut self,
        text: String,
        prefix: String,
        cx: &mut Context<'_, Self>,
    ) {
        self.automatic_completion = None;
        if self.background_buffer
            || !self.command_available(cx)
            || self.command_task.is_some()
            || self.binding_modal()
        {
            return;
        }
        let Some(buffer) = self.buffer.as_ref() else {
            return;
        };
        let editor = buffer.editor.clone();
        let epoch = editor.read(cx).completion_epoch();
        let view = self.views.layout.active;
        let context = self.command_context(cx);
        let Some((invocation, hook)) = self
            .plugins
            .as_ref()
            .into_iter()
            .flat_map(|plugins| &plugins.loaded)
            .find_map(|plugin| {
                plugin.metadata.commands.iter().find_map(|command| {
                    let Handler::Collect {
                        automatic: Some(hook),
                        ..
                    } = &command.handler
                    else {
                        return None;
                    };
                    if command.language.is_some() && command.language != context.language {
                        return None;
                    }
                    Some((
                        Invoke {
                            plugin: plugin.id.clone(),
                            generation: plugin.generation,
                            command: command.clone(),
                        },
                        hook.clone(),
                    ))
                })
            })
        else {
            return;
        };
        let Handler::Collect { topic, .. } = &invocation.command.handler else {
            unreachable!()
        };
        let sources = self
            .plugins
            .as_ref()
            .into_iter()
            .flat_map(|plugins| &plugins.loaded)
            .flat_map(|plugin| &plugin.metadata.contributions)
            .filter(|source| {
                source.topic == *topic
                    && (source.command.language.is_none()
                        || source.command.language == context.language)
            })
            .take(32)
            .cloned()
            .collect();
        let activation = contributions::Activation {
            context,
            text,
            prefix,
            contributions: sources,
        };
        let client = self.plugins.as_ref().unwrap().client.clone();
        let provider = invocation.clone();
        let context = activation.context.clone();
        let guard = self.edit_guard(cx);
        self.automatic_completion = cx
            .try_spawn(
                async move {
                    rxui::sleep(std::time::Duration::from_millis(150)).await;
                    let handle = client
                        .bind_hook(provider.plugin, hook)?
                        .recv()
                        .await
                        .map_err(|e| e.to_string())??;
                    if handle.generation() != provider.generation {
                        return Err("activation provider replaced".to_string());
                    }
                    let output = client
                        .call_hook(
                            handle,
                            serde_json::to_vec(&activation).map_err(|e| e.to_string())?,
                        )
                        .await?;
                    serde_json::from_slice::<Option<String>>(&output).map_err(|e| e.to_string())
                },
                move |this, result, cx| {
                    this.automatic_completion = None;
                    let Ok(Ok(Some(input))) = result else {
                        return;
                    };
                    if input.len() > 16 * 1024
                        || !this.command_available(cx)
                        || this.command_task.is_some()
                        || this.binding_modal()
                        || this.background_buffer
                        || this.views.layout.active != view
                        || !this.guard_valid(&guard, cx)
                        || !this.command_source_matches(&context, cx)
                        || this
                            .buffer
                            .as_ref()
                            .is_none_or(|buffer| buffer.editor.id() != editor.id())
                        || editor.read(cx).completion_epoch() != epoch
                        || editor.read(cx).completion_active()
                        || !this.command_owner_loaded(&invocation.plugin, invocation.generation)
                    {
                        return;
                    }
                    this.begin_collection_input(invocation, false, Some(input), cx);
                },
            )
            .ok();
    }
    pub(super) fn cancel_collection(&mut self, dismiss: bool, cx: &mut Context<'_, Self>) {
        if dismiss {
            self.automatic_completion = None;
            self.cancel_completion_documentation();
        }
        if let Some(state) = self.collection.take() {
            // The terminal event still owns the semantic slot until cancellation
            // is acknowledged. A replacement collection waits for that event.
            if let Some(semantic) = self.buffer.as_mut().and_then(|b| b.semantic.as_mut())
                && let Some(pending) = &mut semantic.command
                && pending.collection.is_some_and(|(id, _)| id == state.id)
                && !pending.cancelled
            {
                pending.cancelled = semantic.service.cancel_command(pending.request).is_ok();
            }
            if dismiss && let Some(buffer) = self.buffer.as_mut() {
                buffer
                    .editor
                    .update(cx, |editor, _| editor.dismiss_completions());
            }
        }
    }
    pub(super) fn begin_collection(
        &mut self,
        invocation: Invoke,
        refresh: bool,
        cx: &mut Context<'_, Self>,
    ) {
        self.begin_collection_input(invocation, refresh, None, cx);
    }
    fn begin_collection_input(
        &mut self,
        invocation: Invoke,
        refresh: bool,
        input: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        self.automatic_completion = None;
        let continuations = if refresh {
            self.collection
                .as_ref()
                .filter(|state| {
                    state.invocation.plugin == invocation.plugin
                        && state.invocation.generation == invocation.generation
                        && state.invocation.command.id == invocation.command.id
                        && state.guard.source == self.buffer.as_ref().map(|buffer| buffer.id)
                        && state.view == self.views.layout.active
                })
                .map(|state| state.continuations.clone())
                .unwrap_or_default()
        } else {
            Default::default()
        };
        self.cancel_collection(false, cx);
        let Handler::Collect { topic, .. } = &invocation.command.handler else {
            return;
        };
        let Some(buffer) = self.buffer.as_ref() else {
            return;
        };
        let editor = buffer.editor.clone();
        let mut context = self.command_context(cx);
        context.input = input;
        let providers: Vec<_> = self
            .plugins
            .as_ref()
            .into_iter()
            .flat_map(|p| &p.loaded)
            .flat_map(|plugin| {
                plugin
                    .metadata
                    .contributions
                    .iter()
                    .filter(|contribution| {
                        contribution.topic == *topic
                            && (contribution.command.language.is_none()
                                || contribution.command.language == context.language)
                    })
                    .map(|contribution| Invoke {
                        plugin: plugin.id.clone(),
                        generation: plugin.generation,
                        command: contribution.command.clone(),
                    })
            })
            .take(32)
            .collect();
        let mut guard = self.edit_guard(cx);
        guard.owner = Some((invocation.plugin.clone(), invocation.generation));
        if !refresh {
            let policy = self
                .plugins
                .as_ref()
                .and_then(|state| {
                    state
                        .matching
                        .get(&self.shell.store.preferences.completion_matching_provider)
                })
                .cloned();
            editor.update(cx, |editor, _| {
                editor.show_completions_with_policy(Vec::new(), false, policy)
            });
        }
        let id = self.next_collection;
        self.next_collection += 1;
        self.collection = Some(State {
            id,
            invocation,
            guard,
            context,
            view: self.views.layout.active,
            epoch: editor.read(cx).completion_epoch(),
            results: vec![None; providers.len()],
            providers: providers.clone(),
            tasks: Vec::new(),
            reducer: None,
            version: 0,
            semantic: Default::default(),
            refresh,
            continuations,
        });
        for (index, provider) in providers.into_iter().enumerate() {
            match provider.command.handler.clone() {
                Handler::Semantic { .. } => {
                    self.collection.as_mut().unwrap().semantic.push_back(index);
                }
                Handler::Request { hook } => {
                    let client = self.plugins.as_ref().unwrap().client.clone();
                    let state = self.collection.as_ref().unwrap();
                    let mut context = state.context.clone();
                    context.input = Some(state.source_input(index));
                    let task = cx.try_spawn(
                        async move {
                            let handle = client
                                .bind_hook(provider.plugin, hook)?
                                .recv()
                                .await
                                .map_err(|e| e.to_string())??;
                            if handle.generation() != provider.generation {
                                return Err("contributor replaced".into());
                            }
                            let output = client
                                .call_hook(
                                    handle,
                                    serde_json::to_vec(&context).map_err(|e| e.to_string())?,
                                )
                                .await?;
                            let outcome: Outcome =
                                serde_json::from_slice(&output).map_err(|e| e.to_string())?;
                            outcome.validate()?;
                            Ok::<_, String>(outcome)
                        },
                        move |this, result, cx| {
                            this.receive_contribution(
                                id,
                                index,
                                result.unwrap_or_else(|error| Err(error.to_string())),
                                cx,
                            );
                        },
                    );
                    match task {
                        Ok(task) => self.collection.as_mut().unwrap().tasks.push(task),
                        Err(error) => {
                            self.receive_contribution(id, index, Err(error.to_string()), cx)
                        }
                    }
                }
                _ => unreachable!("validated contribution handler"),
            }
        }
        self.start_collection_semantic(cx);
        if self
            .collection
            .as_ref()
            .is_some_and(|state| state.providers.is_empty())
        {
            self.reduce_collection(cx);
        }
    }
    pub(super) fn start_collection_semantic(&mut self, cx: &mut Context<'_, Self>) {
        let Some(state) = self.collection.as_mut() else {
            return;
        };
        if self
            .buffer
            .as_ref()
            .and_then(|b| b.semantic.as_ref())
            .is_some_and(|s| s.command.is_some())
        {
            return;
        }
        let Some(index) = state.semantic.pop_front() else {
            return;
        };
        let id = state.id;
        let provider = state.providers[index].clone();
        let mut guard = state.guard.clone();
        guard.owner = Some((provider.plugin.clone(), provider.generation));
        let input = Some(state.source_input(index));
        self.execute_plugin_command(provider, input, guard, cx);
        if let Some(pending) = self
            .buffer
            .as_mut()
            .and_then(|b| b.semantic.as_mut())
            .and_then(|s| s.command.as_mut())
        {
            pending.collection = Some((id, index));
        } else {
            self.receive_contribution(
                id,
                index,
                Err("semantic contributor unavailable".into()),
                cx,
            );
        }
    }
    pub(super) fn receive_contribution(
        &mut self,
        id: u64,
        index: usize,
        outcome: Result<Outcome, String>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(state) = self.collection.as_mut().filter(|state| state.id == id) else {
            self.start_collection_semantic(cx);
            return;
        };
        let provider = &state.providers[index];
        let outcome = outcome.and_then(|outcome| {
            outcome.validate()?;
            Ok(outcome)
        });
        let key = (
            provider.plugin.clone(),
            provider.generation,
            provider.command.id.clone(),
        );
        if let Ok(Outcome::Completions {
            continuation: Some(token),
            ..
        }) = &outcome
        {
            state.continuations.insert(key, token.clone());
        } else {
            state.continuations.remove(&key);
        }
        state.results[index] = Some(contributions::ContributionResult {
            plugin: provider.plugin.clone(),
            generation: provider.generation,
            command: provider.command.id.clone(),
            outcome,
        });
        state.version += 1;
        self.reduce_collection(cx);
        self.start_collection_semantic(cx);
    }
    fn reduce_collection(&mut self, cx: &mut Context<'_, Self>) {
        let Some(state) = self.collection.as_ref() else {
            return;
        };
        if state.reducer.is_some() {
            return;
        }
        let Handler::Collect { hook, .. } = &state.invocation.command.handler else {
            return;
        };
        let (id, version) = (state.id, state.version);
        let invocation = state.invocation.clone();
        let hook = hook.clone();
        let input = contributions::Collection {
            context: state.context.clone(),
            results: state.results.iter().flatten().cloned().collect(),
        };
        let client = self.plugins.as_ref().unwrap().client.clone();
        let task = cx.try_spawn(
            async move {
                let handle = client
                    .bind_hook(invocation.plugin, hook)?
                    .recv()
                    .await
                    .map_err(|e| e.to_string())??;
                if handle.generation() != invocation.generation {
                    return Err("collection reducer replaced".into());
                }
                let output = client
                    .call_hook(handle, input.encode_bounded(1024 * 1024)?)
                    .await?;
                let outcome: Outcome =
                    serde_json::from_slice(&output).map_err(|e| e.to_string())?;
                outcome.validate()?;
                Ok::<_, String>(outcome)
            },
            move |this, result, cx| {
                let Some(state) = this.collection.as_mut().filter(|state| state.id == id) else {
                    return;
                };
                state.reducer = None;
                let guard = state.guard.clone();
                let epoch = state.epoch;
                let again = state.version != version;
                if this.guard_valid(&guard, cx)
                    && this.buffer.editor.read(cx).completion_epoch() == epoch
                    && this.buffer.editor.read(cx).completion_active()
                {
                    match result {
                        Ok(Ok(Outcome::Completions { items, .. })) => {
                            this.buffer
                                .editor
                                .update(cx, |editor, _| editor.show_completions(items, true));
                            this.notice(None);
                        }
                        Ok(Ok(_)) => this.notice(Some(
                            "Collection reducer must return caret suggestions".into(),
                        )),
                        Ok(Err(error)) => this.notice(Some(error)),
                        Err(error) => this.notice(Some(error.to_string())),
                    }
                }
                if again {
                    this.reduce_collection(cx);
                }
            },
        );
        match task {
            Ok(task) => self.collection.as_mut().unwrap().reducer = Some(task),
            Err(error) => self.notice(Some(error.to_string())),
        }
    }
    pub(super) fn sync_collection_completion(
        &mut self,
        source: Entity<velk_editor_widget::Editor>,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let Some(state) = self.collection.as_ref() else {
            return false;
        };
        if self.background_buffer {
            return true;
        }
        let Some(buffer) = self.buffer.as_ref() else {
            self.cancel_collection(false, cx);
            return true;
        };
        if buffer.editor.id() != source.id() {
            return true;
        }
        if state.guard.source != Some(buffer.id)
            || state.view != self.views.layout.active
            || state.context.path != self.command_context(cx).path
            || state.epoch != source.read(cx).completion_epoch()
            || !self.command_owner_loaded(&state.invocation.plugin, state.invocation.generation)
        {
            self.cancel_collection(true, cx);
            return true;
        }
        let accepted = source.update(cx, |editor, cx| {
            let accepted = editor.take_completion();
            if accepted.is_none() {
                cx.unchanged();
            }
            accepted
        });
        if let Some((target, choice)) = accepted {
            let state = self.collection.as_ref().unwrap();
            let guard = state.guard.clone();
            let source_valid = match &choice.action {
                ChoiceAction::Contributed {
                    plugin, generation, ..
                } => state
                    .providers
                    .iter()
                    .any(|p| p.plugin == *plugin && p.generation == *generation),
                _ => false,
            };
            self.cancel_collection(false, cx);
            if target.revision == source.read(cx).buffer().revision()
                && target.position == source.read(cx).caret_position()
                && self.guard_valid(&guard, cx)
                && source_valid
            {
                self.apply_choice_action(choice.choice.action, guard, cx);
            }
            return true;
        }
        if !source.read(cx).completion_active() {
            self.cancel_collection(false, cx);
        } else if source.read(cx).completion_refresh().is_some()
            && !self.command_source_matches(&self.collection.as_ref().unwrap().context, cx)
        {
            let invocation = self.collection.as_ref().unwrap().invocation.clone();
            self.begin_collection(invocation, true, cx);
        }
        true
    }
    pub(super) fn drain_deferred_choice(&mut self, cx: &mut Context<'_, Self>) {
        if let Some((action, guard)) = self.deferred_choice.take() {
            self.apply_choice_action(action, guard, cx);
        }
    }
    pub(super) fn apply_choice_action(
        &mut self,
        action: ChoiceAction,
        mut guard: Guard,
        cx: &mut Context<'_, Self>,
    ) {
        let action = match action {
            ChoiceAction::Contributed {
                plugin,
                generation,
                action,
            } => {
                guard.owner = Some((plugin, generation));
                *action
            }
            action => action,
        };
        if !self.guard_valid(&guard, cx) {
            return;
        }
        match action {
            ChoiceAction::Snippet { snippet } => self.apply_workspace_snippet(snippet, guard, cx),
            ChoiceAction::Edit { edit } => {
                self.apply_guarded_outcome(Outcome::Edit { edit }, guard, cx)
            }
            ChoiceAction::BufferEdit {
                id,
                revision,
                edits,
            } => {
                if let Err(error) = self.host_request(
                    velk_plugin_contracts::host::Request::EditBuffer {
                        id,
                        revision,
                        edits,
                    },
                    guard.owner,
                    cx,
                ) {
                    self.notice(Some(error));
                }
            }
            ChoiceAction::Semantic { payload } => {
                if self
                    .buffer
                    .as_ref()
                    .and_then(|b| b.semantic.as_ref())
                    .is_some_and(|s| s.command.is_some())
                {
                    self.deferred_choice = Some((ChoiceAction::Semantic { payload }, guard));
                    return;
                }
                let Some((plugin, generation)) = guard.owner.clone() else {
                    return;
                };
                self.execute_plugin_command(
                    Invoke {
                        plugin,
                        generation,
                        command: velk_commands::Declaration {
                            id: "velk.result.resolve".into(),
                            title: "Resolve result".into(),
                            language: None,
                            input: None,
                            shortcut: None,
                            handler: Handler::Semantic { payload },
                        },
                    },
                    None,
                    guard,
                    cx,
                );
            }
            ChoiceAction::Location { location } => self.open_location(location, cx),
            ChoiceAction::Contributed { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::tests::Harness;
    use velk_commands::{Declaration, Encoding, Position, TextEdit};
    fn info(id: &str, generation: u64) -> velk_plugin_host::PluginInfo {
        velk_plugin_host::PluginInfo {
            id: id.into(),
            generation,
            version: "1".into(),
            loaded_path: Default::default(),
            description: String::new(),
            metadata: velk_plugin_host::PluginMetadata {
                name: id.into(),
                ..Default::default()
            },
            settings_values: Default::default(),
            settings_error: None,
            resident_code: false,
            hooks: vec![],
            dependencies: vec![],
        }
    }
    fn invocation() -> Invoke {
        Invoke {
            plugin: "test.reducer".into(),
            generation: 1,
            command: Declaration {
                id: "test.complete".into(),
                title: "Complete".into(),
                language: None,
                input: None,
                shortcut: None,
                handler: Handler::Collect {
                    automatic: None,
                    topic: "test.completion".into(),
                    hook: "test.reduce".into(),
                },
            },
        }
    }
    #[test]
    #[allow(unsafe_code)]
    fn contributed_untitled_edits_are_undoable_and_retired_sources_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        // SAFETY: the directory has no native plugin images.
        let host = unsafe { velk_plugin_host::Runtime::start(directory.path().into()) }.unwrap();
        let mut harness = Harness::new();
        harness
            .request(velk_plugin_contracts::host::Request::NewBuffer { text: "pr".into() })
            .unwrap();
        harness.runtime.update(|cx| {
            harness.document.update(cx, |doc, cx| {
                let mut plugins =
                    super::super::plugins::State::new(host.client(), "test.markdown".into());
                plugins.loaded = vec![info("test.reducer", 1), info("test.words", 2)];
                doc.plugins = Some(plugins);
                let id = doc.buffer.id;
                let action = |revision| ChoiceAction::Contributed {
                    plugin: "test.words".into(),
                    generation: 2,
                    action: Box::new(ChoiceAction::BufferEdit {
                        id,
                        revision,
                        edits: vec![TextEdit {
                            start: Position {
                                line: 0,
                                column: 0,
                                encoding: Encoding::Utf32,
                            },
                            end: Position {
                                line: 0,
                                column: 2,
                                encoding: Encoding::Utf32,
                            },
                            text: "print".into(),
                        }],
                    }),
                };
                let guard = doc.edit_guard(cx);
                let revision = doc.buffer.editor.read(cx).buffer().revision();
                doc.apply_choice_action(action(revision), guard, cx);
                assert_eq!(
                    doc.buffer.editor.read(cx).buffer().text().to_string(),
                    "print"
                );
                doc.buffer.editor.update(cx, |editor, _| {
                    editor.execute(velk_editor_model::EditCommand::Undo);
                });
                assert_eq!(doc.buffer.editor.read(cx).buffer().text().to_string(), "pr");
                let revision = doc.buffer.editor.read(cx).buffer().revision();
                let guard = doc.edit_guard(cx);
                doc.plugins
                    .as_mut()
                    .unwrap()
                    .loaded
                    .retain(|p| p.id != "test.words");
                doc.apply_choice_action(action(revision), guard, cx);
                assert_eq!(doc.buffer.editor.read(cx).buffer().text().to_string(), "pr");
            })
        });
    }
    #[test]
    #[allow(unsafe_code)]
    fn replacing_collection_waits_for_the_cancelled_semantic_terminal_event() {
        use velk_semantic::{Event, Input, Service, Status};
        let directory = tempfile::tempdir().unwrap();
        // SAFETY: the directory has no native plugin images.
        let host = unsafe { velk_plugin_host::Runtime::start(directory.path().into()) }.unwrap();
        let mut harness = Harness::new();
        let path = harness.write_file("completion.rs", "pr");
        harness
            .request(velk_plugin_contracts::host::Request::OpenBuffer { path })
            .unwrap();
        harness.drain();
        let (service, session) = Service::external(|| {});
        harness.runtime.update(|cx| harness.document.update(cx, |doc, cx| {
            let mut plugins = super::super::plugins::State::new(host.client(), "test.markdown".into());
            let mut source = info("test.lsp", 2);
            source.metadata.contributions.push(contributions::Contribution {
                metadata: serde_json::Value::Null,
                topic: "test.completion".into(), command: Declaration {
                    handler: Handler::Semantic { payload: serde_json::Value::Null },
                    ..invocation().command
                },
            });
            plugins.loaded = vec![info("test.reducer", 1), source];
            doc.plugins = Some(plugins);
            doc.buffer.semantic = Some(super::super::semantic::State {
                service: service.clone(), revision: 0, status: Status::Ready(Default::default()), count: 0,
                diagnostics: vec![], hover: None, command: None, completion: None, target: None,
                hover_view: None, next_request: 1, task: None, render_task: None,
                renderer: std::sync::Arc::new(velk_markup::PlainText), notice: None,
            });
            doc.begin_collection(invocation(), false, cx);
            let Input::Command { request: old, .. } = session.try_next().unwrap() else { panic!() };
            let first = doc.collection.as_ref().unwrap().id;
            doc.begin_collection(invocation(), true, cx);
            let second = doc.collection.as_ref().unwrap().id;
            assert_ne!(first, second);
            assert!(matches!(session.try_next(), Some(Input::CancelCommand(request)) if request == old));
            assert_eq!(doc.buffer.semantic.as_ref().unwrap().command.as_ref().unwrap().request, old);
            session.events().publish(Event::Command { request: old, result: Err("cancelled".into()) });
            let terminal = pollster::block_on(service.events.recv()).unwrap();
            doc.semantic_event(terminal, cx);
            let Input::Command { request: current, .. } = session.try_next().unwrap() else { panic!() };
            assert_ne!(current, old);
            let pending = doc.buffer.semantic.as_ref().unwrap().command.as_ref().unwrap();
            assert_eq!(pending.collection, Some((second, 0)));
            doc.semantic_event(Event::Command { request: old, result: Ok(Outcome::Completions { items: vec![], continuation: None}) }, cx);
            assert_eq!(doc.buffer.semantic.as_ref().unwrap().command.as_ref().unwrap().request, current);
            let guard = doc.edit_guard(cx);
            doc.cancel_collection(false, cx);
            doc.apply_choice_action(ChoiceAction::Contributed {
                plugin: "test.lsp".into(), generation: 2,
                action: Box::new(ChoiceAction::Semantic { payload: serde_json::json!({"resolve": true}) }),
            }, guard, cx);
            assert!(doc.deferred_choice.is_some());
            assert!(matches!(session.try_next(), Some(Input::CancelCommand(request)) if request == current));
            session.events().publish(Event::Command { request: current, result: Err("cancelled".into()) });
            doc.semantic_event(pollster::block_on(service.events.recv()).unwrap(), cx);
            assert!(matches!(session.try_next(), Some(Input::Command { payload, .. }) if payload["resolve"] == true));
            assert!(doc.deferred_choice.is_none());
            let pending = doc.buffer.semantic.as_ref().unwrap().command.as_ref().unwrap();
            assert_eq!(pending.invocation.plugin, "test.lsp");
            assert!(pending.collection.is_none());
        }));
    }
    #[test]
    #[allow(unsafe_code)]
    fn continuations_survive_superseded_refreshes_but_not_completion_dismissal_or_generation_changes()
     {
        use velk_semantic::{Event, Input, Service, Status};
        let directory = tempfile::tempdir().unwrap();
        // SAFETY: the test directory contains no native code.
        let host = unsafe { velk_plugin_host::Runtime::start(directory.path().into()) }.unwrap();
        let mut harness = Harness::new();
        let path = harness.write_file("continuation.rs", "pr");
        harness
            .request(velk_plugin_contracts::host::Request::OpenBuffer { path })
            .unwrap();
        harness.drain();
        let (service, session) = Service::external(|| {});
        harness.runtime.update(|cx| harness.document.update(cx, |doc, cx| {
            let mut plugins = super::super::plugins::State::new(host.client(), "test.markdown".into());
            let mut source = info("test.lsp", 2);
            source.metadata.contributions.push(contributions::Contribution {
                metadata: serde_json::Value::Null, topic: "test.completion".into(),
                command: Declaration { handler: Handler::Semantic { payload: serde_json::Value::Null }, ..invocation().command },
            });
            plugins.loaded = vec![info("test.reducer", 1), source];
            doc.plugins = Some(plugins);
            doc.buffer.semantic = Some(super::super::semantic::State {
                service: service.clone(), revision: 0, status: Status::Ready(Default::default()), count: 0,
                diagnostics: vec![], hover: None, command: None, completion: None, target: None,
                hover_view: None, next_request: 1, task: None, render_task: None,
                renderer: std::sync::Arc::new(velk_markup::PlainText), notice: None,
            });
            let command = || {
                let Input::Command { request, input, .. } = session.try_next().unwrap() else { panic!() };
                (request, contributions::Input::decode(input.as_deref()).unwrap())
            };
            let complete = |doc: &mut Document, request, continuation, cx: &mut Context<'_, Document>| {
                session.events().publish(Event::Command { request, result: Ok(Outcome::Completions { items: vec![], continuation }) });
                doc.semantic_event(pollster::block_on(service.events.recv()).unwrap(), cx);
            };
            doc.begin_collection(invocation(), false, cx);
            let (first, input) = command();
            assert!(!input.refresh);
            assert!(input.continuation.is_none());
            complete(doc, first, Some(serde_json::json!("opaque")), cx);
            doc.begin_collection(invocation(), true, cx);
            let (second, input) = command();
            assert!(input.refresh);
            assert_eq!(input.continuation, Some(serde_json::json!("opaque")));
            doc.begin_collection(invocation(), true, cx);
            assert!(matches!(session.try_next(), Some(Input::CancelCommand(request)) if request == second));
            complete(doc, second, Some(serde_json::json!("stale")), cx);
            let (third, input) = command();
            assert_eq!(input.continuation, Some(serde_json::json!("opaque")));
            complete(doc, third, None, cx);
            doc.begin_collection(invocation(), true, cx);
            let (fourth, input) = command();
            assert!(input.continuation.is_none());
            complete(doc, fourth, Some(serde_json::json!("new")), cx);
            doc.plugins.as_mut().unwrap().loaded[1].generation = 3;
            doc.begin_collection(invocation(), true, cx);
            let (fifth, input) = command();
            assert!(input.continuation.is_none());
            complete(doc, fifth, Some(serde_json::json!("generation-three")), cx);
            doc.cancel_collection(true, cx);
            doc.begin_collection(invocation(), false, cx);
            let (_, input) = command();
            assert!(!input.refresh);
            assert!(input.continuation.is_none());
        }));
    }
}
