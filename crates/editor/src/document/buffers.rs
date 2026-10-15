//! Window-owned buffers. Each async callback resolves its stable buffer ID before
//! touching editor state; tab placement is never used as document identity.
use super::{Document, search, semantic};
use crate::file::FileState;
use rxui::{Element, Subscription, prelude::*};
use std::path::PathBuf;
use velk_editor_widget::{Editor, Language};

enum Opened {
    Existing(u64),
    Loaded(Box<(FileState, velk_editor_widget::Buffer, PathBuf)>),
}

pub(super) struct BufferState {
    pub id: u64,
    pub editor: Entity<Editor>,
    pub file: FileState,
    pub path: PathBuf,
    pub dirty: bool,
    pub status: Option<String>,
    pub saving: Option<Task>,
    pub semantic: Option<semantic::State>,
    pub search: search::State,
    _subscription: Subscription,
}
impl BufferState {
    pub fn new(
        id: u64,
        editor: Entity<Editor>,
        file: FileState,
        path: PathBuf,
        cx: &mut Context<'_, Document>,
    ) -> Self {
        let dirty = editor.read(cx).buffer().is_dirty();
        let subscription = cx
            .observe(&editor, move |this, _, cx| {
                let mut changed = false;
                this.with_buffer(id, cx, |this, cx| {
                    let dirty = this.buffer.editor.read(cx).buffer().is_dirty();
                    let semantic_changed = this.sync_semantics(cx);
                    let search_changed = this.sync_search(cx);
                    changed = this.buffer.dirty != dirty
                        || (!this.background_buffer && (semantic_changed || search_changed));
                    this.buffer.dirty = dirty;
                });
                if changed {
                    this.title();
                } else {
                    cx.unchanged();
                }
            })
            .expect("editor entity is live");
        Self {
            id,
            editor,
            file,
            path,
            dirty,
            status: None,
            saving: None,
            semantic: None,
            search: search::State::default(),
            _subscription: subscription,
        }
    }
}
impl Drop for BufferState {
    fn drop(&mut self) {
        if let Some(state) = &self.semantic {
            state.service.shutdown();
        }
    }
}

pub(super) struct Open;
impl Command for Open {}
pub(super) struct Close;
impl Command for Close {}
pub(super) struct Next;
impl Command for Next {}
pub(super) struct Previous;
impl Command for Previous {}

