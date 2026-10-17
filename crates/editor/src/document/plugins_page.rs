//! Plugins page: per-plugin state, enable switches and what each plugin adds.
//! `plugins_page` is a plain element; `plugins_modal` wraps it for now.
use super::{
    ClosePlugins, Document, LoadPlugins, TogglePlugins, UnloadPlugins,
    plugins::Operation,
    settings_page::{error_box, error_color, search_field, success_color},
};
use rxui::{Element, prelude::*, taffy::style::AlignItems};
use velk_plugin_host::{PluginInfo, hook_name};

struct FocusFilter;
impl Command for FocusFilter {}

#[derive(Default)]
pub(super) struct Page {
    query: String,
    tree: bool,
    interfaces: bool,
    selected: Option<String>,
    filter: FocusHandle,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Running,
    Off,
    Failed,
}

/// "velk.language-server.rust-analyzer" → "Rust analyzer".
fn display_name(id: &str) -> String {
    let name = id.rsplit('.').next().unwrap_or(id).replace(['-', '_'], " ");
    let mut chars = name.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Host-side description of a plugin's registered hooks.
fn contributions(info: &PluginInfo) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    let mut other = Vec::new();
    if !info.metadata.file_types.is_empty() {
        rows.push((
            "File types",
            info.metadata
                .file_types
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    for hook in &info.hooks {
        let row = match hook.as_str() {
            velk_panels::HOOK => ("Panels", "Window panels (sidebar or bottom drawer)".into()),
            velk_search::HOOK => ("Search", "Current-buffer search backend".into()),
            hook_name::MARKUP_HOOK => ("Rendering", "Markdown for hover documentation".into()),
            hook_name::SEMANTIC_HOOK => {
                ("Services", "Semantic sessions for language servers".into())
            }
            hook_name::LANGUAGE_HOOK => ("Language", "Language server definition".into()),
            hook_name::LIFECYCLE_SNAPSHOT_HOOK
            | hook_name::LIFECYCLE_RESTORE_HOOK
            | hook_name::DIAGNOSTICS_HOOK
            | hook_name::METADATA_HOOK
            | hook_name::SETTINGS_HOOK => continue,
            name => {
                other.push(name.to_owned());
                continue;
            }
        };
        rows.push(row);
    }
    if !other.is_empty() {
        rows.push(("Hooks", other.join("  ")));
    }
    rows
}

/// Render the plugin-owned document as inert styled text, without fetching images
/// or interpreting HTML. Link destinations remain data, as in hover documentation.
fn markdown_body(document: &velk_markup::Document) -> Element {
    rich_text(
        document
            .runs
            .iter()
            .map(|run| {
                let mut span = rxui::span(run.text.clone());
                let style = &run.style;
                if style.bold {
                    span = span.bold();
                }
                if style.italic {
                    span = span.italic();
                }
                if style.code {
                    span = span.monospace();
                }
                if style.strikethrough {
                    span = span.strikethrough();
                }
                if style.heading > 0 {
                    span = span.font_size(24. - f32::from(style.heading.min(6)));
                }
                if style.link.is_some() {
                    span = span.underline().color(ThemeColor::Accent);
                }
                span
            })
            .collect::<Vec<_>>(),
    )
}

fn enabled_dependents(
    id: String,
    inventory: &[velk_plugin_host::PluginSpec],
    disabled: &[String],
) -> Vec<String> {
    let mut selected = vec![id];
    loop {
        let before = selected.len();
        for spec in inventory {
            if !selected.contains(&spec.id)
                && !disabled.contains(&spec.id)
                && spec
                    .dependencies
                    .iter()
                    .any(|dependency| selected.contains(dependency))
            {
                selected.push(spec.id.clone());
            }
        }
        if selected.len() == before {
            return selected;
        }
    }
}

impl Document {
    pub(super) fn set_plugin_enabled(
        &mut self,
        id: String,
        enabled: bool,
        cx: &mut Context<'_, Self>,
    ) {
        let disabled = &mut self.shell.store.preferences.disabled_plugins;
        disabled.retain(|d| *d != id);
        if !enabled {
            disabled.push(id.clone());
        }
        let selected = enabled.then(|| {
            let manifest = velk_plugin_host::Manifest {
                plugins: self
                    .plugins
                    .as_ref()
                    .map(|s| s.inventory.clone())
                    .unwrap_or_default(),
            };
            let effective = manifest
                .resolve_interfaces(&self.shell.store.preferences.interface_bindings)
                .unwrap_or(manifest);
            enabled_dependents(
                id.clone(),
                &effective.plugins,
                &self.shell.store.preferences.disabled_plugins,
            )
        });
        self.persist_shell(cx);
        self.plugin_operation(
            if let Some(selected) = selected {
                Operation::LoadSelected(selected, Vec::new())
            } else {
                Operation::Unload(id)
            },
            cx,
        );
    }
    /// Every plugin worth listing: manifest, loaded, disabled and failed IDs.
    fn plugin_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        let mut add = |id: &String| {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        };
        if let Some(state) = &self.plugins {
            state.available.iter().for_each(&mut add);
            state.loaded.iter().for_each(|p| add(&p.id));
            state.failed.keys().for_each(&mut add);
        }
        self.shell
            .store
            .preferences
            .disabled_plugins
            .iter()
            .for_each(&mut add);
        ids
    }
    fn plugin_status(&self, id: &str) -> Status {
        match &self.plugins {
            Some(s) if s.loaded.iter().any(|p| p.id == id) => Status::Running,
            Some(s) if s.failed.contains_key(id) => Status::Failed,
            _ => Status::Off,
        }
    }
    fn plugin_info(&self, id: &str) -> Option<&PluginInfo> {
        self.plugins.as_ref().and_then(|s| s.known.get(id))
    }
    fn plugin_name(&self, id: &str) -> String {
        self.plugin_info(id)
            .filter(|p| !p.metadata.name.is_empty())
            .map_or_else(|| display_name(id), |p| p.metadata.name.clone())
    }
    fn plugin_install_label(&self, id: &str) -> &'static str {
        use velk_plugin_host::InstallReason;
        match self
            .plugins
            .as_ref()
            .and_then(|state| state.inventory.iter().find(|p| p.id == id))
            .map(|p| p.install_reason)
        {
            Some(InstallReason::Explicit) => "Explicitly installed",
            Some(InstallReason::Bundled) => "Bundled default",
            Some(InstallReason::Dependency) => "Dependency",
            None => "Not in manifest",
        }
    }
    fn plugin_list_row(&self, id: &str, selected: bool, cx: &mut ViewContext<'_, Self>) -> Element {
        let status = self.plugin_status(id);
        let enabled = !self
            .shell
            .store
            .preferences
            .disabled_plugins
            .iter()
            .any(|d| d == id);
        let description = self
            .plugin_info(id)
            .map_or_else(|| id.to_owned(), |p| p.description.clone());
        let dot = column().width(7.).height(7.).radius(3.5).margin_y(6.);
        let dot = match status {
            Status::Running => dot.background(success_color()),
            Status::Failed => dot.background(error_color()),
            Status::Off => dot.border(1., ThemeColor::TextDisabled),
        };
        let select_id = id.to_owned();
        let toggle_id = id.to_owned();
        row()
            .key(format!("plugin-{id}"))
            .fill_width()
            .align_items(AlignItems::START)
            .gap(6.)
            .padding_y(4.)
            .padding_x(6.)
            .border_left(2.)
            .border_color(if selected {
                ThemeColor::Accent
            } else {
                ThemeColor::Surface
            })
            .background(if selected {
                ThemeColor::ControlHover
            } else {
                ThemeColor::Surface
            })
            .child(
                button(
                    row()
                        .fill_width()
                        .gap(10.)
                        .align_items(AlignItems::START)
                        .child(dot)
                        .child(
                            column()
                                .flex_grow(1.)
                                .flex_shrink(1.)
                                .min_width(0.)
                                .clip()
                                .child(label(self.plugin_name(id)).font_weight(FontWeight::MEDIUM))
                                .child(
                                    label(self.plugin_install_label(id))
                                        .font_size(11.)
                                        .color(ThemeColor::TextDisabled),
                                )
                                .child(
                                    label(description)
                                        .font_size(12.)
                                        .color(ThemeColor::TextMuted),
                                ),
                        ),
                )
                .key(format!("plugin-select-{id}"))
                .variant(ButtonVariant::Quiet)
                .flex_grow(1.)
                .flex_shrink(1.)
                .min_width(0.)
                .padding(6.)
                .accessibility_selected(selected)
                .on_click(cx.listener(move |this, _, _| {
                    this.plugin_page.selected = Some(select_id.clone());
                })),
            )
            .child(
                switch(enabled, "")
                    .key(format!("plugin-enabled-{id}"))
                    .accessibility_label(format!("Enable {}", self.plugin_name(id)))
                    .disabled(self.plugins_busy() || self.reloading())
                    .on_change(cx.listener(move |this, event: &ToggleEvent, cx| {
                        this.set_plugin_enabled(toggle_id.clone(), event.checked, cx)
                    })),
            )
    }
    fn plugin_detail(&self, id: &str, cx: &mut ViewContext<'_, Self>) -> Element {
        let status = self.plugin_status(id);
        let info = self.plugin_info(id);
        let enabled = !self
            .shell
            .store
            .preferences
            .disabled_plugins
            .iter()
            .any(|d| d == id);
        let busy = self.plugins_busy() || self.reloading();
        let restart_id = id.to_owned();
        let toggle_id = id.to_owned();
        let mut detail = column()
            .key(format!("plugin-detail-{id}"))
            .fill_width()
            .gap(4.)
            .child(
                label(self.plugin_name(id))
                    .font_size(18.)
                    .font_weight(FontWeight::SEMIBOLD),
            )
            .child(
                label(match info {
                    Some(info) => format!("{id}  {}", info.version),
                    None => id.to_owned(),
                })
                .font_family(FontFamily::Monospace)
                .font_size(12.)
                .color(ThemeColor::TextDisabled),
            )
            .child(
                row()
                    .fill_width()
                    .gap(8.)
                    .padding_y(12.)
                    .child(
                        button("Restart")
                            .key("plugin-restart")
                            .height(28.)
                            .padding_y(0.)
                            .font_size(12.5)
                            .disabled(busy || status != Status::Running)
                            .on_click(cx.listener(move |this, _, cx| {
                                this.plugin_operation(
                                    Operation::Restart(Some(restart_id.clone())),
                                    cx,
                                )
                            })),
                    )
                    .children(info.filter(|p| !p.metadata.settings.is_empty()).map(|_| {
                        let id = id.to_owned();
                        button("Settings")
                            .key("plugin-settings")
                            .height(28.)
                            .padding_y(0.)
                            .font_size(12.5)
                            .on_click(cx.listener(move |this, _, cx| {
                                this.open_plugin_settings(id.clone(), cx)
                            }))
                    }))
                    .child(row().flex_grow(1.))
                    .child(
                        button(if enabled { "Disable" } else { "Enable" })
                            .key("plugin-enable")
                            .height(28.)
                            .padding_y(0.)
                            .font_size(12.5)
                            .disabled(busy)
                            .paint_style(if enabled {
                                PaintStyle::new()
                                    .border_color(rgb8(0x5A, 0x37, 0x34))
                                    .color(error_color())
                            } else {
                                PaintStyle::new()
                            })
                            .on_click(cx.listener(move |this, _, cx| {
                                this.set_plugin_enabled(toggle_id.clone(), !enabled, cx)
                            })),
                    ),
            );
        if let Some(message) = self
            .plugins
            .as_ref()
            .filter(|_| status == Status::Failed)
            .and_then(|s| s.failed.get(id))
        {
            detail = detail.child(error_box("Failed to load", message.clone()));
        }
        detail = detail.child(
            label(self.plugin_install_label(id))
                .font_size(12.)
                .color(ThemeColor::TextMuted),
        );
        if let Some(state) = &self.plugins {
            if let Some(spec) = state.inventory.iter().find(|spec| spec.id == id) {
                detail = detail.child(
                    label("Requires")
                        .font_weight(FontWeight::SEMIBOLD)
                        .padding_y(6.),
                );
                if spec.dependencies.is_empty() {
                    detail = detail.child(label("No plugin dependencies").font_size(12.));
                }
                for dependency in &spec.dependencies {
                    let target = dependency.clone();
                    detail = detail.child(
                        button(format!("{} ({dependency})", self.plugin_name(dependency)))
                            .variant(ButtonVariant::Quiet)
                            .on_click(cx.listener(move |this, _, _| {
                                this.plugin_page.selected = Some(target.clone());
                            })),
                    );
                }
            }
            let consumers: Vec<_> = state
                .inventory
                .iter()
                .filter(|spec| spec.dependencies.iter().any(|dependency| dependency == id))
                .collect();
            if !consumers.is_empty() {
                detail = detail.child(
                    label("Required by")
                        .font_weight(FontWeight::SEMIBOLD)
                        .padding_y(6.),
                );
                for consumer in consumers {
                    let target = consumer.id.clone();
                    detail = detail.child(
                        button(self.plugin_name(&consumer.id))
                            .variant(ButtonVariant::Quiet)
                            .on_click(cx.listener(move |this, _, _| {
                                this.plugin_page.selected = Some(target.clone());
                            })),
                    );
                }
                detail = detail.child(label("Disabling this plugin also stops its dependents. Restarting it restarts active dependents.").font_size(12.).color(ThemeColor::TextMuted));
            }
        }
        if let Some(info) = info {
            if !info.description.is_empty() {
                detail = detail.child(label(info.description.clone()).color(ThemeColor::TextMuted));
            }
            if !info.metadata.readme.is_empty() {
                let readme = self
                    .plugins
                    .as_ref()
                    .and_then(|state| state.readmes.get(id));
                detail = detail.child(
                    readme
                        .map_or_else(|| label(info.metadata.readme.clone()), markdown_body)
                        .key("plugin-readme")
                        .fill_width()
                        .padding_y(12.),
                );
            }
            detail = detail.child(
                label("Capabilities")
                    .font_size(15.)
                    .font_weight(FontWeight::SEMIBOLD)
                    .padding_y(10.),
            );
            detail = detail.child(label("Host services require declared access. Native plugins still run with the editor’s permissions.")
                .font_size(12.).color(ThemeColor::TextMuted));
            if info.metadata.capabilities.is_empty() {
                detail = detail.child(label("No access capabilities declared.").font_size(12.5));
            }
            for capability in &info.metadata.capabilities {
                detail = detail.child(
                    column()
                        .gap(3.)
                        .padding_y(6.)
                        .child(
                            label(format!(
                                "{} · {}",
                                capability.id,
                                if capability.optional {
                                    "Optional"
                                } else {
                                    "Required"
                                }
                            ))
                            .font_size(12.5)
                            .font_weight(FontWeight::MEDIUM),
                        )
                        .child(
                            label(capability.reason.clone())
                                .font_size(12.5)
                                .color(ThemeColor::TextMuted),
                        ),
                );
            }
        }
        detail = detail.child(
            label("Adds to Velk")
                .font_size(15.)
                .font_weight(FontWeight::SEMIBOLD)
                .padding_y(10.),
        );
        let rows = info.map(contributions).unwrap_or_default();
        if rows.is_empty() {
            detail = detail.child(
                label(if info.is_some() {
                    "Nothing visible: this plugin only provides internal hooks."
                } else {
                    "Enable this plugin to see what it adds."
                })
                .font_size(12.5)
                .color(ThemeColor::TextMuted),
            );
        }
        for (kind, text) in rows {
            detail = detail.child(
                row()
                    .gap(16.)
                    .padding_y(3.)
                    .font_size(12.5)
                    .child(
                        label(kind)
                            .width(120.)
                            .flex_shrink(0.)
                            .color(ThemeColor::TextMuted),
                    )
                    .child(label(text).flex_shrink(1.).min_width(0.)),
            );
        }
        if status != Status::Running && info.is_some() {
            detail = detail.child(
                label("From when it last ran this session.")
                    .font_size(12.)
                    .padding_y(6.)
                    .color(ThemeColor::TextDisabled),
            );
        }
        detail
    }
    /// The page itself. `close`, when given, adds a Close button for modal use.
    pub(super) fn plugins_page(
        &self,
        cx: &mut ViewContext<'_, Self>,
        close: Option<rxui::CommandAction<ClosePlugins>>,
    ) -> Element {
        if self.plugin_page.interfaces {
            return self.plugin_interfaces_page(cx, close);
        }
        let page = &self.plugin_page;
        let focus_filter = cx
            .command(FocusFilter, |this, _, cx| {
                let _ = this.plugin_page.filter.focus(cx);
            })
            .shortcut(Shortcut::new(
                KeyboardKey::Character("/".into()),
                Default::default(),
            ));
        let terms: Vec<_> = page
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let ids: Vec<_> = self
            .plugin_ids()
            .into_iter()
            .filter(|id| {
                let description = self.plugin_info(id).map_or("", |p| &p.description);
                let haystack =
                    format!("{id} {} {description}", self.plugin_name(id)).to_lowercase();
                terms.iter().all(|t| haystack.contains(t))
            })
            .collect();
        let selected = page
            .selected
            .clone()
            .filter(|id| ids.contains(id))
            .or_else(|| ids.first().cloned());
        let mut rows = column()
            .key("plugin-rows")
            .fill_width()
            .flex_grow(1.)
            .flex_shrink(1.)
            .min_height(0.)
            .scroll_y()
            .on_command(focus_filter.clone());
        if page.tree && terms.is_empty() {
            let manifest = velk_plugin_host::Manifest {
                plugins: self
                    .plugins
                    .as_ref()
                    .map(|s| s.inventory.clone())
                    .unwrap_or_default(),
            };
            let manifest = manifest
                .resolve_interfaces(&self.shell.store.preferences.interface_bindings)
                .unwrap_or(manifest);
            for (index, node) in manifest.dependency_tree().into_iter().enumerate() {
                let reference = node.reference;
                rows = rows.child(
                    row()
                        .key(format!("dependency-row-{index}"))
                        .fill_width()
                        .child(
                            column()
                                .width((node.depth.min(8) * 14) as f32)
                                .flex_shrink(0.),
                        )
                        .child(
                            column()
                                .flex_grow(1.)
                                .flex_shrink(1.)
                                .min_width(0.)
                                .children(reference.then(|| {
                                    label("Shared dependency")
                                        .font_size(11.)
                                        .color(ThemeColor::TextDisabled)
                                }))
                                .child(self.plugin_list_row(
                                    &node.id,
                                    selected.as_ref() == Some(&node.id),
                                    cx,
                                )),
                        ),
                );
            }
            for id in ids
                .iter()
                .filter(|id| !manifest.plugins.iter().any(|spec| &spec.id == *id))
            {
                rows = rows.child(self.plugin_list_row(id, selected.as_ref() == Some(id), cx));
            }
        } else {
            for label_text in [
                "Explicitly installed",
                "Bundled default",
                "Dependency",
                "Not in manifest",
            ] {
                let group: Vec<_> = ids
                    .iter()
                    .filter(|id| self.plugin_install_label(id) == label_text)
                    .collect();
                if group.is_empty() {
                    continue;
                }
                rows = rows.child(
                    label(label_text)
                        .font_size(12.)
                        .font_weight(FontWeight::SEMIBOLD)
                        .padding(8.),
                );
                for id in group {
                    rows = rows.child(self.plugin_list_row(id, selected.as_ref() == Some(id), cx));
                }
            }
        }
        let empty = match &self.plugins {
            None => Some("Plugin worker unavailable."),
            Some(_) if self.plugin_ids().is_empty() => Some("No plugins found in plugins.json."),
            Some(_) if ids.is_empty() => Some("No plugins match the filter."),
            Some(_) => None,
        };
        rows = rows.children(empty.map(|text| {
            label(text)
                .key("plugin-empty")
                .padding(14.)
                .color(ThemeColor::TextMuted)
        }));
        let list = column()
            .key("plugin-list")
            .width(300.)
            .flex_shrink(0.)
            .fill_height()
            .background(ThemeColor::Surface)
            .border_right(1.)
            .border_color(ThemeColor::Divider)
            .child(
                row().fill_width().padding(12.).child(
                    search_field(&page.query, "Filter plugins", page.filter.clone())
                        .key("plugin-filter")
                        .fill_width()
                        .on_change(cx.listener(|this, event: &TextChangeEvent, _| {
                            this.plugin_page.query = event.value.clone();
                        })),
                ),
            )
            .child(
                button(if page.tree {
                    "View: Dependencies"
                } else {
                    "View: Installed"
                })
                .key("plugin-dependency-view")
                .variant(ButtonVariant::Quiet)
                .on_click(cx.listener(|this, _, _| {
                    this.plugin_page.tree = !this.plugin_page.tree;
                })),
            )
            .child(
                button("Interfaces")
                    .key("plugin-interfaces-tab")
                    .variant(ButtonVariant::Quiet)
                    .on_click(cx.listener(|this, _, _| this.plugin_page.interfaces = true)),
            )
            .child(rows)
            .children(self.plugins.as_ref().map(|state| {
                label(state.status.clone())
                    .key("plugin-status")
                    .fill_width()
                    .padding(12.)
                    .font_size(12.)
                    .color(ThemeColor::TextDisabled)
                    .border_top(1.)
                    .border_color(ThemeColor::Divider)
            }));
        let all = self.plugin_ids();
        let running = all
            .iter()
            .filter(|id| self.plugin_status(id) == Status::Running)
            .count();
        let failed = all
            .iter()
            .filter(|id| self.plugin_status(id) == Status::Failed)
            .count();
        let header = row()
            .fill_width()
            .align_items(AlignItems::CENTER)
            .gap(14.)
            .padding_y(14.)
            .font_size(12.)
            .child(
                label(format!("{running} of {} running", all.len())).color(ThemeColor::TextMuted),
            )
            .children((failed > 0).then(|| label(format!("{failed} failed")).color(error_color())))
            .child(row().flex_grow(1.))
            .children(close.map(|close| {
                close
                    .label("Close")
                    .button()
                    .key("plugin-menu-close")
                    .variant(ButtonVariant::Quiet)
                    .height(28.)
                    .padding_y(0.)
                    .font_size(12.5)
            }));
        let detail = column()
            .key("plugin-pane")
            .flex_grow(1.)
            .flex_shrink(1.)
            .min_width(0.)
            .fill_height()
            .scroll_y()
            .padding_x(28.)
            .padding_y(4.)
            .on_command(focus_filter)
            .child(header)
            .children(selected.map(|id| self.plugin_detail(&id, cx)));
        row()
            .key("plugins-page")
            .fill_width()
            .fill_height()
            .background(ThemeColor::Background)
            .child(list)
            .child(detail)
    }
    fn select_interface(&mut self, interface: &str, provider: &str, cx: &mut Context<'_, Self>) {
        if self.plugins.as_ref().is_none_or(|s| s.task.is_some()) {
            return;
        }
        let mut bindings = self.shell.store.preferences.interface_bindings.clone();
        bindings.insert(interface.into(), provider.into());
        let manifest = velk_plugin_host::Manifest {
            plugins: self.plugins.as_ref().unwrap().inventory.clone(),
        };
        if let Err(error) = manifest.resolve_interfaces(&bindings) {
            self.shell.store.error = Some(error);
            return;
        }
        self.shell.store.preferences.interface_bindings = bindings;
        self.persist_shell(cx);
        self.plugin_operation(Operation::Restart(None), cx);
    }
    fn plugin_interfaces_page(
        &self,
        cx: &mut ViewContext<'_, Self>,
        close: Option<rxui::CommandAction<ClosePlugins>>,
    ) -> Element {
        let mut body = column().key("plugin-interface-list").fill_width().gap(14.);
        body = body.child(label("Choose implementations for shared plugin interfaces. Backend changes restart running plugins; disabled providers stay disabled.").color(ThemeColor::TextMuted));
        if let Some(state) = &self.plugins {
            let mut interfaces = std::collections::BTreeMap::new();
            for spec in &state.inventory {
                for implementation in &spec.interfaces.implements {
                    interfaces
                        .entry(implementation.interface.clone())
                        .or_insert_with(|| implementation.title.clone());
                }
            }
            for (interface, title) in interfaces {
                let default = state
                    .inventory
                    .iter()
                    .flat_map(|s| &s.interfaces.requires)
                    .find(|r| r.interface == interface)
                    .map(|r| r.default_provider.as_str());
                let selected = self
                    .shell
                    .store
                    .preferences
                    .interface_bindings
                    .get(&interface)
                    .map(String::as_str)
                    .or(default);
                let mut choices = column()
                    .key(format!("interface-{interface}"))
                    .gap(6.)
                    .child(label(title).font_weight(FontWeight::SEMIBOLD))
                    .child(
                        label(interface.clone())
                            .font_size(12.)
                            .color(ThemeColor::TextMuted),
                    );
                for spec in state.inventory.iter().filter(|s| {
                    s.interfaces
                        .implements
                        .iter()
                        .any(|i| i.interface == interface)
                }) {
                    let id = spec.id.clone();
                    let target = interface.clone();
                    let active = state.loaded.iter().any(|p| p.id == id);
                    choices = choices.child(
                        button(format!(
                            "{}{}",
                            self.plugin_name(&id),
                            if selected == Some(id.as_str()) {
                                " (selected)"
                            } else {
                                ""
                            }
                        ))
                        .key(format!("interface-{interface}-{id}"))
                        .variant(ButtonVariant::Quiet)
                        .disabled(!active || state.task.is_some())
                        .accessibility_selected(selected == Some(id.as_str()))
                        .on_click(
                            cx.listener(move |this, _, cx| this.select_interface(&target, &id, cx)),
                        ),
                    );
                }
                if selected.is_some_and(|id| !state.loaded.iter().any(|p| p.id == id)) {
                    choices = choices.child(label("Selected provider unavailable. Enable it in Installed to use this interface.").color(error_color()));
                }
                body = body.child(choices);
            }
        }
        for setting in crate::settings::REGISTRY.iter().filter(|s| {
            s.key.ends_with("_provider")
                || s.key == "completion_frontend"
                || s.key == "search_backend"
        }) {
            body = body.child(
                column()
                    .gap(4.)
                    .child(label(setting.title.clone()).font_weight(FontWeight::SEMIBOLD))
                    .child(
                        label(setting.description.clone())
                            .font_size(12.)
                            .color(ThemeColor::TextMuted),
                    )
                    .child(self.setting_control(setting, cx)),
            );
        }
        column()
            .key("plugin-interfaces-page")
            .fill_width()
            .fill_height()
            .padding(20.)
            .gap(16.)
            .child(
                row()
                    .gap(12.)
                    .child(
                        button("Installed")
                            .key("plugin-installed-tab")
                            .variant(ButtonVariant::Quiet)
                            .on_click(
                                cx.listener(|this, _, _| this.plugin_page.interfaces = false),
                            ),
                    )
                    .child(label("Interfaces").font_size(18.))
                    .children(close.map(|action| action.label("Close").button())),
            )
            .child(body.flex_grow(1.).min_height(0.).scroll_y())
            .into_element()
    }
    pub(super) fn plugins_modal(
        &self,
        cx: &mut ViewContext<'_, Self>,
        toggle: rxui::CommandAction<TogglePlugins>,
        close: rxui::CommandAction<ClosePlugins>,
        load: rxui::CommandAction<LoadPlugins>,
        unload: rxui::CommandAction<UnloadPlugins>,
    ) -> Option<Element> {
        if !self.plugin_menu_open {
            return None;
        }
        let page = self
            .plugins_page(cx, Some(close.clone()))
            .on_command(toggle)
            .on_command(close)
            .on_command(load)
            .on_command(unload);
        Some(
            modal(page)
                .key("plugin-menu")
                .width(860.)
                .height(600.)
                .padding(0.)
                .accessibility_label("Plugins")
                .on_dismiss(cx.listener(|this, _: &DismissEvent, cx| {
                    this.set_plugin_menu(false, cx);
                }))
                .into_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reenabling_backend_restores_enabled_frontends_but_preserves_disabled_ones() {
        let spec = |id: &str, dependencies: &[&str]| velk_plugin_host::PluginSpec {
            runtime: Default::default(),
            interfaces: Default::default(),
            id: id.into(),
            library: "test.so".into(),
            digest: "a".repeat(64),
            pack: true,
            dependencies: dependencies.iter().map(|id| (*id).into()).collect(),
            install_reason: Default::default(),
        };
        let inventory = vec![
            spec("test.backend", &[]),
            spec("test.frontend", &["test.backend"]),
            spec("test.other", &["test.backend"]),
            spec("test.extension", &["test.frontend"]),
        ];
        assert_eq!(
            enabled_dependents("test.backend".into(), &inventory, &["test.other".into()]),
            ["test.backend", "test.frontend", "test.extension"]
        );
        assert_eq!(
            enabled_dependents("test.backend".into(), &inventory, &["test.frontend".into()]),
            ["test.backend", "test.other"]
        );
    }
    #[test]
    fn display_names_and_contributions_come_from_ids_and_hooks() {
        assert_eq!(
            display_name("velk.language-server.rust-analyzer"),
            "Rust analyzer"
        );
        assert_eq!(display_name("velk.files"), "Files");
        let info = PluginInfo {
            dependencies: Vec::new(),
            id: "velk.files".into(),
            version: "1".into(),
            generation: 1,
            loaded_path: Default::default(),
            description: String::new(),
            metadata: Default::default(),
            settings_values: Default::default(),
            settings_error: None,
            resident_code: false,
            hooks: vec![
                velk_panels::HOOK.into(),
                hook_name::LIFECYCLE_SNAPSHOT_HOOK.into(),
                "test.custom".into(),
            ],
        };
        let rows = contributions(&info);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "Panels");
        assert_eq!(rows[1], ("Hooks", "test.custom".into()));
    }
    #[test]
    fn disabled_plugins_stay_listed_and_enabling_updates_preferences() {
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
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                this.shell.store.preferences.disabled_plugins = vec!["velk.terminal".into()];
                this.set_plugin_menu(true, cx);
            })
        });
        let mut ui = rxui::Ui::new(&mut runtime, document.clone()).unwrap();
        ui.prepare(&mut runtime, [1000., 700.], &mut Measure)
            .unwrap();
        for key in ["plugin-velk.terminal", "plugin-detail-velk.terminal"] {
            assert!(
                ui.elements().any(|e| e.key == Some(&rxui::Key::from(key))),
                "{key}"
            );
        }
        runtime.update(|cx| document.update(cx, |this, _| this.plugin_page.interfaces = true));
        ui.prepare(&mut runtime, [1000., 700.], &mut Measure)
            .unwrap();
        for key in [
            "plugin-interfaces-page",
            "plugin-interface-list",
            "setting-snippet_provider",
        ] {
            assert!(
                ui.elements().any(|e| e.key == Some(&rxui::Key::from(key))),
                "{key}"
            );
        }
        runtime.update(|cx| document.update(cx, |this, _| this.plugin_page.interfaces = false));
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                this.set_plugin_enabled("velk.terminal".into(), true, cx);
                assert!(this.shell.store.preferences.disabled_plugins.is_empty());
                this.set_plugin_enabled("velk.files".into(), false, cx);
                assert_eq!(
                    this.shell.store.preferences.disabled_plugins,
                    ["velk.files"]
                );
            })
        });
    }
}
