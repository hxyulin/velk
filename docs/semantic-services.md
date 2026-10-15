# Semantic services

Velk's semantic boundary is `velk-semantic`, independent of RXUI, widgets,
Tree-sitter, and LSP wire types. LSP implementation and dependencies live in the
loadable `plugins/lsp` library. Rust policy lives separately in
`plugins/rust-analyzer` (ID `velk.language-server.rust-analyzer`), which supplies a
typed definition naming its `velk.lsp` dependency. The editor routes Rust documents
to a neutral plugin-host endpoint. There is no built-in LSP or Rust-analyzer fallback.

## Public boundary

- `DocumentSnapshot`: path, language identifier, monotonic buffer revision, and an
  immutable Rope clone. No full-document String is created on the UI thread.
- `Backend`: an implementation consumes a `Session` and returns a background
  future. It can own an in-process engine, an LSP connection, or another protocol.
  `Service::start_boxed` accepts a dynamically selected backend.
- `Service`: `external` creates queue-only frontends for the plugin host without
  another thread/reactor. `start_boxed`/`start_observed` execute a local backend on
  one named worker with a current-thread Tokio reactor. `shutdown_and_join` is the
  worker-only quiescence barrier. `update`, `hover`, `cancel_hover`, `saved`, and
  ordinary `shutdown` never wait for results.
- `Event`: status/capabilities, versioned diagnostics, hover results, and request
  failures. Ranges are Unicode scalar offsets. Hover results retain request
  identity, document revision, and requested position.

The first implementation owns **one open document per service**. The app still
opens one file. Sharing a workspace server across multiple open documents needs
keyed document queues and lifecycle management before adding multiple tabs.
Document submissions should come from the single document owner in revision
order. An `Events` clone shares the result stream; it is not a broadcast
subscription for independent consumers.

Document, hover, and save input queues each retain one pending value. Status,
diagnostics, and hover output have separate latest-value slots, so a diagnostic
flood cannot displace the status or hover slot. Independent backend tasks must
coordinate result freshness before publishing. The UI performs another revision
and request-identity check before presentation. An in-process test backend
exercises the same API with no LSP types.

Source editing and immediate Tree-sitter highlighting never wait for semantics.
The RXUI executor delivers semantic events to the document entity. The host's
existing plugin worker drains coalesced snapshots after each command/wake; no
polling timer or second host semantic reactor is required. Native session input
copies typed borrowed data into plugin-owned queues, and the plugin reactor owns
protocol I/O. No future, Rust container, trait object, or shared allocator crosses
the C ABI. Unload cancels/joins session reactors before dropping host callback
storage or the plugin instance. Native results are copied before callbacks return.

The frontend endpoint survives provider unload. Status becomes stopped and the
UI clears semantic decorations; source edits still update the cached latest
snapshot. Reload attaches a new session and replays that snapshot. Pending old
results are cleared at generation changes. Markdown is an independent capability:
unloading LSP leaves its renderer available, and unloading Markdown leaves LSP
working with literal Markdown presentation.

Language definitions are synchronous configuration queries with typed borrowed
records. The host copies executable/argument/root/settings strings, resolves the
exact transport ID, then supplies another borrowed record when creating a session.
The receiving SDK copies it again into transport-owned data. No plugin links the
other implementation or retains its pointers. Unload of a definition or transport
joins its dependent sessions; the definition's library can then be released.
Missing transport dependencies and conflicting definitions fail visibly without
starting a server. Relevant plugin generations and document language form the
resolution key, so ordinary edits do not repeat configuration queries.

Plugin IDs follow `author.[namespace.]name` with arbitrarily nested namespaces
within 63 ASCII bytes. Segments start with a lowercase letter and otherwise allow
lowercase letters, digits, hyphens, and underscores. SDK exports and manifests use
the same parser. IDs resolve exactly; namespace prefixes have no implicit behavior.

Tokio's process-global Unix signal hooks require `velk.lsp` to request resident
code. Successful logical unload reaps servers and joins/destroys sessions and
instances, but retains one code image/private copy per digest for process life;
reloading the same build reuses it. The modal labels this explicitly. Different
builds retain different images until exit. Failed teardown instead quarantines
code and callback storage, blocks reactivation, and requires restart. Arbitrary
plugin hangs/aborts remain outside the cooperative loader's recovery guarantees.

## LSP adapter

The adapter uses `lsp-types` for typed protocol payloads, `serde_json` for JSON-RPC,
`url` for file URIs, and Tokio for pipes, timers, and child lifecycle. The small
Content-Length framing reader bounds headers and bodies and handles fragmented
input. Incoming frames also have a bounded queue. It supports:

