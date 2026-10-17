//! Native plugins with host-scheduled async requests and scoped semantic sessions.
//! No Rust future crosses the ABI. Language definitions resolve exact transport
//! dependencies; manifests declare a validated dependency graph.
use serde::{Deserialize, Serialize};
use std::{
    path::{Component, Path, PathBuf},
    sync::mpsc,
    thread,
};
mod disk_diagnostics;
mod graph;
mod native;
mod services;
pub use graph::{DependencyRow, InstallReason};
pub use native::{HookDescriptor, HookHandle, Manager, PluginInfo};
pub use services::HostRequest;

/// The bundled default; callers may select any compatible loaded provider.
pub const DEFAULT_MARKDOWN_PROVIDER: &str = "velk.markdown";
pub use velk_plugin_contracts::{Capability, PluginMetadata, buffers, host, settings};

pub mod hook_kind {
    pub use velk_plugin_api::{CONTRIBUTION, EVENT, REQUEST, SERVICE};
}
/// Host-defined hook names, for describing what a plugin provides.
pub mod hook_name {
    pub use velk_plugin_api::{
        DIAGNOSTICS_HOOK, HOST_HOOK, LANGUAGE_HOOK, LIFECYCLE_RESTORE_HOOK,
        LIFECYCLE_SNAPSHOT_HOOK, MARKUP_HOOK, METADATA_HOOK, SEMANTIC_HOOK, SETTINGS_HOOK,
    };
}
pub type PluginStates = Vec<(String, Vec<u8>)>;
pub const MANIFEST: &str = "plugins.json";
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub plugins: Vec<PluginSpec>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSpec {
    pub id: String,
    /// Relative path inside the plugin directory, normally content-addressed.
    pub library: PathBuf,
    pub digest: String,
    /// Select this ID from a multi-plugin image. Absent/false means standalone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pack: bool,
    /// Exact required plugin IDs. Distribution metadata, independent of packaging.
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub install_reason: InstallReason,
}
impl Manifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.plugins.len() > 256 {
            return Err("plugin manifest exceeds 256 entries".into());
        }
        let mut ids = std::collections::HashSet::new();
        let mut libraries = std::collections::HashMap::new();
        for p in &self.plugins {
            if let Some(previous) = libraries.insert(&p.library, (&p.digest, p.pack))
                && previous != (&p.digest, p.pack)
            {
                return Err("one library path has conflicting digests or formats".into());
            }
            if velk_plugin_api::parse_plugin_id(&p.id).is_none() {
                return Err(format!(
                    "invalid plugin ID: {} (expected author.[namespace.]name)",
                    p.id
                ));
            }
            if !ids.insert(&p.id) {
                return Err(format!("duplicate plugin ID: {}", p.id));
            }
            if p.library.as_os_str().is_empty()
                || p.library
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err(format!("plugin {} needs a relative library path", p.id));
            }
            if p.digest.len() != 64 || !p.digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("invalid digest for {}", p.id));
            }
        }
        self.dependency_order()?;
        Ok(())
    }
}
#[derive(Debug)]
pub enum Event {
    Loaded(Box<PluginInfo>),
    Removed(String),
    Rejected { id: String, message: String },
}
#[derive(Debug)]
pub struct Report {
    pub undo: std::collections::BTreeMap<
        String,
        std::sync::Arc<dyn velk_core_contracts::history::Provider>,
    >,
    pub syntax: std::collections::BTreeMap<String, std::sync::Arc<dyn velk_syntax::Provider>>,
    pub editing: velk_editing::Providers,
    pub matching: velk_plugin_contracts::commands::completion::matching::Policies,
    pub loaded: usize,
    /// Owned snapshot after this operation, including unchanged active plugins.
    pub plugins: Vec<PluginInfo>,
    pub events: Vec<Event>,
    /// Manifest IDs, including plugins that are not loaded.
    pub available: Vec<String>,
    pub inventory: Vec<PluginSpec>,
}
enum Operation {
    Load(Vec<String>),
    UnloadAll,
    Unload(String),
    Restart(Option<String>),
    LoadSelected(Vec<String>, PluginStates),
}
enum Request {
    Lifecycle {
        operation: Operation,
        reply: async_channel::Sender<Result<Report, String>>,
    },
    Render {
        provider: String,
        source: String,
        reply: async_channel::Sender<Result<velk_markup::Document, String>>,
    },
    OpenSemantic {
        id: u64,
        session: velk_semantic::Session,
    },
    WakeSemantic,
    Bind {
        plugin: String,
        name: String,
        reply: async_channel::Sender<Result<HookHandle, String>>,
    },
    Call {
        handle: HookHandle,
        input: Vec<u8>,
        reply: async_channel::Sender<Result<Vec<u8>, String>>,
    },
    Emit {
        name: String,
        input: Vec<u8>,
        reply: async_channel::Sender<Result<usize, String>>,
    },
    Snapshot {
        ids: Vec<String>,
        reply: async_channel::Sender<Result<PluginStates, String>>,
    },
    Settings(settings::PluginValues),
    DependencyCall {
        caller: String,
        generation: u64,
        provider: String,
        hook: String,
        input: Vec<u8>,
        reply: mpsc::SyncSender<Result<Vec<u8>, String>>,
    },
    Stop,
}
struct Queued {
    request: Request,
    span: tracing::Span,
    queued: Option<std::time::Instant>,
}
#[derive(Clone)]
struct Sender(mpsc::SyncSender<Queued>);
impl Sender {
    fn queued(request: Request) -> Queued {
        let operation = match &request {
            Request::Lifecycle { operation, .. } => match operation {
                Operation::Load(_) => "load",
                Operation::UnloadAll => "unload_all",
                Operation::Unload(_) => "unload",
                Operation::Restart(_) => "restart",
                Operation::LoadSelected(..) => "restore",
            },
            Request::Render { .. } => "markdown",
            Request::OpenSemantic { .. } => "semantic.open",
            Request::WakeSemantic => "semantic.wake",
            Request::Bind { .. } => "hook.bind",
            Request::Call { .. } => "hook.call",
            Request::Emit { .. } => "hook.emit",
            Request::Snapshot { .. } => "snapshot",
            Request::Settings(_) => "settings",
            Request::DependencyCall { .. } => "dependency_call",
            Request::Stop => "stop",
        };
        let span = tracing::debug_span!("plugin.request", async_work = true, operation);
        let queued = (!span.is_disabled()).then(std::time::Instant::now);
        Queued {
            request,
            span,
            queued,
        }
    }
    fn try_send(&self, request: Request) -> Result<(), String> {
        self.0.try_send(Self::queued(request)).map_err(|error| {
            tracing::warn!("plugin queue rejected request");
            error.to_string()
        })
    }
    fn send(&self, request: Request) -> Result<(), String> {
        self.0
            .send(Self::queued(request))
            .map_err(|error| error.to_string())
    }
}
/// Safe command endpoint. Lifecycle work is serialized on a sleeping worker.
#[derive(Clone)]
pub struct Client {
    host: std::sync::Arc<services::Bridge>,
    sender: Sender,
    next_session: std::sync::Arc<std::sync::atomic::AtomicU64>,
}
impl Client {
    /// Reconcile saved-file results off the UI thread before presenting them.
    pub fn reconcile_diagnostics(&self) {
        self.host.reconcile_diagnostics();
    }
    /// Latest publications, including empty replacements that clear earlier results.
    pub fn diagnostic_publications(&self) -> Vec<std::sync::Arc<host::DiagnosticPublication>> {
        self.host.diagnostics()
    }
    /// Publish the authoritative UI-owned workspace context for worker-side file reads.
    pub fn set_workspace(&self, workspace: host::Workspace) {
        *self.host.workspace.lock().unwrap() = Some(workspace);
    }
    /// One editor receiver should consume requests for this runtime.
    pub fn host_requests(&self) -> async_channel::Receiver<HostRequest> {
        self.host.receiver.clone()
    }

