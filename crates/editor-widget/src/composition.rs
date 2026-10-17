//! Persistent view-only replacement; document text and undo history stay untouched.
use crate::{
    Annotations, Selection,
    surface::{Composition, Geometry, Surface},
    viewport::{ViewportState, ViewportSurface},
};
use rxui::{CustomElement, CustomPrepare, ElementInfo, UiError};
use std::{cell::RefCell, ops::Range, rc::Rc, sync::Arc};
use velk_editor_model::projection::{display_anchor, display_ranges};

pub(crate) struct ComposedGeometry {
    pub geometry: Rc<RefCell<Geometry>>,
    pub providers: Arc<Vec<usize>>,
    pub replaced: Range<usize>,
    pub inserted: usize,
}

pub(crate) struct ProjectedComposition {
    pub surface: ViewportSurface,
    pub state: ViewportState,
    revision: u64,
    selection: Selection,
    composition: Composition,
    annotations: Option<Arc<Annotations>>,
    providers: Arc<Vec<usize>>,
}
impl ProjectedComposition {
    pub fn new(source: &Surface) -> Self {
        let mut surface = source.clone();
        surface.geometry = Rc::new(RefCell::new(Geometry::default()));
        surface.composition = None;
        Self {
            surface: ViewportSurface(surface),
            state: ViewportState::default(),
            revision: u64::MAX,
            selection: Selection::default(),
            composition: Composition {
                text: String::new(),
                cursor: None,
            },
            annotations: None,
            providers: Arc::new(Vec::new()),
        }
    }
    pub fn prepare(
        &mut self,
        source: &Surface,
        element: &ElementInfo<'_>,
        cx: &mut CustomPrepare<'_>,
    ) -> Result<(), UiError> {
        #[cfg(any(test, feature = "performance"))]
        let started = std::time::Instant::now();
        let composition = source.composition.as_ref().unwrap();
        let replaced = source.selection.range();
        let inserted = composition.text.chars().count();
        let source_changed = self.revision != source.revision || self.selection != source.selection;
        let annotations_changed = match (&self.annotations, &source.annotations) {
            (None, None) => false,
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            _ => true,
        };
        if source_changed || self.composition.text != composition.text {
            let mut rope = source.rope.clone();
            rope.remove(replaced.clone());
            rope.insert(replaced.start, &composition.text);
            self.surface.0.rope = rope;
            self.surface.0.revision = self.surface.0.revision.wrapping_add(1);
        }
        if source_changed
            || annotations_changed
            || self.composition.text.chars().count() != inserted
        {
            let replacement = (replaced.clone(), inserted);
            let mut ids = Vec::new();
            self.surface.0.annotations = source.annotations.as_ref().map(|annotations| {
                let mut projected = Annotations {
                    document_revision: self.surface.0.revision,
                    ..Annotations::default()
                };
                for (id, span) in annotations.spans.iter().enumerate() {
                    for range in display_ranges(span.range.clone(), Some(&replacement)) {
                        let mut span = span.clone();
                        span.range = range;
                        projected.spans.push(span);
                        ids.push(id);
                    }
                }
                for (id, overlay) in annotations.overlays.iter().enumerate() {
                    if let Some(anchor) = display_anchor(overlay.anchor, Some(&replacement)) {
                        let mut overlay = overlay.clone();
                        overlay.anchor = anchor;
                        projected.overlays.push(overlay);
                        ids.push(annotations.spans.len() + id);
                    }
                }
                Arc::new(projected)
            });
            self.providers = Arc::new(ids);
            self.surface.0.annotation_groups = Some(self.providers.clone());
        }
        let cursor_byte = composition
            .cursor
            .map_or(composition.text.len(), |(_, end)| end);
        let cursor_byte = composition
            .text
            .floor_char_boundary(cursor_byte.min(composition.text.len()));
        let head = replaced.start + composition.text[..cursor_byte].chars().count();
        self.surface.0.syntax_demand = source
            .syntax_demand
            .as_ref()
            .map(|demand| demand.project(replaced.clone(), inserted));
        self.surface.0.syntax = source
            .syntax
            .as_ref()
            .map(|syntax| syntax.project(replaced.clone(), inserted));
        self.surface.0.selection = Selection { anchor: head, head };
        self.surface.0.caret = None;
        self.surface.0.regions = None;
        self.surface.0.preedit = Some(replaced.start..replaced.start + inserted);
        self.surface.0.caret_visible = source.caret_visible && composition.cursor.is_some();
        self.surface.0.epoch = source.epoch;
        #[cfg(any(test, feature = "performance"))]
        {
            self.surface.0.input_started = source.input_started;
        }
        self.surface.0.reveal_caret = source.reveal_caret;
        self.surface.0.selection_color = source.selection_color;
        self.surface.0.geometry.borrow_mut().offset = source.geometry.borrow().offset;
        self.revision = source.revision;
        self.selection = source.selection;
        self.composition = composition.clone();
        self.annotations = source.annotations.clone();
        #[cfg(any(test, feature = "performance"))]
        let projection_ms = started.elapsed().as_secs_f64() * 1000.;
        self.surface.prepare(&mut self.state, element, cx)?;
        let geometry = self.surface.0.geometry.borrow();
        let mut target = source.geometry.borrow_mut();
        target.layout = None;
        target.lines.clear();
        target.hover_regions.clear();
        target.rope = source.rope.clone();
        target.offset = geometry.offset;
        target.viewport = geometry.viewport;
        target.caret = geometry.caret;
        target.epoch = geometry.epoch;
        target.focused = geometry.focused;
        target.origin = geometry.origin;
        target.content_size = geometry.content_size;
        #[cfg(any(test, feature = "performance"))]
        {
            target.metrics = geometry.metrics;
            target.metrics.composition_projected = true;
            target.metrics.projection_ms = projection_ms;
            target.metrics.total_prepare_ms = started.elapsed().as_secs_f64() * 1000.;
            target.last_input = geometry.last_input;
        }
        target.composed = Some(ComposedGeometry {
            geometry: self.surface.0.geometry.clone(),
            providers: self.providers.clone(),
            replaced,
            inserted,
        });
        #[cfg(any(test, feature = "performance"))]
        {
            if std::env::var_os("VELK_PROFILE").is_some() {
                eprintln!("Velk projected frame: {:?}", target.metrics);
            }
        }
        Ok(())
    }
}

