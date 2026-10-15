//! Presentation adapter. Only this module maps neutral semantic results to UI.
use super::Document;
use rxui::prelude::*;
use std::sync::Arc;
use velk_editor_widget::{
    Annotations, Buffer, HoverCard, HoverSource, HoverTarget, LinePattern, OverlayKind,
    OverlayPlacement, SpanStyle, StyledRange, TextOverlay, Underline,
};
use velk_semantic::{DocumentSnapshot, Event, HoverRequest, Service, Severity, Status};

pub(super) struct State {
    pub service: Arc<Service>,
    pub revision: u64,
    pub status: Status,
    pub count: usize,
    pub hover: Option<HoverRequest>,
    pub target: Option<HoverTarget>,
    pub next_hover: u64,
    pub task: Option<Task>,
    pub render_task: Option<Task>,
    pub renderer: Arc<dyn velk_markup::Renderer>,
    pub notice: Option<String>,
}
impl Document {
    pub fn enable_semantics(&mut self, service: Arc<Service>, cx: &mut Context<'_, Self>) {
        self.enable_semantics_with_renderer(service, self.markup_renderer(), cx);
    }
    /// Host extension point: a plugin can supply Markdown interpretation
    /// without knowing about LSP transport or owning the popup's input lifetime.
    pub fn enable_semantics_with_renderer(
        &mut self,
        service: Arc<Service>,
        renderer: Arc<dyn velk_markup::Renderer>,
        cx: &mut Context<'_, Self>,
    ) {
        let snapshot = self.semantic_snapshot(cx);
        service.update(snapshot.clone());
        self.semantic = Some(State {
            service,
            revision: snapshot.revision,
            status: Status::Starting,
            count: 0,
            hover: None,
            target: None,
            next_hover: 1,
            task: None,
            render_task: None,
            renderer,
            notice: None,
        });
        self.arm_semantics(cx);
    }
    pub(super) fn semantic_snapshot(&self, cx: &impl rxui::ReadContext) -> DocumentSnapshot {
        let editor = self.editor.read(cx);
        DocumentSnapshot {
            path: self.file.path().to_path_buf(),
            language: "rust".into(),
            revision: editor.buffer().revision(),
            text: editor.buffer().text().clone(),
        }
    }
    pub(super) fn sync_semantics(&mut self, cx: &mut Context<'_, Self>) -> bool {
        let Some(state) = &mut self.semantic else {
            return false;
        };
        let editor = self.editor.read(cx);
        let buffer = editor.buffer();
        let target = editor.hover_request();
        if state.target != target {
            state.render_task = None;
            state.service.cancel_hover();
            state.target = target;
            state.hover = None;
            state.notice = None;
            if let Some(target) = target {
                let request = HoverRequest {
                    id: state.next_hover,
                    revision: target.revision,
                    position: target.position,
                };
                state.next_hover += 1;
                state.hover = Some(request);
                state.service.hover(request);
            }
        }
        if state.revision == buffer.revision() {
            return false;
        }
        let visible_changed = state.count != 0 || state.notice.is_some();
        state.revision = buffer.revision();
        state.count = 0;
        state.notice = None;
        state.service.update(DocumentSnapshot {
            path: self.file.path().to_path_buf(),
            language: "rust".into(),
            revision: buffer.revision(),
            text: buffer.text().clone(),
        });
        visible_changed
    }
    fn arm_semantics(&mut self, cx: &mut Context<'_, Self>) {
        let Some(state) = &mut self.semantic else {
            return;
        };
        let events = state.service.events.clone();
        match cx.try_spawn(async move { events.recv().await }, |this, result, cx| {
            if let Some(state) = &mut this.semantic {
                state.task = None;
            }
            if let Ok(Ok(event)) = result {
                this.semantic_event(event, cx);
                this.arm_semantics(cx);
            }
        }) {
            Ok(task) => state.task = Some(task),
            Err(error) => {
                state.service.shutdown();
                state.status = Status::Failed(error.to_string());
            }
        }
    }
    #[tracing::instrument(level = "debug", skip_all)]
    fn semantic_event(&mut self, event: Event, cx: &mut Context<'_, Self>) {
        let Some(state) = &mut self.semantic else {
            return;
        };
        match event {
            Event::Status(status) => {
                tracing::debug!(?status, "semantic status");
                if matches!(status, Status::Failed(_) | Status::Stopped) {
                    state.count = 0;
                    state.hover = None;
                    state.render_task = None;
                    self.editor
                        .update(cx, |editor, _| editor.clear_annotations());
                }
                let enabled = matches!(status,Status::Ready(c) if c.hover);
                self.editor
                    .update(cx, |editor, _| editor.set_semantic_hover(enabled));
                state.status = status;
            }
            Event::Diagnostics {
                revision,
                diagnostics,
            } => {
                tracing::debug!(
                    revision,
                    diagnostics = diagnostics.len(),
                    "diagnostics received"
                );
                if matches!(state.status, Status::Failed(_) | Status::Stopped)
                    || self.editor.read(cx).buffer().revision() != revision
                {
                    cx.unchanged();
                    return;
                }
                let annotations = annotations(self.editor.read(cx).buffer(), &diagnostics);
                state.count = diagnostics.len();
                self.editor.update(cx, |editor, _| {
                    let _ = editor.set_annotations(annotations);
                });
            }
            Event::Hover { request, hover } => {
                tracing::debug!(
                    request_id = request.id,
                    revision = request.revision,
                    found = hover.is_some(),
                    "hover received"
                );
                let current = self.editor.read(cx);
                let valid = state.hover == Some(request)
                    && state.target.is_some()
                    && current.hover_request() == state.target
                    && current.buffer().revision() == request.revision;
                drop(current);
                if !valid {
                    cx.unchanged();
                    return;
                }
                let target = state.target.unwrap();
                state.hover = None;
                if let Some(hover) = hover {
                    if hover.markdown {
                        let renderer = state.renderer.clone();
                        let range = hover.range;
                        let fallback = hover.contents.clone();
                        match cx.try_spawn(
                            renderer.render(hover.contents),
                            move |this, result, cx| {
                                let Some(state) = &mut this.semantic else {
                                    return;
                                };
                                if state.target != Some(target) {
                                    cx.unchanged();
                                    return;
                                }
                                state.render_task = None;
                                let document = match result {
                                    Ok(Ok(document)) => document,
                                    _ => velk_markup::Document::plain(fallback),
                                };
                                this.editor.update(cx, |editor, _| {
                                    editor.resolve_formatted_hover(
                                        target,
                                        range,
                                        "Rust".into(),
                                        Arc::new(document),
                                    );
                                });
                            },
                        ) {
                            Ok(task) => state.render_task = Some(task),
                            Err(error) => {
                                self.editor.update(cx, |editor, _| {
                                    editor.resolve_hover(target, None, None);
                                });
                                if target.source == HoverSource::Caret {
                                    state.notice = Some(error.to_string());
                                }
                            }
                        }
                    } else {
                        self.editor.update(cx, |editor, _| {
                            editor.resolve_hover(
                                target,
                                hover.range,
                                Some(HoverCard {
                                    title: "Rust".into(),
                                    body: hover.contents,
                                }),
                            );
                        });
                    }
                } else {
                    self.editor.update(cx, |editor, _| {
                        editor.resolve_hover(target, None, None);
                    });
                    if target.source == HoverSource::Caret {
                        state.notice = Some("No hover information at the caret".into());
                    }
                }
            }
            Event::RequestFailed { request, message } => {
                if state.hover != Some(request)
                    || state.target.is_none()
                    || self.editor.read(cx).hover_request() != state.target
                {
                    cx.unchanged();
                    return;
                }
                state.hover = None;
                let target = state.target.unwrap();
                self.editor.update(cx, |editor, _| {
                    editor.resolve_hover(target, None, None);
                });
                if target.source == HoverSource::Caret {
                    state.notice = Some(message);
                }
            }
        }
    }
    pub(super) fn request_hover(&mut self, cx: &mut Context<'_, Self>) {
        if self.semantic.is_none() {
            return;
        }
        self.editor.update(cx, |editor, _| {
            editor.set_semantic_hover(true);
            editor.request_hover_at_caret();
        });
        self.sync_semantics(cx);
    }
    pub(super) fn semantic_status(&self) -> Option<String> {
        self.semantic.as_ref().map(|state| {
            if let Some(notice) = &state.notice {
                return notice.clone();
            }
            match &state.status {
                Status::Starting => "Rust: starting…".into(),
                Status::Ready(capabilities) => format!(
                    "Rust: {} diagnostics{}",
                    state.count,
                    if capabilities.hover {
                        " · F1 hover"
                    } else {
                        ""
                    }
                ),
                Status::Failed(error) => format!("Rust: unavailable — {error}"),
                Status::Stopped => "Rust: stopped".into(),
            }
        })
    }
}
fn annotations(buffer: &Buffer, diagnostics: &[velk_semantic::Diagnostic]) -> Annotations {
    let mut annotations = Annotations {
        document_revision: buffer.revision(),
        ..Annotations::default()
    };
    for diagnostic in diagnostics {
        let Some(range) = buffer.grapheme_range(diagnostic.range.clone()) else {
            continue;
        };
        let color = match diagnostic.severity {
            Severity::Error => [0.9, 0.12, 0.16, 1.],
            Severity::Warning => [0.9, 0.55, 0.08, 1.],
            Severity::Information => [0.2, 0.55, 0.9, 1.],
            Severity::Hint => [0.35, 0.6, 0.55, 1.],
        };
        let title = diagnostic.source.as_deref().unwrap_or("Diagnostic");
        let title = diagnostic
            .code
            .as_ref()
            .map_or_else(|| title.to_owned(), |code| format!("{title}: {code}"));
        let card = HoverCard {
            title,
            body: diagnostic.message.clone(),
        };
        if range.is_empty() {
            annotations.overlays.push(TextOverlay {
                anchor: range.start,
                text: diagnostic
                    .message
                    .lines()
                    .next()
                    .unwrap_or("Diagnostic")
                    .chars()
                    .filter(|c| !c.is_control() && !matches!(*c, '\u{2028}' | '\u{2029}'))
                    .take(160)
                    .collect(),
                kind: OverlayKind::Diagnostic,
                placement: OverlayPlacement::LineEnd,
                foreground: color,
                background: None,
                hover: Some(card),
            });
        } else {
            annotations.spans.push(StyledRange {
                range,
                style: SpanStyle {
                    underline: Some(Underline {
                        color,
                        pattern: LinePattern::Wavy,
                    }),
                    ..SpanStyle::default()
                },
                hover: Some(card),
            });
        }
    }
    annotations
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxui::Runtime;
    use velk_editor_widget::Editor;
    use velk_semantic::{Backend, BackendFuture, Session};
    struct Idle;
    impl Backend for Idle {
        fn run(self: Box<Self>, session: Session) -> BackendFuture {
            Box::pin(async move {
                session.cancelled().await;
                Ok(())
            })
        }
    }
    #[test]
    fn diagnostics_align_graphemes_and_preserve_message_cards_and_eof_overlays() {
        let buffer = Buffer::new("é\n世界");
        let diagnostic = |range, message: &str| velk_semantic::Diagnostic {
            range,
            severity: Severity::Error,
            message: message.into(),
            source: Some("test".into()),
            code: Some("E1".into()),
        };
        let annotations = annotations(
            &buffer,
            &[
                diagnostic(1..2, "combining mark"),
                diagnostic(5..5, "at EOF\u{2028}\nmore information"),
                diagnostic(99..100, "invalid"),
            ],
        );
        assert_eq!(annotations.spans[0].range, 0..2);
        assert_eq!(
            annotations.spans[0].hover.as_ref().unwrap().title,
            "test: E1"
        );
        assert_eq!(annotations.overlays[0].anchor, 5);
        assert_eq!(annotations.overlays[0].text, "at EOF");
        Editor::new("é\n世界").set_annotations(annotations).unwrap();
    }
    #[test]
    fn hover_shortcut_accepts_native_and_portable_f1_names() {
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
        let path = directory.path().join("file.rs");
        std::fs::write(&path, "fn main() {}").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let document = runtime.update(|cx| {
            let editor = cx.new(|_| editor);
            cx.new(|cx| {
                let mut d = Document::new(editor, file, path, cx);
                d.semantic = Some(State {
                    service: Service::start(Idle).unwrap(),
                    revision: 0,
                    status: Status::Ready(velk_semantic::Capabilities {
                        diagnostics: true,
                        hover: true,
                    }),
                    count: 0,
                    hover: None,
                    target: None,
                    next_hover: 1,
                    task: None,
                    render_task: None,
                    renderer: Arc::new(velk_markup::PlainText),
                    notice: None,
                });
                d
            })
        });
        let mut ui = rxui::Ui::new(&mut runtime, document.clone()).unwrap();
        ui.prepare(&mut runtime, [640., 400.], &mut Measure)
            .unwrap();
        let id = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        ui.focus(id);
        for (index, name) in ["F1", "Named(F1)"].into_iter().enumerate() {
            ui.key(
                &mut runtime,
                rxui::KeyEvent {
                    key: KeyboardKey::Other(name.into()),
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            runtime.update(|cx| {
                assert_eq!(
                    document
                        .read(cx)
                        .semantic
                        .as_ref()
                        .unwrap()
                        .hover
                        .unwrap()
                        .id,
                    index as u64 + 1
                )
            });
            ui.prepare(&mut runtime, [640., 400.], &mut Measure)
                .unwrap();
        }
    }

    #[test]
    fn injected_markdown_renderer_runs_off_ui_and_cancelled_work_cannot_reopen_hover() {
        use rxui::{BackgroundFuture, BlockingJob, SpawnError, TaskExecutor};
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        #[derive(Default)]
        struct Queue(Mutex<Vec<BlockingJob>>);
        impl TaskExecutor for Queue {
            fn spawn(&self, future: BackgroundFuture) -> Result<(), SpawnError> {
                self.0
                    .lock()
                    .unwrap()
                    .push(Box::new(move || pollster::block_on(future)));
                Ok(())
            }
            fn spawn_blocking(&self, job: BlockingJob) -> Result<(), SpawnError> {
                self.0.lock().unwrap().push(job);
                Ok(())
            }
        }
        struct PluginRenderer {
            ui_thread: std::thread::ThreadId,
            calls: Arc<AtomicUsize>,
        }
        impl velk_markup::Renderer for PluginRenderer {
            fn render(&self, _: String) -> velk_markup::RenderFuture {
                let ui_thread = self.ui_thread;
                let calls = self.calls.clone();
                Box::pin(async move {
                    assert_ne!(std::thread::current().id(), ui_thread);
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(velk_markup::Document::plain(
                        "Plugin-rendered documentation",
                    ))
                })
            }
        }
        struct Measure;
        impl rxui::TextMeasure for Measure {
            fn measure(
                &mut self,
                _: rxui::ElementId,
                _: rxui::TextRequest<'_>,
            ) -> Result<[f32; 2], rxui::UiError> {
                Ok([300., 40.])
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.rs");
        std::fs::write(&path, "fn main() {}").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let queue = Arc::new(Queue::default());
        runtime.configure_tasks(queue.clone(), || {}).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let editor = runtime.update(|cx| cx.new(|_| editor));
        let document =
            runtime.update(|cx| cx.new(|cx| Document::new(editor.clone(), file, path, cx)));
        runtime.update(|cx| {
            document.update(cx, |d, _| {
                d.semantic = Some(State {
                    service: Service::start(Idle).unwrap(),
                    revision: 0,
                    status: Status::Ready(velk_semantic::Capabilities {
                        hover: true,
                        diagnostics: true,
                    }),
                    count: 0,
                    hover: None,
                    target: None,
                    next_hover: 1,
                    task: None,
                    render_task: None,
                    renderer: Arc::new(PluginRenderer {
                        ui_thread: std::thread::current().id(),
                        calls: calls.clone(),
                    }),
                    notice: None,
                });
            })
        });
        let mut ui = rxui::Ui::new(&mut runtime, document.clone()).unwrap();
        ui.prepare(&mut runtime, [640., 400.], &mut Measure)
            .unwrap();
        for cancel in [false, true] {
            runtime.update(|cx| {
                document.update(cx, |d, cx| {
                    d.request_hover(cx);
                    let request = d.semantic.as_ref().unwrap().hover.unwrap();
                    d.semantic_event(
                        Event::Hover {
                            request,
                            hover: Some(velk_semantic::Hover {
                                request,
                                contents: "**server Markdown**".into(),
                                markdown: true,
                                range: None,
                            }),
                        },
                        cx,
                    );
                    if cancel {
                        d.editor.update(cx, |e, _| e.insert("x"));
                        d.sync_semantics(cx);
                    }
                })
            });
            // The UI callback only queues interpretation; it has not run here.
            let before = calls.load(Ordering::SeqCst);
            assert!(!queue.0.lock().unwrap().is_empty());
            let jobs = std::mem::take(&mut *queue.0.lock().unwrap());
            std::thread::spawn(move || {
                for job in jobs {
                    job();
                }
            })
            .join()
            .unwrap();
            runtime.poll_tasks();
            runtime.flush().unwrap();
            ui.prepare(&mut runtime, [640., 400.], &mut Measure)
                .unwrap();
            let shown = ui
                .elements()
                .any(|e| e.text == Some("Plugin-rendered documentation"));
            assert_eq!(shown, !cancel);
            if !cancel {
                assert_eq!(calls.load(Ordering::SeqCst), before + 1);
            } else {
                assert_eq!(calls.load(Ordering::SeqCst), before);
            }
        }
    }

    #[test]
    fn obsolete_results_cannot_attach_to_new_text_or_replace_current_hover_request() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.rs");
        std::fs::write(&path, "fn main() {}").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        runtime.update(|cx| {
            let editor = cx.new(|_| editor);
            let document = cx.new(|cx| Document::new(editor.clone(), file, path, cx));
            let request = HoverRequest {
                id: 2,
                revision: 0,
                position: 0,
            };
            document.update(cx, |d, _| {
                d.semantic = Some(State {
                    service: Service::start(Idle).unwrap(),
                    revision: 0,
                    status: Status::Ready(Default::default()),
                    count: 0,
                    hover: Some(request),
                    target: None,
                    next_hover: 3,
                    task: None,
                    render_task: None,
                    renderer: Arc::new(velk_markup::PlainText),
                    notice: None,
                })
            });
            document.update(cx, |d, cx| {
                d.semantic_event(
                    Event::Diagnostics {
                        revision: 99,
                        diagnostics: vec![],
                    },
                    cx,
                )
            });
            assert!(editor.read(cx).annotations().is_none());
            document.update(cx, |d, cx| {
                d.semantic_event(
                    Event::Hover {
                        request: HoverRequest { id: 1, ..request },
                        hover: None,
                    },
                    cx,
                )
            });
            assert_eq!(
                document.read(cx).semantic.as_ref().unwrap().hover,
                Some(request)
            );
            editor.update(cx, |editor, _| editor.insert("x"));
            document.update(cx, |d, cx| {
                d.semantic_event(
                    Event::Diagnostics {
                        revision: 0,
                        diagnostics: vec![],
                    },
                    cx,
                )
            });
            assert!(editor.read(cx).annotations().is_none());
            document.update(cx, |d, cx| {
                d.semantic_event(
                    Event::Hover {
                        request,
                        hover: None,
                    },
                    cx,
                )
            });
            assert!(
                document
                    .read(cx)
                    .semantic
                    .as_ref()
                    .unwrap()
                    .notice
                    .is_none()
            );
            // Queued results must not restore decorations after disconnection.
            document.update(cx, |d, cx| {
                d.semantic_event(Event::Status(Status::Failed("disconnected".into())), cx)
            });
            document.update(cx, |d, cx| {
                d.semantic_event(
                    Event::Diagnostics {
                        revision: 1,
                        diagnostics: vec![],
                    },
                    cx,
                )
            });
            assert!(editor.read(cx).annotations().is_none());
        });
    }
}
