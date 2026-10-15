# Native input redraw experiment

2026-10-15, Apple M3 Pro, macOS 27.0.1, Rust 1.99.0, release builds.
Velk `c0a682d`, RXUI `fc331af`, Astrelis `4801b66` plus the archived experiment
patch. This follows [composition windows](composition-windows.md).

## Result

Rendering immediately after each invalidating native input callback reduces the
handoff to preparation, but loses event coalescing and increases burst surface
acquisition waits. The experiment is **reverted** from the running backend.
The normal scheduler remains unchanged. The opt-in implementation is retained
as [a patch](experiments/input-redraw.patch), rather than a production policy.

The earlier 41 ms pre-request outlier did not recur in these measured ranges.
This experiment therefore does not establish a fix for that rare delay. It does
show that bypassing the wait phase has a measurable cost under bursts.

The remaining latency is not exclusively native scheduling: long-line editing
still performs CPU work in the widget's input callback. A separate offscreen
probe reproduces that work without native event delivery or surface acquisition.

## Implementation and boundaries

The normal Astrelis runner records dirty state during event callbacks, then queues
`Window::request_redraw` in `about_to_wait`. In winit 0.30.13's macOS
`platform_impl/macos/app_state.rs`, `cleared` delivers pending redraws before
`AboutToWait`; a redraw queued in that callback is handled on a subsequent loop
pass. The measured pre-request interval also includes time between callbacks;
these code paths alone do not identify the source of the earlier 41 ms delay.

With `ASTRELIS_EXPERIMENT_INPUT_REDRAW=1`, an invalidating keyboard, IME, mouse,
wheel or pointer callback attempts a frame after the handler and its deferred
operations return. This uses the existing dirty/queued/retry scheduling guard,
checks active/presentable state and excludes closing windows. Preparation and
submission use the normal runner path. No synthetic `RedrawRequested` event is
sent to the application. Existing queued native redraws retain their normal path.
No polling, timer, present-mode or GPU-completion policy is changed.

The experimental trace adds `inline_redraws`. Direct frames have no native
request timestamp: `input_to_request_ms` and `request_to_redraw_ms` remain empty,
and `redraw_to_prepare_ms` starts at entry to the direct redraw path. This avoids
reporting a fabricated native redraw request/delivery duration.

This prototype renders before returning the current native input dispatch to
winit/AppKit. It does not preserve batching of separate native IME callbacks.
Its reported event CPU time ends before this inline rendering work, while total
input-to-submit time includes it. It is unsuitable as a default policy based on
this experiment.

## Native comparison

One experimental binary ran with the environment switch off/on in order
baseline 1 → inline 1 → inline 2 → baseline 2. Each process used the same
full-screen `--unicode-long-lines` fixture. After focusing the editor, Command+Up
then Command+Right explicitly navigate to the Unicode tail of the approximately
110k-character first line. One Option+E, E, Backspace cycle warms native
composition and returns to the starting document.

Each run contains:

- 30 alternating Command+Left/Right presses, with 80 ms after every automation call.
- 15 Option+E, E, Backspace cycles, with 80 ms after each call. This exercises native
  dead-key preedit, accented commit and deletion. A CJK candidate panel was not tested.
- 60 alternating Command+Left/Right presses with no explicit delay.

Startup, fullscreen transitions, activation and warmup are excluded. The retained
ranges include idle blink frames so all submitted frames in those intervals are
available. Timing percentiles below include only frames with invalidating input.
No retained frame has an availability change or acquisition retry. Tracing uses
`ASTRELIS_PROFILE=1` only; widget per-frame logging is disabled in both variants.

| Native dispatch → CPU submission | Normal p50 | Inline p50 | Normal p95 | Inline p95 |
| --- | ---: | ---: | ---: | ---: |
| Paced navigation, 60 input frames each | 1.100 ms | 0.930 ms | 2.288 ms | 2.057 ms |
| Burst navigation, 92 / 120 input frames | 0.756 ms | 1.121 ms | 2.113 ms | 6.054 ms |
| Composition cycles, 90 / 120 input frames | 7.054 ms | 3.523 ms | 11.481 ms | 10.502 ms |

Percentiles use nearest rank on the combined two runs. Navigation burst p95
regressed in each pair: 2.357 → 6.054 ms in pair one; 1.682 → 5.547 ms in pair two.
Paced navigation has a small improvement at this sample size.

Composition frame percentiles are **not equivalent operation distributions**:
the normal path batches 120 invalidating IME/edit callbacks into 90 frames,
whereas inline rendering submits all 120 separately. The lower inline median
includes extra intermediate states; it is not evidence that committed characters
appear twice as fast. Native preedit and commit were visually checked; the test
does not verify candidate-window behavior or every input method's reentrancy.

