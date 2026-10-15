//! Settings page driven by `settings::REGISTRY`. `settings_page` is a plain
//! element so it can later be mounted as an editor tab; `settings_modal` wraps it.
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
        match self.shell.store.preferences.set(key, value) {
            Ok(()) => self.persist_shell(cx),
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
        let key = setting.key;
        let value = self.shell.store.preferences.get(key);
        let control_key = format!("setting-{key}");
        match setting.control {
            Control::Toggle => switch(value.and_then(|v| v.as_bool()).unwrap_or(false), "")
                .key(control_key)
                .accessibility_label(setting.title)
                .on_change(cx.listener(move |this, event: &ToggleEvent, cx| {
                    this.update_setting(key, Some(Value::Bool(event.checked)), cx)
                }))
                .into_element(),
            Control::Choice(options) => {
                let plugins = self.plugins.as_ref().map_or(&[][..], |s| &s.loaded[..]);
                let options = options(plugins);
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
                        .accessibility_label(setting.title);
                    for (option, caption) in options {
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
                                    this.update_setting(key, Some(option.clone().into()), cx)
                                })),
                        );
                    }
                    segments
                } else {
                    select_keyed(cx, key)
                        .key(control_key)
                        .accessibility_label(setting.title)
                        .width(220.)
                        .selected(current)
                        .options(options.into_iter().map(|(v, c)| choice(v, c)))
                        .on_change(cx.listener(move |this, event: &ChoiceEvent, cx| {
                            if let rxui::Key::String(value) = &event.key {
                                this.update_setting(key, Some(value.to_string().into()), cx)
                            }
                        }))
                        .into_element()
                }
            }
            Control::Text => text_input(value.as_ref().and_then(Value::as_str).unwrap_or(""))
                .key(control_key)
                .accessibility_label(setting.title)
                .width(220.)
                .height(28.)
                .on_change(cx.listener(move |this, event: &TextChangeEvent, cx| {
                    this.update_setting(key, Some(event.value.clone().into()), cx)
                })),
            // Unparsable text is rejected, so the field always holds a number.
            Control::Number => text_input(value.map(|v| v.to_string()).unwrap_or_default())
                .key(control_key)
                .accessibility_label(setting.title)
                .width(72.)
                .height(28.)
                .font_family(FontFamily::Monospace)
                .on_change(cx.listener(move |this, event: &TextChangeEvent, cx| {
                    if let Ok(number) = event.value.trim().parse::<serde_json::Number>() {
                        this.update_setting(key, Some(Value::Number(number)), cx)
                    }
                })),
        }
    }
    fn setting_row(&self, setting: &Setting, cx: &mut ViewContext<'_, Self>) -> Element {
        let key = setting.key;
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
                            .child(label(setting.title).font_weight(FontWeight::MEDIUM))
                            .child(mono(key)),
                    )
                    .child(
                        label(setting.description)
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
                    .on_click(cx.listener(move |this, _, cx| this.update_setting(key, None, cx)))
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
            let modified = REGISTRY
                .iter()
                .filter(|s| s.category == category)
                .map(|s| preferences.is_modified(s.key))
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
            let rows: Vec<_> = REGISTRY
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
            for setting in rows {
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
                .map(|s| s.key)
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
}
