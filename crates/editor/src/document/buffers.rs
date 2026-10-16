//! Window-owned buffers. Each async callback resolves its stable buffer ID before
//! touching editor state; tab placement is never used as document identity.
use super::{Document, search, semantic};
use crate::file::FileState;
use rxui::{Element, Subscription, prelude::*};
use std::path::PathBuf;
use velk_editor_widget::{Editor, Language};

enum Opened {
    Existing(u64),
    Loaded(Box<(FileState, velk_core::Buffer, PathBuf)>),
}

/// Optional active document. Window-scoped operations inspect the option;
/// buffer-scoped paths may dereference only after establishing an active ID.
#[derive(Default)]
pub(super) struct ActiveBuffer(Option<BufferState>);
impl ActiveBuffer {
    pub fn as_ref(&self) -> Option<&BufferState> {
        self.0.as_ref()
    }
    pub fn as_mut(&mut self) -> Option<&mut BufferState> {
        self.0.as_mut()
    }
    pub fn is_some(&self) -> bool {
        self.0.is_some()
    }
    pub fn take(&mut self) -> Option<BufferState> {
        self.0.take()
    }
    pub fn replace(&mut self, buffer: BufferState) -> Option<BufferState> {
        self.0.replace(buffer)
    }
    pub fn iter(&self) -> std::option::Iter<'_, BufferState> {
        self.0.iter()
    }
}
impl From<Option<BufferState>> for ActiveBuffer {
    fn from(buffer: Option<BufferState>) -> Self {
        Self(buffer)
    }
}
impl std::ops::Deref for ActiveBuffer {
    type Target = BufferState;
    fn deref(&self) -> &BufferState {
        self.0
            .as_ref()
            .expect("buffer-scoped operation requires an active document")
    }
}
impl std::ops::DerefMut for ActiveBuffer {
    fn deref_mut(&mut self) -> &mut BufferState {
        self.0
            .as_mut()
            .expect("buffer-scoped operation requires an active document")
    }
}

