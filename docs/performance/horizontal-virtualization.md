# Horizontal virtualization: fixed-grid text

Implemented on 2026-10-15, following the local Astrelis preparation pass. Astrelis
commit `9c60dfdd2a2ec6800c701792eff8dd849c90ee48` is now published on
[`perf/editor-text`](https://github.com/hxyulin/astrelis/tree/perf/editor-text).
Velk continues using the sibling path override for backend development.

## Rendering behavior

Eligible logical lines longer than 1,024 characters retain only a horizontal
window of source text, CPU layout, interaction data, and prepared GPU glyphs.
Windows align to 128-column chunks with a chunk of overscan on each side. Work
and retained glyph data scale with viewport width, rather than full-line length.
Small scrolls within a chunk reuse the shaped layout. Returning from a full-line
fallback releases its retained shaping scratch before preparing the bounded window. At a 1,200-unit viewport,
the tests require at most 512 retained source characters/glyphs per eligible line.
Wider viewports prepare proportionally wider windows.

The original Rope remains complete. A window carries its source range, pixel
origin, cell advance, and full line length. The full width drives scroll bounds;
caret reveal and hit testing use document columns. Selection ranges and style
ranges clip to the window, while edits, undo, copy, and accessibility retain
original document positions. Hover regions translate back to document coordinates
and keep their original provider IDs. Newline markers only paint at the actual
line end, never the end of a horizontal slice.

A retained font probe checks every printable ASCII character: all advances must
match, all selected fonts must report monospace, and no glyphs may be missing.
The probe re-evaluates when Astrelis's font generation changes. Source eligibility
is checked once per document revision for each cached line; annotation metric
eligibility is cached until the revision or provider snapshot changes. These
classification scans still depend on line length/span count after edits, even
though extraction, shaping, interaction, and GPU preparation are bounded.

Supported styles preserve normal 16-unit font metrics: foreground colors, range
backgrounds, hover cards, and custom underlines/overlines/strikethroughs. Explicit
normal weight/slant/size are allowed. Lines with Unicode, tabs/control characters,
bold/italic/mixed sizes, full-line background spans, or any text overlays keep the
full-line renderer. Active IME still uses the existing full-document fallback.
This guard preserves context-sensitive shaping and inline positions. Rich/Unicode
horizontal chunking requires a separate run/cluster positioning design.

## CPU measurements

Release viewport harness, Menlo at 16/24 units, 1,200 × 800 logical viewport, two
sequential runs with no concurrent builds. Results include the Rope mutation where
specified, extraction, projection, shaping, and interaction indexing; GPU work,
frame submission, and display presentation are excluded. Values are ranges of the
two medians. Raw per-run medians/p95 values are in [editor-horizontal.csv](editor-horizontal.csv).

| Workload | Median range (ms) | Largest run p95 (ms) |
| --- | ---: | ---: |
| Offscreen edit in a 100,000-character line | 0.029–0.033 | 0.040 |
| Visible edit in a 100,000-character line | 0.058–0.058 | 0.067 |
| Horizontal scroll across changing numbered chunks | 0.078–0.078 | 0.083 |
| Cold middle window with 1,000 color spans | 0.125–0.125 | 0.126 |
| Full-line fallback: 1,000 styles and inline hints | 11.69–11.83 | 12.44 |

The unchanged offscreen-edit fixture took 9.43–9.46 ms before horizontal windowing,
as recorded by the preceding pass in [astrelis-viewport.csv](astrelis-viewport.csv)
(`variant=after`). This comparison uses earlier measurements on the same machine,
not interleaved old/new runs. The warm middle-window median was below 0.001 ms;
that is near microbenchmark timing resolution and is not a frame-rate claim.
The scroll fixture uses changing numbered source slices, so cache reuse of an
identical repeated string does not hide shaping work.

## Native checks

`--long-lines` opens two 100,000-character lines (plain and colored), plus short
Unicode and instruction lines. Checked fullscreen with the release app:

- Command+Right reaches the actual column 100,000 and reveals the tail.
- Shift+Command+Left selects the entire line and shows the correct visible selection.
- Collapsing the selection and typing at the tail edits the correct source position.
- Short Unicode text retains the general renderer.

The final native run initially retained 869 source bytes across all five logical
lines and prepared the frame in 2.47 ms, including 1.57 ms of CPU work preparing GPU
resources. At the far end, individual ASCII text events prepared in roughly
0.23–0.32 ms and retained 678–681 source bytes. These counters describe one retained
source String per cached line, not total process memory or GPU atlas allocation.

[Native raw spot checks](editor-horizontal-native.csv) contain both the initial
validation and the final build after removing redundant caret-line preparation.
They are not a controlled frame-rate or latency distribution. One final
handler-to-prepare sample was 10.6 ms despite 0.31 ms of preparation; the initial
run also had a 40.1 ms sample. Host scheduling/presentation needs separate profiling
before claiming sustained 120 Hz or an 8.33 ms end-to-end input budget. GPU
execution and actual presentation timestamps remain unmeasured.

## Verification and next work

All 51 workspace tests pass, plus formatting, all-feature/all-target Clippy with
warnings denied, and the release build. New checks cover bounded window size,
scroll cache reuse, full-document caret reveal, selection clipping, document-relative
pointer positions outside the viewport, style/provider clipping, and transitions
between virtualized ASCII and Unicode/mixed-metric/inline fallback.

```sh
cargo run --release --locked -- --long-lines
VELK_PROFILE=1 cargo run --release --locked -- --long-lines
cargo bench -p velk-editor-widget --bench viewport --features performance --locked
```

Next priorities are run/cluster-aware windows for styled and inline text, incremental
source eligibility after edits, and native scheduling/presentation tracing. Full-line
backgrounds can be handled separately from text chunks in a follow-up without
requiring general rich-text shaping changes.

The next pass adds rich metric runs and inline labels; see the [rich-window and native timing report](rich-horizontal-and-scheduling.md).
