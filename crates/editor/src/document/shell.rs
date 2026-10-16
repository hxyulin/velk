//! Host-owned chrome. Feature panels will contribute data, not UI-thread callbacks.
use super::{Document, icons::Icon};
use crate::settings::{self, Layout, Store};
use rxui::taffy::prelude::{AlignItems, JustifyContent};
use rxui::{Element, prelude::*};
use velk_panels::Slot;

#[derive(Default)]
pub(super) struct State {
    pub store: Store,
    pub open: bool,
    pub(super) saving: Option<Task>,
    pub(super) hovered_tab: Option<u64>,
    pub(super) page: super::settings_page::Page,
}

pub(super) struct LayoutQuiet;
impl Command for LayoutQuiet {}
pub(super) struct LayoutRail;
impl Command for LayoutRail {}
pub(super) struct LayoutTiled;
impl Command for LayoutTiled {}

// macOS draws the traffic lights over the leading edge of the title bar.
const TITLE_INSET: f32 = if cfg!(target_os = "macos") { 78. } else { 10. };
const ICON_FONT: f32 = 12.;

impl Document {
    pub fn configure_shell(&mut self, store: Store) {
        self.shell.store = store;
        self.show_panels(self.layout().show_panels, None);
    }
    pub(super) fn layout(&self) -> Layout {
        self.shell.store.preferences.resolve_layout().0
    }
    pub(super) fn set_layout(&mut self, name: &str, cx: &mut Context<'_, Self>) {
        self.shell.store.preferences.set_layout(name);
        self.show_panels(self.layout().show_panels, Some(cx));
        self.persist_shell(cx);
    }
    pub(super) fn layout_actions(
        &self,
        cx: &ViewContext<'_, Self>,
    ) -> (
        rxui::CommandAction<LayoutQuiet>,
        rxui::CommandAction<LayoutRail>,
        rxui::CommandAction<LayoutTiled>,
    ) {
        (
            cx.command(LayoutQuiet, |this, _, cx| this.set_layout("quiet", cx))
                .label("Layout: Quiet"),
            cx.command(LayoutRail, |this, _, cx| this.set_layout("rail", cx))
                .label("Layout: Rail"),
            cx.command(LayoutTiled, |this, _, cx| this.set_layout("tiled", cx))
                .label("Layout: Tiled"),
        )
    }
    /// The project folder, or the working directory when none was opened.
    fn project_root(&self) -> Option<std::path::PathBuf> {
        self.panels
            .ui
            .root
            .as_ref()
            .and_then(|p| p.to_path().ok())
            .or_else(|| std::env::current_dir().ok())
    }
    /// The buffer's directory relative to the project, and its file name.
    pub(super) fn path_parts(&self) -> (String, String) {
        let Some(buffer) = self.buffer.as_ref() else {
            return (String::new(), "No documents open".into());
        };
        let path = &buffer.path;
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        let parent = path.parent().unwrap_or(std::path::Path::new(""));
        let parent = self
            .project_root()
            .and_then(|root| parent.strip_prefix(root).ok())
            .unwrap_or(parent);
        let mut directory = parent.display().to_string();
        if !directory.is_empty() {
            directory.push(std::path::MAIN_SEPARATOR);
        }
        (directory, name)
    }
    /// Title bar for buffer tabs and the project name. macOS always draws it
    /// because the window uses a custom title bar there.
    pub(super) fn title_bar(
        &self,
        layout: Layout,
        cx: &mut ViewContext<'_, Self>,
    ) -> Option<Element> {
        let tabs = layout.buffer_tabs == settings::BufferTabs::TitleBar;
        if !tabs && !cfg!(target_os = "macos") {
            return None;
        }
        let project = self
            .project_root()
            .and_then(|root| root.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        Some(
            row()
                .key("title-bar")
                .window_drag_region()
                .fill_width()
                .height(38.)
                .flex_shrink(0.)
                .padding_y(0.)
                .padding_x(0.)
                .gap(2.)
                .align_items(AlignItems::CENTER)
                .background(ThemeColor::Background)
                .border_bottom(1.)
                .border_color(ThemeColor::Divider)
                .child(row().width(TITLE_INSET).flex_shrink(0.))
                .children(tabs.then(|| self.buffer_tabs(true, cx)))
                .child(row().flex_grow(1.).min_width(12.))
                .child(
                    label(project)
                        .key("title-project")
                        .font_size(12.)
                        .color(ThemeColor::TextDisabled)
                        .flex_shrink(0.),
                )
                .child(row().width(12.).flex_shrink(0.))
                .into_element(),
        )
    }
    pub(super) fn editor_tabs(
        &self,
        layout: Layout,
        cx: &mut ViewContext<'_, Self>,
    ) -> Option<Element> {
        (layout.buffer_tabs == settings::BufferTabs::Editor).then(|| {
            row()
                .key("editor-tabs")
                .fill_width()
                .height(33.)
                .flex_shrink(0.)
                .padding_x(6.)
                .border_bottom(1.)
                .border_color(ThemeColor::Divider)
                .child(self.buffer_tabs(false, cx))
                .into_element()
        })
    }
    /// One icon per sidebar panel, with Plugins and Settings pinned below.
    pub(super) fn rail(&self, layout: Layout, cx: &mut ViewContext<'_, Self>) -> Option<Element> {
        if !layout.rail {
            return None;
        }
        let open = self.sidebar_panel();
        let item = |key: String,
                    content: Element,
                    active: bool,
                    hint: String,
                    click: Listener<ClickEvent>,
                    cx: &mut ViewContext<'_, Self>| {
            let control = button(content)
                .key(key)
                .accessibility_label(hint.clone())
                .accessibility_selected(active)
                .variant(ButtonVariant::Quiet)
                .size(32., 32.)
                .padding(0.)
                .radius(7.)
                .font_size(ICON_FONT)
                .justify_content(JustifyContent::CENTER)
                .align_items(AlignItems::CENTER)
                .color(if active {
                    ThemeColor::Text
                } else {
                    ThemeColor::TextMuted
                })
                .on_click(click);
            let control = if active {
                control.background(ThemeColor::Custom(crate::theme::HOVER))
            } else {
                control
            };
            row()
                .width(44.)
                .height(32.)
                .flex_shrink(0.)
                .align_items(AlignItems::CENTER)
                .child(
                    row()
                        .width(2.)
                        .height(16.)
                        .radius(1.)
                        .background(if active {
                            ThemeColor::Accent
                        } else {
                            ThemeColor::Surface
                        }),
                )
                .child(row().width(4.))
                .child(tooltip(cx, control, hint))
        };
        let mut rail = column()
            .key("rail")
            .width(44.)
            .fill_height()
            .flex_shrink(0.)
            .padding_y(8.)
            .padding_x(0.)
            .gap(4.)
            .background(ThemeColor::Surface)
            .border_right(1.)
            .border_color(ThemeColor::Divider);
        for (key, title) in self.panel_titles(Slot::Left) {
            let content = match Icon::for_panel(&title) {
                Some(icon) => icon.element(17.),
                None => label(
                    title
                        .chars()
                        .next()
                        .unwrap_or('?')
                        .to_uppercase()
                        .to_string(),
                )
                .font_size(13.),
            };
            let active = open.as_deref() == Some(key.as_str());
            let select = key.clone();
            rail = rail.child(item(
                format!("rail-{key}"),
                content,
                active,
                title,
                cx.listener(move |this, _, cx| this.toggle_sidebar(&select, cx)),
                cx,
            ));
        }
        let plugins_open = self.plugin_menu_open;
        let settings_open = self.shell.open;
        rail = rail
            .child(column().flex_grow(1.))
            .child(item(
                "rail-plugins".into(),
                Icon::Plugins.element(17.),
                plugins_open,
                "Plugins (F4)".into(),
                cx.listener(|this, _, cx| this.set_plugin_menu(!this.plugin_menu_open, cx)),
                cx,
            ))
            .child(item(
                "rail-settings".into(),
                Icon::Settings.element(17.),
                settings_open,
                format!("Settings ({})", super::palette::primary(",")),
                cx.listener(|this, _, cx| this.set_settings(!this.shell.open, cx)),
                cx,
            ));
        Some(rail.into_element())
    }
    /// Global status line: file, transient messages, then editor state.
    pub(super) fn status_line(
        &self,
        layout: Layout,
        cx: &ViewContext<'_, Self>,
    ) -> Option<Element> {
        let (_, layout_error) = self.shell.store.preferences.resolve_layout();
        let message = self
            .buffer
            .as_ref()
            .and_then(|b| b.status.clone())
            .or_else(|| self.window_status.clone())
            .or(layout_error)
            .or_else(|| self.shell.store.error.clone());
        if !layout.status_line {
            return message.map(|message| {
                label(message)
                    .key("document-notice")
                    .font_size(12.)
                    .padding(6.)
                    .color(ThemeColor::TextMuted)
                    .into_element()
            });
        }
        if !self.has_buffers() {
            return Some(
                row()
                    .key("status-line")
                    .fill_width()
                    .height(26.)
                    .padding_x(10.)
                    .background(ThemeColor::Background)
                    .child(
                        label(message.unwrap_or_else(|| "No documents open".into()))
                            .key("document-status")
                            .font_size(12.)
                            .color(ThemeColor::TextMuted),
                    ),
            );
        }
        let (directory, name) = self.path_parts();
        let (diagnostics, semantic) = self.semantic_status();
        let editor = self.buffer.editor.read(cx);
        let text = editor.buffer().text();
        let head = editor.buffer().selection().head.min(text.len_chars());
        let line = text.char_to_line(head);
        let column = head - text.line_to_char(line) + 1;
        let language = match velk_editor_widget::Language::for_path(&self.buffer.path) {
            Some(velk_editor_widget::Language::Rust) => "Rust",
            None => "Plain Text",
        };
        let mut right = row()
            .key("status-right")
            .flex_shrink(0.)
            .gap(16.)
            .align_items(AlignItems::CENTER);
        if diagnostics > 0 {
            right = right.child(
                label(format!("▲ {diagnostics}"))
                    .key("status-diagnostics")
                    .color(ThemeColor::Custom(crate::theme::WARNING)),
            );
        }
        right = right
            .children(semantic.map(|s| label(s).key("status-semantic")))
            .child(label(language).key("status-language"))
            .child(label(format!("{}:{column}", line + 1)).key("status-position"));
        Some(
            row()
                .key("status-line")
                .fill_width()
                .height(26.)
                .flex_shrink(0.)
                .padding_x(10.)
                .padding_y(0.)
                .gap(14.)
                .align_items(AlignItems::CENTER)
                .font_size(12.)
                .color(ThemeColor::TextMuted)
                .background(ThemeColor::Background)
                .border_top(1.)
                .border_color(ThemeColor::Divider)
                .clip()
                .child(
                    row()
                        .key("status-path")
                        .flex_shrink(1.)
                        .min_width(0.)
                        .gap(0.)
                        .align_items(AlignItems::CENTER)
                        .child(label(directory).flex_shrink(1.).min_width(0.))
                        .child(
                            label(name)
                                .key("status-file")
                                .color(ThemeColor::Text)
                                .font_weight(FontWeight::MEDIUM)
                                .flex_shrink(0.),
                        )
                        .children(self.buffer.dirty.then(|| {
                            label("●")
                                .key("status-dirty")
                                .color(ThemeColor::Accent)
                                .padding_x(6.)
                                .flex_shrink(0.)
                        })),
                )
                .child(
                    label(message.unwrap_or_default())
                        .key("document-status")
                        .flex_grow(1.)
                        .flex_shrink(1.)
                        .min_width(0.)
                        .text_align(TextAlign::Center),
                )
                .child(right)
                .into_element(),
        )
    }
    pub(super) fn set_settings(&mut self, open: bool, cx: &mut Context<'_, Self>) {
        self.shell.open = open;
        if open {
            self.plugin_menu_open = false;
            self.palette.open = false;
        } else {
            let _ = self.editor_focus.focus(cx);
        }
    }
    pub(super) fn persist_shell(&mut self, cx: &mut Context<'_, Self>) {
        // Serialize writes and coalesce changes made while an earlier write runs.
        if self.shell.saving.is_some() {
            return;
        }
        let Some(path) = self.shell.store.path.clone() else {
            return;
        };
        let snapshot = self.shell.store.preferences.clone();
        let saving = snapshot.clone();
        match cx.try_spawn_blocking(
            move || settings::save(&path, &saving),
            move |this, result, cx| {
                this.shell.saving = None;
                match result {
                    Ok(Ok(())) => {
                        this.shell.store.error = None;
                        if this.shell.store.preferences != snapshot {
                            this.persist_shell(cx);
                        }
                    }
                    Ok(Err(error)) => {
                        this.shell.store.error = Some(format!("Cannot save settings: {error}"))
                    }
                    Err(error) => {
                        this.shell.store.error = Some(format!("Cannot save settings: {error}"))
                    }
                }
            },
        ) {
            Ok(task) => self.shell.saving = Some(task),
            Err(error) => self.shell.store.error = Some(format!("Cannot save settings: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxui::{BackgroundFuture, BlockingJob, KeyEvent, Runtime, SpawnError, TaskExecutor, Ui};
    use std::sync::{Arc, Mutex};

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
    fn key(
        ui: &mut Ui<Document>,
        runtime: &mut Runtime,
        key: KeyboardKey,
        modifiers: rxui::Modifiers,
    ) {
        ui.key(
            runtime,
            KeyEvent {
                key,
                modifiers,
                pressed: true,
                repeat: false,
            },
        )
        .unwrap();
    }
    fn click(ui: &mut Ui<Document>, runtime: &mut Runtime, name: &str) {
        let bounds = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from(name)))
            .unwrap()
            .bounds;
        let position = [bounds.x + bounds.width / 2., bounds.y + bounds.height / 2.];
        for down in [true, false] {
            let event = if down {
                rxui::PointerEvent::Down {
                    position,
                    button: rxui::PointerButton::Primary,
                    modifiers: Default::default(),
                }
            } else {
                rxui::PointerEvent::Up {
                    position,
                    button: rxui::PointerButton::Primary,
                    modifiers: Default::default(),
                }
            };
            ui.pointer(runtime, event).unwrap();
        }
    }
    #[test]
    fn settings_preserve_layout_toggle_chrome_and_remain_reachable_without_bars() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let editor = runtime.update(|cx| cx.new(|_| editor));
        let document =
            runtime.update(|cx| cx.new(|cx| Document::new(editor.clone(), file, path, cx)));
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        let prepare = |ui: &mut Ui<Document>, runtime: &mut Runtime| {
            ui.prepare(runtime, [640., 400.], &mut Measure).unwrap()
        };
        prepare(&mut ui, &mut runtime);
        let bounds = |ui: &Ui<Document>| {
            ui.elements()
                .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
                .unwrap()
                .bounds
        };
        let initial = bounds(&ui);
        let source = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        assert!(ui.focus(source));
        let modifiers = if cfg!(target_os = "macos") {
            rxui::Modifiers {
                meta: true,
                ..Default::default()
            }
        } else {
            rxui::Modifiers {
                control: true,
                ..Default::default()
            }
        };
        let open = |ui: &mut Ui<Document>, runtime: &mut Runtime| {
            key(ui, runtime, KeyboardKey::Character(",".into()), modifiers);
            prepare(ui, runtime);
        };
        open(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui), initial);
        assert!(!ui.focus(source));
        click(&mut ui, &mut runtime, "setting-rail");
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui).width, initial.width - 44.);
        click(&mut ui, &mut runtime, "setting-rail");
        prepare(&mut ui, &mut runtime);
        click(&mut ui, &mut runtime, "setting-status_line");
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui).height, initial.height + 26.);
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Escape,
            Default::default(),
        );
        prepare(&mut ui, &mut runtime);
        assert_eq!(ui.focused_element(), Some(source));
        open(&mut ui, &mut runtime);
        assert!(
            ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("settings-menu")))
        );
        let has = |ui: &Ui<Document>, name: &str| {
            ui.elements().any(|e| e.key == Some(&rxui::Key::from(name)))
        };
        // Only the changed row offers a reset; resetting restores its default.
        assert!(has(&ui, "reset-status_line") && !has(&ui, "reset-rail"));
        click(&mut ui, &mut runtime, "reset-status_line");
        prepare(&mut ui, &mut runtime);
        assert!(!has(&ui, "reset-status_line"));
        runtime.update(|cx| {
            let preferences = &document.read(cx).shell.store.preferences;
            assert!(preferences.resolve_layout().0.status_line);
        });
        click(&mut ui, &mut runtime, "settings-close");
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui), initial);
        assert_eq!(ui.focused_element(), Some(source));
        runtime.update(|cx| {
            assert_eq!(editor.read(cx).buffer().text().to_string(), "original");
            assert!(!editor.read(cx).buffer().is_dirty());
        });
    }
    #[test]
    fn presets_place_tabs_rail_and_panes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let editor = runtime.update(|cx| cx.new(|_| editor));
        let document = runtime.update(|cx| cx.new(|cx| Document::new(editor, file, path, cx)));
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        let prepare = |ui: &mut Ui<Document>, runtime: &mut Runtime| {
            ui.prepare(runtime, [800., 500.], &mut Measure).unwrap();
            ui.elements()
                .filter_map(|e| e.key.map(|k| format!("{k:?}")))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let has = |keys: &str, key: &str| keys.contains(&format!("{:?}", rxui::Key::from(key)));
        let keys = prepare(&mut ui, &mut runtime);
        // Quiet: tabs in the title bar, no rail, no text launchers.
        assert!(has(&keys, "title-bar") && has(&keys, "status-line"));
        assert!(has(&keys, "buffer-tab-1") && !has(&keys, "editor-tabs"));
        assert!(!has(&keys, "rail") && !has(&keys, "settings-button"));
        assert!(!ui.elements().any(|e| e.text == Some("Saved")));
        ui.dispatch_command::<LayoutRail>(&mut runtime).unwrap();
        let keys = prepare(&mut ui, &mut runtime);
        assert!(has(&keys, "rail") && has(&keys, "rail-settings") && has(&keys, "editor-tabs"));
        click(&mut ui, &mut runtime, "rail-settings");
        prepare(&mut ui, &mut runtime);
        assert!(runtime.update(|cx| document.read(cx).shell.open));
        runtime.update(|cx| document.update(cx, |this, cx| this.set_settings(false, cx)));
        prepare(&mut ui, &mut runtime);
        ui.dispatch_command::<LayoutTiled>(&mut runtime).unwrap();
        let keys = prepare(&mut ui, &mut runtime);
        assert!(
            has(&keys, "pane-None") && has(&keys, "winbar") && !has(&keys, "rail"),
            "{keys}"
        );
        runtime.update(|cx| {
            let this = document.read(cx);
            assert_eq!(
                this.shell.store.preferences.layout.as_deref(),
                Some("tiled")
            );
            assert!(this.panels.ui.visible);
        });
    }

    #[derive(Default)]
    struct Queued(Mutex<Vec<BlockingJob>>);
    impl TaskExecutor for Queued {
        fn spawn(&self, _: BackgroundFuture) -> Result<(), SpawnError> {
            Err(SpawnError::Unavailable)
        }
        fn spawn_blocking(&self, job: BlockingJob) -> Result<(), SpawnError> {
            self.0.lock().unwrap().push(job);
            Ok(())
        }
    }
    #[test]
    fn settings_writes_are_serial_and_latest_preferences_eventually_win() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        let settings = dir.path().join("settings.json");
        std::fs::write(&path, "original").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let queue = Arc::new(Queued::default());
        runtime.configure_tasks(queue.clone(), || {}).unwrap();
        let document = runtime.update(|cx| {
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, path, cx))
        });
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                this.shell.store.path = Some(settings.clone());
                this.set_layout("rail", cx);
                let preferences = &mut this.shell.store.preferences;
                preferences.override_layout(|o| o.status_line = Some(false));
                this.persist_shell(cx);
            })
        });
        assert!(!settings.exists());
        for _ in 0..2 {
            let mut jobs = queue.0.lock().unwrap();
            assert_eq!(jobs.len(), 1);
            let job = jobs.pop().unwrap();
            drop(jobs);
            std::thread::spawn(job).join().unwrap();
            runtime.poll_tasks();
            runtime.flush().unwrap();
        }
        let saved: crate::settings::Preferences =
            serde_json::from_slice(&std::fs::read(settings).unwrap()).unwrap();
        let layout = saved.resolve_layout().0;
        assert!(layout.rail && !layout.status_line);
        assert!(queue.0.lock().unwrap().is_empty());
    }
}
