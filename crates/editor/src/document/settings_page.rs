//! Host-rendered settings for core preferences and plugin declarations.
use super::{CloseSettings, Document, ToggleSettings};
use crate::settings::{self, Category, Control, REGISTRY, Setting};
use rxui::{Element, prelude::*, taffy::style::AlignItems};
use serde_json::Value;

struct FocusSearch;
impl Command for FocusSearch {}

pub(super) struct Page {
    query: String,
    category: Category,
    search: FocusHandle,
    opening: Option<Task>,
}
impl Default for Page {
    fn default() -> Self {
        Self {
            query: String::new(),
            category: REGISTRY.first().map_or(Category::Editor, |s| s.category),
            search: FocusHandle::new(),
            opening: None,
        }
    }
}

// Status colors the theme has no tokens for.
pub(super) fn error_color() -> [f32; 4] {
    rgb8(0xDE, 0x8A, 0x82)
}
pub(super) fn success_color() -> [f32; 4] {
    rgb8(0xA6, 0xC4, 0x8A)
}
pub(super) fn error_box(title: &str, message: String) -> Element {
    row()
        .key("error-box")
        .fill_width()
        .gap(12.)
        .padding(10.)
        .padding_x(12.)
        .radius(8.)
        .border(1., rgb8(0x5A, 0x37, 0x34))
        .background(rgb8(0x2A, 0x1F, 0x1F))
        .font_size(12.5)
        .child(label("●").color(error_color()))
        .child(
            column()
                .flex_grow(1.)
                .flex_shrink(1.)
                .min_width(0.)
                .gap(2.)
                .child(
                    label(title)
                        .color(error_color())
                        .font_weight(FontWeight::MEDIUM),
                )
                .child(label(message)),
        )
}
pub(super) fn search_field(value: &str, placeholder: &str, focus: FocusHandle) -> Element {
    text_input(value)
        .focus_handle(focus)
        .placeholder(format!("{placeholder}   /"))
        .accessibility_label(placeholder)
        .height(30.)
        .font_size(13.)
        .radius(7.)
        .background(ThemeColor::Background)
        .border(1., ThemeColor::Border)
}
fn mono(text: impl Into<String>) -> Element {
    label(text)
        .font_family(FontFamily::Monospace)
        .font_size(11.)
        .color(ThemeColor::TextDisabled)
}

