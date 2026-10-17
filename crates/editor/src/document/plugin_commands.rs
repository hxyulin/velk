//! Host presentation and dispatch for plugin-declared commands.
use super::{Document, palette};
use rxui::{Element, prelude::*};
use velk_commands::{Context as CommandContext, Declaration, Handler, Location, Outcome};

#[derive(Clone)]
pub(super) struct Invoke {
    pub plugin: String,
    pub generation: u64,
    pub command: Declaration,
}
pub(super) struct Pending {
    pub request: velk_semantic::PositionRequest,
    pub guard: super::workspace_edits::Guard,
    pub invocation: Invoke,
    pub completion_refresh: bool,
    pub completion_epoch: u64,
    pub signature_epoch: u64,
    pub cancelled: bool,
    pub collection: Option<(u64, usize)>,
    pub documentation: Option<velk_commands::completion::DocumentationTarget>,
}
pub(super) enum ResultView {
    Picker(super::picker::State),
    Locations(Vec<Location>),
    Document {
        title: String,
        text: String,
    },
    Prompt {
        invocation: Invoke,
        guard: super::workspace_edits::Guard,
        value: String,
    },
    Choices {
        title: String,
        items: Vec<velk_commands::Choice>,
        guard: super::workspace_edits::Guard,
        query: String,
        selected: usize,
    },
}
struct CloseResult;
impl Command for CloseResult {}
struct AcceptResult;
impl Command for AcceptResult {}
struct NextChoice;
impl Command for NextChoice {}
struct PreviousChoice;
impl Command for PreviousChoice {}

