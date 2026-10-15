# Phase-aware coverage text

Velk pins RXUI `522d78525ee7f4762689175b1e44f097d8f0c715`, which pins Astrelis
`5e5ea9df12f3539661b8d487a83b907e63641bfa`. No local path patches are needed.

Astrelis rasterizes small outline glyphs at a quarter-pixel phase matching their
final physical placement and places the coverage quads on the physical pixel
grid. This avoids linearly shifting an already antialiased zero-phase mask.
MSAA, font choice, colors and shaping policy are unchanged. Quantization affects
ink by at most 1/8 physical pixel per axis; source positions, advances, selection,
hit testing and IME caret geometry remain fractional.

RXUI supplies the final origin for built-in text, rich text, placeholders and
paragraph chunks, including a single-line input's horizontal scroll. It retains
prepared text across whole physical-pixel translation and color/selection changes.
Fractional phase changes replace GPU geometry only for ink that can reach the
clip. Offscreen resources defer that work until revealed, and wrapped chunks
check their own phase when the visible clip changes. Cropped opacity layers shift
by whole physical pixels, preserving phase alignment.

The editor's custom surface uses `CustomPrepare::text_raster_options(origin)`.
For a virtualized line, the final logical origin is:

```text
x = element.x + padding + horizontal_window.left - scroll.x
y = element.y + padding + line.top - scroll.y
```

The ordinary surface computes its origin after scroll clamping/caret reveal.
Each non-inline overlay adds its own rectangle and text insets. Main text and
overlays retain shaped layouts; only their prepared GPU resources change when
physical phase or density changes. Inline overlays are already in the projected
line layout. IME projected viewports follow the same path. The cache key keeps
the fractional physical origin, rather than rounding only the origin into bins:
glyphs have different fractional local positions and can cross phase boundaries
at different times.

Validation for this integration:

- RXUI: all 355 library tests, including 38 GPU/installed-font tests, passed.
  New tests compare UI output with explicitly placed Astrelis text at 1x/2x,
  verify integer-translation reuse, fractional-origin invalidation, cropped opacity
  layers, placeholders, input caret scrolling, wrapped chunks and deferred chunk
  preparation after a clip change with the same base phase.
- The 1,000-row retained-list culling regression bounds fractional-scroll geometry
  preparation to the visible rows rather than rebuilding every retained row.
- Velk: the surface/overlay GPU regression verifies phase after scrolling at 1x/2x,
  whole-pixel cache reuse and retained layout identity. Existing viewport, rich-text,
  hover and IME tests cover the surrounding editor paths.
- Native preview: opened the real editor `main.rs` with Rust highlighting and the
  expanded mock Files panel. No source file was modified by the preview.

These are correctness and cache-reuse checks, not a new latency or frame-rate
benchmark. Astrelis's matched before/after captures and direct-Swash reference
are in its `docs/text-coverage/` directory. Arbitrary custom rotation, shear or
nonuniform scaling should keep Astrelis's default filtered preparation or follow
its explicit final-transform contract; the RXUI helper assumes its uniform scale.
