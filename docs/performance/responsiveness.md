# Editor input responsiveness and IME preparation

Follow-up: [Unicode and composition windows](composition-windows.md) replaces the
multiline full-document fallback and adds guarded horizontal Unicode/preedit
windows. This report records the earlier implementation and its measurements.

2026-10-15, Apple M3 Pro, macOS 27.0.1, Rust 1.99.0, release builds.
This follows [rich horizontal windows](rich-horizontal-and-scheduling.md).
The target remains an 8.33 ms frame budget. Submission, GPU completion and
display presentation are distinct boundaries; none of these probes measures
physical key-to-photon latency.

## Changes

RXUI [`fc331af`](https://github.com/hxyulin/rxui/commit/fc331afca247579cde12a480ed01c687abb1ff5d)
adds `Element::ime_cursor_area_with`. A custom element can retain measured caret
geometry and expose a read-only provider instead of capturing the previous
rectangle in its view. RXUI already syncs native IME state after custom preparation;
the provider therefore publishes the current frame's geometry before submission.
It uses the same border-box coordinates, fallback and geometry validation as the
static setter. The last static/deferred setter wins. Queries before custom
preparation may still observe the last available geometry.

Velk uses this provider and removes the explicit geometry follow-up animation
request. Navigation, typing and composition now settle with one submission instead
of two. Caret blinking remains timer-driven. The native built-in caret timer already
excludes custom text clients; no second native blink loop needed fixing.

Single-line composition now stays in the logical-line cache. Only the composing
line receives a projected preedit replacement; the Rope and source selection stay
unchanged until commit. Its complete logical line uses advanced shaping, including
Unicode and inline annotations. Other visible lines reuse their retained layouts.
The composing line's hit geometry maps projected bytes back to source anchors.
The full-document path remains for a replacement spanning source lines or a
preedit containing line breaks. RXUI's normal native preedit validation already
rejects line-break text; the renderer's guard also protects direct descriptions.
Returning from the fallback releases its full-document scratch resources.

The same-frame regression check caught a resize issue: a narrowed viewport could
leave the caret outside the visible area because reveal only checked the edit
epoch. When caret reveal is active, a viewport change now reveals and prepares
the final window in that frame. Explicit scrolling keeps its existing anchoring.

Astrelis [`4801b66`](https://github.com/hxyulin/astrelis/commit/4801b66d77d00726c12b318a709544488970c7c5)
extends opt-in native tracing to invalidating pointer motion and records both the
oldest and newest coalesced input through submission. No input contents are logged.
On-demand scheduling, redraw coalescing and FIFO presentation remain unchanged.
The immediate-redraw experiment from the previous report remains reverted.

## Offscreen interaction benchmark

`benches/responsiveness.rs` dispatches actual RXUI key/text events into an editor,
then uses the regular reconciliation, viewport preparation, compositor and draw
path into a retained 1,200 × 800 framebuffer at raster scale one. It also queries IME geometry at the
same boundary as the native host. It submits every requested follow-up frame and
records event CPU time, time to first submission, time to settle, and frame count.
Pixel readback is excluded. `VELK_BENCH_GPU_WAIT=1` separately waits for the device's
submitted work to finish, without reading pixels or measuring scanout.

The ordinary cases use 20 warmups and 201 samples per run: alternating Home/End
on 100,000 characters; the same on 20,000 rich characters with 1,000 bold spans and
inlays; insertion/backspace at the top of a 100,000-line/2.5 MB document; and small
Unicode preedit/cancel. Large-document IME uses two warmups and 21 successive
preedit updates on 10,000 lines/250 KB. These are prepared, warm interactions,
not cold font loading or application startup. Editing expires annotations, so the
typing case does not include a real syntax/LSP producer's update cost.

The baseline ordinary cases use Velk `fc18907` with the same initial harness and
pinned RXUI `093d05d`; the optimized cases use the implementation described above.
The large-IME reference was collected after removing the follow-up frame but before
localizing composition, isolating the full-document preparation cost. Both use
21 measured updates with the same fixture and font state. The archived reference
is one run; the optimized data is repeated. Raw samples and the final tables below
make these different comparisons explicit.

Three sequential before/after pairs ran with no native preview or concurrent
build. The table pools 603 samples per ordinary case/variant; percentiles use
nearest rank. Per-run settling medians stayed within 0.232–0.248 ms before and
0.116–0.139 ms after for plain navigation. Frames were exactly two before and
one after across every measured ordinary interaction.

| Offscreen dispatch, p50 (ms) | First submit before | First submit after | Settled before | Settled after |
| --- | ---: | ---: | ---: | ---: |
| 100k-character navigation | 0.124 | 0.123 | 0.239 | 0.123 |
| Rich navigation + 1,000 inlays | 0.267 | 0.253 | 0.304 | 0.253 |
| Typing/backspace, 100k-line document | 0.112 | 0.108 | 0.196 | 0.108 |
| Small Unicode preedit/cancel | 0.101 | 0.100 | 0.151 | 0.100 |

The principal effect is less work after the first frame. It is not a large
first-frame speedup for these already-fast warm operations.

| 10k-line successive IME updates | Full-document reference (21 samples) | Line cache (63 samples) |
| --- | ---: | ---: |
| Dispatch → first submit p50 | 32.598 ms | 0.121 ms |
| p95 | 42.625 ms | 0.172 ms |
| Maximum | 50.357 ms | 0.206 ms |

Localizing composition removes a document-sized operation from each preedit
update in this fixture. This is the largest measured responsiveness improvement
in this pass. It does not bound the cost of a huge composing logical line.

A separate final-code run waited for offscreen GPU work after every sample:

| Dispatch → CPU observes GPU completion | p50 (ms) | p95 (ms) | Maximum (ms) |
| --- | ---: | ---: | ---: |
| Plain navigation | 0.479 | 0.706 | 0.962 |
| Rich navigation | 0.627 | 0.974 | 1.339 |
| 100k-line typing/backspace | 0.486 | 0.694 | 1.330 |
| Small Unicode preedit/cancel | 0.474 | 0.534 | 1.023 |
| 10k-line successive preedit | 0.909 | 0.974 | 1.300 |

This is a blocking device-poll completion boundary, not hardware timestamp-query
data. Waiting changes workload cadence, so these are separate samples rather
than a GPU duration obtained by subtracting the earlier submission results.
The first four GPU-wait cases have 201 samples each; large IME has 21.
All 5,733 raw offscreen samples are in
[editor-responsiveness.csv](editor-responsiveness.csv).

## Native dispatch and queue behavior

The rich long-line fixture ran full-screen with native/widget tracing enabled.
After focusing, Command+Up explicitly selects the document start, then
Command+Right warms the actual rich line's EOF window. Screenshots verify the
EOF ghost hint/caret. An earlier automation attempt omitted the document-start
step and could navigate the trailing blank line; it was discarded and rerun.
Window activation, fullscreen transitions, startup and warmup inputs are excluded
from the retained comparison ranges. Windowed automation initially delivered
inputs while presentation was unavailable; those samples are unsuitable for latency
comparison and are excluded. No retries or availability transitions occur inside
the retained comparison ranges. This is an exploratory automation capture, not a
controlled physical-device/display experiment.

Each paced run alternates 30 Command+Left/Right presses with an explicit 80 ms
pause after each automation call, plus the call's own overhead. Every retained
input has its own input-bearing frame. The baseline submits a second frame after
every input (60 frames); the optimized version submits 30 frames.

| Paced native dispatch → first submission | Baseline | Same-frame geometry |
| --- | ---: | ---: |
| p50 | 1.823 ms | 1.676 ms |
| p95 | 2.913 ms | 3.502 ms |
| Maximum | 4.221 ms | 3.582 ms |

The paced result does **not** establish a consistent first-frame latency
improvement: p50 is slightly lower but p95 is higher. It demonstrates removal of
redundant submissions while preserving current caret geometry. Host preparation
and native scheduling noise dominate the small differences between these samples.

Unpaced bursts sent 60 inputs each, but the automation durations differed (about
1.68 s baseline and 1.35 s optimized). The baseline produced 60 input-bearing plus
60 follow-up frames; the optimized run coalesced 60 inputs into 50 submissions.
Oldest dispatch → submission p50/p95 was 2.520/5.740 ms baseline and
0.922/2.638 ms optimized. Baseline acquisition p95 was 4.490 ms; optimized was
0.076 ms. These are consistent with reduced queue pressure, but unequal cadence
and coalescing prevent attributing those differences to the change alone.

Raw [native samples](editor-responsiveness-native.csv) retain the complete bounded
frame intervals, including follow-up frames. Timing starts at native callback
dispatch, so it excludes device/OS delivery before that point. Submission is CPU
completion of queue submission; it is neither GPU completion nor display arrival.
The new newest-input-to-submit field is recorded by the optimized capture;
baseline rows leave it empty because its older runner did not emit that field.

## Validation and remaining limits

Velk: 60 passing tests, including full-layout reference comparisons, single-frame
IME/caret publication, resize reveal, Unicode composition hit mapping, source
preservation, commit/cancel, multiline fallback and return to cached rendering.
RXUI: 331 passing unit/integration tests and 46 doc tests with all features;
36 environment/manual tests are ignored by default. The focused native IME routing
test was also run explicitly and passed. Default-feature RXUI tests and all-target,
all-feature Clippy pass. Its large macOS test binary emits an existing-size linker
warning about unwind tables; the tests complete successfully.
Astrelis: 176 unit tests and 28 doc tests pass. Velk/Astrelis formatting, all-target
Clippy with warnings denied, and Velk's release build pass.

The composing logical line is fully shaped, so a huge individual line remains
expensive. Multiline selection replacement still shapes the full document.
General Unicode horizontal windows, localized metric/eligibility updates after
edits, and real annotation-provider costs remain future work. Actual platform IME
candidate-window UI and physical input/display latency have not been independently
measured; geometry is checked through the renderer and native routing tests.

## Reproduction

RXUI is pinned to the published commit above. Astrelis still uses the sibling
`../astrelis` path patch on `perf/editor-text` at `4801b66`; Cargo.lock does not pin
path-dependency contents. No local RXUI checkout is required by Velk.

```sh
cargo bench -p velk-editor-widget --bench responsiveness --locked > /tmp/responsiveness.csv
python3 scripts/latency.py summary /tmp/responsiveness.csv
VELK_BENCH_CASE=large_document_ime cargo bench -p velk-editor-widget --bench responsiveness --locked
VELK_BENCH_GPU_WAIT=1 cargo bench -p velk-editor-widget --bench responsiveness --locked
ASTRELIS_PROFILE=1 VELK_PROFILE=1 cargo run --release --locked -- --rich-long-lines 2> /tmp/native.log
python3 scripts/latency.py native /tmp/native.log --capture navigation --first-frame 10 --last-frame 70 > /tmp/native.csv
python3 scripts/latency.py summary /tmp/native.csv --input-only
```

Frame boundaries in the last command are an example; choose the actual stable
interaction interval in your capture. For a baseline checkout at `fc18907`, copy
the benchmark file into `crates/editor-widget/benches/`, register its `[[bench]]`
entry with `harness = false`, and use the same Astrelis checkout. That baseline
still requests two submissions for IME; use `first_submit_ms` when comparing its
full-document IME cost with the archived single-frame full-document reference.
