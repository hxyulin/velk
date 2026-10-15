//! Lazy filesystem panel. Native hook dispatch only queues/polls bounded work;
//! one owned worker handles directory I/O and joins before plugin unload.
mod model;
mod watching;
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
    signal: Arc<Mutex<watching::Signal>>,
}
impl Default for Files {
    fn default() -> Self {
        let shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let state = shared.clone();
        let signal = Arc::new(Mutex::new(watching::Signal::default()));
        let events = signal.clone();
        let worker = std::thread::Builder::new()
            .name("velk-files".into())
            .spawn(move || {
                let mut model = model::Model::default();
                let mut watches = watching::Watches::new(events);
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
                    let mut result =
                        candidate.apply_watched(job.request, &job.cancel, &mut |path| {
                            watches.directory(path)
                        });
                    let accept = {
                        let queue = state.0.lock().unwrap();
                        !queue.stop && !job.cancel.load(Ordering::Acquire)
                    };
                    if accept && result.is_ok() {
                        model = candidate;
                    }
                    // Reconcile even cancelled jobs, which may have installed
                    // watches before reading. Never hold the dispatch mutex here.
                    watches.reconcile(model.watched_paths(), model.root_path());
                    if let Ok(snapshot) = &mut result {
                        watches.decorate(snapshot);
                    }
                    let mut queue = state.0.lock().unwrap();
                    if accept && !queue.stop && !job.cancel.load(Ordering::Acquire) {
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
            signal,
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
            Request::Changes => Ok(if self.signal.lock().unwrap().changed() {
                Reply::Invalidated
            } else {
                Reply::Unchanged
            }),
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
                Reply::Invalidated | Reply::Unchanged => panic!("unexpected change reply"),
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
    fn describe(files: &mut Files, path: &std::path::Path) -> Snapshot {
        let pending = files
            .request(Request::Describe {
                context: velk_panels::Context {
                    root: Some(velk_panels::NativePath::new(path)),
                    ..Default::default()
                },
            })
            .unwrap();
        ready(files, pending)
    }
    fn changed(files: &mut Files) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if matches!(files.request(Request::Changes).unwrap(), Reply::Invalidated) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native watcher missed filesystem change"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    fn names(snapshot: &Snapshot) -> Vec<&str> {
        let velk_panels::Content::Tree(rows) = &snapshot.panels[0].content else {
            panic!("expected tree")
        };
        rows.iter().map(|n| n.label.as_str()).collect()
    }
    #[test]
    fn native_watcher_detects_create_rename_delete_and_folder_switch() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        let nested = root.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let mut files = Files::default();
        let initial = describe(&mut files, &nested);
        assert_eq!(
            names(&initial),
            vec!["nested"],
            "explicit folder overrides parent Git root"
        );
        assert!(initial.watch);
        std::fs::write(nested.join("created.txt"), "first").unwrap();
        changed(&mut files);
        let created = describe(&mut files, &nested);
        assert!(names(&created).contains(&"created.txt"));
        assert!(created.effects.is_empty());
        std::fs::rename(nested.join("created.txt"), nested.join("renamed.txt")).unwrap();
        changed(&mut files);
        let renamed = describe(&mut files, &nested);
        assert!(!names(&renamed).contains(&"created.txt"));
        assert!(names(&renamed).contains(&"renamed.txt"));
        std::fs::remove_file(nested.join("renamed.txt")).unwrap();
        changed(&mut files);
        assert!(!names(&describe(&mut files, &nested)).contains(&"renamed.txt"));
        let other = root.join("other");
        std::fs::create_dir(&other).unwrap();
        assert_eq!(names(&describe(&mut files, &other)), vec!["other"]);
        std::fs::write(other.join("new-root.txt"), "").unwrap();
        changed(&mut files);
        assert!(names(&describe(&mut files, &other)).contains(&"new-root.txt"));
        std::fs::remove_file(other.join("new-root.txt")).unwrap();
        std::fs::remove_dir(&other).unwrap();
        changed(&mut files);
        let absent = describe(&mut files, &other);
        assert!(
            names(&absent)
                .iter()
                .any(|name| name.contains("Cannot read directory"))
        );
        std::fs::create_dir(&other).unwrap();
        changed(&mut files);
        describe(&mut files, &other);
        std::fs::write(other.join("reattached.txt"), "").unwrap();
        changed(&mut files);
        assert!(names(&describe(&mut files, &other)).contains(&"reattached.txt"));
        drop(files); // Watcher threads are stopped along with the owned worker.
        std::fs::write(other.join("after-unload.txt"), "").unwrap();
        let mut restarted = Files::default();
        assert!(names(&describe(&mut restarted, &other)).contains(&"after-unload.txt"));
    }
}
