//! Selected-item documentation shares semantic scheduling, never edit acceptance.
use super::{Document, plugin_commands::Invoke, workspace_edits::Guard};
use rxui::prelude::*;
use std::sync::Arc;
use velk_commands::{
    Declaration, Handler, Outcome,
    completion::{Documentation, DocumentationTarget},
};

pub(super) struct State {
    target: DocumentationTarget,
    view: Entity<velk_editor_widget::Editor>,
    guard: Guard,
    requested: bool,
    render: Option<Task>,
    pending: Option<(Arc<velk_semantic::Service>, velk_semantic::PositionRequest)>,
}
impl Document {
    pub(super) fn cancel_completion_documentation(&mut self) {
        let Some(state) = self.completion_documentation.take() else {
            return;
        };
        let Some((service, request)) = state.pending else {
            return;
        };
        if service.cancel_command(request).is_err() {
            return;
        }
        for buffer in self
            .buffer
            .as_mut()
            .into_iter()
            .chain(self.inactive.values_mut())
        {
            if let Some(semantic) = &mut buffer.semantic
                && Arc::ptr_eq(&semantic.service, &service)
                && let Some(pending) = &mut semantic.command
                && pending.request == request
                && pending.documentation.is_some()
            {
                pending.cancelled = true;
            }
        }
    }
    pub(super) fn sync_completion_documentation(
        &mut self,
        source: Entity<velk_editor_widget::Editor>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.background_buffer
            || self
                .buffer
                .as_ref()
                .is_none_or(|buffer| buffer.editor.id() != source.id())
        {
            return;
        }
        let target = source.read(cx).completion_documentation_target();
        if self.completion_documentation.as_ref().is_some_and(|state| {
            Some(&state.target) != target.as_ref() || state.view.id() != source.id()
        }) {
            self.cancel_completion_documentation();
        }
        let Some(target) = target else {
            return;
        };
        if self.completion_documentation.is_none() {
            let mut guard = self.edit_guard(cx);
            guard.owner = target.source.clone().or_else(|| {
                self.buffer
                    .semantic
                    .as_ref()?
                    .completion
                    .as_ref()
                    .map(|invoke| (invoke.plugin.clone(), invoke.generation))
            });
            self.completion_documentation = Some(State {
                target: target.clone(),
                view: source.clone(),
                guard,
                requested: false,
                render: None,
                pending: None,
            });
        }
        if self.completion_documentation.as_ref().unwrap().requested {
            return;
        }
        let Some(item) = source.read(cx).completion_choice().cloned() else {
            return;
        };
        if let Some(documentation) = item.documentation {
            self.completion_documentation.as_mut().unwrap().requested = true;
            self.render_completion_documentation(documentation, cx);
            return;
        }
        let Some(payload) = item.documentation_resolve else {
            self.completion_documentation.as_mut().unwrap().requested = true;
            return;
        };
        let Some(semantic) = self.buffer.semantic.as_ref() else {
            return;
        };
        if semantic.command.is_some() || !matches!(semantic.status, velk_semantic::Status::Ready(_))
        {
            return;
        }
        if !self.command_available(cx)
            || self.command_task.is_some()
            || self.binding_modal()
            || self
                .plugins
                .as_ref()
                .is_some_and(|plugins| plugins.task.is_some())
        {
            return;
        }
        let state = self.completion_documentation.as_ref().unwrap();
        if !self.guard_valid(&state.guard, cx) {
            return;
        }
        let Some((plugin, generation)) = state.guard.owner.clone() else {
            return;
        };
        let guard = state.guard.clone();
        self.execute_plugin_command(
            Invoke {
                plugin,
                generation,
                command: Declaration {
                    id: "velk.completion.documentation".into(),
                    title: "Resolve completion documentation".into(),
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
        if let Some(pending) = self
            .buffer
            .semantic
            .as_mut()
            .and_then(|semantic| semantic.command.as_mut())
        {
            pending.documentation = Some(target);
            let request = pending.request;
            let service = self.buffer.semantic.as_ref().unwrap().service.clone();
            let state = self.completion_documentation.as_mut().unwrap();
            state.requested = true;
            state.pending = Some((service, request));
        }
    }
    pub(super) fn receive_completion_documentation(
        &mut self,
        target: DocumentationTarget,
        cancelled: bool,
        result: Result<Outcome, String>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(state) = self.completion_documentation.as_ref() else {
            return;
        };
        if cancelled
            || state.target != target
            || !self.guard_valid(&state.guard, cx)
            || state
                .view
                .read(cx)
                .completion_documentation_target()
                .as_ref()
                != Some(&target)
        {
            return;
        }
        self.completion_documentation.as_mut().unwrap().pending = None;
        let outcome = result.ok().filter(|outcome| outcome.validate().is_ok());
        let (text, language) = match outcome {
            Some(Outcome::Document { text, language, .. }) => (text, language),
            _ => (String::new(), None),
        };
        let documentation = Documentation {
            text,
            markdown: language.as_deref() == Some("markdown"),
        };
        let documentation = if documentation.validate().is_ok() {
            documentation
        } else {
            Documentation {
                text: String::new(),
                markdown: false,
            }
        };
        self.render_completion_documentation(documentation, cx);
    }
    fn render_completion_documentation(
        &mut self,
        documentation: Documentation,
        cx: &mut Context<'_, Self>,
    ) {
        if documentation.validate().is_err() {
            return;
        }
        let Some(state) = self.completion_documentation.as_ref() else {
            return;
        };
        let target = state.target.clone();
        let view = state.view.clone();
        let fallback = documentation.text;
        // Publish literal content immediately; formatting can finish asynchronously.
        view.update(cx, |editor, _| {
            editor.resolve_completion_documentation(
                target.clone(),
                Arc::new(velk_markup::Document::plain(fallback.clone())),
            );
        });
        if !documentation.markdown {
            return;
        }
        let renderer = self.buffer.semantic.as_ref().map_or_else(
            || self.markup_renderer(),
            |semantic| semantic.renderer.clone(),
        );
        let task = cx.try_spawn(
            renderer.render(fallback.clone()),
            move |this, result, cx| {
                let Some(state) = this.completion_documentation.as_ref() else {
                    return;
                };
                if state.target != target
                    || state.view.id() != view.id()
                    || !this.guard_valid(&state.guard, cx)
                {
                    return;
                }
                let document = match result {
                    Ok(Ok(document)) => document,
                    _ => velk_markup::Document::plain(fallback),
                };
                view.update(cx, |editor, _| {
                    editor.resolve_completion_documentation(target, Arc::new(document));
                });
                this.completion_documentation.as_mut().unwrap().render = None;
            },
        );
        if let Ok(task) = task {
            self.completion_documentation.as_mut().unwrap().render = Some(task);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::tests::Harness;
    use velk_commands::{Choice, ChoiceAction, completion::Item};
    use velk_semantic::{Event, Input, Service, Status};

    fn item(id: &str) -> Item {
        let mut item: Item = Choice {
            title: id.into(),
            detail: None,
            action: ChoiceAction::Contributed {
                plugin: "test.docs".into(),
                generation: 1,
                action: Box::new(ChoiceAction::Semantic {
                    payload: serde_json::json!({"accept":id}),
                }),
            },
        }
        .into();
        item.documentation_resolve = Some(serde_json::json!({"documentation":id}));
        item
    }
    fn attach(
        doc: &mut Document,
        client: velk_plugin_host::Client,
        service: Arc<Service>,
        cx: &mut Context<'_, Document>,
    ) {
        let mut plugins = super::super::plugins::State::new(client, "test.markdown".into());
        plugins.loaded = vec![velk_plugin_host::PluginInfo {
            id: "test.docs".into(),
            generation: 1,
            version: "1".into(),
            loaded_path: Default::default(),
            description: String::new(),
            metadata: velk_plugin_host::PluginMetadata {
                name: "Docs".into(),
                ..Default::default()
            },
            settings_values: Default::default(),
            settings_error: None,
            resident_code: false,
            hooks: vec![],
            dependencies: vec![],
        }];
        doc.plugins = Some(plugins);
        doc.buffer.semantic = Some(super::super::semantic::State {
            service,
            revision: 0,
            status: Status::Ready(Default::default()),
            count: 0,
            diagnostics: vec![],
            hover: None,
            command: None,
            completion: None,
            target: None,
            hover_view: None,
            next_request: 1,
            task: None,
            render_task: None,
            renderer: Arc::new(velk_markup::PlainText),
            notice: None,
        });
        doc.buffer.editor.update(cx, |editor, _| {
            editor.show_completions(vec![item("alpha")], false)
        });
    }
    fn terminal(service: &Service, session: &velk_semantic::Session, event: Event) -> Event {
        session.events().publish(event);
        pollster::block_on(service.events.recv()).unwrap()
    }
    #[track_caller]
    fn next_command(
        session: &velk_semantic::Session,
    ) -> (velk_semantic::PositionRequest, serde_json::Value) {
        loop {
            match session.try_next().expect("command queued") {
                Input::Command {
                    request, payload, ..
                } => return (request, payload),
                Input::Document(_) | Input::CancelHover => {}
                input => panic!("unexpected input: {input:?}"),
            }
        }
    }
    #[test]
    #[allow(unsafe_code)]
    fn selection_changes_cancel_lazy_resolution_and_late_results_cannot_replace_current_docs() {
        let directory = tempfile::tempdir().unwrap();
        // SAFETY: the directory has no native images.
        let host = unsafe { velk_plugin_host::Runtime::start(directory.path().into()) }.unwrap();
        let mut harness = Harness::new();
        let path = harness.write_file("docs.rs", "");
        harness
            .request(velk_plugin_contracts::host::Request::OpenBuffer { path })
            .unwrap();
        harness.drain();
        let (service, session) = Service::external(|| {});
        harness.runtime.update(|cx| harness.document.update(cx, |doc, cx| {
            attach(doc, host.client(), service.clone(), cx);
            let editor = doc.buffer.editor.clone();
            doc.sync_completion_documentation(editor.clone(), cx);
            let (old, payload) = next_command(&session);
            assert_eq!(payload["documentation"], "alpha");
            doc.sync_completion_documentation(editor.clone(), cx);
            assert!(session.try_next().is_none(), "same selection must not resolve twice");
            editor.update(cx, |editor, _| editor.show_completions(vec![item("beta")], true));
            doc.sync_completion_documentation(editor.clone(), cx);
            assert!(matches!(session.try_next(), Some(Input::CancelCommand(request)) if request == old));
            doc.semantic_event(terminal(&service, &session, Event::Command { request: old, result: Ok(Outcome::Document { title: "old".into(), text: "stale documentation".into(), language: None }) }), cx);
            assert!(editor.read(cx).completion_documentation().is_none());
            let (current, payload) = next_command(&session);
            assert_eq!(payload["documentation"], "beta");
            doc.semantic_event(terminal(&service, &session, Event::Command { request: current, result: Ok(Outcome::Document { title: "beta".into(), text: "Current documentation".into(), language: None }) }), cx);
            assert_eq!(editor.read(cx).completion_documentation().unwrap().text(), "Current documentation");
            assert_eq!(editor.read(cx).buffer().revision(), 0);
            assert_eq!(editor.read(cx).buffer().text().to_string(), "");
            doc.sync_completion_documentation(editor, cx);
            assert!(session.try_next().is_none());
        }));
    }
    #[test]
    #[allow(unsafe_code)]
    fn accepting_during_documentation_resolution_releases_the_slot_without_applying_docs_as_edits()
    {
        let directory = tempfile::tempdir().unwrap();
        // SAFETY: the directory has no native images.
        let host = unsafe { velk_plugin_host::Runtime::start(directory.path().into()) }.unwrap();
        let mut harness = Harness::new();
        let path = harness.write_file("docs.rs", "");
        harness
            .request(velk_plugin_contracts::host::Request::OpenBuffer { path })
            .unwrap();
        harness.drain();
        let (service, session) = Service::external(|| {});
        harness.runtime.update(|cx| harness.document.update(cx, |doc, cx| {
            attach(doc, host.client(), service.clone(), cx);
            let editor = doc.buffer.editor.clone();
            doc.sync_completion_documentation(editor.clone(), cx);
            let (request, _) = next_command(&session);
            let guard = doc.edit_guard(cx);
            doc.apply_choice_action(item("alpha").choice.action, guard, cx);
            assert!(doc.deferred_choice.is_some());
            editor.update(cx, |editor, _| editor.dismiss_completions());
            doc.sync_completion_documentation(editor.clone(), cx);
            assert!(matches!(session.try_next(), Some(Input::CancelCommand(current)) if current == request));
            doc.semantic_event(terminal(&service, &session, Event::Command { request, result: Ok(Outcome::Edit { edit: Default::default() }) }), cx);
            let (_, payload) = next_command(&session);
            assert_eq!(payload["accept"], "alpha");
            assert!(doc.deferred_choice.is_none());
            assert!(editor.read(cx).completion_documentation().is_none());
            assert_eq!(editor.read(cx).buffer().text().to_string(), "");
        }));
    }
    #[test]
    fn inline_markdown_has_a_literal_fallback_without_semantics_or_an_executor() {
        let mut harness = Harness::new();
        harness
            .request(velk_plugin_contracts::host::Request::NewBuffer {
                text: String::new(),
            })
            .unwrap();
        harness.runtime.update(|cx| {
            harness.document.update(cx, |doc, cx| {
                let mut inline = item("alpha");
                inline.choice.action = ChoiceAction::Semantic {
                    payload: serde_json::Value::Null,
                };
                inline.documentation = Some(Documentation {
                    text: "**Inline documentation**".into(),
                    markdown: true,
                });
                let editor = doc.buffer.editor.clone();
                editor.update(cx, |editor, _| editor.show_completions(vec![inline], false));
                doc.sync_completion_documentation(editor.clone(), cx);
                assert_eq!(
                    editor.read(cx).completion_documentation().unwrap().text(),
                    "**Inline documentation**"
                );
                editor.update(cx, |editor, _| editor.dismiss_completions());
                doc.sync_completion_documentation(editor.clone(), cx);
                assert!(doc.completion_documentation.is_none());
                assert!(editor.read(cx).completion_documentation().is_none());
            })
        });
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::document::tests::Harness;
    #[test]
    fn cancellation_reaches_the_originating_service_after_switching_buffers() {
        let mut harness = Harness::new();
        harness
            .request(velk_plugin_contracts::host::Request::NewBuffer {
                text: String::new(),
            })
            .unwrap();
        let (service, session) = velk_semantic::Service::external(|| {});
        harness.runtime.update(|cx| harness.document.update(cx, |doc, cx| {
            let editor = doc.buffer.editor.clone();
            let item: velk_commands::completion::Item = velk_commands::Choice {
                title: "alpha".into(), detail: None,
                action: velk_commands::ChoiceAction::Semantic { payload: Default::default() },
            }.into();
            editor.update(cx, |editor, _| editor.show_completions(vec![item], false));
            let target = editor.read(cx).completion_documentation_target().unwrap();
            let request = velk_semantic::PositionRequest { id: 1, revision: target.target.revision, position: target.target.position };
            service.command(request, serde_json::json!({"documentation":true}), None).unwrap();
            assert!(matches!(session.try_next(), Some(velk_semantic::Input::Command { .. })));
            doc.completion_documentation = Some(State { target, view: editor, guard: doc.edit_guard(cx), requested: true, render: None, pending: Some((service.clone(), request)) });
            doc.host_request(velk_plugin_contracts::host::Request::NewBuffer { text: "another buffer".into() }, None, cx).unwrap();
            doc.cancel_completion_documentation();
            assert!(matches!(session.try_next(), Some(velk_semantic::Input::CancelCommand(current)) if current == request));
            assert!(doc.completion_documentation.is_none());
        }));
    }
}