impl Document {
    fn update_setting(&mut self, key: &str, value: Option<Value>, cx: &mut Context<'_, Self>) {
        let mut value = value;
        if let Some((plugin, local_key)) = settings::plugin_key(key) {
            let definition = self
                .plugins
                .as_ref()
                .and_then(|state| state.known.get(plugin))
                .and_then(|info| info.metadata.settings.iter().find(|s| s.key == local_key));
            let Some(definition) = definition else {
                self.shell.store.error = Some(format!("Unknown plugin setting: {key}"));
                return;
            };
            if let Some(v) = &value {
                if let Err(error) = definition.validate_value(v) {
                    self.shell.store.error = Some(error);
                    return;
                }
                if v == &definition.default {
                    value = None;
                }
            }
        }
        self.shell.store.error = None;
        match self.shell.store.preferences.set(key, value) {
            Ok(()) => {
                if matches!(
                    key,
                    "indentation_provider" | "pairing_provider" | "keymap_provider"
                ) {
                    self.refresh_editing(cx);
                }
                if key == "syntax_provider" {
                    self.refresh_syntax(cx);
                }
                self.persist_shell(cx);
            }
            Err(error) => self.shell.store.error = Some(format!("Cannot set {key}: {error}")),
        }
    }
    /// Opens settings.json as an ordinary buffer, writing current values first
    /// if the file does not exist yet.
    fn open_settings_file(&mut self, cx: &mut Context<'_, Self>) {
        let Some(path) = self.shell.store.path.clone() else {
            self.shell.store.error = Some("Settings location unavailable.".into());
            return;
        };
        let preferences = self.shell.store.preferences.clone();
        match cx.try_spawn_blocking(
            move || {
                if !path.exists() {
                    settings::save(&path, &preferences)?;
                }
                Ok::<_, String>(path)
            },
            |this, result, cx| {
                this.shell.page.opening = None;
                match result {
                    Ok(Ok(path)) => {
                        this.set_settings(false, cx);
                        this.open_path(path, cx);
                    }
                    Ok(Err(error)) => {
                        this.shell.store.error = Some(format!("Cannot create settings: {error}"))
                    }
                    Err(error) => {
                        this.shell.store.error = Some(format!("Cannot create settings: {error}"))
                    }
                }
            },
        ) {
            Ok(task) => self.shell.page.opening = Some(task),
            Err(error) => self.shell.store.error = Some(format!("Cannot open settings: {error}")),
        }
    }
    fn setting_control(&self, setting: &Setting, cx: &mut ViewContext<'_, Self>) -> Element {
        let key = setting.key.clone();
        let value = self
            .shell
            .store
            .preferences
            .get(&key)
            .or_else(|| setting.plugin.as_ref().map(|_| setting.default.clone()));
        let control_key = format!("setting-{key}");
        match &setting.control {
            Control::Toggle => switch(value.and_then(|v| v.as_bool()).unwrap_or(false), "")
                .key(control_key)
                .accessibility_label(setting.title.clone())
                .on_change(cx.listener(move |this, event: &ToggleEvent, cx| {
                    this.update_setting(&key, Some(Value::Bool(event.checked)), cx)
                }))
                .into_element(),
            Control::Choice(_) | Control::Options(_) => {
                let plugins = self.plugins.as_ref().map_or(&[][..], |s| &s.loaded[..]);
                let options = match &setting.control {
                    Control::Choice(options) => options(plugins),
                    Control::Options(options) => options.clone(),
                    _ => unreachable!(),
                };
                let current = value
                    .as_ref()
                    .and_then(Value::as_str)
                    .or(options.first().map(|(v, _)| v.as_str()))
                    .unwrap_or_default()
                    .to_owned();
                if (2..=3).contains(&options.len()) {
                    let mut segments = row()
                        .key(control_key)
                        .gap(2.)
                        .padding(2.)
                        .radius(7.)
                        .border(1., ThemeColor::Border)
                        .background(ThemeColor::Background)
                        .accessibility_role(SemanticRole::Group)
                        .accessibility_label(setting.title.clone());
                    for (option, caption) in options {
                        let key = key.clone();
                        let on = option == current;
                        segments = segments.child(
                            button(caption)
                                .key(format!("{key}-{option}"))
                                .variant(ButtonVariant::Quiet)
                                .height(22.)
                                .padding_x(10.)
                                .padding_y(0.)
                                .radius(5.)
                                .font_size(12.)
                                .accessibility_selected(on)
                                .color(if on {
                                    ThemeColor::Text
                                } else {
                                    ThemeColor::TextMuted
                                })
                                .background(if on {
                                    ThemeColor::ControlHover
                                } else {
                                    ThemeColor::Background
                                })
                                .on_click(cx.listener(move |this, _, cx| {
                                    this.update_setting(&key, Some(option.clone().into()), cx)
                                })),
                        );
                    }
                    segments
                } else {
                    select_keyed(cx, key.clone())
                        .key(control_key)
                        .accessibility_label(setting.title.clone())
                        .width(220.)
                        .selected(current)
                        .options(options.into_iter().map(|(v, c)| choice(v, c)))
                        .on_change(cx.listener(move |this, event: &ChoiceEvent, cx| {
                            if let rxui::Key::String(value) = &event.key {
                                this.update_setting(&key, Some(value.to_string().into()), cx)
                            }
                        }))
                        .into_element()
                }
            }
            Control::Text => text_input(value.as_ref().and_then(Value::as_str).unwrap_or(""))
                .key(control_key)
                .accessibility_label(setting.title.clone())
                .width(220.)
                .height(28.)
                .on_change(cx.listener(move |this, event: &TextChangeEvent, cx| {
                    this.update_setting(&key, Some(event.value.clone().into()), cx)
                })),
            // Unparsable text is rejected, so the field always holds a number.
            Control::Number => text_input(value.map(|v| v.to_string()).unwrap_or_default())
                .key(control_key)
                .accessibility_label(setting.title.clone())
                .width(72.)
                .height(28.)
                .font_family(FontFamily::Monospace)
                .on_change(cx.listener(move |this, event: &TextChangeEvent, cx| {
                    if let Ok(number) = event.value.trim().parse::<serde_json::Number>() {
                        this.update_setting(&key, Some(Value::Number(number)), cx)
                    }
                })),
        }
    }
    fn setting_row(&self, setting: &Setting, cx: &mut ViewContext<'_, Self>) -> Element {
        let key = setting.key.as_str();
        let reset_key = key.to_owned();
        let modified = self.shell.store.preferences.is_modified(key);
        row()
            .key(key)
            .fill_width()
            .align_items(AlignItems::CENTER)
            .gap(16.)
            .padding_y(12.)
            .padding_x(14.)
            .border_bottom(1.)
            .border_color(ThemeColor::Divider)
            .children(modified.then(|| {
                column()
                    .absolute()
                    .left(0.)
                    .top(14.)
                    .bottom(14.)
                    .width(2.)
                    .radius(1.)
                    .background(ThemeColor::Accent)
            }))
            .child(
                column()
                    .flex_grow(1.)
                    .flex_shrink(1.)
                    .min_width(0.)
                    .gap(2.)
                    .child(
                        row()
                            .gap(8.)
                            .align_items(AlignItems::CENTER)
                            .child(label(setting.title.clone()).font_weight(FontWeight::MEDIUM))
                            .child(mono(key)),
                    )
                    .child(
                        label(setting.description.clone())
                            .font_size(12.5)
                            .color(ThemeColor::TextMuted),
                    ),
            )
            .children(modified.then(|| {
                button("Reset")
                    .key(format!("reset-{key}"))
                    .variant(ButtonVariant::Quiet)
                    .height(28.)
                    .padding_y(0.)
                    .font_size(12.)
                    .color(ThemeColor::TextMuted)
                    .accessibility_label(format!("Reset {}", setting.title))
                    .on_click(
                        cx.listener(move |this, _, cx| this.update_setting(&reset_key, None, cx)),
                    )
            }))
            .child(self.setting_control(setting, cx))
    }
    /// The page itself. `close`, when given, adds a Close button for modal use.
    pub(super) fn settings_page(
        &self,
        cx: &mut ViewContext<'_, Self>,
        close: Option<rxui::CommandAction<CloseSettings>>,
    ) -> Element {
        let page = &self.shell.page;
        let mut registry = REGISTRY.to_vec();
        if let Some(plugins) = &self.plugins {
            registry.extend(settings::plugin_rows(&plugins.known));
        }
        let preferences = &self.shell.store.preferences;
        let focus_search = cx
            .command(FocusSearch, |this, _, cx| {
                let _ = this.shell.page.search.focus(cx);
            })
            .shortcut(Shortcut::new(
                KeyboardKey::Character("/".into()),
                Default::default(),
            ));
        let query = page.query.trim();
        let header = row()
            .fill_width()
            .align_items(AlignItems::CENTER)
            .gap(10.)
            .padding_y(12.)
            .padding_x(14.)
            .border_bottom(1.)
            .border_color(ThemeColor::Divider)
            .child(
                label("Settings")
                    .width(196.)
                    .flex_shrink(0.)
                    .font_size(15.)
                    .font_weight(FontWeight::SEMIBOLD),
            )
            .child(
                search_field(&page.query, "Search settings", page.search.clone())
                    .key("settings-search")
                    .flex_grow(1.)
                    .flex_shrink(1.)
                    .min_width(120.)
                    .max_width(420.)
                    .on_change(cx.listener(|this, event: &TextChangeEvent, _| {
                        this.shell.page.query = event.value.clone();
                    })),
            )
            .child(row().flex_grow(1.))
            .child(
                button("Open settings.json")
                    .key("settings-open-file")
                    .height(28.)
                    .padding_y(0.)
                    .font_size(12.5)
                    .disabled(self.shell.store.path.is_none() || page.opening.is_some())
                    .on_click(cx.listener(|this, _, cx| this.open_settings_file(cx))),
            )
            .children(close.map(|close| {
                close
                    .label("Close")
                    .button()
                    .key("settings-close")
                    .variant(ButtonVariant::Quiet)
                    .height(28.)
                    .padding_y(0.)
                    .font_size(12.5)
            }));
        let mut nav = column()
            .key("settings-nav")
            .width(210.)
            .flex_shrink(0.)
            .fill_height()
            .padding(10.)
            .gap(1.)
            .background(ThemeColor::Surface)
            .border_right(1.)
            .border_color(ThemeColor::Divider)
            .on_command(focus_search.clone());
        for category in Category::ALL {
            let modified = registry
                .iter()
                .filter(|s| s.category == category)
                .map(|s| preferences.is_modified(&s.key))
                .collect::<Vec<_>>();
            if modified.is_empty() {
                continue;
            }
            let changed = modified.into_iter().filter(|m| *m).count();
            let on = query.is_empty() && category == page.category;
            nav = nav.child(
                button(
                    row()
                        .fill_width()
                        .align_items(AlignItems::CENTER)
                        .child(label(category.title()).flex_grow(1.))
                        .children((changed > 0).then(|| {
                            label(changed.to_string())
                                .font_size(11.)
                                .color(ThemeColor::Accent)
                        })),
                )
                .key(format!("settings-nav-{}", category.title()))
                .variant(ButtonVariant::Quiet)
                .fill_width()
                .height(28.)
                .padding_x(10.)
                .padding_y(0.)
                .accessibility_selected(on)
                .color(if on {
                    ThemeColor::Text
                } else {
                    ThemeColor::TextMuted
                })
                .background(if on {
                    ThemeColor::ControlHover
                } else {
                    ThemeColor::Surface
                })
                .on_click(cx.listener(move |this, _, _| {
                    this.shell.page.category = category;
                    this.shell.page.query.clear();
                })),
            );
        }
        let mut content = column()
            .key("settings-rows")
            .flex_grow(1.)
            .flex_shrink(1.)
            .min_width(0.)
            .fill_height()
            .scroll_y()
            .padding_y(16.)
            .padding_x(24.)
            .gap(4.)
            .on_command(focus_search);
        if let Some(error) = &self.shell.store.error {
            content = content.child(error_box("Settings not saved", error.clone()));
        }
        let mut shown = 0;
        for category in Category::ALL {
            let rows: Vec<_> = registry
                .iter()
                .filter(|s| s.category == category)
                .filter(|s| {
                    if query.is_empty() {
                        category == page.category
                    } else {
                        s.matches(query)
                    }
                })
                .collect();
            if rows.is_empty() {
                continue;
            }
            shown += rows.len();
            content = content.child(
                label(category.title())
                    .font_size(15.)
                    .font_weight(FontWeight::SEMIBOLD)
                    .padding_y(6.),
            );
            let mut previous_plugin = None;
            for setting in rows {
                if let Some(id) = &setting.plugin
                    && previous_plugin != Some(id)
                {
                    content = content.child(self.plugin_settings_header(id, cx));
                    previous_plugin = Some(id);
                }
                content = content.child(self.setting_row(setting, cx));
            }
        }
        if shown == 0 {
            content = content.child(
                label(format!("No settings match “{query}”."))
                    .key("settings-empty")
                    .padding_y(12.)
                    .color(ThemeColor::TextMuted),
            );
        }
        let footer = match (&self.shell.store.path, self.shell.saving.is_some()) {
            (_, true) => "Saving…".to_owned(),
            (Some(path), false) => format!("Saved to {}", path.display()),
            (None, false) => "Changes apply to this window only.".to_owned(),
        };
        content = content.child(
            label(footer)
                .font_size(12.)
                .padding_y(10.)
                .color(ThemeColor::TextDisabled),
        );
        column()
            .key("settings-page")
            .fill_width()
            .fill_height()
            .background(ThemeColor::Background)
            .child(header)
            .child(
                row()
                    .fill_width()
                    .flex_grow(1.)
                    .flex_shrink(1.)
                    .min_height(0.)
                    .child(nav)
                    .child(content),
            )
    }
    pub(super) fn open_plugin_settings(&mut self, id: String, cx: &mut Context<'_, Self>) {
        self.set_settings(true, cx);
        self.shell.page.category = Category::Plugins;
        self.shell.page.query = id;
    }
    fn plugin_settings_header(&self, id: &str, cx: &mut ViewContext<'_, Self>) -> Element {
        let state = self.plugins.as_ref().expect("settings plugin state");
        let info = &state.known[id];
        let overrides = self
            .shell
            .store
            .preferences
            .plugins
            .get(id)
            .cloned()
            .unwrap_or_default();
        let resolved = velk_plugin_host::settings::resolve(&info.metadata.settings, &overrides);
        let loaded = state.loaded.iter().find(|p| p.id == id);
        let pending = loaded.is_some_and(|p| {
            resolved.as_ref().is_ok_and(|v| *v != p.settings_values) || p.settings_error.is_some()
        });
        let restart_id = id.to_owned();
        let heading = row()
            .align_items(AlignItems::CENTER)
            .gap(10.)
            .child(
                label(info.metadata.name.clone())
                    .font_weight(FontWeight::SEMIBOLD)
                    .flex_grow(1.),
            )
            .children(pending.then(|| {
                button("Restart to apply")
                    .key(format!("settings-restart-{id}"))
                    .height(28.)
                    .padding_y(0.)
                    .font_size(12.)
                    .disabled(self.plugins_busy() || resolved.is_err())
                    .on_click(cx.listener(move |this, _, cx| {
                        this.plugin_operation(
                            super::plugins::Operation::Restart(Some(restart_id.clone())),
                            cx,
                        )
                    }))
            }));
        let status = if loaded.is_none() {
            "Enable this plugin to apply its settings."
        } else if pending {
            "Changes take effect after restart. Restarting ends this plugin’s active sessions."
        } else {
            "Settings apply when this plugin starts. Restarting ends its active sessions."
        };
        let mut header = column()
            .gap(6.)
            .padding_y(12.)
            .child(heading)
            .child(label(status).font_size(12.).color(ThemeColor::TextMuted));
        if let Err(error) = resolved {
            header = header.child(error_box("Invalid plugin settings", error));
        } else if let Some(error) = loaded.and_then(|p| p.settings_error.as_ref()) {
            header = header.child(error_box("Plugin is using defaults", error.clone()));
        }
        header.into_element()
    }
    pub(super) fn settings_modal(
        &self,
        cx: &mut ViewContext<'_, Self>,
        toggle: rxui::CommandAction<ToggleSettings>,
        close: rxui::CommandAction<CloseSettings>,
    ) -> Option<Element> {
        if !self.shell.open {
            return None;
        }
        let page = self
            .settings_page(cx, Some(close.clone()))
            .on_command(toggle)
            .on_command(close);
        Some(
            modal(page)
                .key("settings-menu")
                .width(860.)
                .height(600.)
                .padding(0.)
                .accessibility_label("Settings")
                .on_dismiss(cx.listener(|this, _: &DismissEvent, cx| this.set_settings(false, cx)))
                .into_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_filters_rows_across_categories() {
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
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "text").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = rxui::Runtime::new();
        let document = runtime.update(|cx| {
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, path, cx))
        });
        let mut ui = rxui::Ui::new(&mut runtime, document.clone()).unwrap();
        let mut rows = |query: &str| {
            runtime.update(|cx| {
                document.update(cx, |this, cx| {
                    this.set_settings(true, cx);
                    this.shell.page.query = query.into();
                })
            });
            ui.prepare(&mut runtime, [1000., 700.], &mut Measure)
                .unwrap();
            let mut keys: Vec<_> = REGISTRY
                .iter()
                .map(|s| s.key.as_str())
                .chain(["settings-empty"])
                .filter(|key| ui.elements().any(|e| e.key == Some(&rxui::Key::from(*key))))
                .collect();
            keys.sort();
            keys
        };
        // An empty query shows the selected category only.
        assert_eq!(rows(""), ["layout", "rail", "status_line"]);
        assert_eq!(rows("backend"), ["search_backend"]);
        assert_eq!(rows("current layout"), ["rail", "status_line"]);
        assert_eq!(rows("no such setting"), ["settings-empty"]);
    }
    #[test]
    #[allow(unsafe_code)]
    fn plugin_settings_render_search_validate_reset_and_survive_unload() {
        use velk_plugin_host::{
            PluginInfo, PluginMetadata,
            settings::{Definition, Kind, Values},
        };
        struct Measure;
        impl rxui::TextMeasure for Measure {
            fn measure(
                &mut self,
                _: rxui::ElementId,
                r: rxui::TextRequest<'_>,
            ) -> Result<[f32; 2], rxui::UiError> {
                Ok([r.text.len() as f32 * 7., 18.])
            }
        }
        let directory = tempfile::tempdir().unwrap();
        // SAFETY: this test-owned directory contains no native libraries.
        let plugins = unsafe { velk_plugin_host::Runtime::start(directory.path().into()) }.unwrap();
        let info = PluginInfo {
            id: "test.files".into(),
            version: "1".into(),
            generation: 1,
            loaded_path: directory.path().into(),
            description: String::new(),
            metadata: PluginMetadata {
                name: "Workspace Browser".into(),
                settings: vec![Definition {
                    key: "show_hidden".into(),
                    title: "Show hidden files".into(),
                    description: "Show dot files.".into(),
                    kind: Kind::Boolean,
                    default: true.into(),
                }],
                ..Default::default()
            },
            settings_values: Values::from([("show_hidden".into(), true.into())]),
            settings_error: None,
            resident_code: false,
            hooks: vec![],
        };
        let mut runtime = rxui::Runtime::new();
        let document = runtime.update(|cx| cx.new(|_| Document::empty(1)));
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                let mut state = super::super::plugins::State::new(
                    plugins.client(),
                    velk_plugin_host::DEFAULT_MARKDOWN_PROVIDER.into(),
                );
                state.loaded.push(info.clone());
                state.known.insert(info.id.clone(), info);
                this.plugins = Some(state);
                this.open_plugin_settings("test.files".into(), cx);
                this.shell.page.query = "Workspace Browser".into();
            })
        });
        let mut ui = rxui::Ui::new(&mut runtime, document.clone()).unwrap();
        ui.prepare(&mut runtime, [1000., 700.], &mut Measure)
            .unwrap();
        let has = |ui: &rxui::Ui<Document>, key: &str| {
            ui.elements().any(|e| e.key == Some(&rxui::Key::from(key)))
        };
        let key = "plugins.test.files.show_hidden";
        assert!(has(&ui, key));
        assert!(!has(&ui, "settings-restart-test.files"));
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                this.update_setting(key, Some(false.into()), cx);
                assert_eq!(this.shell.store.preferences.get(key), Some(false.into()));
                assert_eq!(
                    this.plugins.as_ref().unwrap().loaded[0].settings_values["show_hidden"],
                    true
                );
                this.update_setting(key, Some("wrong type".into()), cx);
                assert_eq!(this.shell.store.preferences.get(key), Some(false.into()));
                assert!(this.shell.store.error.is_some());
            })
        });
        ui.prepare(&mut runtime, [1000., 700.], &mut Measure)
            .unwrap();
        assert!(has(&ui, "settings-restart-test.files"));
        assert!(has(&ui, "reset-plugins.test.files.show_hidden"));
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                this.update_setting(key, None, cx);
                assert!(this.shell.store.preferences.get(key).is_none());
                assert!(this.shell.store.error.is_none());
            })
        });
        ui.prepare(&mut runtime, [1000., 700.], &mut Measure)
            .unwrap();
        assert!(!has(&ui, "settings-restart-test.files"));
        runtime.update(|cx| {
            document.update(cx, |this, _| this.plugins.as_mut().unwrap().loaded.clear())
        });
        ui.prepare(&mut runtime, [1000., 700.], &mut Measure)
            .unwrap();
        assert!(has(&ui, key));
    }
}
