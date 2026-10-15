//! Host presentation and scheduling; matching belongs to a loaded provider.
use super::Document;
use rxui::{Element, prelude::*};
use std::time::{Duration, Instant};
use velk_plugin_host::{Client, HookHandle};
use velk_search::{self as model, Query, Reply, Request, Results};

pub(super) struct Find;
impl Command for Find {}
struct Close;
impl Command for Close {}
struct Next;
impl Command for Next {}
struct Previous;
impl Command for Previous {}

pub(super) struct State {
    pub open: bool,
    pub(super) preserve_selection: bool,
    pub(super) query: Query,
    id: u64,
    revision: u64,
    results: Option<Results>,
    selected: Option<usize>,
    notice: String,
    task: Option<Task>,
    pub(super) focus: rxui::FocusHandle,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            preserve_selection: false,
            query: Query::default(),
            id: 0,
            revision: 0,
            results: None,
            selected: None,
            notice: "Find in this file".into(),
            task: None,
            focus: rxui::FocusHandle::new(),
        }
    }
}

async fn bind(client: &Client, backend: String) -> Result<HookHandle, String> {
    client
        .bind_hook(backend, model::HOOK.into())?
        .recv()
        .await
        .map_err(|e| e.to_string())?
}
async fn exchange(client: &Client, handle: &HookHandle, request: Request) -> Result<Reply, String> {
    model::decode(
        &client
            .call_hook(handle.clone(), model::encode(&request)?)
            .await?,
    )
}
async fn accepted(client: &Client, handle: &HookHandle, request: Request) -> Result<(), String> {
    match exchange(client, handle, request).await? {
        Reply::Accepted => Ok(()),
        _ => Err("Search backend returned an unexpected acknowledgement".into()),
    }
}
#[tracing::instrument(level = "debug", skip_all, fields(async_work = true, query_id = id, revision, bytes = text.len_bytes()))]
async fn run(
    client: Client,
    backend: String,
    id: u64,
    revision: u64,
    query: Query,
    text: ropey::Rope,
) -> Result<Results, String> {
    // A cloned Rope is the only document work performed on the UI thread.
    rxui::sleep(Duration::from_millis(50)).await;
    let handle = bind(&client, backend).await?;
    accepted(
        &client,
        &handle,
        Request::Begin {
            id,
            revision,
            query,
        },
    )
    .await?;
    let result = async {
        let mut chunk = String::with_capacity(model::CHUNK_BYTES);
        for part in text.chunks() {
            if chunk.len() + part.len() > model::CHUNK_BYTES && !chunk.is_empty() {
                accepted(
                    &client,
                    &handle,
                    Request::Append {
                        id,
                        text: std::mem::take(&mut chunk),
                    },
                )
                .await?;
            }
            // Rope chunks are small, UTF-8, and never split a scalar.
            if part.len() > model::CHUNK_BYTES {
                return Err("Rope chunk exceeds search upload limit".into());
            }
            chunk.push_str(part);
        }
        if !chunk.is_empty() {
            accepted(&client, &handle, Request::Append { id, text: chunk }).await?;
        }
        accepted(&client, &handle, Request::Start { id }).await?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match exchange(&client, &handle, Request::Poll { id }).await? {
                Reply::Ready(results) => {
                    results.validate(id, revision, text.len_chars())?;
                    return Ok(results);
                }
                Reply::Pending if Instant::now() < deadline => {
                    rxui::sleep(Duration::from_millis(8)).await
                }
                Reply::Pending => return Err("Search timed out".into()),
                _ => return Err("Search backend returned an unexpected result".into()),
            }
        }
    }
    .await;
    if result.is_err() {
        let _ = accepted(&client, &handle, Request::Cancel { id }).await;
    }
    result
}