    /// Blocking delivery for an editor-owned background dispatcher, never the UI.
    pub fn publish_buffer_events(
        &self,
        events: Vec<velk_plugin_contracts::buffers::Event>,
    ) -> Result<(), String> {
        let input = serde_json::to_vec(&events).map_err(|e| e.to_string())?;
        if events.len() > 512 || input.len() > velk_plugin_api::MAX_INPUT_BYTES {
            return Err("buffer event batch exceeds native limits".into());
        }
        let (reply, receive) = async_channel::bounded(1);
        self.sender.send(Request::Emit {
            name: velk_plugin_contracts::buffers::HOOK.into(),
            input,
            reply,
        })?;
        receive.recv_blocking().map_err(|e| e.to_string())??;
        Ok(())
    }
    /// Store overrides for the next load/restart; running instances are unchanged.
    pub fn set_settings(&self, settings: settings::PluginValues) -> Result<(), String> {
        if serde_json::to_vec(&settings)
            .map_err(|e| e.to_string())?
            .len()
            > velk_plugin_api::MAX_INPUT_BYTES
        {
            return Err("plugin settings exceed 1 MiB".into());
        }
        self.sender
            .try_send(Request::Settings(settings))
            .map_err(|e| format!("plugin queue unavailable: {e}"))
    }

