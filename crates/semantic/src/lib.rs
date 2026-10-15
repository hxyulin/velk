//! Semantic services independent of UI widgets, rendering, and wire protocols.
//! One service currently owns one open document. Backends run on a dedicated
//! thread/reactor; input and result queues are bounded and UI methods never wait.
#[cfg(feature = "lsp")]
pub mod lsp;
#[cfg(any(feature = "lsp", test))]
mod text;

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
pub struct HoverRequest {
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
    pub request: HoverRequest,
    pub range: Option<Range<usize>>,
    /// Plain text or raw Markdown for the presentation layer to render.
    pub contents: String,
    pub markdown: bool,
}
#[derive(Clone, Debug)]
pub enum Event {
    Status(Status),
    Diagnostics {
        revision: u64,
        diagnostics: Vec<Diagnostic>,
    },
    Hover {
        request: HoverRequest,
        hover: Option<Hover>,
    },
    RequestFailed {
        request: HoverRequest,
        message: String,
    },
}
#[derive(Clone, Debug)]
pub enum Input {
    Document(DocumentSnapshot),
    Hover(HoverRequest),
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
    hovers: Receiver<Option<HoverRequest>>,
    saves: Receiver<DocumentSnapshot>,
    wake: Receiver<()>,
    stop: Receiver<()>,
    events: EventSink,
}
impl Session {
    /// Prioritize the latest source snapshot before position-sensitive requests.
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
    pub fn events(&self) -> EventSink {
        self.events.clone()
    }
}

/// Separate latest-value result slots prevent a diagnostic flood from dropping
/// status or hover results. Sending never waits for a UI consumer.
#[derive(Clone)]
pub struct EventSink {
    status: Sender<Event>,
    status_pending: Receiver<Event>,
    diagnostics: Sender<Event>,
    diagnostics_pending: Receiver<Event>,
    hover: Sender<Event>,
    hover_pending: Receiver<Event>,
    wake: Sender<()>,
}
impl EventSink {
    pub fn publish(&self, event: Event) {
        match event {
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
    status: Receiver<Event>,
    diagnostics: Receiver<Event>,
    hover: Receiver<Event>,
    wake: Receiver<()>,
}
impl Events {
    pub async fn recv(&self) -> Result<Event, async_channel::RecvError> {
        loop {
            if let Ok(event) = self.status.try_recv() {
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
    documents: Sender<DocumentSnapshot>,
    documents_pending: Receiver<DocumentSnapshot>,
    hovers: Sender<Option<HoverRequest>>,
    hovers_pending: Receiver<Option<HoverRequest>>,
    saves: Sender<DocumentSnapshot>,
    saves_pending: Receiver<DocumentSnapshot>,
    wake: Sender<()>,
    stop: Sender<()>,
    pub events: Events,
}
impl Service {
    /// Starting the backend, executable discovery, and process creation happen on
    /// the worker. The UI only creates queues and starts the named worker thread.
    pub fn start(backend: impl Backend) -> io::Result<Arc<Self>> {
        Self::start_boxed(Box::new(backend))
    }

    /// Object-safe entry point for selecting a provider at runtime.
    pub fn start_boxed(backend: Box<dyn Backend>) -> io::Result<Arc<Self>> {
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
            status: status_pending.clone(),
            diagnostics: diagnostics_pending.clone(),
            hover: hover_pending.clone(),
            wake: event_pending,
        };
        let sink = EventSink {
            status,
            status_pending,
            diagnostics,
            diagnostics_pending,
            hover,
            hover_pending,
            wake: event_wake,
        };
        let session = Session {
            documents: documents_pending.clone(),
            hovers: hovers_pending.clone(),
            saves: saves_pending.clone(),
            wake: input_wake,
            stop: stopped,
            events: sink.clone(),
        };
        std::thread::Builder::new()
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
        Ok(Arc::new(Self {
            documents,
            documents_pending,
            hovers,
            hovers_pending,
            saves,
            saves_pending,
            wake,
            stop,
            events,
        }))
    }
    pub fn update(&self, document: DocumentSnapshot) {
        latest(&self.documents, &self.documents_pending, document);
        let _ = self.wake.try_send(());
    }
    pub fn hover(&self, request: HoverRequest) {
        latest(&self.hovers, &self.hovers_pending, Some(request));
        let _ = self.wake.try_send(());
    }
    pub fn cancel_hover(&self) {
        latest(&self.hovers, &self.hovers_pending, None);
        let _ = self.wake.try_send(());
    }
    pub fn saved(&self, document: DocumentSnapshot) {
        latest(&self.saves, &self.saves_pending, document);
        let _ = self.wake.try_send(());
    }
    pub fn shutdown(&self) {
        self.stop.close();
        self.wake.close();
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
