use velk_project_contracts::{Reply as QueryReply, Request as QueryRequest};
mod index;
mod scan;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use velk_plugin_sdk::{CapabilityDeclaration, Host, Plugin, Registrar, hooks};
#[derive(Clone, Copy)]
enum Mode {
    Files,
    Content,
}
struct Job {
    id: u64,
    ticket: u64,
    session: u64,
    query: velk_search::Query,
}
struct State {
    stop: bool,
    host: Option<Host>,
    job: Option<Job>,
    active: Option<u64>,
    result: Option<(u64, Result<QueryReply, String>)>,
}
struct Worker {
    shared: Arc<(Mutex<State>, Condvar)>,
    ticket: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
    stopping: Arc<AtomicBool>,
    session: Arc<AtomicU64>,
}
impl Worker {
    fn new(mode: Mode) -> Self {
        let shared = Arc::new((
            Mutex::new(State {
                stop: false,
                host: None,
                job: None,
                active: None,
                result: None,
            }),
            Condvar::new(),
        ));
        let ticket = Arc::new(AtomicU64::new(0));
        let stopping = Arc::new(AtomicBool::new(false));
        let session = Arc::new(AtomicU64::new(0));
        let stopped = stopping.clone();
        let current_session = session.clone();
        let state = shared.clone();
        let current = ticket.clone();
        let thread = std::thread::Builder::new()
            .name("velk-project-query".into())
            .spawn(move || {
                let mut index = None::<(u64, index::Index)>;
                loop {
                    let (job, host) = {
                        let mut queue = state.0.lock().unwrap();
                        while !queue.stop && queue.job.is_none() {
                            queue = state.1.wait(queue).unwrap();
                        }
                        if queue.stop {
                            break;
                        }
                        (queue.job.take().unwrap(), queue.host.clone())
                    };
                    let result = host
                        .ok_or_else(|| "host services not attached".to_string())
                        .and_then(|host| {
                            let cancelled = || current.load(Ordering::Acquire) != job.ticket;
                            if matches!(mode, Mode::Files) {
                                if index
                                    .as_ref()
                                    .is_none_or(|(session, _)| *session != job.session)
                                {
                                    let built = index::Index::build(&host, || {
                                        stopped.load(Ordering::Acquire)
                                            || current_session.load(Ordering::Acquire)
                                                != job.session
                                    })?;
                                    index = Some((job.session, built));
                                }
                                index.as_mut().unwrap().1.filter(&job.query.text, cancelled)
                            } else {
                                scan::run(&host, mode, &job.query, cancelled)
                            }
                        });
                    let mut state = state.0.lock().unwrap();
                    if !state.stop && current.load(Ordering::Acquire) == job.ticket {
                        state.result = Some((job.id, result));
                    }
                }
            })
            .ok();
        Self {
            shared,
            ticket,
            thread,
            stopping,
            session,
        }
    }
    fn request(&mut self, request: QueryRequest) -> Result<QueryReply, String> {
        if self
            .thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
        {
            return Err("project worker unavailable".into());
        }
        let mut state = self.shared.0.lock().unwrap();
        match request {
            QueryRequest::Begin {
                id, session, query, ..
            } => {
                if query.text.len() > 1024 {
                    return Err("query exceeds 1024 bytes".into());
                }
                let ticket = self.ticket.fetch_add(1, Ordering::AcqRel) + 1;
                self.session.store(session, Ordering::Release);
                state.result = None;
                state.active = Some(id);
                state.job = Some(Job {
                    id,
                    ticket,
                    session,
                    query,
                });
                self.shared.1.notify_one();
                Ok(QueryReply::Pending)
            }
            QueryRequest::Cancel { id } => {
                drop(state);
                self.cancel(id);
                Ok(QueryReply::Cancelled)
            }
            QueryRequest::Poll { id } => {
                if state.active != Some(id) {
                    return Err("project query cancelled or superseded".into());
                }
                if state
                    .result
                    .as_ref()
                    .is_some_and(|(result_id, _)| *result_id == id)
                {
                    return state.result.take().unwrap().1;
                }
                Ok(QueryReply::Pending)
            }
        }
    }
    fn cancel(&mut self, id: u64) {
        let mut state = self.shared.0.lock().unwrap();
        if state.active == Some(id) {
            self.ticket.fetch_add(1, Ordering::AcqRel);
            state.active = None;
            state.job = None;
            state.result = None;
        }
    }
    fn host(&mut self, host: Host) {
        self.shared.0.lock().unwrap().host = Some(host);
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.ticket.fetch_add(1, Ordering::AcqRel);
        self.shared.0.lock().unwrap().stop = true;
        self.shared.1.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
pub struct FileIndex(
    velk_plugin_sdk::backend::Sessions<FileSession>,
    Option<Host>,
);
#[derive(Default)]
pub struct WorkspaceSearch(
    velk_plugin_sdk::backend::Sessions<SearchSession>,
    Option<Host>,
);
struct FileSession(Worker);
struct SearchSession(Worker);
impl Default for FileSession {
    fn default() -> Self {
        Self(Worker::new(Mode::Files))
    }
}
impl Default for SearchSession {
    fn default() -> Self {
        Self(Worker::new(Mode::Content))
    }
}
macro_rules! backend {
    ($ty:ty, $id:literal, $name:literal) => {
        impl Plugin for $ty {
            const ID: &'static str = $id;
            const NAME: &'static str = $name;
            const README: &'static str = concat!("# ", $name, "\n\nProvides project query data to frontend plugins without prescribing a panel or picker. Each consuming plugin generation has independent queries, cancellation, and caches.");
            const VERSION: &'static str = env!("CARGO_PKG_VERSION");
            const CAPABILITIES: &'static [CapabilityDeclaration] = &[
                CapabilityDeclaration::required(
                    "workspace.read",
                    "Locate the selected project folder.",
                ),
                CapabilityDeclaration::required(
                    "filesystem.project.read",
                    "Read project listings, ignore rules, and text files.",
                ),
                CapabilityDeclaration::required(
                    "editor.buffer.read",
                    "Prefer unsaved buffer contents when searching.",
                ),
            ];
            fn describe(&mut self) -> String {
                $name.into()
            }
            fn attach_host(&mut self, host: Host) {
                self.1 = Some(host);
            }
            fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
                reg.provide(
                    hooks::Request(velk_project_contracts::HOOK),
                    |this: &mut Self, bytes: &[u8]| {
                        let host = this.1.clone().ok_or("host services not attached")?;
                        this.0.request(bytes, |session, bytes| {
                            session.0.host(host);
                            let request =
                                serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                            serde_json::to_vec(&session.0.request(request)?)
                                .map_err(|e| e.to_string())
                        })
                    },
                )?;
                reg.on(
                    hooks::Event(velk_plugin_sdk::contracts::backend::RELEASE_HOOK),
                    |this: &mut Self, bytes: &[u8]| this.0.release(bytes),
                )
            }
        }
    };
}
backend!(FileIndex, "velk.project.files", "Project file index");
backend!(
    WorkspaceSearch,
    "velk.project.search",
    "Project text search"
);
mod frontend;
pub use frontend::{QuickOpen, WorkspaceFind};
