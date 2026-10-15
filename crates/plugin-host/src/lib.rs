//! Native plugins with host-scheduled async requests and scoped semantic sessions.
//! No Rust future crosses the ABI. Language definitions resolve exact transport
//! dependencies; general dependency graphs remain deferred.
use serde::{Deserialize, Serialize};
use std::{
    path::{Component, Path, PathBuf},
    sync::mpsc,
    thread,
};
mod native;
pub use native::{HookDescriptor, HookHandle, Manager, PluginInfo};

pub mod hook_kind {
    pub use velk_plugin_api::{CONTRIBUTION, EVENT, REQUEST, SERVICE};
}
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
}
impl Manifest {
    pub fn validate(&self) -> Result<(), String> {
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
        Ok(())
    }
}
#[derive(Debug)]
pub enum Event {
    Loaded(PluginInfo),
    Removed(String),
    Rejected { id: String, message: String },
}
#[derive(Debug)]
pub struct Report {
    pub loaded: usize,
    /// Owned snapshot after this operation, including unchanged active plugins.
    pub plugins: Vec<PluginInfo>,
    pub events: Vec<Event>,
}
enum Operation {
    Load,
    UnloadAll,
    Unload(String),
}
enum Request {
    Lifecycle {
        operation: Operation,
        reply: async_channel::Sender<Result<Report, String>>,
    },
    Render {
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
    Stop,
}
/// Safe command endpoint. Lifecycle work is serialized on a sleeping worker.
#[derive(Clone)]
pub struct Client {
    sender: mpsc::SyncSender<Request>,
    next_session: std::sync::Arc<std::sync::atomic::AtomicU64>,
}
impl Client {
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
        self.request(Operation::Load)
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
    /// The returned future is local Rust scheduling, not a future across the ABI.
    /// Dropping it closes the result channel, allowing queued work to be skipped.
    pub async fn render_markdown(&self, source: String) -> Result<velk_markup::Document, String> {
        if source.len() > velk_plugin_api::MAX_INPUT_BYTES {
            return Err("Markdown input exceeds 1 MiB".into());
        }
        let (reply, receive) = async_channel::bounded(1);
        self.sender
            .try_send(Request::Render { source, reply })
            .map_err(|e| format!("plugin queue unavailable: {e}"))?;
        receive
            .recv()
            .await
            .map_err(|_| "plugin worker stopped".to_string())?
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
        let (sender, receive) = mpsc::sync_channel::<Request>(16);
        let worker = thread::Builder::new()
            .name("velk-plugins".into())
            .spawn(move || {
                let mut manager = make_manager();
                while let Ok(request) = receive.recv() {
                    let (operation, reply) = match request {
                        Request::Stop => break,
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
                        Request::Render { source, reply } => {
                            if !reply.is_closed() {
                                let result = manager.render_markdown(&source);
                                let _ = reply.try_send(result);
                            }
                            manager.drive_semantics();
                            continue;
                        }
                        Request::Lifecycle { operation, reply } => (operation, reply),
                    };
                    let events = match operation {
                        // SAFETY: start's caller guarantees all artifacts obey v1.
                        Operation::Load => unsafe { manager.load() },
                        Operation::UnloadAll => Ok(manager.unload_all()),
                        Operation::Unload(id) => Ok(manager.unload(&id).into_iter().collect()),
                    };
                    if let Ok(events) = &events {
                        for event in events {
                            match event {
                                Event::Loaded(info) => eprintln!(
                                    "velk: loaded plugin {} {} generation {}: {}",
                                    info.id, info.version, info.generation, info.description
                                ),
                                Event::Removed(id) => eprintln!("velk: unloaded plugin {id}"),
                                Event::Rejected { id, message } => {
                                    eprintln!("velk: plugin {id} rejected: {message}")
                                }
                            }
                        }
                    }
                    manager.drive_semantics();
                    let result = events.map(|events| Report {
                        loaded: manager.plugins().count(),
                        plugins: manager.plugins().cloned().collect(),
                        events,
                    });
                    let _ = reply.try_send(result);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client: Client {
                sender,
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
            future::Future,
            sync::Arc,
            sync::atomic::{AtomicUsize, Ordering},
            task::{Context, Poll, Waker},
            time::{Duration, Instant},
        };
        for cancel_active in [false, true] {
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
            let runtime = Runtime::spawn(move || native::test_provider::manager(control)).unwrap();
            let client = runtime.client();
            let mut cx = Context::from_waker(Waker::noop());
            let mut active = Box::pin(client.render_markdown("active".into()));
            assert!(active.as_mut().poll(&mut cx).is_pending());
            entry.recv_timeout(Duration::from_secs(2)).unwrap();
            let mut cancelled = Box::pin(client.render_markdown("cancelled".into()));
            assert!(cancelled.as_mut().poll(&mut cx).is_pending());
            drop(cancelled);
            let unloaded = client.unload_all().unwrap();
            let mut after = Box::pin(client.render_markdown("after".into()));
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