    pub fn bind_hook(
        &self,
        plugin: String,
        name: String,
    ) -> Result<async_channel::Receiver<Result<HookHandle, String>>, String> {
        let (reply, receive) = async_channel::bounded(1);
        self.sender
            .try_send(Request::Bind {
                plugin,
                name,
                reply,
            })
            .map_err(|e| format!("plugin queue unavailable: {e}"))?;
        Ok(receive)
    }
    pub async fn call_hook(&self, handle: HookHandle, input: Vec<u8>) -> Result<Vec<u8>, String> {
        if input.len() > velk_plugin_api::MAX_INPUT_BYTES {
            return Err("hook input exceeds 1 MiB".into());
        }
        let (reply, receive) = async_channel::bounded(1);
        self.sender
            .try_send(Request::Call {
                handle,
                input,
                reply,
            })
            .map_err(|e| format!("plugin queue unavailable: {e}"))?;
        receive.recv().await.map_err(|e| e.to_string())?
    }
    pub fn emit_hook(
        &self,
        name: String,
        input: Vec<u8>,
    ) -> Result<async_channel::Receiver<Result<usize, String>>, String> {
        if input.len() > velk_plugin_api::MAX_INPUT_BYTES {
            return Err("hook event exceeds 1 MiB".into());
        }
        let (reply, receive) = async_channel::bounded(1);
        self.sender
            .try_send(Request::Emit { name, input, reply })
            .map_err(|e| format!("plugin queue unavailable: {e}"))?;
        Ok(receive)
    }

    /// Queue-only frontend: no reactor/thread or native call on the UI.
    /// Open documents remain attached to this host endpoint across provider unload/load.
    pub fn semantic_service(&self) -> Result<std::sync::Arc<velk_semantic::Service>, String> {
        let id = self
            .next_session
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sender = self.sender.clone();
        let (service, session) = velk_semantic::Service::external(move || {
            // If full, the worker already has work to process; it drains every
            // session after each request, so no document wake can be lost.
            let _ = sender.try_send(Request::WakeSemantic);
        });
        self.sender
            .try_send(Request::OpenSemantic { id, session })
            .map_err(|e| format!("plugin queue unavailable: {e}"))?;
        Ok(service)
    }