pub(super) struct BufferState {
    pub id: u64,
    pub editor: Entity<Editor>,
    pub file: Option<FileState>,
    pub path: PathBuf,
    pub dirty: bool,
    pub disk_changed: bool,
    pub disk_stamp: Option<crate::file::DiskStamp>,
    observed_revision: u64,
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
        file: impl Into<Option<FileState>>,
        path: PathBuf,
        cx: &mut Context<'_, Document>,
    ) -> Self {
        let dirty = editor.read(cx).buffer().is_dirty();
        let observed_revision = editor.read(cx).buffer().revision();
        let subscription = cx
            .observe(&editor, move |this, _, cx| {
                let mut changed = false;
                let mut recovery_changed = false;
                this.with_buffer(id, cx, |this, cx| {
                    let dirty = this.buffer.editor.read(cx).buffer().is_dirty();
                    let revision = this.buffer.editor.read(cx).buffer().revision();
                    recovery_changed =
                        revision != this.buffer.observed_revision || dirty != this.buffer.dirty;
                    this.buffer.observed_revision = revision;
                    let semantic_changed = this.sync_semantics(cx);
                    let search_changed = this.sync_search(cx);
                    changed = this.buffer.dirty != dirty
                        || (!this.background_buffer && (semantic_changed || search_changed));
                    this.buffer.dirty = dirty;
                });
                if recovery_changed {
                    this.schedule_recovery(cx);
                }
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
            file: file.into(),
            path,
            dirty,
            disk_changed: false,
            disk_stamp: None,
            observed_revision,
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

pub(super) struct New;
impl Command for New {}
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
        if !self.has_buffers() {
            return;
        }
        if self.buffer.id == id {
            f(self, cx);
            return;
        }
        let Some(mut buffer) = self.inactive.remove(&id) else {
            return;
        };
        std::mem::swap(self.buffer.as_mut().unwrap(), &mut buffer);
        self.background_buffer = true;
        f(self, cx);
        self.background_buffer = false;
        std::mem::swap(self.buffer.as_mut().unwrap(), &mut buffer);
        self.inactive.insert(id, buffer);
    }
    pub(super) fn ids(&self) -> Vec<u64> {
        self.buffer_order.clone()
    }
    pub(super) fn any_saving(&self) -> bool {
        self.buffer.as_ref().is_some_and(|b| b.saving.is_some())
            || self.inactive.values().any(|b| b.saving.is_some())
    }
    pub(super) fn select_buffer(&mut self, id: u64, cx: &mut Context<'_, Self>) {
        if self.dialog.is_some()
            || self.closing.is_some()
            || self.reloading()
            || self
                .buffer
                .as_ref()
                .is_some_and(|b| b.editor.read(cx).has_composition())
        {
            return;
        }
        if let Some(mut buffer) = self.inactive.remove(&id) {
            self.cancel_search(cx);
            std::mem::swap(self.buffer.as_mut().unwrap(), &mut buffer);
            self.inactive.insert(buffer.id, buffer);
            if self.buffer.search.open {
                self.buffer.search.preserve_selection = true;
                self.restart_search(cx);
            }
            self.title();
            self.schedule_recovery(cx);
            self.restore_focus_pending = true;
        }
    }
    pub(super) fn cycle_buffer(&mut self, backwards: bool, cx: &mut Context<'_, Self>) {
        let ids = self.ids();
        if ids.is_empty() {
            return;
        }
        let index = ids.iter().position(|id| *id == self.buffer.id).unwrap();
        let offset = if backwards { ids.len() - 1 } else { 1 };
        self.select_buffer(ids[(index + offset) % ids.len()], cx);
    }
    pub(super) fn add_buffer(
        &mut self,
        file: impl Into<Option<FileState>>,
        editor: Editor,
        path: PathBuf,
        cx: &mut Context<'_, Self>,
    ) {
        let file = file.into();
        let existing = self
            .buffer
            .iter()
            .chain(self.inactive.values())
            .find(|b| {
                file.as_ref().is_some_and(|f| {
                    b.file
                        .as_ref()
                        .is_some_and(|other| other.path() == f.path())
                })
            })
            .map(|b| b.id);
        if let Some(id) = existing {
            self.select_buffer(id, cx);
            return;
        }
        if self
            .buffer
            .as_ref()
            .is_some_and(|b| b.editor.read(cx).has_composition())
        {
            self.notice(Some(
                "Finish text composition before opening another buffer.".into(),
            ));
            return;
        }
        if self.buffer_order.len() >= 256 {
            self.notice(Some(
                "This workspace already has 256 open documents.".into(),
            ));
            return;
        }
        let editor = cx.new(|_| editor);
        let buffer = BufferState::new(self.next_buffer, editor, file, path, cx);
        self.next_buffer += 1;
        self.buffer_order.push(buffer.id);
        self.cancel_search(cx);
        if let Some(old) = self.buffer.replace(buffer) {
            self.inactive.insert(old.id, old);
        }
        self.window_status = None;
        if self.panels.ui.source.is_none()
            && let Some(file) = &self.buffer.file
        {
            self.panels.ui.source = Some(velk_panels::NativePath::new(file.path()));
            self.refresh_panels(cx);
        }
        self.start_buffer_semantics(cx);
        self.title();
        self.schedule_recovery(cx);
        self.restore_focus_pending = true;
    }
    pub fn restore_buffers(
        &mut self,
        buffers: Vec<crate::session::RestoredBuffer>,
        active_index: usize,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(active) = self.buffer.as_ref().map(|b| b.id) else {
            return;
        };
        for restored in buffers {
            let editor = cx.new(|_| restored.editor);
            let id = if restored.id == 0 {
                self.next_buffer
            } else {
                restored.id
            };
            let mut buffer = BufferState::new(id, editor, restored.file, restored.path, cx);
            buffer.search.open = restored.search_open;
            buffer.search.query = restored.search_query;
            buffer.search.preserve_selection = true;
            self.next_buffer = self.next_buffer.max(buffer.id + 1);
            self.buffer_order.push(buffer.id);
            self.inactive.insert(buffer.id, buffer);
        }
        self.buffer_order.retain(|id| *id != active);
        self.buffer_order
            .insert(active_index.min(self.buffer_order.len()), active);
    }
    pub fn restore_recovered(
        &mut self,
        documents: Vec<crate::recovery::RestoredDocument>,
        workspace: crate::workspace::Workspace,
        order: Vec<u64>,
        next_id: u64,
        cx: &mut Context<'_, Self>,
    ) {
        self.workspace = workspace;
        self.next_buffer = self.next_buffer.max(next_id);
        self.panels.ui.root = self.workspace.root().map(velk_panels::NativePath::new);
        for restored in documents {
            let editor = cx.new(|_| restored.editor);
            let buffer = BufferState::new(restored.id, editor, restored.file, restored.path, cx);
            self.next_buffer = self.next_buffer.max(restored.id + 1);
            self.buffer_order.push(restored.id);
            self.inactive.insert(restored.id, buffer);
        }
        if self.panels.ui.source.is_none() {
            self.panels.ui.source =
                self.buffer
                    .iter()
                    .chain(self.inactive.values())
                    .find_map(|b| {
                        b.file
                            .as_ref()
                            .map(|f| velk_panels::NativePath::new(f.path()))
                    });
        }
        self.buffer_order.sort_by_key(|id| {
            order
                .iter()
                .position(|other| other == id)
                .unwrap_or(order.len())
        });
    }
    pub fn start_all_semantics(&mut self, cx: &mut Context<'_, Self>) {
        for id in self.ids() {
            self.with_buffer(id, cx, |this, cx| this.start_buffer_semantics(cx));
        }
    }
    pub(super) fn start_buffer_semantics(&mut self, cx: &mut Context<'_, Self>) {
        if self.buffer.file.is_some()
            && Language::for_path(&self.buffer.path) == Some(Language::Rust)
            && let Some(plugins) = &self.plugins
        {
            match plugins.client.semantic_service() {
                Ok(service) => self.enable_semantics(service, cx),
                Err(error) => self.notice(Some(format!("Semantic service unavailable: {error}"))),
            }
        }
    }
    pub(super) fn new_buffer(&mut self, cx: &mut Context<'_, Self>) {
        if self.busy() || self.closing.is_some() {
            return;
        }
        self.add_buffer(
            None,
            Editor::new(""),
            PathBuf::from(format!("Untitled {}", self.next_buffer)),
            cx,
        );
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
                Err(error) => this.notice(Some(format!("Open failed: {error}"))),
            }
        }) {
            Ok(task) => self.dialog = Some(task),
            Err(error) => self.notice(Some(format!("Open failed: {error}"))),
        }
    }
    pub(super) fn open_path(&mut self, path: PathBuf, cx: &mut Context<'_, Self>) {
        if self.closing.is_some() || self.reloading() {
            return;
        }
        if self.saving_as || self.dialog.is_some() {
            self.notice(Some(
                "Finish the current dialog or Save As before opening a file.".into(),
            ));
            return;
        }
        if self.opening.is_some() {
            if self.pending_opens.len() >= 32 {
                self.notice(Some("Too many queued file opens; please wait".into()));
            } else if !self.pending_opens.contains(&path) {
                self.pending_opens.push_back(path);
            }
            return;
        }
        let open: Vec<_> = self
            .buffer
            .iter()
            .chain(self.inactive.values())
            .filter_map(|b| {
                b.file
                    .as_ref()
                    .map(|file| (file.path().to_path_buf(), b.id))
            })
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
                            Err(error) => this.notice(Some(format!("Open failed: {error}"))),
                        }
                    }
                    Ok(Err(error)) => this.notice(Some(format!("Open failed: {error}"))),
                    Err(error) => this.notice(Some(format!("Open failed: {error}"))),
                }
                if let Some(next) = this.pending_opens.pop_front() {
                    this.open_path(next, cx);
                }
            },
        ) {
            Ok(task) => self.opening = Some(task),
            Err(error) => {
                self.pending_opens.clear();
                self.notice(Some(format!("Open failed: {error}")));
            }
        }
    }
    pub(super) fn remove_current_buffer(&mut self, cx: &mut Context<'_, Self>) {
        let ids = self.ids();
        if ids.is_empty() {
            return;
        }
        if ids.len() == 1 {
            self.cancel_search(cx);
            self.buffer.take();
            self.buffer_order.clear();
            self.window_status = None;
            self.title();
            self.schedule_recovery(cx);
            self.restore_focus_pending = true;
            return;
        }
        let index = ids.iter().position(|id| *id == self.buffer.id).unwrap();
        let next = if index + 1 < ids.len() {
            ids[index + 1]
        } else {
            ids[index - 1]
        };
        let next = self.inactive.remove(&next).unwrap();
        self.cancel_search(cx);
        self.buffer_order.retain(|id| *id != self.buffer.id);
        self.buffer.replace(next);
        if self.buffer.search.open {
            self.buffer.search.preserve_selection = true;
            self.restart_search(cx);
        }
        self.title();
        self.schedule_recovery(cx);
        self.restore_focus_pending = true;
    }
    fn close_buffer(&mut self, id: u64, cx: &mut Context<'_, Self>) {
        self.select_buffer(id, cx);
        if !self.has_buffers() {
            return;
        }
        if self.buffer.as_ref().is_some_and(|b| b.id == id) {
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
    fn closing_last_document_keeps_a_real_empty_window_and_new_open_reuse_no_ids() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "saved").unwrap();
        let mut runtime = Runtime::new();
        let executor = Arc::new(QueuedExecutor::default());
        runtime.configure_tasks(executor.clone(), || {}).unwrap();
        let root = create(&mut runtime, &path);
        let mut ui = Ui::new(&mut runtime, root.clone()).unwrap();
        prepare(&mut ui, &mut runtime);
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                d.workspace.set_root(directory.path().into());
                d.intent(Intent::Buffer, cx);
                assert!(!d.has_buffers());
                assert!(d.ids().is_empty());
                assert!(d.inactive.is_empty());
                assert!(d.closing.is_none());
                assert_eq!(d.workspace.root(), Some(directory.path()));
                d.intent(Intent::Buffer, cx); // closing an empty tab set is a no-op
                d.save(None, cx);
                d.save_as(None, cx);
                d.save_all(cx);
                d.cycle_buffer(false, cx);
                d.request_hover(cx);
                assert!(d.can_reload(cx));
            })
        });
        prepare(&mut ui, &mut runtime);
        assert!(
            ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("empty-workspace")))
        );
        assert!(
            !ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("editor-surface")))
        );
        assert!(
            !ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("buffer-tab-1")))
        );
        assert_eq!(
            ui.dispatch_command::<super::super::Save>(&mut runtime)
                .unwrap(),
            CommandStatus::Disabled
        );
        assert_eq!(
            ui.dispatch_command::<Close>(&mut runtime).unwrap(),
            CommandStatus::Disabled
        );
        runtime.update(|cx| {
            let action = root.read(cx).focus_restoration().unwrap();
            action.invoke(cx).unwrap();
        });
        prepare(&mut ui, &mut runtime);
        let new_button = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("empty-new")))
            .unwrap()
            .id;
        assert_eq!(ui.focused_element(), Some(new_button));
        assert_eq!(
            ui.dispatch_command::<New>(&mut runtime).unwrap(),
            CommandStatus::Handled
        );
        prepare(&mut ui, &mut runtime);
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                assert_eq!(d.buffer.id, 2);
                assert!(d.buffer.file.is_none());
                d.intent(Intent::Buffer, cx);
                d.open_path(path.clone(), cx);
            })
        });
        executor.complete(&mut runtime);
        prepare(&mut ui, &mut runtime);
        runtime.update(|cx| {
            assert_eq!(root.read(cx).buffer.id, 3);
            assert!(root.read(cx).has_buffers());
        });
        assert!(
            ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("editor-surface")))
        );
    }
    #[test]
    fn closed_last_buffer_rejects_late_observations_and_reports_window_errors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "saved").unwrap();
        let mut runtime = Runtime::new();
        let root = create(&mut runtime, &path);
        let old_editor = runtime.update(|cx| root.read(cx).buffer.editor.clone());
        runtime.update(|cx| root.update(cx, |d, cx| d.intent(Intent::Buffer, cx)));
        runtime.update(|cx| old_editor.update(cx, |editor, _| editor.insert("late")));
        runtime.flush().unwrap();
        runtime.update(|cx| {
            root.update(cx, |d, cx| {
                d.with_buffer(1, cx, |_, _| panic!("closed ID must stay retired"));
                d.notice(Some("Window-level message".into()));
                assert_eq!(d.window_status.as_deref(), Some("Window-level message"));
                d.restore_ui(Default::default());
                assert!(!d.has_buffers());
            })
        });
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