| Burst behavior, both runs | Normal | Inline |
| --- | ---: | ---: |
| Invalidating input callbacks | 120 | 120 |
| Input-bearing submissions | 92 | 120 |
| Surface acquisition p95 | 0.071 ms | 5.549 ms |
| Surface acquisition maximum | 0.084 ms | 6.033 ms |
| Automation duration per 60 presses | 684 / 662 ms | 1,006 / 1,013 ms |

The extra submissions coincide with surface acquisition pressure and slower
automation. Acquisition time is a CPU-side wait, not a GPU execution duration.
Unequal burst delivery cadence and different coalescing prevent a controlled
per-key causal estimate. Even at the slower delivered cadence, the inline variant
has the worse tail. It should not be promoted on the strength of its shorter
callback-to-prepare interval.

For paced navigation, input-to-prepare p50 falls from 0.237 to 0.102 ms. In the
inline IME range, callback CPU alone reaches p95 8.310 ms and maximum 10.785 ms;
moving the redraw cannot remove work already inside that callback. The host event
measurement includes UI input preparation, editor listeners, native text-state
sync and accessibility progress; it does not isolate each of those costs.

There are **788 retained native frame records**, representing **600 invalidating
input callbacks**, in [editor-input-redraw-native.csv](editor-input-redraw-native.csv).
The [capture manifest](editor-input-redraw-captures.json) records inclusive frame
bounds, capture order, revisions and automation durations. These are automated,
instrumented exploratory measurements, not physical key-to-photon or display
presentation measurements. The previous isolated 41 ms outlier remains unresolved.

## Offscreen long-line edit probe

The responsiveness harness now has `long_line_unicode_typing`: the same 110k ASCII
prefix and CJK/combining/emoji tail, collapsed selection at line end, alternating
insertion of `é` and Backspace. This uses the normal RXUI/widget event pipeline and
an offscreen 1200 × 800 target at scale one. There are 20 warmups and 200 measured
operations, yielding 100 insertions and 100 deletions.

| Offscreen operation | Event CPU p50 / p95 | Dispatch → submit p50 / p95 |
| --- | ---: | ---: |
| Insert `é` | 1.311 / 1.338 ms | 1.808 / 1.842 ms |
| Backspace | 2.571 / 2.604 ms | 3.065 / 3.102 ms |

All operations submit one frame and retain fewer than 4,000 cached source bytes.
The raw [offscreen samples](editor-input-redraw-offscreen.csv) preserve per-sample
event and submission times. Even samples insert; odd samples delete. These timings
do not directly subtract from the native fullscreen/scale-two results.

Code inspection identifies a candidate for the next pass: `Buffer::replace`
clears the grapheme-boundary cache, segments the full logical line to snap the new
caret, then `changed` clears that newly built cache again. Backspace also obtains
the preceding boundary before replacement. Whole-line segmentation is still
linear after edits despite bounded rendering. This probe establishes reproducible
application-side input cost; it does not attribute every native callback
millisecond to segmentation. A chunk-aware grapheme cursor and preserving valid
post-edit cache entries are appropriate next experiments, with Unicode reference
checks before changing caret semantics.

## Reproduce and validation

The experiment's Astrelis-winit build passed 11 unit tests and two doc tests. The
archived patch applies cleanly to `4801b66`; the sibling checkout has been restored
to that revision. Velk formatting, all-target/all-feature Clippy with warnings
denied and the workspace tests pass. Native preedit, commit and repeated burst
navigation completed in both variants without a crash. The production release
binary is rebuilt after restoring the backend.

```sh
# CPU event cost, normal backend; native preview should be stopped.
VELK_BENCH_CASE=long_line_unicode_typing VELK_BENCH_SAMPLES=200 \
  cargo bench -p velk-editor-widget --bench responsiveness --locked > /tmp/edits.csv
python3 scripts/latency.py summary /tmp/edits.csv

# From the Astrelis checkout at 4801b66, apply the archived prototype.
git apply ../velk/docs/performance/experiments/input-redraw.patch

# From Velk: build once, compare the same executable with the switch off and on.
cargo build --release --locked
ASTRELIS_PROFILE=1 target/release/velk --unicode-long-lines
ASTRELIS_PROFILE=1 ASTRELIS_EXPERIMENT_INPUT_REDRAW=1 \
  target/release/velk --unicode-long-lines

# From Astrelis after the experiment, reverse just this patch and rebuild Velk.
git apply -R ../velk/docs/performance/experiments/input-redraw.patch
```
