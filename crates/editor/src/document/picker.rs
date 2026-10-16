use super::{
    Document,
    plugin_commands::{Invoke, ResultView},
    workspace_edits::Guard,
};
use rxui::{Element, prelude::*};
use velk_commands::{Choice, ChoiceAction, Handler, PickerMode, PickerReply, PickerRequest};

pub(super) struct State {
    root: Option<std::path::PathBuf>,
    invocation: Invoke,
    guard: Guard,
    query: velk_search::Query,
    items: Vec<Choice>,
    selected: usize,
    id: u64,
    session: u64,
    ready: bool,
    notice: String,
    task: Option<Task>,
}
struct Accept;
impl Command for Accept {}
struct Close;
impl Command for Close {}
struct Next;
impl Command for Next {}
struct Previous;
impl Command for Previous {}
struct Cancel {
    client: velk_plugin_host::Client,
    plugin: String,
    id: u64,
}
impl Drop for Cancel {
    fn drop(&mut self) {
        if let Ok(bytes) = serde_json::to_vec(&velk_commands::PickerCancel {
            plugin: self.plugin.clone(),
            id: self.id,
        }) {
            let _ = self
                .client
                .emit_hook(velk_commands::PICKER_CANCEL_HOOK.into(), bytes);
        }
    }
}
impl Document {
    pub(super) fn open_picker(
        &mut self,
        invocation: Invoke,
        guard: Guard,
        cx: &mut Context<'_, Self>,
    ) {
        self.command_result = Some(ResultView::Picker(State {
            root: self.workspace.root().map(std::path::Path::to_path_buf),
            invocation,
            guard,
            query: Default::default(),
            items: Vec::new(),
            selected: 0,
            id: 0,
            session: self.next_picker,
            ready: false,
            notice: String::new(),
            task: None,
        }));
        self.query_picker(cx);
    }
    fn query_picker(&mut self, cx: &mut Context<'_, Self>) {
        let context = self.command_context(cx);
        let Some(ResultView::Picker(state)) = &mut self.command_result else {
            return;
        };
        state.task = None;
        state.ready = false;
        state.id = self.next_picker;
        self.next_picker += 1;
        let id = state.id;
        let session = state.session;
        let invocation = state.invocation.clone();
        let Handler::Picker { hook, .. } = invocation.command.handler.clone() else {
            return;
        };
        let query = state.query.clone();
        if query.text.len() > 1024 {
            state.notice = "Query exceeds 1024 bytes".into();
            return;
        }
        state.notice = "Searching…".into();
        let Some(plugins) = &self.plugins else {
            return;
        };
        let client = plugins.client.clone();
        let cancel = Cancel {
            client: client.clone(),
            plugin: invocation.plugin.clone(),
            id,
        };
        match cx.try_spawn(
            async move {
                let _cancel = cancel;
                let handle = client
                    .bind_hook(invocation.plugin.clone(), hook)?
                    .recv()
                    .await
                    .map_err(|e| e.to_string())??;
                if handle.generation() != invocation.generation {
                    return Err("picker provider was replaced".into());
                }
                let mut request = PickerRequest::Begin {
                    id,
                    session,
                    context,
                    query,
                };
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                loop {
                    let bytes = client
                        .call_hook(
                            handle.clone(),
                            serde_json::to_vec(&request).map_err(|e| e.to_string())?,
                        )
                        .await?;
                    let reply: PickerReply =
                        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                    if let PickerReply::Ready { items, notice } = reply {
                        velk_commands::Outcome::Choices {
                            title: "Picker".into(),
                            items: items.clone(),
                        }
                        .validate()?;
                        if items
                            .iter()
                            .any(|item| !matches!(item.action, ChoiceAction::Location { .. }))
                        {
                            return Err("picker returned an unsupported action".into());
                        }
                        return Ok::<_, String>((items, notice));
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err("picker search timed out".into());
                    }
                    rxui::sleep(std::time::Duration::from_millis(16)).await;
                    request = PickerRequest::Poll { id };
                }
            },
            move |this, result, _| {
                let Some(ResultView::Picker(state)) = &mut this.command_result else {
                    return;
                };
                if state.id != id {
                    return;
                }
                state.task = None;
                match result {
                    Ok(Ok((items, notice))) => {
                        state.items = items;
                        state.selected = 0;
                        state.ready = true;
                        state.notice = notice;
                    }
                    Ok(Err(error)) => state.notice = error,
                    Err(error) => state.notice = error.to_string(),
                }
            },
        ) {
            Ok(task) => state.task = Some(task),
            Err(error) => state.notice = error.to_string(),
        }
    }
    pub(super) fn accept_picker(&mut self, state: State, cx: &mut Context<'_, Self>) {
        if !state.ready {
            return;
        }
        if !self.guard_valid(&state.guard, cx) || self.workspace.root() != state.root.as_deref() {
            self.notice(Some("Picker source changed; reopen the picker".into()));
            return;
        }
        if let Some(Choice {
            action: ChoiceAction::Location { location },
            ..
        }) = state.items.get(state.selected)
        {
            self.open_location(location.clone(), cx);
        }
    }
    pub(super) fn picker_modal(&self, state: &State, cx: &mut ViewContext<'_, Self>) -> Element {
        let close = cx
            .command(Close, |this, _, cx| {
                this.command_result = None;
                let _ = this.editor_focus.focus(cx);
            })
            .shortcut(Shortcut::new(KeyboardKey::Escape, Default::default()));
        let accept = cx
            .command(Accept, |this, _, cx| {
                if !matches!(&this.command_result, Some(ResultView::Picker(state)) if state.ready) {
                    return;
                }
                if let Some(ResultView::Picker(state)) = this.command_result.take() {
                    this.accept_picker(state, cx);
                }
            })
            .enabled(state.ready && !state.items.is_empty())
            .shortcut(Shortcut::new(KeyboardKey::Enter, Default::default()));
        let count = state.items.len();
        let next = cx
            .command(Next, move |this, _, _| {
                if let Some(ResultView::Picker(state)) = &mut this.command_result {
                    state.selected = if count == 0 {
                        0
                    } else {
                        (state.selected + 1) % count
                    };
                }
            })
            .shortcut(Shortcut::new(KeyboardKey::ArrowDown, Default::default()));
        let previous = cx
            .command(Previous, move |this, _, _| {
                if let Some(ResultView::Picker(state)) = &mut this.command_result {
                    state.selected = if count == 0 {
                        0
                    } else {
                        (state.selected + count - 1) % count
                    };
                }
            })
            .shortcut(Shortcut::new(KeyboardKey::ArrowUp, Default::default()));
        let mut content = column()
            .gap(8.)
            .padding(12.)
            .height(500.)
            .fill_width()
            .clip()
            .on_command(close)
            .on_command(next)
            .on_command(previous)
            .child(label(state.invocation.command.title.clone()))
            .child(
                text_input(state.query.text.clone())
                    .key("plugin-picker-query")
                    .fill_width()
                    .height(32.)
                    .flex_shrink(0.)
                    .placeholder("Type to search the project")
                    .on_command(accept)
                    .on_change(cx.listener(|this, event: &TextChangeEvent, cx| {
                        if let Some(ResultView::Picker(state)) = &mut this.command_result {
                            state.query.text = event.value.clone();
                        }
                        this.query_picker(cx);
                    })),
            );
        if matches!(
            state.invocation.command.handler,
            Handler::Picker {
                mode: PickerMode::Content,
                ..
            }
        ) {
            content = content.child(
                row()
                    .gap(8.)
                    .child(
                        button(if state.query.case_sensitive {
                            "Case: sensitive"
                        } else {
                            "Case: insensitive"
                        })
                        .on_click(cx.listener(|this, _, cx| {
                            if let Some(ResultView::Picker(state)) = &mut this.command_result {
                                state.query.case_sensitive = !state.query.case_sensitive;
                            }
                            this.query_picker(cx);
                        })),
                    )
                    .child(
                        button(if state.query.whole_word {
                            "Whole word: on"
                        } else {
                            "Whole word: off"
                        })
                        .on_click(cx.listener(|this, _, cx| {
                            if let Some(ResultView::Picker(state)) = &mut this.command_result {
                                state.query.whole_word = !state.query.whole_word;
                            }
                            this.query_picker(cx);
                        })),
                    ),
            );
        }
        content = content.child(
            label(state.notice.clone())
                .height(24.)
                .flex_shrink(0.)
                .clip(),
        );
        let mut results = column()
            .gap(4.)
            .fill_width()
            .flex_grow(1.)
            .min_height(0.)
            .clip();
        for (index, item) in state
            .items
            .iter()
            .enumerate()
            .skip(state.selected.saturating_sub(4))
            .take(9)
        {
            let caption = item.detail.as_ref().map_or_else(
                || item.title.clone(),
                |detail| format!("{} · {detail}", item.title),
            );
            results = results.child(
                button(caption)
                    .fill_width()
                    .height(34.)
                    .flex_shrink(0.)
                    .clip()
                    .key(format!("picker-result-{index}"))
                    .background(if state.selected == index {
                        ThemeColor::Selection
                    } else {
                        ThemeColor::Control
                    })
                    .on_click(cx.listener(move |this, _, cx| {
                        if !matches!(&this.command_result, Some(ResultView::Picker(state)) if state.ready) { return; }
                        if let Some(ResultView::Picker(mut state)) = this.command_result.take() {
                            state.selected = index;
                            this.accept_picker(state, cx);
                        }
                    })),
            );
        }
        modal(content.child(results))
            .key("plugin-picker")
            .width(840.)
            .on_dismiss(cx.listener(|this, _: &DismissEvent, _| this.command_result = None))
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::tests::QueuedExecutor;
    use rxui::{KeyEvent, Runtime, Ui};
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
    fn state(
        document: &Document,
        cx: &impl rxui::ReadContext,
        paths: &[std::path::PathBuf],
    ) -> State {
        State {
            root: document.workspace.root().map(std::path::Path::to_path_buf),
            invocation: Invoke {
                plugin: "test.picker".into(),
                generation: 1,
                command: velk_commands::Declaration {
                    id: "test.picker".into(),
                    title: "Find".into(),
                    language: None,
                    input: None,
                    shortcut: None,
                    handler: Handler::Picker {
                        hook: "test.query".into(),
                        mode: PickerMode::Content,
                    },
                },
            },
            guard: document.edit_guard(cx),
            query: Default::default(),
            items: paths
                .iter()
                .map(|path| Choice {
                    title: path.display().to_string(),
                    detail: None,
                    action: ChoiceAction::Location {
                        location: velk_commands::Location {
                            path: path.clone(),
                            position: velk_commands::Position {
                                line: 0,
                                column: 1,
                                encoding: velk_commands::Encoding::Utf32,
                            },
                        },
                    },
                })
                .collect(),
            selected: 0,
            id: 1,
            session: 1,
            ready: true,
            notice: "Ready".into(),
            task: None,
        }
    }
    #[test]
    fn picker_geometry_stays_fixed_across_loading_empty_and_long_results() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a.txt");
        std::fs::write(&path, "text").unwrap();
        let mut runtime = Runtime::new();
        let document = runtime.update(|cx| {
            let (file, editor) = crate::file::FileState::open(&path).unwrap();
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, path.clone(), cx))
        });
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        let mut expected = None;
        for count in [0, 1, 9, 1000, 0] {
            runtime.update(|cx| {
                document.update(cx, |d, cx| {
                    let mut state = state(d, cx, &vec![path.clone(); count]);
                    state.notice = "A long status message ".repeat(count.min(30));
                    for item in &mut state.items {
                        item.title = "long/path/".repeat(100);
                    }
                    state.ready = count != 0;
                    d.command_result = Some(ResultView::Picker(state));
                })
            });
            ui.prepare(&mut runtime, [1000., 700.], &mut Measure)
                .unwrap();
            let bounds = ["plugin-picker", "plugin-picker-query"].map(|key| {
                let bounds = ui
                    .elements()
                    .find(|e| e.key == Some(&rxui::Key::from(key)))
                    .unwrap()
                    .bounds;
                [bounds.x, bounds.y, bounds.width, bounds.height]
            });
            if let Some(expected) = expected {
                assert_eq!(bounds, expected);
            } else {
                expected = Some(bounds);
            }
        }
    }
    #[test]
    fn picker_keyboard_navigates_to_a_result_and_escape_dismisses_it() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("a.txt");
        let second = directory.path().join("b.txt");
        std::fs::write(&first, "first").unwrap();
        std::fs::write(&second, "second").unwrap();
        let mut runtime = Runtime::new();
        let executor = std::sync::Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = runtime.update(|cx| {
            let (file, editor) = crate::file::FileState::open(&first).unwrap();
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, first.clone(), cx))
        });
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.command_result = Some(ResultView::Picker(state(
                    d,
                    cx,
                    &[first.clone(), second.clone()],
                )))
            })
        });
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        let query = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("plugin-picker-query")))
            .unwrap()
            .id;
        ui.focus(query);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.query_picker(cx);
                let Some(ResultView::Picker(state)) = &d.command_result else {
                    panic!()
                };
                assert_eq!(state.items.len(), 2);
                assert!(!state.ready);
            })
        });
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        ui.key(
            &mut runtime,
            KeyEvent {
                key: KeyboardKey::Enter,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
        )
        .unwrap();
        runtime.update(|cx| {
            document.update(cx, |d, _| {
                let Some(ResultView::Picker(state)) = &mut d.command_result else {
                    panic!("pending results must not close the picker")
                };
                state.ready = true;
            })
        });
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
        for key in [KeyboardKey::ArrowDown, KeyboardKey::Enter] {
            ui.key(
                &mut runtime,
                KeyEvent {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            ui.prepare(&mut runtime, [900., 600.], &mut Measure)
                .unwrap();
        }
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                assert_eq!(
                    d.buffer.file.as_ref().unwrap().path(),
                    second.canonicalize().unwrap()
                );
                assert_eq!(d.buffer.editor.read(cx).buffer().selection().head, 1);
                d.command_result = Some(ResultView::Picker(state(
                    d,
                    cx,
                    std::slice::from_ref(&first),
                )));
            })
        });
        ui.prepare(&mut runtime, [900., 600.], &mut Measure)
            .unwrap();
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
        runtime.update(|cx| assert!(document.read(cx).command_result.is_none()));
    }
}
