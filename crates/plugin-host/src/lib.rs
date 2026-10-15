//! Disk-only native plugin experiment. No dependency resolver or async plugin ABI.
use serde::{Deserialize, Serialize};
use std::{
    path::{Component, Path, PathBuf},
    sync::mpsc,
    thread,
};
mod native;
pub use native::{Manager, PluginInfo};

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
}
impl Manifest {
    pub fn validate(&self) -> Result<(), String> {
        let mut ids = std::collections::HashSet::new();
        for p in &self.plugins {
            if p.id.is_empty()
                || p.id.len() >= 64
                || !p
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
            {
                return Err(format!("invalid plugin ID: {}", p.id));
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
    pub events: Vec<Event>,
}
enum Operation {
    Load,
    UnloadAll,
    Stop,
}
struct Request {
    operation: Operation,
    reply: async_channel::Sender<Result<Report, String>>,
}
/// Safe command endpoint. Lifecycle work is serialized on a sleeping worker.
#[derive(Clone)]
pub struct Client {
    sender: mpsc::Sender<Request>,
}
impl Client {
    pub fn load(&self) -> Result<async_channel::Receiver<Result<Report, String>>, String> {
        self.request(Operation::Load)
    }
    pub fn unload_all(&self) -> Result<async_channel::Receiver<Result<Report, String>>, String> {
        self.request(Operation::UnloadAll)
    }
    fn request(
        &self,
        operation: Operation,
    ) -> Result<async_channel::Receiver<Result<Report, String>>, String> {
        let (reply, receiver) = async_channel::bounded(1);
        self.sender
            .send(Request { operation, reply })
            .map_err(|_| "plugin worker stopped".to_string())?;
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
        let (sender, receive) = mpsc::channel::<Request>();
        let worker = thread::Builder::new()
            .name("velk-plugins".into())
            .spawn(move || {
                let mut manager = Manager::new(directory);
                while let Ok(request) = receive.recv() {
                    let events = match request.operation {
                        // SAFETY: start's caller guarantees all artifacts obey v1.
                        Operation::Load => unsafe { manager.load() },
                        Operation::UnloadAll => Ok(manager.unload_all()),
                        Operation::Stop => break,
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
                    let result = events.map(|events| Report {
                        loaded: manager.plugins().count(),
                        events,
                    });
                    let _ = request.reply.try_send(result);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client: Client { sender },
            worker: Some(worker),
        })
    }
    pub fn client(&self) -> Client {
        self.client.clone()
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let (reply, _) = async_channel::bounded(1);
        let _ = self.client.sender.send(Request {
            operation: Operation::Stop,
            reply,
        });
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_duplicate_ids_and_nonlocal_paths_before_loading() {
        let spec = PluginSpec {
            id: "velk.test".into(),
            library: "../escape.dylib".into(),
            digest: "0".repeat(64),
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
