//! Bounded logical-line layout and GPU cache. Document positions stay in the Rope.
use crate::{
    Annotations, Selection,
    horizontal::{FontMetrics, RichLine, RichWindow},
    surface::{Geometry, LINE_HEIGHT, PADDING, Surface, SurfaceState, editor_text_style},
};
use ropey::Rope;
use rxui::astrelis::{PaintSession, Rect, TextBuffer, TextLayout, TextShaping, TextSystem};
use rxui::{CustomElement, CustomPrepare, ElementInfo, UiError};
use std::{cell::RefCell, collections::HashMap, ops::Range, rc::Rc, sync::Arc, time::Instant};

const CACHE_LIMIT: usize = 256;
const OVERSCAN: usize = 4;

// Direct line-level composition support for CPU reference probes. Native
// composition is projected into a transient Rope before entering this cache.
fn composition_line(surface: &Surface) -> Option<usize> {
    let composition = surface.composition.as_ref()?;
    let range = surface.selection.range();
    let line = surface.rope.char_to_line(range.start);
    (surface.rope.char_to_line(range.end) == line && !composition.text.contains(['\n', '\r']))
        .then_some(line)
}

/// CPU/GPU preparation diagnostics for the most recently prepared frame.
/// GPU preparation is CPU submission/resource work, not GPU execution time.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameMetrics {
    pub visible_lines: usize,
    pub cached_lines: usize,
    pub cached_source_bytes: usize,
    pub shaped_lines: usize,
    pub cache_hits: usize,
    pub gpu_prepared_lines: usize,
    pub annotation_index_ms: f64,
    pub cpu_layout_ms: f64,
    pub gpu_prepare_ms: f64,
    pub total_prepare_ms: f64,
    pub paint_record_ms: f64,
    /// Editor input-listener start to preparation; excludes GPU execution/presentation.
    pub input_to_prepare_ms: Option<f64>,
    /// Retained diagnostic for older consumers; projected composition keeps this false.
    pub composition_fallback: bool,
    pub horizontal_lines: usize,
    pub rich_horizontal_lines: usize,
    pub unicode_horizontal_lines: usize,
    pub composition_projected: bool,
    pub projection_ms: f64,
}

pub(super) struct LineHit {
    pub top: f32,
    pub start_char: usize,
    pub left: f32,
    pub grid: Option<(f32, usize)>,
    pub rich: Option<Arc<RichLine>>,
    pub unicode: Option<Arc<crate::unicode_window::UnicodeLine>>,
    pub geometry: Rc<RefCell<Geometry>>,
}

/// A Fenwick tree of deviations from the default line height. No document
/// strings or shaped glyphs are retained here. Lookups and updates are O(log N).
struct Heights {
    values: Vec<f32>,
    tree: Vec<f32>,
}
impl Heights {
    fn new(count: usize) -> Self {
        Self {
            values: vec![LINE_HEIGHT; count],
            tree: vec![0.; count + 1],
        }
    }
    fn top(&self, line: usize) -> f32 {
        let mut sum = line as f32 * LINE_HEIGHT;
        let mut index = line;
        while index > 0 {
            sum += self.tree[index];
            index &= index - 1;
        }
        sum
    }
    fn set(&mut self, line: usize, height: f32) {
        let delta = height - self.values[line];
        self.values[line] = height;
        let mut index = line + 1;
        while index < self.tree.len() {
            self.tree[index] += delta;
            index += index.isolate_lowest_one();
        }
    }
    fn line_at(&self, y: f32) -> usize {
        // Prefix lookup with binary lifting, including each node's base height.
        let mut index = 0;
        let mut sum = 0.;
        let mut step = self.values.len().next_power_of_two();
        while step > 0 {
            let next = index + step;
            if next < self.tree.len() {
                let height = self.tree[next] + next.isolate_lowest_one() as f32 * LINE_HEIGHT;
                if sum + height <= y {
                    index = next;
                    sum += height;
                }
            }
            step >>= 1;
        }
        index.min(self.values.len() - 1)
    }
}

#[derive(Default)]
struct AnnotationIndex {
    snapshot: Option<Arc<Annotations>>,
    spans: Vec<(usize, usize, usize)>,
    // Augmented interval tree skips complete subtrees whose ranges end before
    // the queried line, even with a document-wide span overlapping local spans.
    max_end: Vec<usize>,
    leaves: usize,
    overlays: Vec<(usize, usize)>,
    line_count: usize,
}
impl AnnotationIndex {
    fn update(&mut self, annotations: &Option<Arc<Annotations>>, rope: &Rope) -> bool {
        if match (&self.snapshot, annotations) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        } {
            return false;
        }
        self.snapshot = annotations.clone();
        self.line_count = rope.len_lines();
        self.spans.clear();
        self.overlays.clear();
        if let Some(annotations) = annotations {
            self.spans.extend(
                annotations
                    .spans
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !s.range.is_empty())
                    .map(|(id, s)| (s.range.start, s.range.end, id)),
            );
            self.spans.sort_by_key(|s| s.0);
            self.overlays.extend(
                annotations
                    .overlays
                    .iter()
                    .enumerate()
                    .map(|(id, o)| (o.anchor, id)),
            );
            self.overlays.sort_by_key(|o| o.0);
        }
        self.leaves = self.spans.len().max(1).next_power_of_two();
        self.max_end.clear();
        self.max_end.resize(self.leaves * 2, 0);
        for (index, span) in self.spans.iter().enumerate() {
            self.max_end[self.leaves + index] = span.1;
        }
        for index in (1..self.leaves).rev() {
            self.max_end[index] = self.max_end[index * 2].max(self.max_end[index * 2 + 1]);
        }
        true
    }
    fn span_ids(&self, range: &Range<usize>) -> Vec<usize> {
        if self.spans.is_empty() {
            return Vec::new();
        }
        let end = self.spans.partition_point(|span| span.0 < range.end);
        let mut ids = Vec::new();
        let mut stack = vec![(1, 0, self.leaves)];
        while let Some((node, start, stop)) = stack.pop() {
            if start >= end || self.max_end[node] <= range.start {
                continue;
            }
            if stop - start == 1 {
                ids.push(self.spans[start].2);
            } else {
                let middle = (start + stop) / 2;
                stack.push((node * 2 + 1, middle, stop));
                stack.push((node * 2, start, middle));
            }
        }
        ids.sort_unstable();
        ids
    }
    fn overlay_ids(&self, line: usize, range: &Range<usize>) -> Vec<usize> {
        let start = self.overlays.partition_point(|o| o.0 < range.start);
        let end = self.overlays.partition_point(|o| {
            o.0 < range.end || line + 1 == self.line_count && o.0 == range.end
        });
        let mut ids = self.overlays[start..end]
            .iter()
            .map(|o| o.1)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }
    #[cfg(test)]
    fn line(&self, line: usize, range: Range<usize>) -> Option<Arc<Annotations>> {
        self.project(line, range, &[]).0
    }
    fn project(
        &self,
        line: usize,
        range: Range<usize>,
        backgrounds: &[usize],
    ) -> (Option<Arc<Annotations>>, Vec<usize>) {
        let Some(annotations) = self.snapshot.as_ref() else {
            return (None, Vec::new());
        };
        let mut ids = self.span_ids(&range);
        ids.extend_from_slice(backgrounds);
        ids.sort_unstable();
        ids.dedup();
        let spans = ids
            .iter()
            .map(|&id| {
                let mut span = annotations.spans[id].clone();
                if span.range.end <= range.start || span.range.start >= range.end {
                    span.range = 0..range.len();
                    span.hover = None;
                    span.style = crate::SpanStyle {
                        background: span.style.background,
                        full_line_background: true,
                        ..crate::SpanStyle::default()
                    };
                } else {
                    span.range = span.range.start.max(range.start) - range.start
                        ..span.range.end.min(range.end) - range.start;
                }
                span
            })
            .collect::<Vec<_>>();
        let overlay_ids = self.overlay_ids(line, &range);
        let overlays = overlay_ids
            .iter()
            .map(|&id| {
                let mut overlay = annotations.overlays[id].clone();
                overlay.anchor -= range.start;
                overlay
            })
            .collect::<Vec<_>>();
        let mut hover_ids = ids;
        hover_ids.extend(
            overlay_ids
                .into_iter()
                .map(|id| annotations.spans.len() + id),
        );
        if spans.is_empty() && overlays.is_empty() {
            (None, hover_ids)
        } else {
            (
                Some(Arc::new(Annotations {
                    document_revision: 0,
                    spans,
                    overlays,
                })),
                hover_ids,
            )
        }
    }
    #[cfg(test)]
    fn hover_id(&self, line: usize, range: Range<usize>, local_id: usize) -> usize {
        let ids = self.span_ids(&range);
        if let Some(&id) = ids.get(local_id) {
            id
        } else {
            self.snapshot.as_ref().unwrap().spans.len()
                + self.overlay_ids(line, &range)[local_id - ids.len()]
        }
    }
}

