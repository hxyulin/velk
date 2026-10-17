# Core and component boundaries

Velk is moving toward a thin native RXUI frontend and a replaceable editor engine.
Core means required services with replaceable implementations. A required service
does not need to live permanently in the application executable.

## Current implementation

`velk-core` is a linked Rust crate with no RXUI, plugin-host, or Tree-sitter
dependency. It owns shared document text, per-view selection and grapheme navigation, revisioned
transactions, validated undo/redo application, saved checkpoints, change tracking,
and session snapshots. It depends on `velk-core-contracts` for document coordinates,
Unicode helpers, and the undo service contract; it has no normal dependency on
the bundled history implementation.
Its `lifecycle` module owns close/prompt decisions and Save All sequencing,
returning actions for the frontend to execute against stable document IDs.
Ropey remains the storage implementation, and Rust callers still have direct
access to it. This is an extraction of the default implementation, not a stable
engine API or a dynamically replaceable storage provider.

`velk-editor-model` owns editing commands and default key mapping, along with
renderer-independent annotation styles, overlays, hover
request tokens, validation, and source/display coordinate projections for inline
hints and IME replacements. It depends only on Core. Semantic presentation code
can build annotations without depending on RXUI, Astrelis, or the widget crate.

`velk-editor-widget` owns the RXUI view, native input and IME adaptation, viewport
and text rendering, and presentation of annotations and syntax. It translates
model styles into Astrelis text spans. It consumes Core and the presentation
model, re-exporting their existing types for callers embedding the widget.

`velk-editor` supplies document facts to Core's lifecycle policy and performs
native dialogs, file I/O, recovery, workspace state, settings, and shell effects.
The renderer-independent `velk_editor_model::workspace::Documents` registry owns
stable document IDs, resolved-path deduplication, tab order, active selection,
and close-neighbor policy. The native adapter owns entities, dialogs, file I/O,
and per-document tasks. The registry performs no filesystem access, so another
frontend or a headless test can supply path identities directly. Split placement
remains in the model's `views::Layout`; registry validation checks its document
references and active selection.

Core emits parser-independent changes with document revisions, Unicode scalar
ranges, and UTF-8 byte/line coordinates. `velk-syntax` supplies the neutral syntax
service and background worker. The `velk.tree-sitter` plugin translates changes
to Tree-sitter input edits; the editor does not depend on its parser or grammar. Rust crates intentionally retain direct Ropey access for
cheap snapshots, borrowed chunks, and coordinate conversion. An abstract storage
interface is deferred until a concrete replacement or binary boundary requires it.

Internal snapshot encoding remains version 1 during development. Its shape can
change without migrations; older development recovery files and undo archives
may be rejected. It is not a compatibility promise between engine implementations.

Run Core's editing, Unicode, transaction, and history tests without building the
native frontend or terminal:

```sh
cargo test -p velk-core --locked
cargo test -p velk-editor-model --locked
```

## Documents and views

`TextDocument` owns one authoritative Rope, revision, document-owned undo service
session, change journal, and saved checkpoint. `Buffer` is a view handle with an independent
selection and preferred movement column. `Buffer::new_view()` duplicates that
view's caret without copying text or history. Every edit, including undo and
redo, rebases the other views' selections through the individual changes.

`Buffer::text()` returns a cheap immutable Rope snapshot. It holds no document
lock after returning and remains valid after later edits. Callers can borrow
chunks from that snapshot without copying the full text.

The editor model owns the renderer-independent split tree and serializable view
state. Widgets own scroll, geometry, IME composition, hover targets, and editing
provider state such as Vim modes and tracked pairs. The application owns file
identity, saves, recovery, search, and one semantic service per document. A split
creates another widget over the same document; it does not reopen the file or
start another document service.

## History and position tracking

The live Rope is separate from its undo provider. The default `RamHistory` lives
in `plugins/undo/engine` (`velk-undo-tree`), not in Core. `plugins/undo` exports it
as the bundled native `velk.undo.tree` service. Its storage is a tree of reversible
patches.
Each patch retains inserted and removed text and its original change coordinates;
there is no historical Rope per entry and no whole-document diff during encoding.
Each transaction is one history node unless the caller explicitly groups input.
Editing after undo retains the alternative branch. `redo()` follows the preferred
child, `redo_branches()` lists alternatives, and `redo_branch(state)` selects one.
State IDs remain unique when grouped entries are extended, so old snapshots and
saved checkpoints cannot accidentally identify newer text.

`Buffer::new` and `Default` create provider-free headless buffers. Use
`Buffer::with_history(text, provider)` or `attach_history(provider)` to opt in.
The widget and application explicitly supply a provider. Core never creates
an undo implementation implicitly. Headless tests inject the linked tree engine.

The provider receives exact commits plus a merge hint and proposes undo/redo
steps. Core validates source state, patch text and coordinates, target selection,
and revision capacity before checkout and publication. Providers own branching,
pruning, retention, and snapshot encoding. Core owns the input-group boundary
hint, saved identities, and the actual mutation/rebasing of views.