impl Document {
    pub(super) fn cancel_search(&mut self, cx: &mut Context<'_, Self>) {
        self.buffer.search.task = None;
        if let Some(state) = &self.plugins {
            let client = state.client.clone();
            let backend = self.shell.store.preferences.search_backend.clone();
            let id = self.buffer.search.id;
            if let Ok(task) = cx.try_spawn(
                async move {
                    if let Ok(handle) = bind(&client, backend).await {
                        let _ = accepted(&client, &handle, Request::Cancel { id }).await;
                    }
                },
                |_, _, _| {},
            ) {
                task.detach();
            }
        }
    }
    pub(super) fn sync_search(&mut self, cx: &mut Context<'_, Self>) -> bool {
        if self.background_buffer
            || !self.buffer.search.open
            || self.buffer.search.revision == self.buffer.editor.read(cx).buffer().revision()
        {
            return false;
        }
        self.restart_search(cx);
        true
    }
    pub(super) fn restart_search(&mut self, cx: &mut Context<'_, Self>) {
        self.cancel_search(cx);
        self.next_search += 1;
        self.buffer.search.id = self.next_search;
        self.buffer.search.results = None;
        self.buffer.search.selected = None;
        let (text, revision, caret) = {
            let editor = self.buffer.editor.read(cx);
            let buffer = editor.buffer();
            (
                buffer.text().clone(),
                buffer.revision(),
                buffer.selection().head,
            )
        };
        self.buffer.search.revision = revision;
        let query = self.buffer.search.query.clone();
        if query.text.is_empty() {
            self.buffer.search.notice = "Find in this file".into();
            return;
        }
        if let Err(error) = query.validate() {
            self.buffer.search.notice = error;
            return;
        }
        if text.len_bytes() > model::MAX_DOCUMENT_BYTES {
            self.buffer.search.notice = "Search supports buffers up to 64 MiB".into();
            return;
        }
        let Some(plugins) = &self.plugins else {
            self.buffer.search.notice = "Search backend unavailable; load a search plugin".into();
            return;
        };
        self.buffer.search.notice = "Searching…".into();
        let id = self.buffer.search.id;
        let buffer_id = self.buffer.id;
        match cx.try_spawn(
            run(
                plugins.client.clone(),
                self.shell.store.preferences.search_backend.clone(),
                id,
                revision,
                query,
                text,
            ),
            move |this, result, cx| {
                this.with_buffer(buffer_id, cx, |this, cx| {
                    if !this.buffer.search.open
                        || this.buffer.search.id != id
                        || this.buffer.editor.read(cx).buffer().revision() != revision
                    {
                        return;
                    }
                    this.buffer.search.task = None;
                    match result {
                        Ok(Ok(results)) => {
                            let selected = results
                                .matches
                                .iter()
                                .position(|m| m.start >= caret)
                                .unwrap_or(0);
                            this.buffer.search.results = Some(results);
                            this.buffer.search.selected = None;
                            if this.buffer.search.preserve_selection {
                                this.buffer.search.preserve_selection = false;
                                let range =
                                    this.buffer.editor.read(cx).buffer().selection().range();
                                let results = this.buffer.search.results.as_ref().unwrap();
                                this.buffer.search.selected = results
                                    .matches
                                    .iter()
                                    .position(|m| m.start == range.start && m.end == range.end);
                                this.buffer.search.notice = match this.buffer.search.selected {
                                    Some(index) => {
                                        format!("{} of {}", index + 1, results.matches.len())
                                    }
                                    None => format!("{} matches", results.matches.len()),
                                };
                            } else {
                                this.select_search_match(selected, cx);
                            }
                        }
                        Ok(Err(error)) => {
                            this.buffer.search.notice = format!("Search unavailable: {error}")
                        }
                        Err(error) => this.buffer.search.notice = format!("Search failed: {error}"),
                    }
                });
            },
        ) {
            Ok(task) => self.buffer.search.task = Some(task),
            Err(error) => self.buffer.search.notice = error.to_string(),
        }
    }
    fn select_search_match(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        let Some(results) = &self.buffer.search.results else {
            return;
        };
        if results.revision != self.buffer.editor.read(cx).buffer().revision() {
            return;
        }
        if let Some(m) = results.matches.get(index) {
            let range = m.start..m.end;
            self.buffer.search.selected = Some(index);
            self.buffer.search.notice = format!(
                "{} of {}{}",
                index + 1,
                results.matches.len(),
                if results.truncated { "+" } else { "" }
            );
            self.buffer.editor.update(cx, |editor, _| {
                editor.select_range(range);
            });
        } else {
            self.buffer.search.notice = "No matches".into();
        }
    }
    fn navigate_search(&mut self, previous: bool, cx: &mut Context<'_, Self>) {
        let count = self
            .buffer
            .search
            .results
            .as_ref()
            .map_or(0, |r| r.matches.len());
        if count == 0 {
            return;
        }
        let index = self
            .buffer
            .search
            .selected
            .unwrap_or(if previous { 0 } else { count - 1 });
        self.select_search_match(
            if previous {
                (index + count - 1) % count
            } else {
                (index + 1) % count
            },
            cx,
        );
    }
    fn close_search(&mut self, cx: &mut Context<'_, Self>) {
        self.cancel_search(cx);
        self.buffer.search.open = false;
        self.buffer.search.results = None;
        let _ = self.editor_focus.focus(cx);
    }
    pub(super) fn search_bar(
        &self,
        cx: &mut ViewContext<'_, Self>,
    ) -> (rxui::CommandAction<Find>, Element) {
        let find = cx
            .command(Find, |this, _, cx| {
                if !this.buffer.search.open {
                    let editor = this.buffer.editor.read(cx);
                    let range = editor.buffer().selection().range();
                    if !range.is_empty() && range.len() <= model::MAX_QUERY_BYTES {
                        let selected = editor.buffer().text().slice(range);
                        if selected.len_bytes() <= model::MAX_QUERY_BYTES {
                            this.buffer.search.query.text = selected.to_string();
                        }
                    }
                }
                this.buffer.search.open = true;
                let _ = this.buffer.search.focus.focus(cx);
                this.restart_search(cx);
            })
            .label("Find in file")
            .shortcut(Shortcut::primary("f"));
        let close = cx
            .command(Close, |this, _, cx| this.close_search(cx))
            .label("Close")
            .enabled(self.buffer.search.open)
            .shortcut(Shortcut::new(KeyboardKey::Escape, Default::default()));
        let has_matches = self
            .buffer
            .search
            .results
            .as_ref()
            .is_some_and(|r| !r.matches.is_empty());
        let next = cx
            .command(Next, |this, _, cx| this.navigate_search(false, cx))
            .label("Next")
            .enabled(has_matches)
            .shortcut(Shortcut::new(KeyboardKey::Enter, Default::default()))
            .shortcut(Shortcut::primary("g"));
        let previous = cx
            .command(Previous, |this, _, cx| this.navigate_search(true, cx))
            .label("Previous")
            .enabled(has_matches)
            .shortcut(Shortcut::new(
                KeyboardKey::Enter,
                rxui::Modifiers {
                    shift: true,
                    ..Default::default()
                },
            ))
            .shortcut(Shortcut::primary("g").shift());
        let bar = column()
            .key("search-bar")
            .fill_width()
            .flex_shrink(0.)
            .layout(|style| {
                style.display = if self.buffer.search.open {
                    rxui::taffy::Display::Flex
                } else {
                    rxui::taffy::Display::None
                }
            })
            .padding(6.)
            .gap(4.)
            .background(ThemeColor::Surface)
            .on_command(close.clone())
            .on_command(next.clone())
            .on_command(previous.clone())
            .child(
                row()
                    .gap(6.)
                    .fill_width()
                    .child(
                        text_input(self.buffer.search.query.text.clone())
                            .key("search-query")
                            .accessibility_label("Find in file")
                            .placeholder("Find in this file")
                            .focus_handle(self.buffer.search.focus.clone())
                            .flex_grow(1.)
                            .min_width(0.)
                            .on_change(cx.listener(|this, event: &TextChangeEvent, cx| {
                                this.buffer.search.preserve_selection = false;
                                this.buffer.search.query.text = event.value.clone();
                                this.restart_search(cx);
                            })),
                    )
                    .child(previous.button())
                    .child(next.button())
                    .child(close.button()),
            )
            .child(
                row()
                    .gap(12.)
                    .child(
                        checkbox(self.buffer.search.query.case_sensitive, "Match case")
                            .key("search-case")
                            .on_change(cx.listener(|this, event: &ToggleEvent, cx| {
                                this.buffer.search.query.case_sensitive = event.checked;
                                this.restart_search(cx);
                            })),
                    )
                    .child(
                        checkbox(self.buffer.search.query.whole_word, "Whole word")
                            .key("search-word")
                            .on_change(cx.listener(|this, event: &ToggleEvent, cx| {
                                this.buffer.search.query.whole_word = event.checked;
                                this.restart_search(cx);
                            })),
                    )
                    .child(
                        label(self.buffer.search.notice.clone())
                            .key("search-status")
                            .font_size(12.)
                            .color(ThemeColor::TextMuted),
                    ),
            );
        (find, bar)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    #[test]
    fn find_focus_navigation_wrap_and_stale_revision_do_not_edit_the_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "foo bar foo").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let editor = runtime.update(|cx| cx.new(|_| editor));
        let document =
            runtime.update(|cx| cx.new(|cx| Document::new(editor.clone(), file, path, cx)));
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        ui.prepare(&mut runtime, [800., 400.], &mut Measure)
            .unwrap();
        let source = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        ui.focus(source);
        let modifiers = Shortcut::primary("f").modifiers;
        ui.key(
            &mut runtime,
            KeyEvent {
                key: KeyboardKey::Character("f".into()),
                modifiers,
                pressed: true,
                repeat: false,
            },
        )
        .unwrap();
        ui.prepare(&mut runtime, [800., 400.], &mut Measure)
            .unwrap();
        let input = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("search-query")))
            .unwrap()
            .id;
        assert_eq!(
            ui.focused_element(),
            Some(input),
            "Cmd/Ctrl+F must immediately focus the input"
        );
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                this.buffer.search.results = Some(Results {
                    id: this.buffer.search.id,
                    revision: 0,
                    matches: vec![
                        model::Match { start: 0, end: 3 },
                        model::Match { start: 8, end: 11 },
                    ],
                    truncated: false,
                });
                this.select_search_match(0, cx);
                this.navigate_search(true, cx);
            })
        });
        runtime.update(|cx| {
            assert_eq!(editor.read(cx).buffer().selection().range(), 8..11);
            assert!(!editor.read(cx).buffer().is_dirty());
            assert!(!editor.read(cx).buffer().can_undo());
            document.update(cx, |this, cx| this.navigate_search(false, cx));
            assert_eq!(editor.read(cx).buffer().selection().range(), 0..3);
            editor.update(cx, |e, _| e.insert("new"));
            let selection = editor.read(cx).buffer().selection();
            document.update(cx, |this, cx| this.navigate_search(false, cx));
            assert_eq!(
                editor.read(cx).buffer().selection(),
                selection,
                "old results must not select new text"
            );
        });
        runtime.flush().unwrap();
        ui.prepare(&mut runtime, [800., 400.], &mut Measure)
            .unwrap();
        ui.key(
            &mut runtime,
            KeyEvent {
                key: KeyboardKey::Escape,
                modifiers: Default::default(),
                pressed: true,
                repeat: false,
            },
        )
        .unwrap();
        ui.prepare(&mut runtime, [800., 400.], &mut Measure)
            .unwrap();
        assert_eq!(ui.focused_element(), Some(source));
        runtime.update(|cx| assert!(!document.read(cx).buffer.search.open));
    }
}
