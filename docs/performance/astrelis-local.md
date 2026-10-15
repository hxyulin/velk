# Local Astrelis text preparation optimizations

This is a historical experiment report. The improvements are now integrated into
Astrelis `main` at `4801b66`, and current Velk builds use RXUI's pinned Git backend
without a local path override. See [the current dependency setup](../../README.md#workspace).
The revisions and local paths below describe the measurement setup at the time.

Measured on 2026-10-15. RXUI remains pinned to `093d05d9a2fa5491c3cebe26be46b8720b4c6158`.
Astrelis was cloned into `~/dev/projects/astrelis` and checked out from RXUI's pinned
`d8ac1d1b5fd0c57d5ae0811abb3a004f6e3f6923`, on branch `perf/editor-text`.
The optimized local revision is `9c60dfdd2a2ec6800c701792eff8dd849c90ee48`.
Velk patches both `astrelis` and `astrelis-winit` to sibling paths in its root manifest,
so RXUI and its native host use the same local backend.

## Changes

- Resolve overlapping rich spans with a boundary sweep and ten independent sets of
  active providers. The latest provider setting each field wins, including explicit
  false decoration values. Empty spans are ignored; default-only spans still preserve
  their covered ranges. This stage becomes O(S log S), rather than checking every
  provider at every boundary. Adjacent equal resolved styles still merge.
- Binary-search sorted resolved segments before building each paragraph's attributes,
  and stop at its end. This avoids a full segment scan for every paragraph while
  preserving global segment IDs used for glyph colors.
- Add `TextShaping::{Advanced, Basic}` and `TextBuffer::set_shaping`. Advanced remains
  the default. Basic uses Cosmic Text's direct character-to-glyph mapping, enabled
  through its existing `swash` feature; it skips OpenType substitution/positioning.
  Strategy changes rebuild backend paragraphs and preserve old immutable snapshots.
- For printable ASCII, build the interaction boundary vector directly and map glyph
  cluster endpoints to boundary indices without binary searches. This works with
  advanced shaping too, including ligatures and wrapping. General Unicode indexing
  remains unchanged.
- Velk opts into Basic only for printable ASCII with no text-style spans and no active
  IME composition. If the selected face is not monospace or glyphs are missing, it
  rebuilds with Advanced. Unicode, controls, styled text, and inline projections
  retain Advanced. Background-only annotations can still use Basic.

These changes reduce CPU text preparation. They do not change the glyph atlas,
GPU draw pipeline, event-driven frame scheduling, viewport cache, or IME fallback.

## Measurements

The release executables for the pinned and patched backends were run twice each,
sequentially, after builds and correctness checks finished. Values below are ranges
of the two run medians; p95 values remain separate in the raw CSVs. There was no
concurrent Rust compilation during these runs. They are local microbenchmarks,
not a sustained 120 Hz presentation measurement. CPU intervals exclude GPU work,
window submission, and display presentation.

The viewport harness uses installed Menlo at 16/24 logical units and the same
fixtures, samples, Rope mutations, projection, shaping, and interaction preparation
as the earlier report. Its long-line fixtures are still prepared in full.
The backend scaling harness uses an embedded Source Sans 3 font, 11 measured cold
buffer preparations after two warmups, and Advanced shaping in both variants.
It includes buffer allocation, spans, shaping, layout, and interaction preparation.

| Workload | Pinned median range (ms) | Patched median range (ms) |
| --- | ---: | ---: |
| Edit a 100,000-character ASCII line | 14.35–14.83 | 9.43–9.46 |
| 10,000-character line with 1,000 styles and inline hints | 15.69–15.86 | 12.20–12.24 |
| 5,000 paragraphs with one rich span each | 66.21–67.69 | 19.32–20.27 |
| One 55,000-byte line with 5,000 rich spans | 146.76–147.25 | 115.67–115.88 |

The plain long-line fixture improves by roughly 35%; the dense inline fixture by
roughly 23%; the rich-paragraph fixture by roughly 70%. The pathological single
rich line remains expensive. The 8.33 ms budget still requires horizontal
virtualization/chunking, rather than preparing every glyph of a visible logical line.
Small samples vary with machine state; the raw CSV includes all ordinary viewport
cases and span counts, so improvements are not inferred from a selected fastest run.

Raw results: [viewport before/after](astrelis-viewport.csv) and
[fixed-font rich-text scaling](astrelis-rich.csv).

## Verification and commands

Velk: 47 tests; Astrelis: 174 unit tests plus 28 doc tests, including native-host and
GPU rendering tests. Both repositories pass formatting and all-target Clippy with
warnings denied; Velk also passes the all-features checks and release build.
The new checks compare the sweep with the previous algorithm on randomized overlaps,
compare dense ASCII interaction with the general index including ligatures/wrapping,
exercise shaping transitions and insertion of paragraphs, and compare Menlo ASCII
glyphs/carets/hit testing against Advanced before verifying Unicode and IME fallback.
The rebuilt native rich-text showcase starts successfully.

```sh
# From Velk; requires the patched sibling Astrelis checkout.
cargo bench -p velk-editor-widget --bench viewport --features performance --locked
cargo run --release --locked -- --showcase

# From ../astrelis.
cargo bench -p astrelis --bench rich_text --locked
cargo test --workspace --locked
```

The backend benchmark source uses APIs available in the pinned revision, so it can
also be built against that revision for comparison. The recorded baseline used an
isolated temporary Cargo package depending on the pinned Git revision; the viewport
baseline used its retained release executable built before applying the local patch.

The local path patch is an experiment dependency: Cargo.lock does not pin edits in
`../astrelis`. To reproduce this version, retain the local backend commit above.
Moving back to a Git dependency requires publishing and pinning the backend revision
that supplies `TextShaping`; removing the patch alone will not compile the new API use.

These measurements describe Velk before horizontal virtualization. See the [follow-up report](horizontal-virtualization.md) for the subsequent renderer.
