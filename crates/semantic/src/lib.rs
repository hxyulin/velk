//! Semantic services independent of UI widgets, rendering, and wire protocols.
//! One service currently owns one open document. Backends run on a dedicated
//! thread/reactor or an external provider; queues are bounded and UI methods never wait.

use async_channel::{Receiver, Sender};
use ropey::Rope;
use std::{future::Future, io, ops::Range, path::PathBuf, pin::Pin, sync::Arc};

#[derive(Clone, Debug)]
pub struct DocumentSnapshot {
    pub path: PathBuf,
    pub language: String,
    pub revision: u64,
    pub text: Rope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionRequest {
    pub id: u64,
    pub revision: u64,
    /// Unicode scalar offset, independent of any backend's encoding.
    pub position: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub diagnostics: bool,
    pub hover: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Starting,
    Ready(Capabilities),
    Failed(String),
    Stopped,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    pub request: PositionRequest,
    pub range: Option<Range<usize>>,
    /// Plain text or raw Markdown for the presentation layer to render.
    pub contents: String,
    pub markdown: bool,
}
#[derive(Clone, Debug)]
pub enum Event {
    Command {
        request: PositionRequest,
        result: Result<velk_commands::Outcome, String>,
    },
    Status(Status),
    Diagnostics {
        revision: u64,
        diagnostics: Vec<Diagnostic>,
    },
    Hover {
        request: PositionRequest,
        hover: Option<Hover>,
    },
    RequestFailed {
        request: PositionRequest,
        message: String,
    },
}
#[derive(Clone, Debug)]
pub enum Input {
    Command {
        request: PositionRequest,
        payload: serde_json::Value,
    },
    Document(DocumentSnapshot),
    Hover(PositionRequest),
    CancelHover,
    Saved(DocumentSnapshot),
}

pub type BackendFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
/// Implementations own their analysis/communication, consume snapshots, and emit
/// neutral results. No LSP or UI types are required. Observe `Session::cancelled`
/// during initialization and long waits, and clean up resources before returning.
pub trait Backend: Send + 'static {
    fn run(self: Box<Self>, session: Session) -> BackendFuture;
}

pub struct Session {
    documents: Receiver<DocumentSnapshot>,
    hovers: Receiver<Option<PositionRequest>>,
    commands: Receiver<Input>,
    saves: Receiver<DocumentSnapshot>,
    wake: Receiver<()>,
    stop: Receiver<()>,
    events: EventSink,
}
impl Session {
    /// Whether the frontend has closed or requested shutdown.
    pub fn is_cancelled(&self) -> bool {
        self.stop.is_closed()
    }
    /// Nonblocking drain used by an externally scheduled provider. Source
    /// snapshots take priority over position-sensitive requests.
    pub fn try_next(&self) -> Option<Input> {
        if self.is_cancelled() {
            return None;
        }
        if let Ok(document) = self.documents.try_recv() {
            return Some(Input::Document(document));
        }
        if let Ok(saved) = self.saves.try_recv() {
            return Some(Input::Saved(saved));
        }
        if let Ok(command) = self.commands.try_recv() {
            return Some(command);
        }
        self.hovers
            .try_recv()
            .ok()
            .map(|hover| hover.map_or(Input::CancelHover, Input::Hover))
    }
    pub async fn next(&self) -> Option<Input> {
        loop {
            if self.stop.is_closed() {
                return None;
            }
            if let Ok(document) = self.documents.try_recv() {
                return Some(Input::Document(document));
            }
            if let Ok(saved) = self.saves.try_recv() {
                return Some(Input::Saved(saved));
            }
            if let Ok(command) = self.commands.try_recv() {
                return Some(command);
            }
            if let Ok(hover) = self.hovers.try_recv() {
                return Some(hover.map_or(Input::CancelHover, Input::Hover));
            }
            if self.wake.recv().await.is_err() {
                return None;
            }
        }
    }
    pub async fn cancelled(&self) {
        let _ = self.stop.recv().await;
    }
    /// Retire position-sensitive work when replacing a provider. Document/save
    /// snapshots remain available for reattachment; commands are never replayed.
    pub fn clear_requests(&self) {
        while self.commands.try_recv().is_ok() {}
        while self.hovers.try_recv().is_ok() {}
        self.events.clear();
    }
    pub fn events(&self) -> EventSink {
        self.events.clone()
    }
}

