# Velk

A desktop editor built with Rust and [RXUI](https://github.com/hxyulin/rxui).
The application opens a single UTF-8 file in a native rope-backed text editor.

Current development version: **0.0.0-dev** (set explicitly in `Cargo.toml`).
The dependency-free [`velk` crate](crates/velk/README.md) reserves the package name
for this project; it does not distribute the editor or expose a public API.
The application and internal crates remain unpublished. See the
[name research and release notes](docs/name-research.md) for registry checks,
prerelease behavior, publishing, and a later yank.

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.

## Development

Install [rustup](https://rustup.rs/). The repository pins Rust 1.99.0 in
`rust-toolchain.toml`; rustup installs the toolchain when needed.

Install [prek](https://prek.j178.dev/) and enable the Git pre-commit hook for each
checkout:

```sh
prek install
prek run --all-files
```

The hooks normalize text files to LF, strip trailing whitespace, remove extra
blank lines at EOF, and ensure one final newline in nonempty files. Rust/TOML
changes also run `cargo fmt --all` using the pinned toolchain. Fixers run in order;
if they change staged files, review and stage those changes before committing
again. `.gitattributes` also enforces LF for Git text-file normalization.

Run the editor:

```sh
cargo run --locked -- path/to/file.txt
```

The binary is named `velk`. It can also be run explicitly with
`cargo run --locked -p velk-editor --bin velk -- path/to/file.txt`.

Build and run the normal application:

```sh
cargo build --release --locked
./target/release/velk path/to/file.txt
```

Exactly one file path is required. Quote paths containing spaces. The app reports
usage errors, unreadable files and invalid UTF-8 on stderr before starting the UI.
Files load directly into a Rope; CRLF and CR line endings normalize to LF in the
editing buffer. Loading creates no undo entry and leaves the source file unchanged.
Command/Ctrl+S saves to the opened file asynchronously. A `*` in the window title
and the status line indicate unsaved changes; undo/redo can return to a saved
checkpoint. Closing or quitting a dirty document asks whether to save, discard,
or cancel. Editing stays available while a save runs, and newer edits remain
unsaved when the snapshot finishes writing.

The normal binary has no showcase or stress-test switches. Widget profiling,
frame counters and input timing are compiled only for tests or the opt-in
`performance` feature.

Check the workspace:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Velk installs its own dark color scheme from `crates/editor/src/theme.rs`.
UI surfaces, focus outlines, links, and selections use that explicit palette,
independent of the OS accent color or appearance. Markdown code uses the text
foreground with a monospace face. The source editor retains its syntax palette.

## Local native plugins (experimental)

Build the editor and bundled plugins together:

```sh
cargo xtask build --release
cargo xtask run --release -- path/to/file.rs
```

`cargo xtask build` uses the debug profile. The tool respects Cargo's target
directory (`CARGO_TARGET_DIR` included) and stages native artifacts into
`target/<profile>/plugins/`, beside the editor. `cargo xtask run` sets
`VELK_PLUGIN_DIR` explicitly. Direct launches search `plugins/` beside the
executable; `VELK_PLUGIN_DIR=/absolute/path` overrides that directory.

Plugins listed in `plugins.json` load asynchronously at startup, except IDs in
the `disabled_plugins` setting. Press **F2** to open or close the Plugins page;
**Escape**, an outside click, or its Close button also closes it. Each plugin has
a status dot (running, off, failed) and an enable switch that persists to
settings. The detail pane offers Restart and Disable/Enable, shows load errors
in place, and lists what the plugin adds to Velk, derived from its registered
hooks. **Load from disk** and **Unload all** remain in the command palette.
Controls are disabled while a lifecycle operation is pending.

The status line reports the loaded count and failures. Disk I/O, library loading,
plugin calls, and teardown run serially on one sleeping background worker. There
is no filesystem watcher, polling timer, automatic replacement, or state migration.
Rebuild a plugin, explicitly unload it, then load it to use the new implementation:

```sh
cargo xtask plugins --release
```

This only builds/stages the bundled plugins. Editor recompilation is unnecessary.
The bundled `velk.markdown` plugin renders hover documentation. `velk.lsp` owns
generic language-server transport, protocol handling, and persistent sessions.
`velk.language-server.rust-analyzer` supplies Rust's executable, arguments, settings,
and language match, declaring `velk.lsp` as its transport dependency. There is no
Rust-analyzer default inside the generic transport, and neither plugin links the
other's Rust implementation. The host copies a typed server definition between
them. Unloading either stops dependent sessions; loading it reattaches the latest
unsaved document. Rust Tree-sitter syntax highlighting stays built in. Without
Markdown, hover displays literal provider text.

Plugin IDs use **`author.[namespace.]name`**, with zero or more namespace segments:
`velk.markdown`, `velk.language-server.rust-analyzer`, or
`author.tools.language.server.rust`. The full ID is limited to 63 ASCII bytes;
each segment starts with a lowercase letter and contains lowercase letters,
digits, hyphens, or underscores. Empty segments, uppercase letters, and unscoped
names are rejected by shared SDK/manifest validation. IDs are exact flat keys:
namespace prefixes do not imply dependencies, permissions, ownership, or wildcard
matching. There is no fixed nesting depth beyond the total length bound.

The staging tool publishes immutable, content-addressed libraries and atomically
replaces `plugins.json`. The loader verifies BLAKE3 digests, ABI/layout, metadata,
and lifecycle entry points, then loads a private generation copy. Instance
teardown precedes library release and temporary-copy cleanup. Host-owned result
strings survive unload. Missing/bad artifacts are reported without stopping
editing; a broken manifest does not remove active plugins. Unloading does not
need the manifest to be readable. Old staged build artifacts are retained on disk;
a package cache/cleanup policy is future work.

Plugin authors implement `velk_plugin_sdk::Plugin` and invoke
`velk_plugin_sdk::export_plugin!(Type)` in a `cdylib` crate. The SDK generates the
C entry point, catches unwinding panics inside lifecycle wrappers, and copies
results into host-owned buffers. The experimental ABI contains fixed metadata,
opaque instances, and function pointers; it does not exchange Rust containers or
trait objects. Its contracts are not frozen. Language definitions provide the
first typed inter-plugin dependency: they name an exact semantic transport ID.
Missing transports produce an availability error, and competing definitions for a
language are rejected. Resolution retries when relevant plugins change; ordinary
edits do not re-query definitions. Distribution, general dependency graphs/version
constraints, and generic service discovery remain deferred.

Markdown requests are asynchronous to the editor. The host uses a bounded queue
(16 pending operations) and executes the synchronous C function on its plugin
worker. A single typed run batch is copied into host-owned text/style/link data;
there is no JSON round trip, plugin-owned future, retained callback, or extra
thread per render. Input is limited to 1 MiB, output to 4 MiB and 65,536 runs.
Dropping an async request closes its result channel: queued work is skipped, while
an already executing call finishes normally and its unwanted result is discarded.
Unload is serialized after active calls, so their library cannot disappear beneath
them. Existing hover documents remain valid after unload. Requests processed after
unload use literal text; loading the provider restores formatting for new requests.
A full queue reports backpressure rather than blocking the UI. Provider errors
also fall back to literal text in the app. An active render can delay unload; it
cannot block the UI event loop. Multiple Markdown providers are reported as a
conflict until explicit provider selection is implemented.

The internal ABI stays at v1 (`velk_plugin_entry_v1`) during development. Its
layout is experimental: rebuild/stage bundled plugins with `cargo xtask build`
after SDK changes. Layout size checks reject incompatible function tables.
Versioned compatibility will be introduced when the plugin API becomes public.

Semantic providers have a separate ownership model. The host creates an opaque
session and retains its thread-safe event sink until the plugin's session destroy
returns. The SDK runs each backend on one Tokio reactor, with typed C input views
and event callbacks; no Rust future, trait object, Rope, or allocator ownership
crosses the library boundary. Document snapshots are coalesced before marshaling
on the host worker; the UI only clones the Rope. Document text is bounded at
64 MiB, event output at 4 MiB/65,536 diagnostics. Unix paths preserve native bytes.
Unload cancels/joins every session before destroying the plugin instance. Callback
results are copied into host-owned data, and old queued results are cleared before
reattachment. Host semantic endpoints survive unload and replay the latest source
snapshot when the provider returns. No polling loop or extra host reactor/thread
is added for these endpoints.

`velk.lsp` requests **resident code**: Tokio's Unix process support installs global
signal hooks which can outlive a reactor. Logical unload still closes/reaps the
server, joins the reactor, destroys sessions/instances, and drops their data, but
that library image/private copy stays mapped until process exit. Reloading the
same digest reuses its image; each different build retains another image. The
modal identifies this with “Code retained until exit”. Markdown retains ordinary
library release behavior. Fully reclaimable LSP code would require moving process
and reactor ownership into a host service or an unregisterable transport.

The Rust support plugin defaults to `rust-analyzer` on PATH
(`VELK_RUST_ANALYZER` overrides it). It also owns the existing
`VELK_LSP_CONFIG=/path/to/config.json` override, using `executable`, `arguments`, `root`, `initialization_options`, and
`configuration`. The current editor still routes only Rust documents to semantics.
One session owns one document/server; multi-document sharing is future work.

These are trusted in-process native plugins. Lifecycle/input calls are serialized;
only the explicit semantic sink/context may be retained through a session. All
session producers and callbacks must finish before successful session destruction.
Other borrowed pointers cannot escape their calls, and no unload-sensitive TLS
may outlive the owning threads. Safe author-facing methods do not enforce those
restrictions on arbitrary Rust code. A reported destructor failure leaves code
(and any potentially live callback storage) retained, blocks reactivation of that
ID, and requires restart. A hung or aborting native plugin cannot be recovered by
this loader;
logical deactivation and actual OS unmapping are distinct. The editor/SDK/loader
use `deny(unsafe_code)` with narrowly allowed FFI modules and generated exports;
the rest of the workspace continues to forbid unsafe code.

Exercise actual native libraries without starting the UI:

```sh
cargo xtask test-plugins
```

This builds the Markdown plugin, verifies that restaging does not replace an
active instance, explicitly unloads/loads a restaged artifact, and checks repeated
lifecycle cleanup, host-owned data, missing/corrupt libraries, digest mismatches,
and malformed manifests. It additionally tests native styled Markdown output,
async requests, fallback/reactivation, and result ownership after unload. Worker
tests cover queued and active cancellation and unload ordering. The LSP DSO smoke
uses a deterministic Python peer to check async diagnostics/hover, cancellation,
saves, unload during initialization and held requests, process reaping, latest
snapshot reattachment, resident image reuse, and independent Markdown operation.
It also checks definition unload/reload, reclaimable Rust plugin copies, missing
transport/profile behavior, and absence of an implicit server fallback.
Build commands
target the local native platform; cross-compilation and distribution are not part
of this tooling yet.

## Workspace

- `crates/editor`: the `velk-editor` package and `velk` application binary.
- `crates/editor-widget`: reusable RXUI editor view and UI-independent rope buffer.
- `crates/syntax`: in-process Tree-sitter parsing, Rust grammar, and background worker.
- `crates/semantic`: transport-independent semantic models, queues, and backend lifecycle.
- `crates/markup`: UI-independent styled documents and host-facing async renderer contract.
- `crates/plugin-api`, `crates/plugin-sdk`, `crates/plugin-host`: experimental native ABI, author SDK, and lifecycle worker.
- `plugins/markdown`: native CommonMark provider; owns the `pulldown-cmark` dependency.
- `plugins/lsp`: generic native LSP transport; owns protocol types, JSON-RPC, URIs, and language-server processes.
- `plugins/rust-analyzer`: namespaced Rust language-server definition; depends on the LSP plugin at runtime.
- `xtask`: local editor/plugin build and staging commands.
- `Cargo.toml`: shared package settings, dependencies, and lints for future crates.

RXUI is pinned to commit `96e1f4a833785edbce5885945a8bf8ba0a9e75f7`
with its `native` feature enabled. Keep `Cargo.lock` committed to preserve the
resolved dependency graph. To update RXUI, change the workspace dependency's
`rev`, refresh the lockfile, and run the workspace checks.

RXUI pins both Astrelis crates to commit
`4801b66d77d00726c12b318a709544488970c7c5`, which includes the editor text and
native profiling improvements. All three crates resolve from Git revisions;
no sibling RXUI or Astrelis checkout or local Cargo patch is required. The
[Astrelis performance report](docs/performance/astrelis-local.md) records the
earlier experiments and their measurement revisions.

## Saving

Saving clones the Rope and checkpoint on the UI thread, then writes, hashes, and
synchronizes on RXUI's blocking pool. Only one save can run at a time; if you edit
during a save, save again afterward to persist the newer edits. Failures appear
in the status line and retain the dirty state and undo history.

The application uses `tempfile` for a sibling temporary file and atomic
replacement, and BLAKE3 to compare the original bytes before replacement. A file
changed or deleted externally is preserved and saving reports an error; reload or
conflict resolution is not implemented yet. The comparison is best effort, not
an operating-system compare-and-swap: another writer can race the final rename.

The dominant LF, CRLF, or CR line-ending style is detected during loading and
restored on save. Ties prefer LF, then CRLF, then CR; files without newlines use
LF. Mixed endings normalize to the chosen style on an edited save. Trailing
newlines, Unicode text, and any UTF-8 BOM are retained. Saving an opened symlink
updates its resolved target and preserves the link. File permissions are copied;
read-only files are refused. Atomic replacement creates a new file identity, so
hard-link relationships, extended attributes, and ACLs are not preserved by this
initial implementation. Save As and automatic external-file reload are deferred.

## Transactions

`Buffer::apply_transaction` and `Editor::apply_transaction` apply validated,
non-overlapping replacements atomically as one undo step. Existing input uses
this mechanism without changing indentation, Tab, or pairing behavior. Stale
versions and invalid edits leave document state untouched; per-replacement
coordinates continue to support incremental parsing through undo/redo.
See [edit transactions and saved checkpoints](docs/edit-transactions.md) for the
API, selection mapping, revision rules, and dirty-state semantics.

## Rust syntax highlighting

Files with a `.rs` extension enable Tree-sitter automatically. The official Rust
bindings (`tree-sitter` 0.27.1) and Rust grammar (`tree-sitter-rust` 0.24.2) are
linked into Velk. Tree-sitter does not spawn a parser process. Rust files also start the
separate semantic service described below. Runtime loading of external grammar libraries is not yet
implemented; additional languages can use the same in-process service.

Try the app on its own source:

```sh
cargo run --release --locked -- crates/editor/src/main.rs
```

One persistent worker per language-enabled editor compiles the grammar/query,
parses borrowed Rope chunks, queries the requested viewport, and builds a capture
index. Edits, undo, and redo provide Tree-sitter byte/point coordinates. Document
and viewport requests each keep only their latest pending value; obsolete parsing
and querying are cancelled cooperatively. If the worker falls behind the bounded
edit log, it performs a fresh parse.

The UI requests visible lines and horizontal windows without waiting. It reads
immutable, indexed captures and projects their colors into cached line layouts;
no Tree-sitter parser, query, or tree is used or destroyed on the UI thread. While
waiting, existing captures are mapped around edits and newly inserted or newly
exposed text stays plain until the result arrives. Multiline syntax changes
become accurate when that result arrives. Explicit annotations override syntax
colors, and syntax does not add font metric changes. IME preedit projects colors
around the temporary replacement and requests only represented source ranges,
excluding uncommitted text and the replaced selection.

The pinned RXUI host already supplies shared pools with two async and two blocking
workers. Async result delivery reuses that executor; the persistent syntax CPU
worker is separate. Syntax delivery requires no second runtime. The LSP plugin owns its separate Tokio reactor for asynchronous language-server I/O. Input handling, immediate Rope edits, viewport layout and GPU work
remain UI responsibilities. File loading currently finishes before the native
event loop starts; an in-window asynchronous loading flow is not implemented.
The syntax tree remains on the worker for future structural features; resolved
types and other semantic information come from the separate semantic provider.

Parser benchmarks: `cargo bench -p velk-syntax --bench syntax --locked`.
See [initial syntax measurements](docs/performance/syntax.md) and the
[async syntax audit](docs/performance/async-syntax.md) for results and limits.

## Semantic services and Rust LSP

Rust files start a persistent rust-analyzer connection through the new
`velk-semantic` service. The UI submits Rope snapshots; the service handles
initialization, position encoding, document synchronization, diagnostics, hover,
and shutdown on its own worker. The public semantic interface uses document
revisions and scalar offsets, so another backend can provide the same results
without LSP.

The pinned development toolchain includes `rust-analyzer` and `rust-src`.
For a packaged/local executable, ensure `rust-analyzer` is on PATH, or set
`VELK_RUST_ANALYZER` to its absolute path. A missing or failed server appears in
the status line; plain editing and syntax highlighting keep working.

Diagnostics appear as colored wavy underlines with message cards. Rest the pointer
on a symbol for 350 ms to request semantic hover, or press **F1** at the caret.
The popup combines diagnostic messages and symbol documentation; its body supports
text selection and independent scrolling. Markdown documentation renders headings,
emphasis, code, lists, quotes, and styled links using native rich text. Parsing runs
on a background worker through the replaceable `velk-markup::Renderer` interface.
Moving within the same identifier keeps
the result, and a short leave grace lets you enter the popup. Editing, navigating,
or scrolling the source dismisses it and cancels obsolete requests.
The status line shows availability
and diagnostic count. Compiler checking on save is initially disabled; native
rust-analyzer diagnostics still work, but this does not replace `cargo check`.
Cargo/project/build-script/proc-macro helpers may still run. Python 3 is needed
only for scripted LSP integration tests, not to run Velk.

This first service manages one open document. Completion, navigation, formatting,
inlay hints, code actions, and automatic restart are deferred. See
[semantic services](docs/semantic-services.md) for the backend contract, queue and
revision policy, supported LSP behavior, and real-server validation.

## Editor widget

`velk-editor-widget` uses Ropey 1.6.1 for document storage and bounded undo/redo
snapshots, unicode-segmentation 1.13.3 for grapheme navigation, and RXUI's
Astrelis text engine for shaping, hit testing, and selection/caret geometry.
The widget owns its text rather than mirroring a String into a built-in text area.

Create an `Editor` entity with `Editor::default()` for an empty document or
`Editor::new(text)` for initial content, and use it as an RXUI window root or
child view. Create a separate editor entity per window. `Editor::buffer()` gives
read-only access to the rope and selection; `Editor::insert(text)` replaces the
selection programmatically. The exported `Buffer` can also be used independently
of the UI. Its selection positions are Ropey character offsets, snapped to
extended grapheme boundaries; its line endings are normalized to LF.

Click to focus and place the caret; drag or Shift-click to select. Supported keys:

- Arrow keys move by grapheme or logical line; Shift extends the selection.
- Home/End move to line boundaries; Command/Control+Home/End move to document boundaries.
- On macOS, Command+Left/Right move to line boundaries and Command+Up/Down to document boundaries.
- Enter inserts a newline; Tab inserts four spaces. Shift+Tab allows focus traversal.
- Backspace/Delete remove the selection or an adjacent grapheme.
- Command/Control+A, C, X, V select all, copy, cut, and paste.
- Command/Control+Z undoes; Command/Control+Shift+Z or Control+Y redoes.

The view supports native IME preedit/commit/cancellation, a blinking caret,
horizontal and vertical wheel scrolling, and scrolling to reveal the caret.
Undo history retains up to 128 transactions; each native text insertion, paste,
or deletion currently creates one transaction. There is no typing coalescence yet.

Text shaping and GPU text resources are cached per logical line. The renderer
prepares the viewport plus a small overscan region, retaining unaffected layouts
on ordinary edits. IME composition projects syntax and annotations into a temporary Rope without
changing the source document. Soft wrapping, word motions, and multiple cursors remain future work.
The application supplies asynchronous file persistence around the widget.
Vertical navigation preserves a grapheme column rather than a pixel column.
RXUI custom text clients currently expose the editor's accessibility role and
label, but do not publish document text or selection to assistive technology.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org/), for example
`feat(editor): open a blank RXUI window` or
`chore(deps): update the pinned RXUI revision`.

## Rich text playground

Run the separate development example (not part of the normal app binary):

```sh
cargo run --locked -p velk-editor --example widget-preview --features performance -- --showcase
```

Its preview-specific tests run with
`cargo test -p velk-editor --example widget-preview --features performance --locked`.

It demonstrates foreground and range backgrounds, a full-line debugger
background, bold/italic and mixed font sizes, solid/dashed/wavy underlines,
overlines, strikethroughs, delayed hover cards, and document-anchored inlay hints,
diagnostic badges, debugger values, and ghost text. The toolbar switches editor
appearance and toggles decorations/overlays. In this example, LSP and debugger
content is mocked; no language server or debugger is connected.

Applications can call `Editor::set_annotations(Annotations { ... })` to apply a
snapshot for `editor.buffer().revision()`. `StyledRange` ranges and `TextOverlay`
anchors use Ropey character offsets on grapheme boundaries, including for
Unicode text. Later foreground/font attributes override earlier overlapping
spans. Backgrounds and custom lines paint in span order. Colors use Astrelis's
linear straight RGBA space. Overlays are visual content; they never enter the
rope, selection text, clipboard, or undo history.

Snapshots are rejected if their document revision is stale. Edits, undo, and redo
clear annotations; navigation and appearance changes preserve them. In the
prototype, use **Refresh annotations** after editing, or **Reset sample** to
restore the showcase. External diagnostic/LSP producers should recompute and
submit an updated snapshot after document changes. Built-in syntax uses its own
background capture pipeline. `Editor::set_text` replaces the
whole document as one undoable edit.

`OverlayPlacement::LineEnd` stacks labels after the source line. `AtAnchor`
paints at an existing caret position without reserving document layout space;
use it for ghost completions at line ends or empty positions. `Inline` reserves
shaped space at an anchor and moves the following source text. The playground
starts with inline type hints beside variables, a parameter-name hint before an
argument, and a debugger value beside a literal. **Inlays: inline / line end**
switches those hints between the two placements.

Inline labels live only in a projected display string. Carets retain source
character offsets and sit after hints at their anchor; clicking a label maps to
that anchor. Selection highlighting, source styles, copied text, and edits skip
the virtual label. This projection composes with IME replacement and supports
multiple labels at one anchor. Labels are currently single-line text; arbitrary
embedded widgets, soft-wrapped inlays, interactive diagnostic actions, automatic
annotation remapping, and a rich-text file format are future work.

Run the benchmarks and the separate native stress fixtures:

```sh
cargo bench -p velk-editor-widget --bench editing --locked
cargo bench -p velk-editor-widget --bench viewport --features performance --locked
cargo bench -p velk-editor-widget --bench responsiveness --features performance --locked
cargo run --release --locked -p velk-editor --example widget-preview --features performance -- --stress
cargo run --release --locked -p velk-editor --example widget-preview --features performance -- --long-lines
cargo run --release --locked -p velk-editor --example widget-preview --features performance -- --rich-long-lines
cargo run --release --locked -p velk-editor --example widget-preview --features performance -- --unicode-long-lines
```

In a development build with `--features performance`, set `VELK_PROFILE=1` to
log preparation counters/timings and draw-recording time. That feature also exposes
`Editor::frame_metrics()`; normal app builds omit this instrumentation. Our target is a 120 Hz
frame budget (8.33 ms); these measurements do not establish presentation cadence.

See [the performance report](docs/performance/editor-prototype.md) for measured
results, cache behavior, scope, and next optimizations.

Long printable-ASCII lines now prepare horizontal windows plus overscan. The rich
path supports verified monospace metric runs, bold/italic/mixed sizes, backgrounds,
and printable-ASCII inline labels. Lines outside these fast paths keep the full-line
renderer; the Unicode extension is described below. See the [rich-window and native timing report](docs/performance/rich-horizontal-and-scheduling.md)
for current eligibility, results, and remaining scheduling limits; the
[fixed-grid report](docs/performance/horizontal-virtualization.md) records the first pass.
Set `ASTRELIS_PROFILE=1` to add native CPU frame-stage timings to `VELK_PROFILE=1`.

Custom IME geometry is now published after preparation in the same frame, so
navigation, typing and composition no longer request a second submission just
to update the candidate rectangle. IME uses a transient Rope projection, so
replacements spanning source lines also use the viewport cache. A bounded LTR
Unicode/preedit region with retained shaping context can be windowed inside a
verified monospace ASCII line. RTL, large/dispersed Unicode regions, and rich
metric styles keep full-line shaping. See [Unicode/composition windows](docs/performance/composition-windows.md)
for current eligibility, first-preedit timings, reference checks and remaining limits.
The [native redraw experiment](docs/performance/native-input-redraw.md) compares
rendering after input callbacks with the normal scheduler. It retains the normal
policy after finding burst acquisition pressure, and adds the
`long_line_unicode_typing` benchmark for remaining application-side input cost.
The [grapheme-query follow-up](docs/performance/grapheme-cursors.md) replaces
whole-line point-query segmentation with chunk-aware cursors and records paired
offscreen/native edit latency improvements. Up/Down retains its line-column cache.
See [the responsiveness report](docs/performance/responsiveness.md) for controlled
submission timings, native event latency, limits and reproduction commands.
Set `VELK_BENCH_GPU_WAIT=1` on the responsiveness benchmark to additionally wait
for offscreen GPU completion; this does not measure display presentation.
