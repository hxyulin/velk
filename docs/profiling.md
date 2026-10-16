# Profiling and tracing

Velk installs executable-owned `tracing` diagnostics before opening the document
or starting workers. Detailed tracing is idle by default and can be switched on
without rebuilding or restarting. Ordinary builds retain filtered structured
logging. The public application CLI remains `velk <file>`.

## Capture an interaction

Open the command palette (Command+Shift+P / Control+Shift+P, or F3) and run:

1. **Diagnostics: Start trace capture**.
2. Reproduce the edit, scrolling, hover, search or plugin lifecycle operation.
3. Optionally run **Diagnostics: Add trace marker** to mark an interesting point.
4. Run **Diagnostics: Stop trace capture**. The status bar reports the file path,
   event count, dropped event count and stop reason.

File creation, finalization, flushing and joining the trace writer happen on the
blocking pool. Producers never wait for disk or for space in the trace queue.
Capture stops automatically after 30 seconds by default. Stop also reports the
last completed automatic capture. Start can then begin another capture.

Files default to the system temporary directory's `velk-traces` subdirectory,
with timestamp, PID and sequence in the filename. They are Chrome Trace Event
JSON, suitable for opening with Perfetto's **Open trace file**. Captures are local
files; the app does not upload them or open an external viewer automatically.

```sh
# Capture startup as well as interactions, with a 60-second limit.
VELK_TRACE=1 VELK_TRACE_SECONDS=60 VELK_TRACE_DIR=/tmp/velk-traces \
  cargo xtask run --release -- crates/editor/src/main.rs

# Print timing distributions without opening a browser.
cargo xtask trace-summary /tmp/velk-traces/<capture>.trace.json
```

`trace-summary` reports count, inclusive total, p50/p95/p99 and maximum duration
per scope. `[wall]` rows are asynchronous elapsed lifetimes, including waiting;
the other rows are elapsed synchronous/poll scopes on their thread. Totals
include nested scopes and must not be summed as CPU utilization. Input-to-prepare
and queue-wait samples are reported separately. In-flight async spans at the
capture boundary are reported as incomplete, rather than assigned a fabricated
duration.

## Generated artifacts

Keep benchmark CSVs, capture manifests, rendered reports, logs, traces, and
profiler output local. These files depend on the machine, build, and interaction
being measured; they are not source inputs. The repository ignores generated
CSV, JSON, and HTML files under `docs/performance/`, as well as common trace,
log, profiling, and Python cache outputs. Authored analysis notes, benchmark
harnesses, scripts, and experiment patches remain versioned.

Prefer Cargo's ignored `target/` directory or a system temporary directory for
new results. For example:

```sh
mkdir -p target/performance
cargo bench -p velk-editor-widget --bench responsiveness --locked \
  > target/performance/responsiveness.csv
python3 scripts/latency.py summary target/performance/responsiveness.csv
```

The historical notes in `docs/performance/` retain their interpretation and
reported measurements, but their raw capture files and HTML reports are not
stored in Git. Regenerate samples for the revision and hardware under test;
exact timings and native interactions will vary. Share selected findings in
review or an issue rather than committing a full capture.

## Coverage and interpretation

The existing RXUI/Astrelis `profiling` instrumentation is connected through
`profiling/profile-with-tracing`. Current coverage includes:

- RXUI input dispatch, view evaluation, style resolution, layout, task polling,
  accessibility updates, GPU resource preparation and UI composition.
- Astrelis frame preparation, surface acquisition, command recording, submission,
  text layout/measurement, glyph rasterization and uploads.
- Editor key/text/pointer handling, transactions, viewport preparation, line/cache
  activity and recording. Input IDs and document revisions identify the work.
- Tree-sitter grammar initialization, queued/coalesced parse requests, incremental
  parsing and viewport highlight queries.
- Save submission, worker queue wait, file I/O and completion; session encoding
  and replacement-process launch during Reload Window.
- Plugin request queue wait/execution, image loading, factories, restart and
  teardown. Request spans distinguish waiting in the queue from execution.
