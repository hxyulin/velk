# Editor prototype performance

Measured on an Apple M3 Pro (macOS arm64), 2026-10-15, with Rust 1.99.0,
optimized builds, RXUI pinned to `093d05d9a2fa5491c3cebe26be46b8720b4c6158`,
and its pinned Astrelis dependency. The target is **120 Hz: 8.33 ms per frame**.
CPU preparation below that budget does not establish 120 Hz presentation.

## Viewport renderer results

The widget now shapes visible logical lines plus four lines of overscan on each
side, and the caret line when necessary for keyboard navigation. Layouts, inline
projections, decoration geometry, and prepared GPU text are cached by logical
line. Normal same-line edits retain unaffected layouts; insertions/deletions of
lines may rebuild the visible lines whose contents moved. The cache retains up
to 256 lines under normal viewport sizes; a viewport needing more lines necessarily
retains its visible lines plus the caret. A per-document height index stores
measured deviations from the 24-pixel default, without retaining document glyphs.
It provides logarithmic line/offset lookup and keeps the top source line anchored
as mixed-size lines are measured. Unvisited heights are estimates; horizontal
scroll extent follows measured cached lines and overlays.

Styles use an augmented interval index, so a document-wide style does not force
scanning every earlier local style. Overlays use a compact sorted anchor index.
Provider order is retained for overlapping styles and hints at equal anchors.
A new immutable annotation snapshot is indexed once; equal per-line results keep
existing layout resources. Source ranges remain independent of viewport coordinates.

### CPU benchmark

```sh
cargo bench -p velk-editor-widget --bench viewport --features performance --locked
```

Raw data: [editor-viewport.csv](editor-viewport.csv). The fixture repeats a
25-byte ASCII line and uses a 1200 × 800 logical-pixel viewport. The full-document
reference and viewport edit both change a character in the first line, with the
same font, size, line height, and disabled ligatures. Rich cases add one bold
foreground span and one inline hint **on every source line**. This is denser than
the historical rich benchmark below and must not be compared as the same workload.

| CPU operation, median | 1,000 lines | 10,000 lines | 100,000 lines |
| --- | ---: | ---: | ---: |
| Full-document edit + layout reference | 1.363 ms | 15.708 ms | 182.086 ms |
| Edit + cached viewport layout | 0.021 ms | 0.022 ms | 0.023 ms |
| Cold rich/inline viewport | 0.689 ms | 0.757 ms | 1.505 ms |
| Warm rich/inline viewport | 0.027 ms | 0.030 ms | 0.032 ms |
| Replace rich snapshot + visible layout | 0.170 ms | 0.216 ms | 0.968 ms |
| Scroll + visible layout | 0.747 ms | 0.758 ms | 0.774 ms |

At 100,000 lines, p95 was 0.023 ms for a viewport edit, 1.675 ms for a cold
rich viewport, 2.335 ms for snapshot replacement, and 0.844 ms for scrolling.
Samples: 101 for warm/edit/caret cases, 21 for full-document/cold cases, and 31
for snapshot/scroll cases. The reported p95 is an order statistic, not a confidence
interval. System load and font/glyph cache warmth affect separate runs.

The viewport benchmark executes the same CPU source extraction, annotation
queries, inline projection, shaping, interaction-index construction, line-height
lookup, and cache eviction path used by the widget. It excludes annotation
validation, provider construction, GPU preparation, decoration/selection rectangle
preparation, draw recording, GPU execution, window presentation, and native input
dispatch. Immutable replacement snapshots are prebuilt outside the timed section.
Cold cases reset the line/annotation cache while retaining loaded fonts.
Scroll samples advance by 40 logical lines; the small 100-line case mostly reuses
cached lines after its initial passes. The reference's fonts are independently
loaded; neither fixture times font discovery.

Two pathological single-line cases still exceed 8.33 ms: editing/shaping a
100,000-character line measured **13.684 ms median / 14.677 ms p95**; a
10,000-character line with 1,000 styles and inline hints measured
**15.725 ms median / 15.935 ms p95**. The former takes 21 samples and the latter
11. Inline coordinate lookup now uses binary search, and span queries skip
unrelated hint anchors; remaining cost includes whole-line shaping and the
backend's rich-span processing. These CPU timings exclude GPU preparation.

### Native release checks

```sh
VELK_PROFILE=1 cargo run --release --locked -- --stress
VELK_PROFILE=1 cargo run --release --locked -- --showcase
```

`--stress` opens 100,000 lines (2.5 MB), with a style and inline hint on every
line. `Editor::frame_metrics()` exposes per-frame counters and timings; the
opt-in environment variable logs preparation and paint recording. GPU preparation
means CPU resource/upload submission work, **not GPU execution time**. Native
handler-to-preparation timing excludes presentation and the OS input queue.
The stored [native observations](editor-native-viewport.csv) are interactive
spot checks, not a controlled latency distribution or sustained frame-rate test.

Representative release observations from the stress fixture:

- Initial 38-line preparation: 4.336 ms, including indexing 100,000 styles/hints,
  shaping, decoration geometry, and preparing text for the GPU.
- First edit cleared the annotation snapshot: 7.575 ms, including 4.792 ms in
  snapshot retirement/index reset. All 44 visible/overscan lines then changed
  appearance and required preparation. Later edits prepared one line in roughly
  0.24–0.30 ms in those observations.
