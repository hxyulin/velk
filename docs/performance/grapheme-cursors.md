# Chunk-aware grapheme queries and edit latency

2026-10-15, Apple M3 Pro, macOS 27.0.1, Rust 1.99.0, release builds.
Baseline Velk `ffd1ee7`; RXUI stays pinned to `fc331af`, and the sibling Astrelis
patch stays at `4801b66`. This implements the application-side follow-up from
[the native redraw experiment](native-input-redraw.md). No dependencies or backend
policies changed.

## Implementation

Caret snapping, boundary validation, horizontal movement and deletion now use
`unicode-segmentation 1.13.3`'s existing extended `GraphemeCursor` over borrowed
Ropey chunks. The adapter converts source character offsets to byte offsets,
supplies requested previous/next chunks and exact-ending pre-context, then converts
the result back to character offsets. It does not materialize the logical line or
allocate a boundary vector for these queries. Unicode context is not arbitrarily
truncated: the dependency can request more chunks for combining sequences,
regional-indicator parity, emoji ZWJ sequences and Indic conjuncts.

Insertion and replacement check the resulting caret directly. If an edit joins
neighboring scalars into one grapheme, the caret advances to that grapheme's end.
There is no longer a backward snap followed by a second forward traversal.
Home/End uses Ropey's logical-line metadata and excludes the trailing LF, without
segmenting the line. Newline normalization and selection semantics are preserved.

Up/Down still uses full-line grapheme-column indexing with an eight-line cache.
Edits discard the changed line and all subsequent cached entries, because their
contents, line numbers or absolute character offsets may have changed. Entries
before the first changed line survive. Revision/goal-column updates no longer
clear valid cache data a second time. Undo and redo invalidate the complete cache.
The edited line's old column table is never reused after mutation.

The cost of a point query is the Rope lookup plus the grapheme/context traversed,
not a fixed upper bound independent of text. A very long combining cluster or
regional-indicator run can still require substantial context. Vertical movement
can still allocate/segment long logical lines on a cache miss. These cases are
distinct from ordinary point queries near the end of a large ASCII prefix.

## Paired offscreen comparison

The unchanged `responsiveness` harness ran in three sequential before/after pairs,
using separate preserved release executables. The native preview was stopped, and
no build or tests ran concurrently with the captures. Each process uses a
1200 × 800 offscreen framebuffer at raster scale one and real RXUI/widget input,
preparation and CPU GPU-command submission. There is no GPU readback or blocking
completion wait in these samples.

Ordinary cases use 20 warmups/201 samples per run; composition stress cases use
two warmups/21 samples. Percentiles use nearest rank. The long-line typing fixture
is the existing 110k-character ASCII prefix with a CJK/combining/emoji tail,
selection at line end, alternating insertion of `é` and Backspace. Its sample
parity starts with insertion after 20 warmups, giving 303 insertions and 300
deletions per variant across all three runs.

| Long-line operation | Before event p50 / p95 | After event p50 / p95 |
| --- | ---: | ---: |
| Insert `é` | 1.282 / 1.350 ms | 0.0015 / 0.0021 ms |
| Backspace | 2.561 / 2.673 ms | 0.0014 / 0.0019 ms |

| Dispatch → CPU submission | Before p50 / p95 | After p50 / p95 |
| --- | ---: | ---: |
| Long-line insert `é` | 1.776 / 1.871 ms | 0.525 / 0.559 ms |
| Long-line Backspace | 3.052 / 3.181 ms | 0.522 / 0.561 ms |
| Plain long-line navigation | 0.128 / 0.261 ms | 0.119 / 0.253 ms |
| Rich long-line navigation | 0.252 / 0.290 ms | 0.253 / 0.286 ms |
| Short-line typing in a 100k-line document | 0.107 / 0.149 ms | 0.107 / 0.162 ms |
| Unicode-island navigation | 0.105 / 0.125 ms | 0.111 / 0.214 ms |
| Small preedit/cancel | 0.123 / 0.242 ms | 0.114 / 0.170 ms |
| Small-line preedit in a 10k-line document | 0.122 / 0.137 ms | 0.123 / 0.164 ms |
| Long ASCII line + CJK preedit | 0.385 / 0.403 ms | 0.376 / 0.410 ms |
| Long ASCII line + emoji preedit | 0.380 / 0.405 ms | 0.373 / 0.413 ms |
| Multiline replacement preedit | 0.117 / 0.171 ms | 0.123 / 0.158 ms |
| Long ASCII line + RTL preedit fallback | 26.496 / 27.422 ms | 26.495 / 26.980 ms |

All measured interactions in both variants submit one frame. The long-line edit
fixture retains 335–337 cached source bytes. The point-query change removes the
dominant edit callback work; remaining long-line revision classification and
rendering still take approximately half a millisecond in this fixture. The
ordinary/IME controls have mixed small percentile movements; these runs do not
establish improvements for those controls. In particular, Unicode-island
navigation p95 rises from 0.125 to 0.214 ms. The RTL fallback remains far above
the 8.33 ms target.

