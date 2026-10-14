# Velk

A desktop editor built with Rust and [RXUI](https://github.com/hxyulin/rxui).
The initial application opens a blank native window.

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
cargo clippy --workspace --all-targets --locked -- -D warnings
```

## Workspace

- `crates/editor`: the `velk-editor` package and `velk` application binary.
- `Cargo.toml`: shared package settings, dependencies, and lints for future crates.

RXUI is pinned to commit `093d05d9a2fa5491c3cebe26be46b8720b4c6158`
with its `native` feature enabled. Keep `Cargo.lock` committed to preserve the
resolved dependency graph. To update RXUI, change the workspace dependency's
`rev`, refresh the lockfile, and run the workspace checks.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org/), for example
`feat(editor): open a blank RXUI window` or
`chore(deps): update the pinned RXUI revision`.
