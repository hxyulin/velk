//! Exact retained shaping of a bounded Unicode island within verified ASCII text.
//! Bidi/control text and large/dispersed Unicode regions retain full-line shaping.
use crate::{Annotations, horizontal::RichWindow, surface::editor_text_style};
use ropey::{Rope, RopeSlice};
use rxui::{
    UiError,
    astrelis::{TextBuffer, TextLayout, TextLineMetrics, TextPosition, TextSystem, TextWrap},
};
use std::{ops::Range, sync::Arc};
use unicode_bidi::{BidiClass, bidi_class};

#[derive(Debug)]
pub(crate) struct UnicodeLine {
    range: Range<usize>,
    context: Rope,
    layout: Arc<TextLayout>,
    cell: f32,
    length: usize,
    pub width: f32,
    pub line: TextLineMetrics,
}

impl UnicodeLine {
    pub fn context_range(source: RopeSlice<'_>) -> Option<Range<usize>> {
        let mut first = None;
        let mut last = 0;
        for (i, c) in source.chars().enumerate() {
            if c.is_ascii() {
                if !(' '..='~').contains(&c) {
                    return None;
                }
            } else {
                let allowed = matches!(
                    bidi_class(c),
                    BidiClass::L
                        | BidiClass::EN
                        | BidiClass::ES
                        | BidiClass::ET
                        | BidiClass::CS
                        | BidiClass::NSM
                        | BidiClass::ON
                        | BidiClass::WS
                ) || matches!(c, '\u{200c}' | '\u{200d}');
                if !allowed || c.is_control() {
                    return None;
                }
                first.get_or_insert(i);
                last = i + 1;
            }
        }
        let first = first?;
        if last - first > 4096 {
            return None;
        }
        Some(first.saturating_sub(128)..(last + 128).min(source.len_chars()))
    }
    pub fn build(
        source: RopeSlice<'_>,
        range: Range<usize>,
        cell: f32,
        annotations: Option<&Annotations>,
        fonts: &mut TextSystem,
    ) -> Result<Option<Self>, UiError> {
        let text = source.slice(range.clone()).to_string();
        let context = Rope::from_str(&text);
        let spans = annotations
            .into_iter()
            .flat_map(|a| &a.spans)
            .filter_map(|span| {
                crate::annotations::text_span(
                    &span.style,
                    context.char_to_byte(span.range.start.min(context.len_chars()))
                        ..context.char_to_byte(span.range.end.min(context.len_chars())),
                )
            })
            .collect::<Vec<_>>();
        let mut buffer = TextBuffer::new();
        buffer.set_rich_text(&text, editor_text_style(), spans)?;
        buffer.set_wrap(TextWrap::None);
        let layout = buffer.layout(fonts)?;
        layout.prepare_interaction()?;
        if layout.lines().len() != 1
            || layout.lines()[0].rtl
            || layout
                .glyphs()
                .iter()
                .any(|g| g.bidi_level != 0 || !g.advance.is_finite() || g.advance < 0.)
        {
            return Ok(None);
        }
        // The retained ASCII context must agree with the external grid. A font
        // fallback or a contextual substitution crossing that boundary opts out.
        for glyph in layout.glyphs() {
            let cluster = &text[glyph.cluster.clone()];
            if cluster.bytes().all(|b| (b' '..=b'~').contains(&b))
                && (cluster.len() != 1 || (glyph.advance - cell).abs() > 0.0001)
            {
                return Ok(None);
            }
        }
        let line = &layout.lines()[0];
        let metrics = TextLineMetrics {
            ascent: line.ascent,
            descent: line.descent,
            height: line.height,
        };
        let width = (range.start as f64 * f64::from(cell)
            + f64::from(layout.size()[0])
            + (source.len_chars() - range.end) as f64 * f64::from(cell)) as f32;
        if !width.is_finite() {
            return Ok(None);
        }
        Ok(Some(Self {
            range,
            context,
            layout,
            cell,
            length: source.len_chars(),
            width,
            line: metrics,
        }))
    }
    fn left(&self) -> f32 {
        self.range.start as f32 * self.cell
    }
    pub fn caret(&self, source: usize) -> f32 {
        if source < self.range.start {
            return source as f32 * self.cell;
        }
        if source > self.range.end {
            return self.left()
                + self.layout.size()[0]
                + (source - self.range.end) as f32 * self.cell;
        }
        let byte = self.context.char_to_byte(source - self.range.start);
        let byte = if self.layout.is_text_boundary(byte) {
            byte
        } else {
            self.layout.previous_boundary(byte).unwrap_or(0)
        };
        self.left() + self.layout.caret(TextPosition::new(byte)).unwrap().origin[0]
    }
    pub fn hover_source(&self, x: f32) -> Option<(usize, f32)> {
        if x < 0. || x >= self.width {
            return None;
        }
        if x < self.left() {
            let position = (x / self.cell).floor() as usize;
            return Some((position, position as f32 * self.cell));
        }
        let right = self.left() + self.layout.size()[0];
        if x >= right {
            let offset = ((x - right) / self.cell).floor() as usize;
            return Some((self.range.end + offset, right + offset as f32 * self.cell));
        }
        let point = [x - self.left(), self.line.height / 2.];
        let (byte, rect) = crate::surface::layout_hover(&self.layout, point)?;
        Some((
            self.range.start + self.context.byte_to_char(byte),
            self.left() + rect.x,
        ))
    }
    pub fn hit(&self, x: f32) -> usize {
        if x < self.left() {
            return ((x.max(0.) / self.cell).round() as usize).min(self.range.start);
        }
        if x > self.left() + self.layout.size()[0] {
            return (self.range.end
                + ((x - self.left() - self.layout.size()[0]) / self.cell).round() as usize)
                .min(self.length);
        }
        let position = self
            .layout
            .hit_test([x - self.left(), self.line.height / 2.])
            .unwrap();
        self.range.start + self.context.byte_to_char(position.byte_offset)
    }
    pub fn window(&self, offset: f32, width: f32) -> RichWindow {
        const BUCKET: f32 = 1024.;
        let left = ((offset.max(0.) / BUCKET).floor() - 1.).max(0.) * BUCKET;
        let right = ((offset.max(0.) + width.max(0.)) / BUCKET).floor() * BUCKET + 2. * BUCKET;
        let mut range = self.hit(left)..self.hit(right).saturating_add(1).min(self.length);
        if range.start <= self.range.end && range.end >= self.range.start {
            range.start = range.start.min(self.range.start);
            range.end = range.end.max(self.range.end);
        }
        RichWindow {
            left: self.caret(range.start),
            range,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_unicode_context_matches_full_advanced_layout_at_window_edges() {
        let mut fonts = TextSystem::new();
        assert!(!fonts.load_system_fonts().is_empty());
        let mut probe = TextBuffer::new();
        probe.set_text("x", editor_text_style()).unwrap();
        let cell = probe.layout(&mut fonts).unwrap().glyphs()[0].advance;
        for island in [
            "café",
            "e\u{301}",
            "日本語",
            "가나다",
            "👩‍💻🇭🇰",
            "कर्म",
            "क्‍ष",
            "\u{301}e",
            "\u{200d}👩",
        ] {
            let source = format!("{}{}{}", "x".repeat(5000), island, "y".repeat(4000));
            let rope = Rope::from_str(&source);
            let range = UnicodeLine::context_range(rope.slice(..)).unwrap();
            let index = UnicodeLine::build(rope.slice(..), range, cell, None, &mut fonts)
                .unwrap()
                .unwrap();
            let mut reference = TextBuffer::new();
            reference.set_text(&source, editor_text_style()).unwrap();
            reference.set_wrap(TextWrap::None);
            let layout = reference.layout(&mut fonts).unwrap();
            let originals = layout
                .glyphs()
                .iter()
                .map(|g| ((g.cluster.start, g.glyph_id), g))
                .collect::<std::collections::HashMap<_, _>>();
            assert!(
                (index.width - layout.size()[0]).abs() < 0.1,
                "{island:?}: {} != {}",
                index.width,
                layout.size()[0]
            );
            for i in [
                0,
                4800,
                4999,
                5000,
                5001,
                5000 + island.chars().count(),
                5200,
                rope.len_chars(),
            ] {
                let byte = rope.char_to_byte(i);
                if !layout.is_text_boundary(byte) {
                    continue;
                }
                let caret = layout.caret(TextPosition::new(byte)).unwrap();
                assert!(
                    (index.caret(i) - caret.origin[0]).abs() < 0.1,
                    "{island:?}, char {i}"
                );
                let expected = layout
                    .hit_test([caret.origin[0], index.line.height / 2.])
                    .unwrap();
                assert_eq!(
                    index.hit(caret.origin[0]),
                    rope.byte_to_char(expected.byte_offset),
                    "{island:?}, hit {i}"
                );
                let window = index.window(index.caret(i), 1200.);
                let mut sliced = TextBuffer::new();
                sliced
                    .set_text(
                        &rope.slice(window.range.clone()).to_string(),
                        editor_text_style(),
                    )
                    .unwrap();
                sliced.set_wrap(TextWrap::None);
                sliced.set_minimum_line_metrics(Some(index.line)).unwrap();
                let slice = sliced.layout(&mut fonts).unwrap();
                for glyph in slice.glyphs() {
                    let cluster = glyph.cluster.start + rope.char_to_byte(window.range.start);
                    let original = originals[&(cluster, glyph.glyph_id)];
                    assert!(
                        (glyph.position[0] + window.left - original.position[0]).abs() < 0.1,
                        "{island:?}, glyph x"
                    );
                    assert!(
                        (glyph.position[1] - original.position[1]).abs() < 0.1,
                        "{island:?}, glyph baseline"
                    );
                }
            }
        }
    }
    #[test]
    fn directional_controls_rtl_tabs_and_unbounded_islands_keep_general_shaping() {
        for source in [
            "x العربية y",
            "x אבג y",
            "x\u{202e}abc",
            "x\t日",
            "x\u{2060}y",
        ] {
            let rope = Rope::from_str(source);
            assert!(
                UnicodeLine::context_range(rope.slice(..)).is_none(),
                "{source:?}"
            );
        }
        let rope = Rope::from_str(&"日".repeat(4097));
        assert!(UnicodeLine::context_range(rope.slice(..)).is_none());
        let rope = Rope::from_str(&format!("日{}日", "x".repeat(5000)));
        assert!(UnicodeLine::context_range(rope.slice(..)).is_none());
    }
}