pub(super) struct PluginShortcut(Invoke);
impl Command for PluginShortcut {}
fn shortcut(spec: &velk_commands::Shortcut) -> rxui::Shortcut {
    let key = if spec.key == "Space" || spec.key == " " {
        KeyboardKey::Space
    } else if spec.key.starts_with('F') && spec.key.len() > 1 {
        KeyboardKey::Other(spec.key.clone())
    } else {
        KeyboardKey::Character(spec.key.to_ascii_lowercase())
    };
    Shortcut::new(
        key,
        rxui::Modifiers {
            shift: spec.shift,
            alt: spec.alt,
            control: spec.control || spec.primary && !cfg!(target_os = "macos"),
            meta: spec.primary && cfg!(target_os = "macos"),
        },
    )
}
impl Document {
    pub(super) fn command_available(&self, cx: &impl rxui::ReadContext) -> bool {
        !self.busy()
            && self.opening.is_none()
            && self.lifecycle.close_intent().is_none()
            && !self
                .buffer
                .as_ref()
                .is_some_and(|b| b.editor.read(cx).has_composition())
    }
    pub(super) fn command_context(&self, cx: &impl rxui::ReadContext) -> CommandContext {
        let active = self.buffer.as_ref();
        CommandContext {
            input: None,
            path: active.and_then(|b| b.file.as_ref().map(|f| f.path().to_path_buf())),
            language: active.and_then(|b| b.file.as_ref()).and_then(|f| {
                velk_editor_widget::Language::for_path(f.path()).map(|language| match language {
                    velk_editor_widget::Language::Rust => "rust".into(),
                })
            }),
            revision: active.map(|b| b.editor.read(cx).buffer().revision()),
            position: active.map(|b| b.editor.read(cx).caret_position()),
        }
    }
    pub(super) fn command_source_matches(
        &self,
        context: &CommandContext,
        cx: &impl rxui::ReadContext,
    ) -> bool {
        let mut source = context.clone();
        source.input = None;
        !self.background_buffer && self.command_context(cx) == source
    }
    pub(super) fn command_owner_loaded(&self, plugin: &str, generation: u64) -> bool {
        self.plugins.as_ref().is_some_and(|state| {
            state
                .loaded
                .iter()
                .any(|p| p.id == plugin && p.generation == generation)
        })
    }
    pub(super) fn plugin_command_entries(
        &self,
        cx: &impl rxui::ReadContext,
    ) -> Vec<palette::Entry> {
        let Some(plugins) = &self.plugins else {
            return Vec::new();
        };
        let context = self.command_context(cx);
        plugins
            .loaded
            .iter()
            .flat_map(|plugin| {
                plugin
                    .metadata
                    .commands
                    .iter()
                    .filter(|command| {
                        command.language.is_none() || command.language == context.language
                    })
                    .map(|command| {
                        let ready = match &command.handler {
                            Handler::Request { .. } | Handler::Picker { .. } => true,
                            Handler::Collect { .. } => self.has_buffers(),
                            Handler::Semantic { .. } => self
                                .buffer
                                .as_ref()
                                .and_then(|b| b.semantic.as_ref())
                                .is_some_and(|s| {
                                    matches!(s.status, velk_semantic::Status::Ready(_))
                                        && s.command.is_none()
                                }),
                        };
                        palette::Entry::plugin(
                            Invoke {
                                plugin: plugin.id.clone(),
                                generation: plugin.generation,
                                command: command.clone(),
                            },
                            ready
                                && self.command_available(cx)
                                && plugins.task.is_none()
                                && self.command_task.is_none(),
                        )
                    })
            })
            .collect()
    }
    pub(super) fn plugin_shortcuts(
        &self,
        cx: &mut ViewContext<'_, Self>,
    ) -> Vec<rxui::CommandAction<PluginShortcut>> {
        let Some(plugins) = &self.plugins else {
            return Vec::new();
        };
        let language = self.command_context(cx).language;
        let mut seen = std::collections::BTreeSet::new();
        let mut actions = Vec::new();
        for plugin in &plugins.loaded {
            for command in &plugin.metadata.commands {
                let Some(spec) = &command.shortcut else {
                    continue;
                };
                if command.language.is_some() && command.language != language {
                    continue;
                }
                if !seen.insert(format!("{:?}", shortcut(spec))) {
                    continue;
                }
                let enabled = self.command_available(cx)
                    && plugins.task.is_none()
                    && self.command_task.is_none()
                    && match &command.handler {
                        Handler::Request { .. } | Handler::Picker { .. } => true,
                        Handler::Collect { .. } => self.has_buffers(),
                        Handler::Semantic { .. } => self
                            .buffer
                            .as_ref()
                            .and_then(|b| b.semantic.as_ref())
                            .is_some_and(|s| {
                                matches!(s.status, velk_semantic::Status::Ready(_))
                                    && s.command.is_none()
                            }),
                    };
                let invocation = Invoke {
                    plugin: plugin.id.clone(),
                    generation: plugin.generation,
                    command: command.clone(),
                };
                let mut action = cx
                    .command(PluginShortcut(invocation), |this, command, cx| {
                        this.invoke_plugin_command(command.0.clone(), cx)
                    })
                    .label(command.title.clone())
                    .enabled(enabled)
                    .shortcut(shortcut(spec));
                if spec.key.starts_with('F') && spec.key.len() > 1 {
                    let mut portable = shortcut(spec);
                    portable.key = KeyboardKey::Other(format!("Named({})", spec.key));
                    action = action.shortcut(portable);
                }
                actions.push(action);
            }
        }
        actions
    }
    pub(super) fn invoke_plugin_command(&mut self, invocation: Invoke, cx: &mut Context<'_, Self>) {
        self.invoke_plugin_command_with_input(invocation, None, cx);
    }
    pub(super) fn invoke_plugin_command_with_input(
        &mut self,
        invocation: Invoke,
        input: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.command_available(cx)
            || self.plugins.as_ref().is_some_and(|p| p.task.is_some())
            || self.command_task.is_some()
            || !self.command_owner_loaded(&invocation.plugin, invocation.generation)
        {
            return;
        }
        self.deferred_choice = None;
        self.cancel_collection(true, cx);
        let mut guard = self.edit_guard(cx);
        guard.owner = Some((invocation.plugin.clone(), invocation.generation));
        if matches!(invocation.command.handler, Handler::Picker { .. }) {
            if input.is_some() {
                self.notice(Some("Picker commands do not accept fixed input".into()));
                return;
            }
            self.open_picker(invocation, guard, cx);
            return;
        }
        if invocation.command.input.is_some() && input.is_none() {
            self.command_result = Some(ResultView::Prompt {
                invocation,
                guard,
                value: String::new(),
            });
            return;
        }
        self.execute_plugin_command(invocation, input, guard, cx);
    }
    pub(super) fn execute_plugin_command(
        &mut self,
        invocation: Invoke,
        input: Option<String>,
        guard: super::workspace_edits::Guard,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.command_available(cx)
            || self.command_task.is_some()
            || self.plugins.as_ref().is_some_and(|p| p.task.is_some())
            || !self.guard_valid(&guard, cx)
        {
            self.notice(Some(
                "Command source changed or its provider is busy".into(),
            ));
            return;
        }
        let mut context = self.command_context(cx);
        context.input = input;
        if invocation.command.language.is_some() && invocation.command.language != context.language
        {
            return;
        }
        let continuation = invocation.clone();
        let completion_epoch = self
            .buffer
            .as_ref()
            .map(|buffer| buffer.editor.read(cx).completion_epoch())
            .unwrap_or(0);
        let signature_epoch = self
            .buffer
            .as_ref()
            .map(|buffer| buffer.editor.read(cx).signature_epoch())
            .unwrap_or(0);
        match invocation.command.handler {
            Handler::Collect { .. } => self.begin_collection(continuation, false, cx),
            Handler::Picker { .. } => unreachable!("pickers use their own live query flow"),
            Handler::Semantic { payload } => {
                let Some(state) = self.buffer.as_mut().and_then(|b| b.semantic.as_mut()) else {
                    return;
                };
                if state.command.is_some()
                    || !matches!(state.status, velk_semantic::Status::Ready(_))
                {
                    return;
                }
                let request = velk_semantic::PositionRequest {
                    id: state.next_request,
                    revision: context.revision.unwrap(),
                    position: context.position.unwrap(),
                };
                state.next_request += 1;
                match state.service.command(request, payload, context.input) {
                    Ok(()) => {
                        state.command = Some(Pending {
                            request,
                            guard,
                            invocation: continuation,
                            completion_refresh: false,
                            completion_epoch,
                            signature_epoch,
                            cancelled: false,
                            collection: None,
                            documentation: None,
                        })
                    }
                    Err(error) => self.notice(Some(error)),
                }
            }
            Handler::Request { hook } => {
                let client = self.plugins.as_ref().unwrap().client.clone();
                let owner = invocation.plugin.clone();
                let generation = invocation.generation;
                let source = context.clone();
                let source_id = self.buffer.as_ref().map(|b| b.id);
                let result = cx.try_spawn(
                    async move {
                        let handle = client
                            .bind_hook(owner.clone(), hook)?
                            .recv()
                            .await
                            .map_err(|e| e.to_string())??;
                        if handle.generation() != generation {
                            return Err("command provider was replaced".into());
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
                        this.command_task = None;
                        if this.buffer.as_ref().map(|b| b.id) != source_id
                            || !this.command_source_matches(&source, cx)
                            || !this.command_owner_loaded(&invocation.plugin, generation)
                        {
                            return;
                        }
                        match result {
                            Ok(Ok(outcome)) => this.apply_guarded_outcome(outcome, guard, cx),
                            Ok(Err(error)) => this.notice(Some(error)),
                            Err(error) => this.notice(Some(error.to_string())),
                        }
                    },
                );
                match result {
                    Ok(task) => self.command_task = Some(task),
                    Err(error) => self.notice(Some(error.to_string())),
                }
            }
        }
    }
    #[cfg(test)]
    pub(super) fn apply_command_outcome(&mut self, outcome: Outcome, cx: &mut Context<'_, Self>) {
        self.apply_guarded_outcome(outcome, self.edit_guard(cx), cx);
    }
    pub(super) fn apply_guarded_outcome(
        &mut self,
        outcome: Outcome,
        guard: super::workspace_edits::Guard,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.command_available(cx) || !self.guard_valid(&guard, cx) {
            return;
        }
        if let Err(error) = outcome.validate() {
            self.notice(Some(error));
            return;
        }
        match outcome {
            Outcome::SignatureHelp { help } => {
                if let Some(buffer) = self.buffer.as_ref() {
                    buffer
                        .editor
                        .update(cx, |editor, _| editor.show_signature(help));
                }
            }
            Outcome::RevealDiagnostic { location } => self.reveal_diagnostic(location, cx),
            Outcome::RevealBuffer { location } => self.reveal_plugin_buffer(location, cx),
            Outcome::ShowPanel { panel } => {
                if let Some((provider, _)) = guard.owner {
                    self.show_plugin_panel(&provider, &panel, cx);
                }
            }
            Outcome::Edit { edit } => self.apply_workspace_edit(edit, guard, cx),
            Outcome::Snippet { snippet } => self.apply_snippet_template(snippet, guard, cx),
            Outcome::Completions { .. } => {
                self.notice(Some("Completion results require a semantic session".into()));
            }
            Outcome::Choices { title, items } => {
                if items.is_empty() {
                    self.notice(Some("No supported results at the caret".into()));
                } else {
                    self.command_result = Some(ResultView::Choices {
                        title,
                        items,
                        guard,
                        query: String::new(),
                        selected: 0,
                    });
                }
            }
            Outcome::Message { text } => self.notice(Some(text)),
            Outcome::Locations { mut locations } => match locations.len() {
                0 => self.notice(Some("No navigation targets found".into())),
                1 => self.open_location(locations.remove(0), cx),
                _ => self.command_result = Some(ResultView::Locations(locations)),
            },
            Outcome::Document { title, text, .. } => {
                self.command_result = Some(ResultView::Document { title, text })
            }
        }
    }
    fn accept_result(&mut self, cx: &mut Context<'_, Self>) {
        let Some(result) = self.command_result.take() else {
            return;
        };
        match result {
            ResultView::Picker(state) => {
                self.accept_picker(state, cx);
            }
            ResultView::Prompt {
                invocation,
                guard,
                value,
            } => {
                if value.trim().is_empty() || value.len() > 1024 {
                    self.notice(Some("Enter a name of at most 1024 bytes".into()));
                    return;
                }
                self.execute_plugin_command(invocation, Some(value), guard, cx);
            }
            ResultView::Choices {
                items,
                guard,
                query,
                selected,
                ..
            } => {
                let Some(index) = choice_indices(&items, &query).get(selected).copied() else {
                    return;
                };
                if !self.guard_valid(&guard, cx) {
                    self.notice(Some("Result source changed".into()));
                    return;
                }
                self.apply_choice_action(items[index].action.clone(), guard, cx);
            }
            _ => {}
        }
        self.restore_focus_pending = true;
    }
    pub(super) fn command_result_modal(&self, cx: &mut ViewContext<'_, Self>) -> Option<Element> {
        let result = self.command_result.as_ref()?;
        let close = cx
            .command(CloseResult, |this, _, cx| {
                this.command_result = None;
                let _ = this.editor_focus.focus(cx);
            })
            .shortcut(Shortcut::new(KeyboardKey::Escape, Default::default()));
        let mut content = column().gap(8.).padding(12.).on_command(close.clone());
        match result {
            ResultView::Picker(state) => {
                return Some(self.picker_modal(state, cx));
            }
            ResultView::Prompt {
                invocation, value, ..
            } => {
                let accept = cx
                    .command(AcceptResult, |this, _, cx| this.accept_result(cx))
                    .enabled(!value.trim().is_empty())
                    .shortcut(Shortcut::new(KeyboardKey::Enter, Default::default()));
                content = content
                    .child(label(invocation.command.title.clone()))
                    .child(
                        text_input(value.clone())
                            .key("command-input")
                            .placeholder(invocation.command.input.clone().unwrap_or_default())
                            .on_command(accept)
                            .on_change(cx.listener(|this, event: &TextChangeEvent, _| {
                                if let Some(ResultView::Prompt { value, .. }) =
                                    &mut this.command_result
                                {
                                    *value = event.value.clone();
                                }
                            })),
                    );
            }
            ResultView::Choices {
                title,
                items,
                query,
                selected,
                ..
            } => {
                let indices = choice_indices(items, query);
                let count = indices.len();
                let selected = (*selected).min(count.saturating_sub(1));
                let accept = cx
                    .command(AcceptResult, |this, _, cx| this.accept_result(cx))
                    .enabled(count > 0)
                    .shortcut(Shortcut::new(KeyboardKey::Enter, Default::default()));
                let next = cx
                    .command(NextChoice, move |this, _, _| {
                        if let Some(ResultView::Choices { selected, .. }) = &mut this.command_result
                        {
                            *selected = if count == 0 {
                                0
                            } else {
                                (*selected + 1) % count
                            };
                        }
                    })
                    .shortcut(Shortcut::new(KeyboardKey::ArrowDown, Default::default()));
                let previous = cx
                    .command(PreviousChoice, move |this, _, _| {
                        if let Some(ResultView::Choices { selected, .. }) = &mut this.command_result
                        {
                            *selected = if count == 0 {
                                0
                            } else {
                                (*selected + count - 1) % count
                            };
                        }
                    })
                    .shortcut(Shortcut::new(KeyboardKey::ArrowUp, Default::default()));
                content = content
                    .on_command(next)
                    .on_command(previous)
                    .child(label(format!("{title} ({count})")))
                    .child(
                        text_input(query.clone())
                            .key("command-choice-query")
                            .placeholder("Filter results")
                            .on_command(accept)
                            .on_change(cx.listener(|this, event: &TextChangeEvent, _| {
                                if let Some(ResultView::Choices {
                                    query, selected, ..
                                }) = &mut this.command_result
                                {
                                    *query = event.value.clone();
                                    *selected = 0;
                                }
                            })),
                    );
                for (position, index) in indices
                    .into_iter()
                    .enumerate()
                    .skip(selected.saturating_sub(4))
                    .take(9)
                {
                    let item = &items[index];
                    let caption = match &item.detail {
                        Some(detail) => format!("{} · {}", item.title, detail),
                        None => item.title.clone(),
                    };
                    content = content.child(
                        button(caption)
                            .key(format!("command-choice-{index}"))
                            .background(if position == selected {
                                ThemeColor::Selection
                            } else {
                                ThemeColor::Control
                            })
                            .on_click(cx.listener(move |this, _, cx| {
                                if let Some(ResultView::Choices { selected, .. }) =
                                    &mut this.command_result
                                {
                                    *selected = position;
                                }
                                this.accept_result(cx);
                            })),
                    );
                }
            }
            ResultView::Locations(locations) => {
                content = content.child(label(format!("{} navigation targets", locations.len())));
                for (index, location) in locations.iter().enumerate() {
                    let location = location.clone();
                    content = content.child(
                        button(format!(
                            "{}:{}:{}",
                            location.path.display(),
                            u64::from(location.position.line) + 1,
                            u64::from(location.position.column) + 1
                        ))
                        .key(format!("command-target-{index}"))
                        .on_click(cx.listener(move |this, _, cx| {
                            this.command_result = None;
                            this.open_location(location.clone(), cx);
                        })),
                    );
                }
            }
            ResultView::Document { title, text } => {
                content = content
                    .child(label(title.clone()))
                    .child(rich_text([rxui::span(text.clone()).monospace()]).fill_width());
            }
        }
        content = content.child(button("Close").on_click(cx.listener(|this, _, cx| {
            this.command_result = None;
            let _ = this.editor_focus.focus(cx);
        })));
        Some(
            modal(content.scroll_y().max_height(520.))
                .key("command-result")
                .width(720.)
                .on_dismiss(cx.listener(|this, _: &DismissEvent, _| this.command_result = None))
                .into_element(),
        )
    }
}

fn choice_indices(items: &[velk_commands::Choice], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item.title.to_lowercase().contains(&query)
                || item
                    .detail
                    .as_ref()
                    .is_some_and(|d| d.to_lowercase().contains(&query))
        })
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxui::Runtime;
    use velk_commands::{Encoding, Position};
    use velk_semantic::{Event, Input, Service, Status};

    fn command() -> Declaration {
        Declaration {
            input: None,
            shortcut: Some(velk_commands::Shortcut {
                key: "F12".into(),
                ..Default::default()
            }),
            id: "test.definition".into(),
            title: "Go to test definition".into(),
            language: Some("rust".into()),
            handler: Handler::Semantic {
                payload: serde_json::json!({"operation":"definition"}),
            },
        }
    }
    fn invoke() -> Invoke {
        Invoke {
            plugin: "test.commands".into(),
            generation: 1,
            command: command(),
        }
    }
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
    #[allow(unsafe_code)]
    fn plugin_palette_navigation_stale_results_and_result_views() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.rs");
        let target = dir.path().join("target.rs");
        std::fs::write(&source, "source").unwrap();
        std::fs::write(&target, "a𐐀界 target").unwrap();
        // SAFETY: this test directory contains no plugin images.
        let host = unsafe { velk_plugin_host::Runtime::start(dir.path().join("plugins")) }.unwrap();
        let (service, session) = Service::external(|| {});
        let release = |request| {
            session.events().publish(Event::Command {
                request,
                result: Ok(Outcome::Message {
                    text: String::new(),
                }),
            });
            let _ = pollster::block_on(service.events.recv()).unwrap();
        };
        let mut runtime = Runtime::new();
        let executor = std::sync::Arc::new(super::super::tests::QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = runtime.update(|cx| {
            let (file, editor) = crate::file::FileState::open(&source).unwrap();
            let editor = cx.new(|_| editor);
            cx.new(|cx| {
                let mut document = Document::new(editor, file, source.clone(), cx);
                let mut plugins = super::super::plugins::State::new(
                    host.client(),
                    velk_plugin_host::DEFAULT_MARKDOWN_PROVIDER.into(),
                );
                plugins.loaded.push(velk_plugin_host::PluginInfo {
                    dependencies: Vec::new(),
                    id: "test.commands".into(),
                    version: "1".into(),
                    generation: 1,
                    loaded_path: dir.path().into(),
                    description: String::new(),
                    metadata: velk_plugin_host::PluginMetadata {
                        name: "Commands".into(),
                        commands: vec![command()],
                        ..Default::default()
                    },
                    settings_values: Default::default(),
                    settings_error: None,
                    resident_code: false,
                    hooks: vec![],
                });
                document.plugins = Some(plugins);
                document.buffer.semantic = Some(super::super::semantic::State {
                    service: service.clone(),
                    revision: 0,
                    status: Status::Ready(Default::default()),
                    count: 0,
                    diagnostics: Vec::new(),
                    hover: None,
                    command: None,
                    completion: None,
                    signature: None,
                    target: None,
                    hover_view: None,
                    next_request: 1,
                    task: None,
                    render_task: None,
                    renderer: std::sync::Arc::new(velk_markup::PlainText),
                    notice: None,
                });
                document.palette.open = true;
                document.palette.query = "Go to test definition".into();
                document
            })
        });
        let mut ui = rxui::Ui::new(&mut runtime, document.clone()).unwrap();
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        assert!(
            ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("test.commands:test.definition")))
        );
        runtime.update(|cx| document.update(cx, |d, _| d.palette.open = false));
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        let surface = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        ui.focus(surface);
        ui.key(
            &mut runtime,
            rxui::KeyEvent {
                key: KeyboardKey::Other("F12".into()),
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
        )
        .unwrap();
        let Some(Input::Command { request, .. }) = session.try_next() else {
            panic!("command was not dispatched");
        };
        release(request);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.semantic_event(
                    Event::Command {
                        request,
                        result: Ok(Outcome::Locations {
                            locations: vec![Location {
                                path: target.clone(),
                                position: Position {
                                    line: 0,
                                    column: 4,
                                    encoding: Encoding::Utf16,
                                },
                            }],
                        }),
                    },
                    cx,
                );
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                assert_eq!(d.buffer.editor.read(cx).buffer().selection().head, 3);
                let target_id = d.buffer.id;
                d.navigate_history(false, cx);
                assert_eq!(d.buffer.id, 1);
                d.navigate_history(true, cx);
                assert_eq!(d.buffer.id, target_id);
                assert_eq!(d.buffer.editor.read(cx).buffer().selection().head, 3);
                d.buffer
                    .editor
                    .update(cx, |editor, _| editor.insert("unsaved"));
                d.open_location(
                    Location {
                        path: target.clone(),
                        position: Position {
                            line: 0,
                            column: 1,
                            encoding: Encoding::Utf16,
                        },
                    },
                    cx,
                );
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                assert_eq!(
                    d.buffer.file.as_ref().unwrap().path(),
                    std::fs::canonicalize(&target).unwrap()
                );
                assert_eq!(d.buffer.editor.read(cx).buffer().selection().head, 1);
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "a𐐀界unsaved target"
                );
                assert!(d.buffer.dirty);
                assert_eq!(d.ids().len(), 2);
                d.select_buffer(1, cx);
                d.invoke_plugin_command(invoke(), cx);
            })
        });
        let Some(Input::Command { request, .. }) = session.try_next() else {
            panic!("missing second command");
        };
        release(request);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.buffer
                    .editor
                    .update(cx, |editor, _| editor.insert("changed"));
                d.semantic_event(
                    Event::Command {
                        request,
                        result: Ok(Outcome::Message {
                            text: "stale".into(),
                        }),
                    },
                    cx,
                );
                assert_ne!(d.buffer.status.as_deref(), Some("stale"));
                d.invoke_plugin_command(invoke(), cx);
            })
        });
        let Some(Input::Command { request, .. }) = std::iter::from_fn(|| session.try_next())
            .find(|input| matches!(input, Input::Command { .. }))
        else {
            panic!("missing third command");
        };
        release(request);
        {
            runtime.update(|cx| {
                document.update(cx, |d, cx| {
                    d.plugins.as_mut().unwrap().loaded.clear();
                    d.semantic_event(
                        Event::Command {
                            request,
                            result: Ok(Outcome::Message {
                                text: "retired".into(),
                            }),
                        },
                        cx,
                    );
                    assert_ne!(d.buffer.status.as_deref(), Some("retired"));
                })
            });
        }
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.apply_command_outcome(
                    Outcome::Locations {
                        locations: vec![
                            Location {
                                path: source.clone(),
                                position: Position {
                                    line: 0,
                                    column: 0,
                                    encoding: Encoding::Utf32
                                }
                            };
                            2
                        ],
                    },
                    cx,
                );
                assert!(matches!(d.command_result, Some(ResultView::Locations(_))));
                let before = d.buffer.editor.read(cx).buffer().text();
                d.apply_command_outcome(
                    Outcome::Document {
                        title: "Expansion".into(),
                        text: "fn expanded() {}".into(),
                        language: Some("rust".into()),
                    },
                    cx,
                );
                assert!(matches!(
                    d.command_result,
                    Some(ResultView::Document { .. })
                ));
                assert_eq!(d.buffer.editor.read(cx).buffer().text(), before);
                assert!(d.plugin_command_entries(cx).is_empty());
            })
        });
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        assert!(
            ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("command-result")))
        );
    }
    #[test]
    #[allow(unsafe_code)]
    fn completion_popup_refreshes_latest_source_accepts_edits_and_rejects_dismissed_results() {
        use rxui::{KeyEvent, Ui};
        let mut harness = super::super::tests::Harness::new();
        let path = harness.write_file("completion.rs", "pr");
        harness
            .request(velk_plugin_contracts::host::Request::OpenBuffer { path: path.clone() })
            .unwrap();
        harness.drain();
        // SAFETY: this directory contains no plugin images.
        let directory = tempfile::tempdir().unwrap();
        let host =
            unsafe { velk_plugin_host::Runtime::start(directory.path().to_path_buf()) }.unwrap();
        let (service, session) = Service::external(|| {});
        harness.runtime.update(|cx| {
            harness.document.update(cx, |document, cx| {
                let mut plugins = super::super::plugins::State::new(
                    host.client(),
                    velk_plugin_host::DEFAULT_MARKDOWN_PROVIDER.into(),
                );
                plugins.loaded.push(velk_plugin_host::PluginInfo {
                    id: "test.commands".into(),
                    version: "1".into(),
                    generation: 1,
                    loaded_path: directory.path().into(),
                    description: String::new(),
                    dependencies: Vec::new(),
                    metadata: Default::default(),
                    settings_values: Default::default(),
                    settings_error: None,
                    resident_code: false,
                    hooks: vec![],
                });
                document.plugins = Some(plugins);
                document.buffer.editor.update(cx, |editor, _| {
                    editor.select_range(2..2);
                });
                document.buffer.semantic = Some(super::super::semantic::State {
                    service: service.clone(),
                    revision: 0,
                    status: Status::Ready(Default::default()),
                    count: 0,
                    diagnostics: Vec::new(),
                    hover: None,
                    command: None,
                    completion: None,
                    signature: None,
                    target: None,
                    hover_view: None,
                    next_request: 1,
                    task: None,
                    render_task: None,
                    renderer: std::sync::Arc::new(velk_markup::PlainText),
                    notice: None,
                });
                document.invoke_plugin_command(invoke(), cx);
            })
        });
        let next_request = || loop {
            match session.try_next().expect("completion request queued") {
                Input::Command { request, .. } => break request,
                Input::Document(_) | Input::CancelHover => {}
                other => panic!("unexpected completion input: {other:?}"),
            }
        };
        let reply = |request: velk_semantic::PositionRequest| {
            session.events().publish(Event::Command {
                request,
                result: Ok(Outcome::Completions {
                    items: vec![
                        velk_commands::Choice {
                            title: "print".into(),
                            detail: Some("Print a value".into()),
                            action: velk_commands::ChoiceAction::Edit {
                                edit: velk_commands::WorkspaceEdit {
                                    documents: vec![velk_commands::DocumentEdit {
                                        path: path.clone(),
                                        revision: Some(request.revision),
                                        edits: vec![velk_commands::TextEdit {
                                            start: Position {
                                                line: 0,
                                                column: 0,
                                                encoding: Encoding::Utf32,
                                            },
                                            end: Position {
                                                line: 0,
                                                column: request.position as u32,
                                                encoding: Encoding::Utf32,
                                            },
                                            text: "print".into(),
                                        }],
                                    }],
                                },
                            },
                        }
                        .into(),
                    ],
                    continuation: None,
                }),
            });
            pollster::block_on(service.events.recv()).unwrap()
        };
        let event = reply(next_request());
        harness.runtime.update(|cx| {
            harness.document.update(cx, |d, cx| {
                d.buffer.status = Some("No result at the caret".into());
                d.semantic_event(event, cx);
                assert!(d.buffer.status.is_none());
            })
        });
        harness.runtime.update(|cx| {
            harness.document.update(cx, |d, cx| {
                assert!(d.command_result.is_none());
                assert!(d.buffer.editor.read(cx).completion_active());
                d.buffer.editor.update(cx, |editor, _| editor.insert("i"));
            })
        });
        harness.runtime.flush().unwrap();
        let old_request = next_request();
        harness.runtime.update(|cx| {
            harness.document.update(cx, |d, cx| {
                d.buffer.editor.update(cx, |editor, _| editor.insert("n"))
            })
        });
        harness.runtime.flush().unwrap();
        let event = reply(old_request);
        harness.runtime.update(|cx| {
            harness
                .document
                .update(cx, |d, cx| d.semantic_event(event, cx))
        });
        harness.runtime.flush().unwrap();
        let newest = next_request();
        assert!(newest.revision > old_request.revision);
        assert_eq!(newest.position, 4);
        let event = reply(newest);
        harness.runtime.update(|cx| {
            harness
                .document
                .update(cx, |d, cx| d.semantic_event(event, cx))
        });
        let mut ui = Ui::new(&mut harness.runtime, harness.document.clone()).unwrap();
        ui.prepare(&mut harness.runtime, [900., 600.], &mut Measure)
            .unwrap();
        assert!(
            ui.elements()
                .any(|element| element.key == Some(&rxui::Key::from("editor-completions")))
        );
        let source = ui
            .elements()
            .find(|element| element.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        ui.focus(source);
        ui.key(
            &mut harness.runtime,
            KeyEvent {
                key: KeyboardKey::Enter,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
        )
        .unwrap();
        harness.drain();
        harness.runtime.update(|cx| {
            harness.document.update(cx, |d, cx| {
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "print"
                );
                assert!(!d.buffer.editor.read(cx).completion_active());
                d.invoke_plugin_command(invoke(), cx);
                d.buffer
                    .editor
                    .update(cx, |editor, _| editor.dismiss_completions());
            })
        });
        let event = reply(next_request());
        harness.runtime.update(|cx| {
            harness.document.update(cx, |d, cx| {
                d.semantic_event(event, cx);
                assert!(!d.buffer.editor.read(cx).completion_active());
                d.invoke_plugin_command(invoke(), cx);
            })
        });
        let request = loop {
            match session.try_next().unwrap() {
                Input::CancelCommand(_) | Input::Document(_) => {}
                Input::Command { request, .. } => break request,
                other => panic!("unexpected input: {other:?}"),
            }
        };
        harness.runtime.update(|cx| {
            harness.document.update(cx, |d, cx| {
                d.split_editor(velk_editor_model::views::Axis::Horizontal, cx)
            })
        });
        let event = reply(request);
        harness.runtime.update(|cx| {
            harness.document.update(cx, |d, cx| {
                d.semantic_event(event, cx);
                assert!(!d.buffer.editor.read(cx).completion_active());
            })
        });
    }
}