/// Fixed-grid windows are aligned to 128-column chunks, with one chunk of
/// overscan on each side. A small scroll reuses the existing shaped text.
#[derive(Clone, Debug, PartialEq)]
struct HorizontalWindow {
    range: Range<usize>,
    cell: f32,
    full_len: usize,
}
impl HorizontalWindow {
    fn new(length: usize, cell: f32, offset: f32, width: f32) -> Self {
        const CHUNK: usize = 128;
        let first = ((offset.max(0.) / cell) as usize).min(length);
        let last = (((offset.max(0.) + width.max(0.)) / cell).ceil() as usize).min(length);
        let start = (first / CHUNK).saturating_sub(1) * CHUNK;
        let end = (last / CHUNK + 2).saturating_mul(CHUNK).min(length);
        Self {
            range: start..end,
            cell,
            full_len: length,
        }
    }
    fn left(&self) -> f32 {
        self.range.start as f32 * self.cell
    }
    fn width(&self) -> f32 {
        self.full_len as f32 * self.cell
    }
}

struct CachedLine {
    text: String,
    surface: Surface,
    state: SurfaceState,
    used: u64,
    gpu_dirty: bool,
    checked_revision: Option<u64>,
    checked_annotations: u64,
    checked_fonts: u64,
    rich: Option<Arc<RichLine>>,
    rich_window: Option<RichWindow>,
    unicode: Option<Arc<crate::unicode_window::UnicodeLine>>,
    unicode_window: Option<RichWindow>,
    hover_ids: Vec<usize>,
    metric_safe: bool,
    annotation_range: Range<usize>,
    grid_eligible: bool,
    window: Option<HorizontalWindow>,
    source_start: usize,
    full_len: usize,
}