All **7,866 offscreen records**, including other composition/navigation controls,
event times, frame counts and cache counters, are in
[editor-grapheme-cursors.csv](editor-grapheme-cursors.csv).

## Native composition and editing

Native captures use the same fullscreen `--unicode-long-lines` fixture, with
`ASTRELIS_PROFILE=1` and widget profiling disabled. The baseline release executable
was preserved before rebuilding; the two executable hashes and capture bounds are
in [the manifest](editor-grapheme-captures.json). Order is before 1 → after 1 →
after 2 → before 2, using a fresh process each time. Both use the normal scheduler;
the previous inline-redraw prototype is not applied.

After focus, Command+Up and Command+Right select the actual long line's Unicode
tail. One Option+E, E, Backspace cycle warms the native composition/edit path. Each
measured run sends 15 more cycles, with 80 ms after each automation call. Native
dead-key preedit, underline/caret and the Unicode tail were visually checked;
repeated accented commit/deletion cycles completed. This does not independently
verify CJK candidate-panel placement or every IME.

| Native input-bearing frames, combined runs | Before p50 / p95 | After p50 / p95 |
| --- | ---: | ---: |
| Input callback CPU | 3.583 / 8.240 ms | 0.115 / 0.299 ms |
| Dispatch → prepare | 4.255 / 8.494 ms | 0.303 / 3.218 ms |
| Host preparation | 1.569 / 2.392 ms | 1.493 / 2.156 ms |
| Surface acquisition | 0.058 / 0.141 ms | 0.049 / 0.125 ms |
| Dispatch → CPU submission | 5.903 / 11.431 ms | 2.290 / 4.868 ms |

Each variant has 120 invalidating callbacks coalesced into 90 input-bearing frames,
45 frames per run. Unlike the inline-redraw experiment, the frame distributions
are unchanged. Submission p95 improves in both pairs: 11.485 → 4.452 ms in pair
one, and 10.539 → 5.268 ms in the reverse-order pair. After-change maximum is
6.324 ms across these input-bearing frames, compared with 13.626 ms before.
That observation is not a universal 120 Hz or worst-case latency guarantee.

There are no acquisition retries or availability changes in the retained ranges.
Startup, activation, fullscreen transitions and warmup are excluded. Idle blink
frames inside the retained ranges remain archived. The complete **269 native
frame records**, representing **240 invalidating callbacks**, are in
[editor-grapheme-native.csv](editor-grapheme-native.csv).

The host callback measurement includes UI input preparation, editor listeners,
native text-state sync and accessibility progress; it is broader than the
offscreen event metric. The point-query optimization also benefits validation of
annotation positions and other boundary queries, not only `Buffer::replace`.
There is still a native handoff tail: after-change input → request p95 is
3.212 ms, despite callback CPU p95 0.299 ms. The earlier isolated 41 ms wait did
not recur here and remains unresolved. These are automated, instrumented CPU
dispatch/submission measurements, not physical key-to-photon, GPU completion or
display presentation timings.

## Correctness and validation

69 workspace tests pass, including five added tests:

- Compare chunk queries at every scalar position against contiguous extended
  grapheme segmentation for mixed Unicode, CRLF/control characters, flags, emoji
  modifiers/ZWJ, prepend characters, Hangul and Indic conjuncts.
- Exercise clusters and context spanning several chunks, including 3,000 combining
  marks and long regional-indicator, prepend, emoji and Indic runs. Repeat after
  Rope insert/remove operations change leaf layouts.
- Check deterministic mixed-category strings after Rope mutations at every scalar.
- Verify earlier column-cache entries survive edits, shifted lines are recomputed,
  and undo/redo restore correct grapheme columns.
- Compare insertion, Backspace, caret positions and undo/redo against a contiguous
  String reference near Unicode and newline joins at external scalar offsets.

Existing rendering, viewport, annotation, composition, pointer/input and history
tests also pass. Formatting, all-target/all-feature Clippy with warnings denied,
and the release build pass. RXUI and Astrelis remain unchanged.

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo bench -p velk-editor-widget --bench responsiveness --locked > /tmp/graphemes.csv
python3 scripts/latency.py summary /tmp/graphemes.csv
VELK_BENCH_CASE=long_line_unicode_typing \
  cargo bench -p velk-editor-widget --bench responsiveness --locked
ASTRELIS_PROFILE=1 cargo run --release --locked -- --unicode-long-lines
```

To reproduce the paired baseline, preserve a release responsiveness executable
and editor binary at `ffd1ee7`, rebuild after the change with the same dependency
checkouts, and alternate the preserved executables. Do not compile or leave a
native preview active during offscreen measurements.
