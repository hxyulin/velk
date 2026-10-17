//! Widget-owned hover lifetime. Providers return content against a target token;
//! neither the document caret nor a transport-specific request identifies it.
use crate::{Editor, HoverCard, HoverSource, HoverTarget, graphemes, surface::HoverHit};
use rxui::{Context, Task};
use std::{ops::Range, time::Duration};

const DELAY: Duration = Duration::from_millis(350);
const LEAVE_GRACE: Duration = Duration::from_millis(180);

#[derive(Default)]
pub(super) struct State {
    pub(super) enabled: bool,
    generation: u64,
    active: Option<Interaction>,
    delay: Option<Task>,
    leave: Option<Task>,
    in_card: bool,
    pub(super) scroll: rxui::ScrollHandle,
}
struct Interaction {
    target: HoverTarget,
    range: Range<usize>,
    anchor: [f32; 2],
    context: [f32; 6],
    annotation: Option<HoverHit>,
    semantic: Option<HoverCard>,
    formatted: Option<std::sync::Arc<velk_markup::Document>>,
    ready: bool,
    resolved: bool,
    visible: bool,
}
impl Editor {
    pub fn set_semantic_hover(&mut self, enabled: bool) {
        if self.hover.enabled != enabled {
            self.hide_hover();
            self.hover.enabled = enabled;
        }
    }
    /// Exposed only after dwell, or immediately for an explicit caret request.
    pub fn hover_request(&self) -> Option<HoverTarget> {
        self.hover
            .active
            .as_ref()
            .filter(|a| {
                self.hover.enabled
                    && a.ready
                    && a.target.revision == self.buffer.revision()
                    && a.context == self.hover_context()
            })
            .map(|a| a.target)
    }
    /// Resolve exactly the still-active target. An empty result is remembered so
    /// moving within the same symbol does not repeatedly query the provider.
    pub fn resolve_hover(
        &mut self,
        target: HoverTarget,
        range: Option<Range<usize>>,
        card: Option<HoverCard>,
    ) -> bool {
        if self.hover_request() != Some(target) {
            return false;
        }
        let active = self.hover.active.as_mut().unwrap();
        if let Some(range) = range.filter(|r| r.contains(&target.position)) {
            let intersection = active.range.start.max(range.start)..active.range.end.min(range.end);
            if !intersection.is_empty() {
                active.range = intersection;
            }
        }
        active.formatted = None;
        active.resolved = true;
        active.semantic = card.filter(|card| !card.body.trim().is_empty());
        active.visible = active.semantic.is_some() || active.annotation.is_some();
        self.hover_changed();
        true
    }
    /// Present a provider-rendered document. Parsing belongs on a worker; the
    /// widget only maps cached runs to native text spans. Target validation is
    /// identical to plain hover, including stale background-work rejection.
    pub fn resolve_formatted_hover(
        &mut self,
        target: HoverTarget,
        range: Option<Range<usize>>,
        title: String,
        document: std::sync::Arc<velk_markup::Document>,
    ) -> bool {
        let body = document.text();
        if !self.resolve_hover(target, range, Some(HoverCard { title, body })) {
            return false;
        }
        self.hover.active.as_mut().unwrap().formatted = Some(document);
        true
    }
    pub(super) fn hover_body(&self, card: &HoverCard) -> rxui::Element {
        use rxui::prelude::*;
        let Some(active) = self.hover.active.as_ref() else {
            return label(card.body.clone());
        };
        let Some(document) = &active.formatted else {
            return label(card.body.clone());
        };
        let mut spans = Vec::new();
        if let Some(annotation) = &active.annotation {
            spans.push(rxui::span(format!(
                "{}\n{}\n\n",
                annotation.card.title, annotation.card.body
            )));
        }
        spans.extend(document.runs.iter().map(|run| {
            let mut span = rxui::span(run.text.clone());
            let style = &run.style;
            if style.bold {
                span = span.bold();
            }
            if style.italic {
                span = span.italic();
            }
            if style.code {
                span = span.monospace().color(ThemeColor::Text);
            }
            if style.strikethrough {
                span = span.strikethrough();
            }
            if style.heading > 0 {
                span = span.font_size(19. - f32::from(style.heading.min(6)));
            }
            // Retain the destination in the document for a future host link
            // action; rendering never opens URLs or executes command URIs.
            if style.link.is_some() {
                span = span.underline().color(ThemeColor::Accent);
            }
            span
        }));
        rxui::rich_text(spans)
    }
    pub fn request_hover_at_caret(&mut self) {
        self.hide_hover();
        if !self.hover.enabled || self.composition.is_some() {
            return;
        }
        let position = self.caret_position();
        let geometry = self.geometry.borrow();
        let caret = geometry.caret_area();
        let anchor = [
            geometry.origin[0] + caret.x,
            geometry.origin[1] + caret.y + caret.height,
        ];
        drop(geometry);
        let range = position..position.saturating_add(1);
        self.begin_hover(position, range, anchor, None, HoverSource::Caret);
        if let Some(active) = &mut self.hover.active {
            active.ready = true;
        }
    }
    pub(super) fn refresh_hover_annotations(&mut self) {
        let Some(active) = &mut self.hover.active else {
            return;
        };
        if active.target.position == usize::MAX {
            self.hide_hover();
            return;
        }
        active.annotation = self.annotations.as_ref().and_then(|annotations| {
            annotations
                .spans
                .iter()
                .enumerate()
                .rev()
                .find_map(|(id, span)| {
                    span.range
                        .contains(&active.target.position)
                        .then(|| span.hover.clone())
                        .flatten()
                        .map(|card| HoverHit {
                            id,
                            card,
                            anchor: active.anchor,
                        })
                })
        });
        if active.ready {
            active.visible = active.annotation.is_some() || active.semantic.is_some();
        }
    }
    pub(super) fn hide_hover(&mut self) {
        let had_hover = self.hover.active.is_some();
        self.hover.generation = self.hover.generation.wrapping_add(1);
        self.hover.delay = None;
        self.hover.leave = None;
        self.hover.active = None;
        self.hover.in_card = false;
        if had_hover {
            self.hover.scroll = rxui::ScrollHandle::new();
            self.hover_changed();
        }
    }
    pub(super) fn has_hover(&self) -> bool {
        self.hover.active.is_some()
    }
    fn hover_context(&self) -> [f32; 6] {
        let g = self.geometry.borrow();
        [
            g.origin[0],
            g.origin[1],
            g.viewport[0],
            g.viewport[1],
            g.offset[0],
            g.offset[1],
        ]
    }
    fn hover_changed(&mut self) {
        self.epoch += 1;
        self.reveal_caret = false;
    }
    fn begin_hover(
        &mut self,
        position: usize,
        range: Range<usize>,
        anchor: [f32; 2],
        annotation: Option<HoverHit>,
        source: HoverSource,
    ) {
        let target = HoverTarget {
            id: self.hover.generation,
            revision: self.buffer.revision(),
            position,
            source,
        };
        self.hover.active = Some(Interaction {
            target,
            range,
            anchor,
            context: self.hover_context(),
            annotation,
            semantic: None,
            formatted: None,
            ready: false,
            resolved: false,
            visible: false,
        });
    }
    pub(super) fn hover(&mut self, point: [f32; 2], cx: &mut Context<'_, Self>) {
        self.hover.in_card = false;
        if self.composition.is_some() {
            self.hide_hover();
            return;
        }
        let geometry = self.geometry.borrow();
        let annotation = geometry.hover(point);
        let source = self
            .hover
            .enabled
            .then(|| geometry.text_hover(point))
            .flatten()
            .filter(|(position, _)| {
                *position < self.buffer.text().len_chars()
                    && !self.buffer.text().char(*position).is_whitespace()
            });
        drop(geometry);
        let same = self.hover.active.as_ref().is_some_and(|a| {
            a.target.source == HoverSource::Pointer
                && a.context == self.hover_context()
                && a.target.revision == self.buffer.revision()
                && a.annotation.as_ref().map(|h| h.id) == annotation.as_ref().map(|h| h.id)
                && match source {
                    Some((position, _)) => a.range.contains(&position),
                    None => a.annotation.is_some() && a.target.position == usize::MAX,
                }
        });
        if same {
            self.hover.leave = None;
            let active = self.hover.active.as_mut().unwrap();
            if !active.ready
                && !active.resolved
                && active.visible
                && active.target.position != usize::MAX
            {
                active.ready = true;
            } else {
                cx.unchanged();
            }
            return;
        }
        if source.is_none()
            && annotation.is_none()
            && self.hover.active.as_ref().is_some_and(|a| a.visible)
        {
            if self.hover.leave.is_none() {
                self.leave_hover(cx);
            } else {
                cx.unchanged();
            }
            return;
        }
        if self.hover.active.is_none() && annotation.is_none() && source.is_none() {
            cx.unchanged();
            return;
        }
        self.hide_hover();
        if source.is_none() && annotation.is_none() {
            return;
        }
        let (position, range, anchor) = if let Some((position, anchor)) = source {
            (
                position,
                symbol_range(&self.buffer.text(), position),
                anchor,
            )
        } else {
            let annotation = annotation.as_ref().unwrap();
            (usize::MAX, 0..0, annotation.anchor)
        };
        self.begin_hover(position, range, anchor, annotation, HoverSource::Pointer);
        let id = self.hover.generation;
        self.hover.delay = cx
            .try_spawn(rxui::sleep(DELAY), move |this, result, cx| {
                if result.is_ok()
                    && this
                        .hover
                        .active
                        .as_ref()
                        .is_some_and(|a| a.target.id == id && a.context == this.hover_context())
                {
                    let active = this.hover.active.as_mut().unwrap();
                    // Annotation-only overlays have no source position to query.
                    active.ready = active.target.position != usize::MAX;
                    active.visible = active.annotation.is_some();
                    this.hover_changed();
                } else {
                    cx.unchanged();
                }
            })
            .ok();
    }
    /// A short leave grace lets the pointer cross the gap into the popup. The
    /// request is invalidated immediately; the visible content alone may linger.
    pub(super) fn leave_hover(&mut self, cx: &mut Context<'_, Self>) {
        self.hover.delay = None;
        let Some(active) = &mut self.hover.active else {
            cx.unchanged();
            return;
        };
        if !active.visible {
            self.hide_hover();
            return;
        }
        active.ready = false;
        let id = active.target.id;
        self.hover.leave = cx
            .try_spawn(rxui::sleep(LEAVE_GRACE), move |this, result, cx| {
                if result.is_ok()
                    && !this.hover.in_card
                    && this
                        .hover
                        .active
                        .as_ref()
                        .is_some_and(|a| a.target.id == id)
                {
                    this.hide_hover();
                } else {
                    cx.unchanged();
                }
            })
            .ok();
    }
    pub(super) fn card_hover(&mut self, inside: bool, cx: &mut Context<'_, Self>) {
        if inside {
            self.hover.in_card = true;
            self.hover.leave = None;
            cx.unchanged();
        } else {
            self.hover.in_card = false;
            self.leave_hover(cx);
        }
    }
    pub(super) fn hover_card(&self) -> Option<([f32; 2], HoverCard)> {
        let active = self
            .hover
            .active
            .as_ref()
            .filter(|a| a.visible && a.context == self.hover_context())?;
        let card = match (&active.annotation, &active.semantic) {
            (Some(annotation), Some(semantic)) => HoverCard {
                title: semantic.title.clone(),
                body: format!(
                    "{}\n{}\n\n{}",
                    annotation.card.title, annotation.card.body, semantic.body
                ),
            },
            (Some(annotation), None) => annotation.card.clone(),
            (None, Some(semantic)) => semantic.clone(),
            (None, None) => return None,
        };
        Some((active.anchor, card))
    }
}
fn symbol_range(text: &ropey::Rope, position: usize) -> Range<usize> {
    if !unicode_ident::is_xid_continue(text.char(position)) {
        return position..graphemes::next(text, position);
    }
    // Bound work even for pathological identifiers on multi-megabyte lines.
    let left = text
        .chars_at(position)
        .reversed()
        .take(256)
        .take_while(|&c| unicode_ident::is_xid_continue(c))
        .count();
    let right = text
        .chars_at(position)
        .take(256)
        .take_while(|&c| unicode_ident::is_xid_continue(c))
        .count();
    position - left..position + right
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        surface::{Geometry, LINE_HEIGHT, PADDING},
        viewport::LineHit,
    };
    use rxui::{Entity, Runtime, ThreadPoolExecutor};
    use std::{cell::RefCell, rc::Rc, sync::Arc, time::Instant};

    fn editor() -> (Runtime, Entity<Editor>) {
        let mut runtime = Runtime::new();
        runtime
            .configure_tasks(Arc::new(ThreadPoolExecutor::new(1, 1).unwrap()), || {})
            .unwrap();
        let editor = runtime.update(|cx| {
            cx.new(|_| {
                let mut editor = Editor::new("alpha beta\n");
                editor.set_semantic_hover(true);
                let mut geometry = editor.geometry.borrow_mut();
                geometry.viewport = [400., 140.];
                geometry.lines.push(LineHit {
                    top: 0.,
                    height: LINE_HEIGHT,
                    start_char: 0,
                    left: 0.,
                    grid: Some((10., 10)),
                    rich: None,
                    unicode: None,
                    geometry: Rc::new(RefCell::new(Geometry::default())),
                });
                drop(geometry);
                editor
            })
        });
        (runtime, editor)
    }
    fn point(column: f32) -> [f32; 2] {
        [PADDING + column * 10., PADDING + 12.]
    }
    fn request(runtime: &mut Runtime, editor: &Entity<Editor>) -> HoverTarget {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            runtime.poll_tasks();
            runtime.flush().unwrap();
            if let Some(target) = runtime.update(|cx| editor.read(cx).hover_request()) {
                return target;
            }
            assert!(Instant::now() < deadline, "hover dwell did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn dwell_targets_pointer_without_moving_caret_and_reuses_a_symbol() {
        let (mut runtime, editor) = editor();
        runtime.update(|cx| editor.update(cx, |e, cx| e.hover(point(6.9), cx)));
        assert!(
            runtime
                .update(|cx| editor.read(cx).hover_request())
                .is_none()
        );
        let target = request(&mut runtime, &editor);
        assert_eq!(target.position, 6);
        assert_eq!(target.source, HoverSource::Pointer);
        runtime.update(|cx| {
            editor.update(cx, |e, cx| {
                assert_eq!(e.buffer.selection().head, 0);
                assert!(e.resolve_hover(
                    target,
                    Some(6..10),
                    Some(HoverCard {
                        title: "Rust".into(),
                        body: "let beta: usize".into()
                    })
                ));
                e.hover(point(8.4), cx);
                assert_eq!(e.hover_request(), Some(target));
                assert!(e.hover_card().unwrap().1.body.contains("beta"));
            })
        });
    }
    #[test]
    fn target_changes_and_escape_or_scroll_lifetime_reject_late_results() {
        let (mut runtime, editor) = editor();
        runtime.update(|cx| editor.update(cx, |e, cx| e.hover(point(1.2), cx)));
        let old = request(&mut runtime, &editor);
        runtime.update(|cx| {
            editor.update(cx, |e, cx| {
                e.hover(point(7.2), cx);
                assert!(!e.resolve_hover(
                    old,
                    None,
                    Some(HoverCard {
                        title: "old".into(),
                        body: "stale".into()
                    })
                ));
            })
        });
        let new = request(&mut runtime, &editor);
        assert_ne!(old.id, new.id);
        runtime.update(|cx| {
            editor.update(cx, |e, _| {
                e.hide_hover(); // Same invalidation used by Escape, wheel, editing, and IME.
                assert!(!e.resolve_hover(new, None, None));
                assert!(e.hover_card().is_none());
            })
        });
    }
    #[test]
    fn blank_space_hints_and_geometry_changes_do_not_attach_to_nearest_caret() {
        let (mut runtime, editor) = editor();
        for location in [point(5.3), point(13.), [20., 90.], [2., 20.], [-1., 20.]] {
            runtime.update(|cx| {
                editor.update(cx, |e, cx| {
                    e.hover(location, cx);
                    assert!(e.hover.active.is_none());
                })
            });
        }
        runtime.update(|cx| editor.update(cx, |e, cx| e.hover(point(1.), cx)));
        let target = request(&mut runtime, &editor);
        runtime.update(|cx| {
            editor.update(cx, |e, _| {
                e.geometry.borrow_mut().offset[0] = 20.;
                assert!(e.hover_request().is_none());
                assert!(!e.resolve_hover(target, None, None));
            })
        });
    }
    #[test]
    fn popup_enter_preserves_content_and_leaving_invalidates_pending_provider() {
        let (mut runtime, editor) = editor();
        runtime.update(|cx| editor.update(cx, |e, cx| e.hover(point(1.), cx)));
        let target = request(&mut runtime, &editor);
        runtime.update(|cx| {
            editor.update(cx, |e, cx| {
                e.resolve_hover(
                    target,
                    None,
                    Some(HoverCard {
                        title: "Rust".into(),
                        body: "alpha docs".into(),
                    }),
                );
                e.leave_hover(cx);
                assert!(e.hover_request().is_none());
                e.card_hover(true, cx);
                assert!(e.hover_card().is_some());
            })
        });
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            runtime.poll_tasks();
            std::thread::sleep(Duration::from_millis(5));
        }
        runtime.update(|cx| {
            editor.update(cx, |e, cx| {
                assert!(e.hover_card().is_some());
                e.card_hover(false, cx);
            })
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            runtime.poll_tasks();
            if runtime.update(|cx| editor.read(cx).hover_card().is_none()) {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn long_hover_is_bounded_selectable_and_keeps_its_own_scroll_area() {
        struct Measure;
        impl rxui::TextMeasure for Measure {
            fn measure(
                &mut self,
                _: rxui::ElementId,
                r: rxui::TextRequest<'_>,
            ) -> Result<[f32; 2], rxui::UiError> {
                let width = match r.width {
                    rxui::TextWidth::Available(w) => w.max(1.),
                    _ => 400.,
                };
                let rows = r
                    .text
                    .lines()
                    .map(|line| ((line.len() as f32 * 7.) / width).ceil().max(1.))
                    .sum::<f32>()
                    .max(1.);
                Ok([width, rows * 18.])
            }
        }
        let (mut runtime, editor) = editor();
        runtime.update(|cx| {
            editor.update(cx, |e, _| {
                e.request_hover_at_caret();
                let target = e.hover_request().unwrap();
                assert!(e.resolve_hover(
                    target,
                    None,
                    Some(HoverCard {
                        title: "Rust".into(),
                        body: "Long documentation line\n".repeat(100)
                    })
                ));
            })
        });
        let mut ui = rxui::Ui::new(&mut runtime, editor.clone()).unwrap();
        ui.prepare(&mut runtime, [400., 140.], &mut Measure)
            .unwrap();
        let key = rxui::Key::from("editor-hover-body");
        let bounds = ui.elements().find(|e| e.key == Some(&key)).unwrap().bounds;
        assert!(
            bounds.height >= 18. && bounds.height <= 40.,
            "body must have bounded usable height: {bounds:?}"
        );
        assert!(bounds.width <= 376.);
        let source_id = ui
            .elements()
            .find(|e| e.key == Some(&rxui::Key::from("editor-surface")))
            .unwrap()
            .id;
        assert_eq!(
            ui.hit_test([399., 139.]),
            Some(source_id),
            "popup backdrop must let source receive pointer input"
        );
        ui.pointer(&mut runtime, rxui::PointerEvent::Moved([399., 139.]))
            .unwrap();
        let inside = [bounds.x + bounds.width / 2., bounds.y + bounds.height / 2.];
        ui.pointer(&mut runtime, rxui::PointerEvent::Moved(inside))
            .unwrap();
        assert!(runtime.update(|cx| editor.read(cx).hover.in_card));
        assert!(runtime.update(|cx| editor.read(cx).hover_card().is_some()));
        ui.wheel(&mut runtime, inside, [0., 80.], Default::default())
            .unwrap();
        let scrolled = ui
            .elements()
            .find(|e| e.scroll_offset[1] > 0.)
            .expect("documentation should scroll")
            .id;
        assert_eq!(
            runtime.update(|cx| editor.read(cx).geometry.borrow().offset),
            [0., 0.]
        );
        runtime.update(|cx| editor.update(cx, |e, _| e.epoch += 1));
        ui.prepare(&mut runtime, [400., 140.], &mut Measure)
            .unwrap();
        assert!(
            ui.element(scrolled).unwrap().scroll_offset[1] > 0.,
            "view refresh must preserve documentation scroll"
        );
        assert_eq!(
            runtime.update(|cx| editor.read(cx).buffer.selection().head),
            0
        );
    }

    #[test]
    fn diagnostics_refresh_without_cancelling_a_symbol_request_and_merge_with_its_result() {
        let (mut runtime, editor) = editor();
        runtime.update(|cx| editor.update(cx, |e, cx| e.hover(point(1.), cx)));
        let target = request(&mut runtime, &editor);
        runtime.update(|cx| {
            editor.update(cx, |e, _| {
                e.set_annotations(crate::Annotations {
                    document_revision: 0,
                    spans: vec![
                        crate::StyledRange::new(0..5, Default::default())
                            .with_hover("Warning", "diagnostic detail"),
                    ],
                    overlays: vec![],
                })
                .unwrap();
                assert_eq!(e.hover_request(), Some(target));
                assert!(e.resolve_hover(
                    target,
                    None,
                    Some(HoverCard {
                        title: "Rust".into(),
                        body: "symbol documentation".into()
                    })
                ));
                let card = e.hover_card().unwrap().1;
                assert!(
                    card.body.contains("diagnostic detail")
                        && card.body.contains("symbol documentation")
                );
            })
        });
    }

    #[test]
    fn unicode_identifier_scan_is_bounded_and_keeps_combining_marks() {
        let text = ropey::Rope::from_str("fn café_世界é() {}");
        assert_eq!(text.slice(symbol_range(&text, 5)).to_string(), "café_世界é");
        let long = ropey::Rope::from_str(&"x".repeat(100_000));
        assert!(symbol_range(&long, 50_000).len() <= 512);
    }
}