- Initialization, capabilities, and UTF-8/UTF-16/UTF-32 position negotiation; UTF-16
  is the protocol default. Mid-codepoint and mid-surrogate incoming offsets are
  rejected. Scalar offsets are converted at the adapter boundary.
- `didOpen`, negotiated incremental/full `didChange`, supported `didSave`, and
  `didClose`. Language servers without document-change support fail visibly on
  an edit, rather than silently using stale text.
- Background comparison of the last sent and newest Rope snapshots. Equal chunks
  can be skipped, including pointer-identical chunks. One bounding replacement
  represents a coalesced update, even when intermediate revisions were skipped.
  Unchanged content at a newer revision advances the server version with an empty
  incremental edit. Multiple separated changes may include intervening unchanged
  text in their bounding replacement; primitive transaction streaming is deferred.
- Versioned push diagnostics for the current document; wrong URI/version results
  are discarded. **Unversioned diagnostics are accepted only before the first
  document change.** This conservative policy avoids assigning an old result to
  new text. Servers that never version push diagnostics need a further policy or
  pull-diagnostics support before they can provide diagnostics throughout editing.
- One pending hover request, supersession/cancellation, a five-second deadline,
  and rejection of late responses to cancelled requests. Results preserve plain
  text versus Markdown information; the UI delegates Markdown to the loaded renderer.
- Workspace configuration lookup, workspace-folder responses, and progress-token
  acknowledgment. Workspace edits are explicitly refused until the editor has
  the complete multi-file application flow. Unsupported requests receive a
  MethodNotFound response; unsupported features are not advertised.
- A 30-second initialization deadline. Shutdown cancels outstanding work, sends
  close/shutdown/exit, and waits on the worker, with bounded forced termination
  and child reaping if needed. Dropping a service never joins its worker on the
  UI thread. Process kill-on-drop covers unwinding; backend panics become failure
  status. No automatic restart loop is introduced.

Every effective synchronization gets a separate LSP version mapped to the current
buffer revision. The adapter does not cast buffer revisions to LSP integers or
confuse their units with character positions. Request IDs used with rust-analyzer
stay within its supported integer range, including shutdown.

## Rust and presentation

Opening `.rs` creates an endpoint which resolves the Rust support plugin and its
LSP transport dependency. The Rust definition selects persistent rust-analyzer;
the generic transport has no language-specific default. The transport discovers the
nearest ancestor Cargo manifest on the worker; an explicit `LspBackend::root`
can override it. The Rust definition defaults to `rust-analyzer` on PATH and can be
replaced with `VELK_RUST_ANALYZER=/absolute/path/to/rust-analyzer`. Initialization
options and workspace configuration are separate configurable JSON objects in
the LSP adapter. `VELK_LSP_CONFIG=/path/to/config.json` can set `executable`,
`arguments`, `root`, `initialization_options`, and `configuration` privately in the
Rust definition plugin; the file and copied definition are limited to 1 MiB. The editor has no user-configurable language
routing yet, and one session still owns one document/server.

`checkOnSave` is initially disabled. Native analyzer diagnostics still arrive,
but compiler/check diagnostics are not equivalent to running `cargo check`.
Cargo project discovery, build scripts, and procedural macro support may still
start helper processes. Opening a file does not spawn a process for every hover
or edit; the server and its stdio connection persist.

The app status line shows startup, availability, diagnostic count, and hover
request failures. Ready means the protocol connection is initialized; Cargo
project loading and analysis can continue in the background. Diagnostic ranges expand to grapheme boundaries and receive
severity-colored wavy underlines with message cards; empty ranges get line-end
badges. Diagnostics do not replace syntax foreground colors. The current app
owns the explicit annotation provider; multi-provider semantic/debug annotation
composition is future work.

Pointer hover waits 350 ms before requesting symbol documentation; F1 requests it
immediately at the caret. The widget owns `HoverTarget` tokens (generation,
revision, scalar position, and pointer/caret source), and the host maps these to
semantic request IDs. Results must match the current target and viewport geometry.
Retargeting or dismissal invalidates pending results immediately and cancels the
provider request. Empty results are cached for the current target; moving within
the same Unicode identifier does not repeat requests or restart dwell.