pub(crate) struct ViewportSurface(pub Surface);
impl PartialEq for ViewportSurface {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

pub(crate) struct ViewportState {
    cache: HashMap<usize, CachedLine>,
    index: AnnotationIndex,
    heights: Heights,
    revision: Option<u64>,
    frame: u64,
    visible: Range<usize>,
    projected: Option<Box<crate::composition::ProjectedComposition>>,
    metrics: FrameMetrics,
    grid_probe: TextBuffer,
    grid_layout: Option<Arc<TextLayout>>,
    cell: Option<f32>,
    annotation_epoch: u64,
    font_epoch: u64,
    rich_fonts: FontMetrics,
}
impl Default for ViewportState {
    fn default() -> Self {
        Self {
            cache: HashMap::new(),
            index: AnnotationIndex::default(),
            heights: Heights::new(1),
            revision: None,
            frame: 0,
            visible: 0..0,
            projected: None,
            metrics: FrameMetrics::default(),
            grid_probe: {
                let mut buffer = TextBuffer::new();
                let ascii: String = (b' '..=b'~').map(char::from).collect();
                buffer.set_text(&ascii, editor_text_style()).unwrap();
                buffer.set_shaping(TextShaping::Basic);
                buffer
            },
            grid_layout: None,
            cell: None,
            annotation_epoch: 0,
            font_epoch: 0,
            rich_fonts: FontMetrics::default(),
        }
    }
}

impl ViewportState {
    fn prepare_line(
        &mut self,
        surface: &Surface,
        line: usize,
        width: f32,
        offset: f32,
        fonts: &mut TextSystem,
    ) -> Result<(), UiError> {
        let start = surface.rope.line_to_char(line);
        let slice = surface.rope.line(line);
        let length = slice.len_chars()
            - usize::from(slice.get_char(slice.len_chars().saturating_sub(1)) == Some('\n'));
        let range = start..start + slice.len_chars();
        let selection = surface.selection;
        let composition = (composition_line(surface) == Some(line))
            .then(|| surface.composition.clone())
            .flatten();
        let entry = self.cache.entry(line).or_insert_with(|| CachedLine {
            text: String::new(),
            surface: Surface {
                rope: Rope::new(),
                revision: 0,
                selection: Selection::default(),
                composition: None,
                preedit: None,
                geometry: Rc::new(RefCell::new(Geometry::default())),
                epoch: 0,
                input_started: None,
                reveal_caret: false,
                caret_visible: false,
                minimum_line_metrics: None,
                annotations: None,
                annotation_groups: None,
                selection_color: surface.selection_color,
            },
            state: SurfaceState::default(),
            used: 0,
            gpu_dirty: true,
            checked_revision: None,
            checked_annotations: u64::MAX,
            checked_fonts: u64::MAX,
            rich: None,
            rich_window: None,
            unicode: None,
            unicode_window: None,
            hover_ids: Vec::new(),
            metric_safe: false,
            annotation_range: range.clone(),
            grid_eligible: false,
            window: None,
            source_start: start,
            full_len: length,
        });
        let changed = entry.checked_revision != Some(surface.revision);
        if changed {
            entry.grid_eligible = slice
                .slice(..length)
                .chunks()
                .all(|chunk| chunk.bytes().all(|byte| (b' '..=b'~').contains(&byte)));
            entry.checked_revision = Some(surface.revision);
        }
        if changed
            || entry.checked_annotations != self.annotation_epoch
            || entry.checked_fonts != self.font_epoch
        {
            entry.metric_safe = self.index.snapshot.as_ref().is_none_or(|a| {
                self.index.span_ids(&range).into_iter().all(|id| {
                    let style = &a.spans[id].style;
                    style.font_size.is_none_or(|size| size == 16.)
                        && style.weight.is_none_or(|weight| weight == 400)
                        && !style.italic.unwrap_or(false)
                        && !style.full_line_background
                }) && self.index.overlay_ids(line, &range).is_empty()
            });
            entry.rich = if entry.grid_eligible
                && !entry.metric_safe
                && length > 1024
                && self.cell.is_some()
            {
                let annotations = self.index.snapshot.as_ref().unwrap();
                RichLine::build(
                    length,
                    start,
                    annotations,
                    &self.index.span_ids(&range),
                    &self.index.overlay_ids(line, &range),
                    &mut self.rich_fonts,
                    fonts,
                )?
                .map(Arc::new)
            } else {
                None
            };
            entry.checked_annotations = self.annotation_epoch;
            entry.checked_fonts = self.font_epoch;
            entry.unicode = if !entry.grid_eligible && entry.metric_safe && length > 1024 {
                if let (Some(cell), Some(context)) = (
                    self.cell,
                    crate::unicode_window::UnicodeLine::context_range(slice.slice(..length)),
                ) {
                    let annotations = self
                        .index
                        .project(line, start + context.start..start + context.end, &[])
                        .0;
                    crate::unicode_window::UnicodeLine::build(
                        slice.slice(..length),
                        context,
                        cell,
                        annotations.as_deref(),
                        fonts,
                    )?
                    .map(Arc::new)
                } else {
                    None
                }
            } else {
                None
            };
        }
        let window = self
            .cell
            .filter(|_| {
                composition.is_none() && entry.grid_eligible && entry.metric_safe && length > 1024
            })
            .map(|cell| HorizontalWindow::new(length, cell, offset, width));
        let rich_window = entry
            .rich
            .as_ref()
            .filter(|_| composition.is_none())
            .map(|index| index.window(length, offset, width));
        let unicode_window = entry
            .unicode
            .as_ref()
            .filter(|_| composition.is_none())
            .map(|index| index.window(offset, width));
        let columns = unicode_window
            .as_ref()
            .or(rich_window.as_ref())
            .as_ref()
            .map(|window| window.range.clone())
            .or_else(|| window.as_ref().map(|window| window.range.clone()))
            .unwrap_or(0..length);
        let text = slice.slice(columns.clone()).to_string();
        let annotation_range =
            if window.is_some() || rich_window.is_some() || unicode_window.is_some() {
                start + columns.start..if columns.end == length {
                    range.end
                } else {
                    start + columns.end
                }
            } else {
                range.clone()
            };
        let (annotations, hover_ids) = self.index.project(
            line,
            annotation_range.clone(),
            entry
                .rich
                .as_ref()
                .map_or(&[], |rich| rich.backgrounds.as_slice()),
        );
        entry.hover_ids = hover_ids;
        entry.surface.annotation_groups = surface
            .annotation_groups
            .as_ref()
            .map(|groups| Arc::new(entry.hover_ids.iter().map(|&id| groups[id]).collect()));
        entry.annotation_range = annotation_range;
        let clamp = |position: usize| {
            position
                .saturating_sub(start + columns.start)
                .min(columns.len())
        };
        if entry.window.is_none()
            && entry.rich_window.is_none()
            && entry.unicode_window.is_none()
            && (window.is_some() || rich_window.is_some() || unicode_window.is_some())
            && !entry.text.is_empty()
        {
            // A former Unicode/rich fallback can retain whole-line shaping
            // scratch. Release it when returning to bounded grid preparation.
            entry.state = SurfaceState::default();
            entry.gpu_dirty = true;
            entry.surface.geometry.borrow_mut().content_size = [0.; 2];
        }
        entry.window = window;
        entry.rich_window = rich_window;
        entry.unicode_window = unicode_window;
        entry.surface.minimum_line_metrics = entry
            .unicode
            .as_ref()
            .map(|unicode| unicode.line)
            .or_else(|| entry.rich.as_ref().map(|rich| rich.line));
        entry.source_start = start;
        entry.full_len = length;
        if entry.text != text {
            entry.surface.rope = Rope::from_str(&text);
            entry.surface.revision += 1;
            entry.text = text;
        }
        let annotations = annotations.map(|a| {
            let mut a = (*a).clone();
            for span in &mut a.spans {
                span.range.end = span.range.end.min(columns.len());
                span.range.start = span.range.start.min(columns.len());
            }
            Arc::new(a)
        });
        if entry.surface.annotations.as_deref() != annotations.as_deref() {
            entry.surface.annotations = annotations;
        }
        entry.surface.selection = Selection {
            anchor: clamp(selection.anchor),
            head: clamp(selection.head),
        };
        entry.surface.composition = composition;
        entry.surface.preedit = surface.preedit.as_ref().and_then(|preedit| {
            let start = preedit.start.max(entry.source_start + columns.start);
            let end = preedit.end.min(entry.source_start + columns.end);
            (start < end).then(|| {
                start - entry.source_start - columns.start..end - entry.source_start - columns.start
            })
        });
        entry.surface.selection_color = surface.selection_color;
        entry.surface.caret_visible = surface.caret_visible
            && surface.rope.char_to_line(selection.head) == line
            && selection.head >= start + columns.start
            && selection.head <= start + columns.end;
        entry.surface.epoch = surface.epoch;
        entry.used = self.frame;
        let shaped = entry.surface.shape(&mut entry.state, width, fonts)?;
        if shaped {
            self.metrics.shaped_lines += 1;
            entry.gpu_dirty = true;
        } else {
            self.metrics.cache_hits += 1;
        }
        let layout = entry.surface.layout(&entry.state);
        self.heights.set(line, layout.size()[1].max(LINE_HEIGHT));
        Ok(())
    }
    fn prepare_cpu(
        &mut self,
        surface: &Surface,
        viewport: [f32; 2],
        fonts: &mut TextSystem,
    ) -> Result<(), UiError> {
        self.metrics = FrameMetrics::default();
        self.frame += 1;
        let started = Instant::now();
        let document_changed = self.revision != Some(surface.revision);
        let index_changed = self.index.update(&surface.annotations, &surface.rope);
        if index_changed {
            self.annotation_epoch += 1;
        }
        self.metrics.annotation_index_ms = started.elapsed().as_secs_f64() * 1000.;
        let mut geometry = surface.geometry.borrow_mut();
        geometry.layout = None;
        let viewport_changed = geometry.viewport != viewport;
        geometry.viewport = viewport;
        let old_line = self.heights.line_at(geometry.offset[1]);
        let intra = geometry.offset[1] - self.heights.top(old_line);
        if self.heights.values.len() != surface.rope.len_lines() || index_changed {
            self.heights = Heights::new(surface.rope.len_lines());
        }
        if document_changed {
            self.cache
                .retain(|&line, _| line < surface.rope.len_lines());
        }
        self.revision = Some(surface.revision);
        let width = (viewport[0] - PADDING * 2.).max(0.);
        let started = Instant::now();
        let probe = if !fonts.as_cosmic().db().is_empty() {
            Some(self.grid_probe.layout(fonts)?)
        } else {
            None
        };
        if let Some(probe) = probe
            && self
                .grid_layout
                .as_ref()
                .is_none_or(|old| !Arc::ptr_eq(old, &probe))
        {
            let advance = probe.glyphs().first().map_or(0., |glyph| glyph.advance);
            self.cell = (advance.is_finite()
                && advance > 0.
                && probe.missing_glyphs().is_empty()
                && probe.fonts().iter().all(|font| {
                    fonts
                        .font_info(font.id())
                        .is_some_and(|info| info.monospaced)
                })
                && probe
                    .glyphs()
                    .iter()
                    .all(|glyph| (glyph.advance - advance).abs() < 0.0001))
            .then_some(advance);
            self.grid_layout = Some(probe);
            self.font_epoch += 1;
            self.rich_fonts.clear();
        }
        // Shape the caret line even when it is offscreen so document-edge and
        // keyboard navigation can reveal exact caret geometry without a scan.
        let caret_line = surface.rope.char_to_line(surface.selection.head);
        self.prepare_line(surface, caret_line, width, geometry.offset[0], fonts)?;
        let reveal =
            surface.reveal_caret && (geometry.epoch != Some(surface.epoch) || viewport_changed);
        if reveal {
            let entry = &self.cache[&caret_line];
            geometry.caret =
                Some(entry.document_caret(surface.selection.head, self.heights.top(caret_line)));
            geometry.content_size[0] = geometry.content_size[0].max(entry.width());
            geometry.content_size[1] = self.heights.top(self.heights.values.len());
            let old_offset = geometry.offset[0];
            geometry.reveal();
            if geometry.offset[0] != old_offset {
                self.prepare_line(surface, caret_line, width, geometry.offset[0], fonts)?;
            }
        }
        let start = self
            .heights
            .line_at(geometry.offset[1])
            .saturating_sub(OVERSCAN);
        let end = (self.heights.line_at(geometry.offset[1] + viewport[1]) + OVERSCAN + 1)
            .min(surface.rope.len_lines());
        self.visible = start..end;
        for line in self.visible.clone() {
            if line != caret_line {
                self.prepare_line(surface, line, width, geometry.offset[0], fonts)?;
            }
        }
        // Preserve the top source line as exact mixed-size measurements replace
        // estimates. Explicit caret reveal takes precedence over scroll anchoring.
        if !reveal {
            geometry.offset[1] =
                self.heights.top(old_line.min(surface.rope.len_lines() - 1)) + intra;
        }
        if self.cache.len() > CACHE_LIMIT {
            let mut old = self
                .cache
                .iter()
                .filter(|(line, _)| !self.visible.contains(line) && **line != caret_line)
                .map(|(&line, e)| (e.used, line))
                .collect::<Vec<_>>();
            old.sort_unstable();
            for (_, line) in old.into_iter().take(self.cache.len() - CACHE_LIMIT) {
                self.cache.remove(&line);
            }
        }
        geometry.content_size = [
            self.cache
                .values()
                .map(CachedLine::width)
                .fold(viewport[0] - PADDING * 2., f32::max),
            self.heights.top(surface.rope.len_lines()),
        ];
        let prepared_offset = geometry.offset[0];
        geometry.clamp_scroll();
        if geometry.offset[0] != prepared_offset {
            // Width changes can move the final viewport left after caret reveal.
            // Prepare that viewport in this frame, without relying on a follow-up.
            for line in self.visible.clone() {
                self.prepare_line(surface, line, width, geometry.offset[0], fonts)?;
            }
            if !self.visible.contains(&caret_line) {
                self.prepare_line(surface, caret_line, width, geometry.offset[0], fonts)?;
            }
        }
        self.metrics.visible_lines = self.visible.len();
        self.metrics.horizontal_lines = self
            .visible
            .clone()
            .filter(|line| {
                self.cache[line].window.is_some() || self.cache[line].rich_window.is_some()
            })
            .count();
        self.metrics.rich_horizontal_lines = self
            .visible
            .clone()
            .filter(|line| self.cache[line].rich_window.is_some())
            .count();
        self.metrics.cached_lines = self.cache.len();
        self.metrics.unicode_horizontal_lines = self
            .visible
            .clone()
            .filter(|line| self.cache[line].unicode_window.is_some())
            .count();
        self.metrics.cached_source_bytes = self.cache.values().map(|entry| entry.text.len()).sum();
        self.metrics.cpu_layout_ms = started.elapsed().as_secs_f64() * 1000.;
        Ok(())
    }
}
impl CachedLine {
    fn left(&self) -> f32 {
        if let Some(window) = &self.unicode_window {
            return window.left;
        }
        self.rich_window.as_ref().map_or_else(
            || self.window.as_ref().map_or(0., HorizontalWindow::left),
            |window| window.left,
        )
    }
    fn width(&self) -> f32 {
        if self.surface.composition.is_some() {
            self.state.content_width()
        } else if let Some(unicode) = &self.unicode {
            unicode.width
        } else if let Some(rich) = &self.rich {
            rich.width
        } else {
            self.window
                .as_ref()
                .map_or_else(|| self.state.content_width(), HorizontalWindow::width)
        }
    }
    fn columns(&self) -> Range<usize> {
        self.unicode_window
            .as_ref()
            .or(self.rich_window.as_ref())
            .map(|window| window.range.clone())
            .or_else(|| self.window.as_ref().map(|window| window.range.clone()))
            .unwrap_or(0..self.full_len)
    }
    fn document_caret(&self, head: usize, top: f32) -> Rect {
        if self.surface.composition.is_some() {
            let caret = self.surface.candidate_caret(&self.state).unwrap();
            Rect::new(caret.x, top + caret.y, caret.width, caret.height)
        } else if let Some(unicode) = &self.unicode {
            Rect::new(
                unicode.caret(head.saturating_sub(self.source_start).min(self.full_len)),
                top,
                1.5,
                unicode.line.height,
            )
        } else if let Some(rich) = &self.rich {
            Rect::new(
                rich.caret(head.saturating_sub(self.source_start).min(self.full_len)),
                top,
                1.5,
                rich.line.height,
            )
        } else if let Some(window) = &self.window {
            Rect::new(
                head.saturating_sub(self.source_start).min(self.full_len) as f32 * window.cell,
                top,
                1.5,
                LINE_HEIGHT,
            )
        } else {
            let local = self.surface.rope.char_to_byte(self.surface.selection.head);
            let caret = self
                .surface
                .layout(&self.state)
                .caret(rxui::astrelis::TextPosition::new(
                    self.state_projection(local),
                ))
                .unwrap();
            Rect::new(caret.origin[0], top + caret.origin[1], 1.5, caret.height)
        }
    }
    fn state_projection(&self, byte: usize) -> usize {
        self.state.project_caret(byte)
    }
}

impl CustomElement for ViewportSurface {
    type State = ViewportState;
    fn prepare(
        &self,
        state: &mut ViewportState,
        element: &ElementInfo<'_>,
        cx: &mut CustomPrepare<'_>,
    ) -> Result<(), UiError> {
        let started = Instant::now();
        let surface = &self.0;
        if surface.composition.is_some() {
            let projected = state.projected.get_or_insert_with(|| {
                Box::new(crate::composition::ProjectedComposition::new(surface))
            });
            return projected.prepare(surface, element, cx);
        }
        state.projected = None;
        surface.geometry.borrow_mut().composed = None;
        state.prepare_cpu(
            surface,
            [element.bounds.width, element.bounds.height],
            cx.fonts,
        )?;
        let mut geometry = surface.geometry.borrow_mut();
        geometry.lines.clear();
        geometry.hover_regions.clear();
        geometry.origin = [element.bounds.x, element.bounds.y];
        geometry.rope = surface.rope.clone();
        geometry.focused = element.focused;
        let started_gpu = Instant::now();
        for line in state.visible.clone() {
            let entry = state.cache.get_mut(&line).unwrap();
            let gpu_dirty = entry.gpu_dirty || entry.last_raster() != cx.raster_scale;
            entry.surface.prepare(&mut entry.state, element, cx)?;
            if gpu_dirty {
                state.metrics.gpu_prepared_lines += 1;
            }
            entry.gpu_dirty = false;
            let top = state.heights.top(line);
            let start_char = surface.rope.line_to_char(line);
            let mut local = entry.surface.geometry.borrow_mut();
            local.offset = [0.; 2];
            geometry.content_size[0] = geometry.content_size[0].max(local.content_size[0]);
            for hit in &local.hover_regions {
                let mut hit = hit.clone();
                hit.id = entry.hover_ids[hit.id];
                hit.rect.x += entry.left();
                hit.rect.y += top;
                geometry.hover_regions.push(hit);
            }
            if line == surface.rope.char_to_line(surface.selection.head) {
                geometry.caret = Some(entry.document_caret(surface.selection.head, top));
            }
            geometry.lines.push(LineHit {
                top,
                start_char: if entry.window.is_some()
                    || entry.rich_window.is_some()
                    || entry.unicode_window.is_some()
                {
                    start_char
                } else {
                    start_char + entry.columns().start
                },
                left: entry.left(),
                grid: entry.window.as_ref().map(|w| (w.cell, w.full_len)),
                rich: entry
                    .rich
                    .clone()
                    .filter(|_| entry.surface.composition.is_none()),
                unicode: entry
                    .unicode
                    .clone()
                    .filter(|_| entry.surface.composition.is_none()),
                geometry: entry.surface.geometry.clone(),
            });
        }
        state.metrics.gpu_prepare_ms = started_gpu.elapsed().as_secs_f64() * 1000.;
        state.metrics.total_prepare_ms = started.elapsed().as_secs_f64() * 1000.;
        geometry.epoch = Some(surface.epoch);
        if surface.input_started != geometry.last_input {
            state.metrics.input_to_prepare_ms = surface
                .input_started
                .map(|start| start.elapsed().as_secs_f64() * 1000.);
            geometry.last_input = surface.input_started;
        }
        geometry.metrics = state.metrics;
        if std::env::var_os("VELK_PROFILE").is_some() {
            eprintln!("Velk frame: {:?}", state.metrics);
        }
        Ok(())
    }
    fn paint(
        &self,
        state: &ViewportState,
        element: &ElementInfo<'_>,
        paint: &mut PaintSession<'_, '_>,
    ) -> Result<(), UiError> {
        if self.0.composition.is_some() {
            let projected = state.projected.as_ref().unwrap();
            projected.surface.paint(&projected.state, element, paint)?;
            self.0.geometry.borrow_mut().metrics.paint_record_ms = projected
                .surface
                .0
                .geometry
                .borrow()
                .metrics
                .paint_record_ms;
            return Ok(());
        }
        let started = Instant::now();
        let geometry = self.0.geometry.borrow();
        for line in state.visible.clone() {
            let entry = &state.cache[&line];
            let top = state.heights.top(line);
            let bottom = top + state.heights.values[line];
            if bottom < geometry.offset[1] || top > geometry.offset[1] + geometry.viewport[1] {
                continue;
            }
            let origin = [
                element.bounds.x + PADDING + entry.left() - geometry.offset[0],
                element.bounds.y + PADDING + top - geometry.offset[1],
            ];
            if entry.text.is_empty() {
                for span in entry.surface.annotations.iter().flat_map(|a| &a.spans) {
                    if span.style.full_line_background
                        && let Some(color) = span.style.background
                    {
                        paint.fill_rect(
                            Rect::new(
                                element.bounds.x,
                                origin[1],
                                element.bounds.width,
                                state.heights.values[line],
                            ),
                            color,
                        )?;
                    }
                }
            }
            let source_start = self.0.rope.line_to_char(line);
            let lf = source_start + entry.full_len;
            let selected = self.0.selection.range();
            if line + 1 < self.0.rope.len_lines()
                && selected.start <= lf
                && selected.end > lf
                && entry.columns().end == entry.full_len
            {
                let position = entry
                    .surface
                    .layout(&entry.state)
                    .caret(rxui::astrelis::TextPosition::new(
                        entry.state_projection(entry.text.len()),
                    ))
                    .unwrap();
                paint.fill_rect(
                    Rect::new(
                        origin[0] + position.origin[0],
                        origin[1],
                        4.,
                        state.heights.values[line],
                    ),
                    self.0.selection_color,
                )?;
            }
            entry
                .surface
                .paint_at(&entry.state, element, paint, origin)?;
        }
        drop(geometry);
        let elapsed = started.elapsed().as_secs_f64() * 1000.;
        self.0.geometry.borrow_mut().metrics.paint_record_ms = elapsed;
        if std::env::var_os("VELK_PROFILE").is_some() {
            eprintln!("Velk paint record: {elapsed:.6} ms");
        }
        Ok(())
    }
}
impl CachedLine {
    fn last_raster(&self) -> f32 {
        self.state.raster_scale()
    }
}

/// CPU-only probe for the benchmark feature. It runs the same line cache and
/// projection/shaping path as the native widget, without GPU resources.
#[cfg(feature = "performance")]
pub struct CpuViewportProbe {
    surface: Surface,
    state: ViewportState,
    fonts: TextSystem,
}
#[cfg(feature = "performance")]
impl CpuViewportProbe {
    pub fn new(rope: Rope) -> Self {
        let mut fonts = TextSystem::new();
        assert!(
            !fonts.load_system_fonts().is_empty(),
            "benchmark requires installed fonts"
        );
        Self {
            surface: Surface {
                rope,
                revision: 0,
                selection: Selection::default(),
                composition: None,
                preedit: None,
                geometry: Rc::new(RefCell::new(Geometry::default())),
                epoch: 0,
                input_started: None,
                reveal_caret: false,
                caret_visible: true,
                minimum_line_metrics: None,
                annotations: None,
                annotation_groups: None,
                selection_color: [0.; 4],
            },
            state: ViewportState::default(),
            fonts,
        }
    }
    pub fn reset_cache(&mut self) {
        self.state = ViewportState::default();
    }
    pub fn set_document(&mut self, rope: Rope, revision: u64) {
        self.surface.rope = rope;
        self.surface.revision = revision;
    }
    pub fn set_annotations(&mut self, annotations: Option<Arc<Annotations>>) {
        self.surface.annotations = annotations;
    }
    pub fn set_caret(&mut self, position: usize) {
        self.surface.selection = Selection {
            anchor: position,
            head: position,
        };
    }
    pub fn prepare(&mut self, viewport: [f32; 2], scroll_y: f32) -> FrameMetrics {
        self.prepare_at(viewport, [0., scroll_y])
    }
    pub fn prepare_at(&mut self, viewport: [f32; 2], offset: [f32; 2]) -> FrameMetrics {
        self.surface.geometry.borrow_mut().offset = offset;
        let started = Instant::now();
        self.state
            .prepare_cpu(&self.surface, viewport, &mut self.fonts)
            .unwrap();
        self.state.metrics.total_prepare_ms = started.elapsed().as_secs_f64() * 1000.;
        self.state.metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OverlayKind, OverlayPlacement, SpanStyle, StyledRange, TextOverlay};
    fn surface(source: &str) -> Surface {
        Surface {
            rope: Rope::from_str(source),
            revision: 0,
            selection: Selection::default(),
            composition: None,
            preedit: None,
            geometry: Rc::new(RefCell::new(Geometry::default())),
            epoch: 0,
            input_started: None,
            reveal_caret: false,
            caret_visible: true,
            minimum_line_metrics: None,
            annotations: None,
            annotation_groups: None,
            selection_color: [0.; 4],
        }
    }
    fn fonts() -> Option<TextSystem> {
        let mut fonts = TextSystem::new();
        if fonts.load_system_fonts().is_empty() {
            None
        } else {
            Some(fonts)
        }
    }
    #[test]
    fn line_composition_matches_full_layout_and_keeps_other_lines_cached() {
        let Some(mut fonts) = fonts() else {
            return;
        };
        let source = "aébc 👩‍💻 def\n".repeat(1000);
        let mut full = surface(&source);
        let start = full.rope.line_to_char(500);
        full.selection = Selection {
            anchor: start + 3,
            head: start + 1,
        };
        full.composition = Some(crate::surface::Composition {
            text: "かんじ".into(),
            cursor: Some((0, 9)),
        });
        full.reveal_caret = true;
        let mut annotations = Annotations::default();
        annotations.overlays.push(TextOverlay {
            anchor: start + 1,
            text: "hint".into(),
            kind: OverlayKind::InlayHint,
            placement: OverlayPlacement::Inline,
            foreground: [1.; 4],
            background: None,
            hover: None,
        });
        full.annotations = Some(Arc::new(annotations));
        assert_eq!(composition_line(&full), Some(500));
        let mut reference = SurfaceState::default();
        full.shape(&mut reference, 608., &mut fonts).unwrap();
        let reference_caret = full.candidate_caret(&reference).unwrap();
        let mut state = ViewportState::default();
        state.prepare_cpu(&full, [640., 240.], &mut fonts).unwrap();
        let entry = &state.cache[&500];
        let caret = entry.document_caret(full.selection.head, state.heights.top(500));
        assert!((caret.x - reference_caret.x).abs() < 0.01);
        assert!((caret.y - reference_caret.y).abs() < 0.01);
        assert_eq!(entry.surface.selection, Selection { anchor: 3, head: 1 });
        assert_eq!(entry.surface.rope.to_string(), "aébc 👩‍💻 def");
        assert!(state.metrics.cached_source_bytes < 2000);
        assert!(state.metrics.shaped_lines < 30);
        let other = state.visible.clone().find(|&line| line != 500).unwrap();
        let layout = state.cache[&other]
            .surface
            .layout(&state.cache[&other].state)
            .clone();
        full.composition.as_mut().unwrap().text = "にほん".into();
        full.epoch += 1;
        state.prepare_cpu(&full, [640., 240.], &mut fonts).unwrap();
        assert_eq!(state.metrics.shaped_lines, 1);
        assert!(Arc::ptr_eq(
            &layout,
            state.cache[&other]
                .surface
                .layout(&state.cache[&other].state)
        ));
        assert_eq!(full.rope.to_string(), source);
        full.composition = None;
        full.epoch += 1;
        state.prepare_cpu(&full, [640., 240.], &mut fonts).unwrap();
        assert!(state.cache[&500].surface.composition.is_none());
    }

