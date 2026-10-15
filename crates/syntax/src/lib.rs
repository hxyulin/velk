//! In-process, Rope-backed Rust parsing. One worker coalesces queued revisions.
use ropey::Rope;
use std::{
    ops::{ControlFlow, Range},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tree_sitter::{
    InputEdit, Node, ParseOptions, Parser, Point, Query, QueryCursor, QueryCursorOptions,
    StreamingIterator, TextProvider, Tree,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
}
impl Language {
    pub fn for_path(path: &std::path::Path) -> Option<Self> {
        (path.extension()?.eq_ignore_ascii_case("rs")).then_some(Self::Rust)
    }
}

/// Coordinates refer to the document immediately before and after an edit.
#[derive(Clone, Debug)]
pub struct Edit {
    pub revision: u64,
    pub input: InputEdit,
    pub replaced: Range<usize>,
    pub inserted: usize,
}
impl Edit {
    pub fn between(
        revision: u64,
        before: &Rope,
        after: &Rope,
        range: Range<usize>,
        inserted: usize,
    ) -> Self {
        let point = |rope: &Rope, c| {
            let row = rope.char_to_line(c);
            Point::new(row, rope.char_to_byte(c) - rope.line_to_byte(row))
        };
        Self {
            revision,
            input: InputEdit {
                start_byte: before.char_to_byte(range.start),
                old_end_byte: before.char_to_byte(range.end),
                new_end_byte: after.char_to_byte(range.start + inserted),
                start_position: point(before, range.start),
                old_end_position: point(before, range.end),
                new_end_position: point(after, range.start + inserted),
            },
            replaced: range,
            inserted,
        }
    }
    pub fn inverse(&self, revision: u64) -> Self {
        Self {
            revision,
            input: InputEdit {
                old_end_byte: self.input.new_end_byte,
                new_end_byte: self.input.old_end_byte,
                old_end_position: self.input.new_end_position,
                new_end_position: self.input.old_end_position,
                ..self.input
            },
            replaced: self.replaced.start..self.replaced.start + self.inserted,
            inserted: self.replaced.len(),
        }
    }
}

pub struct Snapshot {
    pub revision: u64,
    pub text: Rope,
    tree: Tree,
    query: Arc<Query>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Highlight {
    pub range: Range<usize>,
    pub capture: String,
}
struct RopeText<'a>(&'a Rope);
impl<'a> TextProvider<&'a [u8]> for RopeText<'a> {
    type I = std::iter::Map<ropey::iter::Chunks<'a>, fn(&'a str) -> &'a [u8]>;
    fn text(&mut self, node: Node) -> Self::I {
        self.0
            .byte_slice(node.byte_range())
            .chunks()
            .map(str::as_bytes)
    }
}
impl Snapshot {
    /// Query only the requested character window; text predicates borrow Rope chunks.
    pub fn highlights(&self, range: Range<usize>) -> Vec<Highlight> {
        self.highlights_cancellable(range, || false).unwrap()
    }
    fn highlights_cancellable(
        &self,
        range: Range<usize>,
        cancelled: impl Fn() -> bool,
    ) -> Option<Vec<Highlight>> {
        if cancelled() {
            return None;
        }
        if range.is_empty() {
            return Some(Vec::new());
        }
        let mut cursor = QueryCursor::new();
        cursor
            .set_byte_range(self.text.char_to_byte(range.start)..self.text.char_to_byte(range.end));
        let mut progress = |_: &tree_sitter::QueryCursorState| {
            if cancelled() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut captures = cursor.captures_with_options(
            &self.query,
            self.tree.root_node(),
            RopeText(&self.text),
            QueryCursorOptions::new().progress_callback(&mut progress),
        );
        let mut result = Vec::new();
        while let Some((m, index)) = captures.next() {
            if cancelled() {
                return None;
            }
            let capture = m.captures()[*index];
            let start = self
                .text
                .byte_to_char(capture.node.start_byte())
                .max(range.start);
            let end = self
                .text
                .byte_to_char(capture.node.end_byte())
                .min(range.end);
            if start < end {
                result.push((
                    m.pattern_index,
                    Highlight {
                        range: start..end,
                        capture: self.query.capture_names()[capture.index as usize].to_owned(),
                    },
                ));
            }
        }
        // Later query patterns specialize earlier generic identifier captures.
        result.sort_by_key(|(priority, _)| *priority);
        if cancelled() {
            None
        } else {
            Some(result.into_iter().map(|(_, h)| h).collect())
        }
    }
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
    pub fn prepare(
        snapshot: &Snapshot,
        viewport: u64,
        ranges: Vec<Range<usize>>,
        cancelled: impl Fn() -> bool,
    ) -> Option<Arc<Self>> {
        let coverage = merge_ranges(ranges, snapshot.text.len_chars());
        let mut captures = Vec::new();
        for range in &coverage {
            for highlight in snapshot.highlights_cancellable(range.clone(), &cancelled)? {
                captures.push((captures.len(), highlight));
            }
        }
        if cancelled() {
            return None;
        }
        captures.sort_by_key(|(_, h)| h.range.start);
        let mut end = 0;
        let prefix_end = captures
            .iter()
            .map(|(_, h)| {
                end = end.max(h.range.end);
                end
            })
            .collect();
        Some(Arc::new(Self {
            revision: snapshot.revision,
            viewport,
            text: snapshot.text.clone(),
            coverage,
            captures,
            prefix_end,
        }))
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
fn merge_ranges(mut ranges: Vec<Range<usize>>, length: usize) -> Vec<Range<usize>> {
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

pub struct Engine {
    parser: Parser,
    query: Arc<Query>,
    snapshot: Option<Arc<Snapshot>>,
}
impl Engine {
    pub fn new(language: Language) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let language = match language {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        };
        let mut parser = Parser::new();
        parser.set_language(&language)?;
        Ok(Self {
            parser,
            query: Arc::new(Query::new(&language, tree_sitter_rust::HIGHLIGHTS_QUERY)?),
            snapshot: None,
        })
    }
    pub fn update(
        &mut self,
        revision: u64,
        text: Rope,
        edits: &[Edit],
        cancelled: impl Fn() -> bool,
    ) -> Option<Arc<Snapshot>> {
        if cancelled() {
            return None;
        }
        let mut old = self.snapshot.as_ref().map(|s| s.tree.clone());
        if let Some(previous) = &self.snapshot {
            let changes: Vec<_> = edits
                .iter()
                .filter(|e| e.revision > previous.revision && e.revision <= revision)
                .collect();
            if previous.revision + changes.len() as u64 != revision
                || changes
                    .iter()
                    .enumerate()
                    .any(|(i, e)| e.revision != previous.revision + i as u64 + 1)
            {
                old = None;
            } else if let Some(tree) = &mut old {
                for change in changes {
                    tree.edit(&change.input);
                }
            }
        }
        let mut progress = |_: &tree_sitter::ParseState| {
            if cancelled() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let tree = self.parser.parse_with_options(
            &mut |offset, _| {
                if offset >= text.len_bytes() {
                    return &[][..];
                }
                let (chunk, start, _, _) = text.chunk_at_byte(offset);
                &chunk.as_bytes()[offset - start..]
            },
            old.as_ref(),
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        if cancelled() || tree.is_none() {
            self.parser.reset();
            return None;
        }
        let snapshot = Arc::new(Snapshot {
            revision,
            text,
            tree: tree.unwrap(),
            query: self.query.clone(),
        });
        self.snapshot = Some(snapshot.clone());
        Some(snapshot)
    }
}

struct Request {
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
    pub fn new(language: Language) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::spawn(move || Engine::new(language))
    }
    fn spawn(
        initialize: impl FnOnce() -> Result<Engine, Box<dyn std::error::Error + Send + Sync>>
        + Send
        + 'static,
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
                let mut parsed: Option<Arc<Snapshot>> = None;
                let mut viewport: Option<ViewportRequest> = None;
                let mut parsed_generation = 0;
                let mut published = None;
                while wakes.recv_blocking().is_ok() {
                    if let Ok(request) = requests.try_recv()
                        && let Some(snapshot) =
                            engine.update(request.revision, request.text, &request.edits, || {
                                current.load(Ordering::Relaxed) != request.generation
                            })
                    {
                        parsed_generation = request.generation;
                        parsed = Some(snapshot);
                    }
                    if let Ok(request) = viewports.try_recv() {
                        viewport = Some(request);
                    }
                    if let (Some(snapshot), Some(view)) = (&parsed, &viewport)
                        && snapshot.revision == view.revision
                        && published != Some((snapshot.revision, view.generation))
                        && let Some(highlights) = HighlightSnapshot::prepare(
                            snapshot,
                            view.generation,
                            view.ranges.clone(),
                            || {
                                current.load(Ordering::Relaxed) != parsed_generation
                                    || current_viewport.load(Ordering::Relaxed) != view.generation
                            },
                        )
                    {
                        published = Some((snapshot.revision, view.generation));
                        send_latest(&results, &stale_results, Ok(highlights));
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
    fn fresh(text: &Rope) -> Vec<Highlight> {
        Engine::new(Language::Rust)
            .unwrap()
            .update(0, text.clone(), &[], || false)
            .unwrap()
            .highlights(0..text.len_chars())
    }
    #[test]
    fn rust_captures_real_source_and_clips_windows() {
        let text = Rope::from_str(include_str!("../../editor/src/main.rs"));
        let mut engine = Engine::new(Language::Rust).unwrap();
        let snapshot = engine.update(0, text.clone(), &[], || false).unwrap();
        let highlights = snapshot.highlights(0..text.len_chars());
        for kind in ["keyword", "function", "type", "string"] {
            assert!(
                highlights.iter().any(|h| h.capture == kind),
                "missing {kind}"
            );
        }
        let start = text.to_string().find("fn main").unwrap();
        let start = text.byte_to_char(start);
        assert!(
            snapshot
                .highlights(start..start + 7)
                .iter()
                .all(|h| h.range.start >= start && h.range.end <= start + 7)
        );
    }
    #[test]
    fn per_line_queries_match_full_document_captures() {
        let text = Rope::from_str(include_str!("../../editor/src/main.rs"));
        let snapshot = Engine::new(Language::Rust)
            .unwrap()
            .update(0, text.clone(), &[], || false)
            .unwrap();
        let all = snapshot.highlights(0..text.len_chars());
        for line in 0..text.len_lines() {
            let start = text.line_to_char(line);
            let end = start + text.line(line).len_chars();
            let expected: Vec<_> = all
                .iter()
                .filter_map(|h| {
                    let range = h.range.start.max(start)..h.range.end.min(end);
                    (range.start < range.end).then(|| Highlight {
                        range,
                        capture: h.capture.clone(),
                    })
                })
                .collect();
            assert_eq!(
                snapshot.highlights(start..end),
                expected,
                "line {line}: {}",
                text.line(line)
            );
        }
    }

    #[test]
    fn incremental_unicode_multiline_edits_and_inverse_match_full_parse() {
        let mut engine = Engine::new(Language::Rust).unwrap();
        let mut text = Rope::from_str("fn café() {\n    let name = \"世界👩‍💻\";\n}\n");
        engine.update(0, text.clone(), &[], || false).unwrap();
        let mut revision = 0;
        for (range, inserted) in [
            (0..0, "/* comment\n"),
            (4..11, "λ\n"),
            (0..0, "*/\n"),
            (8..8, "\"x\""),
        ] {
            let before = text.clone();
            text.remove(range.clone());
            text.insert(range.start, inserted);
            revision += 1;
            let edit = Edit::between(revision, &before, &text, range, inserted.chars().count());
            let snapshot = engine
                .update(revision, text.clone(), std::slice::from_ref(&edit), || {
                    false
                })
                .unwrap();
            assert_eq!(snapshot.highlights(0..text.len_chars()), fresh(&text));
            revision += 1;
            let inverse = edit.inverse(revision);
            text = before;
            let snapshot = engine
                .update(revision, text.clone(), &[inverse], || false)
                .unwrap();
            assert_eq!(snapshot.highlights(0..text.len_chars()), fresh(&text));
        }
    }
    #[test]
    fn lost_edit_chain_falls_back_and_text_predicates_refresh() {
        let mut engine = Engine::new(Language::Rust).unwrap();
        let text = Rope::from_str("fn main() { let name = value; }");
        engine.update(0, text, &[], || false).unwrap();
        let text = Rope::from_str("fn main() { let name = Some; }");
        let snapshot = engine.update(200, text.clone(), &[], || false).unwrap();
        assert_eq!(snapshot.highlights(0..text.len_chars()), fresh(&text));
        assert!(engine.update(201, text, &[], || true).is_none());
    }
    #[test]
    fn same_shape_identifier_edit_rechecks_text_predicates() {
        let mut engine = Engine::new(Language::Rust).unwrap();
        let before = Rope::from_str("fn main() { Some; }");
        let first = engine.update(0, before.clone(), &[], || false).unwrap();
        let mut after = before.clone();
        after.remove(12..16);
        after.insert(12, "none");
        let edit = Edit::between(1, &before, &after, 12..16, 4);
        let next = engine.update(1, after.clone(), &[edit], || false).unwrap();
        assert_ne!(
            first.highlights(0..before.len_chars()),
            next.highlights(0..after.len_chars())
        );
        assert_eq!(next.highlights(0..after.len_chars()), fresh(&after));
    }

    fn receive(
        worker: &Worker,
        predicate: impl Fn(&HighlightSnapshot) -> bool,
    ) -> Arc<HighlightSnapshot> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(update) = worker.updates.try_recv() {
                let snapshot = update.unwrap();
                if predicate(&snapshot) {
                    return snapshot;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker result timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    #[test]
    fn initialization_errors_are_delivered_asynchronously() {
        let worker =
            Worker::spawn(|| Err(Box::new(std::io::Error::other("grammar unavailable")))).unwrap();
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
            Engine::new(Language::Rust)
        })
        .unwrap();
        assert_ne!(
            on_worker
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            caller
        );
        // The worker cannot process any of these until the gate opens. This
        // deterministically checks progress without a wall-clock latency assertion.
        for revision in 0..1000 {
            worker.request(revision, Rope::from_str("fn main() {}"), vec![]);
            worker.request_viewport(revision, std::iter::once(0..12).collect::<Vec<_>>());
        }
        drop(worker);
        release.send(()).unwrap();
    }
    #[test]
    fn scrolling_requests_new_captures_without_a_document_edit() {
        let worker = Worker::new(Language::Rust).unwrap();
        let text = Rope::from_str("fn first() {}\nfn second() {}\n");
        worker.request(0, text.clone(), vec![]);
        worker.request_viewport(0, std::iter::once(0..13).collect::<Vec<_>>());
        let first = receive(&worker, |s| s.revision == 0);
        assert_eq!(first.coverage, std::iter::once(0..13).collect::<Vec<_>>());
        worker.request_viewport(0, std::iter::once(14..text.len_chars()).collect::<Vec<_>>());
        let next = receive(&worker, |s| s.revision == 0 && s.viewport > first.viewport);
        assert_eq!(
            next.coverage,
            std::iter::once(14..text.len_chars()).collect::<Vec<_>>()
        );
        assert!(next.highlights(0..14).is_empty());
        assert!(
            next.highlights(14..text.len_chars())
                .iter()
                .any(|h| h.capture == "function")
        );
    }
    #[test]
    fn indexed_captures_match_queries_with_overlaps_clipping_and_priority() {
        let text = Rope::from_str(
            "#[derive(Debug)]\nfn café() { let s = \"hello\\n世界\"; } // comment\n",
        );
        let snapshot = Engine::new(Language::Rust)
            .unwrap()
            .update(0, text.clone(), &[], || false)
            .unwrap();
        let highlights = HighlightSnapshot::prepare(
            &snapshot,
            1,
            vec![5..30, 0..10, 25..text.len_chars()],
            || false,
        )
        .unwrap();
        assert_eq!(
            highlights.coverage,
            std::iter::once(0..text.len_chars()).collect::<Vec<_>>()
        );
        for start in 0..text.len_chars() {
            let end = (start + 7).min(text.len_chars());
            assert_eq!(
                highlights.highlights(start..end),
                snapshot.highlights(start..end)
            );
        }
        assert!(
            HighlightSnapshot::prepare(
                &snapshot,
                2,
                std::iter::once(0..text.len_chars()).collect::<Vec<_>>(),
                || true
            )
            .is_none()
        );
        assert!(
            snapshot
                .highlights_cancellable(0..text.len_chars(), || true)
                .is_none()
        );
    }
    #[test]
    fn viewport_requests_coalesce_and_do_not_cancel_initial_parsing() {
        let worker = Worker::new(Language::Rust).unwrap();
        let text = Rope::from_str(&"fn sample() { let value = Some(42); }\n".repeat(10_000));
        worker.request(0, text.clone(), vec![]);
        for start in 0..1000 {
            worker.request_viewport(0, std::iter::once(start..start + 100).collect::<Vec<_>>());
        }
        let final_snapshot = receive(&worker, |s| s.viewport == 1000);
        assert_eq!(
            final_snapshot.coverage,
            std::iter::once(999..1099).collect::<Vec<_>>()
        );
        let expected = Engine::new(Language::Rust)
            .unwrap()
            .update(0, text, &[], || false)
            .unwrap();
        assert_eq!(
            final_snapshot.highlights(999..1099),
            expected.highlights(999..1099)
        );
    }

    #[test]
    fn worker_coalesces_to_final_revision() {
        let worker = Worker::new(Language::Rust).unwrap();
        for revision in 0..100 {
            worker.request(
                revision,
                Rope::from_str(&format!("fn main() {{ let n = {revision}; }}")),
                vec![],
            );
        }
        worker.request_viewport(99, std::iter::once(0..100).collect::<Vec<_>>());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(Ok(snapshot)) = worker.updates.try_recv()
                && snapshot.revision == 99
            {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}
