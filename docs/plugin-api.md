# Built-in feature services

Velk's editor features are compiled into the application. There is no dynamic
plugin loading, external plugin directory, artifact manifest, or Wasm runtime.
The former architecture is preserved at the `plugin-architecture-checkpoint` Git
tag. The current priority is a coherent editor for Vim users.

## Internal organization

The `plugins/` source directories and `plugin-*` crate names remain internal
implementation details. `plugins/builtin` registers the linked features, and
`crates/plugin-host` schedules their work and coordinates shutdown. Existing
registration adapters are internal to one build; they are not a supported native
ABI or an extension SDK. The editor executable includes its features directly.

Built-ins include Vim input, indentation, autopairs, syntax highlighting,
completion and snippets, file browsing, quick open, workspace search, diagnostics,
Markdown documentation, autosave, terminal integration, and undo history.
The undo-tree panel has been removed; undo/redo, branching history, recovery, and
optional disk history remain.

Settings configure behavior rather than installing or loading code. All built-ins
start with the editor; optional behavior, such as autosave, can retain its own
setting. Vim is the default keymap; Standard editing remains selectable in
Settings > Features. Feature settings currently retain namespaced internal IDs.
There is no plugin manager, dependency browser, or interface-selection page.

## Ownership and execution

The document model owns text, identity, revisions, and validated transactions.
Feature requests propose work through existing document services. Background
workers own blocking I/O and computation; the shell owns focus and presentation.
The service worker still coordinates settings application and joins feature
workers before application teardown, including language-server shutdown.

Registration and request adapters currently retain the former function-table and
bounded-message machinery. They are private implementation debt, not a reason to
preserve binary compatibility. Prefer ordinary Rust interfaces when changing this
code; remove adapters when a concrete feature change makes that useful.

## Verification

Run `cargo xtask test-headless` for model, widget, and application tests, and
`cargo xtask test-features` for linked feature integration checks. A plain
`cargo build -p velk-editor` includes all built-ins when the terminal build inputs
are configured. `cargo xtask run --release -- path/to/file.rs` configures those
inputs automatically and launches the editor without staging plugins.