    #[test]
    fn unicode_window_cache_handles_metric_and_directional_fallback_transitions() {
        let Some(mut fonts) = fonts() else {
            return;
        };
        let source = format!("{}日本語 👩‍💻{}", "x".repeat(5000), "y".repeat(4000));
        let mut full = surface(&source);
        let mut state = ViewportState::default();
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        assert_eq!(state.metrics.unicode_horizontal_lines, 1);
        assert!(state.metrics.cached_source_bytes < 4000);
        full.geometry.borrow_mut().offset[0] = 49_000.;
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        assert!(state.cache[&0].text.contains("日本語"));
        assert!(state.cache[&0].text.contains("👩‍💻"));
        full.annotations = Some(Arc::new(Annotations {
            spans: vec![StyledRange::new(
                0..2,
                SpanStyle {
                    font_size: Some(32.),
                    ..SpanStyle::default()
                },
            )],
            ..Annotations::default()
        }));
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        assert_eq!(state.metrics.unicode_horizontal_lines, 0);
        assert_eq!(state.cache[&0].text, source);
        full.annotations = None;
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        assert_eq!(state.metrics.unicode_horizontal_lines, 1);
        assert!(state.metrics.cached_source_bytes < 4000);
        full.rope = Rope::from_str(&source.replace("日本語", "אבג"));
        full.revision += 1;
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        assert_eq!(state.metrics.unicode_horizontal_lines, 0);
        assert!(state.cache[&0].unicode.is_none());
    }
    #[test]
    fn shrinking_a_line_rewindows_after_the_horizontal_scroll_clamp() {
        let Some(mut fonts) = fonts() else {
            return;
        };
        let mut full = surface(&"a".repeat(100_000));
        full.geometry.borrow_mut().offset[0] = 900_000.;
        let mut state = ViewportState::default();
        state.prepare_cpu(&full, [2600., 800.], &mut fonts).unwrap();
        full.rope = Rope::from_str(&"a".repeat(2000));
        full.revision += 1;
        full.epoch += 1;
        full.selection = Selection {
            anchor: 2000,
            head: 2000,
        };
        full.reveal_caret = true;
        state.prepare_cpu(&full, [2600., 800.], &mut fonts).unwrap();
        let entry = &state.cache[&0];
        let window = entry.window.as_ref().unwrap();
        assert!(
            window.left() <= full.geometry.borrow().offset[0],
            "prepared window misses the final viewport after clamping"
        );
    }

