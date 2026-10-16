# Diagnostics demo

This standalone Rust project intentionally fails to compile. It is excluded from
Velk's workspace so normal builds and tests are unaffected.

From the Velk repository root, run:

```sh
cargo xtask run --release -- examples/diagnostics/src/lib.rs
```

Load the **Diagnostics** plugin if it is not enabled. Use **Diagnostics: Show
Problems** in the command palette. Leave `types.rs` and `ownership.rs` unopened:
after rust-analyzer checks the project, their errors should appear alongside
`lib.rs`. Click a row, or use `F8` and `Shift+F8`, to open the corresponding file
and navigate to the result. Closing its buffer should preserve saved-file results.

The files demonstrate an unresolved function, mismatched return and argument
types, use after move, and conflicting borrows. Fix an error and save to watch the
panel update. Restarting the language service may be needed when changing the
project manifest.

`cargo check --manifest-path examples/diagnostics/Cargo.toml --target-dir target/diagnostics-demo`
is expected to fail.
Local build output goes into the repository's ignored `target/diagnostics-demo/`
directory when run from the demo directory. From the repository root, use the
explicit `--target-dir` argument above because Cargo does not load nested
configuration based on `--manifest-path`.
