//! Provider-neutral syntax sessions, coalescing workers, and indexed highlight results.
pub use ropey::Rope;
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Language(String);
impl Language {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub use velk_core::{Change as Edit, TextPoint};

pub const HOOK: &str = "velk.syntax.session";
pub const CONTRACT: &str = "velk.contract.syntax-session";
pub const DEFAULT_PROVIDER: &str = "velk.tree-sitter";
pub const MAX_HIGHLIGHTS: usize = 65_536;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Highlight {
    /// Scalar offsets clipped to the requested viewport range.
    pub range: Range<usize>,
    /// Host style role, such as `keyword` or `function.builtin`.
    /// Unknown roles are left unstyled; renderer colors remain host-owned.
    pub capture: String,
}

/// Factories are cheap and reentrant. Opening and using a session happens only
/// on its document's background worker; no Rust trait object crosses the ABI.
pub trait Provider: std::fmt::Debug + Send + Sync + 'static {
    fn open(&self, language: Language) -> Result<Box<dyn Session>, String>;
}
/// One worker owns each session, including destruction. Offsets are Unicode
/// scalar positions; captures are ordered from general to more specific styles.
pub trait Session: Send {
    /// Return false on cancellation. Missing incremental history requires a fresh
    /// parse; only a successful update may become the source of new captures.
    fn update(
        &mut self,
        revision: u64,
        text: Rope,
        edits: &[Edit],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<bool, String>;
    /// Query the last successful update. Return None on cancellation, never a
    /// partial result. Later captures take precedence over earlier captures.
    fn highlights(
        &mut self,
        range: Range<usize>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Vec<Highlight>>, String>;
}
pub fn validate_highlights(
    highlights: &[Highlight],
    range: &Range<usize>,
    length: usize,
) -> Result<(), String> {
    if highlights.len() > MAX_HIGHLIGHTS
        || highlights.iter().any(|h| {
            h.range.start >= h.range.end
                || h.range.start < range.start
                || h.range.end > range.end
                || h.range.end > length
                || h.capture.is_empty()
                || h.capture.len() > 128
                || !h
                    .capture
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        })
    {
        return Err("invalid syntax highlights".into());
    }
    Ok(())
}
/// Immutable, indexed viewport captures. Contains no parser, query, or syntax tree.
/// Constructed entirely on the syntax worker; UI reads only intersecting captures.
pub struct HighlightSnapshot {
    pub revision: u64,
    pub viewport: u64,
    pub text: Rope,
    pub coverage: Vec<Range<usize>>,
    captures: Vec<(usize, Highlight)>,
    prefix_end: Vec<usize>,
}
impl HighlightSnapshot {
    pub fn new(
        revision: u64,
        viewport: u64,
        text: Rope,
        coverage: Vec<Range<usize>>,
        highlights: Vec<Highlight>,
    ) -> Arc<Self> {
        let mut captures: Vec<_> = highlights.into_iter().enumerate().collect();
        captures.sort_by_key(|(_, h)| h.range.start);
        let mut end = 0;
        let prefix_end = captures
            .iter()
            .map(|(_, h)| {
                end = end.max(h.range.end);
                end
            })
            .collect();
        Arc::new(Self {
            revision,
            viewport,
            text,
            coverage,
            captures,
            prefix_end,
        })
    }
    pub fn highlights(&self, range: Range<usize>) -> Vec<Highlight> {
        if range.is_empty() {
            return Vec::new();
        }
        let first = self.prefix_end.partition_point(|&end| end <= range.start);
        let last = self
            .captures
            .partition_point(|(_, h)| h.range.start < range.end);
        let mut result: Vec<_> = self.captures[first..last.max(first)]
            .iter()
            .filter(|(_, h)| h.range.end > range.start)
            .collect();
        result.sort_by_key(|(order, _)| *order);
        result
            .into_iter()
            .map(|(_, h)| Highlight {
                range: h.range.start.max(range.start)..h.range.end.min(range.end),
                capture: h.capture.clone(),
            })
            .collect()
    }
}
pub fn merge_ranges(mut ranges: Vec<Range<usize>>, length: usize) -> Vec<Range<usize>> {
    for range in &mut ranges {
        range.start = range.start.min(length);
        range.end = range.end.min(length).max(range.start);
    }
    ranges.retain(|r| !r.is_empty());
    ranges.sort_by_key(|r| r.start);
    let mut result: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(last) = result.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
        } else {
            result.push(range);
        }
    }
    result
}