    #[test]
    fn rendered_rich_window_maps_visible_hint_hover_and_pointer_to_global_source() {
        let Ok(graphics) = pollster::block_on(rxui::astrelis::GraphicsContext::headless()) else {
            return;
        };
        let mut editor = crate::Editor::new(&"a".repeat(10_000));
        let mut annotations = Annotations::default();
        annotations.spans.push(StyledRange::new(
            0..3,
            SpanStyle {
                font_size: Some(32.),
                ..SpanStyle::default()
            },
        ));
        for anchor in (0..10_000).step_by(10) {
            annotations.overlays.push(TextOverlay {
                anchor,
                text: ": i32".into(),
                kind: OverlayKind::InlayHint,
                placement: OverlayPlacement::Inline,
                foreground: [1.; 4],
                background: Some([0., 0., 0., 1.]),
                hover: Some(crate::HoverCard {
                    title: "inlay".into(),
                    body: "source anchor".into(),
                }),
            });
        }
        editor.set_annotations(annotations).unwrap();
        let mut ui = rxui::TestUi::new(&graphics, [640, 240], editor).unwrap();
        if ui.painter_mut().fonts_mut().load_system_fonts().is_empty() {
            return;
        }
        ui.render().unwrap();
        let entity = ui.entity().clone();
        ui.runtime().update(|cx| {
            entity.update(cx, |editor, _| {
                editor.geometry.borrow_mut().offset[0] = 50_000.;
                editor.epoch += 1;
                editor.reveal_caret = false;
            })
        });
        ui.render().unwrap();
        ui.read(|editor| {
            let geometry = editor.geometry.borrow();
            let region = geometry
                .hover_regions
                .iter()
                .find(|region| {
                    region.rect.x > geometry.offset[0]
                        && region.rect.x + region.rect.width < geometry.offset[0] + 600.
                })
                .expect("visible projected hint");
            let point = [
                PADDING + region.rect.x + region.rect.width * 0.5 - geometry.offset[0],
                PADDING + region.rect.y + region.rect.height * 0.5,
            ];
            let hover = geometry.hover(point).unwrap();
            assert_eq!(hover.id, region.id);
            assert_eq!(hover.card.title, "inlay");
            let overlay = &editor.annotations.as_ref().unwrap().overlays[hover.id - 1];
            assert_eq!(geometry.hit(point), Some(overlay.anchor));
            assert_eq!(geometry.metrics.rich_horizontal_lines, 1);
            assert!(geometry.metrics.cached_source_bytes < 1024);
            assert_eq!(editor.buffer.text().len_chars(), 10_000);
            assert!(
                (geometry.lines[0]
                    .geometry
                    .borrow()
                    .layout
                    .as_ref()
                    .unwrap()
                    .lines()[0]
                    .height
                    - 48.)
                    .abs()
                    < 0.001
            );
        });
        ui.runtime()
            .update(|cx| entity.update(cx, |editor, _| editor.set_text("short")));
        ui.render().unwrap();
        ui.read(|editor| {
            assert_eq!(
                editor.geometry.borrow().offset[0],
                0.,
                "old decoration widths must not keep a short document scrolled"
            )
        });
    }

