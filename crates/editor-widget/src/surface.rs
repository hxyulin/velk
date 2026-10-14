use crate::inline::InlineProjection;
use crate::{
    Annotations, Color, HoverCard, LinePattern, OverlayKind, OverlayPlacement, Selection,
    Underline,
    annotations::{display_anchor, display_ranges},
};
use ropey::Rope;
use rxui::astrelis::{
    FontFamily, FontSlant, LineDraw, PaintSession, PreparedText, Rect, TextBuffer, TextDraw,
    TextLayout, TextPosition, TextRasterOptions, TextStyle, TextWrap,
};
use rxui::{Bounds, CustomElement, CustomPrepare, ElementInfo, UiError};
use std::{cell::RefCell, ops::Range, rc::Rc, sync::Arc};

pub(crate) const PADDING: f32 = 16.;
pub(crate) const LINE_HEIGHT: f32 = 24.;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Composition {
    pub text: String,
    pub cursor: Option<(usize, usize)>,
}

/// The shaped snapshot used by pointer events and the IME candidate window.
#[derive(Default)]
pub(crate) struct Geometry {
    pub(super) layout: Option<Arc<TextLayout>>,
    pub(super) rope: Rope,
    pub(super) replacement: Option<(Range<usize>, usize)>,
    pub(super) projection: InlineProjection,
    pub(super) offset: [f32; 2],
    pub(super) viewport: [f32; 2],
    pub(super) caret: Option<Rect>,
    pub epoch: Option<u64>,
    pub focused: bool,
    pub(super) origin: [f32; 2],
    pub(super) content_size: [f32; 2],
    pub(super) hover_regions: Vec<HoverRegion>,
    pub(super) lines: Vec<crate::viewport::LineHit>,
    pub(super) metrics: crate::viewport::FrameMetrics,
    pub(super) last_input: Option<std::time::Instant>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HoverHit {
    pub id: usize,
    pub card: HoverCard,
    pub anchor: [f32; 2],
}

#[derive(Clone)]
pub(super) struct HoverRegion {
    pub(super) id: usize,
    pub(super) rect: Rect,
    pub(super) card: HoverCard,
}

impl Geometry {
    pub fn hover(&self, point: [f32; 2]) -> Option<HoverHit> {
        if point[0] < 0.
            || point[1] < 0.
            || point[0] >= self.viewport[0]
            || point[1] >= self.viewport[1]
        {
            return None;
        }
        let local = [
            point[0] - PADDING + self.offset[0],
            point[1] - PADDING + self.offset[1],
        ];
        self.hover_regions
            .iter()
            .rev()
            .find(|hit| {
                local[0] >= hit.rect.x
                    && local[0] < hit.rect.x + hit.rect.width
                    && local[1] >= hit.rect.y
                    && local[1] < hit.rect.y + hit.rect.height
            })
            .map(|hit| HoverHit {
                id: hit.id,
                card: hit.card.clone(),
                anchor: [
                    self.origin[0] + PADDING + hit.rect.x - self.offset[0],
                    self.origin[1] + PADDING + hit.rect.y + hit.rect.height - self.offset[1],
                ],
            })
    }

    pub fn caret_area(&self) -> Bounds {
        let caret = self.caret.unwrap_or(Rect::new(0., 0., 1., LINE_HEIGHT));
        Bounds {
            x: (PADDING + caret.x - self.offset[0]).clamp(0., self.viewport[0].max(0.)),
            y: (PADDING + caret.y - self.offset[1]).clamp(0., self.viewport[1].max(0.)),
            width: 1.,
            height: caret.height,
        }
    }

    pub fn hit(&self, point: [f32; 2]) -> Option<usize> {
        if !self.lines.is_empty() {
            let y = point[1] - PADDING + self.offset[1];
            let index = self
                .lines
                .partition_point(|line| line.top <= y)
                .saturating_sub(1);
            let line = &self.lines[index];
            return line
                .geometry
                .borrow()
                .hit([
                    point[0] + self.offset[0],
                    point[1] + self.offset[1] - line.top,
                ])
                .map(|position| line.start_char + position);
        }
        let layout = self.layout.as_ref()?;
        let byte = layout
            .hit_test([
                point[0] - PADDING + self.offset[0],
                point[1] - PADDING + self.offset[1],
            ])?
            .byte_offset;
        let byte = self.projection.base_offset(byte);
        // Map the transient preedit display back into the unchanged document.
        let byte = match &self.replacement {
            Some((range, length)) if byte >= range.start => {
                if byte <= range.start + length {
                    range.start
                } else {
                    byte - length + range.len()
                }
            }
            _ => byte,
        };
        Some(self.rope.byte_to_char(byte.min(self.rope.len_bytes())))
    }