- Jumping to the document end prepared 43 new lines in 1.799 ms.
- Warm frames reused layouts and prepared text; no lines were shaped or uploaded.
  The separate rich-playground checks recorded draw commands in roughly
  0.04–0.08 ms in ordinary warm frames. These values are not GPU completion times.

Native checks exercised source selection beside inline hints, editing, document-end
navigation, hover cards, line-end placement, and dark/light appearance changes.
Windowed automation produced stale frames despite source state updates;
checks with the native window fully visible rendered updates correctly. The
precise window eligibility condition was not captured in those earlier checks. The
pinned Astrelis host intentionally skips preparation/presentation when its cached
window eligibility (including occlusion) is false. No deferred-input delay or
continuous redraw workaround was added to the widget.

## Grapheme navigation and idle work

A bounded eight-line LRU cache now stores grapheme boundaries for navigation and
annotation validation. It is invalidated before mutations and after undo/redo,
so combining marks, ZWJ sequences, and Rope chunk boundaries remain correct.
Warm left/right movement on one 100,000-character line measured roughly
0.00025 ms, versus the earlier 3.912 ms scan-based median. The precise sub-microsecond
value is sensitive to timer resolution; the useful result is removal of repeated
full-line segmentation. Cold boundary discovery and editing that changes a very
long line still scan that logical line. Raw post-change run:
[editor-grapheme-cache.csv](editor-grapheme-cache.csv).

Caret movement/blinking reuse shaped text. Selection geometry is rebuilt only
for changed selections/layouts; drawing is restricted to visible lines. Blink
and hover tasks retain their 500 ms and 350 ms delays. Unfocused editors request
no blink redraw. No continuous animation loop was introduced.

## Remaining work toward a verified 120 Hz editor

1. **IME virtualization:** active composition uses the proven whole-document
   renderer to preserve cross-line replacements and preedit behavior. Its large-file
   cost is explicitly flagged by `composition_fallback`; full-document resources
   are released when composition ends. Virtualizing this path needs dedicated
   cross-line composition geometry tests.
2. **Long-line shaping:** a visible logical line is still shaped in full. Horizontal
   chunking requires preserving bidi, ligatures, grapheme edges, and tab alignment.
   The navigation cache alone does not solve this rendering case.
3. **Snapshot retirement:** replacing/clearing very large provider snapshots still
   performs document-scale indexing/deallocation. Consider off-thread construction
   and retirement or provider deltas before adding live LSP churn.
4. **Presentation and memory profiling:** add GPU timestamps/presentation tracing
   and controlled sustained-scroll/input sampling. Current counters measure retained
   source bytes/line counts, not allocator activity, RSS, atlas memory, GPU execution,
   or actual display frame cadence. `xctrace` is unavailable in this environment,
   so an Instruments GPU/presentation capture was not possible in this pass. Glyph atlases are owned by Astrelis and may
   outlive evicted prepared line resources.
5. **Estimated geometry:** refine unvisited mixed-size line heights and horizontal
   extents if scrollbar accuracy becomes a product requirement. Document-wide
   height/index metadata remains O(number of lines/annotations); expensive text/GPU
   resources are restricted to cached lines.

## Historical full-document baseline

| Document | Bytes | Insert + undo | Text edit + CPU layout | Rich style update + CPU layout |
| --- | ---: | ---: | ---: | ---: |
| 100 lines | 4,700 | 0.00146 ms | 0.272 ms | 0.253 ms |
| 1,000 lines | 47,000 | 0.00142 ms | 2.502 ms | 2.704 ms |
| 10,000 lines | 470,000 | 0.00133 ms | 29.123 ms | 46.203 ms |

The corresponding p95 layout times for 10,000 lines were 32.709 ms (text edit)
and 66.708 ms (rich style update). A 60 Hz frame is 16.67 ms. Layout alone exceeds
that budget in this workload, before GPU preparation or drawing.

## What the benchmark includes

- Each document repeats a 47-byte ASCII source line, with a middle-document edit.
- Insert + undo includes Ropey mutation, a history snapshot, caret snapping, and
  restoration. It takes 101 samples per document size.
- Rope-to-String materialization is timed separately with 101 samples. At 10,000
  lines its median was 0.01475 ms; shaping/layout is the larger cost.
- Text edit + CPU layout includes materialization, changing one character,
  `TextBuffer::set_text`, layout evaluation, and building the interaction index.
  It takes 21 samples per size.
- Rich style update + CPU layout changes the foreground color of one bold span
  every five source lines (20, 200, and 2,000 spans), then evaluates rich text and
  its interaction index. It takes 11 samples per size; its p95 is the maximum
  sample at that sample count. It does not change source text.
- Cached layout measures fetching an existing `Arc<TextLayout>`. It is near the
  timer's measurement floor; the individual nanosecond values are not useful as
  a throughput claim.
- A left/right movement pair on a single 100,000-character line had a 3.912 ms
  median. Navigation currently materializes and segments the current logical
  line, including building a boundary vector.

Excluded: GPU upload, glyph rasterization, render-pass recording, drawing,
window presentation, annotation validation/projection, custom decoration
geometry, overlay text preparation, hover card layout, and input dispatch.
The rich benchmark measures Astrelis rich text, not the full widget pipeline.
These historical timings do not include inline hints or the viewport renderer.

A subsequent local backend pass is recorded in [the Astrelis report](astrelis-local.md).
