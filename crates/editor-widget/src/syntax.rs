use crate::{Annotations, Buffer, SpanStyle, StyledRange, annotations::display_ranges, graphemes};
use rxui::{Context, Listener, Task};
use std::{
    ops::Range,
    sync::{Arc, Weak},
};
use velk_syntax::{Snapshot, Worker};

pub(crate) struct Monitor {
    worker: Arc<Worker>,
    publish: Listener<Arc<Snapshot>>,
    task: Option<Task>,
}
impl Monitor {
    pub fn new(
        worker: Arc<Worker>,
        publish: Listener<Arc<Snapshot>>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut monitor = Self {
            worker,
            publish,
            task: None,
        };
        monitor.arm(cx);
        monitor
    }
    fn arm(&mut self, cx: &mut Context<'_, Self>) {
        let updates = self.worker.updates.clone();
        self.task = cx
            .try_spawn(async move { updates.recv().await }, |this, result, cx| {
                if let Ok(Ok(snapshot)) = result {
                    let _ = this.publish.dispatch(&snapshot, cx);
                    cx.unchanged();
                    this.arm(cx);
                } else {
                    cx.unchanged();
                }
            })
            .ok();
    }
}

#[derive(Clone)]
pub(crate) struct SyntaxView {
    snapshot: Arc<Snapshot>,
    replacements: Vec<(Range<usize>, usize)>,
}
/// A cached line must not keep an obsolete document tree alive offscreen.
pub(crate) struct SyntaxKey {
    snapshot: Weak<Snapshot>,
    replacements: Vec<(Range<usize>, usize)>,
}
impl SyntaxKey {
    pub fn matches(&self, view: &SyntaxView) -> bool {
        self.snapshot.as_ptr() == Arc::as_ptr(&view.snapshot)
            && self.replacements == view.replacements
    }
}
impl SyntaxView {
    pub fn new(snapshot: Arc<Snapshot>, buffer: &Buffer) -> Option<Self> {
        if snapshot.revision > buffer.revision() {
            return None;
        }
        let replacements = buffer
            .edits_since(snapshot.revision)?
            .into_iter()
            .map(|e| (e.replaced, e.inserted))
            .collect();
        Some(Self {
            snapshot,
            replacements,
        })
    }
    pub fn project(&self, range: Range<usize>, inserted: usize) -> Self {
        let mut view = self.clone();
        view.replacements.push((range, inserted));
        view
    }
    pub fn key(&self) -> SyntaxKey {
        SyntaxKey {
            snapshot: Arc::downgrade(&self.snapshot),
            replacements: self.replacements.clone(),
        }
    }
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.snapshot, &other.snapshot) && self.replacements == other.replacements
    }
    pub fn spans(&self, rope: &ropey::Rope, window: Range<usize>) -> Vec<StyledRange> {
        // Map the visible window back into the parsed snapshot. A window inside
        // freshly inserted text still queries the surrounding original context.
        let mut source = window.clone();
        for (old, inserted) in self.replacements.iter().rev() {
            let unmap = |p: usize, end: bool| {
                if p < old.start {
                    p
                } else if p >= old.start + inserted {
                    p - inserted + old.len()
                } else if end {
                    old.end
                } else {
                    old.start
                }
            };
            source = unmap(source.start, false)..unmap(source.end, true);
        }
        source.start = source.start.min(self.snapshot.text.len_chars());
        source.end = source
            .end
            .min(self.snapshot.text.len_chars())
            .max(source.start);
        let mut result = Vec::new();
        for highlight in self.snapshot.highlights(source) {
            let Some(color) = color(&highlight.capture) else {
                continue;
            };
            let mut ranges = vec![highlight.range];
            for replacement in &self.replacements {
                ranges = ranges
                    .into_iter()
                    .flat_map(|r| display_ranges(r, Some(replacement)))
                    .collect();
            }
            for range in ranges {
                let mut start = range.start.max(window.start);
                let mut end = range.end.min(window.end);
                if start >= end {
                    continue;
                }
                if !graphemes::is_boundary(rope, start) {
                    start = graphemes::next(rope, start);
                }
                if !graphemes::is_boundary(rope, end) {
                    end = graphemes::previous(rope, end);
                }
                if start < end {
                    result.push(StyledRange::new(
                        start - window.start..end - window.start,
                        SpanStyle {
                            foreground: Some(color),
                            ..SpanStyle::default()
                        },
                    ));
                }
            }
        }
        result
    }
}
fn color(capture: &str) -> Option<[f32; 4]> {
    let rgb: [u8; 3] = match capture.split('.').next()? {
        "keyword" => [198, 120, 221],
        "string" => [152, 195, 121],
        "comment" => [113, 128, 150],
        "function" => [97, 175, 239],
        "type" | "constructor" => [229, 192, 123],
        "constant" | "number" | "boolean" => [209, 154, 102],
        "property" => [224, 108, 117],
        "attribute" | "label" => [86, 182, 194],
        "escape" => [229, 192, 123],
        "variable" if capture == "variable.builtin" => [224, 108, 117],
        _ => return None,
    };
    let linear = |c: u8| {
        let c = c as f32 / 255.;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    Some([linear(rgb[0]), linear(rgb[1]), linear(rgb[2]), 1.])
}

pub(crate) fn merge(
    syntax: &[StyledRange],
    annotations: Option<Arc<Annotations>>,
    revision: u64,
) -> Option<Arc<Annotations>> {
    if syntax.is_empty() {
        return annotations;
    }
    let mut annotations = annotations.as_deref().cloned().unwrap_or(Annotations {
        document_revision: revision,
        ..Annotations::default()
    });
    let mut spans = syntax.to_vec();
    spans.append(&mut annotations.spans);
    annotations.spans = spans;
    Some(Arc::new(annotations))
}

#[cfg(test)]
mod tests {
    use super::*;
    use velk_syntax::{Engine, Language};
    fn view(buffer: &Buffer) -> SyntaxView {
        let snapshot = Engine::new(Language::Rust)
            .unwrap()
            .update(buffer.revision(), buffer.text().clone(), &[], || false)
            .unwrap();
        SyntaxView::new(snapshot, buffer).unwrap()
    }
    #[test]
    fn stale_highlights_map_through_edits_undo_redo_and_ime() {
        let mut buffer = Buffer::new("fn café() { let value = \"世界\"; }\n");
        let old = view(&buffer).snapshot;
        buffer.insert("// 👩‍💻\n");
        let mapped = SyntaxView::new(old.clone(), &buffer).unwrap();
        let spans = mapped.spans(buffer.text(), 0..buffer.text().len_chars());
        assert!(
            spans
                .iter()
                .any(|s| buffer.text().slice(s.range.clone()) == "fn")
        );
        assert!(
            spans
                .iter()
                .all(|s| s.range.start >= "// 👩‍💻\n".chars().count())
        );
        buffer.undo();
        assert_eq!(
            SyntaxView::new(old.clone(), &buffer)
                .unwrap()
                .spans(buffer.text(), 0..buffer.text().len_chars()),
            view(&buffer).spans(buffer.text(), 0..buffer.text().len_chars())
        );
        buffer.redo();
        let mapped = SyntaxView::new(old, &buffer).unwrap();
        let mut projected = buffer.text().clone();
        projected.insert(0, "候補\n");
        let spans = mapped
            .project(0..0, 3)
            .spans(&projected, 0..projected.len_chars());
        assert!(
            spans
                .iter()
                .any(|s| projected.slice(s.range.clone()) == "fn")
        );
        assert!(
            spans
                .iter()
                .all(|s| graphemes::is_boundary(&projected, s.range.start)
                    && graphemes::is_boundary(&projected, s.range.end))
        );
    }
    #[test]
    fn visible_window_is_local_and_explicit_annotations_override_syntax() {
        let buffer = Buffer::new("fn main() { let name = \"é👩‍💻\"; }\n");
        let syntax = view(&buffer);
        let spans = syntax.spans(buffer.text(), 11..buffer.text().len_chars());
        assert!(
            spans
                .iter()
                .all(|s| s.range.end <= buffer.text().len_chars() - 11)
        );
        let explicit = StyledRange::new(
            0..3,
            SpanStyle {
                foreground: Some([1.; 4]),
                ..SpanStyle::default()
            },
        )
        .with_hover("test", "hover");
        let merged = merge(
            &spans,
            Some(Arc::new(Annotations {
                spans: vec![explicit.clone()],
                ..Annotations::default()
            })),
            0,
        )
        .unwrap();
        assert_eq!(merged.spans.last(), Some(&explicit));
    }
    #[test]
    fn buffer_history_provides_incremental_coordinates_for_every_revision() {
        let mut buffer = Buffer::new("fn café() { let text = \"世界\"; }\n");
        let mut engine = Engine::new(Language::Rust).unwrap();
        engine
            .update(0, buffer.text().clone(), &[], || false)
            .unwrap();
        for operation in 0..8 {
            match operation {
                0 => buffer.insert("// 👩‍💻\n"),
                1 => {
                    buffer.document_edge(true, false);
                    buffer.insert("fn next() {}\n");
                }
                2 => buffer.backspace(),
                3 | 4 => buffer.undo(),
                5 | 6 => buffer.redo(),
                _ => {
                    buffer.select_all();
                    buffer.insert("/* é */\nfn new() {}\n");
                }
            }
            let changes = buffer.edits_since(buffer.revision() - 1).unwrap();
            let incremental = engine
                .update(buffer.revision(), buffer.text().clone(), &changes, || false)
                .unwrap();
            let fresh = Engine::new(Language::Rust)
                .unwrap()
                .update(buffer.revision(), buffer.text().clone(), &[], || false)
                .unwrap();
            assert_eq!(
                incremental.highlights(0..buffer.text().len_chars()),
                fresh.highlights(0..buffer.text().len_chars()),
                "operation {operation}"
            );
        }
    }

    #[test]
    fn offscreen_cache_key_does_not_retain_obsolete_trees() {
        let buffer = Buffer::new("fn main() {}");
        let view = view(&buffer);
        let key = view.key();
        assert!(key.matches(&view));
        let snapshot = Arc::downgrade(&view.snapshot);
        drop(view);
        assert!(snapshot.upgrade().is_none());
        assert!(key.snapshot.upgrade().is_none());
    }

    #[test]
    fn old_snapshot_beyond_edit_log_is_rejected() {
        let mut buffer = Buffer::new("fn main() {}");
        let snapshot = view(&buffer).snapshot;
        for _ in 0..129 {
            buffer.insert(" ");
        }
        assert!(SyntaxView::new(snapshot, &buffer).is_none());
    }
}