    pub fn scroll(&mut self, delta: [f32; 2]) {
        for (offset, delta) in self.offset.iter_mut().zip(delta) {
            *offset = (*offset + delta).max(0.);
        }
        self.clamp_scroll();
    }

    pub(super) fn clamp_scroll(&mut self) {
        let layout_size = self.layout.as_ref().map_or([0.; 2], |layout| layout.size());
        for (axis, size) in layout_size.into_iter().enumerate() {
            let maximum =
                (size.max(self.content_size[axis]) + PADDING * 2. - self.viewport[axis]).max(0.);
            self.offset[axis] = self.offset[axis].clamp(0., maximum);
        }
    }

    pub(super) fn reveal(&mut self) {
        if let Some(caret) = self.caret {
            for (axis, (start, length)) in [(caret.x, caret.width), (caret.y, caret.height)]
                .into_iter()
                .enumerate()
            {
                let space = (self.viewport[axis] - PADDING * 2.).max(LINE_HEIGHT);
                if start < self.offset[axis] {
                    self.offset[axis] = start;
                } else if start + length > self.offset[axis] + space {
                    self.offset[axis] = start + length - space;
                }
            }
        }
        self.clamp_scroll();
    }
}

pub(crate) struct Surface {
    pub rope: Rope,
    pub revision: u64,
    pub selection: Selection,
    pub composition: Option<Composition>,
    pub geometry: Rc<RefCell<Geometry>>,
    pub epoch: u64,
    pub input_started: Option<std::time::Instant>,
    pub reveal_caret: bool,
    pub caret_visible: bool,
    pub annotations: Option<Arc<Annotations>>,
    pub selection_color: Color,
}

impl PartialEq for Surface {
    fn eq(&self, other: &Self) -> bool {
        self.epoch == other.epoch
            && self.caret_visible == other.caret_visible
            && Rc::ptr_eq(&self.geometry, &other.geometry)
    }
}

#[derive(Default)]
pub(crate) struct SurfaceState {
    buffer: TextBuffer,
    revision: Option<u64>,
    composition: Option<Composition>,
    selection: Selection,
    prepared: Option<PreparedText>,
    layout: Option<Arc<TextLayout>>,
    raster_scale: f32,
    caret: Option<Rect>,
    selection_rects: Vec<Rect>,
    composition_rects: Vec<Rect>,
    annotations: Option<Arc<Annotations>>,
    decorations: DecorationCache,
    projection: InlineProjection,
    shaped_annotations: Option<Arc<Annotations>>,
    shaped_selection: Selection,
    prepared_layout: Option<Arc<TextLayout>>,
}

impl SurfaceState {
    pub(super) fn project_caret(&self, byte: usize) -> usize {
        self.projection.display_offset(byte, true)
    }
    pub(super) fn raster_scale(&self) -> f32 {
        self.raster_scale
    }
}

impl Surface {
    pub(super) fn shape(
        &self,
        state: &mut SurfaceState,
        width: f32,
        fonts: &mut rxui::astrelis::TextSystem,
    ) -> Result<bool, UiError> {
        let range = self.rope.char_to_byte(self.selection.range().start)
            ..self.rope.char_to_byte(self.selection.range().end);
        let replacement = self
            .composition
            .as_ref()
            .map(|c| (range.clone(), c.text.len()));
        let composition_changed = state.composition != self.composition;
        if state.revision != Some(self.revision)
            || composition_changed
            || self.composition.is_some() && state.shaped_selection != self.selection
            || !same_annotations(&state.shaped_annotations, &self.annotations)
        {
            let mut text = self.rope.to_string();
            if let Some(composition) = &self.composition {
                text.replace_range(range.clone(), &composition.text);
            }
            let hints = self
                .annotations
                .iter()
                .flat_map(|a| a.overlays.iter().enumerate())
                .filter(|(_, overlay)| overlay.placement == OverlayPlacement::Inline)
                .filter_map(|(index, overlay)| {
                    display_anchor(self.rope.char_to_byte(overlay.anchor), replacement.as_ref())
                        .map(|anchor| (anchor, index, overlay.text.as_str()))
                })
                .collect::<Vec<_>>();
            let (text, projection) = if hints.is_empty() {
                (text, InlineProjection::default())
            } else {
                InlineProjection::build(&text, hints)
            };
            let mut spans = self
                .annotations
                .iter()
                .flat_map(|a| &a.spans)
                .flat_map(|span| {
                    let source = self.rope.char_to_byte(span.range.start)
                        ..self.rope.char_to_byte(span.range.end);
                    display_ranges(source, replacement.as_ref())
                        .into_iter()
                        .flat_map(|range| projection.ranges(range))
                        .filter_map(|range| span.style.text_span(range))
                })
                .collect::<Vec<_>>();
            if let Some(annotations) = &self.annotations {
                for hint in &projection.insertions {
                    let overlay = &annotations.overlays[hint.overlay_index];
                    spans.push(
                        rxui::astrelis::TextSpan::new(hint.display.clone())
                            .font_size(12.)
                            .line_height(LINE_HEIGHT)
                            .weight(400)
                            .slant(if overlay.kind == OverlayKind::GhostText {
                                FontSlant::Italic
                            } else {
                                FontSlant::Normal
                            })
                            .color(overlay.foreground),
                    );
                }
            }
            state
                .buffer
                .set_rich_text(&text, editor_text_style(), spans)?;
            state.projection = projection;
            state.buffer.set_wrap(TextWrap::None);
            state.revision = Some(self.revision);
            state.composition = self.composition.clone();
        }
        state.shaped_selection = self.selection;
        state.shaped_annotations = self.annotations.clone();
        state.buffer.set_width(Some(width))?;
        let layout = state.buffer.layout(fonts)?;
        layout.prepare_interaction()?;
        let layout_changed = state
            .layout
            .as_ref()
            .is_none_or(|old| !Arc::ptr_eq(old, &layout));
        state.layout = Some(layout);
        Ok(layout_changed)
    }
    pub(super) fn layout<'a>(&self, state: &'a SurfaceState) -> &'a Arc<TextLayout> {
        state.layout.as_ref().unwrap()
    }
}

