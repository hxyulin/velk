# Async syntax pipeline and UI-thread audit

2026-10-15, Apple M3 Pro, Rust 1.99.0, optimized Cargo bench profile.
The previous implementation parsed in the background but compiled the grammar/query
on the caller and executed viewport queries from line preparation on the UI thread.
This change moves both operations, and capture index construction, to the persistent
syntax worker. Syntax trees are now created, edited, queried and destroyed there.

The UI submits cheap Rope snapshots/edit coordinates on document changes and source
character windows on viewport changes. It never waits for the service. Each mailbox
has capacity one; a new request replaces queued work. Document and viewport generations
are separate, so scrolling cancels obsolete queries without restarting the parser.
Tree-sitter progress callbacks and capture iteration check cancellation. A burst of
edits therefore does not create a thread or job backlog. Cancellation is cooperative,
not a hard deadline for a single parsing or predicate operation.

The worker combines overlapping/touching requested ranges, queries them, and publishes
an immutable capture snapshot with a sorted interval index and priority information.
No full-document highlighting pass is introduced. The UI uses binary searches to find
intersecting cached captures, preserves query precedence, and applies colors to the
bounded prepared line/window. New viewport results are accepted even when the document
revision is unchanged. Repeated prepares/cursor blinks with identical ranges submit
no new query work. Empty results still carry coverage so empty syntax is a valid result.

During IME composition, the renderer submits source ranges corresponding to visible
committed text. It excludes the inserted preedit and removed selection, avoiding a
query of an enormous selected region represented by a short preedit. Old captures
still map through source edits until a newer snapshot arrives; newly exposed or
inserted text can remain plain briefly. Captures outside the current viewport are
not retained as a document-wide highlight cache.

## Executor choice and remaining UI work

RXUI's pinned native `Application` already installs
`ThreadPoolExecutor::new(2, 2)`: two async workers and two separate blocking workers.
Result notification uses that pool and RXUI's native wakeup adapter. One persistent
CPU thread per syntax-enabled editor performs parsing/querying. It does not occupy
an async executor thread while computing or waiting for document requests.

Tokio would be useful if future services need its filesystem, network, timer or
process APIs. Installing a second runtime now would duplicate existing execution
services without moving CPU work by itself. Long CPU work would still need an
explicit dedicated worker or bounded blocking pool. This implementation keeps
async transport independent of the executor and adds no Tokio dependency.

The UI still handles input, immediate Rope mutations/selection, bounded edit
metadata, cached-capture projection, visible layout/shaping, hit testing, GPU
preparation and submission. These are not claimed asynchronous. Moving mutable
font/layout/GPU state to another thread needs a separate rendering ownership design;
this change does not alter RXUI or Astrelis. Existing large/complex-line fallback
costs remain. Initial file I/O and UTF-8/newline decoding currently happen before
the native event loop starts; startup can still wait on a large/slow file, although
there is no active editor UI to block. A loading-window flow is a separate task.

## Measurements

```sh
cargo bench -p velk-syntax --bench syntax --locked
VELK_BENCH_CASE=rust_plain_typing VELK_BENCH_WARMUP=10 VELK_BENCH_SAMPLES=101 \
  cargo bench -p velk-editor-widget --bench responsiveness --features performance --locked
VELK_BENCH_CASE=rust_syntax_typing VELK_BENCH_WARMUP=10 VELK_BENCH_SAMPLES=101 \
  cargo bench -p velk-editor-widget --bench responsiveness --features performance --locked
```

The parser microbenchmark inserts spaces in the middle line, 101 samples, as in the
initial report. Added measurements separate worker viewport preparation (query,
priority ordering, range merging and indexing) from 40 indexed per-line lookups.
The indexed lookup measurement excludes UI style conversion, grapheme projection,
shaping and GPU work.

- Real `main.rs`, 4,578 bytes: background viewport preparation median/p95
  **0.0794 / 0.0910 ms**; 40 indexed line lookups **0.0121 / 0.0170 ms**.
- 10,000 repeated Rust function lines, 500,000 bytes: background viewport
  preparation **0.2115 / 0.2332 ms**; lookups **0.0270 / 0.0335 ms**.
- 100,000 repeated Rust function lines, 5,000,000 bytes: incremental parsing
  **76.8395 / 107.0073 ms**, background viewport preparation **0.2082 / 0.2228 ms**,
  and indexed lookups **0.0260 / 0.0304 ms**.

The responsiveness cases use the same 100,000-line, 4,200,000-byte Rust document,
1200×800 offscreen target, 10 warmup inputs and 101 measured inputs. They alternate
insertion/backspace at the beginning of the file. Both install RXUI's shared 2+2
executor; the syntax case waits for its initial captures before collecting samples.
Ready background completions are polled at frame boundaries. Incremental parsing
runs concurrently with the syntax case's typing burst.

- Plain Rust text: input dispatch median/p95 **0.0015 / 0.0040 ms**; first frame
  submission **0.1901 / 0.2812 ms**, maximum **0.2924 ms**.
- Rust syntax enabled: dispatch **0.0033 / 0.0061 ms**; first frame submission
  **0.7424 / 1.0715 ms**, maximum **1.1223 ms**. Each input submitted in one frame.

This is a plain-versus-syntax comparison of the new pipeline, not a controlled
before/after benchmark against the old implementation. First submission does not
wait for the new syntax result. The benchmark excludes native event delivery,
window surface waits, compositor/scanout, and end-to-end highlight turnaround;
GPU completion waiting was disabled. The sub-8.33 ms CPU results support keeping
analysis off the UI thread, but do not establish a native 120 Hz or latency guarantee.
Large-file highlight catch-up can still take tens of milliseconds.

## Validation

Tests hold worker initialization behind a gate while the caller submits 1,000
revisions/viewports and drops the service, proving these operations do not wait for
that work. They verify initialization occurs on another thread and failures arrive
as asynchronous results. Further checks cover viewport coalescing during initial
parsing, scrolling without a document edit, indexed/direct-query equivalence with
overlapping captures, cancellation, Unicode edits/undo/redo, IME request projection,
annotation precedence, horizontal virtualization and async publication/rendering.

The background-launch native preview caveat recorded in the initial syntax report
is not claimed fixed by this scheduling change.
