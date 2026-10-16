# Core and component boundaries

Velk is moving toward a thin native RXUI frontend and a replaceable editor engine.
Core means required services with replaceable implementations. A required service
does not need to live permanently in the application executable.

## Current implementation

`velk-core` is a linked Rust crate with no RXUI, plugin-host, or Tree-sitter
dependency. It owns buffer text, selection and grapheme navigation, revisioned
transactions, undo/redo, saved checkpoints, change tracking, and session snapshots.
Ropey remains the storage implementation, and Rust callers still have direct
access to it. This is an extraction of the default implementation, not a stable
engine API or a dynamically replaceable storage provider.

`velk-editor-model` owns renderer-independent annotation styles, overlays, hover
request tokens, validation, and source/display coordinate projections for inline
hints and IME replacements. It depends only on Core. Semantic presentation code
can build annotations without depending on RXUI, Astrelis, or the widget crate.

`velk-editor-widget` owns the RXUI view, native input and IME adaptation, viewport
and text rendering, and presentation of annotations and syntax. It translates
model styles into Astrelis text spans. It consumes Core and the presentation
model, re-exporting their existing types for callers embedding the widget.

`velk-editor` still coordinates document lifecycle, file I/O, recovery, workspace
state, settings, and the shell. Those responsibilities have not moved into a
loadable Core module yet.

Core emits parser-independent changes with document revisions, Unicode scalar
ranges, and UTF-8 byte/line coordinates. `velk-syntax` translates these changes to
Tree-sitter input edits. Rust crates intentionally retain direct Ropey access for
cheap snapshots, borrowed chunks, and coordinate conversion. An abstract storage
interface is deferred until a concrete replacement or binary boundary requires it.

Buffer snapshot encoding remains version 1 so existing recovery and reload data
continue to work. That process format is not a promise of compatibility between
arbitrary engine implementations.

Run Core's editing, Unicode, transaction, and history tests without building the
native frontend or terminal:

```sh
cargo test -p velk-core --locked
cargo test -p velk-editor-model --locked
```

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
