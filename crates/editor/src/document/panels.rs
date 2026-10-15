//! Host-owned panel snapshots and virtualized controls; plugin calls stay off UI.
use super::Document;
use rxui::{Element, prelude::*};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use velk_panels::{Content, Panel, Request, Slot, Snapshot};
use velk_plugin_host::{Client, HookHandle, PluginInfo};

pub(super) struct Toggle;
impl Command for Toggle {}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct ViewState {
    expanded: BTreeSet<String>,
    selected: Option<String>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct UiState {
    visible: bool,
    tabs: BTreeMap<Slot, String>,
    closed: BTreeSet<Slot>,
    views: BTreeMap<String, ViewState>,
}
pub(super) struct Entry {
    provider: String,
    key: String,
    panel: Panel,
    visible: Vec<usize>,
    scroll: rxui::ScrollHandle,
}
#[derive(Default)]
pub(super) struct State {
    pub ui: UiState,
    entries: Vec<Entry>,
    handles: BTreeMap<String, HookHandle>,
    epoch: u64,
    task: Option<Task>,
    error: Option<String>,
}
struct Contribution {
    provider: String,
    handle: HookHandle,
    snapshot: Snapshot,
}
fn empty_notice(state: &State) -> String {
    if let Some(error) = &state.error {
        format!("Panels: {error}")
    } else if state.task.is_some() {
        "Panels: loading…".into()
    } else {
        "Panels: no contributions loaded".into()
    }
}
impl Entry {
    fn rebuild(&mut self, view: &ViewState) {
        self.visible.clear();
        if let Content::Tree(nodes) = &self.panel.content {
            let mut hidden_below = None;
            for (i, node) in nodes.iter().enumerate() {
                if hidden_below.is_some_and(|depth| node.depth > depth) {
                    continue;
                }
                hidden_below = None;
                self.visible.push(i);
                if node.branch && !view.expanded.contains(&node.id) {
                    hidden_below = Some(node.depth);
                }
            }
        }
    }
}
#[tracing::instrument(level="debug", skip_all, fields(providers=plugins.len(), async_work=true))]
async fn discover(
    client: Client,
    plugins: Vec<PluginInfo>,
) -> Result<(Vec<Contribution>, Vec<String>), String> {
    let mut contributions = Vec::new();
    let mut errors = Vec::new();
    let mut panels = 0;
    let mut nodes = 0;
    for plugin in plugins {
        // Missing optional hooks are expected. No plugin API special case or
        // synchronous UI callback is needed to discover a new contribution kind.
        let Ok(receive) = client.bind_hook(plugin.id.clone(), velk_panels::HOOK.into()) else {
            continue;
        };
        let Ok(Ok(handle)) = receive.recv().await else {
            continue;
        };
        let result: Result<Contribution, String> = async {
            let bytes = client
                .call_hook(handle.clone(), velk_panels::encode(&Request::Describe)?)
                .await?;
            let snapshot: Snapshot = velk_panels::decode(&bytes)?;
            snapshot.validate()?;
            let count = snapshot
                .panels
                .iter()
                .map(|p| match &p.content {
                    Content::Tree(n) => n.len(),
                    _ => 0,
                })
                .sum::<usize>();
            if panels + snapshot.panels.len() > 64 || nodes + count > 20_000 {
                return Err("window panel contribution limit exceeded".into());
            }
            panels += snapshot.panels.len();
            nodes += count;
            Ok(Contribution {
                provider: plugin.id.clone(),
                handle,
                snapshot,
            })
        }
        .await;
        match result {
            Ok(c) => contributions.push(c),
            Err(e) => errors.push(format!("{}: {e}", plugin.id)),
        }
    }
    Ok((contributions, errors))
}
impl Document {
    /// Invalidate pending replies immediately, before a lifecycle operation is queued.
    pub(super) fn suspend_panels(&mut self) {
        self.panels.epoch = self.panels.epoch.wrapping_add(1);
        self.panels.task = None;
        self.panels.entries.clear();
        self.panels.handles.clear();
        self.panels.error = None;
    }
    pub(super) fn refresh_panels(&mut self, cx: &mut Context<'_, Self>) {
        self.panels.epoch = self.panels.epoch.wrapping_add(1);
        let epoch = self.panels.epoch;
        self.panels.task = None;
        let Some(plugins) = &self.plugins else { return };
        let client = plugins.client.clone();
        let loaded = plugins.loaded.clone();
        match cx.try_spawn(discover(client, loaded), move |this, result, _| {
            if this.panels.epoch != epoch {
                return;
            }
            this.panels.task = None;
            match result {
                Ok(Ok((contributions, errors))) => {
                    this.install_panels(contributions);
                    this.panels.error = (!errors.is_empty())
                        .then(|| errors.into_iter().take(4).collect::<Vec<_>>().join("; "));
                }
                Ok(Err(e)) => this.panels.error = Some(e),
                Err(e) => this.panels.error = Some(e.to_string()),
            }
            if let Some(error) = &this.panels.error {
                this.status = Some(format!("Panels: {error}"));
            } else if this
                .status
                .as_deref()
                .is_some_and(|s| s.starts_with("Panels:"))
            {
                this.status = Some(format!("Panels: {} available", this.panels.entries.len()));
            }
        }) {
            Ok(t) => self.panels.task = Some(t),
            Err(e) => self.panels.error = Some(e.to_string()),
        }
    }
    fn install_panels(&mut self, contributions: Vec<Contribution>) {
        let mut old = std::mem::take(&mut self.panels.entries);
        self.panels.handles.clear();
        for c in contributions {
            self.panels.handles.insert(c.provider.clone(), c.handle);
            for panel in c.snapshot.panels {
                let key = format!("{}:{}", c.provider, panel.id);
                let view = self
                    .panels
                    .ui
                    .views
                    .entry(key.clone())
                    .or_insert_with(|| ViewState {
                        expanded: match &panel.content {
                            Content::Tree(nodes) => nodes
                                .iter()
                                .filter(|n| n.branch && n.expanded)
                                .map(|n| n.id.clone())
                                .collect(),
                            _ => Default::default(),
                        },
                        selected: None,
                    });
                if let Content::Tree(nodes) = &panel.content {
                    let branches: BTreeSet<_> =
                        nodes.iter().filter(|n| n.branch).map(|n| &n.id).collect();
                    view.expanded.retain(|id| branches.contains(id));
                    if view
                        .selected
                        .as_ref()
                        .is_some_and(|id| !nodes.iter().any(|n| &n.id == id))
                    {
                        view.selected = None;
                    }
                }
                let scroll = old
                    .iter()
                    .position(|e| e.key == key)
                    .map(|i| old.remove(i).scroll)
                    .unwrap_or_default();
                let mut entry = Entry {
                    provider: c.provider.clone(),
                    key,
                    panel,
                    visible: Vec::new(),
                    scroll,
                };
                entry.rebuild(view);
                self.panels.entries.push(entry);
            }
        }
        // Persist only bounded UI state, including unloaded panels for restarts.
        while self.panels.ui.views.len() > 64 {
            let stale = self
                .panels
                .ui
                .views
                .keys()
                .find(|k| !self.panels.entries.iter().any(|e| &e.key == *k))
                .cloned();
            let Some(key) = stale else { break };
            self.panels.ui.views.remove(&key);
        }
        self.panels
            .ui
            .tabs
            .retain(|_, key| self.panels.entries.iter().any(|e| &e.key == key));
    }
    fn replace_panel_provider(
        &mut self,
        provider: String,
        handle: HookHandle,
        snapshot: Snapshot,
    ) -> Result<(), String> {
        snapshot.validate()?;
        for panel in &snapshot.panels {
            if self.panels.entries.iter().any(|e| {
                e.provider == provider
                    && e.panel.id == panel.id
                    && e.panel.revision > panel.revision
            }) {
                return Err("stale panel revision".into());
            }
        }
        let mut contributions: Vec<_> = self
            .panels
            .entries
            .iter()
            .filter(|e| e.provider != provider)
            .map(|e| Contribution {
                provider: e.provider.clone(),
                handle: self.panels.handles[&e.provider].clone(),
                snapshot: Snapshot {
                    panels: vec![e.panel.clone()],
                },
            })
            .collect();
        contributions.push(Contribution {
            provider,
            handle,
            snapshot,
        });
        let panels = contributions
            .iter()
            .flat_map(|c| &c.snapshot.panels)
            .collect::<Vec<_>>();
        let nodes = panels
            .iter()
            .map(|p| match &p.content {
                Content::Tree(n) => n.len(),
                _ => 0,
            })
            .sum::<usize>();
        if panels.len() > 64 || nodes > 20_000 {
            return Err("window panel contribution limit exceeded".into());
        }
        self.install_panels(contributions);
        Ok(())
    }
    fn activate_panel_row(&mut self, key: &str, node: &str, cx: &mut Context<'_, Self>) {
        let Some(entry) = self.panels.entries.iter_mut().find(|e| e.key == key) else {
            return;
        };
        let Content::Tree(nodes) = &entry.panel.content else {
            return;
        };
        let Some(node) = nodes.iter().find(|n| n.id == node) else {
            return;
        };
        let view = self.panels.ui.views.entry(key.into()).or_default();
        view.selected = Some(node.id.clone());
        if node.branch {
            if !view.expanded.remove(&node.id) {
                view.expanded.insert(node.id.clone());
            }
            entry.rebuild(view);
            return;
        }
        let Some(action) = node.action.clone() else {
            return;
        };
        if self.panels.task.is_some() {
            return;
        }
        let Some(plugins) = &self.plugins else { return };
        let client = plugins.client.clone();
        let Some(handle) = self.panels.handles.get(&entry.provider).cloned() else {
            return;
        };
        let request = Request::Invoke {
            panel: entry.panel.id.clone(),
            revision: entry.panel.revision,
            node: node.id.clone(),
            action,
        };
        let provider = entry.provider.clone();
        let epoch = self.panels.epoch;
        let response_handle = handle.clone();
        let input = match velk_panels::encode(&request) {
            Ok(b) => b,
            Err(e) => {
                self.panels.error = Some(e);
                return;
            }
        };
        match cx.try_spawn(
            async move {
                let bytes = client.call_hook(handle, input).await?;
                let snapshot: Snapshot = velk_panels::decode(&bytes)?;
                snapshot.validate()?;
                Ok::<_, String>(snapshot)
            },
            move |this, result, _| {
                if this.panels.epoch != epoch {
                    return;
                }
                this.panels.task = None;
                match result {
                    Ok(Ok(snapshot)) => {
                        this.panels.error = this
                            .replace_panel_provider(provider, response_handle, snapshot)
                            .err();
                    }
                    Ok(Err(e)) => this.panels.error = Some(format!("{provider}: {e}")),
                    Err(e) => this.panels.error = Some(e.to_string()),
                }
            },
        ) {
            Ok(t) => self.panels.task = Some(t),
            Err(e) => self.panels.error = Some(e.to_string()),
        }
    }
    pub(super) fn panels_action(&self, cx: &ViewContext<'_, Self>) -> rxui::CommandAction<Toggle> {
        cx.command(Toggle, |this, _, cx| {
            let showing = this.panels.ui.visible
                && this
                    .panels
                    .entries
                    .iter()
                    .any(|e| !this.panels.ui.closed.contains(&e.panel.slot));
            this.panels.ui.visible = !showing;
            if this.panels.ui.visible && this.panels.entries.is_empty() {
                this.status = Some(self::empty_notice(&this.panels));
            }
            if this.panels.ui.visible {
                this.panels.ui.closed.clear();
            } else {
                let _ = this.editor_focus.focus(cx);
            }
        })
        .label("Panels: Toggle visibility")
        .shortcut(Shortcut::primary("b"))
    }
    pub(super) fn panel_slot(&self, slot: Slot, cx: &mut ViewContext<'_, Self>) -> Option<Element> {
        if !self.panels.ui.visible || self.panels.ui.closed.contains(&slot) {
            return None;
        }
        let entries: Vec<_> = self
            .panels
            .entries
            .iter()
            .filter(|e| e.panel.slot == slot)
            .collect();
        let entry = self
            .panels
            .ui
            .tabs
            .get(&slot)
            .and_then(|key| entries.iter().find(|e| &e.key == key).copied())
            .or_else(|| entries.first().copied())?;
        let mut tabs = row().fill_width().gap(3.).flex_shrink(0.).padding(4.);
        for tab in &entries {
            let key = tab.key.clone();
            tabs = tabs.child(
                button(tab.panel.title.clone())
                    .key(format!("panel-tab-{}", tab.key))
                    .variant(if tab.key == entry.key {
                        rxui::ButtonVariant::Default
                    } else {
                        rxui::ButtonVariant::Quiet
                    })
                    .font_size(12.)
                    .on_click(cx.listener(move |this, _, _| {
                        this.panels.ui.tabs.insert(slot, key.clone());
                    })),
            );
        }
        tabs = tabs
            .child(row().flex_grow(1.))
            .child(
                button("↻")
                    .accessibility_label("Refresh panels")
                    .disabled(self.panels.task.is_some() || self.plugins_busy())
                    .on_click(cx.listener(|this, _, cx| this.refresh_panels(cx))),
            )
            .child(
                button("×")
                    .accessibility_label("Close panel")
                    .key(format!("panel-close-{slot:?}"))
                    .on_click(cx.listener(move |this, _, cx| {
                        this.panels.ui.closed.insert(slot);
                        let _ = this.editor_focus.focus(cx);
                    })),
            );
        let view = self.panels.ui.views.get(&entry.key);
        let content = match &entry.panel.content {
            Content::Text(text) => column()
                .fill_width()
                .fill_height()
                .min_height(0.)
                .scroll_y()
                .padding(8.)
                .child(label(text.clone()).font_size(13.))
                .into_element(),
            Content::Tree(nodes) => {
                virtual_list(entry.visible.len(), 26., &entry.scroll, cx, |i| {
                    let node = &nodes[entry.visible[i]];
                    let expanded = view.is_some_and(|v| v.expanded.contains(&node.id));
                    let selected = view.is_some_and(|v| v.selected.as_deref() == Some(&node.id));
                    let text = format!(
                        "{}{}",
                        if node.branch {
                            if expanded { "▾ " } else { "▸ " }
                        } else {
                            "  "
                        },
                        node.label
                    );
                    let key = entry.key.clone();
                    let id = node.id.clone();
                    let mut control = button(text).accessibility_selected(selected);
                    if node.branch {
                        control = control.accessibility_expanded(expanded);
                    }
                    control
                        .key(format!("panel-row-{}-{}", entry.key, node.id))
                        .accessibility_label(format!(
                            "{}{}",
                            node.label,
                            if node.branch {
                                if expanded {
                                    ", expanded"
                                } else {
                                    ", collapsed"
                                }
                            } else {
                                ""
                            }
                        ))
                        .font_size(13.)
                        .height(26.)
                        .fill_width()
                        .justify_content(rxui::taffy::prelude::JustifyContent::START)
                        .padding_x(8. + f32::from(node.depth) * 14.)
                        .variant(if selected {
                            rxui::ButtonVariant::Default
                        } else {
                            rxui::ButtonVariant::Quiet
                        })
                        .on_click(
                            cx.listener(move |this, _, cx| this.activate_panel_row(&key, &id, cx)),
                        )
                })
                .fill_width()
                .fill_height()
                .into_element()
            }
        };
        let content = content.min_height(0.).min_width(0.).flex_shrink(1.);
        let mut panel = column()
            .key(format!("panel-slot-{slot:?}"))
            .accessibility_label(entry.panel.title.clone())
            .background(ThemeColor::Surface)
            .clip()
            .flex_shrink(0.)
            .min_height(0.)
            .child(tabs)
            .child(content);
        if let Some(error) = &self.panels.error {
            panel = panel.child(label(error.clone()).font_size(12.).padding(6.));
        }
        Some(match slot {
            Slot::Bottom => panel.fill_width().height(180.),
            _ => panel.width(280.).fill_height(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxui::{Runtime, Ui};
    use velk_panels::Node;
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
    fn node(id: String, depth: u8, branch: bool) -> Node {
        Node {
            label: id.clone(),
            id,
            depth,
            branch,
            expanded: false,
            action: None,
        }
    }
    fn entry(slot: Slot, content: Content) -> Entry {
        let key = format!("test:{slot:?}");
        Entry {
            provider: "test".into(),
            key,
            panel: Panel {
                id: format!("{slot:?}"),
                title: format!("{slot:?}"),
                slot,
                revision: 1,
                content,
            },
            visible: vec![],
            scroll: Default::default(),
        }
    }
    fn click(ui: &mut Ui<Document>, runtime: &mut Runtime, key: &str) {
        let e = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from(key)))
            .unwrap();
        let position = [
            e.bounds.x + e.bounds.width / 2.,
            e.bounds.y + e.bounds.height / 2.,
        ];
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
    fn closed_branches_hide_descendants_and_state_roundtrips() {
        let mut e = entry(
            Slot::Left,
            Content::Tree(vec![
                node("root".into(), 0, true),
                node("child".into(), 1, true),
                node("file".into(), 2, false),
                node("other".into(), 0, false),
            ]),
        );
        let mut view = ViewState::default();
        e.rebuild(&view);
        assert_eq!(e.visible, vec![0, 3]);
        view.expanded.insert("root".into());
        e.rebuild(&view);
        assert_eq!(e.visible, vec![0, 1, 3]);
        view.expanded.insert("child".into());
        view.selected = Some("file".into());
        e.rebuild(&view);
        assert_eq!(e.visible, vec![0, 1, 2, 3]);
        let mut state = UiState {
            visible: true,
            ..Default::default()
        };
        state.views.insert(e.key.clone(), view);
        state.tabs.insert(Slot::Left, e.key.clone());
        let restored: UiState =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(restored.visible);
        e.rebuild(&restored.views[&e.key]);
        assert_eq!(e.visible.len(), 4);
        assert_eq!(restored.views[&e.key].selected.as_deref(), Some("file"));
    }
    #[test]
    fn panels_are_opt_in_virtualized_and_close_restores_editor_focus() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let editor = runtime.update(|cx| cx.new(|_| editor));
        let document =
            runtime.update(|cx| cx.new(|cx| Document::new(editor.clone(), file, path, cx)));
        runtime.update(|cx| {
            document.update(cx, |this, _| {
                let mut e = entry(
                    Slot::Left,
                    Content::Tree(
                        (0..1000)
                            .map(|i| node(format!("file-{i}"), 0, false))
                            .collect(),
                    ),
                );
                e.rebuild(&ViewState::default());
                this.panels
                    .ui
                    .views
                    .insert(e.key.clone(), ViewState::default());
                this.panels.entries = vec![
                    e,
                    entry(Slot::Right, Content::Text("Details".into())),
                    entry(Slot::Bottom, Content::Text("Output".into())),
                ];
            })
        });
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        let prepare = |ui: &mut Ui<Document>, r: &mut Runtime| {
            ui.prepare(r, [1280., 800.], &mut Measure).unwrap()
        };
        let bounds = |ui: &Ui<Document>| {
            ui.elements()
                .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
                .unwrap()
                .bounds
        };
        prepare(&mut ui, &mut runtime);
        let full = bounds(&ui);
        let source = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        assert!(
            !ui.elements()
                .any(|e| e.key == Some(&rxui::Key::from("panel-slot-Left")))
        );
        ui.dispatch_command::<Toggle>(&mut runtime).unwrap();
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui).width, full.width - 560.);
        assert_eq!(bounds(&ui).height, full.height - 180.);
        // Visible rows plus overscan only; 1,000 descriptions must not be built.
        let mounted = ui
            .elements()
            .filter(|e| {
                e.key
                    .is_some_and(|k| format!("{k:?}").contains("panel-row-"))
            })
            .count();
        assert!(mounted > 0 && mounted < 40, "mounted {mounted} rows");
        click(&mut ui, &mut runtime, "panel-close-Left");
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui).width, full.width - 280.);
        assert_eq!(ui.focused_element(), Some(source));
        ui.dispatch_command::<Toggle>(&mut runtime).unwrap();
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui), full);
        ui.dispatch_command::<Toggle>(&mut runtime).unwrap();
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui).width, full.width - 560.);
        // A lifecycle transition removes cached contributions immediately.
        runtime.update(|cx| document.update(cx, |this, _| this.suspend_panels()));
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui), full);
        runtime.update(|cx| assert_eq!(editor.read(cx).buffer().text().to_string(), "original"));
    }
}
