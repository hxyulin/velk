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
pub(crate) struct Bridge {
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
        })
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
