# Plugin packs

See [Plugin metadata and access declarations](plugin-api.md) for display names,
required and optional capabilities, and Markdown READMEs. Each pack member has
its own metadata through the ordinary named request contract.

The default built-in artifact is now `velk_plugin_builtin`, a single native
library containing Markdown, generic LSP, the Rust-analyzer definition, buffer
search, Files, Terminal, Indentation, and Autopairs. Each keeps its existing plugin ID, named registrations, instance state,
generation, dependency resolution, and load/unload controls.

Run `cargo xtask setup-terminal` once to cache the pinned terminal build inputs.
`cargo xtask build [--release]`, `plugins`, and `run` build/stage this pack.
The manifest has eight entries selecting IDs from one content-addressed artifact:

```json
{
  "id": "velk.markdown",
  "library": "artifacts/<digest>/libvelk_plugin_builtin.dylib",
  "digest": "<digest>",
  "pack": true
}
```

The other entries select `velk.lsp`, `velk.language-server.rust-analyzer`, and
`velk.search.literal`, `velk.files`, `velk.terminal`, `velk.indentation`, and `velk.autopairs` using the same library and digest. A manifest selects which
members to instantiate; discovery never automatically enables unlisted members.
Absent/false `pack` retains the standalone v1 format. Conflicting digests/formats
for one artifact path and duplicate selected IDs are rejected.

## Implementation and exports

Each implementation crate is an ordinary `rlib`, with no exported loader symbol.
The built-in pack depends on those crates and calls:

```rust
velk_plugin_sdk::export_plugin_pack!(
    velk_plugin_markdown::CommonMark,
    velk_plugin_lsp::Lsp,
    velk_plugin_rust_analyzer::RustAnalyzer,
    velk_plugin_search::Search,
    velk_plugin_files::Files,
    velk_plugin_terminal::Terminal,
);
```

Standalone wrappers remain available as cdylib examples. For example:

```sh
cargo build --release -p velk-plugin-markdown --example velk_plugin_markdown
```

The standalone artifact is in `target/release/examples/`, with the platform's
library prefix/suffix. Separating wrappers avoids feature unification and duplicate
entry symbols when packs and standalone targets are built together. The built-in
pack contains no duplicate source copies or feature-gated implementation forks.

The pack exports `velk_plugin_pack_entry_v1`, returning a checked `PluginPackApi`
with a bounded member count and a C factory-table query. Each member returns the
existing `PluginApi`. All member IDs, versions, required functions, layout and
lifetime flags are validated before any factory runs. Duplicate member IDs,
unknown lifetime flags, invalid counts/layouts and missing selected IDs fail.
The internal ABI remains **1**. No Rust containers or trait objects cross it.

## Code image versus instance lifetime

The host verifies and privately copies an artifact once, then shares one code
image across its instances. It caches active images weakly; unloading the last
ordinary member releases the image and removes its private copy. Reloading one
member while another remains active reuses that image, creates fresh state and
advances only that member's generation. Old hook handles stay retired.

Artifacts are keyed by digest/format. Each load operation revalidates selected
artifact sources, including resident cached builds; members of the same source
share that operation's verification. Different artifact builds can coexist.
Loading does not replace active IDs. Explicitly unload before replacing a member.

If **any advertised member** requires resident code, the whole image is retained
until process exit, even if that member is not selected in the manifest. This is
decided before initialization, including initialization failures. The built-in
pack inherits LSP's resident requirement because process-global Tokio signal
hooks may outlive a session. Logical unload still cancels/joins sessions and
destroys plugin state. Destruction failures retain the image and block that ID
until restart, as with standalone plugins.

Failures are per selected instance: a failed factory does not remove working
siblings. Invalid pack metadata rejects the artifact before initialization.
Instances retain a shared image lease through teardown, so one member cannot
unmap sibling callbacks. Resident images keep their private copies by design.

`cargo xtask test-plugins` covers the real built-in pack, independent state and
partial initialization failure in a nonresident test pack, duplicate metadata,
stale handles, last-member cleanup, resident subsets, artifact revalidation, and
LSP sessions across individual member unload/reload. Standalone plugin tests
remain part of the same command. Test probe packs are never staged as built-ins.