impl CustomElement for Surface {
    type State = SurfaceState;

    fn prepare(
        &self,
        state: &mut SurfaceState,
        element: &ElementInfo<'_>,
        cx: &mut CustomPrepare<'_>,
    ) -> Result<(), UiError> {
        let range = self.rope.char_to_byte(self.selection.range().start)
            ..self.rope.char_to_byte(self.selection.range().end);
        let mut replacement = None;
        if let Some(composition) = &self.composition {
            replacement = Some((range.clone(), composition.text.len()));
        }
        let composition_changed = state.composition != self.composition;
        let selection_changed = state.selection != self.selection;
        let annotations_changed = !same_annotations(&state.annotations, &self.annotations);
        let mut layout_changed = self.shape(
            state,
            (element.bounds.width - PADDING * 2.).max(0.),
            cx.fonts,
        )?;
        let layout = state.layout.as_ref().unwrap().clone();
        layout_changed |= state
            .prepared_layout
            .as_ref()
            .is_none_or(|old| !Arc::ptr_eq(old, &layout));
        state.selection = self.selection;
        let raster_changed = state.raster_scale != cx.raster_scale;
        if layout_changed || raster_changed {
            state.prepared = Some(cx.painter.prepare_text(
                &layout,
                TextRasterOptions::new().raster_scale(cx.raster_scale),
            )?);
            state.raster_scale = cx.raster_scale;
        }
        let caret_byte = match &self.composition {
            Some(composition) => {
                range.start
                    + composition
                        .cursor
                        .map_or(composition.text.len(), |(_, end)| end)
            }
            None => self.rope.char_to_byte(self.selection.head),
        };
        // IME cursor byte offsets need not themselves be grapheme boundaries.
        let caret_byte =
            boundary_before(&layout, state.projection.display_offset(caret_byte, true));
        state.caret = layout
            .caret(TextPosition::new(caret_byte))
            .map(|caret| Rect::new(caret.origin[0], caret.origin[1], 1.5, caret.height));
        let candidate_caret = state.caret;
        if self
            .composition
            .as_ref()
            .is_some_and(|c| c.cursor.is_none())
        {
            state.caret = None;
        }
        if layout_changed || selection_changed || composition_changed || annotations_changed {
            state.selection_rects.clear();
            state.composition_rects.clear();
            if let Some(composition) = &self.composition {
                let base_range = range.start..range.start + composition.text.len();
                for projected in state.projection.ranges(base_range) {
                    let start = boundary_before(&layout, projected.start);
                    let end = boundary_after(&layout, projected.end);
                    state
                        .composition_rects
                        .extend(layout.selection_rects(start..end)?);
                }
            } else if !range.is_empty() {
                for projected in state.projection.ranges(range) {
                    state
                        .selection_rects
                        .extend(layout.selection_rects(projected)?);
                }
            }
        }
        let decorations_changed = layout_changed || annotations_changed || raster_changed;
        if decorations_changed {
            state.decorations =
                prepare_annotations(self, &layout, replacement.as_ref(), &state.projection, cx)?;
            state.annotations = self.annotations.clone();
        }
        let mut geometry = self.geometry.borrow_mut();
        let viewport = [element.bounds.width, element.bounds.height];
        let resized = geometry.viewport != viewport;
        geometry.viewport = viewport;
        geometry.origin = [element.bounds.x, element.bounds.y];
        geometry.content_size = state.decorations.content_size;
        if decorations_changed {
            geometry.hover_regions = state.decorations.hover_regions.clone();
            geometry.projection = state.projection.clone();
        }
        geometry.layout = Some(layout.clone());
        geometry.rope = self.rope.clone();
        geometry.replacement = replacement;
        geometry.caret = candidate_caret;
        geometry.focused = element.focused;
        if (geometry.epoch != Some(self.epoch) && self.reveal_caret) || resized {
            geometry.reveal();
        }
        geometry.clamp_scroll();
        geometry.epoch = Some(self.epoch);
        state.prepared_layout = Some(layout.clone());
        state.layout = Some(layout);
        Ok(())
    }

