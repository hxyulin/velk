# Initial Rust syntax highlighting

Measured 2026-10-15 on an Apple M3 Pro (arm64), Rust 1.99.0, optimized Cargo
bench profile. Tree-sitter 0.27.1 and tree-sitter-rust 0.24.2 are linked into the
application; parsing does not launch a process. RXUI/Astrelis revisions remain
unchanged from the application's dependency pin.

```sh
cargo bench -p velk-plugin-tree-sitter --bench syntax --locked
```

The final run was made after the test suites completed. Each incremental sample
inserts one space at the beginning of the middle logical line and supplies the
corresponding edit to the existing parser/tree. There are 101 incremental samples
per case. Each query covers up to 40 following lines. Initial parse is a single
sample and excludes grammar/query initialization and Rope construction.

| Document | Bytes | Initial parse, ms | Incremental median / p95, ms | 40-line query median / p95, ms |
| --- | ---: | ---: | ---: | ---: |
| Actual `crates/editor/src/main.rs` | 4,578 | 0.692 | 0.0139 / 0.0162 | 0.1148 / 0.1304 |
| 10,000 repeated function lines | 500,000 | 38.564 | 4.4217 / 8.5374 | 0.2006 / 0.2246 |
| 100,000 repeated function lines | 5,000,000 | 352.879 | 70.4745 / 104.6208 | 0.2292 / 0.2551 |

These measure parser/tree editing and highlight query CPU work, excluding worker
scheduling, UI span conversion, shaping, GPU preparation, presentation, and
input-to-display latency. The renderer issues and caches queries per prepared
line/window rather than using this exact 40-line batching benchmark. Consequently
the query column is not a measurement of a rendered frame. Ordinary background
applications remained running; these are development measurements rather than a
controlled hardware comparison.

The repeated-function cases stress large numbers of top-level siblings. The
100,000-line result is a real limitation of this first implementation: syntax can
lag rapid edits on very large documents. Its cause has not yet been isolated
between tree editing/copying, parsing, and other snapshot work. Incremental
parsing does not imply constant-time updates. The next performance investigation
should time those phases separately and include representative large Rust files,
multiline comment/string edits, worker turnaround, and native presentation timing.
A byte-range query limits the requested output, but predicates or enclosing syntax
nodes can still make individual queries more expensive; it is not a hard time bound.

The UI never waits for the parser. A persistent worker receives cheap Rope
snapshots and a bounded history of edit coordinates, coalesces pending revisions,
and cooperatively cancels obsolete parsing. Missing edit history triggers a full
parse. Until a result arrives, existing syntax spans are mapped around edited
regions; newly inserted text remains plain and changed multiline syntax can
briefly retain old colors. A result reevaluates queries even when Tree-sitter's
structural changed ranges would be empty, covering identifier text predicates.

Visible syntax is cached across cursor blinking. Offscreen cache entries retain
weak snapshot identities so they do not keep obsolete document trees alive.
Syntax colors preserve the existing horizontal virtualization and are layered
under explicit user annotations, including hover providers and text overlays.
IME composition projects the existing syntax around its temporary replacement;
uncommitted text is not parsed into the document tree.

Native verification opened the actual application source, inserted an opening
block comment, observed following lines recolor, and undid the edits to restore
the source and colors. No source file was saved by that experiment. Unit tests
compare incremental parsing with fresh parsing through Unicode/multiline edits,
undo/redo, missing edit chains, and same-shape identifier changes. Rendering tests
cover a 160,000-character logical line, annotation precedence/provider IDs, and
zero reshaping on a cursor blink. Late-arriving syntax is compared with an
initially highlighted render, and async result publication is compared with an
explicit annotation reference without resetting caret activity. These checks do not establish a 120 Hz or native
latency guarantee for syntax-enabled editing.

One native preview caveat remains: when the temporary app bundle was launched in
the background from the shell, its initial screenshot sometimes showed stale
colors until the window was raised. The CPU glyph layouts already contained the
correct colors, and headless late-publication pixel comparisons passed. Bringing
the window forward showed the full palette; active editing and undo recolored
correctly. The background presentation behavior has not been isolated to the
application, native host, or capture path and is not claimed fixed by this change.
