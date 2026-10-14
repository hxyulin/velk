use crate::Selection;
use ropey::Rope;
use rxui::astrelis::{
    FontFamily, PaintSession, PreparedText, Rect, TextBuffer, TextDraw, TextLayout, TextPosition,
    TextRasterOptions, TextStyle, TextWrap,
};
use rxui::{Bounds, CustomElement, CustomPrepare, ElementInfo, UiError};
use std::{cell::RefCell, ops::Range, rc::Rc, sync::Arc};

const PADDING: f32 = 16.;
const LINE_HEIGHT: f32 = 24.;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Composition {
    pub text: String,
    pub cursor: Option<(usize, usize)>,
}

/// The shaped snapshot used by pointer events and the IME candidate window.
#[derive(Default)]
pub(crate) struct Geometry {
    layout: Option<Arc<TextLayout>>,
    rope: Rope,
    replacement: Option<(Range<usize>, usize)>,
    offset: [f32; 2],
    viewport: [f32; 2],
    caret: Option<Rect>,
    pub epoch: Option<u64>,
    pub focused: bool,
}

impl Geometry {
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
        let layout = self.layout.as_ref()?;
        let byte = layout
            .hit_test([
                point[0] - PADDING + self.offset[0],
                point[1] - PADDING + self.offset[1],
            ])?
            .byte_offset;
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

    fn clamp_scroll(&mut self) {
        if let Some(layout) = &self.layout {
            let size = layout.size();
            for (axis, size) in size.into_iter().enumerate() {
                let maximum = (size + PADDING * 2. - self.viewport[axis]).max(0.);
                self.offset[axis] = self.offset[axis].clamp(0., maximum);
            }
        }
    }

    fn reveal(&mut self) {
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
    pub reveal_caret: bool,
    pub caret_visible: bool,
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
        if state.revision != Some(self.revision)
            || state.composition != self.composition
            || self.composition.is_some() && state.selection != self.selection
        {
            let mut text = self.rope.to_string();
            if let Some(composition) = &self.composition {
                text.replace_range(range.clone(), &composition.text);
            }
            state.buffer.set_text(&text, editor_text_style())?;
            state.buffer.set_wrap(TextWrap::None);
            state.revision = Some(self.revision);
            state.composition = self.composition.clone();
        }
        state.selection = self.selection;
        state
            .buffer
            .set_width(Some((element.bounds.width - PADDING * 2.).max(0.)))?;
        let layout = state.buffer.layout(cx.fonts)?;
        if state
            .layout
            .as_ref()
            .is_none_or(|old| !Arc::ptr_eq(old, &layout))
            || state.raster_scale != cx.raster_scale
        {
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
        let caret_byte = boundary_before(&layout, caret_byte);
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
        state.selection_rects.clear();
        state.composition_rects.clear();
        if let Some(composition) = &self.composition {
            let start = boundary_before(&layout, range.start);
            let end = boundary_after(&layout, range.start + composition.text.len());
            state
                .composition_rects
                .extend(layout.selection_rects(start..end)?);
        } else if !range.is_empty() {
            state.selection_rects.extend(layout.selection_rects(range)?);
        }
        let mut geometry = self.geometry.borrow_mut();
        let viewport = [element.bounds.width, element.bounds.height];
        let resized = geometry.viewport != viewport;
        geometry.viewport = viewport;
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
        for rect in &state.selection_rects {
            paint.fill_rect(translated(*rect, origin), [0.18, 0.40, 0.75, 0.35])?;
        }
        if let Some(prepared) = &state.prepared {
            paint.draw_text(prepared, TextDraw::new(origin).color(element.color))?;
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