    #[test]
    fn rich_run_cache_reuses_index_and_invalidates_on_annotation_or_source_changes() {
        let Some(mut fonts) = fonts() else {
            return;
        };
        let mut full = surface(&"a".repeat(5000));
        let mut annotations = Annotations::default();
        annotations.overlays.push(TextOverlay {
            anchor: 2500,
            text: ": i32".into(),
            kind: OverlayKind::InlayHint,
            placement: OverlayPlacement::Inline,
            foreground: [1.; 4],
            background: None,
            hover: None,
        });
        full.annotations = Some(Arc::new(annotations));
        let mut state = ViewportState::default();
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        let first = state.cache[&0].rich.as_ref().unwrap().clone();
        full.geometry.borrow_mut().offset[0] = 20_000.;
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        assert!(Arc::ptr_eq(&first, state.cache[&0].rich.as_ref().unwrap()));
        assert_eq!(state.metrics.rich_horizontal_lines, 1);
        let mut changed = full.annotations.as_ref().unwrap().as_ref().clone();
        changed.overlays[0].text = "a much longer hint".into();
        full.annotations = Some(Arc::new(changed.clone()));
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        assert!(state.cache[&0].rich.as_ref().unwrap().width > first.width);
        changed.overlays[0].text = "Unicode: é".into();
        full.annotations = Some(Arc::new(changed));
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        assert!(state.cache[&0].rich.is_none());
        assert_eq!(state.cache[&0].text.len(), 5000);
        let mut crowded = Annotations::default();
        for _ in 0..300 {
            crowded.overlays.push(TextOverlay {
                anchor: 2500,
                text: ": i32".into(),
                kind: OverlayKind::InlayHint,
                placement: OverlayPlacement::Inline,
                foreground: [1.; 4],
                background: None,
                hover: None,
            });
        }
        full.annotations = Some(Arc::new(crowded));
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        assert!(state.cache[&0].rich.is_none());
        full.annotations = None;
        full.rope.insert(2500, "👩‍💻");
        full.revision += 1;
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        assert!(state.cache[&0].rich.is_none());
        assert!(state.cache[&0].window.is_none());
    }

    #[test]
    fn rich_index_matches_full_shaping_and_keeps_offscreen_font_baseline() {
        let Some(mut fonts) = fonts() else {
            return;
        };
        let mut full = surface(&"a".repeat(1536));
        let mut annotations = Annotations::default();
        annotations.spans.push(StyledRange::new(
            0..32,
            SpanStyle {
                font_size: Some(32.),
                weight: Some(700),
                ..SpanStyle::default()
            },
        ));
        annotations.spans.push(StyledRange::new(
            400..700,
            SpanStyle {
                italic: Some(true),
                ..SpanStyle::default()
            },
        ));
        annotations.spans.push(StyledRange::new(
            600..800,
            SpanStyle {
                font_size: Some(20.),
                ..SpanStyle::default()
            },
        ));
        for (id, anchor) in [0, 16, 16, 256, 512, 768, 1200, 1536]
            .into_iter()
            .enumerate()
        {
            annotations.overlays.push(TextOverlay {
                anchor,
                text: if id == 2 {
                    "second".into()
                } else {
                    ": i32".into()
                },
                kind: if id == 3 {
                    OverlayKind::GhostText
                } else {
                    OverlayKind::InlayHint
                },
                placement: OverlayPlacement::Inline,
                foreground: [1.; 4],
                background: None,
                hover: None,
            });
        }
        let (_, projection) = crate::inline::InlineProjection::build(
            &full.rope.to_string(),
            annotations
                .overlays
                .iter()
                .enumerate()
                .map(|(id, o)| (o.anchor, id, o.text.as_str()))
                .collect(),
        );
        full.annotations = Some(Arc::new(annotations));
        let mut reference = SurfaceState::default();
        full.shape(&mut reference, 1200., &mut fonts).unwrap();
        let natural = full.layout(&reference).clone();
        let mut state = ViewportState::default();
        full.geometry.borrow_mut().offset[0] = 10_000.;
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        let entry = &state.cache[&0];
        let rich = entry.rich.as_ref().expect("rich run index");
        assert!(entry.columns().start > 32);
        assert!(entry.text.len() < 1024);
        let slice = entry.surface.layout(&entry.state);
        assert!((slice.lines()[0].height - natural.lines()[0].height).abs() < 0.001);
        assert!((slice.lines()[0].baseline - natural.lines()[0].baseline).abs() < 0.001);
        assert!(
            (rich.width - natural.size()[0]).abs() < 0.05,
            "{} {}",
            rich.width,
            natural.size()[0]
        );
        for position in (0..=1536)
            .step_by(17)
            .chain([0, 16, 256, 512, 768, 1200, 1536])
        {
            let expected = natural
                .caret(rxui::astrelis::TextPosition::new(
                    projection.display_offset(position, true),
                ))
                .unwrap();
            assert!(
                (rich.caret(position) - expected.origin[0]).abs() < 0.05,
                "{position}: {} {}",
                rich.caret(position),
                expected.origin[0]
            );
        }
        for hint in &projection.insertions {
            let a = natural
                .caret(rxui::astrelis::TextPosition::new(hint.display.start))
                .unwrap();
            let b = natural
                .caret(rxui::astrelis::TextPosition::new(hint.display.end))
                .unwrap();
            let x = (a.origin[0] + b.origin[0]) * 0.5;
            let hit = natural.hit_test([x, 12.]).unwrap();
            assert_eq!(rich.hit(x), projection.base_offset(hit.byte_offset));
        }
    }

