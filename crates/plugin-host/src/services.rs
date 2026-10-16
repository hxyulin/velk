//! Requests cross to the editor without re-entering its UI or plugin worker.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use velk_plugin_contracts::host::{Request, Response};

pub struct HostRequest {
    pub plugin: String,
    pub generation: u64,
    pub request: Request,
    active: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    reply: mpsc::SyncSender<Result<Response, String>>,
}
impl HostRequest {
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire) && !self.closed.load(Ordering::Acquire)
    }
    pub fn respond(self, result: Result<Response, String>) {
        let _ = self.reply.try_send(result);
    }
}
type PublicationKey = (String, u64, u64, std::path::PathBuf);
struct Publication {
    active: Arc<AtomicBool>,
    value: Arc<velk_plugin_contracts::host::DiagnosticPublication>,
}
pub(crate) struct Bridge {
    diagnostics: std::sync::Mutex<std::collections::BTreeMap<PublicationKey, Publication>>,
    pub sender: async_channel::Sender<HostRequest>,
    pub receiver: async_channel::Receiver<HostRequest>,
    pub closed: Arc<AtomicBool>,
    pub workspace: std::sync::Mutex<Option<velk_plugin_contracts::host::Workspace>>,
}
impl Bridge {
    pub fn new() -> Arc<Self> {
        let (sender, receiver) = async_channel::bounded(64);
        Arc::new(Self {
            sender,
            receiver,
            closed: Arc::new(AtomicBool::new(false)),
            workspace: Default::default(),
            diagnostics: Default::default(),
        })
    }
    pub fn publish_diagnostics(
        &self,
        plugin: &str,
        generation: u64,
        client: u64,
        active: Arc<AtomicBool>,
        mut value: velk_plugin_contracts::host::DiagnosticPublication,
    ) -> Result<(), String> {
        let mut store = self.diagnostics.lock().unwrap();
        store.retain(|_, p| p.active.load(Ordering::Acquire));
        let key = (plugin.into(), generation, client, value.path.clone());
        if value.diagnostics.is_empty() {
            // Retain the empty publication so a reader cannot resurrect an older
            // document-local result while its semantic event is still queued.
            value.text.clear();
            value.revision = None;
            value.disk_matches = false;
            value.truncated = false;
        }
        let size = |publication: &velk_plugin_contracts::host::DiagnosticPublication| {
            publication.text.len()
                + publication
                    .diagnostics
                    .iter()
                    .map(|d| {
                        d.message.len()
                            + d.source.as_ref().map_or(0, String::len)
                            + d.code.as_ref().map_or(0, String::len)
                            + std::mem::size_of_val(d)
                    })
                    .sum::<usize>()
        };
        let bytes: usize = store
            .iter()
            .filter(|(k, _)| **k != key)
            .map(|(_, p)| size(&p.value))
            .sum();
        if store.len() >= 4096 && !store.contains_key(&key)
            || bytes + size(&value) > 64 * 1024 * 1024
        {
            return Err("workspace diagnostic storage limit reached".into());
        }
        store.insert(
            key,
            Publication {
                active,
                value: Arc::new(value),
            },
        );
        Ok(())
    }
    pub fn clear_diagnostics(&self, plugin: &str, generation: u64, client: u64) {
        self.diagnostics
            .lock()
            .unwrap()
            .retain(|(p, g, c, _), _| p != plugin || *g != generation || *c != client);
    }
    pub fn diagnostics(&self) -> Vec<Arc<velk_plugin_contracts::host::DiagnosticPublication>> {
        let mut store = self.diagnostics.lock().unwrap();
        store.retain(|_, p| p.active.load(Ordering::Acquire));
        store.values().map(|p| p.value.clone()).collect()
    }
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.sender.close();
    }
    pub fn request(
        &self,
        plugin: &str,
        generation: u64,
        active: Arc<AtomicBool>,
        request: Request,
    ) -> Result<Response, String> {
        let (reply, result) = mpsc::sync_channel(1);
        self.sender
            .try_send(HostRequest {
                plugin: plugin.into(),
                generation,
                request,
                active: active.clone(),
                closed: self.closed.clone(),
                reply,
            })
            .map_err(|_| "editor host request queue unavailable")?;
        loop {
            if self.closed.load(Ordering::Acquire) || !active.load(Ordering::Acquire) {
                return Err("host service retired".into());
            }
            match result.recv_timeout(std::time::Duration::from_millis(25)) {
                Ok(result) => {
                    if let Ok(Response::Workspace { workspace }) = &result {
                        *self.workspace.lock().unwrap() = Some(workspace.clone());
                    }
                    return result;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("editor dropped host request".into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn publication(message: &str) -> velk_plugin_contracts::host::DiagnosticPublication {
        use velk_plugin_contracts::host::*;
        DiagnosticPublication {
            path: "/project/file.rs".into(),
            revision: None,
            text: "bad".into(),
            disk_matches: true,
            truncated: false,
            diagnostics: vec![Diagnostic {
                start: 0,
                end: 3,
                line: 0,
                column: 0,
                severity: Severity::Error,
                message: message.into(),
                source: None,
                code: None,
            }],
        }
    }
    #[test]
    fn publications_replace_clear_and_retire_independently_of_buffers() {
        let bridge = Bridge::new();
        let active = Arc::new(AtomicBool::new(true));
        bridge
            .publish_diagnostics("provider", 1, 1, active.clone(), publication("first"))
            .unwrap();
        bridge
            .publish_diagnostics("provider", 1, 1, active.clone(), publication("replacement"))
            .unwrap();
        assert_eq!(bridge.diagnostics().len(), 1);
        assert_eq!(
            bridge.diagnostics()[0].diagnostics[0].message,
            "replacement"
        );
        bridge
            .publish_diagnostics(
                "provider",
                1,
                2,
                active.clone(),
                publication("other client"),
            )
            .unwrap();
        let mut empty = publication("");
        empty.diagnostics.clear();
        bridge
            .publish_diagnostics("provider", 1, 1, active.clone(), empty)
            .unwrap();
        assert_eq!(
            bridge
                .diagnostics()
                .iter()
                .filter(|p| !p.diagnostics.is_empty())
                .count(),
            1
        );
        bridge.clear_diagnostics("provider", 2, 2);
        assert_eq!(
            bridge
                .diagnostics()
                .iter()
                .filter(|p| !p.diagnostics.is_empty())
                .count(),
            1,
            "another generation cannot clear this client's results"
        );
        bridge.clear_diagnostics("provider", 1, 2);
        assert!(
            bridge
                .diagnostics()
                .iter()
                .all(|p| p.diagnostics.is_empty())
        );
        assert_eq!(
            bridge.diagnostics().len(),
            1,
            "empty publication suppresses older buffer-local results"
        );
        bridge.clear_diagnostics("provider", 1, 1);
        assert!(bridge.diagnostics().is_empty());
        bridge
            .publish_diagnostics("provider", 1, 1, active.clone(), publication("retired"))
            .unwrap();
        active.store(false, Ordering::Release);
        assert!(bridge.diagnostics().is_empty());
        assert!(
            bridge.receiver.is_empty(),
            "publications never block on the UI dispatcher"
        );
    }
    #[test]
    fn shutdown_and_retirement_release_waiters_and_invalidate_queued_requests() {
        for shutdown in [false, true] {
            let bridge = Bridge::new();
            let active = Arc::new(AtomicBool::new(true));
            let send = bridge.clone();
            let token = active.clone();
            let worker =
                std::thread::spawn(move || send.request("test.host", 7, token, Request::Buffers));
            let queued = bridge.receiver.recv_blocking().unwrap();
            assert!(queued.is_active());
            if shutdown {
                bridge.close();
            } else {
                active.store(false, Ordering::Release);
            }
            assert!(!queued.is_active());
            assert!(worker.join().unwrap().unwrap_err().contains("retired"));
        }
    }
    #[test]
    fn dropped_editor_request_returns_an_error_instead_of_hanging() {
        let bridge = Bridge::new();
        let send = bridge.clone();
        let worker = std::thread::spawn(move || {
            send.request(
                "test.host",
                1,
                Arc::new(AtomicBool::new(true)),
                Request::Buffers,
            )
        });
        drop(bridge.receiver.recv_blocking().unwrap());
        assert!(worker.join().unwrap().unwrap_err().contains("dropped"));
    }
}
