//! LTR ASCII metric runs and inline anchors. Unicode/bidi keep the general shaper.
use crate::{
    Annotations, OverlayKind, OverlayPlacement,
    surface::{LINE_HEIGHT, editor_text_style},
};
use rxui::UiError;
use rxui::astrelis::{FontSlant, TextBuffer, TextLineMetrics, TextSystem, TextWrap};
use std::{
    collections::{BTreeSet, HashMap},
    ops::Range,
};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Key {
    size: u32,
    weight: u16,
    italic: bool,
}
impl Key {
    fn new(size: f32, weight: u16, italic: bool) -> Self {
        Self {
            size: size.to_bits(),
            weight,
            italic,
        }
    }
}
#[derive(Clone, Copy)]
struct Metric {
    cell: f32,
    line: TextLineMetrics,
}
#[derive(Default)]
pub(crate) struct FontMetrics {
    cache: HashMap<Key, Option<Metric>>,
}
impl FontMetrics {
    pub fn clear(&mut self) {
        self.cache.clear();
    }
    fn get(&mut self, key: Key, fonts: &mut TextSystem) -> Result<Option<Metric>, UiError> {
        if let Some(metric) = self.cache.get(&key) {
            return Ok(*metric);
        }
        let size = f32::from_bits(key.size);
        let text: String = (b' '..=b'~').map(char::from).collect();
        let mut buffer = TextBuffer::new();
        buffer.set_text(
            &text,
            editor_text_style()
                .font_size(size)
                .line_height(size * LINE_HEIGHT / 16.)
                .weight(key.weight)
                .slant(if key.italic {
                    FontSlant::Italic
                } else {
                    FontSlant::Normal
                }),
        )?;
        buffer.set_wrap(TextWrap::None);
        let layout = buffer.layout(fonts)?;
        let cell = layout.glyphs().first().map_or(0., |g| g.advance);
        let line = &layout.lines()[0];
        let metric = (cell.is_finite()
            && cell > 0.
            && layout.glyphs().len() == text.len()
            && layout.missing_glyphs().is_empty()
            && layout
                .glyphs()
                .iter()
                .enumerate()
                .all(|(i, g)| g.cluster == (i..i + 1) && (g.advance - cell).abs() < 0.0001)
            && layout
                .fonts()
                .iter()
                .all(|f| fonts.font_info(f.id()).is_some_and(|i| i.monospaced)))
        .then_some(Metric {
            cell,
            line: TextLineMetrics {
                ascent: line.ascent,
                descent: line.descent,
                height: line.height,
            },
        });
        self.cache.insert(key, metric);
        Ok(metric)
    }
}
#[derive(Clone, Debug)]
struct Run {
    source: Range<usize>,
    x: f32,
    cell: f32,
}
#[derive(Clone, Debug)]
enum Content {
    Source(Range<usize>, f32),
    Hint(usize),
}
#[derive(Clone, Debug)]
struct Item {
    x: f32,
    end: f32,
    content: Content,
}
#[derive(Debug)]
pub(crate) struct RichLine {
    runs: Vec<Run>,
    items: Vec<Item>,
    hints: Vec<(usize, f32)>,
    pub width: f32,
    pub line: TextLineMetrics,
    pub backgrounds: Vec<usize>,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RichWindow {
    pub range: Range<usize>,
    pub left: f32,
}
impl RichLine {
    pub fn build(
        length: usize,
        source_start: usize,
        annotations: &Annotations,
        spans: &[usize],
        overlays: &[usize],
        metrics: &mut FontMetrics,
        fonts: &mut TextSystem,
    ) -> Result<Option<Self>, UiError> {
        let mut hints = Vec::new();
        for &id in overlays {
            let overlay = &annotations.overlays[id];
            if overlay.placement != OverlayPlacement::Inline
                || overlay.text.len() > 512
                || !overlay.text.bytes().all(|b| (b' '..=b'~').contains(&b))
            {
                return Ok(None);
            }
            hints.push((overlay.anchor - source_start, id));
        }
        hints.sort_by_key(|&(anchor, id)| (anchor, id));
        let mut group_anchor = None;
        let mut group_bytes = 0usize;
        for &(anchor, id) in &hints {
            if group_anchor != Some(anchor) {
                group_bytes = 0;
                group_anchor = Some(anchor);
            }
            group_bytes += annotations.overlays[id].text.len() + 8;
            // An anchor is an indivisible source position. Large co-anchored
            // clusters need their own projected-text slicing path.
            if group_bytes > 2048 {
                return Ok(None);
            }
        }
        let mut events = Vec::new();
        let mut bounds = vec![0, length];
        let mut backgrounds = Vec::new();
        for &id in spans {
            let span = &annotations.spans[id];
            if span.style.full_line_background {
                backgrounds.push(id);
            }
            let start = span.range.start.saturating_sub(source_start).min(length);
            let end = span.range.end.saturating_sub(source_start).min(length);
            if start == end {
                continue;
            }
            let style = &span.style;
            if style.font_size.is_some() || style.weight.is_some() || style.italic.is_some() {
                events.push((start, id, true));
                events.push((end, id, false));
                bounds.extend([start, end]);
            }
        }
        bounds.extend(hints.iter().map(|h| h.0));
        bounds.sort_unstable();
        bounds.dedup();
        events.sort_unstable();
        let mut active: [BTreeSet<usize>; 3] = std::array::from_fn(|_| BTreeSet::new());
        let mut event = 0;
        let mut hint = 0;
        let mut x = 0f64;
        let mut result = Self {
            runs: Vec::new(),
            items: Vec::new(),
            hints: Vec::new(),
            width: 0.,
            line: TextLineMetrics {
                ascent: 0.,
                descent: 0.,
                height: 0.,
            },
            backgrounds,
        };
        for (index, &start) in bounds.iter().enumerate() {
            while event < events.len() && events[event].0 == start {
                let (_, id, enter) = events[event];
                let style = &annotations.spans[id].style;
                for (set, present) in active.iter_mut().zip([
                    style.font_size.is_some(),
                    style.weight.is_some(),
                    style.italic.is_some(),
                ]) {
                    if present {
                        if enter {
                            set.insert(id);
                        } else {
                            set.remove(&id);
                        }
                    }
                }
                event += 1;
            }
            while hint < hints.len() && hints[hint].0 == start {
                let overlay = &annotations.overlays[hints[hint].1];
                let Some(mut metric) = metrics.get(
                    Key::new(12., 400, overlay.kind == OverlayKind::GhostText),
                    fonts,
                )?
                else {
                    return Ok(None);
                };
                // Inlays explicitly keep the editor's 24-unit line height.
                metric.line.height = LINE_HEIGHT;
                let end = x + f64::from(metric.cell) * (overlay.text.len() + 2) as f64;
                result.hints.push((start, x as f32));
                result.items.push(Item {
                    x: x as f32,
                    end: end as f32,
                    content: Content::Hint(start),
                });
                result.include(metric.line);
                x = end;
                hint += 1;
            }
            if let Some(&end) = bounds.get(index + 1)
                && start < end
            {
                let style =
                    |field: usize| active[field].last().map(|&id| &annotations.spans[id].style);
                let key = Key::new(
                    style(0).and_then(|s| s.font_size).unwrap_or(16.),
                    style(1).and_then(|s| s.weight).unwrap_or(400),
                    style(2).and_then(|s| s.italic).unwrap_or(false),
                );
                let Some(metric) = metrics.get(key, fonts)? else {
                    return Ok(None);
                };
                let right = x + f64::from(metric.cell) * (end - start) as f64;
                result.runs.push(Run {
                    source: start..end,
                    x: x as f32,
                    cell: metric.cell,
                });
                result.items.push(Item {
                    x: x as f32,
                    end: right as f32,
                    content: Content::Source(start..end, metric.cell),
                });
                result.include(metric.line);
                x = right;
            }
        }
        result.width = x as f32;
        if !result.width.is_finite() || result.line.height <= 0. {
            return Ok(None);
        }
        Ok(Some(result))
    }
    fn include(&mut self, line: TextLineMetrics) {
        self.line.ascent = self.line.ascent.max(line.ascent);
        self.line.descent = self.line.descent.max(line.descent);
        self.line.height = self.line.height.max(line.height);
    }
    pub fn caret(&self, source: usize) -> f32 {
        let index = self.runs.partition_point(|run| run.source.end <= source);
        self.runs.get(index).map_or(self.width, |run| {
            run.x + source.saturating_sub(run.source.start) as f32 * run.cell
        })
    }
    fn before(&self, source: usize) -> f32 {
        let hint = self.hints.partition_point(|h| h.0 < source);
        self.hints
            .get(hint)
            .filter(|h| h.0 == source)
            .map_or_else(|| self.caret(source), |h| h.1)
    }
    pub fn hover_source(&self, x: f32) -> Option<(usize, f32)> {
        let item = self
            .items
            .get(self.items.partition_point(|item| item.end <= x))?;
        if x < item.x || x >= item.end {
            return None;
        }
        match &item.content {
            Content::Hint(_) => None,
            Content::Source(source, cell) => {
                let column = ((x - item.x) / cell).floor() as usize;
                (column < source.len())
                    .then_some((source.start + column, item.x + column as f32 * cell))
            }
        }
    }
    pub fn hit(&self, x: f32) -> usize {
        let index = self.items.partition_point(|item| item.end < x);
        let item = &self.items[index.min(self.items.len() - 1)];
        match &item.content {
            Content::Hint(anchor) => *anchor,
            Content::Source(source, cell) => {
                source.start + (((x - item.x).max(0.) / cell).round() as usize).min(source.len())
            }
        }
    }
    pub fn window(&self, length: usize, offset: f32, width: f32) -> RichWindow {
        // Align pixels, rather than source columns: otherwise aligning back
        // 128 columns could pull many very wide offscreen labels into a slice.
        const BUCKET: f32 = 1024.;
        let left = ((offset.max(0.) / BUCKET).floor() - 1.).max(0.) * BUCKET;
        let right = ((offset.max(0.) + width.max(0.)) / BUCKET).floor() * BUCKET + BUCKET * 2.;
        let start = self.hit(left).min(length);
        let end = self.hit(right).saturating_add(1).min(length);
        let range = start..end.max(start);
        RichWindow {
            left: self.before(range.start),
            range,
        }
    }
}