/// Separate latest-value result slots prevent a diagnostic flood from dropping
/// status or hover results. Sending never waits for a UI consumer.
#[derive(Clone)]
pub struct EventSink {
    command_busy: Arc<std::sync::atomic::AtomicBool>,
    observer: Option<Arc<dyn Fn(Event) + Send + Sync>>,
    status: Sender<Event>,
    status_pending: Receiver<Event>,
    diagnostics: Sender<Event>,
    diagnostics_pending: Receiver<Event>,
    hover: Sender<Event>,
    hover_pending: Receiver<Event>,
    command: Sender<Event>,
    command_pending: Receiver<Event>,
    wake: Sender<()>,
}
impl EventSink {
    /// Discard results from a retired provider before attaching a new generation.
    pub fn clear(&self) {
        self.command_busy
            .store(false, std::sync::atomic::Ordering::Release);
        while self.status_pending.try_recv().is_ok() {}
        while self.diagnostics_pending.try_recv().is_ok() {}
        while self.hover_pending.try_recv().is_ok() {}
        while self.command_pending.try_recv().is_ok() {}
    }
    pub fn close(&self) {
        self.wake.close();
    }
    pub fn publish(&self, event: Event) {
        if let Some(observer) = &self.observer {
            if matches!(event, Event::Command { .. }) {
                self.command_busy
                    .store(false, std::sync::atomic::Ordering::Release);
            }
            observer(event);
            return;
        }
        match event {
            Event::Command { .. } => latest(&self.command, &self.command_pending, event),
            Event::Status(_) => latest(&self.status, &self.status_pending, event),
            Event::Diagnostics { .. } => {
                latest(&self.diagnostics, &self.diagnostics_pending, event)
            }
            _ => latest(&self.hover, &self.hover_pending, event),
        }
        let _ = self.wake.try_send(());
    }
}
/// Single-consumer event stream. Clones share queues rather than broadcasting.
#[derive(Clone)]
pub struct Events {
    command_busy: Arc<std::sync::atomic::AtomicBool>,
    status: Receiver<Event>,
    diagnostics: Receiver<Event>,
    hover: Receiver<Event>,
    command: Receiver<Event>,
    wake: Receiver<()>,
}
impl Events {
    pub async fn recv(&self) -> Result<Event, async_channel::RecvError> {
        loop {
            if let Ok(event) = self.status.try_recv() {
                return Ok(event);
            }
            if let Ok(event) = self.command.try_recv() {
                self.command_busy
                    .store(false, std::sync::atomic::Ordering::Release);
                return Ok(event);
            }
            if let Ok(event) = self.diagnostics.try_recv() {
                return Ok(event);
            }
            if let Ok(event) = self.hover.try_recv() {
                return Ok(event);
            }
            self.wake.recv().await?;
        }
    }
}

