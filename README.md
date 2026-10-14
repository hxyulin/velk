# Velk

A desktop editor built with Rust and [RXUI](https://github.com/hxyulin/rxui).
The application opens a native window with an empty rope-backed text editor.

## Development

Install [rustup](https://rustup.rs/). The repository pins Rust 1.99.0 in
`rust-toolchain.toml`; rustup installs the toolchain when needed.

Run the editor:

```sh
cargo run --locked
```

The binary is named `velk`. It can also be run explicitly with
`cargo run --locked -p velk-editor --bin velk`.

Check the workspace:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

## Workspace

- `crates/editor`: the `velk-editor` package and `velk` application binary.
- `crates/editor-widget`: reusable RXUI editor view and UI-independent rope buffer.
- `Cargo.toml`: shared package settings, dependencies, and lints for future crates.

RXUI is pinned to commit `093d05d9a2fa5491c3cebe26be46b8720b4c6158`
with its `native` feature enabled. Keep `Cargo.lock` committed to preserve the
resolved dependency graph. To update RXUI, change the workspace dependency's
`rev`, refresh the lockfile, and run the workspace checks.

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
Undo history retains up to 128 edits; each native text insertion, paste, or
deletion is one edit. There is no typing coalescence yet.

Text shaping is cached between document edits, but this first version shapes the
whole document. It does not yet provide viewport-only line rendering, soft
wrapping, word motions, multiple cursors, syntax highlighting, or file persistence.
Vertical navigation preserves a grapheme column rather than a pixel column.
RXUI custom text clients currently expose the editor's accessibility role and
label, but do not publish document text or selection to assistive technology.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org/), for example
`feat(editor): open a blank RXUI window` or
`chore(deps): update the pinned RXUI revision`.