impl ComposedGeometry {
    pub fn hit(&self, point: [f32; 2]) -> Option<usize> {
        let position = self.geometry.borrow().hit(point)?;
        Some(if position < self.replaced.start {
            position
        } else if position <= self.replaced.start + self.inserted {
            self.replaced.start
        } else {
            position - self.inserted + self.replaced.len()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Editor, OverlayKind, OverlayPlacement, SpanStyle, StyledRange, TextOverlay};
    use rxui::{TestUi, astrelis::GraphicsContext, prelude::*};
    struct Reference(Surface);
    impl View for Reference {
        fn view(&self, _: &mut ViewContext<'_, Self>) -> impl IntoElement {
            let appearance = crate::EditorAppearance::default();
            column().fill_width().fill_height().child(
                rxui::custom(self.0.clone())
                    .key("reference")
                    .fill_width()
                    .fill_height()
                    .clip()
                    .background(appearance.background)
                    .color(appearance.foreground)
                    .text_input_client(true),
            )
        }
    }

    fn compare(
        source: &str,
        selection: Range<usize>,
        text: &str,
        annotations: Annotations,
        unicode_window: bool,
        graphics: &GraphicsContext,
    ) {
        let mut editor = Editor::new(source);
        editor.set_annotations(annotations).unwrap();
        editor.buffer.set_caret(selection.start, false);
        editor.buffer.set_caret(selection.end, true);
        editor.composition = Some(Composition {
            text: text.into(),
            cursor: Some((0, text.len())),
        });
        editor.edited();
        let mut actual = TestUi::new(graphics, [640, 240], editor).unwrap();
        actual.painter_mut().fonts_mut().load_system_fonts();
        let id = actual.element("editor-surface").unwrap().unwrap().id;
        actual.parts().1.focus(id);
        let pixels = actual.render().unwrap();
        let reference = actual.read(|e| Surface {
            syntax: None,
            syntax_demand: None,
            rope: e.buffer.text(),
            revision: e.buffer.revision(),
            selection: e.buffer.selection(),
            caret: None,
            selected_line_break: false,
            regions: None,
            composition: e.composition.clone(),
            preedit: None,
            geometry: Rc::new(RefCell::new(Geometry::default())),
            epoch: e.epoch,
            input_started: None,
            reveal_caret: true,
            caret_visible: true,
            minimum_line_metrics: None,
            annotations: e.annotations.clone(),
            annotation_groups: None,
            selection_color: e.appearance.selection,
        });
        let reference_geometry = reference.geometry.clone();
        let mut expected = TestUi::new(graphics, [640, 240], Reference(reference)).unwrap();
        expected.painter_mut().fonts_mut().load_system_fonts();
        let id = expected.element("reference").unwrap().unwrap().id;
        expected.parts().1.focus(id);
        let reference_pixels = expected.render().unwrap();
        let mismatched = pixels
            .iter()
            .zip(&reference_pixels)
            .filter(|(a, b)| a.abs_diff(**b) > 4)
            .count();
        if mismatched >= pixels.len() / 200 {
            actual.read(|e| {
                let g = e.geometry.borrow();
                let child = g.composed.as_ref().unwrap().geometry.borrow();
                eprintln!(
                    "actual: caret {:?}, offset {:?}, metrics {:?}",
                    g.caret, g.offset, g.metrics
                );
                for line in &child.lines {
                    eprintln!(
                        "actual line: top {}, {:?}",
                        line.top,
                        line.geometry.borrow().layout.as_ref().map(|l| l.lines())
                    );
                }
            });
            let reference = reference_geometry.borrow();
            eprintln!(
                "reference: caret {:?}, offset {:?}, lines {:?}",
                reference.caret,
                reference.offset,
                reference.layout.as_ref().unwrap().lines()
            );
        }
        assert!(
            mismatched < pixels.len() / 200,
            "{text:?}: {mismatched} channels differ from full-document paint"
        );
        actual.read(|e| {
            let geometry = e.geometry.borrow();
            let reference = reference_geometry.borrow();
            assert!(geometry.metrics.composition_projected);
            assert!(!geometry.metrics.composition_fallback);
            assert_eq!(
                geometry.metrics.unicode_horizontal_lines > 0,
                unicode_window
            );
            if unicode_window {
                assert!(geometry.metrics.cached_source_bytes < 4000);
            }
            let caret = geometry.caret_area();
            let other = reference.caret_area();
            assert!(
                (caret.x - other.x).abs() < 0.1,
                "caret x: {caret:?}, {other:?}"
            );
            assert!(
                (caret.y - other.y).abs() < 0.1,
                "caret y: {caret:?}, {other:?}"
            );
            for x in (16..620).step_by(7) {
                assert_eq!(
                    geometry.hit([x as f32, caret.y + caret.height / 2.]),
                    reference.hit([x as f32, other.y + other.height / 2.]),
                    "hit x={x}"
                );
            }
            for y in (18..200).step_by(12) {
                for x in (18..620).step_by(13) {
                    let hit = geometry.hover([x as f32, y as f32]);
                    let other = reference.hover([x as f32, y as f32]);
                    assert_eq!(
                        hit.as_ref().map(|h| (h.id, &h.card)),
                        other.as_ref().map(|h| (h.id, &h.card)),
                        "hover {x},{y}"
                    );
                }
            }
            assert_eq!(e.buffer.text().to_string(), source);
            assert_eq!(e.buffer.selection().range(), selection);
        });
    }

    #[test]
    fn windowed_preedit_paint_caret_and_hits_match_full_document_reference() {
        let graphics = pollster::block_on(GraphicsContext::headless()).unwrap();
        let source = format!("{}{}", "x".repeat(5000), "y".repeat(4000));
        for (text, windowed) in [
            ("日本語", true),
            ("e\u{301}👩‍💻", true),
            ("कर्म", true),
            ("\u{301}e", true),
            ("العربية", false),
        ] {
            let mut annotations = Annotations::default();
            annotations.spans.push(StyledRange::new(
                4998..5006,
                SpanStyle {
                    foreground: Some([0.2, 0.8, 0.3, 1.]),
                    background: Some([0.2, 0.2, 0.4, 0.4]),
                    ..SpanStyle::default()
                },
            ));
            compare(&source, 5000..5003, text, annotations, windowed, &graphics);
        }
    }

    #[test]
    fn multiline_projection_with_rich_hints_matches_full_document_reference() {
        let graphics = pollster::block_on(GraphicsContext::headless()).unwrap();
        let source = "ab café xyz\n".repeat(40);
        let mut annotations = Annotations::default();
        annotations.spans.push(
            StyledRange::new(
                0..2,
                SpanStyle {
                    font_size: Some(32.),
                    ..SpanStyle::default()
                },
            )
            .with_hover("large", "preserved source provider"),
        );
        annotations.spans.push(
            StyledRange::new(
                0..source.chars().count(),
                SpanStyle {
                    background: Some([0.1, 0.2, 0.3, 0.3]),
                    full_line_background: true,
                    ..SpanStyle::default()
                },
            )
            .with_hover("background", "split source provider"),
        );
        for anchor in [2, 3, 35 * 12 + 7] {
            annotations.overlays.push(TextOverlay {
                anchor,
                text: "hint".into(),
                kind: OverlayKind::InlayHint,
                placement: OverlayPlacement::Inline,
                foreground: [0.5, 0.7, 1., 1.],
                background: None,
                hover: Some(crate::HoverCard {
                    title: "hint".into(),
                    body: format!("anchor {anchor}"),
                }),
            });
        }
        compare(
            &source,
            3..35 * 12 + 4,
            "かんじ",
            annotations.clone(),
            false,
            &graphics,
        );
        compare(
            &source,
            3..35 * 12 + 4,
            "漢\n\n字",
            annotations,
            false,
            &graphics,
        );
    }
}