pub struct Service {
    command_busy: Arc<std::sync::atomic::AtomicBool>,
    commands: Sender<Input>,
    documents: Sender<DocumentSnapshot>,
    documents_pending: Receiver<DocumentSnapshot>,
    hovers: Sender<Option<PositionRequest>>,
    hovers_pending: Receiver<Option<PositionRequest>>,
    saves: Sender<DocumentSnapshot>,
    saves_pending: Receiver<DocumentSnapshot>,
    wake: Sender<()>,
    stop: Sender<()>,
    pub events: Events,
    notify: Option<Arc<dyn Fn() + Send + Sync>>,
    worker: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl Service {
    /// Starting the backend, executable discovery, and process creation happen on
    /// the worker. The UI only creates queues and starts the named worker thread.
    pub fn start(backend: impl Backend) -> io::Result<Arc<Self>> {
        Self::start_boxed(Box::new(backend))
    }

    /// Object-safe entry point for selecting a provider at runtime.
    pub fn start_boxed(backend: Box<dyn Backend>) -> io::Result<Arc<Self>> {
        Self::start_observed_boxed(backend, None)
    }
    /// Execute a backend on its own reactor and synchronously forward owned events.
    /// Shutdown/join guarantees no observer calls remain in flight afterward.
    pub fn start_observed(
        backend: Box<dyn Backend>,
        observer: impl Fn(Event) + Send + Sync + 'static,
    ) -> io::Result<Arc<Self>> {
        Self::start_observed_boxed(backend, Some(Arc::new(observer)))
    }
    fn start_observed_boxed(
        backend: Box<dyn Backend>,
        observer: Option<Arc<dyn Fn(Event) + Send + Sync>>,
    ) -> io::Result<Arc<Self>> {
        let (service, session) = Self::channels(None, observer);
        let sink = session.events();
        let worker = std::thread::Builder::new()
            .name("velk-semantic".into())
            .spawn(move || {
                sink.publish(Event::Status(Status::Starting));
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| error.to_string())
                        .and_then(|runtime| runtime.block_on(backend.run(session)))
                }))
                .unwrap_or_else(|_| Err("semantic backend panicked".into()));
                sink.publish(Event::Status(match result {
                    Ok(()) => Status::Stopped,
                    Err(error) => Status::Failed(error),
                }));
                // Receiver clones do not retain the service or keep it alive.
                sink.wake.close();
            })?;
        *service.worker.lock().unwrap() = Some(worker);
        Ok(service)
    }
    /// Queue-only frontend. The owner drains Session after each notification;
    /// no extra thread/runtime is created for an external native provider.
    pub fn external(notify: impl Fn() + Send + Sync + 'static) -> (Arc<Self>, Session) {
        Self::channels(Some(Arc::new(notify)), None)
    }
    fn channels(
        notify: Option<Arc<dyn Fn() + Send + Sync>>,
        observer: Option<Arc<dyn Fn(Event) + Send + Sync>>,
    ) -> (Arc<Self>, Session) {
        let command_busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (commands, command_pending) = async_channel::bounded(1);
        let (command, command_results) = async_channel::bounded(1);
        let (documents, documents_pending) = async_channel::bounded(1);
        let (hovers, hovers_pending) = async_channel::bounded(1);
        let (saves, saves_pending) = async_channel::bounded(1);
        let (wake, input_wake) = async_channel::bounded(1);
        let (stop, stopped) = async_channel::bounded(1);
        let (status, status_pending) = async_channel::bounded(1);
        let (diagnostics, diagnostics_pending) = async_channel::bounded(1);
        let (hover, hover_pending) = async_channel::bounded(1);
        let (event_wake, event_pending) = async_channel::bounded(1);
        let events = Events {
            command_busy: command_busy.clone(),
            command: command_results.clone(),
            status: status_pending.clone(),
            diagnostics: diagnostics_pending.clone(),
            hover: hover_pending.clone(),
            wake: event_pending,
        };
        let sink = EventSink {
            command_busy: command_busy.clone(),
            command,
            command_pending: command_results,
            observer,
            status,
            status_pending,
            diagnostics,
            diagnostics_pending,
            hover,
            hover_pending,
            wake: event_wake,
        };
        let session = Session {
            commands: command_pending,
            documents: documents_pending.clone(),
            hovers: hovers_pending.clone(),
            saves: saves_pending.clone(),
            wake: input_wake,
            stop: stopped,
            events: sink.clone(),
        };
        let service = Arc::new(Self {
            command_busy,
            commands,
            documents,
            documents_pending,
            hovers,
            hovers_pending,
            saves,
            saves_pending,
            wake,
            stop,
            events,
            notify,
            worker: std::sync::Mutex::new(None),
        });
        (service, session)
    }
    /// Must run away from the UI. Cancels the backend and joins its reactor,
    /// including runtime teardown, before observer storage or code can be freed.
    pub fn shutdown_and_join(&self) -> Result<(), String> {
        self.shutdown();
        if let Some(worker) = self
            .worker
            .lock()
            .map_err(|_| "semantic join lock poisoned")?
            .take()
        {
            worker
                .join()
                .map_err(|_| "semantic worker panicked".to_string())?;
        }
        Ok(())
    }
    fn signal(&self) {
        let _ = self.wake.try_send(());
        if let Some(notify) = &self.notify {
            notify();
        }
    }
    pub fn update(&self, document: DocumentSnapshot) {
        latest(&self.documents, &self.documents_pending, document);
        self.signal();
    }
    /// One command may be outstanding until its result is consumed. Explicit
    /// actions are never silently replaced by hover traffic or later commands.
    pub fn command(
        &self,
        request: PositionRequest,
        payload: serde_json::Value,
    ) -> Result<(), String> {
        if serde_json::to_vec(&payload)
            .map_err(|e| e.to_string())?
            .len()
            > 16 * 1024
        {
            return Err("command payload exceeds 16 KiB".into());
        }
        if self
            .command_busy
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return Err("a command is already running".into());
        }
        if let Err(error) = self.commands.try_send(Input::Command { request, payload }) {
            self.command_busy
                .store(false, std::sync::atomic::Ordering::Release);
            return Err(format!("command queue unavailable: {error}"));
        }
        self.signal();
        Ok(())
    }
    pub fn hover(&self, request: PositionRequest) {
        latest(&self.hovers, &self.hovers_pending, Some(request));
        self.signal();
    }
    pub fn cancel_hover(&self) {
        latest(&self.hovers, &self.hovers_pending, None);
        self.signal();
    }
    pub fn saved(&self, document: DocumentSnapshot) {
        latest(&self.saves, &self.saves_pending, document);
        self.signal();
    }
    pub fn shutdown(&self) {
        self.stop.close();
        self.wake.close();
        if let Some(notify) = &self.notify {
            notify();
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn latest<T>(sender: &Sender<T>, pending: &Receiver<T>, value: T) {
    if let Err(async_channel::TrySendError::Full(value)) = sender.try_send(value) {
        let _ = pending.try_recv();
        let _ = sender.try_send(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observed_backend_join_waits_for_final_callbacks_and_reactor_exit() {
        use std::sync::{Mutex, mpsc};
        use std::time::Duration;
        struct Gated {
            entered: mpsc::SyncSender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Backend for Gated {
            fn run(self: Box<Self>, session: Session) -> BackendFuture {
                Box::pin(async move {
                    session.cancelled().await;
                    self.entered.send(()).unwrap();
                    self.release.recv().unwrap();
                    session.events().publish(Event::Diagnostics {
                        revision: 5,
                        diagnostics: vec![],
                    });
                    Ok(())
                })
            }
        }
        let (entered, entry) = mpsc::sync_channel(1);
        let (release, resume) = mpsc::sync_channel(1);
        let output = Arc::new(Mutex::new(Vec::new()));
        let observer = output.clone();
        let service = Service::start_observed(
            Box::new(Gated {
                entered,
                release: resume,
            }),
            move |e| observer.lock().unwrap().push(e),
        )
        .unwrap();
        service.shutdown();
        entry.recv_timeout(Duration::from_secs(2)).unwrap();
        let (joined, done) = mpsc::sync_channel(1);
        let owner = service.clone();
        let thread = std::thread::spawn(move || {
            owner.shutdown_and_join().unwrap();
            joined.send(()).unwrap();
        });
        assert!(
            done.try_recv().is_err(),
            "join cannot complete while the backend is running"
        );
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(2)).unwrap();
        thread.join().unwrap();
        let events = output.lock().unwrap();
        assert!(matches!(
            events.as_slice(),
            [
                Event::Status(Status::Starting),
                Event::Diagnostics { revision: 5, .. },
                Event::Status(Status::Stopped)
            ]
        ));
    }
    struct InProcess;
    impl Backend for InProcess {
        fn run(self: Box<Self>, session: Session) -> BackendFuture {
            Box::pin(async move {
                while let Some(input) = session.next().await {
                    if let Input::Document(document) = input {
                        session.events().publish(Event::Diagnostics {
                            revision: document.revision,
                            diagnostics: vec![],
                        });
                    }
                }
                Ok(())
            })
        }
    }
    #[test]
    fn non_lsp_backend_coalesces_snapshots_and_drop_does_not_retain_service() {
        let service = Service::start_boxed(Box::new(InProcess)).unwrap();
        let weak = Arc::downgrade(&service);
        for revision in 0..1000 {
            service.update(DocumentSnapshot {
                path: "/file".into(),
                language: "plain".into(),
                revision,
                text: Rope::from_str("snapshot"),
            });
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if matches!(
                        service.events.recv().await.unwrap(),
                        Event::Diagnostics { revision: 999, .. }
                    ) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
        });
        let events = service.events.clone();
        drop(service);
        assert!(weak.upgrade().is_none());
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if matches!(events.recv().await.unwrap(), Event::Status(Status::Stopped)) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
        });
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn commands_keep_their_result_until_consumed_and_do_not_share_hover_slots() {
        let (service, session) = Service::external(|| {});
        let request = PositionRequest {
            id: 1,
            revision: 0,
            position: 0,
        };
        service
            .command(request, serde_json::json!({"operation":"test"}))
            .unwrap();
        assert!(service.command(request, serde_json::Value::Null).is_err());
        assert!(matches!(session.try_next(), Some(Input::Command { .. })));
        session.events().publish(Event::Command {
            request,
            result: Ok(velk_commands::Outcome::Message {
                text: "kept".into(),
            }),
        });
        for _ in 0..100 {
            session.events().publish(Event::Hover {
                request,
                hover: None,
            });
            session.events().publish(Event::Diagnostics {
                revision: 0,
                diagnostics: vec![],
            });
        }
        assert!(service.command(request, serde_json::Value::Null).is_err());
        loop {
            if let Event::Command { result, .. } = service.events.recv().await.unwrap() {
                assert!(
                    matches!(result, Ok(velk_commands::Outcome::Message { text }) if text == "kept")
                );
                break;
            }
        }
        service.command(request, serde_json::Value::Null).unwrap();
        assert!(matches!(session.try_next(), Some(Input::Command { .. })));
        session.clear_requests();
        service.command(request, serde_json::Value::Null).unwrap();
        session.clear_requests();
        assert!(session.try_next().is_none());
        service.command(request, serde_json::Value::Null).unwrap();
    }
}