- Built-in plugin work: search protocol/matching on its own worker, Markdown
  parsing, LSP process spawn, initialization, message encoding/decoding, async
  message waits, hover round trips and shutdown.

Painter preparation is split into RXUI pipeline, text, image, custom-widget and
composition stages. Composition planning and new layer allocation have separate
scopes. Astrelis further separates shape, line, path and text pipeline preparation,
with coverage and MTSDF text variants distinguished. On a mesh pipeline cache miss,
`astrelis::create_mesh_pipeline` includes validation/setup and a nested
`astrelis::wgpu::create_render_pipeline` scope measures the host API call. Cache
hits return before either creation scope. These scopes measure host elapsed time,
including any driver blocking, rather than GPU execution.

Async Rust work uses `tracing::Instrument` / `#[tracing::instrument]`. Entered
spans describe individual polls; an `async_work = true` field adds a separate
wall-time lifetime. Never retain a normal entered CPU span across an `.await`.
A blocked scope's elapsed time includes blocking; this is not sampled CPU time.

**Input latency here is editor input-listener entry to completed CPU preparation.**
It excludes the earlier OS event path, GPU execution, compositor scheduling and
panel scanout. Frame acquisition and finish scopes expose host waits/submission,
not GPU timestamp queries. No GPU readback or GPU completion wait is introduced.
Use native sampling/GPU tools alongside these traces when investigating actual
CPU utilization, allocations, Metal execution or input-to-photon latency.

## Limits and overhead

Capture has an 8,192-event bounded queue, a one-million-event limit and a 128 MiB
file budget. On overload, producers drop events instead of blocking. A final
`velk.capture.summary` event records losses and stop reason. The writer reserves
space for final metadata. A lossy capture is useful for investigation but cannot
provide complete timing distributions.

Field strings are truncated to 256 characters, and the field visitor bounds
string/debug fields. Velk's instrumentation records operation names, IDs,
revisions, sizes and statuses rather than document text, search query contents,
hover Markdown or raw LSP message bodies. Error messages and explicitly enabled
third-party debug logging may contain paths or other application data.

A local optimized microbenchmark of a simple structured scope measured roughly
80 ns with capture disabled and 1.27 microseconds while capturing, with zero drops
in that run. This is instrumentation overhead, not an editor responsiveness
benchmark or a guarantee across machines. Capturing many small scopes adds
measurable overhead; narrow the targets and compare with tracing disabled.

```sh
cargo bench -p velk-diagnostics --features collector --bench instrumentation
```

## Configuration and logs

| Variable | Meaning |
| --- | --- |
| `VELK_TRACE=1` | Start a capture during application startup. |
| `VELK_TRACE_DIR` | Output directory; default is system temp + `velk-traces`. |
| `VELK_TRACE_SECONDS` | Automatic duration, 1–600 seconds; default 30. |
| `VELK_TRACE_TARGETS` | Comma-separated target prefixes; default `velk,rxui,astrelis`. `*` includes all tracing targets. |
| `VELK_LOG` | `tracing-subscriber` filter directives; defaults to `warn,velk=info`. Falls back to `RUST_LOG`. |
| `VELK_LOG_FORMAT=json` | Emit app-owned logs as JSON records; otherwise readable text. |
| `VELK_LOG_DIR` | Optional directory for `velk-<pid>.log`; otherwise stderr. |
| `NO_COLOR` | A nonempty value disables terminal log colors. |

For example, `VELK_TRACE_TARGETS=velk_plugin,velk_syntax` narrows a capture to
plugin and syntax activity. `VELK_LOG=warn,velk_plugin_host=debug` enables detailed
host logs. Target filtering applies to SDK timings as well as host tracing;
control markers/summary metadata remain in the capture.

Readable logs use colors when stderr is a terminal, unless `NO_COLOR` is nonempty
or `TERM=dumb`. Redirected output, log files, and JSON logs remain uncolored.
Language-server stderr is piped through the host as INFO records with source
`velk_plugin_lsp::server`. The server's text is preserved; stderr itself does not
provide a reliable severity. Lines are truncated to 4 KiB and drained through EOF
during shutdown.

