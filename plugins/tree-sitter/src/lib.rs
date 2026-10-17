//! Bundled Tree-sitter syntax provider. Grammar, trees, and queries stay here.
use ropey::Rope;
use std::{
    ops::{ControlFlow, Range},
    sync::Arc,
};
use tree_sitter::{
    InputEdit, Node, ParseOptions, Parser, Point, Query, QueryCursor, QueryCursorOptions,
    StreamingIterator, TextProvider, Tree,
};
#[cfg(test)]
use velk_syntax::Worker;
use velk_syntax::{Edit, Highlight, HighlightSnapshot, Language, merge_ranges};
fn input_edit(change: &Edit) -> InputEdit {
    let point = |position: velk_core::TextPoint| Point::new(position.line, position.column);
    InputEdit {
        start_byte: change.start.byte,
        old_end_byte: change.old_end.byte,
        new_end_byte: change.new_end.byte,
        start_position: point(change.start),
        old_end_position: point(change.old_end),
        new_end_position: point(change.new_end),
    }
}

pub struct Snapshot {
    pub revision: u64,
    pub text: Rope,
    tree: Tree,
    query: Arc<Query>,
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

pub struct Engine {
    parser: Parser,
    query: Arc<Query>,
    snapshot: Option<Arc<Snapshot>>,
}
impl Engine {
    pub fn new(language: Language) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let _trace = velk_plugin_sdk::diagnostics::scope!("syntax.initialize");
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
        let _trace = velk_plugin_sdk::diagnostics::scope!("syntax.update");
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
                    tree.edit(&input_edit(change));
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

pub fn prepare(
    snapshot: &Snapshot,
    viewport: u64,
    ranges: Vec<Range<usize>>,
    cancelled: impl Fn() -> bool,
) -> Option<Arc<HighlightSnapshot>> {
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
    Some(HighlightSnapshot::new(
        snapshot.revision,
        viewport,
        snapshot.text.clone(),
        coverage,
        captures.into_iter().map(|(_, h)| h).collect(),
    ))
}

#[derive(Default, Debug)]
pub struct TreeSitter;
impl velk_plugin_sdk::Plugin for TreeSitter {
    const ID: &'static str = "velk.tree-sitter";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const NAME: &'static str = "Tree-sitter";
    const README: &'static str = include_str!("../README.md");
    fn register(reg: &mut velk_plugin_sdk::Registrar<Self>) -> Result<(), String> {
        reg.syntax::<Self>()
    }
    fn describe(&mut self) -> String {
        "Incremental Rust syntax highlighting".into()
    }
}
impl velk_syntax::Provider for TreeSitter {
    fn open(&self, language: Language) -> Result<Box<dyn velk_syntax::Session>, String> {
        Ok(Box::new(Engine::new(language).map_err(|e| e.to_string())?))
    }
}
impl velk_syntax::Session for Engine {
    fn update(
        &mut self,
        revision: u64,
        text: Rope,
        edits: &[Edit],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<bool, String> {
        Ok(Engine::update(self, revision, text, edits, cancelled).is_some())
    }
    fn highlights(
        &mut self,
        range: Range<usize>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Vec<Highlight>>, String> {
        let _trace = velk_plugin_sdk::diagnostics::scope!("syntax.query");
        Ok(self
            .snapshot
            .as_ref()
            .and_then(|s| s.highlights_cancellable(range, cancelled)))
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
        let text = Rope::from_str(include_str!("../../../crates/editor/src/main.rs"));
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
        let text = Rope::from_str(include_str!("../../../crates/editor/src/main.rs"));
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
    fn scrolling_requests_new_captures_without_a_document_edit() {
        let worker = Worker::new(Arc::new(TreeSitter), Language::Rust).unwrap();
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
        let highlights = prepare(
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
            prepare(
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
        let worker = Worker::new(Arc::new(TreeSitter), Language::Rust).unwrap();
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
        let worker = Worker::new(Arc::new(TreeSitter), Language::Rust).unwrap();
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