#[cfg(test)]
mod edit_interaction_tests {
    use super::*;
    use rxui::{KeyEvent, Runtime, TextInputEvent, Ui};
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
    fn enter(ui: &mut Ui<Document>, runtime: &mut Runtime) {
        ui.key(
            runtime,
            KeyEvent {
                key: KeyboardKey::Enter,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
        )
        .unwrap();
    }
    #[test]
    fn result_picker_filters_accepts_edits_and_rejects_a_stale_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.txt");
        std::fs::write(&path, "old").unwrap();
        let mut runtime = Runtime::new();
        let executor = std::sync::Arc::new(super::super::tests::QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = runtime.update(|cx| {
            let (file, editor) = crate::file::FileState::open(&path).unwrap();
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, path.clone(), cx))
        });
        let choices = |revision| {
            let position = |column| velk_commands::Position {
                line: 0,
                column,
                encoding: velk_commands::Encoding::Utf16,
            };
            Outcome::Choices {
                title: "Completion".into(),
                items: ["first", "second"]
                    .into_iter()
                    .map(|text| velk_commands::Choice {
                        title: text.into(),
                        detail: None,
                        action: velk_commands::ChoiceAction::Edit {
                            edit: velk_commands::WorkspaceEdit {
                                documents: vec![velk_commands::DocumentEdit {
                                    path: path.clone(),
                                    revision: Some(revision),
                                    edits: vec![velk_commands::TextEdit {
                                        start: position(0),
                                        end: position(3),
                                        text: text.into(),
                                    }],
                                }],
                            },
                        },
                    })
                    .collect(),
            }
        };
        runtime.update(|cx| document.update(cx, |d, cx| d.apply_command_outcome(choices(0), cx)));
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        let query = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("command-choice-query")))
            .unwrap()
            .id;
        ui.focus(query);
        ui.text_input(
            &mut runtime,
            TextInputEvent::Insert("second".into()),
            &mut Measure,
        )
        .unwrap();
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        enter(&mut ui, &mut runtime);
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "second"
                );
                d.buffer.editor.update(cx, |e, _| {
                    e.execute(velk_editor_model::EditCommand::Undo);
                });
                assert_eq!(d.buffer.editor.read(cx).buffer().text().to_string(), "old");
                let revision = d.buffer.editor.read(cx).buffer().revision();
                d.apply_command_outcome(choices(revision), cx);
                d.buffer.editor.update(cx, |e, _| e.insert("changed"));
                d.accept_result(cx);
                assert!(d.workspace_edit.is_none());
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "changedold"
                );
            })
        });
    }
    #[test]
    #[allow(unsafe_code)]
    fn declared_rename_shortcut_prompts_and_passes_input_to_semantic_service() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.rs");
        std::fs::write(&path, "fn old() {}").unwrap();
        // SAFETY: this directory contains no plugin images.
        let host = unsafe { velk_plugin_host::Runtime::start(dir.path().join("plugins")) }.unwrap();
        let (service, session) = velk_semantic::Service::external(|| {});
        let mut runtime = Runtime::new();
        let document = runtime.update(|cx| {
            let (file, editor) = crate::file::FileState::open(&path).unwrap();
            let editor = cx.new(|_| editor);
            cx.new(|cx| {
                let mut d = Document::new(editor, file, path.clone(), cx);
                let declaration = Declaration {
                    id: "test.rename".into(),
                    title: "Rename test symbol".into(),
                    input: Some("New name".into()),
                    shortcut: Some(velk_commands::Shortcut {
                        key: "F2".into(),
                        ..Default::default()
                    }),
                    language: Some("rust".into()),
                    handler: Handler::Semantic {
                        payload: serde_json::json!({"operation":"rename"}),
                    },
                };
                let mut plugins = super::super::plugins::State::new(
                    host.client(),
                    velk_plugin_host::DEFAULT_MARKDOWN_PROVIDER.into(),
                );
                plugins.loaded.push(velk_plugin_host::PluginInfo {
                    dependencies: Vec::new(),
                    id: "test.commands".into(),
                    version: "1".into(),
                    generation: 1,
                    loaded_path: dir.path().into(),
                    description: String::new(),
                    metadata: velk_plugin_host::PluginMetadata {
                        name: "Commands".into(),
                        commands: vec![
                            declaration,
                            Declaration {
                                id: "test.complete".into(),
                                title: "Complete".into(),
                                input: None,
                                shortcut: Some(velk_commands::Shortcut {
                                    key: "Space".into(),
                                    control: true,
                                    ..Default::default()
                                }),
                                language: Some("rust".into()),
                                handler: Handler::Semantic {
                                    payload: serde_json::json!({"operation":"complete"}),
                                },
                            },
                        ],
                        ..Default::default()
                    },
                    settings_values: Default::default(),
                    settings_error: None,
                    resident_code: false,
                    hooks: vec![],
                });
                d.plugins = Some(plugins);
                d.buffer.semantic = Some(super::super::semantic::State {
                    service: service.clone(),
                    revision: 0,
                    status: velk_semantic::Status::Ready(Default::default()),
                    count: 0,
                    diagnostics: Vec::new(),
                    hover: None,
                    command: None,
                    completion: None,
                    signature: None,
                    target: None,
                    hover_view: None,
                    next_request: 1,
                    task: None,
                    render_task: None,
                    renderer: std::sync::Arc::new(velk_markup::PlainText),
                    notice: None,
                });
                d
            })
        });
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        let source = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        ui.focus(source);
        ui.key(
            &mut runtime,
            KeyEvent {
                key: KeyboardKey::Other("F2".into()),
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
        )
        .unwrap();
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        assert!(session.try_next().is_none());
        let input = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("command-input")))
            .unwrap()
            .id;
        ui.focus(input);
        ui.text_input(
            &mut runtime,
            TextInputEvent::Insert("renamed".into()),
            &mut Measure,
        )
        .unwrap();
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        enter(&mut ui, &mut runtime);
        let Some(velk_semantic::Input::Command {
            request,
            input: Some(input),
            ..
        }) = session.try_next()
        else {
            panic!("rename was not dispatched")
        };
        assert_eq!(input, "renamed");
        session.events().publish(velk_semantic::Event::Command {
            request,
            result: Ok(Outcome::Message {
                text: String::new(),
            }),
        });
        let _ = pollster::block_on(service.events.recv()).unwrap();
        runtime.update(|cx| {
            document.update(cx, |d, _| {
                d.buffer.semantic.as_mut().unwrap().command = None
            })
        });
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        let source = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        ui.focus(source);
        ui.key(
            &mut runtime,
            KeyEvent {
                key: KeyboardKey::Space,
                pressed: true,
                repeat: false,
                modifiers: rxui::Modifiers {
                    control: true,
                    ..Default::default()
                },
            },
        )
        .unwrap();
        let completion = session.try_next();
        assert!(
            matches!(&completion, Some(velk_semantic::Input::Command {input:None,payload,..}) if payload["operation"]=="complete"),
            "unexpected input: {completion:?}"
        );
        runtime.update(|cx| {
            assert_eq!(
                document
                    .read(cx)
                    .buffer
                    .editor
                    .read(cx)
                    .buffer()
                    .text()
                    .to_string(),
                "fn old() {}"
            )
        });
        session.events().publish(velk_semantic::Event::Command {
            request: runtime.update(|cx| {
                document
                    .read(cx)
                    .buffer
                    .semantic
                    .as_ref()
                    .unwrap()
                    .command
                    .as_ref()
                    .unwrap()
                    .request
            }),
            result: Ok(Outcome::Message {
                text: String::new(),
            }),
        });
        let _ = pollster::block_on(service.events.recv()).unwrap();
        runtime.update(|cx| {
            document.update(cx, |d, _| {
                d.buffer.semantic.as_mut().unwrap().command = None
            })
        });
        runtime.update(|cx| document.update(cx, |d, cx| {
            d.keybindings.bindings = velk_commands::keybindings::Bindings::parse(
                br#"{"bindings":[{"keys":"F9","command":"test.commands:test.rename","input":"fixed-name"}]}"#
            ).unwrap();
            d.sync_buffer_keybindings(cx);
        }));
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        ui.key(
            &mut runtime,
            KeyEvent {
                key: KeyboardKey::Other("F9".into()),
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
        )
        .unwrap();
        assert!(
            matches!(session.try_next(), Some(velk_semantic::Input::Command { input: Some(input), .. }) if input == "fixed-name")
        );
        runtime.update(|cx| {
            document.update(cx, |d, _| {
                assert!(
                    d.command_result.is_none(),
                    "fixed input must bypass the prompt"
                );
                d.buffer.semantic.as_mut().unwrap().command = None;
            })
        });
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        let source = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        ui.focus(source);
    }
}