    #[test]
    fn rich_windows_include_eof_hints_and_offscreen_backgrounds_in_provider_order() {
        let Some(mut fonts) = fonts() else {
            return;
        };
        let mut full = surface(&format!("{}\n", "a".repeat(4096)));
        let mut annotations = Annotations::default();
        annotations.spans.push(StyledRange::new(
            0..1,
            SpanStyle {
                full_line_background: true,
                background: Some([0.2, 0., 0., 1.]),
                ..SpanStyle::default()
            },
        ));
        annotations.spans.push(
            StyledRange::new(
                3000..4096,
                SpanStyle {
                    background: Some([0., 0.2, 0., 1.]),
                    ..SpanStyle::default()
                },
            )
            .with_hover("range", "correct provider"),
        );
        annotations.overlays.push(TextOverlay {
            anchor: 4096,
            text: "EOF".into(),
            kind: OverlayKind::InlayHint,
            placement: OverlayPlacement::Inline,
            foreground: [1.; 4],
            background: None,
            hover: Some(crate::HoverCard {
                title: "hint".into(),
                body: "at EOF".into(),
            }),
        });
        full.annotations = Some(Arc::new(annotations));
        full.selection = Selection {
            anchor: 0,
            head: 4096,
        };
        full.epoch = 1;
        full.reveal_caret = true;
        let mut state = ViewportState::default();
        state.prepare_cpu(&full, [1200., 800.], &mut fonts).unwrap();
        let entry = &state.cache[&0];
        assert_eq!(entry.columns().end, 4096);
        let local = entry.surface.annotations.as_ref().unwrap();
        assert_eq!(local.overlays.len(), 1);
        assert_eq!(local.overlays[0].anchor, entry.text.len());
        assert_eq!(entry.hover_ids, vec![0, 1, 2]);
        assert!(local.spans[0].style.full_line_background);
        assert_eq!(local.spans[0].range, 0..entry.text.len());
        assert!(local.spans[0].hover.is_none());
        assert_eq!(entry.surface.selection.range(), 0..entry.text.len());
        let rich = entry.rich.as_ref().unwrap();
        assert_eq!(full.geometry.borrow().caret.unwrap().x, rich.width);
        assert_eq!(rich.hit(rich.width + 100.), 4096);
    }

    #[test]
    fn horizontal_windows_cover_viewport_and_keep_bounded_overscan() {
        for offset in [0., 1., 1279., 500_000., 999_990.] {
            let window = HorizontalWindow::new(100_000, 10., offset, 1200.);
            assert!(window.range.len() <= 512);
            assert!(window.left() <= offset);
            assert!(
                window.range.end == window.full_len
                    || window.range.end as f32 * window.cell >= offset + 1200.
            );
        }
    }

    #[test]
    fn long_lines_reuse_windows_and_reveal_real_document_end() {
        let Some(mut fonts) = fonts() else {
            return;
        };
        let mut surface = surface(&"a".repeat(100_000));
        let mut state = ViewportState::default();
        state
            .prepare_cpu(&surface, [1200., 800.], &mut fonts)
            .unwrap();
        let first = &state.cache[&0];
        let cell = first.window.as_ref().expect("verified mono font").cell;
        assert!(first.text.len() <= 512);
        assert!(first.surface.layout(&first.state).glyphs().len() <= 512);
        assert!((surface.geometry.borrow().content_size[0] - cell * 100_000.).abs() < 1.);
        let layout = first.surface.layout(&first.state).clone();
        surface.geometry.borrow_mut().offset[0] = cell * 4.;
        state
            .prepare_cpu(&surface, [1200., 800.], &mut fonts)
            .unwrap();
        assert!(Arc::ptr_eq(
            &layout,
            state.cache[&0].surface.layout(&state.cache[&0].state)
        ));
        surface.selection = Selection {
            anchor: 0,
            head: 100_000,
        };
        surface.epoch += 1;
        surface.reveal_caret = true;
        state
            .prepare_cpu(&surface, [1200., 800.], &mut fonts)
            .unwrap();
        let last = &state.cache[&0];
        assert_eq!(last.columns().end, 100_000);
        assert!(last.text.len() <= 512);
        assert_eq!(last.surface.selection.range(), 0..last.text.len());
        assert!(last.surface.caret_visible);
        let geometry = surface.geometry.borrow();
        assert!(geometry.offset[0] > 900_000.);
        assert!((geometry.caret.unwrap().x - 100_000. * cell).abs() < 1.);
        assert_eq!(surface.rope.len_chars(), 100_000);
    }

    #[test]
    fn grid_hits_remain_document_relative_after_horizontal_scroll() {
        let Some(mut fonts) = fonts() else {
            return;
        };
        let mut surface = surface(&format!("prefix\n{}", "a".repeat(100_000)));
        surface.selection = Selection { anchor: 7, head: 7 };
        let mut state = ViewportState::default();
        state
            .prepare_cpu(&surface, [1200., 800.], &mut fonts)
            .unwrap();
        let cell = state.cache[&1].window.as_ref().unwrap().cell;
        surface.geometry.borrow_mut().offset[0] = 50_000. * cell;
        state
            .prepare_cpu(&surface, [1200., 800.], &mut fonts)
            .unwrap();
        let line = &state.cache[&1];
        assert!(line.columns().start > 49_000);
        let mut geometry = surface.geometry.borrow_mut();
        geometry.lines = vec![LineHit {
            top: LINE_HEIGHT,
            start_char: 7,
            left: line.left(),
            grid: Some((cell, line.full_len)),
            rich: None,
            unicode: None,
            geometry: line.surface.geometry.clone(),
        }];
        assert_eq!(
            geometry.hit([PADDING + cell * 5., PADDING + LINE_HEIGHT + 12.]),
            Some(50_012)
        );
        // Pointer capture can move outside the viewport; use the full source grid.
        assert_eq!(
            geometry.hit([PADDING + cell * 70_000., PADDING + LINE_HEIGHT + 12.]),
            Some(100_007)
        );
    }