struct Request {
    trace: tracing::Span,
    revision: u64,
    text: Rope,
    edits: Vec<Edit>,
    generation: u64,
}
struct ViewportRequest {
    revision: u64,
    ranges: Vec<Range<usize>>,
    generation: u64,
}
pub type Update = Result<Arc<HighlightSnapshot>, Arc<str>>;

/// Single UI producer, one persistent CPU worker. Document and viewport mailboxes
/// each retain only the latest request; neither submission waits for computation.
pub struct Worker {
    sender: async_channel::Sender<Request>,
    pending: async_channel::Receiver<Request>,
    viewport_sender: async_channel::Sender<ViewportRequest>,
    viewport_pending: async_channel::Receiver<ViewportRequest>,
    wake: async_channel::Sender<()>,
    pub updates: async_channel::Receiver<Update>,
    generation: Arc<AtomicU64>,
    viewport_generation: Arc<AtomicU64>,
    pub id: u64,
}
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn send_latest<T>(
    sender: &async_channel::Sender<T>,
    pending: &async_channel::Receiver<T>,
    value: T,
) {
    let _ = pending.try_recv();
    let _ = sender.try_send(value);
}
impl Worker {
    pub fn new(
        provider: Arc<dyn Provider>,
        language: Language,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::spawn(move || provider.open(language))
    }
    fn spawn(
        initialize: impl FnOnce() -> Result<Box<dyn Session>, String> + Send + 'static,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (sender, pending) = async_channel::bounded::<Request>(1);
        let (viewport_sender, viewport_pending) = async_channel::bounded::<ViewportRequest>(1);
        let (wake, wakes) = async_channel::bounded(1);
        let (results, updates) = async_channel::bounded(1);
        let requests = pending.clone();
        let viewports = viewport_pending.clone();
        let stale_results = updates.clone();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        let viewport_generation = Arc::new(AtomicU64::new(0));
        let current_viewport = viewport_generation.clone();
        std::thread::Builder::new()
            .name("velk-syntax".into())
            .spawn(move || {
                // Grammar/query compilation and destruction belong to this worker too.
                let mut engine = match initialize() {
                    Ok(engine) => engine,
                    Err(error) => {
                        send_latest(&results, &stale_results, Err(Arc::from(error.to_string())));
                        return;
                    }
                };
                let mut parsed: Option<(u64, Rope)> = None;
                let mut viewport: Option<ViewportRequest> = None;
                let mut parsed_generation = 0;
                let mut published = None;
                while wakes.recv_blocking().is_ok() {
                    if let Ok(request) = requests.try_recv() {
                        let _entered = request.trace.enter();
                        match engine.update(
                            request.revision,
                            request.text.clone(),
                            &request.edits,
                            &|| current.load(Ordering::Relaxed) != request.generation,
                        ) {
                            Ok(true) => {
                                parsed_generation = request.generation;
                                parsed = Some((request.revision, request.text));
                            }
                            Ok(false) => {}
                            Err(error) => {
                                parsed = None;
                                send_latest(&results, &stale_results, Err(Arc::from(error)));
                            }
                        }
                    }
                    if let Ok(request) = viewports.try_recv() {
                        viewport = Some(request);
                    }
                    if let (Some((revision, text)), Some(view)) = (&parsed, &viewport)
                        && *revision == view.revision
                        && published != Some((*revision, view.generation))
                    {
                        let cancelled = || {
                            current.load(Ordering::Relaxed) != parsed_generation
                                || current_viewport.load(Ordering::Relaxed) != view.generation
                        };
                        let coverage = merge_ranges(view.ranges.clone(), text.len_chars());
                        let result: Result<Option<Arc<HighlightSnapshot>>, String> = (|| {
                            let mut highlights = Vec::new();
                            for range in &coverage {
                                let Some(captures) =
                                    engine.highlights(range.clone(), &cancelled)?
                                else {
                                    return Ok(None);
                                };
                                validate_highlights(&captures, range, text.len_chars())?;
                                highlights.extend(captures);
                            }
                            if cancelled() {
                                return Ok(None);
                            }
                            Ok(Some(HighlightSnapshot::new(
                                *revision,
                                view.generation,
                                text.clone(),
                                coverage,
                                highlights,
                            )))
                        })(
                        );
                        match result {
                            Ok(Some(snapshot)) => {
                                published = Some((*revision, view.generation));
                                send_latest(&results, &stale_results, Ok(snapshot));
                            }
                            Ok(None) => {}
                            Err(error) => {
                                send_latest(&results, &stale_results, Err(Arc::from(error)));
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            sender,
            pending,
            viewport_sender,
            viewport_pending,
            wake,
            updates,
            generation,
            viewport_generation,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        })
    }
    pub fn request(&self, revision: u64, text: Rope, edits: Vec<Edit>) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        send_latest(
            &self.sender,
            &self.pending,
            Request {
                trace: tracing::debug_span!(
                    "syntax.request",
                    async_work = true,
                    revision,
                    generation
                ),
                revision,
                text,
                edits,
                generation,
            },
        );
        let _ = self.wake.try_send(());
    }
    pub fn request_viewport(&self, revision: u64, ranges: Vec<Range<usize>>) {
        let generation = self.viewport_generation.fetch_add(1, Ordering::Relaxed) + 1;
        send_latest(
            &self.viewport_sender,
            &self.viewport_pending,
            ViewportRequest {
                revision,
                ranges,
                generation,
            },
        );
        let _ = self.wake.try_send(());
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.viewport_generation.fetch_add(1, Ordering::Relaxed);
        self.wake.close();
        self.sender.close();
        self.viewport_sender.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct EmptySession;
    impl Session for EmptySession {
        fn update(
            &mut self,
            _: u64,
            _: Rope,
            _: &[Edit],
            _: &dyn Fn() -> bool,
        ) -> Result<bool, String> {
            Ok(true)
        }
        fn highlights(
            &mut self,
            _: Range<usize>,
            _: &dyn Fn() -> bool,
        ) -> Result<Option<Vec<Highlight>>, String> {
            Ok(Some(vec![]))
        }
    }
    #[test]
    fn initialization_errors_are_delivered_asynchronously() {
        let worker = Worker::spawn(|| Err("grammar unavailable".into())).unwrap();
        let Err(message) = worker.updates.recv_blocking().unwrap() else {
            panic!("expected initialization error");
        };
        assert_eq!(&*message, "grammar unavailable");
    }
    #[test]
    fn initialization_and_busy_worker_never_block_submissions_or_drop() {
        let caller = std::thread::current().id();
        let (started, on_worker) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let worker = Worker::spawn(move || {
            started.send(std::thread::current().id()).unwrap();
            blocked.recv().unwrap();
            Ok(Box::new(EmptySession))
        })
        .unwrap();
        assert_ne!(
            on_worker
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            caller
        );
        for revision in 0..1000 {
            worker.request(revision, Rope::from_str("fn main() {}"), vec![]);
            worker.request_viewport(revision, std::iter::once(0..12).collect());
        }
        drop(worker);
        release.send(()).unwrap();
    }
    #[test]
    fn rejects_out_of_viewport_reversed_and_oversized_captures() {
        for h in [
            Highlight {
                range: 0..2,
                capture: "keyword".into(),
            },
            Highlight {
                range: Range { start: 2, end: 1 },
                capture: "keyword".into(),
            },
            Highlight {
                range: 1..2,
                capture: "x".repeat(129),
            },
        ] {
            assert!(validate_highlights(&[h], &(1..3), 3).is_err());
        }
        assert!(
            validate_highlights(
                &[Highlight {
                    range: 1..3,
                    capture: "function.builtin".into()
                }],
                &(1..3),
                3
            )
            .is_ok()
        );
    }
}
