# AGENTS.md

Velk is a native Rust editor built with RXUI. Use the pinned toolchain in
`rust-toolchain.toml`; see [README.md](README.md) for setup and current limitations.

## Commands

- `cargo xtask setup-terminal`: cache the pinned Zig and Ghostty build inputs.
- `cargo xtask run --release -- path/to/file.rs`: build and run with bundled plugins.
- `prek install`: install file checks and Conventional Commit message validation.
- `prek run --all-files`: run repository checks and formatters.
- `cargo xtask test`: run workspace tests with the cached terminal build inputs.
- `cargo xtask test-plugins`: check native plugin loading and shared contracts.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: lint with
  Zig available on `PATH`, for example inside `nix develop`.

Run checks relevant to the change. Add regression coverage for behavior changes;
documentation-only changes do not need application tests. Report what was tested
and any checks that could not run.

## Structure and boundaries

- `crates/core` owns text, history, snapshots, and UI-independent close/save policy.
- `crates/editor-model` owns editing commands, key mapping, annotations, and text
  projections without renderer dependencies.
- `crates/editor` adapts document lifecycle, saves, recovery, workspace state,
  and the shell. `crates/editor-widget` adapts Core to RXUI input and rendering.
- Shared contracts and services live in `crates/`; feature plugins live in
  `plugins/`. Build and staging commands live in `xtask/`.
- Common plugin definitions live in `plugin-contracts`; native tables/codecs in
  `plugin-api`. The host must not depend on SDK authoring/lifecycle code.
- Keep one authoritative document model and a consistent service interface.
  Extend existing contracts before introducing parallel plugin-specific mechanisms. Update the
  host, SDK, bundled plugins, and relevant docs together when a contract changes.
- Native plugins are trusted in-process code. Capability declarations currently
  describe requirements; they do not enforce permissions or provide isolation.

## Coding conventions

- No emoji in code, comments, documentation, or commit messages.
- Prefer descriptive names, small cohesive functions, and explicit control flow
  so code explains itself.
- Keep comments sparse. Explain non-obvious intent, invariants, tradeoffs, and
  safety requirements when needed; do not narrate the code. Document public
  contracts where callers need information the types cannot express.
- Match surrounding Rust conventions and use `cargo fmt`.

## YAGNI

Implement the current requirement with the smallest coherent change. Avoid
speculative features, configuration, dependencies, and abstractions for imagined
future needs. Introduce shared machinery when concrete use cases justify it.

## Repository hygiene

- Use Conventional Commits, such as `feat(editor): add a document command`.
  Scope is optional; use `!` for breaking changes.
- Keep `Cargo.lock` and `flake.lock` tracked. They are excluded from default
  ripgrep searches; use an explicit file path or `rg --no-ignore` to inspect them.
- Keep generated reports, measurements, traces, logs, caches, and build output
  out of Git. Prefer `target/` or a system temporary directory for local artifacts.
- Keep instructions concise and update the relevant docs when behavior changes.