    pub fn load(&self) -> Result<async_channel::Receiver<Result<Report, String>>, String> {
        self.request(Operation::Load(Vec::new()))
    }
    /// Load manifest plugins except the given IDs.
    pub fn load_except(
        &self,
        excluded: Vec<String>,
    ) -> Result<async_channel::Receiver<Result<Report, String>>, String> {
        self.request(Operation::Load(excluded))
    }
    pub fn unload_all(&self) -> Result<async_channel::Receiver<Result<Report, String>>, String> {
        self.request(Operation::UnloadAll)
    }
    pub fn unload(
        &self,
        id: String,
    ) -> Result<async_channel::Receiver<Result<Report, String>>, String> {
        self.request(Operation::Unload(id))
    }
    /// Load only the requested manifest IDs, preserving a restored enabled set.
    pub fn load_selected(
        &self,
        ids: Vec<String>,
    ) -> Result<async_channel::Receiver<Result<Report, String>>, String> {
        self.load_restored(ids, Vec::new())
    }
    pub fn load_restored(
        &self,
        ids: Vec<String>,
        states: PluginStates,
    ) -> Result<async_channel::Receiver<Result<Report, String>>, String> {
        if states
            .iter()
            .any(|(id, bytes)| !ids.contains(id) || bytes.len() > velk_plugin_api::MAX_INPUT_BYTES)
        {
            return Err("invalid plugin session state".into());
        }
        self.request(Operation::LoadSelected(ids, states))
    }
    pub async fn snapshot_plugins(&self, ids: Vec<String>) -> Result<PluginStates, String> {
        let (reply, receive) = async_channel::bounded(1);
        self.sender
            .try_send(Request::Snapshot { ids, reply })
            .map_err(|e| e.to_string())?;
        receive.recv().await.map_err(|e| e.to_string())?
    }
    /// Restart one active plugin, or all active plugins, in one serialized operation.
    pub fn restart(
        &self,
        id: Option<String>,
    ) -> Result<async_channel::Receiver<Result<Report, String>>, String> {
        self.request(Operation::Restart(id))
    }
    /// Render with the bundled default provider.
    /// The returned future is local Rust scheduling, not a future across the ABI.
    /// Dropping it closes the result channel, allowing queued work to be skipped.
    pub async fn render_markdown(&self, source: String) -> Result<velk_markup::Document, String> {
        self.render_markdown_with(DEFAULT_MARKDOWN_PROVIDER, source)
            .await
    }
    /// Select by exact plugin ID. A missing, disabled, or incompatible provider
    /// yields literal text; another loaded provider is never chosen implicitly.
    pub async fn render_markdown_with(
        &self,
        provider: &str,
        source: String,
    ) -> Result<velk_markup::Document, String> {
        if velk_plugin_contracts::parse_plugin_id(provider).is_none() {
            return Err("invalid Markdown provider ID".into());
        }
        if source.len() > velk_plugin_api::MAX_INPUT_BYTES {
            return Err("Markdown input exceeds 1 MiB".into());
        }
        let (reply, receive) = async_channel::bounded(1);
        self.sender
            .try_send(Request::Render {
                provider: provider.into(),
                source,
                reply,
            })
            .map_err(|e| format!("plugin queue unavailable: {e}"))?;
        receive
            .recv()
            .await
            .map_err(|_| "plugin worker stopped".to_string())?
    }
    /// Bind a documentation renderer to a provider ID, following its current
    /// loaded generation on each call. Different consumers can select independently.
    pub fn markdown_renderer(&self, provider: impl Into<String>) -> MarkdownRenderer {
        MarkdownRenderer {
            client: self.clone(),
            provider: provider.into(),
        }
    }

