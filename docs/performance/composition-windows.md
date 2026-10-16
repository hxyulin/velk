# Unicode and IME composition windows

> Historical analysis: raw measurements, capture manifests, and rendered reports
> are generated locally and are not tracked. Artifact filenames below identify
> the original runs; the reported values describe those runs. See the
> [profiling guide](../profiling.md#generated-artifacts) for new captures.

2026-10-15, Apple M3 Pro, macOS 27.0.1, Rust 1.99.0, release builds.
This follows [the responsiveness pass](responsiveness.md), Velk `d9b420f`.
The follow-up [native redraw experiment](native-input-redraw.md) tests callback
rendering, records its burst regression, and isolates remaining long-line edit cost.
RXUI remains pinned to `fc331afca247579cde12a480ed01c687abb1ff5d`; the local
Astrelis patch remains `4801b66` on `perf/editor-text`. No backend API changes
were needed for this pass. `unicode-bidi 0.3.18`, already in the dependency graph,
is now an explicit widget dependency for directional classification.

## Composition projection

IME now renders through a transient Rope: clone the source Rope, remove the
source selection and insert preedit. Ropey's persistent chunks avoid flattening
the whole source into a String. Only affected paths are changed; the editor's
source Rope, selection and undo history remain unchanged until the native commit.
The projected document uses the regular logical-line viewport cache, including
when a selection spans source lines. An explicit preedit range supplies underlines
without turning it into source selection. Cursor offsets are converted from
UTF-8 to projected character positions and snapped by the layout's caret geometry.
An absent native preedit cursor hides the painted caret but retains candidate
geometry. Commit/cancel returns to the source cache and drops projected resources.

Projected hit positions map through the replaced source range. Positions inside
preedit map to its source anchor; the suffix shifts back by inserted/removed
character counts. Source style ranges split around the replacement, and virtual
labels inside it disappear. Surviving styles and labels keep their original hover
provider IDs. This is tested against the original full-document composition path.

A rendering regression found during reference checking is fixed: two pieces of
one full-line translucent background must paint once on their shared joined line.
Retained provider groups now deduplicate those backgrounds across the split.
Other styles remain separate, so source colors/fonts do not leak into preedit.

## Guarded Unicode windows

A long logical line may use a Unicode context index when:

- Source outside one bounded Unicode region is printable ASCII with the existing
  verified uniform monospace advances.
- The first-to-last non-ASCII region spans at most 4,096 source characters. Up to
  128 ASCII characters on each side are retained with it. No grapheme, emoji ZWJ
  sequence or contextual Unicode run inside that region is cut into windows.
- Directional classification permits LTR/neutral content, combining marks and
  ZWJ/ZWNJ. RTL, embedding/isolate controls, tabs and unsupported controls opt out.
- The context shapes as a single visual line at bidi level zero, and its ASCII
  glyph clusters/advances still agree with the external grid.
- The line has no inline/overpaint labels or metric-changing styles. Foreground,
  non-full-line backgrounds and nonmetric decorations remain supported. Rich
  metric/inlay windows continue using the earlier ASCII path.

The complete context is shaped with advanced shaping and retained for caret/hit
geometry. Prefix/suffix positions use the verified ASCII grid. Pixel-aligned
windows retain the entire context whenever they intersect it; otherwise they
prepare ordinary ASCII slices. Minimum line metrics retain the global baseline
even with the Unicode context offscreen. The context and the extra window overscan
bound extraction/shaping/GPU preparation for eligible fixtures, not total metadata
work for every possible document.

This is a conservative Unicode fast path, not general bidi virtualization. Long
all-Unicode lines, widely separated Unicode regions, RTL and unsupported rich
lines keep full-line shaping. Multiline composition still avoids whole-document
shaping even when an individual projected line takes that fallback.

## Paired interaction measurements

The same updated `responsiveness` harness ran against a detached `d9b420f`
checkout and the new implementation. There were three sequential before/after
pairs, no native preview or concurrent build, at 1,200 × 800 logical units and
raster scale one. GPU readback is excluded. Percentiles use nearest rank.

Ordinary cases retain 20 warmups/201 samples per run; composition stress cases
use two warmups/21 samples. The long-line fixtures compose at the end of 100,000
ASCII characters. CJK alternates Japanese preedit; emoji alternates complete ZWJ
sequences; RTL alternates Arabic/Hebrew and intentionally takes the fallback.
The multiline fixture selects the first 1,000 lines of a 10,000-line document,
leaving about 9,000 lines after the replacement. Selection setup is outside timing.

| Warm dispatch → CPU submission | Before p50 | After p50 | After p95 |
| --- | ---: | ---: | ---: |
| 100k-character line + CJK preedit | 18.557 ms | 0.392 ms | 0.430 ms |
| 100k-character line + emoji preedit | 18.603 ms | 0.385 ms | 0.415 ms |
| Replacement spanning 1,000 lines | 26.540 ms | 0.131 ms | 0.171 ms |
| 100k-character line + RTL preedit | 27.025 ms | 26.971 ms | 27.996 ms |
| Small-line preedit in a 10k-line document | 0.117 ms | 0.126 ms | 0.184 ms |

Each row has 63 samples per variant. Both versions use one submitted frame per
interaction. Warm ordinary typing/navigation remain close to the previous timings;
the added projection has a small overhead on already-cheap small-line composition.
The RTL case remains above the 8.33 ms target and is explicitly not improved here.

The long-line CJK/emoji windows retain 296/298 source bytes per line, compared with
100,000 before. These are counters for a cached source String, not total memory
or RSS. The multiline fixture retains 921 bytes across 38 cached/visible lines;
the earlier fallback's zero counters meant uninstrumented full-document work,
not zero memory or zero rendered lines.

## First preedit and GPU completion

First-preedit probes use a fresh process for each fixture/version/run, source
already rendered, zero preedit warmups and one measured update. Three runs are
retained per variant. This includes first-use preedit shaping/raster preparation;
it excludes app startup/system-font discovery and does not flush OS/driver caches.
The small sample count is not a latency distribution.

| First-preedit dispatch | Before CPU p50 | After CPU p50 | After GPU-observed p50 | After GPU-observed max |
| --- | ---: | ---: | ---: | ---: |
| Long-line CJK | 21.532 ms | 0.692 ms | 3.158 ms | 3.191 ms |
| Long-line emoji | 21.665 ms | 0.766 ms | 4.652 ms | 4.713 ms |
| Multiline replacement | 108.415 ms | 1.030 ms | 2.027 ms | 2.124 ms |
| Long-line RTL fallback | 29.484 ms | 29.930 ms | 33.629 ms | 33.898 ms |

A separate warm GPU-wait run, 21 samples per stress case, observes completion
at p95 1.485 ms for CJK, 1.539 ms for emoji and 1.439 ms for multiline replacement.
RTL remains 30.170 ms. Completion is observed by a blocking device poll; it is
not a hardware duration query or display presentation. Waiting changes workload
cadence, so this is a separate run, not a subtraction from CPU-only samples.

All **8,904 raw samples**, including ordinary controls, first-preedit samples,
GPU-wait samples, frame counts and cached-byte/visibility counters, are in
`editor-composition-windows.csv`.

## Native check and outstanding latency

`--unicode-long-lines` places mixed CJK, combining Latin, emoji ZWJ and a flag at
the tail of approximately 110,000 ASCII characters. The full-screen native check
explicitly navigated to document start then the first logical line's end. The
Unicode tail, background and caret rendered correctly. macOS Option+E dead-key
preedit reached the projected path; completing accented input exercised the native
commit path. The profiled projected windows retained 484–486 source bytes across
four cached lines. Native composition cancellation remains covered by automated
events; a CJK candidate popup has not been independently inspected.

The native smoke check also reproduced a scheduling outlier. Frame 19 reports
41.489 ms dispatch → preparation and 43.509 ms dispatch → submission. The input
callback consumed 0.147 ms, request → redraw delivery 0.139 ms, host preparation
1.640 ms, and acquisition 0.076 ms. Most delay was before the redraw request:
input → request was 41.346 ms. This agrees with the earlier investigation's
location of outliers outside text preparation; it does not establish their exact
platform cause. No scheduling or presentation-policy experiment was retained.

`editor-composition-native.csv` contain the complete 133-frame
interval, including startup/activation, idle blinks and eight input-bearing frames.
They are exploratory, instrumented, automated events with no before/after pairing
or controlled cadence. Widget profiling prints both the inner viewport record and
`Velk projected frame`, whose total also includes projection and wrapper work;
profiling output itself has overhead. Do not treat these frames as a sustained
latency distribution or a physical key-to-photon measurement.

## Validation and remaining work

64 tests pass. Full-layout comparisons cover glyph positions/baselines, caret/hit
positions, combining marks, CJK, Hangul, emoji/flags, Indic and RTL fallback.
GPU-backed comparisons cover painted composition, caret/hit/hover mapping,
multiline replacement, inline hints, mixed metrics, translucent background provider
identity and direct multiline preedit descriptions. Commit/cancel, resize and
return to the original source cache are checked. Formatting, all-target/all-feature
Clippy with warnings denied, and the release build pass.

Eligibility/classification still scans logical-line text after source revisions.
Projected annotation metadata rebuilds when selection, source snapshot or inserted
character count changes; large provider sets can dominate. New projected height
metadata scales with projected line count on first composition. General Unicode
cluster/bidi indexing, localized classification/annotation updates and native
event-loop latency need further work. The renderer's gains are not an end-to-end
120 Hz guarantee.

```sh
cargo run --release --locked -- --unicode-long-lines
cargo bench -p velk-editor-widget --bench responsiveness --locked > /tmp/windows.csv
python3 scripts/latency.py summary /tmp/windows.csv
VELK_BENCH_CASE=long_line_ime cargo bench -p velk-editor-widget --bench responsiveness --locked
VELK_BENCH_CASE=long_line_emoji_ime VELK_BENCH_WARMUP=0 VELK_BENCH_SAMPLES=1 VELK_BENCH_GPU_WAIT=1 cargo bench -p velk-editor-widget --bench responsiveness --locked
```

For the paired baseline, copy the current benchmark file into a detached checkout
at `d9b420f`, keep the same sibling Astrelis revision, and run the same commands.
The existing `[[bench]]` entry already registers this harness. Each fresh-process
sample needs a separate command invocation; normal warm samples use default counts.
