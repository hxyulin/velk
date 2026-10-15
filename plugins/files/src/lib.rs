//! Lazy filesystem panel. Native hook dispatch only queues/polls bounded work;
//! one owned worker handles directory I/O and joins before plugin unload.
mod model;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use velk_panels::{Reply, Request, Snapshot};
use velk_plugin_sdk::{Plugin, Registrar, hooks};
struct Job {
    id: u64,
    request: Request,
    cancel: Arc<AtomicBool>,
}
#[derive(Default)]
struct Queue {
    job: Option<Job>,
    result: Option<(u64, Result<Snapshot, String>)>,
    stop: bool,
}
pub struct Files {
    shared: Arc<(Mutex<Queue>, Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
    current: Option<(u64, Arc<AtomicBool>)>,
    next: u64,
}
impl Default for Files {
    fn default() -> Self {
        let shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let state = shared.clone();
        let worker = std::thread::Builder::new()
            .name("velk-files".into())
            .spawn(move || {
                let mut model = model::Model::default();
                loop {
                    let job = {
                        let mut queue = state.0.lock().unwrap();
                        while queue.job.is_none() && !queue.stop {
                            queue = state.1.wait(queue).unwrap();
                        }
                        if queue.stop {
                            break;
                        }
                        queue.job.take().unwrap()
                    };
                    let mut candidate = model.clone();
                    let result = candidate.apply(job.request, &job.cancel);
                    let mut queue = state.0.lock().unwrap();
                    if !queue.stop && !job.cancel.load(Ordering::Acquire) {
                        if result.is_ok() {
                            model = candidate;
                        }
                        queue.result = Some((job.id, result));
                    }
                }
            })
            .ok();
        Self {
            shared,
            worker,
            current: None,
            next: 0,
        }
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        if let Some((_, cancel)) = &self.current {
            cancel.store(true, Ordering::Release);
        }
        self.shared.0.lock().unwrap().stop = true;
        self.shared.1.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Plugin for Files {
    const ID: &'static str = "velk.files";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    fn describe(&mut self) -> String {
        "Lazy project file tree".into()
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.provide(
            hooks::Request(velk_panels::HOOK),
            |this: &mut Self, bytes| {
                let _trace = velk_plugin_sdk::diagnostics::scope!("files.protocol");
                velk_panels::encode(&this.request(velk_panels::decode(bytes)?)?)
            },
        )
    }
}
impl Files {
    fn request(&mut self, request: Request) -> Result<Reply, String> {
        match request {
            Request::Poll { id } => {
                if self
                    .current
                    .as_ref()
                    .is_none_or(|(current, _)| *current != id)
                {
                    return Err("file tree job retired".into());
                }
                let mut queue = self.shared.0.lock().unwrap();
                if queue.result.as_ref().is_some_and(|(job, _)| *job == id) {
                    let (_, result) = queue.result.take().unwrap();
                    return result.map(|snapshot| Reply::Ready { snapshot });
                }
                if self.worker.as_ref().is_none_or(|w| w.is_finished()) {
                    return Err("file tree worker unavailable".into());
                }
                Ok(Reply::Pending { id })
            }
            Request::Cancel { id } => {
                if self
                    .current
                    .as_ref()
                    .is_some_and(|(current, _)| *current == id)
                {
                    let (_, cancel) = self.current.take().unwrap();
                    cancel.store(true, Ordering::Release);
                    let mut queue = self.shared.0.lock().unwrap();
                    queue.job = None;
                    queue.result = None;
                }
                Ok(Reply::Cancelled)
            }
            request => {
                if self.worker.as_ref().is_none_or(|w| w.is_finished()) {
                    return Err("file tree worker unavailable".into());
                }
                if let Some((_, cancel)) = &self.current {
                    cancel.store(true, Ordering::Release);
                }
                self.next = self
                    .next
                    .checked_add(1)
                    .ok_or("file tree job counter exhausted")?;
                let id = self.next;
                let cancel = Arc::new(AtomicBool::new(false));
                let mut queue = self.shared.0.lock().unwrap();
                queue.result = None;
                queue.job = Some(Job {
                    id,
                    request,
                    cancel: cancel.clone(),
                });
                self.current = Some((id, cancel));
                self.shared.1.notify_one();
                Ok(Reply::Pending { id })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready(files: &mut Files, mut reply: Reply) -> Snapshot {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match reply {
                Reply::Ready { snapshot } => return snapshot,
                Reply::Pending { id } => {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    reply = files.request(Request::Poll { id }).unwrap();
                }
                Reply::Cancelled => panic!("unexpected cancellation"),
            }
        }
    }
    #[test]
    fn requests_are_async_cancellable_and_restart_with_a_fresh_worker() {
        let mut files = Files::default();
        let pending = files
            .request(Request::Describe {
                context: Default::default(),
            })
            .unwrap();
        let Reply::Pending { id } = pending else {
            panic!("dispatch must queue work")
        };
        assert!(matches!(
            files.request(Request::Cancel { id }).unwrap(),
            Reply::Cancelled
        ));
        assert!(files.request(Request::Poll { id }).is_err());
        let pending = files
            .request(Request::Describe {
                context: Default::default(),
            })
            .unwrap();
        let snapshot = ready(&mut files, pending);
        snapshot.validate().unwrap();
        assert!(snapshot.effects.is_empty());
        assert_eq!(snapshot.panels[0].title, "Files");
        drop(files); // Cancels/wakes/joins the worker, including a pending receive.
        let mut restarted = Files::default();
        let pending = restarted
            .request(Request::Describe {
                context: Default::default(),
            })
            .unwrap();
        assert_eq!(ready(&mut restarted, pending).panels[0].title, "Files");
    }
}
