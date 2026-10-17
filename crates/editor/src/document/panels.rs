//! Host-owned panel snapshots and virtualized controls; plugin calls stay off UI.
use super::Document;
use rxui::taffy::prelude::{AlignItems, JustifyContent};
use rxui::{Element, prelude::*};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::{Duration, Instant},
};
use velk_panels::{
    Content, Context as PanelContext, Effect, NativePath, Panel, Reply, Request, Slot, Snapshot,
};
use velk_plugin_host::{Client, HookHandle, PluginInfo};

pub(super) struct Toggle;
impl Command for Toggle {}
pub(super) struct ToggleBottom;
impl Command for ToggleBottom {}
pub(super) struct Refresh;
impl Command for Refresh {}
pub(super) struct OpenFolder;
impl Command for OpenFolder {}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct ViewState {
    expanded: BTreeSet<String>,
    selected: Option<String>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct UiState {
    #[serde(default)]
    pub(super) source: Option<NativePath>,
    #[serde(default)]
    pub(super) root: Option<NativePath>,
    pub(super) visible: bool,
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
    grid: Option<std::rc::Rc<super::grid::Connection>>,
}
#[derive(Default)]
pub(super) struct State {
    pub ui: UiState,
    entries: Vec<Entry>,
    handles: BTreeMap<String, HookHandle>,
    epoch: u64,
    task: Option<Task>,
    watching: Option<Task>,
    watch_providers: BTreeSet<String>,
    stream_providers: BTreeSet<String>,
    slow_checked: Option<Instant>,
    // Tiled pane sizes and the active pane; None is the editor.
    pub(super) split: BTreeMap<Slot, SplitPosition>,
    pub(super) active: Option<Slot>,
    focus_bottom: bool,
    refresh_pending: bool,
    error: Option<String>,
    pending: VecDeque<PendingAction>,
}
struct PendingAction {
    key: String,
    node: String,
    expand: bool,
}
impl State {
    pub(super) fn epoch(&self) -> u64 {
        self.epoch
    }
    pub(super) fn new(source: Option<NativePath>) -> Self {
        Self {
            ui: UiState {
                source,
                ..Default::default()
            },
            ..Default::default()
        }
    }
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
/// Polling is a transport detail; plugin handlers remain short queue operations.
async fn exchange(
    client: &Client,
    handle: &HookHandle,
    request: Request,
) -> Result<Snapshot, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut request = request;
    let mut job = None;
    loop {
        let bytes = client
            .call_hook(handle.clone(), velk_panels::encode(&request)?)
            .await?;
        match velk_panels::decode::<Reply>(&bytes)? {
            Reply::Ready { snapshot } => {
                snapshot.validate()?;
                return Ok(snapshot);
            }
            Reply::Pending { id } => {
                if job.is_some_and(|current| current != id) {
                    return Err("panel provider changed job identity".into());
                }
                job = Some(id);
                if Instant::now() >= deadline {
                    let _ = client
                        .call_hook(
                            handle.clone(),
                            velk_panels::encode(&Request::Cancel { id })?,
                        )
                        .await;
                    return Err("panel request timed out".into());
                }
                rxui::sleep(Duration::from_millis(8)).await;
                request = Request::Poll { id };
            }
            Reply::Ack | Reply::Unchanged | Reply::Invalidated => {
                return Err("unexpected panel change reply".into());
            }
            Reply::Cancelled => return Err("panel request cancelled".into()),
        }
    }
}
#[tracing::instrument(level="debug", skip_all, fields(providers=plugins.len(), async_work=true))]
async fn discover(
    client: Client,
    plugins: Vec<PluginInfo>,
    mut contexts: BTreeMap<String, PanelContext>,
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
            let context = contexts.remove(&plugin.id).unwrap_or_default();
            let snapshot = exchange(&client, &handle, Request::Describe { context }).await?;
            if !snapshot.effects.is_empty() {
                return Err("panel discovery cannot request host effects".into());
            }
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
        self.panels.pending.clear();
        self.panels.watching = None;
        self.panels.watch_providers.clear();
        self.panels.stream_providers.clear();
        self.panels.refresh_pending = false;
        self.panels.entries.clear();
        self.panels.handles.clear();
        self.panels.error = None;
    }
    pub(super) fn refresh_panels(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(plugins) = &self.plugins {
            self.publish_plugin_workspace(&plugins.client);
        }

        self.panels.watching = None;
        self.panels.refresh_pending = false;
        self.panels.epoch = self.panels.epoch.wrapping_add(1);
        let epoch = self.panels.epoch;
        self.panels.task = None;
        self.panels.pending.clear();
        let Some(plugins) = &self.plugins else { return };
        let client = plugins.client.clone();
        let loaded = plugins.loaded.clone();
        let contexts = loaded
            .iter()
            .map(|p| (p.id.clone(), self.panel_context(&p.id)))
            .collect();
        match cx.try_spawn(
            discover(client, loaded, contexts),
            move |this, result, cx| {
                if this.panels.epoch != epoch {
                    return;
                }
                this.panels.task = None;
                match result {
                    Ok(Ok((contributions, errors))) => {
                        this.install_panels(contributions, None, cx);
                        this.panels.error = (!errors.is_empty())
                            .then(|| errors.into_iter().take(4).collect::<Vec<_>>().join("; "));
                    }
                    Ok(Err(e)) => this.panels.error = Some(e),
                    Err(e) => this.panels.error = Some(e.to_string()),
                }
                if let Some(error) = &this.panels.error {
                    this.notice(Some(format!("Panels: {error}")));
                } else if this
                    .buffer
                    .as_ref()
                    .and_then(|buffer| buffer.status.as_deref())
                    .or(this.window_status.as_deref())
                    .is_some_and(|s| s.starts_with("Panels:"))
                {
                    this.notice(Some(format!(
                        "Panels: {} available",
                        this.panels.entries.len()
                    )));
                }
                this.start_panel_action(cx);
                this.watch_panels(cx);
            },
        ) {
            Ok(t) => self.panels.task = Some(t),
            Err(e) => self.panels.error = Some(e.to_string()),
        }
    }
    fn install_panels(
        &mut self,
        contributions: Vec<Contribution>,
        provider: Option<&str>,
        cx: &mut Context<'_, Self>,
    ) {
        let mut old = std::mem::take(&mut self.panels.entries);
        if let Some(provider) = provider {
            let (previous, retained) = old.into_iter().partition(|e| e.provider == provider);
            old = previous;
            self.panels.entries = retained;
            self.panels.handles.remove(provider);
            self.panels.watch_providers.remove(provider);
            self.panels.stream_providers.remove(provider);
        } else {
            self.panels.handles.clear();
            self.panels.watch_providers.clear();
            self.panels.stream_providers.clear();
        }
        for c in contributions {
            if c.snapshot.stream {
                self.panels.stream_providers.insert(c.provider.clone());
            }
            if c.snapshot.watch {
                self.panels.watch_providers.insert(c.provider.clone());
            }
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
                    if !nodes.iter().any(|n| n.lazy) {
                        view.expanded.retain(|id| branches.contains(id));
                    }
                    while view.expanded.len() > velk_panels::MAX_NODES {
                        view.expanded.pop_last();
                    }
                    if view
                        .selected
                        .as_ref()
                        .is_some_and(|id| !nodes.iter().any(|n| &n.id == id))
                    {
                        view.selected = None;
                    }
                }
                let previous = old.iter().position(|e| e.key == key).map(|i| old.remove(i));
                let scroll = previous
                    .as_ref()
                    .map(|e| e.scroll.clone())
                    .unwrap_or_default();
                let mut grid = previous.and_then(|e| e.grid);
                if matches!(panel.content, Content::Grid(_))
                    && grid.is_none()
                    && let Some(plugins) = &self.plugins
                {
                    match super::grid::Connection::new(
                        plugins.client.clone(),
                        self.panels.handles[&c.provider].clone(),
                        panel.id.clone(),
                        self.panels.epoch,
                        cx,
                    ) {
                        Ok(connection) => grid = Some(connection),
                        Err(error) => self.panels.error = Some(error),
                    }
                }
                if let Some(connection) = &grid {
                    connection.set_epoch(self.panels.epoch);
                }
                let mut entry = Entry {
                    provider: c.provider.clone(),
                    key,
                    panel,
                    visible: Vec::new(),
                    scroll,
                    grid,
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
        cx: &mut Context<'_, Self>,
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
        let panels = self
            .panels
            .entries
            .iter()
            .filter(|e| e.provider != provider)
            .count()
            + snapshot.panels.len();
        let count = |content: &Content| match content {
            Content::Tree(rows) => rows.len(),
            _ => 0,
        };
        let nodes = self
            .panels
            .entries
            .iter()
            .filter(|e| e.provider != provider)
            .map(|e| count(&e.panel.content))
            .sum::<usize>()
            + snapshot
                .panels
                .iter()
                .map(|p| count(&p.content))
                .sum::<usize>();
        if panels > 64 || nodes > 20_000 {
            return Err("window panel contribution limit exceeded".into());
        }
        let name = provider.clone();
        self.install_panels(
            vec![Contribution {
                provider,
                handle,
                snapshot,
            }],
            Some(&name),
            cx,
        );
        Ok(())
    }
    fn panel_context(&self, provider: &str) -> PanelContext {
        let prefix = format!("{provider}:");
        let views: Vec<_> = self
            .panels
            .ui
            .views
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .collect();
        PanelContext {
            root: self.workspace.root().map(NativePath::new),
            document: self.panels.ui.source.clone().or_else(|| {
                self.buffer
                    .as_ref()
                    .and_then(|b| b.file.as_ref())
                    .map(|f| NativePath::new(f.path()))
            }),
            initialized: !views.is_empty(),
            expanded: views
                .into_iter()
                .flat_map(|(_, v)| v.expanded.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .take(velk_panels::MAX_NODES)
                .collect(),
        }
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
        let expand = node.branch;
        let node_id = node.id.clone();
        if node.branch {
            let opening = !view.expanded.remove(&node.id);
            if opening {
                view.expanded.insert(node.id.clone());
            }
            let lazy = node.lazy;
            entry.rebuild(view);
            if !opening || !lazy {
                return;
            }
        } else if node.action.is_none() {
            return;
        }
        if self.panels.pending.len() >= 32 {
            self.panels.error = Some("Too many queued panel actions; please wait".into());
            return;
        }
        self.panels.pending.push_back(PendingAction {
            key: key.into(),
            node: node_id,
            expand,
        });
        self.start_panel_action(cx);
    }
    fn start_panel_action(&mut self, cx: &mut Context<'_, Self>) {
        if self.panels.task.is_some() {
            return;
        }
        let Some(pending) = self.panels.pending.pop_front() else {
            if self.panels.refresh_pending {
                self.refresh_panels(cx);
            }
            return;
        };
        let Some(entry) = self.panels.entries.iter().find(|e| e.key == pending.key) else {
            self.start_panel_action(cx);
            return;
        };
        let Content::Tree(nodes) = &entry.panel.content else {
            self.start_panel_action(cx);
            return;
        };
        let Some(node) = nodes.iter().find(|n| n.id == pending.node) else {
            self.start_panel_action(cx);
            return;
        };
        if pending.expand
            && !self
                .panels
                .ui
                .views
                .get(&pending.key)
                .is_some_and(|v| v.expanded.contains(&pending.node))
        {
            self.start_panel_action(cx);
            return;
        }
        let Some(plugins) = &self.plugins else {
            return;
        };
        let client = plugins.client.clone();
        let Some(handle) = self.panels.handles.get(&entry.provider).cloned() else {
            return;
        };
        let request = if pending.expand {
            Request::Expand {
                panel: entry.panel.id.clone(),
                revision: entry.panel.revision,
                node: node.id.clone(),
                expanded: self.panel_context(&entry.provider).expanded,
            }
        } else {
            let Some(action) = node.action.clone() else {
                self.start_panel_action(cx);
                return;
            };
            Request::Invoke {
                panel: entry.panel.id.clone(),
                revision: entry.panel.revision,
                node: node.id.clone(),
                action,
            }
        };
        let provider = entry.provider.clone();
        let epoch = self.panels.epoch;
        let response_handle = handle.clone();
        let allow_effects = !pending.expand;
        match cx.try_spawn(
            async move {
                let snapshot = exchange(&client, &handle, request).await?;
                if !allow_effects && !snapshot.effects.is_empty() {
                    return Err("folder expansion cannot request host effects".into());
                }
                Ok::<_, String>(snapshot)
            },
            move |this, result, cx| {
                if this.panels.epoch != epoch {
                    return;
                }
                this.panels.task = None;
                match result {
                    Ok(Ok(mut snapshot)) => {
                        let effects = std::mem::take(&mut snapshot.effects);
                        this.panels.error = this
                            .replace_panel_provider(provider, response_handle, snapshot, cx)
                            .err();
                        if this.panels.error.is_none() {
                            for effect in effects {
                                match effect {
                                    Effect::RevealDiagnostic { location } => {
                                        this.reveal_diagnostic(location, cx)
                                    }
                                    Effect::RevealBuffer { location } => {
                                        this.reveal_plugin_buffer(location, cx)
                                    }
                                    Effect::OpenFile { path } => match path.to_path() {
                                        Ok(path) => this.open_path(path, cx),
                                        Err(error) => this.panels.error = Some(error),
                                    },
                                }
                            }
                        }
                    }
                    Ok(Err(error)) => this.panels.error = Some(format!("{provider}: {error}")),
                    Err(error) => this.panels.error = Some(error.to_string()),
                }
                this.start_panel_action(cx);
            },
        ) {
            Ok(task) => self.panels.task = Some(task),
            Err(error) => self.panels.error = Some(error.to_string()),
        }
    }
    fn watch_panels(&mut self, cx: &mut Context<'_, Self>) {
        if self.panels.watching.is_some() {
            return;
        }
        let Some(plugins) = &self.plugins else {
            return;
        };
        let handles: Vec<_> = self
            .panels
            .watch_providers
            .union(&self.panels.stream_providers)
            .filter(|provider| {
                !self.panels.stream_providers.contains(*provider)
                    || (self.panels.ui.visible
                        && self.panels.entries.iter().any(|e| {
                            &e.provider == *provider
                                && !self.panels.ui.closed.contains(&e.panel.slot)
                        }))
            })
            .filter_map(|provider| {
                self.panels.handles.get(provider).map(|handle| {
                    (
                        provider.clone(),
                        handle.clone(),
                        self.panels.stream_providers.contains(provider),
                        self.panels
                            .entries
                            .iter()
                            .filter(|e| &e.provider == provider)
                            .map(|e| e.panel.revision)
                            .max()
                            .unwrap_or(0),
                    )
                })
            })
            .collect();
        if handles.is_empty() {
            return;
        }
        let streaming = handles.iter().any(|(_, _, stream, _)| *stream);
        let client = plugins.client.clone();
        let epoch = self.panels.epoch;
        let checked = self.panels.slow_checked.unwrap_or_else(Instant::now);
        match cx.try_spawn(
            async move {
                let mut slow = checked;
                loop {
                    rxui::sleep(Duration::from_millis(if streaming { 8 } else { 200 })).await;
                    let check_slow = !streaming || slow.elapsed() >= Duration::from_millis(200);
                    let mut updates = Vec::new();
                    for (provider, handle, stream, revision) in &handles {
                        if !stream && !check_slow {
                            continue;
                        }
                        let request = if *stream {
                            Request::Updates {
                                revision: *revision,
                            }
                        } else {
                            Request::Changes
                        };
                        let bytes = client
                            .call_hook(handle.clone(), velk_panels::encode(&request)?)
                            .await?;
                        match velk_panels::decode::<Reply>(&bytes)? {
                            Reply::Invalidated => return Ok::<_, String>((None, slow)),
                            Reply::Unchanged => {}
                            Reply::Ready { snapshot } if *stream => {
                                snapshot.validate()?;
                                if !snapshot.effects.is_empty() {
                                    return Err(
                                        "streamed panel updates cannot request effects".into()
                                    );
                                }
                                updates.push(Contribution {
                                    provider: provider.clone(),
                                    handle: handle.clone(),
                                    snapshot,
                                });
                            }
                            _ => return Err("unexpected panel update reply".into()),
                        }
                    }
                    if check_slow {
                        slow = Instant::now();
                    }
                    if !updates.is_empty() {
                        return Ok((Some(updates), slow));
                    }
                }
            },
            move |this, result, cx| {
                if this.panels.epoch != epoch {
                    return;
                }
                this.panels.watching = None;
                match result {
                    Ok(Ok((Some(contributions), checked))) => {
                        this.panels.slow_checked = Some(checked);
                        for contribution in contributions {
                            if let Err(error) = this.replace_panel_provider(
                                contribution.provider,
                                contribution.handle,
                                contribution.snapshot,
                                cx,
                            ) {
                                this.panels.error = Some(error);
                            }
                        }
                        this.watch_panels(cx);
                    }
                    Ok(Ok((None, checked))) => {
                        this.panels.slow_checked = Some(checked);
                        this.panels.refresh_pending = true;
                        this.start_panel_action(cx);
                    }
                    Ok(Err(error)) => this.panels.error = Some(format!("Panel watching: {error}")),
                    Err(error) => this.panels.error = Some(format!("Panel watching: {error}")),
                }
            },
        ) {
            Ok(task) => self.panels.watching = Some(task),
            Err(error) => self.panels.error = Some(error.to_string()),
        }
    }
    pub(super) fn bottom_available(&self) -> bool {
        self.panels
            .entries
            .iter()
            .any(|e| e.panel.slot == Slot::Bottom)
    }
    pub(super) fn toggle_bottom(&mut self, cx: &mut Context<'_, Self>) {
        if self.panels.ui.visible && !self.panels.ui.closed.contains(&Slot::Bottom) {
            self.panels.ui.closed.insert(Slot::Bottom);
            self.panels.focus_bottom = false;
            let _ = self.editor_focus.focus(cx);
        } else {
            self.panels.ui.visible = true;
            self.panels.ui.closed.remove(&Slot::Bottom);
            self.panels.focus_bottom = true;
            self.restore_focus_pending = true;
        }
        self.panels.watching = None;
        self.watch_panels(cx);
    }
    pub(super) fn focus_bottom(
        &mut self,
        cx: &mut Context<'_, Self>,
    ) -> Option<Result<(), rxui::FocusError>> {
        if !self.panels.focus_bottom {
            return None;
        }
        let connection = self
            .panels
            .entries
            .iter()
            .find(|e| e.panel.slot == Slot::Bottom)
            .and_then(|e| e.grid.as_ref())?;
        let result = connection.focus(cx);
        if result.is_ok() {
            self.panels.focus_bottom = false;
        }
        Some(result)
    }
    pub(super) fn panels_refresh_busy(&self) -> bool {
        self.panels.task.is_some()
    }
    pub(super) fn choose_folder(&mut self, cx: &mut Context<'_, Self>) {
        if self.busy() || self.reloading() {
            return;
        }
        let Some(window) = &self.window else {
            return;
        };
        let directory = self
            .panels
            .ui
            .root
            .as_ref()
            .and_then(|p| p.to_path().ok())
            .or_else(|| {
                self.panels
                    .ui
                    .source
                    .as_ref()
                    .and_then(|p| p.to_path().ok())
                    .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
            });
        let mut options = FileDialog::new().parent(window).title("Open folder");
        if let Some(directory) = directory {
            options = options.directory(directory);
        }
        match cx.pick_folder(options, |this, result, cx| {
            this.dialog = None;
            match result {
                Ok(Some(path)) => this.set_panel_root(path, cx),
                Ok(None) => {}
                Err(error) => this.notice(Some(format!("Open folder failed: {error}"))),
            }
        }) {
            Ok(task) => self.dialog = Some(task),
            Err(error) => self.notice(Some(format!("Open folder failed: {error}"))),
        }
    }
    fn set_panel_root(&mut self, path: std::path::PathBuf, cx: &mut Context<'_, Self>) {
        self.workspace.set_root(path.clone());
        self.panels.ui.root = Some(NativePath::new(&path));
        self.schedule_recovery(cx);
        self.panels.ui.views.clear();
        self.panels.ui.visible = true;
        self.panels.ui.closed.clear();
        self.suspend_panels();
        self.refresh_panels(cx);
    }
    pub(super) fn show_plugin_panel(
        &mut self,
        provider: &str,
        panel: &str,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(entry) = self
            .panels
            .entries
            .iter()
            .find(|e| e.provider == provider && e.panel.id == panel)
        else {
            self.notice(Some(
                "Panel is not available; refresh panels and try again".into(),
            ));
            return;
        };
        let slot = entry.panel.slot;
        self.panels.ui.tabs.insert(slot, entry.key.clone());
        self.panels.ui.closed.remove(&slot);
        self.panels.ui.visible = true;
        self.panels.watching = None;
        self.watch_panels(cx);
    }
    /// Applies a layout's panel default; `cx` restarts watching for streams.
    pub(super) fn show_panels(&mut self, show: bool, cx: Option<&mut Context<'_, Self>>) {
        self.panels.ui.visible = show;
        if show {
            self.panels.ui.closed.clear();
        }
        if let Some(cx) = cx {
            self.panels.watching = None;
            self.watch_panels(cx);
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
                this.notice(Some(self::empty_notice(&this.panels)));
            }
            if this.panels.ui.visible {
                this.panels.ui.closed.clear();
            } else {
                let _ = this.editor_focus.focus(cx);
            }
            this.panels.watching = None;
            this.watch_panels(cx);
        })
        .label("Panels: Toggle visibility")
        .shortcut(Shortcut::primary("b"))
    }
    fn slot_open(&self, slot: Slot) -> bool {
        self.panels.ui.visible && !self.panels.ui.closed.contains(&slot)
    }
    fn slot_entries(&self, slot: Slot) -> Vec<&Entry> {
        self.panels
            .entries
            .iter()
            .filter(|e| e.panel.slot == slot)
            .collect()
    }
    /// The selected panel for a role, if that role is open and has panels.
    fn slot_entry(&self, slot: Slot) -> Option<&Entry> {
        if !self.slot_open(slot) {
            return None;
        }
        let entries = self.slot_entries(slot);
        self.panels
            .ui
            .tabs
            .get(&slot)
            .and_then(|key| entries.iter().find(|e| &e.key == key).copied())
            .or_else(|| entries.first().copied())
    }
    pub(super) fn panel_titles(&self, slot: Slot) -> Vec<(String, String)> {
        self.slot_entries(slot)
            .into_iter()
            .map(|e| (e.key.clone(), e.panel.title.clone()))
            .collect()
    }
    /// Key of the sidebar panel currently shown.
    pub(super) fn sidebar_panel(&self) -> Option<String> {
        self.slot_entry(Slot::Left).map(|e| e.key.clone())
    }
    /// Shows a sidebar panel, or collapses the sidebar if it is already shown.
    pub(super) fn toggle_sidebar(&mut self, key: &str, cx: &mut Context<'_, Self>) {
        if self.sidebar_panel().as_deref() == Some(key) {
            self.panels.ui.closed.insert(Slot::Left);
            let _ = self.editor_focus.focus(cx);
        } else {
            if !self.panels.ui.visible {
                // Opening the sidebar must not also open the bottom drawer.
                self.panels.ui.visible = true;
                self.panels.ui.closed = [Slot::Right, Slot::Bottom].into();
            }
            self.panels.ui.closed.remove(&Slot::Left);
            self.panels.ui.tabs.insert(Slot::Left, key.into());
        }
        self.panels.watching = None;
        self.watch_panels(cx);
    }
    fn close_slot(&mut self, slot: Slot, cx: &mut Context<'_, Self>) {
        self.panels.ui.closed.insert(slot);
        self.panels.watching = None;
        self.watch_panels(cx);
        let _ = self.editor_focus.focus(cx);
    }
    /// Panel titles as text tabs, then a close button. With `tabs` false only the
    /// selected title is shown, for when the rail switches panels.
    fn panel_header(
        &self,
        slot: Slot,
        selected: &str,
        tabs: bool,
        cx: &mut ViewContext<'_, Self>,
    ) -> Element {
        let mut header = row()
            .fill_width()
            .flex_shrink(0.)
            .gap(2.)
            .padding_y(0.)
            .align_items(AlignItems::CENTER)
            .font_size(12.);
        for tab in self.slot_entries(slot) {
            let active = tab.key == selected;
            if !tabs && !active {
                continue;
            }
            let key = tab.key.clone();
            header = header.child(
                button(tab.panel.title.clone())
                    .key(format!("panel-tab-{}", tab.key))
                    .accessibility_selected(active)
                    .variant(rxui::ButtonVariant::Quiet)
                    .background([0.; 4])
                    .color(if active {
                        ThemeColor::Text
                    } else {
                        ThemeColor::TextMuted
                    })
                    .height(22.)
                    .padding_y(0.)
                    .padding_x(6.)
                    .on_click(cx.listener(move |this, _, _| {
                        this.panels.ui.tabs.insert(slot, key.clone());
                    })),
            );
        }
        header.child(row().flex_grow(1.)).child(tooltip(
            cx,
            button(super::icons::Icon::Close.element(12.))
                .accessibility_label("Close panel")
                .key(format!("panel-close-{slot:?}"))
                .variant(rxui::ButtonVariant::Quiet)
                .color(ThemeColor::TextMuted)
                .size(22., 22.)
                .padding(0.)
                .radius(5.)
                .justify_content(JustifyContent::CENTER)
                .align_items(AlignItems::CENTER)
                .on_click(cx.listener(move |this, _, cx| this.close_slot(slot, cx))),
            match slot {
                Slot::Bottom => "Close panel (Ctrl+`)".to_owned(),
                _ => format!("Close panels ({})", super::palette::primary("B")),
            },
        ))
    }
    fn panel_content(&self, entry: &Entry, cx: &mut ViewContext<'_, Self>) -> Element {
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
            Content::Grid(grid) => entry
                .grid
                .as_ref()
                .map(|connection| connection.element(grid.clone(), cx))
                .unwrap_or_else(|| label("Interactive panel unavailable").into_element()),
            Content::Tree(nodes) => {
                let width = entry
                    .scroll
                    .state(cx)
                    .map_or(280., |state| state.viewport[0]);
                virtual_list(entry.visible.len(), 24., &entry.scroll, cx, |i| {
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
                        .font_size(12.5)
                        .height(24.)
                        .fill_width()
                        .radius(0.)
                        .padding_y(0.)
                        .padding_left((12. + f32::from(node.depth) * 14.).min(width * 0.35))
                        .padding_right(12.)
                        .text_overflow(rxui::TextOverflow::Ellipsis)
                        .variant(rxui::ButtonVariant::Quiet)
                        .color(if selected {
                            ThemeColor::Text
                        } else {
                            ThemeColor::TextMuted
                        })
                        .background(if selected {
                            ThemeColor::Custom(crate::theme::HOVER)
                        } else {
                            ThemeColor::Surface
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
        let mut content = column()
            .fill_width()
            .fill_height()
            .min_height(0.)
            .min_width(0.)
            .flex_shrink(1.)
            .child(content.min_height(0.).min_width(0.).flex_shrink(1.));
        if self.panels.task.is_some() {
            content = content.child(label("Loading…").font_size(12.).padding(6.));
        }
        if let Some(error) = &self.panels.error {
            content = content.child(label(error.clone()).font_size(12.).padding(6.));
        }
        content.into_element()
    }
    /// A panel role docked beside or below the editor.
    pub(super) fn panel_slot(
        &self,
        slot: Slot,
        layout: crate::settings::Layout,
        cx: &mut ViewContext<'_, Self>,
    ) -> Option<Element> {
        let entry = self.slot_entry(slot)?;
        // With a rail, its icons switch sidebar panels instead of tabs.
        let tabs = !(layout.rail && slot == Slot::Left);
        let header = self
            .panel_header(slot, &entry.key, tabs, cx)
            .height(if slot == Slot::Bottom { 28. } else { 32. })
            .padding_x(6.);
        let panel = column()
            .key(format!("panel-slot-{slot:?}"))
            .accessibility_label(entry.panel.title.clone())
            .background(ThemeColor::Surface)
            .border_color(ThemeColor::Divider)
            .clip()
            .flex_shrink(0.)
            .min_height(0.)
            .child(header)
            .child(self.panel_content(entry, cx));
        Some(match slot {
            Slot::Bottom => panel.fill_width().border_top(1.).height(
                if matches!(entry.panel.content, Content::Grid(_)) {
                    320.
                } else {
                    180.
                },
            ),
            Slot::Left => panel.width(280.).fill_height().border_right(1.),
            Slot::Right => panel.width(280.).fill_height().border_left(1.),
        })
    }
    /// Tiled layout: the editor and each open panel role are panes in a split
    /// tree, each with a thin winbar. The active pane's winbar has an accent edge.
    pub(super) fn tiled(&self, editor: Element, cx: &mut ViewContext<'_, Self>) -> Element {
        let mut tree = DockTree::from_panels(["editor"]).expect("one panel");
        let editor_group = tree.root().id();
        for (slot, side) in [
            (Slot::Left, DockSide::Left),
            (Slot::Right, DockSide::Right),
            (Slot::Bottom, DockSide::Bottom),
        ] {
            if self.slot_entry(slot).is_some() {
                let position = self.panels.split.get(&slot).copied().unwrap_or(match slot {
                    Slot::Left => SplitPosition::Pixels(240.),
                    Slot::Right => SplitPosition::Fraction(0.75),
                    Slot::Bottom => SplitPosition::Fraction(0.7),
                });
                tree.split(editor_group, side, pane_key(slot), position)
                    .expect("split the editor group");
            }
        }
        self.tiled_node(tree.root(), &mut Some(editor), cx)
    }
    fn tiled_node(
        &self,
        node: &DockNode,
        editor: &mut Option<Element>,
        cx: &mut ViewContext<'_, Self>,
    ) -> Element {
        let slot_of = |node: &DockNode| match node {
            DockNode::Tabs(group) => group.selected().and_then(|key| {
                [Slot::Left, Slot::Right, Slot::Bottom]
                    .into_iter()
                    .find(|slot| &pane_key(*slot) == key)
            }),
            DockNode::Split(_) => None,
        };
        match node {
            DockNode::Tabs(_) => match slot_of(node) {
                None => {
                    let (directory, name) = self.path_parts();
                    let winbar = row()
                        .child(label(directory).color(ThemeColor::TextDisabled))
                        .child(label(name));
                    self.pane(None, winbar, editor.take().expect("one editor pane"), cx)
                }
                Some(slot) => {
                    let entry = self.slot_entry(slot).expect("open pane");
                    let winbar = self.panel_header(slot, &entry.key, true, cx);
                    let content = self.panel_content(entry, cx);
                    self.pane(Some(slot), winbar, content, cx)
                }
            },
            DockNode::Split(split) => {
                let slot = slot_of(split.first())
                    .or_else(|| slot_of(split.second()))
                    .unwrap_or(Slot::Bottom);
                let first = self.tiled_node(split.first(), editor, cx);
                let second = self.tiled_node(split.second(), editor, cx);
                match split.axis() {
                    rxui::Axis::Horizontal => split_row(first, second),
                    rxui::Axis::Vertical => split_column(first, second),
                }
                .position(split.position())
                .min_first(120.)
                .min_second(120.)
                .divider_size(4.)
                .on_resize(cx.listener(move |this, event: &ResizeEvent, _| {
                    this.panels.split.insert(slot, event.position);
                }))
                .into_element()
                .key(format!("tiled-split-{slot:?}"))
            }
        }
    }
    fn pane(
        &self,
        slot: Option<Slot>,
        winbar: Element,
        content: Element,
        cx: &mut ViewContext<'_, Self>,
    ) -> Element {
        let active = self.panels.active == slot;
        let background = if slot.is_some() {
            ThemeColor::Surface
        } else {
            ThemeColor::Background
        };
        column()
            .key(format!("pane-{slot:?}"))
            .fill_width()
            .fill_height()
            .min_width(0.)
            .min_height(0.)
            .background(background)
            .on_pointer_down_capture(cx.listener(move |this, _: &PointerInput, cx| {
                if this.panels.active == slot {
                    cx.unchanged();
                }
                this.panels.active = slot;
            }))
            .child(
                row()
                    .key("winbar-edge")
                    .fill_width()
                    .height(2.)
                    .flex_shrink(0.)
                    .background(if active {
                        ThemeColor::Accent
                    } else {
                        background
                    }),
            )
            .child(
                winbar
                    .key("winbar")
                    .fill_width()
                    .height(24.)
                    .flex_shrink(0.)
                    .padding_x(12.)
                    .padding_y(0.)
                    .align_items(AlignItems::CENTER)
                    .font_size(12.)
                    .color(if active {
                        ThemeColor::Text
                    } else {
                        ThemeColor::TextDisabled
                    })
                    .border_bottom(1.)
                    .border_color(ThemeColor::Divider),
            )
            .child(content)
            .into_element()
    }
}
fn pane_key(slot: Slot) -> Key {
    Key::from(format!("{slot:?}"))
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
            lazy: false,
            action: None,
        }
    }
    #[test]
    #[allow(unsafe_code)]
    fn panel_discovery_can_finish_after_the_last_buffer_closes_or_in_an_empty_window() {
        use rxui::{BackgroundFuture, BlockingJob, SpawnError, TaskExecutor};
        use std::sync::{Arc, Mutex};
        #[derive(Default)]
        struct Executor {
            futures: Mutex<Vec<BackgroundFuture>>,
            jobs: Mutex<Vec<BlockingJob>>,
        }
        impl TaskExecutor for Executor {
            fn spawn(&self, future: BackgroundFuture) -> Result<(), SpawnError> {
                self.futures.lock().unwrap().push(future);
                Ok(())
            }
            fn spawn_blocking(&self, job: BlockingJob) -> Result<(), SpawnError> {
                self.jobs.lock().unwrap().push(job);
                Ok(())
            }
        }
        for (close_last, notice) in [
            (true, None),
            (false, Some("Panels: refreshing")),
            (false, Some("Unrelated notice")),
        ] {
            let directory = tempfile::tempdir().unwrap();
            // SAFETY: this test-owned directory contains no native libraries.
            let host =
                unsafe { velk_plugin_host::Runtime::start(directory.path().into()) }.unwrap();
            let executor = Arc::new(Executor::default());
            let mut runtime = Runtime::new();
            runtime.configure_tasks(executor.clone(), || {}).unwrap();
            let document = runtime.update(|cx| cx.new(|_| Document::empty(1)));
            runtime.update(|cx| {
                document.update(cx, |this, cx| {
                    if close_last {
                        this.new_buffer(cx);
                    }
                    this.plugins = Some(super::super::plugins::State::new(
                        host.client(),
                        String::new(),
                    ));
                    this.refresh_panels(cx);
                    assert!(this.panels.task.is_some());
                    if close_last {
                        this.intent(super::super::Intent::Buffer, cx);
                    }
                    assert!(!this.has_buffers());
                    this.notice(notice.map(str::to_owned));
                })
            });
            let futures = std::mem::take(&mut *executor.futures.lock().unwrap());
            let jobs = std::mem::take(&mut *executor.jobs.lock().unwrap());
            std::thread::spawn(move || {
                for future in futures {
                    pollster::block_on(future);
                }
                for job in jobs {
                    job();
                }
            })
            .join()
            .unwrap();
            runtime.poll_tasks();
            runtime.flush().unwrap();
            runtime.update(|cx| {
                let document = document.read(cx);
                assert!(!document.has_buffers());
                assert!(document.panels.task.is_none());
                assert!(document.panels.error.is_none());
                assert_eq!(
                    document.window_status.as_deref(),
                    match notice {
                        Some("Panels: refreshing") => Some("Panels: 0 available"),
                        other => other,
                    }
                );
            });
            let mut ui = Ui::new(&mut runtime, document).unwrap();
            ui.prepare(&mut runtime, [800., 500.], &mut Measure)
                .unwrap();
            assert!(
                ui.elements()
                    .any(|e| e.key == Some(&Key::from("empty-workspace")))
            );
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
            grid: None,
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
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                this.panels.ui.closed.insert(Slot::Left);
                this.panels.ui.closed.insert(Slot::Bottom);
                this.show_plugin_panel("test", "Bottom", cx);
                assert!(this.panels.ui.visible);
                assert!(this.panels.ui.closed.contains(&Slot::Left));
                assert!(!this.panels.ui.closed.contains(&Slot::Bottom));
                assert_eq!(this.panels.ui.tabs[&Slot::Bottom], "test:Bottom");
            })
        });
        // A lifecycle transition removes cached contributions immediately.
        runtime.update(|cx| document.update(cx, |this, _| this.suspend_panels()));
        prepare(&mut ui, &mut runtime);
        assert_eq!(bounds(&ui), full);
        runtime.update(|cx| assert_eq!(editor.read(cx).buffer().text().to_string(), "original"));
    }
    #[test]
    fn narrow_tree_rows_keep_text_space_and_reveal_both_scroll_extremes() {
        let mut runtime = Runtime::new();
        let document = runtime.update(|cx| cx.new(|_| Document::empty(1)));
        let scroll = rxui::ScrollHandle::new();
        runtime.update(|cx| {
            document.update(cx, |document, _| {
                let nodes: Vec<_> = (0..100)
                    .map(|i| {
                        let mut node = node(format!("row-{i}"), i.min(32) as u8, i < 99);
                        node.label =
                            "A long undo description that must stay within a single tree row"
                                .into();
                        node
                    })
                    .collect();
                let view = ViewState {
                    expanded: nodes.iter().map(|n| n.id.clone()).collect(),
                    selected: None,
                };
                let mut entry = entry(Slot::Right, Content::Tree(nodes));
                entry.scroll = scroll.clone();
                entry.rebuild(&view);
                document.panels.ui.views.insert(entry.key.clone(), view);
                document.panels.ui.visible = true;
                document.panels.entries.push(entry);
            })
        });
        let mut ui = Ui::new(&mut runtime, document).unwrap();
        ui.prepare(&mut runtime, [460., 350.], &mut Measure)
            .unwrap();
        let check = |ui: &Ui<Document>, id: usize| {
            let key = rxui::Key::from(format!("panel-row-test:Right-row-{id}"));
            let wrapper = ui.elements().find(|e| e.key == Some(&key)).unwrap().id;
            let row = ui
                .elements()
                .find(|e| {
                    e.kind == rxui::ElementType::Button
                        && (e.id == wrapper || e.parent == Some(wrapper))
                })
                .unwrap();
            assert_eq!(row.text_style.overflow, rxui::TextOverflow::Ellipsis);
            assert_eq!(row.bounds.height, 24.);
            assert!(row.content_bounds.width >= row.bounds.width * 0.5);
            assert!(row.bounds.y >= row.clip_bounds.y - 0.01);
            assert!(
                row.bounds.y + row.bounds.height
                    <= row.clip_bounds.y + row.clip_bounds.height + 0.01
            );
        };
        check(&ui, 0);
        let viewport = ui.elements().find(|e| e.scroll_range[1] > 0.).unwrap().id;
        ui.semantic_action(
            &mut runtime,
            rxui::SemanticAction::Scroll {
                target: viewport,
                offset: [0., 1_000_000.],
            },
            &mut Measure,
        )
        .unwrap();
        ui.prepare(&mut runtime, [460., 350.], &mut Measure)
            .unwrap();
        check(&ui, 99);
        ui.semantic_action(
            &mut runtime,
            rxui::SemanticAction::Scroll {
                target: viewport,
                offset: [0., 0.],
            },
            &mut Measure,
        )
        .unwrap();
        ui.prepare(&mut runtime, [460., 350.], &mut Measure)
            .unwrap();
        check(&ui, 0);
    }
    #[test]
    fn rail_toggles_one_sidebar_and_tiled_places_roles_in_panes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let editor = runtime.update(|cx| cx.new(|_| editor));
        let document = runtime.update(|cx| cx.new(|cx| Document::new(editor, file, path, cx)));
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                this.panels.entries = vec![
                    entry(Slot::Left, Content::Text("Files".into())),
                    entry(Slot::Bottom, Content::Text("Output".into())),
                ];
                this.set_layout("rail", cx);
            })
        });
        let mut ui = Ui::new(&mut runtime, document.clone()).unwrap();
        let prepare = |ui: &mut Ui<Document>, r: &mut Runtime| {
            ui.prepare(r, [1000., 700.], &mut Measure).unwrap()
        };
        let has = |ui: &Ui<Document>, key: &str| {
            ui.elements().any(|e| e.key == Some(&rxui::Key::from(key)))
        };
        prepare(&mut ui, &mut runtime);
        assert!(has(&ui, "rail-test:Left") && !has(&ui, "panel-slot-Left"));
        click(&mut ui, &mut runtime, "rail-test:Left");
        prepare(&mut ui, &mut runtime);
        // The sidebar opens alone; the bottom drawer stays closed.
        assert!(has(&ui, "panel-slot-Left") && !has(&ui, "panel-slot-Bottom"));
        click(&mut ui, &mut runtime, "rail-test:Left");
        prepare(&mut ui, &mut runtime);
        assert!(!has(&ui, "panel-slot-Left"));
        runtime.update(|cx| document.update(cx, |this, cx| this.set_layout("tiled", cx)));
        prepare(&mut ui, &mut runtime);
        for key in ["pane-None", "pane-Some(Left)", "pane-Some(Bottom)"] {
            assert!(has(&ui, key), "{key}");
        }
        assert!(!has(&ui, "rail"));
        click(&mut ui, &mut runtime, "panel-close-Bottom");
        prepare(&mut ui, &mut runtime);
        assert!(!has(&ui, "pane-Some(Bottom)") && has(&ui, "pane-Some(Left)"));
    }
    #[test]
    fn choosing_folder_preserves_buffers_and_persists_exact_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let editor = runtime.update(|cx| cx.new(|_| editor));
        let document =
            runtime.update(|cx| cx.new(|cx| Document::new(editor.clone(), file, path.clone(), cx)));
        runtime.update(|cx| {
            editor.update(cx, |e, _| e.insert("unsaved"));
            document.update(cx, |d, cx| {
                d.panels
                    .ui
                    .views
                    .insert("test:files".into(), ViewState::default());
                d.set_panel_root(dir.path().to_path_buf(), cx);
                assert_eq!(d.buffer.path, path);
                assert!(d.buffer.editor.read(cx).buffer().is_dirty());
                assert!(d.panels.ui.views.is_empty());
                assert!(d.panels.ui.visible);
                let restored: UiState =
                    serde_json::from_slice(&serde_json::to_vec(&d.panels.ui).unwrap()).unwrap();
                assert_eq!(restored.root.unwrap().to_path().unwrap(), dir.path());
                assert_eq!(
                    d.panel_context("test").root.unwrap().to_path().unwrap(),
                    dir.path()
                );
            });
        });
    }
}