    fn paint(
        &self,
        state: &SurfaceState,
        element: &ElementInfo<'_>,
        paint: &mut PaintSession<'_, '_>,
    ) -> Result<(), UiError> {
        let geometry = self.geometry.borrow();
        let origin = [
            element.bounds.x + PADDING - geometry.offset[0],
            element.bounds.y + PADDING - geometry.offset[1],
        ];
        self.paint_at(state, element, paint, origin)
    }
}

impl Surface {
    pub(super) fn paint_at(
        &self,
        state: &SurfaceState,
        element: &ElementInfo<'_>,
        paint: &mut PaintSession<'_, '_>,
        origin: [f32; 2],
    ) -> Result<(), UiError> {
        for background in &state.decorations.backgrounds {
            let rect = if background.full_line {
                Rect::new(
                    element.bounds.x,
                    origin[1] + background.rect.y,
                    element.bounds.width,
                    background.rect.height,
                )
            } else {
                translated(background.rect, origin)
            };
            if intersects(rect, element.clip_bounds) {
                paint.fill_rect(rect, background.color)?;
            }
        }
        for rect in &state.selection_rects {
            paint.fill_rect(translated(*rect, origin), self.selection_color)?;
        }
        if let Some(prepared) = &state.prepared {
            paint.draw_text(prepared, TextDraw::new(origin).color(element.color))?;
        }
        for line in &state.decorations.lines {
            paint_decoration(paint, line, origin, element.clip_bounds)?;
        }
        for overlay in &state.decorations.overlays {
            let rect = translated(overlay.rect, origin);
            if !intersects(rect, element.clip_bounds) {
                continue;
            }
            if let Some(color) = overlay.background {
                paint.fill_rounded_rect(rect, 4., color)?;
            }
            paint.draw_text(
                &overlay.prepared,
                TextDraw::new([rect.x + 6., rect.y + 3.]).color(overlay.foreground),
            )?;
        }
        for rect in &state.composition_rects {
            paint.fill_rect(
                Rect::new(
                    origin[0] + rect.x,
                    origin[1] + rect.y + rect.height - 2.,
                    rect.width,
                    1.,
                ),
                element.color,
            )?;
        }
        if element.focused
            && self.caret_visible
            && let Some(caret) = state.caret
        {
            paint.fill_rect(translated(caret, origin), element.color)?;
        }
        Ok(())
    }
}

fn translated(rect: Rect, origin: [f32; 2]) -> Rect {
    Rect::new(
        rect.x + origin[0],
        rect.y + origin[1],
        rect.width,
        rect.height,
    )
}

