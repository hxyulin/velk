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
    pub plugin: String,
    pub generation: u64,
    pub request: velk_semantic::PositionRequest,
}
pub(super) enum ResultView {
    Locations(Vec<Location>),
    Document { title: String, text: String },
}
struct CloseResult;
impl Command for CloseResult {}

impl Document {
    fn command_available(&self, cx: &impl rxui::ReadContext) -> bool {
        !self.busy()
            && self.opening.is_none()
            && self.lifecycle.close_intent().is_none()
            && !self
                .buffer
                .as_ref()
                .is_some_and(|b| b.editor.read(cx).has_composition())
    }
    fn command_context(&self, cx: &impl rxui::ReadContext) -> CommandContext {
        let active = self.buffer.as_ref();
        CommandContext {
            path: active.and_then(|b| b.file.as_ref().map(|f| f.path().to_path_buf())),
            language: active.and_then(|b| b.file.as_ref()).and_then(|f| {
                velk_editor_widget::Language::for_path(f.path()).map(|language| match language {
                    velk_editor_widget::Language::Rust => "rust".into(),
                })
            }),
            revision: active.map(|b| b.editor.read(cx).buffer().revision()),
            position: active.map(|b| b.editor.read(cx).buffer().selection().head),
        }
    }
    pub(super) fn command_source_matches(
        &self,
        context: &CommandContext,
        cx: &impl rxui::ReadContext,
    ) -> bool {
        !self.background_buffer && self.command_context(cx) == *context
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
                            Handler::Request { .. } => true,
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
    pub(super) fn invoke_plugin_command(&mut self, invocation: Invoke, cx: &mut Context<'_, Self>) {
        if !self.command_available(cx)
            || self.plugins.as_ref().is_some_and(|p| p.task.is_some())
            || self.command_task.is_some()
            || !self.command_owner_loaded(&invocation.plugin, invocation.generation)
        {
            return;
        }
        let context = self.command_context(cx);
        if invocation.command.language.is_some() && invocation.command.language != context.language
        {
            return;
        }
        match invocation.command.handler {
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
                match state.service.command(request, payload) {
                    Ok(()) => {
                        state.command = Some(Pending {
                            plugin: invocation.plugin,
                            generation: invocation.generation,
                            request,
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
                            Ok(Ok(outcome)) => this.apply_command_outcome(outcome, cx),
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
    pub(super) fn apply_command_outcome(&mut self, outcome: Outcome, cx: &mut Context<'_, Self>) {
        if !self.command_available(cx) {
            return;
        }
        if let Err(error) = outcome.validate() {
            self.notice(Some(error));
            return;
        }
        match outcome {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rxui::Runtime;
    use velk_commands::{Encoding, Position};
    use velk_semantic::{Event, Input, Service, Status};

    fn command() -> Declaration {
        Declaration {
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
                    hover: None,
                    command: None,
                    target: None,
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
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.palette.open = false;
                d.invoke_plugin_command(invoke(), cx);
            })
        });
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
                let before = d.buffer.editor.read(cx).buffer().text().clone();
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
                assert_eq!(*d.buffer.editor.read(cx).buffer().text(), before);
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
}
