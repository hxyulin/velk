//! Host-owned chrome. Feature panels will contribute data, not UI-thread callbacks.
use super::{CloseSettings, Document, TogglePlugins, ToggleSettings};
use crate::settings::{self, Preferences, Store};
use rxui::{Element, prelude::*};

#[derive(Default)]
pub(super) struct State {
    pub store: Store,
    pub open: bool,
    saving: Option<Task>,
}

impl Document {
    pub fn configure_shell(&mut self, store: Store) {
        self.shell.store = store;
    }
    pub(super) fn set_settings(&mut self, open: bool, cx: &mut Context<'_, Self>) {
        self.shell.open = open;
        if open {
            self.plugin_menu_open = false;
        } else {
            let _ = self.editor_focus.focus(cx);
        }
    }
    fn persist_shell(&mut self, cx: &mut Context<'_, Self>) {
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
    pub(super) fn activity_bar(
        &self,
        settings: rxui::CommandAction<ToggleSettings>,
        plugins: rxui::CommandAction<TogglePlugins>,
    ) -> Option<Element> {
        self.shell.store.preferences.activity_bar.then(|| {
            column()
                .key("activity-bar")
                .width(72.)
                .fill_height()
                .flex_shrink(0.)
                .padding(4.)
                .gap(4.)
                .background(ThemeColor::Surface)
                .child(column().flex_grow(1.))
                .child(
                    plugins
                        .label("Plugins")
                        .button()
                        .font_size(12.)
                        .variant(rxui::ButtonVariant::Quiet)
                        .fill_width(),
                )
                .child(
                    settings
                        .button()
                        .font_size(12.)
                        .variant(rxui::ButtonVariant::Quiet)
                        .fill_width(),
                )
        })
    }
    pub(super) fn status_bar(
        &self,
        status: String,
        settings: rxui::CommandAction<ToggleSettings>,
        plugins: rxui::CommandAction<TogglePlugins>,
    ) -> Option<Element> {
        self.shell.store.preferences.status_bar.then(|| {
            let mut bar = row()
                .key("status-bar")
                .fill_width()
                .height(30.)
                .flex_shrink(0.)
                .padding_x(6.)
                .gap(8.)
                .background(ThemeColor::Surface)
                .clip()
                .child(
                    label(status)
                        .key("document-status")
                        .font_size(12.)
                        .color(ThemeColor::TextMuted)
                        .flex_shrink(1.)
                        .min_width(0.),
                )
                .child(row().flex_grow(1.).min_width(0.));
            // Keep launchers in one place when the activity bar is visible.
            if !self.shell.store.preferences.activity_bar {
                bar = bar
                    .child(
                        plugins
                            .label("Plugins")
                            .button()
                            .font_size(12.)
                            .variant(rxui::ButtonVariant::Quiet)
                            .key("plugins-button"),
                    )
                    .child(
                        settings
                            .button()
                            .font_size(12.)
                            .variant(rxui::ButtonVariant::Quiet)
                            .key("settings-button"),
                    );
            }
            bar
        })
    }
    pub(super) fn settings_menu(
        &self,
        cx: &mut ViewContext<'_, Self>,
        toggle: rxui::CommandAction<ToggleSettings>,
        close: rxui::CommandAction<CloseSettings>,
    ) -> Option<Element> {
        if !self.shell.open {
            return None;
        }
        let preferences = &self.shell.store.preferences;
        let mut content = column()
            .fill_width()
            .padding(18.)
            .gap(12.)
            .background(ThemeColor::Surface)
            .on_command(toggle)
            .on_command(close.clone())
            .child(label("Settings").font_size(20.))
            .child(label("Appearance").color(ThemeColor::TextMuted))
            .child(
                checkbox(preferences.status_bar, "Show status bar")
                    .key("show-status-bar")
                    .on_change(cx.listener(|this, event: &ToggleEvent, cx| {
                        this.shell.store.preferences.status_bar = event.checked;
                        this.persist_shell(cx);
                    })),
            )
            .child(
                checkbox(preferences.activity_bar, "Show left activity bar")
                    .key("show-activity-bar")
                    .on_change(cx.listener(|this, event: &ToggleEvent, cx| {
                        this.shell.store.preferences.activity_bar = event.checked;
                        this.persist_shell(cx);
                    })),
            )
            .child(
                label("Open Settings anytime with ⌘, on macOS or Ctrl+, elsewhere.")
                    .font_size(12.)
                    .color(ThemeColor::TextMuted),
            );
        if let Some(error) = &self.shell.store.error {
            content = content.child(label(error.clone()).font_size(12.));
        } else if self.shell.saving.is_some() {
            content = content.child(label("Saving settings…").font_size(12.));
        }
        content =
            content.child(
                row()
                    .gap(8.)
                    .child(button("Reset appearance").key("reset-appearance").on_click(
                        cx.listener(|this, _, cx| {
                            // Leave unknown/newer settings intact.
                            let defaults = Preferences::default();
                            this.shell.store.preferences.status_bar = defaults.status_bar;
                            this.shell.store.preferences.activity_bar = defaults.activity_bar;
                            this.persist_shell(cx);
                        }),
                    ))
                    .child(
                        button("Plugins…")
                            .on_click(cx.listener(|this, _, cx| this.set_plugin_menu(true, cx))),
                    )
                    .child(close.label("Close").button().key("settings-close")),
            );
        Some(
            modal(content)
                .key("settings-menu")
                .width(480.)
                .accessibility_label("Settings")
                .on_dismiss(cx.listener(|this, _: &DismissEvent, cx| this.set_settings(false, cx)))
                .into_element(),
        )
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
        click(&mut ui, &mut runtime, "settings-button");
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui), initial);
        assert!(!ui.focus(source));
        click(&mut ui, &mut runtime, "show-activity-bar");
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui).width, initial.width - 72.);
        click(&mut ui, &mut runtime, "show-activity-bar");
        prepare(&mut ui, &mut runtime);
        click(&mut ui, &mut runtime, "show-status-bar");
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui).height, 400.);
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Escape,
            Default::default(),
        );
        prepare(&mut ui, &mut runtime);
        assert_eq!(ui.focused_element(), Some(source));
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
        key(
            &mut ui,
            &mut runtime,
            KeyboardKey::Character(",".into()),
            modifiers,
        );
        prepare(&mut ui, &mut runtime);
        assert!(
            ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("settings-menu")))
        );
        click(&mut ui, &mut runtime, "reset-appearance");
        prepare(&mut ui, &mut runtime);
        click(&mut ui, &mut runtime, "settings-close");
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui), initial);
        assert_eq!(ui.focused_element(), Some(source));
        runtime.update(|cx| {
            assert_eq!(editor.read(cx).buffer().text().to_string(), "original");
            assert!(!editor.read(cx).buffer().is_dirty());
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
                this.shell.store.preferences.activity_bar = true;
                this.persist_shell(cx);
                this.shell.store.preferences.status_bar = false;
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
        let saved: Preferences = serde_json::from_slice(&std::fs::read(settings).unwrap()).unwrap();
        assert!(saved.activity_bar && !saved.status_bar);
        assert!(queue.0.lock().unwrap().is_empty());
    }
}