    fn request(
        &self,
        operation: Operation,
    ) -> Result<async_channel::Receiver<Result<Report, String>>, String> {
        let (reply, receiver) = async_channel::bounded(1);
        self.sender
            .try_send(Request::Lifecycle { operation, reply })
            .map_err(|e| format!("plugin queue unavailable: {e}"))?;
        Ok(receiver)
    }
}
/// The owner joins the worker at shutdown. Client clones do not extend its life.
pub struct Runtime {
    client: Client,
    worker: Option<thread::JoinHandle<()>>,
}
impl Runtime {
    /// # Safety
    /// The directory must contain only trusted native libraries implementing the
    /// v1 ABI and its unload contract. Native code is not sandboxed.
    #[allow(unsafe_code)]
    pub unsafe fn start(directory: PathBuf) -> Result<Self, String> {
        Self::spawn(move || Manager::new(directory))
    }
    // Construction stays private; the public entry point establishes native-code
    // trust, and tests can supply a controlled in-process provider.
    #[allow(unsafe_code)]
    fn spawn(make_manager: impl FnOnce() -> Manager + Send + 'static) -> Result<Self, String> {
        let (sender, receive) = mpsc::sync_channel::<Queued>(16);
        let host = services::Bridge::new();
        *host.plugin_sender.lock().unwrap() = Some(Sender(sender.clone()));
        let worker_host = host.clone();
        let worker = thread::Builder::new()
            .name("velk-plugins".into())
            .spawn(move || {
                let mut manager = make_manager();
                *worker_host.plugin_thread.lock().unwrap() = Some(thread::current().id());
                manager.host = Some(worker_host);
                while let Ok(queued) = receive.recv() {
                    let _entered = queued.span.enter();
                    if let Some(start) = queued.queued {
                        tracing::debug!(
                            queue_wait_ms = start.elapsed().as_secs_f64() * 1000.,
                            "plugin request dequeued"
                        );
                    }
                    let request = queued.request;
                    let (operation, reply) = match request {
                        Request::Stop => break,
                        Request::DependencyCall { caller, generation, provider, hook, input, reply } => {
                            let result = manager.call_dependency(&caller, generation, &provider, &hook, &input);
                            let _ = reply.try_send(result);
                            continue;
                        }
                        Request::Settings(settings) => {
                            manager.set_settings(settings);
                            continue;
                        }
                        Request::Snapshot { ids, reply } => {
                            let _ = reply.try_send(manager.snapshot_plugins(&ids));
                            continue;
                        }
                        Request::Bind {
                            plugin,
                            name,
                            reply,
                        } => {
                            let _ = reply.try_send(manager.bind_hook(&plugin, &name));
                            manager.drive_semantics();
                            continue;
                        }
                        Request::Call {
                            handle,
                            input,
                            reply,
                        } => {
                            if !reply.is_closed() {
                                let _ = reply.try_send(manager.call_hook(&handle, &input));
                            }
                            manager.drive_semantics();
                            continue;
                        }
                        Request::Emit { name, input, reply } => {
                            let _ = reply.try_send(manager.emit_hook(&name, &input));
                            manager.drive_semantics();
                            continue;
                        }
                        Request::OpenSemantic { id, session } => {
                            manager.open_semantics(id, session);
                            manager.drive_semantics();
                            continue;
                        }
                        Request::WakeSemantic => {
                            manager.drive_semantics();
                            continue;
                        }
                        Request::Render { provider, source, reply } => {
                            if !reply.is_closed() {
                                let result = manager.render_markdown_with(&provider, &source);
                                let _ = reply.try_send(result);
                            }
                            manager.drive_semantics();
                            continue;
                        }
                        Request::Lifecycle { operation, reply } => (operation, reply),
                    };
                    let events = match operation {
                        // SAFETY: start's caller guarantees all artifacts obey v1.
                        Operation::Load(excluded) => unsafe { manager.load_except(&excluded) },
                        Operation::LoadSelected(ids, states) => {
                            unsafe { manager.load_selected(&ids) }.map(|mut events| {
                                events.extend(manager.restore_plugins(states));
                                events
                            })
                        }
                        Operation::Restart(id) => unsafe { manager.restart(id.as_deref()) },
                        Operation::UnloadAll => Ok(manager.unload_all()),
                        Operation::Unload(id) => Ok(manager.unload_with_dependents(&id)),
                    };
                    if let Ok(events) = &events {
                        for event in events {
                            match event {
                                Event::Loaded(info) => tracing::info!(plugin = %info.id, version = %info.version, generation = info.generation, "plugin loaded"),
                                Event::Removed(id) => tracing::info!(plugin = %id, "plugin unloaded"),
                                Event::Rejected { id, message } => tracing::warn!(plugin = %id, error = %message, "plugin rejected"),
                            }
                        }
                    }
                    manager.drive_semantics();
                    let result = events.map(|events| Report {
                        editing: manager.editing_providers(),
                        matching: manager.matching_providers(),
                        syntax: manager.syntax_providers(),
                        undo: manager.undo_providers(),
                        loaded: manager.plugins().count(),
                        plugins: manager.plugins().cloned().collect(),
                        events,
                        available: manager.available(),
                        inventory: manager.inventory(),
                    });
                    let _ = reply.try_send(result);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client: Client {
                host,
                sender: Sender(sender),
                next_session: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1)),
            },
            worker: Some(worker),
        })
    }
    pub fn client(&self) -> Client {
        self.client.clone()
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.client.host.close();
        let _ = self.client.sender.send(Request::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn default_directory() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("VELK_PLUGIN_DIR") {
        return Ok(path.into());
    }
    Ok(std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("executable has no directory")?
        .join("plugins"))
}
pub fn read_manifest(directory: &Path) -> Result<Manifest, String> {
    let path = directory.join(MANIFEST);
    if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 1024 * 1024 {
        return Err("manifest exceeds 1 MiB".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    manifest.validate()?;
    Ok(manifest)
}
/// Host-side documentation renderer bound to one provider ID.
#[derive(Clone)]
pub struct MarkdownRenderer {
    client: Client,
    provider: String,
}
impl velk_markup::Renderer for MarkdownRenderer {
    fn render(&self, source: String) -> velk_markup::RenderFuture {
        let renderer = self.clone();
        Box::pin(async move {
            renderer
                .client
                .render_markdown_with(&renderer.provider, source)
                .await
        })
    }
}

impl velk_markup::Renderer for Client {
    fn render(&self, source: String) -> velk_markup::RenderFuture {
        let client = self.clone();
        Box::pin(async move { client.render_markdown(source).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_manifest_defaults_to_standalone_and_shared_artifacts_must_agree() {
        let json = format!(
            r#"{{"plugins":[{{"id":"test.one","library":"plugin.dylib","digest":"{}"}}]}}"#,
            "0".repeat(64)
        );
        let mut manifest: Manifest = serde_json::from_str(&json).unwrap();
        assert!(!manifest.plugins[0].pack);
        manifest.plugins[0].pack = true;
        let mut second = manifest.plugins[0].clone();
        second.id = "test.two".into();
        manifest.plugins.push(second);
        assert!(manifest.validate().is_ok());
        manifest.plugins[1].pack = false;
        assert!(manifest.validate().is_err());
        manifest.plugins[1].pack = true;
        manifest.plugins[1].digest = "1".repeat(64);
        assert!(manifest.validate().is_err());
    }
    #[test]
    fn rejects_duplicate_ids_and_nonlocal_paths_before_loading() {
        let spec = PluginSpec {
            id: "velk.test".into(),
            dependencies: Vec::new(),
            install_reason: Default::default(),
            library: "../escape.dylib".into(),
            digest: "0".repeat(64),
            pack: false,
        };
        assert!(
            Manifest {
                plugins: vec![spec.clone()]
            }
            .validate()
            .is_err()
        );
        let spec = PluginSpec {
            library: "test/library.dylib".into(),
            ..spec
        };
        assert!(
            Manifest {
                plugins: vec![spec.clone(), spec]
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn async_render_cancellation_and_unload_obey_call_lifetimes() {
        use std::{
            sync::Arc,
            sync::atomic::{AtomicUsize, Ordering},
            task::{Context, Poll, Waker},
            time::{Duration, Instant},
        };
        for (cancel_active, provider) in [
            (false, DEFAULT_MARKDOWN_PROVIDER),
            (true, "community.markdown"),
        ] {
            let (entered, entry) = mpsc::sync_channel(1);
            let (release, resume) = mpsc::sync_channel(1);
            let calls = Arc::new(AtomicUsize::new(0));
            let drops = Arc::new(AtomicUsize::new(0));
            let control = native::test_provider::Control {
                entered,
                release: resume,
                calls: calls.clone(),
                drops: drops.clone(),
            };
            let runtime =
                Runtime::spawn(move || native::test_provider::manager_for(control, provider))
                    .unwrap();
            let client = runtime.client();
            use velk_markup::Renderer;
            let renderer = client.markdown_renderer(provider);
            let mut cx = Context::from_waker(Waker::noop());
            let mut active = renderer.render("active".into());
            assert!(active.as_mut().poll(&mut cx).is_pending());
            entry.recv_timeout(Duration::from_secs(2)).unwrap();
            let mut cancelled = renderer.render("cancelled".into());
            assert!(cancelled.as_mut().poll(&mut cx).is_pending());
            drop(cancelled);
            let unloaded = client.unload_all().unwrap();
            let mut after = renderer.render("after".into());
            assert!(after.as_mut().poll(&mut cx).is_pending());
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            assert!(
                unloaded.try_recv().is_err(),
                "unload must wait for the active native call"
            );
            let mut active = (!cancel_active).then_some(active);
            // then_some drops the active future in the cancelled case.
            release.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Ok(report) = unloaded.try_recv() {
                    assert_eq!(report.unwrap().loaded, 0);
                    break;
                }
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "cancelled queued work never called the provider"
            );
            if let Some(active) = &mut active {
                let Poll::Ready(Ok(document)) = active.as_mut().poll(&mut cx) else {
                    panic!("active result must be available after unload")
                };
                assert_eq!(document.text(), "active");
                assert!(
                    document.runs[0].style.bold,
                    "host-owned style survives unload"
                );
            }
            loop {
                if let Poll::Ready(result) = after.as_mut().poll(&mut cx) {
                    assert_eq!(result.unwrap(), velk_markup::Document::plain("after"));
                    break;
                }
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
        }
    }

    #[test]
    #[allow(unsafe_code)]
    fn lifecycle_worker_sleeps_until_command_and_stops_even_with_client_clones() {
        let directory = tempfile::tempdir().unwrap();
        // SAFETY: this test directory is empty and contains no native code.
        let runtime = unsafe { Runtime::start(directory.path().into()).unwrap() };
        let client = runtime.client();
        for result in [client.load().unwrap(), client.unload_all().unwrap()] {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                if let Ok(report) = result.try_recv() {
                    assert_eq!(report.unwrap().loaded, 0);
                    break;
                }
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        drop(runtime);
        assert!(client.load().is_err());
    }
}
