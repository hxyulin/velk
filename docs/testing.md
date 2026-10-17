# Headless testing

Use `cargo xtask test-headless` for document, workspace, editing, completion,
widget, application-adapter, and SDK fixture tests. It opens no native windows
and does not need a running language server. The application/widget tests still
compile RXUI and the terminal dependencies; `xtask` supplies cached terminal build
inputs. This is a focused suite, not a replacement for all workspace tests.

Pass a test-name filter when working on one behavior:

```sh
cargo xtask test-headless workspace
cargo xtask test-headless completion
cargo xtask test-headless headless_harness
```

For the fastest model-only loop, avoid compiling the native application:

```sh
cargo test -p velk-editor-model --locked
cargo test -p velk-commands --locked
```

## Application fixtures

`crates/editor/src/document/tests.rs` provides a shared `Harness`. It creates an
empty document, isolated temporary files, an RXUI runtime without a window, and a
controllable background executor. Tests can issue the same typed host requests
that plugins use, inspect JSON snapshots of open documents, and drain chained
background callbacks with a bounded number of batches.

`QueuedExecutor::complete_at(index, runtime)` executes one queued blocking job.
Use it to finish independent saves in reverse order, or edit/close a document
before allowing a pending result to arrive. `complete` runs one captured batch;
`drain` continues through jobs scheduled by callbacks. Assertions should check
text, identities, revisions, dirty state, and disk contents, rather than timing.
Async futures are deliberately unavailable in this fixture; use plugin-host
runtime tests for async services and cancellation. Native dialogs also need
separate interaction coverage.

## Plugin host mocks

Enable the SDK's `test-support` feature in a plugin's dev-dependencies:

```toml
[dev-dependencies]
velk-plugin-sdk = { workspace = true, features = ["test-support"] }
```

`velk_plugin_sdk::test_support::MockHost::new(handler)` creates a safe host fixture.
The handler accepts `host::Request` and returns `Result<host::Response, String>`.
Attach `fixture.host()` to the plugin; `fixture.take_requests()` returns requests
received since the preceding call. Capture an `Arc<Mutex<_>>` in the handler to
model changing buffers, files, diagnostics, and failures. Reject unexpected
requests in the handler so accidental host dependencies fail the test.

The fixture uses the SDK's actual serialization, synchronous reply callbacks,
and host-reference lifetime path. It is available only with `test-support` and
does not load dynamic libraries or enforce plugin capability declarations.
Use `cargo xtask test-plugins` for native loading, capability checks, dependency
routing, retirement, and other behavior that needs the real host.

Keep generated screenshots, scenario output, logs, and snapshots in `target/`
or a system temporary directory. Tests should use temporary directories rather
than the user's settings, recovery store, workspace, or installed plugins.

## Live feature walkthrough and captures

An opt-in GPU walkthrough exercises the real bundled plugins and installed
`rust-analyzer`. It uses an isolated temporary Rust project, never the user's
files. It checks Ctrl+Space, typing and refreshing completion, selected-item documentation
through the Markdown plugin, keyboard selection and acceptance, shared views, rename with unsaved text, saving/undo/redo, file and
directory creation, deletion guards, and closing the final document. It then
unloads LSP and checks automatic document-word completion after real keyboard
input, acceptance, and undo in an untitled buffer.

```sh
cargo xtask plugins
cargo xtask test-headless render_feature_walkthrough -- --ignored --nocapture
python3 tools/rgba_to_png.py target/feature-demo/captures --width 1280 --height 800
```

This requires `rust-analyzer` on PATH and a working GPU backend, but no native
window. Initial completion queries may return no result while the server indexes;
the walkthrough retries those queries within a fixed bound. Runtime callbacks and
render preparation are pumped between steps. Captures use the actual RXUI painter
and syntax/layout services. The converter needs only Python's standard library.

The test is ignored in the default suite because it depends on an external server
and GPU. Frames and its verification report stay under `target/feature-demo/`.
These checks do not replace native testing of IME, clipboard integration,
operating-system shortcuts, window scaling, or multiple monitors.

## Language-server process cleanup

`cargo test -p velk-plugin-lsp` includes Unix regression fixtures whose server
spawns a child holding a real filesystem lock. These check graceful shutdown,
forced termination, transport drop, and isolation from a second live server.
The timeout fixture deliberately ignores the LSP shutdown request.

With `rust-analyzer` and `rust-src` installed, run the real Cargo-lock tests:

```sh
cargo test -p velk-plugin-lsp rust_analyzer_shutdown_releases_an_active_cargo_build_lock -- --ignored --nocapture
```

These use temporary Cargo projects with a sleeping build script. They wait until
Cargo holds its build-directory lock, then check that server shutdown releases
it. One case suspends the server to force the shutdown timeout path. They do not
use the workspace's build directory or modify user configuration.

Completion regression coverage also checks provider filter/sort metadata, identity
across label changes and incremental results, trigger declarations/settings,
keyboard versus paste/IME/programmatic input, dismissal and movement invalidation,
and actual LSP wire contexts. Unsupported trigger characters must not send a server
request. These deterministic tests do not require an installed language server.

Completion documentation tests cover canceled and late semantic replies, accepting
while documentation resolution owns the command slot, literal fallback without an
executor, metadata format and size limits, and a bounded independent scroll area.
The LSP fixture returns documentation together with import edits, verifying that
reading documentation cannot apply those edits.

Completion matching regressions cover late exact-match promotion, explicit
selection retention, reranking after typing, Unicode, typo edit ranges, and
keeping the best document words before the source result cap.