impl Document {
    // Runs a buffer-local operation without changing visible selection. Inactive
    // buffers retain their entities, subscriptions, services and pending tasks.
    pub(super) fn with_buffer(
        &mut self,
        id: u64,
        cx: &mut Context<'_, Self>,
        f: impl FnOnce(&mut Self, &mut Context<'_, Self>),
    ) {
        if self.buffer.id == id {
            f(self, cx);
            return;
        }
        let Some(mut buffer) = self.inactive.remove(&id) else {
            return;
        };
        std::mem::swap(&mut self.buffer, &mut buffer);
        self.background_buffer = true;
        f(self, cx);
        self.background_buffer = false;
        std::mem::swap(&mut self.buffer, &mut buffer);
        self.inactive.insert(id, buffer);
    }
    pub(super) fn ids(&self) -> Vec<u64> {
        self.buffer_order.clone()
    }
    pub(super) fn any_saving(&self) -> bool {
        self.buffer.saving.is_some() || self.inactive.values().any(|b| b.saving.is_some())
    }
    pub(super) fn select_buffer(&mut self, id: u64, cx: &mut Context<'_, Self>) {
        if self.dialog.is_some()
            || self.closing.is_some()
            || self.reloading()
            || self.buffer.editor.read(cx).has_composition()
        {
            return;
        }
        if let Some(mut buffer) = self.inactive.remove(&id) {
            self.cancel_search(cx);
            std::mem::swap(&mut self.buffer, &mut buffer);
            self.inactive.insert(buffer.id, buffer);
            if self.buffer.search.open {
                self.buffer.search.preserve_selection = true;
                self.restart_search(cx);
            }
            self.title();
            self.restore_focus_pending = true;
        }
    }
    pub(super) fn cycle_buffer(&mut self, backwards: bool, cx: &mut Context<'_, Self>) {
        let ids = self.ids();
        let index = ids.iter().position(|id| *id == self.buffer.id).unwrap();
        let offset = if backwards { ids.len() - 1 } else { 1 };
        self.select_buffer(ids[(index + offset) % ids.len()], cx);
    }
    pub(super) fn add_buffer(
        &mut self,
        file: FileState,
        editor: Editor,
        path: PathBuf,
        cx: &mut Context<'_, Self>,
    ) {
        let existing = std::iter::once(&self.buffer)
            .chain(self.inactive.values())
            .find(|b| b.file.path() == file.path())
            .map(|b| b.id);
        if let Some(id) = existing {
            self.select_buffer(id, cx);
            return;
        }
        if self.buffer.editor.read(cx).has_composition() {
            self.buffer.status =
                Some("Finish text composition before opening another buffer.".into());
            return;
        }
        let editor = cx.new(|_| editor);
        let buffer = BufferState::new(self.next_buffer, editor, file, path, cx);
        self.next_buffer += 1;
        self.buffer_order.push(buffer.id);
        self.cancel_search(cx);
        let old = std::mem::replace(&mut self.buffer, buffer);
        self.inactive.insert(old.id, old);
        self.start_buffer_semantics(cx);
        self.title();
        self.restore_focus_pending = true;
    }
    pub fn restore_buffers(
        &mut self,
        buffers: Vec<crate::session::RestoredBuffer>,
        active_index: usize,
        cx: &mut Context<'_, Self>,
    ) {
        let active = self.buffer.id;
        for restored in buffers {
            let editor = cx.new(|_| restored.editor);
            let mut buffer =
                BufferState::new(self.next_buffer, editor, restored.file, restored.path, cx);
            buffer.search.open = restored.search_open;
            buffer.search.query = restored.search_query;
            buffer.search.preserve_selection = true;
            self.next_buffer += 1;
            self.buffer_order.push(buffer.id);
            self.inactive.insert(buffer.id, buffer);
        }
        self.buffer_order.retain(|id| *id != active);
        self.buffer_order
            .insert(active_index.min(self.buffer_order.len()), active);
    }
    pub fn start_all_semantics(&mut self, cx: &mut Context<'_, Self>) {
        for id in self.ids() {
            self.with_buffer(id, cx, |this, cx| this.start_buffer_semantics(cx));
        }
    }
    pub(super) fn start_buffer_semantics(&mut self, cx: &mut Context<'_, Self>) {
        if Language::for_path(&self.buffer.path) == Some(Language::Rust)
            && let Some(plugins) = &self.plugins
        {
            match plugins.client.semantic_service() {
                Ok(service) => self.enable_semantics(service, cx),
                Err(error) => {
                    self.buffer.status = Some(format!("Semantic service unavailable: {error}"))
                }
            }
        }
    }
    pub(super) fn open_file(&mut self, cx: &mut Context<'_, Self>) {
        if self.busy() || self.opening.is_some() {
            return;
        }
        let Some(window) = &self.window else {
            return;
        };
        let options = FileDialog::new().parent(window).title("Open file");
        match cx.pick_file(options, |this, result, cx| {
            this.dialog = None;
            match result {
                Ok(Some(path)) => this.open_path(path, cx),
                Ok(None) => {}
                Err(error) => this.buffer.status = Some(format!("Open failed: {error}")),
            }
        }) {
            Ok(task) => self.dialog = Some(task),
            Err(error) => self.buffer.status = Some(format!("Open failed: {error}")),
        }
    }
    pub(super) fn open_path(&mut self, path: PathBuf, cx: &mut Context<'_, Self>) {
        if self.closing.is_some() || self.reloading() {
            return;
        }
        if self.opening.is_some() {
            if self.pending_opens.len() >= 32 {
                self.buffer.status = Some("Too many queued file opens; please wait".into());
            } else if !self.pending_opens.contains(&path) {
                self.pending_opens.push_back(path);
            }
            return;
        }
        let open: Vec<_> = std::iter::once(&self.buffer)
            .chain(self.inactive.values())
            .map(|b| (b.file.path().to_path_buf(), b.id))
            .collect();
        match cx.try_spawn_blocking(
            move || {
                let resolved = std::fs::canonicalize(&path)?;
                if let Some((_, id)) = open.iter().find(|(path, _)| *path == resolved) {
                    return Ok(Opened::Existing(*id));
                }
                let (file, buffer) = FileState::read(&resolved)?;
                Ok::<_, std::io::Error>(Opened::Loaded(Box::new((file, buffer, path))))
            },
            |this, result, cx| {
                this.opening = None;
                match result {
                    Ok(Ok(Opened::Existing(id))) => this.select_buffer(id, cx),
                    Ok(Ok(Opened::Loaded(loaded))) => {
                        let (file, buffer, path) = *loaded;
                        let mut editor = Editor::from_buffer(buffer);
                        match editor.set_language(Language::for_path(&path)) {
                            Ok(()) => this.add_buffer(file, editor, path, cx),
                            Err(error) => {
                                this.buffer.status = Some(format!("Open failed: {error}"))
                            }
                        }
                    }
                    Ok(Err(error)) => this.buffer.status = Some(format!("Open failed: {error}")),
                    Err(error) => this.buffer.status = Some(format!("Open failed: {error}")),
                }
                if let Some(next) = this.pending_opens.pop_front() {
                    this.open_path(next, cx);
                }
            },
        ) {
            Ok(task) => self.opening = Some(task),
            Err(error) => {
                self.pending_opens.clear();
                self.buffer.status = Some(format!("Open failed: {error}"));
            }
        }
    }
    pub(super) fn remove_current_buffer(&mut self, cx: &mut Context<'_, Self>) {
        let ids = self.ids();
        let index = ids.iter().position(|id| *id == self.buffer.id).unwrap();
        let next = if index + 1 < ids.len() {
            ids[index + 1]
        } else {
            ids[index - 1]
        };
        let next = self.inactive.remove(&next).unwrap();
        self.cancel_search(cx);
        self.buffer_order.retain(|id| *id != self.buffer.id);
        self.buffer = next;
        if self.buffer.search.open {
            self.buffer.search.preserve_selection = true;
            self.restart_search(cx);
        }
        self.title();
        self.restore_focus_pending = true;
    }
    fn close_buffer(&mut self, id: u64, cx: &mut Context<'_, Self>) {
        self.select_buffer(id, cx);
        if self.buffer.id == id {
            self.intent(super::Intent::Buffer, cx);
        }
    }
    /// Buffer tabs as title-bar pills or as underlined editor tabs. A dirty
    /// buffer shows a dot where the close button appears on hover.
    pub(super) fn buffer_tabs(&self, pills: bool, cx: &mut ViewContext<'_, Self>) -> Element {
        let close_hint = format!("Close buffer ({})", super::palette::primary("W"));
        let tabs = self
            .ids()
            .into_iter()
            .map(|id| {
                let b = if id == self.buffer.id {
                    &self.buffer
                } else {
                    &self.inactive[&id]
                };
                let name = b
                    .path
                    .file_name()
                    .unwrap_or(b.path.as_os_str())
                    .to_string_lossy()
                    .into_owned();
                let active = id == self.buffer.id;
                let hovered = self.shell.hovered_tab == Some(id);
                let trailing = if hovered || (active && !b.dirty) {
                    tooltip(
                        cx,
                        button(super::icons::Icon::Close.element(12.))
                            .key(format!("buffer-close-{id}"))
                            .accessibility_label(format!("Close {name}"))
                            .variant(ButtonVariant::Quiet)
                            .size(16., 16.)
                            .padding(0.)
                            .radius(4.)
                            .color(ThemeColor::TextMuted)
                            .justify_content(rxui::taffy::prelude::JustifyContent::CENTER)
                            .align_items(rxui::taffy::prelude::AlignItems::CENTER)
                            .on_click(cx.listener(move |this, _, cx| this.close_buffer(id, cx))),
                        close_hint.clone(),
                    )
                } else if b.dirty {
                    row()
                        .key(format!("buffer-dirty-{id}"))
                        .size(16., 16.)
                        .child(
                            row()
                                .size(7., 7.)
                                .margin(4.5)
                                .radius(3.5)
                                .background(ThemeColor::Accent),
                        )
                } else {
                    row().size(16., 16.)
                };
                let caption = button(name.as_str())
                    .on_click(cx.listener(move |this, _, cx| this.select_buffer(id, cx)))
                    .key(format!("buffer-tab-{id}"))
                    .accessibility_label(format!(
                        "{}{}{}",
                        b.path.display(),
                        if b.dirty { ", modified" } else { "" },
                        if active { " (active)" } else { "" }
                    ))
                    .accessibility_selected(active)
                    .variant(ButtonVariant::Quiet)
                    .background([0.; 4])
                    .color(if active {
                        ThemeColor::Text
                    } else {
                        ThemeColor::TextMuted
                    })
                    .fill_height()
                    .padding_y(0.)
                    .padding_x(0.);
                let tab = row()
                    .key(format!("buffer-pill-{id}"))
                    .flex_shrink(0.)
                    .gap(7.)
                    .align_items(rxui::taffy::prelude::AlignItems::CENTER)
                    .on_hover(cx.listener(move |this, event: &HoverEvent, cx| {
                        let next = if event.hovered {
                            Some(id)
                        } else {
                            this.shell.hovered_tab.filter(|h| *h != id)
                        };
                        if this.shell.hovered_tab == next {
                            cx.unchanged();
                        }
                        this.shell.hovered_tab = next;
                    }))
                    .child(caption)
                    .child(trailing);
                if pills {
                    tab.height(28.)
                        .padding_x(10.)
                        .padding_y(0.)
                        .radius(6.)
                        .background(if active {
                            ThemeColor::Raised
                        } else if hovered {
                            ThemeColor::Custom(crate::theme::HOVER)
                        } else {
                            ThemeColor::Background
                        })
                } else {
                    tab.height(32.)
                        .padding_x(12.)
                        .padding_y(0.)
                        .border_bottom(2.)
                        .border_color(if active {
                            ThemeColor::Accent
                        } else {
                            ThemeColor::Background
                        })
                }
                .into_element()
            })
            .collect::<Vec<_>>();
        row()
            .key("buffer-tabs")
            .min_width(0.)
            .flex_shrink(1.)
            .gap(2.)
            .align_items(rxui::taffy::prelude::AlignItems::CENTER)
            .scroll_x()
            .children(tabs)
            .into_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Intent, tests::QueuedExecutor};
    use rxui::{KeyEvent, Runtime, Ui};
    use std::sync::Arc;

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
    fn prepare(ui: &mut Ui<Document>, runtime: &mut Runtime) {
        runtime.flush().unwrap();
        ui.prepare(runtime, [800., 500.], &mut Measure).unwrap();
    }
    fn create(runtime: &mut Runtime, path: &std::path::Path) -> Entity<Document> {
        let (file, editor) = FileState::open(path).unwrap();
        runtime.update(|cx| {
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, path.to_path_buf(), cx))
        })
    }
    #[test]
    fn saves_finish_in_their_own_buffers_after_switching_and_preserve_newer_edits() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "aaa").unwrap();
        std::fs::write(&b, "bbb").unwrap();
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = create(&mut runtime, &a);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.buffer.editor.update(cx, |e, _| e.insert("A"));
                d.save(None, cx);
                let (file, editor) = FileState::open(&b).unwrap();
                d.add_buffer(file, editor, b.clone(), cx);
                d.buffer.editor.update(cx, |e, _| e.insert("B"));
                d.save(None, cx);
                d.select_buffer(1, cx);
                d.buffer.editor.update(cx, |e, _| e.insert("new"));
                assert_eq!(d.buffer.id, 1);
                assert_eq!(d.inactive[&2].path, b);
            })
        });
        executor.complete(&mut runtime);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "Aaaa");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "Bbbb");
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                assert_eq!(d.buffer.id, 1);
                assert!(d.buffer.dirty);
                assert!(d.buffer.saving.is_none());
                assert!(!d.inactive[&2].dirty);
                assert!(d.inactive[&2].saving.is_none());
                assert_eq!(
                    d.buffer.editor.read(cx).buffer().text().to_string(),
                    "Anewaaa"
                );
                d.save(None, cx);
                d.select_buffer(2, cx);
            })
        });
        executor.complete(&mut runtime);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "Anewaaa");
        runtime.update(|cx| {
            let d = document.read(cx);
            assert_eq!(d.buffer.id, 2);
            assert!(!d.inactive[&1].dirty);
            assert!(d.inactive[&1].editor.read(cx).buffer().can_undo());
        });
    }
    #[test]
    fn tabs_cycle_preserve_selection_deduplicate_paths_and_close_only_current_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "abcdef").unwrap();
        std::fs::write(&b, "second").unwrap();
        let mut runtime = Runtime::new();
        let document = create(&mut runtime, &a);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.buffer.editor.update(cx, |e, _| {
                    e.select_range(2..4);
                });
                let (file, editor) = FileState::open(&b).unwrap();
                d.add_buffer(file, editor, b.clone(), cx);
            })
        });
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        prepare(&mut ui, &mut runtime);
        for tab in ["buffer-tab-1", "buffer-tab-2"] {
            assert!(ui.elements().any(|e| e.key == Some(&rxui::Key::from(tab))));
        }
        let surface = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        ui.focus(surface);
        ui.key(
            &mut runtime,
            KeyEvent {
                key: KeyboardKey::Tab,
                modifiers: rxui::Modifiers {
                    control: true,
                    ..Default::default()
                },
                pressed: true,
                repeat: false,
            },
        )
        .unwrap();
        prepare(&mut ui, &mut runtime);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                assert_eq!(d.buffer.id, 1);
                assert_eq!(d.buffer.editor.read(cx).buffer().selection().range(), 2..4);
                let (file, editor) = FileState::open(&b).unwrap();
                d.add_buffer(file, editor, b.clone(), cx);
                assert_eq!(d.buffer.id, 2);
                assert_eq!(d.ids(), vec![1, 2]);
                d.buffer.editor.update(cx, |e, _| e.insert("unsaved"));
                d.select_buffer(1, cx);
                d.intent(Intent::Buffer, cx);
                assert_eq!(d.buffer.id, 2);
                assert_eq!(d.ids(), vec![2]);
                assert!(d.buffer.editor.read(cx).buffer().is_dirty());
                let mut called = false;
                d.with_buffer(1, cx, |_, _| called = true);
                assert!(!called, "late completion cannot target a closed buffer");
            })
        });
        prepare(&mut ui, &mut runtime);
        assert!(
            ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("buffer-dirty-2")))
        );
    }
    #[test]
    fn file_open_is_queued_and_failure_keeps_all_existing_buffers() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "first").unwrap();
        std::fs::write(&b, "second").unwrap();
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = create(&mut runtime, &a);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.open_path(b.clone(), cx);
                assert_eq!(d.ids().len(), 1);
                assert!(d.opening.is_some());
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                assert_eq!(d.ids().len(), 2);
                assert_eq!(d.buffer.path, b);
                d.open_path(dir.path().join("missing"), cx);
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            let d = document.read(cx);
            assert_eq!(d.ids().len(), 2);
            assert_eq!(d.buffer.path, b);
            assert!(d.buffer.status.as_deref().unwrap().contains("Open failed"));
        });
        std::fs::write(&b, [0xff]).unwrap();
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.select_buffer(1, cx);
                d.open_path(b.clone(), cx);
            })
        });
        executor.complete(&mut runtime);
        runtime.update(|cx| {
            let d = document.read(cx);
            assert_eq!(d.buffer.id, 2);
            assert_eq!(d.ids().len(), 2);
            assert_eq!(
                d.buffer.editor.read(cx).buffer().text().to_string(),
                "second"
            );
        });
    }
    #[test]
    fn rapid_file_opens_are_drained_after_failures_and_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "first").unwrap();
        std::fs::write(&b, "second").unwrap();
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let document = create(&mut runtime, &a);
        runtime.update(|cx| {
            document.update(cx, |d, cx| {
                d.buffer.editor.update(cx, |e, _| e.insert("unsaved"));
                d.open_path(dir.path().join("missing"), cx);
                d.open_path(b.clone(), cx);
                d.open_path(b.clone(), cx);
                d.open_path(a.clone(), cx);
                assert_eq!(d.pending_opens.len(), 2);
            })
        });
        for _ in 0..3 {
            executor.complete(&mut runtime);
        }
        runtime.update(|cx| {
            let d = document.read(cx);
            assert_eq!(d.ids().len(), 2);
            assert_eq!(d.buffer.id, 1);
            assert!(d.buffer.editor.read(cx).buffer().is_dirty());
            assert!(d.pending_opens.is_empty() && d.opening.is_none());
        });
    }
}