    #[test]
    fn horizontal_styles_clip_with_provider_order_and_complex_lines_fall_back() {
        let Some(mut fonts) = fonts() else {
            return;
        };
        let mut surface = surface(&"a".repeat(10_000));
        let mut annotations = Annotations::default();
        annotations
            .spans
            .push(StyledRange::new(0..2, SpanStyle::default()));
        annotations.spans.push(
            StyledRange::new(
                4000..6000,
                SpanStyle {
                    foreground: Some([1., 0., 0., 1.]),
                    background: Some([0., 0., 0., 1.]),
                    ..SpanStyle::default()
                },
            )
            .with_hover("style", "clipped provider"),
        );
        surface.annotations = Some(Arc::new(annotations));
        let mut state = ViewportState::default();
        state
            .prepare_cpu(&surface, [1200., 800.], &mut fonts)
            .unwrap();
        let cell = state.cache[&0].window.as_ref().unwrap().cell;
        surface.geometry.borrow_mut().offset[0] = 5000. * cell;
        state
            .prepare_cpu(&surface, [1200., 800.], &mut fonts)
            .unwrap();
        let entry = &state.cache[&0];
        let spans = &entry.surface.annotations.as_ref().unwrap().spans;
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].range, 0..entry.text.len());
        assert_eq!(
            state.index.hover_id(0, entry.annotation_range.clone(), 0),
            1
        );
        let mut mixed = (*surface.annotations.as_ref().unwrap().as_ref()).clone();
        mixed.spans[1].style.font_size = Some(24.);
        surface.annotations = Some(Arc::new(mixed));
        state
            .prepare_cpu(&surface, [1200., 800.], &mut fonts)
            .unwrap();
        assert!(state.cache[&0].window.is_none());
        surface.annotations = None;
        surface.rope.insert(3, "é");
        surface.revision += 1;
        state
            .prepare_cpu(&surface, [1200., 800.], &mut fonts)
            .unwrap();
        assert!(state.cache[&0].window.is_none());
        surface.rope.remove(3..4);
        surface.revision += 1;
        state
            .prepare_cpu(&surface, [1200., 800.], &mut fonts)
            .unwrap();
        assert!(state.cache[&0].window.is_some());
        assert!(state.cache[&0].text.len() <= 512);
        let mut hints = Annotations::default();
        hints.overlays.push(TextOverlay {
            anchor: 5,
            text: ": i32".into(),
            kind: OverlayKind::InlayHint,
            placement: OverlayPlacement::Inline,
            foreground: [1.; 4],
            background: None,
            hover: None,
        });
        surface.annotations = Some(Arc::new(hints));
        state
            .prepare_cpu(&surface, [1200., 800.], &mut fonts)
            .unwrap();
        assert!(state.cache[&0].window.is_none());
    }

    #[test]
    fn interval_queries_match_provider_order_with_document_wide_overlaps() {
        let mut surface = surface(&"x\n".repeat(1000));
        let mut annotations = Annotations::default();
        annotations
            .spans
            .push(StyledRange::new(0..2000, SpanStyle::default()));
        for line in (0..1000).rev() {
            annotations.spans.push(StyledRange::new(
                line * 2..line * 2 + 1,
                SpanStyle::default(),
            ));
        }
        surface.annotations = Some(Arc::new(annotations));
        let mut index = AnnotationIndex::default();
        index.update(&surface.annotations, &surface.rope);
        for line in [0, 1, 500, 999] {
            let range = line * 2..line * 2 + 2;
            let expected = surface
                .annotations
                .as_ref()
                .unwrap()
                .spans
                .iter()
                .enumerate()
                .filter(|(_, s)| s.range.start < range.end && s.range.end > range.start)
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            assert_eq!(index.span_ids(&range), expected);
        }
    }
    #[test]
    fn eof_hints_belong_to_final_empty_line_after_lf() {
        let mut surface = surface("x\n");
        surface.annotations = Some(Arc::new(Annotations {
            document_revision: 0,
            spans: Vec::new(),
            overlays: vec![TextOverlay {
                anchor: 2,
                text: "end".into(),
                kind: OverlayKind::InlayHint,
                placement: OverlayPlacement::Inline,
                foreground: [1.; 4],
                background: None,
                hover: None,
            }],
        }));
        let mut index = AnnotationIndex::default();
        index.update(&surface.annotations, &surface.rope);
        assert!(index.line(0, 0..2).is_none());
        assert_eq!(index.line(1, 2..2).unwrap().overlays[0].anchor, 0);
    }
    #[test]
    fn heights_find_mixed_size_lines_and_update_prefixes() {
        let mut heights = Heights::new(10);
        heights.set(2, 48.);
        heights.set(5, 72.);
        assert_eq!(heights.top(3), 96.);
        assert_eq!(heights.top(10), 312.);
        for line in 0..10 {
            assert_eq!(heights.line_at(heights.top(line)), line);
            assert_eq!(heights.line_at(heights.top(line) + 1.), line);
        }
        assert_eq!(heights.line_at(10000.), 9);
        heights.set(2, 24.);
        assert_eq!(heights.top(3), 72.);
    }
    #[test]
    fn interval_index_preserves_provider_order_and_anchor_line() {
        let mut surface = surface("one\ntwo\nthree");
        surface.annotations = Some(Arc::new(Annotations {
            document_revision: 0,
            spans: vec![
                StyledRange::new(4..7, SpanStyle::default()),
                StyledRange::new(0..13, SpanStyle::default()),
                StyledRange::new(8..13, SpanStyle::default()),
            ],
            overlays: vec![TextOverlay {
                anchor: 4,
                text: ": hint".into(),
                kind: OverlayKind::InlayHint,
                placement: OverlayPlacement::Inline,
                foreground: [1.; 4],
                background: None,
                hover: None,
            }],
        }));
        let mut index = AnnotationIndex::default();
        index.update(&surface.annotations, &surface.rope);
        let line = index.line(1, 4..8).unwrap();
        assert_eq!(line.spans[0].range, 0..3);
        assert_eq!(line.spans[1].range, 0..4);
        assert_eq!(line.overlays[0].anchor, 0);
        assert_eq!(index.hover_id(1, 4..8, 0), 0);
        assert_eq!(index.hover_id(1, 4..8, 1), 1);
        assert_eq!(index.hover_id(1, 4..8, 2), 3);
    }
    #[test]
    fn caret_and_single_line_edits_reuse_unaffected_layouts() {
        let Some(mut fonts) = fonts() else {
            return;
        };
        let mut surface = surface(&"let value = 42;\n".repeat(10000));
        let mut state = ViewportState::default();
        state
            .prepare_cpu(&surface, [800., 600.], &mut fonts)
            .unwrap();
        assert!(state.metrics.shaped_lines < 40);
        let unchanged = state.cache[&3]
            .surface
            .layout(&state.cache[&3].state)
            .clone();
        surface.selection = Selection { anchor: 3, head: 3 };
        state
            .prepare_cpu(&surface, [800., 600.], &mut fonts)
            .unwrap();
        assert_eq!(state.metrics.shaped_lines, 0);
        surface.rope.insert(2, "x");
        surface.revision += 1;
        state
            .prepare_cpu(&surface, [800., 600.], &mut fonts)
            .unwrap();
        assert_eq!(state.metrics.shaped_lines, 1);
        assert!(Arc::ptr_eq(
            &unchanged,
            state.cache[&3].surface.layout(&state.cache[&3].state)
        ));
    }
    #[test]
    fn equal_annotations_reuse_inline_projection_and_changed_line_only_reshapes() {
        let Some(mut fonts) = fonts() else {
            return;
        };
        let mut surface = surface("let x = 1;\nlet y = 2;\n");
        let make = |color| {
            Arc::new(Annotations {
                document_revision: 0,
                spans: vec![StyledRange::new(
                    0..11,
                    SpanStyle {
                        foreground: Some(color),
                        ..SpanStyle::default()
                    },
                )],
                overlays: vec![TextOverlay {
                    anchor: 5,
                    text: ": i32".into(),
                    kind: OverlayKind::InlayHint,
                    placement: OverlayPlacement::Inline,
                    foreground: [1.; 4],
                    background: None,
                    hover: None,
                }],
            })
        };
        surface.annotations = Some(make([1.; 4]));
        let mut state = ViewportState::default();
        state
            .prepare_cpu(&surface, [800., 600.], &mut fonts)
            .unwrap();
        state
            .prepare_cpu(&surface, [800., 600.], &mut fonts)
            .unwrap();
        assert_eq!(state.metrics.shaped_lines, 0);
        surface.annotations = Some(make([1.; 4]));
        state
            .prepare_cpu(&surface, [800., 600.], &mut fonts)
            .unwrap();
        assert_eq!(state.metrics.shaped_lines, 0);
        surface.annotations = Some(make([0.5; 4]));
        state
            .prepare_cpu(&surface, [800., 600.], &mut fonts)
            .unwrap();
        assert_eq!(state.metrics.shaped_lines, 1);
    }
    #[test]
    fn scrolling_keeps_cache_bounded_and_document_end_reachable() {
        let Some(mut fonts) = fonts() else {
            return;
        };
        let surface = surface(&"hello\n".repeat(10000));
        let mut state = ViewportState::default();
        for line in (0..10000).step_by(100) {
            surface.geometry.borrow_mut().offset[1] = line as f32 * LINE_HEIGHT;
            state
                .prepare_cpu(&surface, [800., 600.], &mut fonts)
                .unwrap();
            assert!(state.cache.len() <= CACHE_LIMIT);
        }
        surface.geometry.borrow_mut().offset[1] = 240000.;
        state
            .prepare_cpu(&surface, [800., 600.], &mut fonts)
            .unwrap();
        assert_eq!(state.visible.end, 10001);
        assert!(surface.geometry.borrow().offset[1] > 239000.);
    }
}