fn boundary_before(layout: &TextLayout, byte: usize) -> usize {
    let byte = byte.min(layout.text().len());
    if layout.is_text_boundary(byte) {
        byte
    } else {
        layout.previous_boundary(byte).unwrap_or(0)
    }
}

fn boundary_after(layout: &TextLayout, byte: usize) -> usize {
    let byte = byte.min(layout.text().len());
    if layout.is_text_boundary(byte) {
        byte
    } else {
        layout.next_boundary(byte).unwrap_or(layout.text().len())
    }
}

fn editor_text_style() -> TextStyle {
    // Named families keep Cosmic Text from requiring monospaced fallback faces
    // for CJK and emoji, which can produce nonfinite advances on macOS.
    let family = if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    };
    TextStyle::new()
        .family(FontFamily::from(family))
        .font_size(16.)
        .line_height(LINE_HEIGHT)
        .feature(*b"liga", 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxui::astrelis::TextSystem;

    #[test]
    fn editor_fonts_shape_unicode_and_supply_selection_geometry() {
        let mut fonts = TextSystem::new();
        if fonts.load_system_fonts().is_empty() {
            // Headless CI may have no installed fonts. Buffer/input tests remain
            // font-independent; run this check on a desktop for native coverage.
            return;
        }
        let mut buffer = TextBuffer::new();
        buffer.set_wrap(TextWrap::None);
        buffer.set_width(Some(640.)).unwrap();
        for text in ["", "Hello", "café", "世界", "👩‍💻", "a\n\nb"] {
            buffer.set_text(text, editor_text_style()).unwrap();
            let layout = buffer.layout(&mut fonts).unwrap();
            let caret = layout.caret(TextPosition::new(text.len())).unwrap();
            assert!(caret.origin.into_iter().all(f32::is_finite));
            assert!(layout.hit_test(caret.origin).is_some());
            assert!(layout.selection_rects(0..text.len()).is_ok());
        }
    }

    #[test]
    fn scrolling_reveals_carets_and_clamps_to_document_bounds() {
        let mut fonts = TextSystem::new();
        let mut buffer = TextBuffer::new();
        buffer
            .set_text(&"\n".repeat(100), editor_text_style())
            .unwrap();
        let layout = buffer.layout(&mut fonts).unwrap();
        let mut geometry = Geometry {
            layout: Some(layout),
            viewport: [640., 100.],
            caret: Some(Rect::new(0., 2400., 1., 24.)),
            ..Geometry::default()
        };
        geometry.reveal();
        assert!(geometry.offset[1] > 2300.);
        assert!(geometry.caret_area().y + geometry.caret_area().height <= 100.);
        geometry.scroll([1000., -10000.]);
        assert_eq!(geometry.offset, [0., 0.]);
    }
}

#[derive(Default)]
struct DecorationCache {
    backgrounds: Vec<Background>,
    lines: Vec<DecorationLine>,
    overlays: Vec<PreparedOverlay>,
    hover_regions: Vec<HoverRegion>,
    content_size: [f32; 2],
}
struct Background {
    rect: Rect,
    color: Color,
    full_line: bool,
}
struct DecorationLine {
    rect: Rect,
    underline: Underline,
    overline: bool,
}
struct PreparedOverlay {
    rect: Rect,
    prepared: PreparedText,
    foreground: Color,
    background: Option<Color>,
}

