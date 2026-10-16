# Native keybinding latency check

Checked revision `faf27f4` on macOS ARM64 with an optimized release build.
This is a short local smoke check, not a repeatable performance guarantee.

## Workload

The native app opened a disposable Rust crate containing about 32,000 lines
and 622 KB of text: 8,000 small functions and an intentional type error.
Rust highlighting, rust-analyzer diagnostics, Vim, indentation, and pairing were
active. The interaction included literal typing, a user-bound insert command,
Vim motions, and scrolling. The configuration contained five user bindings.

Capture started at process startup. A trace marker separated startup and setup
from the measured interval. The full capture contained 61,871 events with zero
drops; the interval after the marker contained 37,484 events.

| Scope | Samples | Median | p95 | Maximum |
| --- | ---: | ---: | ---: | ---: |
| Input listener to prepared frame, CPU only | 21 | 2.216 ms | 3.672 ms | 10.078 ms |
| Key dispatch | 94 | 0.016 ms | 0.088 ms | 0.112 ms |
| UI preparation | 959 | 0.298 ms | 1.250 ms | 2.082 ms |
| Painter preparation | 366 | 0.426 ms | 0.644 ms | 1.333 ms |
| Frame acquisition | 366 | 0.132 ms | 3.598 ms | 7.729 ms |

The input-latency maximum occurred on the first edit after the marker. Nearby
main-thread work included a 2.102 ms runtime flush and view/painter preparation.
These nested scope durations cannot be added together or assigned as independent
causes of the outlier. Syntax updates ran on their worker, with a 3.037 ms median
and 8.050 ms maximum in this interval.

The measured shortcut dispatch cost is small. The CPU input path looks acceptable
for this workload, but the sample count is insufficient to characterize rare
stalls. GPU execution, compositor scheduling, and display scanout are excluded.
Tracing itself adds overhead. Input samples can coalesce, so their count is not
the number of keys sent.

## Repeating the check

Build with `cargo xtask build --release`. Use disposable files and isolated
`VELK_SETTINGS_PATH`, `VELK_KEYBINDINGS_PATH`, and `VELK_RECOVERY_DIR` paths under
`target/performance/`. Select `velk.vim` as the keymap provider. Start with
`VELK_TRACE=1`, `VELK_TRACE_SECONDS=120`, and `VELK_TRACE_DIR` pointing to an
ignored output directory. Wait for diagnostics and highlighting, add a trace
marker, then exercise typing, bound commands, navigation, and scrolling.

Stop capture through the command palette and run `cargo xtask trace-summary`
against the completed trace. Inspect the interval after the marker separately
from startup. Retain raw traces and generated summaries only in ignored output.

## Follow-ups observed

- The initially opened file did not enter the configured Vim mode until the
  keymap setting was changed to Standard editing and back to Vim. The measured
  interval began after Normal mode was confirmed in the status line.
- Captures started after app initialization omitted renderer-preparation scopes
  in this run. A continuous capture enabled at startup included them. Investigate
  this before relying on mid-session captures for renderer latency comparisons.