The default RAM limits are 128 entries and 64 MiB of retained patch data and
entry overhead per document. `set_history_limits` changes these linked Rust
limits. Pruning removes the oldest inactive branch leaves before advancing the
root along the active path. A patch larger than the budget can leave its edit
without an undo entry. The limit excludes live text, allocator bookkeeping,
and immutable snapshots still held by recovery/save workers.

`BufferSnapshot` captures the current text/checkpoint and an immutable, opaque
provider snapshot tagged with its provider ID. With the tree provider, this
contains the exact undo tree and preferred branches. Immutable entries share
their data with the live tree until modified. Encoding and validation run off the UI thread. Restore checks graph
connectivity, patch contents, Unicode selections, coordinates, and limits.
`write_archive(path)` atomically replaces a disk archive;
`Buffer::read_archive_with_history(path, expected_text, provider)` rejects a stale
file baseline and requires the archive's provider.
These are blocking APIs for workers, not a disk paging layer. Disk archives
preserve the retained RAM tree; they do not extend its retention budget.
See [history.md](history.md) for application integration.

`begin_undo_group` / `end_undo_group` group compatible typing, backspace, or delete
operations from one view, with a one-second idle boundary. Caret movement, saves,
undo/redo, view changes, paste, IME commits, and separate commands end a run.
A frontend must end a run before capturing an asynchronous save checkpoint.
Headless callers that do not request grouping keep one step per transaction.

`Change::map_position` and `map_selection` define anchor mapping with explicit
before/after insertion affinity. Core views, navigation, pair tracking, visual
selections, and multiple carets use these rules. Consumers of `edits_since` must
invalidate their cached positions when it returns `None`; silently reusing an
expired position is not supported. Core view selections follow edits directly
and are independent of this bounded journal.

## Commands and lifecycle

`velk-editor-model::EditCommand` describes editing intent over a Core buffer:
insert/replace text, delete, undo/redo, selection, and cursor movement. Its `apply`
result reports acceptance, including valid no-ops at boundaries; it does not mean
the text revision changed. Invalid selection ranges leave the buffer unchanged.
`commands::key_command` maps normalized keys and modifiers with an explicit
platform argument, so Mac and other-platform bindings can be tested on any host.
Clipboard I/O, native menu shortcuts, focus traversal, and IME events stay in the
frontend. `Editor::execute` applies a command and refreshes widget presentation.
The command type is a linked Rust API, not a plugin wire format.

`velk-core::lifecycle::Lifecycle` takes current document facts, rather than UI
entities or cached dirty labels. Close actions request a prompt, save, document
removal, window close, or application exit. Prompt responses refer to the prompted
document ID. All documents remain alive until a whole-window close is approved;
cancelling a later prompt must not discard earlier documents. A successful save
of an older snapshot keeps newer edits open. Save All chooses dirty documents in
the supplied order and can stop after a cancelled destination picker or failed
write. Dialogs, async file jobs, checkpoint updates, and final recovery cleanup
remain frontend responsibilities.

## Intended boundaries

- The frontend supplies RXUI rendering and native interaction, platform services,
  and module bootstrap and compatibility checks.
- One active engine owns the authoritative document model. Storage, transactions,
  and history initially stay together in one coherent implementation.
- Feature plugins consume versioned services rather than concrete engine types.
  Settings controls, commands, and document operations share contracts.
- Required services have a default implementation distributed with Velk. Optional
  features can be disabled; required services must resolve before startup.

Separate Rust crates are the current modularity boundary. Editor behavior,
rendering, and platform hosting can have alternative implementations without
requiring a dynamic library for each one. Rendering and window/event-loop backends
also involve RXUI/Astrelis interfaces; this extraction does not make them selectable.

A later plugin architecture can support trusted native modules and sandboxed
Wasm modules through shared service semantics, with separate transport bindings.
Rust is the primary SDK language; Rust trait objects, futures, closures, RXUI
objects, and Ropey internals must not become the independently compiled ABI.
Native plugins remain trusted in-process code. Current capability declarations
are descriptive and do not enforce access restrictions.

## Migration order

1. Extract and test the linked, UI-independent document implementation. Done in
   Core; it is not registered as an optional feature plugin.
2. Separate renderer-independent presentation data and text projections. Done in
   the editor model; text shaping, geometry, and painting remain in the widget.
3. Establish editor behavior, text layout, and frontend boundaries as alternative
   implementations need them. Keep hit testing, caret placement, IME, and
   accessibility consistent with the implementation that owns layout.
4. Separate platform hosting from application behavior, using RXUI's host
   facilities where appropriate. Desktop event loops and a future browser host
   supply different implementations of native interaction and scheduling.
5. Introduce native module entry points or Wasm bindings when independent loading
   or sandboxing is needed. Validate real editing workloads and recovery before
   moving the default engine into Wasm. Live engine migration remains deferred.

Module packaging and logical service boundaries are separate choices. Avoid an
independent provider or dynamic library for every internal helper; expose a new
replacement point when a concrete implementation needs it. Maintain behavioral
conformance tests so replacing a provider preserves plugin expectations.