fn same_annotations(a: &Option<Arc<Annotations>>, b: &Option<Arc<Annotations>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn prepare_annotations(
    surface: &Surface,
    layout: &TextLayout,
    replacement: Option<&(std::ops::Range<usize>, usize)>,
    projection: &InlineProjection,
    cx: &mut CustomPrepare<'_>,
) -> Result<DecorationCache, UiError> {
    let mut cache = DecorationCache {
        content_size: layout.size(),
        ..DecorationCache::default()
    };
    let Some(annotations) = &surface.annotations else {
        return Ok(cache);
    };
    for (id, span) in annotations.spans.iter().enumerate() {
        let mut background_lines = std::collections::HashSet::new();
        let source =
            surface.rope.char_to_byte(span.range.start)..surface.rope.char_to_byte(span.range.end);
        for range in display_ranges(source, replacement)
            .into_iter()
            .flat_map(|range| projection.ranges(range))
        {
            let range = boundary_before(layout, range.start)..boundary_after(layout, range.end);
            if let Some(color) = span.style.background
                && span.style.full_line_background
            {
                // Include blank and newline-only lines, which have no selection ink.
                let start = layout
                    .caret(TextPosition::new(range.start))
                    .map_or(0, |c| c.line_index);
                let end = layout
                    .caret(TextPosition::new(range.end))
                    .map_or(start, |c| c.line_index);
                for (index, line) in layout.lines().iter().enumerate().take(end + 1).skip(start) {
                    if index == end && range.end == line.source.start && index > start {
                        continue;
                    }
                    if !background_lines.insert(index) {
                        continue;
                    }
                    cache.backgrounds.push(Background {
                        rect: Rect::new(0., line.top, line.width, line.height),
                        color,
                        full_line: true,
                    });
                }
            }
            for rect in layout.selection_rects(range)? {
                if let Some(color) = span.style.background
                    && !span.style.full_line_background
                {
                    cache.backgrounds.push(Background {
                        rect,
                        color,
                        full_line: false,
                    });
                }
                if let Some(underline) = span.style.underline {
                    cache.lines.push(DecorationLine {
                        rect,
                        underline,
                        overline: false,
                    });
                }
                if let Some(color) = span.style.overline {
                    cache.lines.push(DecorationLine {
                        rect,
                        underline: Underline {
                            color,
                            pattern: LinePattern::Solid,
                        },
                        overline: true,
                    });
                }
                if let Some(card) = &span.hover {
                    cache.hover_regions.push(HoverRegion {
                        id,
                        rect,
                        card: card.clone(),
                    });
                }
            }
        }
    }
    let mut line_ends = layout
        .lines()
        .iter()
        .map(|line| line.left + line.width)
        .collect::<Vec<_>>();
    for (index, overlay) in annotations.overlays.iter().enumerate() {
        if overlay.placement == OverlayPlacement::Inline {
            if let Some(hint) = projection
                .insertions
                .iter()
                .find(|h| h.overlay_index == index)
            {
                for rect in layout.selection_rects(hint.display.clone())? {
                    if let Some(color) = overlay.background {
                        cache.backgrounds.push(Background {
                            rect,
                            color,
                            full_line: false,
                        });
                    }
                    if let Some(card) = &overlay.hover {
                        cache.hover_regions.push(HoverRegion {
                            id: annotations.spans.len() + index,
                            rect,
                            card: card.clone(),
                        });
                    }
                }
            }
            continue;
        }
        let Some(byte) = display_anchor(surface.rope.char_to_byte(overlay.anchor), replacement)
        else {
            continue;
        };
        let Some(caret) = layout.caret(TextPosition::new(boundary_after(
            layout,
            projection.display_offset(byte, true),
        ))) else {
            continue;
        };
        let line = &layout.lines()[caret.line_index];
        let mut buffer = TextBuffer::new();
        let style = editor_text_style().font_size(12.).line_height(18.).slant(
            if overlay.kind == OverlayKind::GhostText {
                FontSlant::Italic
            } else {
                FontSlant::Normal
            },
        );
        buffer.set_text(&overlay.text, style)?;
        buffer.set_wrap(TextWrap::None);
        buffer.set_width(Some(4096.))?;
        let text = buffer.layout(cx.fonts)?;
        let size = text.size();
        let x = match overlay.placement {
            OverlayPlacement::LineEnd => line_ends[caret.line_index] + 16.,
            OverlayPlacement::AtAnchor => caret.origin[0],
            OverlayPlacement::Inline => {
                unreachable!("inline text is already in the projected layout")
            }
        };
        let rect = Rect::new(
            x,
            line.top + (line.height - size[1] - 6.).max(0.) / 2.,
            size[0] + 12.,
            size[1] + 6.,
        );
        line_ends[caret.line_index] = line_ends[caret.line_index].max(rect.x + rect.width);
        cache.content_size[0] = cache.content_size[0].max(rect.x + rect.width);
        cache.content_size[1] = cache.content_size[1].max(rect.y + rect.height);
        let prepared = cx.painter.prepare_text(
            &text,
            TextRasterOptions::new().raster_scale(cx.raster_scale),
        )?;
        cache.overlays.push(PreparedOverlay {
            rect,
            prepared,
            foreground: overlay.foreground,
            background: overlay.background,
        });
        if let Some(card) = &overlay.hover {
            cache.hover_regions.push(HoverRegion {
                id: annotations.spans.len() + index,
                rect,
                card: card.clone(),
            });
        }
    }
    Ok(cache)
}

fn intersects(rect: Rect, clip: Bounds) -> bool {
    rect.x < clip.x + clip.width
        && rect.x + rect.width > clip.x
        && rect.y < clip.y + clip.height
        && rect.y + rect.height > clip.y
}

fn paint_decoration(
    paint: &mut PaintSession<'_, '_>,
    line: &DecorationLine,
    origin: [f32; 2],
    clip: Bounds,
) -> Result<(), UiError> {
    let rect = translated(line.rect, origin);
    if !intersects(rect, clip) {
        return Ok(());
    }
    let y = if line.overline {
        rect.y + 2.
    } else {
        rect.y + rect.height - 3.
    };
    let left = rect.x.max(clip.x);
    let right = (rect.x + rect.width).min(clip.x + clip.width);
    let color = line.underline.color;
    match line.underline.pattern {
        LinePattern::Solid => {
            paint.fill_rect(Rect::new(left, y, (right - left).max(0.), 1.), color)?
        }
        LinePattern::Dashed => {
            let mut x = rect.x + ((left - rect.x) / 6.).floor() * 6.;
            while x < right {
                let begin = x.max(left);
                let end = (x + 3.).min(right);
                if begin < end {
                    paint.fill_rect(Rect::new(begin, y, end - begin, 1.), color)?;
                }
                x += 6.;
            }
        }
        LinePattern::Wavy => {
            // Generate only visible wave segments; a long decorated line cannot
            // allocate or issue draws for millions of offscreen pixels.
            let mut x = rect.x + ((left - rect.x) / 8.).floor() * 8.;
            let mut draws = Vec::new();
            while x < right {
                draws.push(LineDraw::new([x, y], [x + 4., y + 2.], color));
                draws.push(LineDraw::new([x + 4., y + 2.], [x + 8., y], color));
                x += 8.;
            }
            paint.draw_lines(&draws)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod rich_tests {
    use super::*;
    use crate::{SpanStyle, annotations::display_ranges};
    use rxui::astrelis::TextSystem;

    #[test]
    fn rich_layout_reuses_cached_snapshot_and_changes_mixed_size_geometry() {
        let mut fonts = TextSystem::new();
        if fonts.load_system_fonts().is_empty() {
            return;
        }
        let mut buffer = TextBuffer::new();
        buffer.set_width(Some(640.)).unwrap();
        buffer.set_wrap(TextWrap::None);
        let text = "large café\nsmall";
        let span = SpanStyle {
            font_size: Some(32.),
            italic: Some(true),
            weight: Some(700),
            foreground: Some([0.2, 0.6, 0.9, 1.]),
            strikethrough: Some(true),
            ..SpanStyle::default()
        }
        .text_span(0..5)
        .unwrap();
        buffer
            .set_rich_text(text, editor_text_style(), vec![span])
            .unwrap();
        let layout = buffer.layout(&mut fonts).unwrap();
        assert!(layout.lines()[0].height >= 48.);
        assert!(layout.lines()[1].height < layout.lines()[0].height);
        assert!(!layout.decorations().is_empty());
        let caret = layout.caret(TextPosition::new(5)).unwrap();
        assert_eq!(
            layout
                .hit_test([caret.origin[0], caret.origin[1] + caret.height / 2.])
                .unwrap()
                .byte_offset,
            5
        );
        let cached = buffer.layout(&mut fonts).unwrap();
        assert!(Arc::ptr_eq(&layout, &cached));
        assert!(layout.selection_rects(0..text.len()).unwrap().count() >= 2);
        // A composition replacement can be projected without styling its text.
        assert_eq!(display_ranges(0..10, Some(&(6..10, 3))), vec![0..6]);
    }

    #[test]
    fn hover_uses_real_ink_regions_with_scroll_and_parent_offsets() {
        let geometry = Geometry {
            viewport: [200., 100.],
            origin: [10., 80.],
            offset: [30., 100.],
            hover_regions: vec![HoverRegion {
                id: 7,
                rect: Rect::new(50., 120., 40., 24.),
                card: HoverCard {
                    title: "type".into(),
                    body: "i32".into(),
                },
            }],
            ..Geometry::default()
        };
        let hit = geometry.hover([40., 40.]).unwrap();
        assert_eq!(hit.id, 7);
        assert_eq!(hit.anchor, [46., 140.]);
        assert!(
            geometry.hover([100., 40.]).is_none(),
            "nearest text position must not invent a hover outside its actual range"
        );
        assert!(geometry.hover([40., -10.]).is_none());
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;
    use rxui::astrelis::{TextSpan, TextSystem};

    fn layout_with_hint(
        base: &str,
        anchor: usize,
        label: &str,
    ) -> Option<(Arc<TextLayout>, InlineProjection, Arc<TextLayout>)> {
        let mut fonts = TextSystem::new();
        if fonts.load_system_fonts().is_empty() {
            return None;
        }
        let mut buffer = TextBuffer::new();
        buffer.set_width(Some(640.)).unwrap();
        buffer.set_wrap(TextWrap::None);
        buffer.set_text(base, editor_text_style()).unwrap();
        let original = buffer.layout(&mut fonts).unwrap();
        let (text, projection) = InlineProjection::build(base, vec![(anchor, 0, label)]);
        buffer
            .set_rich_text(
                &text,
                editor_text_style(),
                vec![
                    TextSpan::new(projection.insertions[0].display.clone())
                        .font_size(12.)
                        .line_height(LINE_HEIGHT),
                ],
            )
            .unwrap();
        let layout = buffer.layout(&mut fonts).unwrap();
        Some((layout, projection, original))
    }

    #[test]
    fn inline_layout_shifts_following_source_and_hit_testing_maps_to_document() {
        let source = "let count = 42;";
        let Some((layout, projection, original)) = layout_with_hint(source, 9, ": i32") else {
            return;
        };
        assert!(layout.size()[0] > original.size()[0]);
        let geometry = Geometry {
            layout: Some(layout.clone()),
            rope: Rope::from_str(source),
            projection: projection.clone(),
            viewport: [640., 480.],
            ..Geometry::default()
        };
        let hint = layout
            .selection_rects(projection.insertions[0].display.clone())
            .unwrap()
            .next()
            .unwrap();
        assert_eq!(
            geometry.hit([
                PADDING + hint.x + hint.width / 2.,
                PADDING + hint.y + hint.height / 2.
            ]),
            Some(9)
        );
        for byte in [0, 8, 9, 10, source.len()] {
            let caret = layout
                .caret(TextPosition::new(projection.display_offset(byte, true)))
                .unwrap();
            assert_eq!(
                geometry.hit([
                    PADDING + caret.origin[0],
                    PADDING + caret.origin[1] + caret.height / 2.
                ]),
                Some(byte)
            );
        }
        let selection = projection
            .ranges(0..source.len())
            .into_iter()
            .flat_map(|r| layout.selection_rects(r).unwrap())
            .collect::<Vec<_>>();
        assert!(
            selection
                .iter()
                .all(|r| r.x + r.width <= hint.x + 0.01 || r.x >= hint.x + hint.width - 0.01)
        );
    }

    #[test]
    fn inline_mapping_composes_with_ime_replacement_and_unicode() {
        let source = "é count = 世界;";
        let base = "你 count = 世界;";
        // IME replaces source é (two bytes) with 你 (three bytes). The hint is
        // after count, shifted by that one-byte difference in the display base.
        let anchor = "你 count".len();
        let Some((layout, projection, _)) = layout_with_hint(base, anchor, ": 类型") else {
            return;
        };
        let geometry = Geometry {
            layout: Some(layout.clone()),
            rope: Rope::from_str(source),
            projection: projection.clone(),
            replacement: Some((0..2, 3)),
            viewport: [640., 480.],
            ..Geometry::default()
        };
        let hint = layout
            .selection_rects(projection.insertions[0].display.clone())
            .unwrap()
            .next()
            .unwrap();
        assert_eq!(
            geometry.hit([
                PADDING + hint.x + hint.width / 2.,
                PADDING + hint.y + hint.height / 2.
            ]),
            Some("é count".chars().count())
        );
        let caret = layout
            .caret(TextPosition::new(
                projection.display_offset(base.len(), true),
            ))
            .unwrap();
        assert_eq!(
            geometry.hit([
                PADDING + caret.origin[0],
                PADDING + caret.origin[1] + caret.height / 2.
            ]),
            Some(source.chars().count())
        );
    }
}
