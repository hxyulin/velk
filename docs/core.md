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

`velk-editor-widget` owns the RXUI view, native input and IME adaptation, viewport
and text rendering, and presentation of annotations and syntax. It consumes Core
and re-exports the existing buffer types for callers embedding the widget.

`velk-editor` still coordinates document lifecycle, file I/O, recovery, workspace
state, settings, and the shell. Those responsibilities have not moved into a
loadable Core module yet.

Core emits parser-independent changes with document revisions, Unicode scalar
ranges, and UTF-8 byte/line coordinates. `velk-syntax` translates these changes to
Tree-sitter input edits. Syntax and language-service snapshots still use concrete
Ropey types and need further separation before an engine can be loaded independently.

Buffer snapshot encoding remains version 1 so existing recovery and reload data
continue to work. That process format is not a promise of compatibility between
arbitrary engine implementations.

Run Core's editing, Unicode, transaction, and history tests without building the
native frontend or terminal:

```sh
cargo test -p velk-core --locked
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

The intended plugin architecture supports trusted native modules and sandboxed
Wasm modules through the same service semantics, with separate transport bindings.
Rust is the primary SDK language; Rust trait objects, futures, closures, RXUI
objects, and Ropey internals must not become the independently compiled ABI.
Native plugins remain trusted in-process code. Current capability declarations
are descriptive and do not enforce access restrictions.

## Migration order

1. Extract and test the linked, UI-independent document implementation. This is
   the current step; it does not register Core as an optional feature plugin.
2. Replace concrete storage types at service boundaries with document/snapshot
   handles, bounded reads, revisioned transactions, and explicit lifetimes. Define
   the engine/frontend input and viewport contract, including IME and accessibility.
3. Move application-level engine responsibilities behind those contracts and add
   a versioned native engine entry point. Select an engine at startup, with a
   bundled default. Engine replacement during a live session is deferred.
4. Validate Wasm bindings and resource grants against real workloads before
   deciding which default implementations run in Wasm. Wasm is not required for
   the initial Core extraction or native module implementation.

Module packaging and logical service boundaries are separate choices. Avoid an
independent provider or dynamic library for every internal helper; expose a new
replacement point when a concrete implementation needs it. Maintain behavioral
conformance tests so replacing a provider preserves plugin expectations.