Hit testing uses source grapheme cells rather than nearest caret placement,
including horizontally virtualized lines and mixed bidirectional text. Blank
space, line padding, and virtual inline hints do not request source documentation.
Diagnostic and semantic content share one bounded popup with selectable text
and an independent scroll area. A 180 ms leave grace permits entering the popup;
it stays while the pointer is inside or selecting text. Source edits, navigation,
scrolling, IME activity, Escape, and outside clicks dismiss it. Hover does not move
the source caret. No per-hover process or UI-thread parsing is introduced.

Markdown hover responses are preferred over plaintext during LSP negotiation.
The native `velk.markdown` plugin uses
[pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) to interpret
Markdown into owned styled runs. `velk-markup` contains only the model and
host-facing asynchronous interface; neither the editor nor the widget links the
parser. Headings, bold/italic emphasis,
inline/fenced code, strikethrough, ordered/unordered/task lists, quotes, rules, and
basic flattened tables are supported. The widget maps these runs to one selectable
RXUI rich-text element, preserving selection across paragraphs and independent
popup scrolling. Code uses a monospace face; code syntax highlighting and separate
code-block backgrounds are deferred. Links receive accent styling and underlines,
with destinations preserved as data; activation is deferred. Images retain alt
text without loading resources, and raw HTML is displayed literally. Plaintext
responses and diagnostic text are never parsed as Markdown.

Interpretation is submitted asynchronously through `velk_plugin_host::Client`.
A bounded host queue sends work to the plugin worker, which calls the optional
Markdown C ABI capability synchronously. Input is borrowed for the call, and a
single callback batch copies text/style/link data into host-owned runs before
returning. Dropping the future closes its result channel; the worker skips
cancelled queued requests. Active parsing completes normally and never outlives
its loaded library: unload is another operation on the same serial worker.
The UI awaits completion through `try_spawn`, maps cached runs to native spans,
and still rejects stale target/revision/geometry results. There is no additional
blocking-pool job waiting on the plugin worker.

The host can inject an `Arc<dyn velk_markup::Renderer>` with
`enable_semantics_with_renderer`. Its Rust future is entirely host-side and never
crosses the C ABI. With no provider loaded, requests return literal Markdown as
plain text; provider/queue errors also use a literal fallback. Loading a provider
restores formatting for subsequent requests. The parser is not retained as a
built-in fallback. See the README's local-plugin workflow for load/unload commands,
ABI v1, limits, and the cooperative native-code lifecycle contract.

Completion, navigation, formatting, inlay hints,
code actions, pull diagnostics, file-watch registration, and automatic server
restart are deferred. Input modifiers, indentation, and pairing behavior are
unchanged.

## Validation

```sh
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test -p velk-semantic --locked
cargo xtask test-plugins
```

The actual native-library smoke additionally verifies unload during a held hover
and stalled initialization, server process reaping before the unload reply, host
results surviving unload, latest-document reattachment, resident image reuse,
independent Markdown rendering, Rust definition unload/reload, no implicit server
without a definition, and missing transport errors. Unit tests check typed ABI record validation,
namespace parsing, exact dependency lookup, conflicting definitions,
Unicode/non-UTF-8 Unix paths, joining final callbacks, and failed-session teardown.

Scripted LSP tests use `python3` and only its standard library. They check UTF-16
incremental updates and coalescing, configuration requests, refused workspace
edits, stale diagnostic versions, late hover responses, deadlines, saved snapshot
text, graceful shutdown, and cancellation of a stuck initializer. Unit tests cover
all supported position encodings, chunk/Unicode diff reconstruction, framing,
neutral backend delivery/lifetime, grapheme decorations, stale presentation, and
both portable/native F1 spellings. Widget tests cover dwell, retargeting, empty and
stale results, popup entry/leave, independent scrolling, diagnostic refresh, and
shaped grapheme hit testing across Unicode and bidirectional runs.

A separate optional test requires rust-analyzer and rust-src:

```sh
cargo test -p velk-plugin-lsp --test lsp --locked \
  real_rust_analyzer_hover_uses_unsaved_document -- --ignored --nocapture
```

This was run against rust-analyzer 1.99.0. It changed an in-memory declaration from
`usize` to `i64`, received the new type through hover, verified graceful shutdown,
and left the file on disk unchanged. Native smoke testing also verified a live
syntax diagnostic, its removal after undo, and pointer/caret hovers showing the
variable type. Pointer hover was also checked for entering the popup, selecting
its text, leaving, and reopening it. A native Markdown fixture verified headings,
emphasis, strikethrough, code, lists, styled links, and text selection. Markdown
rendering also has a GPU selection regression test and a host test for asynchronous execution, renderer injection, and cancellation. Actionable links remain deferred.
No native end-to-end latency measurement is claimed for this integration.
