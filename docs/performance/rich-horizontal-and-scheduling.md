# Rich horizontal windows and native scheduling investigation

> Historical analysis: raw measurements, capture manifests, and rendered reports
> are generated locally and are not tracked. Artifact filenames below identify
> the original runs; the reported values describe those runs. See the
> [profiling guide](../profiling.md#generated-artifacts) for new captures.

Follow-up: [input responsiveness and IME preparation](responsiveness.md) implements
same-frame measured IME publication and line-local composition, with native and
offscreen interaction benchmarks. Dependency pins below describe this earlier pass.

2026-10-15. This follows the fixed-grid pass in Velk `8e5b26d`. RXUI remains pinned
to `093d05d9a2fa5491c3cebe26be46b8720b4c6158`. Astrelis changes are published on
[`perf/editor-text`](https://github.com/hxyulin/astrelis/tree/perf/editor-text):

- `a2ab5e8`: minimum line metrics for horizontal slices.
- `a9cc220`: opt-in native frame-stage timing.
- `b66648b`: native profiling documentation.

Velk still patches both Astrelis crates to the sibling checkout. That checkout
must include these commits; Cargo.lock does not pin edits in a path dependency.

## Rich rendering

Long printable-ASCII source lines can now use a compact metric-run index with
bold, italic, mixed sizes, full-line backgrounds, and inline inlay/debug/ghost text.
The index merges overlapping font-size/weight/slant overrides in provider order.
Each distinct metric style is probed against the selected monospace face. Inline
labels use their explicit 12-unit normal/italic metrics, including padding.
Source runs and inline anchors contribute to cumulative horizontal positions.
Double-precision accumulation avoids repeated per-character float addition in
long runs; draw positions remain Astrelis local-unit floats.

Pixel-aligned windows retain a visible region plus overscan. They do not align
backward by source columns, which could pull many wide offscreen labels into a
slice. Source carets have right affinity after every hint at the same anchor;
pointer positions inside labels map to their source anchor. Source selections
exclude labels. EOF hints work both before LF and at the end of the document.
Hover/provider IDs remain global after clipping. Offscreen full-line backgrounds
are projected as background-only spans in their original provider order, so they
continue to paint without extending offscreen font or hover attributes.

Astrelis's new `TextLineMetrics` and `TextBuffer::set_minimum_line_metrics` preserve
global ascent, descent, and line-box height in a slice. Glyph and decoration
baselines shift together; interaction and width-dependent measurement use the
same line heights. Thus a large offscreen font no longer makes visible text jump
vertically as a window changes. Metrics are opt-in; existing Astrelis layout is
unchanged by default.

The run index is retained across scroll/caret frames and rebuilt when source,
annotations, or the font generation change. Its metadata and cold construction
still scale with source style/hint count. Glyph layout, projection, GPU preparation,
and drawing operate on the window. A width-shrink regression also ensures the
window is prepared again if final scroll clamping moves the viewport after caret
reveal, without waiting for a follow-up frame. Cached decoration widths are used only
when they belong to the current layout and annotation snapshot, so old rich
content cannot keep a newly short document scrolled.

## Eligibility and limits

Source and label text must be printable ASCII, with verified uniform advances for
each selected metric style. Non-ASCII source/labels, bidi/complex scripts, tabs and
controls, and non-inline overlays retain full-line shaping. Labels longer than
512 bytes and co-anchored projected label groups larger than 2,048 bytes also use
that fallback. These guards preserve shaping and anchor semantics; the current
index is not a general Unicode cluster-layout implementation.

Initial eligibility scans and provider-index construction are not bounded by the
viewport. A viewport may also contain many tiny glyphs or many distinct styles.
The benchmarks establish performance for the prototype fixtures, not a universal
8.33 ms bound. IME still uses the full-document composition fallback. Edits expire
the immutable annotation snapshot, as before; a real producer must refresh it.

## CPU measurements

Same release viewport harness and Menlo 16/24 units at 1,200 × 800 logical units.
Two sequential runs, no native app or concurrent build during measured execution.
Cold rich preparation resets the line and font-metric caches while retaining the
application font system. It includes classification, annotation indexing, metric
probes, run construction, extraction, projection, shaping, and interaction indexing.
GPU work and presentation are excluded.

| Fixture: 10,000 source characters + 1,000 bold spans/inlays | Median range (ms) | Largest run p95 (ms) |
| --- | ---: | ---: |
| Previous full-line fallback | 11.69–11.83 | 12.44 |
| Cold rich window | 0.379–0.381 | 0.703 |
| Warm middle window | 0.0029–0.0030 | 0.0032 |
| Scroll through rich windows | 0.217–0.228 | 0.304 |

Previous values are the earlier pass in `editor-horizontal.csv`,
not an interleaved A/B run. New raw results, including the ordinary document and
fixed-grid cases, are in `editor-rich-horizontal.csv`.

## Native timing investigation

The `--rich-long-lines` fixture has 20,000 source characters, 1,000 bold spans and
inline hints, a 32-unit initial span, a line background, and an EOF ghost label.
The native checks cover start/end navigation, selection excluding hint labels, and
baseline/background stability when the large initial font is offscreen. The final
profiled startup prepared four logical lines in 2.93 ms, retaining 354 source bytes
in line windows (a counter for one source String per line, not RSS or total GPU memory).

`ASTRELIS_PROFILE=1` now records CPU phases at the native runner boundary:
invalidating event callbacks, redraw request/delivery, host preparation, surface
acquisition, recording, and submission. It also reports coalesced input count,
acquisition retries, and availability changes. Source/input text is not logged.
Only callbacks that invalidate the window are counted as input. Native dispatch
is the earliest timestamp; time before OS event delivery is excluded. Completion
of a submission is not GPU completion or actual display presentation.

An isolated synthetic key probe reproduced an 11.32 ms dispatch-to-prepare sample:
its callback took 0.107 ms, redraw-request-to-delivery took 0.006 ms, and redraw
callback-to-preparation took 0.001 ms. The remaining approximately 11.31 ms was
before requesting the native redraw. Host preparation then took 0.493 ms, surface
acquisition 0.052 ms, recording 0.056 ms, and submission 0.256 ms.

An experiment queued input redraws immediately after the callback, while retaining
coalescing/retry rules. Its isolated sample still took 14.30 ms to reach preparation:
requesting took 0.155 ms and redraw delivery waited another 14.14 ms. The experiment
did not establish a latency improvement and was reverted. The retained runner
keeps its original on-demand scheduling and FIFO presentation policy.

The compiled winit 0.30.13 AppKit implementation queues redraws and dispatches them
from its run-loop `cleared` observer; `AboutToWait` follows that dispatch. The trace
therefore points to native event-loop delivery/waiting in these isolated probes,
rather than text preparation. It does not establish an OS defect or explain every
historical outlier. Synthetic automation itself can affect event-loop timing.

Repeated start/end navigation also showed surface-acquisition waits up to 5.46 ms,
with no acquisition retries or availability changes in the captured input frames.
This is separate from the pre-preparation delay. Follow-up submissions occur after
navigation; the editor intentionally requests a follow-up to publish measured IME
geometry. Reducing that work may reduce queue pressure, but a causal improvement
has not been measured and removing it would require an IME geometry design change.

Raw `editor-native-stages.csv` retain initial frame and
input-bearing frame records for the default, reverted experiment, and final default
runs. `editor-rich-native.csv` retain initial, input,
and GPU-preparing records. These independent record counters are not one-to-one.
The samples are exploratory; counts, durations, and interaction order differ.
They do not establish a frame-rate/latency distribution or sustained 120 Hz.

## Checks and usage

Velk: 56 tests, including GPU-backed rich-window hover and pointer mapping, full-line
reference comparisons, repeated/EOF hint anchors, offscreen font metrics, background
provider order, cache invalidation, fallback transitions, and the scroll-clamp
regression. Astrelis: 176 unit tests and 28 doc tests. Both repositories pass
formatting and all-target Clippy with warnings denied; Velk also passes all-feature
checks and a release build. Pollster is added only as a dev dependency for the
headless GPU integration check.

```sh
cargo run --release --locked -- --rich-long-lines
ASTRELIS_PROFILE=1 VELK_PROFILE=1 cargo run --release --locked -- --rich-long-lines
cargo bench -p velk-editor-widget --bench viewport --features performance --locked
```

Next work: context-preserving Unicode cluster windows, localized eligibility/run
updates after edits, and RXUI/native IME geometry publication without redundant
submissions. Actual GPU/presentation timestamps are still needed for an end-to-end
120 Hz claim.
