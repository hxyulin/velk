mod scan;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicU64, Ordering},
};
use velk_plugin_sdk::{
    CapabilityDeclaration, Host, Plugin, Registrar,
    commands::{self, PickerCancel, PickerMode, PickerReply, PickerRequest},
    hooks,
};
#[derive(Clone, Copy)]
enum Mode {
    Files,
    Content,
}
struct Job {
    id: u64,
    ticket: u64,
    query: velk_search::Query,
}
struct State {
    stop: bool,
    host: Option<Host>,
    job: Option<Job>,
    active: Option<u64>,
    result: Option<(u64, Result<PickerReply, String>)>,
}
struct Worker {
    shared: Arc<(Mutex<State>, Condvar)>,
    ticket: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
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
        let state = shared.clone();
        let current = ticket.clone();
        let thread = std::thread::Builder::new()
            .name("velk-project-picker".into())
            .spawn(move || {
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
                            scan::run(&host, mode, &job.query, || {
                                current.load(Ordering::Acquire) != job.ticket
                            })
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
        }
    }
    fn request(&mut self, request: PickerRequest) -> Result<PickerReply, String> {
        if self
            .thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
        {
            return Err("project worker unavailable".into());
        }
        let mut state = self.shared.0.lock().unwrap();
        match request {
            PickerRequest::Begin { id, query, .. } => {
                if query.text.len() > 1024 {
                    return Err("query exceeds 1024 bytes".into());
                }
                let ticket = self.ticket.fetch_add(1, Ordering::AcqRel) + 1;
                state.result = None;
                state.active = Some(id);
                state.job = Some(Job { id, ticket, query });
                self.shared.1.notify_one();
                Ok(PickerReply::Pending)
            }
            PickerRequest::Poll { id } => {
                if state.active != Some(id) {
                    return Err("picker request cancelled or superseded".into());
                }
                if state
                    .result
                    .as_ref()
                    .is_some_and(|(result_id, _)| *result_id == id)
                {
                    return state.result.take().unwrap().1;
                }
                Ok(PickerReply::Pending)
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
        self.ticket.fetch_add(1, Ordering::AcqRel);
        self.shared.0.lock().unwrap().stop = true;
        self.shared.1.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub struct QuickOpen(Worker);
pub struct WorkspaceFind(Worker);
impl Default for QuickOpen {
    fn default() -> Self {
        Self(Worker::new(Mode::Files))
    }
}
impl Default for WorkspaceFind {
    fn default() -> Self {
        Self(Worker::new(Mode::Content))
    }
}
macro_rules! plugin {
    ($ty:ty,$id:literal,$name:literal,$hook:literal,$key:literal,$shift:literal,$mode:expr,$readme:literal) => {
        impl Plugin for $ty {
            const ID: &'static str = $id;
            const NAME: &'static str = $name;
            const VERSION: &'static str = env!("CARGO_PKG_VERSION");
            const README: &'static str = $readme;
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
                CapabilityDeclaration::required(
                    "editor.buffer.manage",
                    "Open the selected project result through the host.",
                ),
            ];
            fn describe(&mut self) -> String {
                $name.into()
            }
            fn attach_host(&mut self, host: Host) {
                self.0.host(host);
            }
            fn commands() -> Vec<commands::Declaration> {
                vec![commands::Declaration {
                    id: $id.into(),
                    title: $name.into(),
                    language: None,
                    input: None,
                    shortcut: Some(commands::Shortcut {
                        key: $key.into(),
                        primary: true,
                        shift: $shift,
                        ..Default::default()
                    }),
                    handler: commands::Handler::Picker {
                        hook: $hook.into(),
                        mode: $mode,
                    },
                }]
            }
            fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
                reg.provide(hooks::Request($hook), |this: &mut Self, bytes: &[u8]| {
                    let request = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                    serde_json::to_vec(&this.0.request(request)?).map_err(|e| e.to_string())
                })?;
                reg.on(
                    hooks::Event(commands::PICKER_CANCEL_HOOK),
                    |this: &mut Self, bytes: &[u8]| {
                        let cancel: PickerCancel =
                            serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
                        if cancel.plugin == Self::ID {
                            this.0.cancel(cancel.id);
                        }
                        Ok(())
                    },
                )
            }
        }
    };
}
plugin!(
    QuickOpen,
    "velk.quick-open",
    "Quick Open",
    "velk.quick-open.query",
    "p",
    false,
    PickerMode::Files,
    "# Quick Open\n\nFind project files with fuzzy path matching. Cmd/Ctrl+P opens the live picker. Respects project ignore files and skips symlinks. Select a result with Enter or click to open it."
);
plugin!(
    WorkspaceFind,
    "velk.workspace-find",
    "Find in Workspace",
    "velk.workspace-find.query",
    "f",
    true,
    PickerMode::Content,
    "# Find in Workspace\n\nCmd/Ctrl+Shift+F searches project text, preferring unsaved open buffers. Case and whole-word controls update the search. Results show file, line, and matching text. Enter or click navigates to a result. Scans are cancellable and explicitly bounded."
);