Logs use `tracing-appender`'s bounded 4,096-line nonblocking queue. Log overload is
lossy and its dropped-record count is reported on shutdown. A guard outlives the
application and plugin runtime, flushing logs and finalizing the current trace
on normal exit, including an early file-open error. With `VELK_LOG_DIR`, app logs
are separate from native OS and child language-server stderr. A forced kill or
abort can leave an unfinished trace; this is not a crash-recovery journal.

Reload Window continues an active capture in a **new, separate PID trace file**.
The old process finalizes its file during shutdown; the replacement captures
startup and restored work. The restored status reports the replacement's path.
Clock origins are per process; files are not silently merged. An idle/completed
capture is not automatically restarted on reload.

## Optimized builds for sampling

```sh
cargo xtask profile -- crates/editor/src/main.rs
```

This builds the editor and built-in pack under Cargo's `profiling` profile
(optimized like release, with packed debug symbols and stripping disabled), stages the
matching plugin pack, enables the existing `performance` feature for detailed viewport/cache counters,
and runs with startup tracing enabled. Artifacts live under
`target/profiling` (including dSYM bundles on macOS). They can be used with macOS Instruments Time Profiler and
other native sampling/debugging tools. These tools are separate from the app's
scope tracer; the app does not spawn an external profiler.

## Instrumenting plugins

Rust globals in separately loaded DLLs are not used to share a tracing subscriber.
The SDK automatically registers the optional `velk.diagnostics.attach.v1` named
service when a registration slot is available. A plugin already using the full
hook capacity remains loadable with host boundary timings. The host copies its process-owned callback table into the plugin image.
SDK guards call the host clock and forward borrowed names synchronously; the
host copies them into its bounded queue. The same `DiagnosticsSink` C table also
provides log-level queries and log emission. Plugins use the host's filtering,
formatting, colors, output queue, and file configuration; they do not install a
subscriber or writer. No Rust spans, futures, allocators or
subscriber trait objects cross the C ABI. The root ABI stays at version 1.

```rust
use velk_plugin_sdk::diagnostics::{self, Level};

diagnostics::log!(Level::Info, "Indexed {} files", count);
```

Plugin records use the host target `velk_plugin`; `plugin_target` preserves the
SDK caller's module path. Use `VELK_LOG=warn,velk_plugin=debug` to enable debug
messages. Logging works without an active trace capture. Disabled messages avoid
formatting; messages are bounded to 8 KiB of UTF-8, and source names to 256 bytes.
Without a host attachment the facade is silent. Callbacks may run concurrently
and copy borrowed views before returning, so queued output survives plugin unload.
The timing and logging callbacks are the existing host-service boundary, rather
than shared Rust globals or trait objects.

```rust
let _cpu = velk_plugin_sdk::diagnostics::scope!("my_plugin.parse");
// Synchronous work only; do not hold this across await.
```

```rust
let _operation = velk_plugin_sdk::diagnostics::async_scope!("my_plugin.request");
// Owned wall-time lifetime; may span awaits and move between threads.
```

`async_scope!` records waiting time, not CPU occupancy. CPU scopes can be nested
around synchronous portions of the operation. Guards created outside a capture
are disabled; work already in flight when capture starts may only have its later
suboperations recorded. Host/SDK session checks reject late events from a
previous capture. Plugins must still join workers before unload. Callback code is
executable-owned and remains valid for process lifetime; copied names remain
host-owned after a plugin unloads. Older plugins without the optional hook still
load, with host boundary timings only. Arbitrary plugin-local `tracing` subscribers
are not automatically installed or shared. For a third-party plugin crate, add
its module prefix to `VELK_TRACE_TARGETS` or use `*`; the default prefixes cover
Velk and its built-ins.

Validation helpers:

```sh
cargo xtask test-diagnostics  # Real DLL logging/filtering and worker timing bridge.
cargo xtask test-plugins     # Existing ABI, pack, restart and service checks.
```
