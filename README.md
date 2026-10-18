# Velk

A native Rust editor for Vim users, built with [RXUI](https://github.com/hxyulin/rxui).

Velk aims for a useful out-of-box experience: modal editing, fast navigation,
completion, diagnostics, project search, and a terminal, without assembling a
plugin collection first. It is early development, with macOS as the currently
exercised desktop platform. Vim support is a useful subset, not full Vim or
Neovim compatibility.

The editor features are built into one executable. Source modules remain separate
where that makes the code easier to understand and test. Dynamic plugin loading
and Wasm experiments have been removed from the active development path; their
checkpoint is tagged [`plugin-architecture-checkpoint`](https://github.com/hxyulin/velk/tree/plugin-architecture-checkpoint).

## Previews

| Quiet | Rail |
| --- | --- |
| [![Quiet layout](docs/images/layout-quiet.jpg)](docs/images/layout-quiet.jpg) | [![Rail layout](docs/images/layout-rail.jpg)](docs/images/layout-rail.jpg) |

[![Tiled layout](docs/images/layout-tiled.jpg)](docs/images/layout-tiled.jpg)

These are earlier captures of the native editor. Layout presets remain available
in Settings; the former plugin-management screen has been removed.

## What works today

- **Native editing:** Unicode and grapheme-aware navigation, IME input,
  selection, clipboard operations, undo/redo, and virtualized text rendering.
- **Multiple views:** nested horizontal and vertical splits, independent carets
  and scrolling over shared text and undo history, with layout recovery. Split
  right with Command+\ / Control+\ or use the command palette.
- **Configurable shortcuts:** text-file keybindings, chords, Vim mode filters, and
  direct invocation of editor commands.
- **Editing assistance:** Tab / Shift+Tab indentation, aligned newlines, matching brackets, and selection wrapping.
- **Documents that survive interruptions:** untitled and file-backed buffers,
  Save As, Save All, atomic file replacement, disk-change conflict protection,
  and separate recovery snapshots with exact undo trees. Typing groups into undo
  steps with branching history; [optional disk undo](docs/history.md) retains saved history across sessions.
- **A persistent workspace:** tab order, selections, scroll positions, workspace
  roots, and recovery state. Closing the final tab leaves the window open with
  New and Open actions.
- **A shared editor shell:** command palette, settings, docked panels, layout
  presets, and built-in feature settings.
- **Rust support:** highlighting through Tree-sitter and, with `rust-analyzer` installed,
  hover documentation and diagnostics through the built-in language services.
- **Bundled tools:** a watched file tree with create, rename/move, and guarded delete
  commands, literal search in the current buffer,
  CommonMark rendering, and a terminal backed by Ghostty's terminal library.

## Quickstart

Install [Rust through rustup](https://rustup.rs/), your platform's native compiler
and linker tools, Python 3, and `curl`. The checkout pins its Rust toolchain.

```sh
git clone https://github.com/hxyulin/velk.git
cd velk
cargo xtask setup-terminal
cargo xtask run --release -- path/to/file.rs
```

`setup-terminal` caches the pinned Zig and Ghostty inputs under
`~/.cache/velk/toolchains`; `VELK_TOOLCHAIN_DIR` can override that location.
For Rust language services, install `rust-analyzer` on `PATH` or set
`VELK_RUST_ANALYZER` to its executable path. Editing works without a language server.

To build without launching:

```sh
cargo xtask build --release
./target/release/velk path/to/file.rs
```

The executable includes the built-in features. There is no separate plugin
staging step or runtime plugin directory. Cargo target-directory overrides apply.
A [Nix development shell](flake.nix) supplies Zig and `prek`; Rust uses rustup.

## Editing and navigation

Vim mode is enabled by default. Use `i` to insert, Escape or Ctrl+[ to return to
Normal mode, `h j k l`, `w b e`, `gg G`, counts, `x`, `dd`, `u`, and Ctrl+r.
Characterwise, linewise, and block Visual modes use `v`, `V`, and Ctrl+v.
See [the current Vim keymap](plugins/vim/README.md) for supported behavior and limits.
Settings > Features > Keymap provider also offers Standard editing.

| Shortcut | Action |
| --- | --- |
| Command/Ctrl+P | Quick open |
| Command/Ctrl+Shift+P | Command palette |
| Command/Ctrl+F | Find in buffer |
| Command/Ctrl+S | Save |
| Command/Ctrl+W | Close buffer |
| Command/Ctrl+, | Settings |
| F2 | Rename symbol when supported by the language server |

Closing the last buffer leaves the window open. Unsaved documents prompt before
closing, and recovery snapshots preserve unsaved work across application restarts.

## Development

```sh
prek install
prek run --all-files
cargo xtask test-headless
cargo xtask test-features
cargo xtask test
```

Use Conventional Commits. Read [AGENTS.md](AGENTS.md) for repository conventions
and [testing](docs/testing.md) for headless workflows. Generated captures, traces,
logs, and build output belong under `target/` or a temporary directory.

The internal feature runtime is documented in [built-in feature services](docs/plugin-api.md).
Document and rendering code remain in separate crates; their interfaces are
internal and may change during early development. We do not promise compatibility
with earlier configuration, archives, or internal APIs, and prefer correcting
incorrect designs over carrying compatibility shims.

Useful references: [documents](docs/document-foundation.md),
[history](docs/history.md), [keybindings](docs/keybindings.md),
[completion](docs/completion.md), and [diagnostics and language services](docs/semantic-services.md).

## Limitations

Velk is experimental. Rust is the primary exercised language integration; broader
language coverage, fuller Vim behavior, and large-file handling need more work.
There is no external plugin API, scripting environment, or browser build.

## License

Licensed under either [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
