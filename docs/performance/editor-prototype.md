# Editor prototype performance

Measured on an Apple M3 Pro (macOS arm64), 2026-10-15, with Rust 1.99.0,
`cargo bench`'s optimized profile, RXUI pinned to
`093d05d9a2fa5491c3cebe26be46b8720b4c6158`, and its pinned Astrelis dependency.
These are local CPU timings, not end-to-end editor latency or frame-rate measurements.

Run the benchmark with:

```sh
cargo bench -p velk-editor-widget --bench editing
```

The harness prints CSV with median and p95 milliseconds. The original baseline
is in [editor-baseline.csv](editor-baseline.csv); the run including rich text is
in [editor-rich-text.csv](editor-rich-text.csv). Separate runs vary with system
load and do not establish a statistically significant speedup or regression.

## Latest run

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

## Cache behavior and idle work

The widget retains the rope, CPU text buffer, shaped layout, GPU prepared text,
annotation geometry, hover regions, and prepared overlay text. Caret movement
and blinking reuse the shaped text. Selection rectangles are rebuilt only when
the layout, selection, or IME composition changes. Decoration geometry and
prepared overlay labels are rebuilt when their annotation snapshot, source
layout, or raster scale changes. Background-only changes can retain the same
shaped text because no font/foreground spans changed.

Caret blinking uses a 500 ms task rather than requesting continuous animation
frames. The timer still wakes twice a second while mounted, but requests no
redraw while the editor is unfocused. Hover cards use a cancellable 350 ms delay;
motion over the same region does not invalidate the editor. Wavy/dashed lines
only issue paint operations for the visible horizontal range, and offscreen
background/overlay draws are culled.

The current text renderer still shapes the full document and prepares/draws
whole-document glyph resources. Rich text, GPU work, and large numbers of
annotations will increase costs beyond the measurements above. Full document
range caches also grow with the number of spans.

## Next optimizations

1. Cache and shape visible logical lines, with a small overscan region. Keep
   document anchors independent of the viewport and invalidate affected lines.
2. Use chunk-aware grapheme cursors or cached line boundaries for long lines.
3. Index spans/overlays by line and visible range. Dense style updates currently
   pass through Astrelis's segment construction, which scans spans for each
   segment; this becomes expensive with many overlapping spans.
4. Measure GPU preparation, draw recording, allocations, and native input-to-frame
   latency before claiming large-file or 60/120 Hz performance.

The current implementation is appropriate for exploring rich text behavior on
small documents. The rope buffer is not the primary bottleneck in these cases.
