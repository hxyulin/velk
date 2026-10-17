# Wasm plugins

Velk can load pure matching providers as core-Wasm modules alongside native plugins.
The first bundled implementation is `velk.completion.wasm`, using the same ranking
algorithm as `velk.completion`. Native matching remains the default.

## Build and select

The pinned Rust toolchain includes `wasm32-unknown-unknown`. Run:

```sh
cargo xtask run --release --wasm -- path/to/file.rs
```

Open **Plugins > Interfaces > Completion matching provider** and select
`velk.completion.wasm`. The provider is also available through Settings > Plugins.
`cargo xtask plugins --wasm` stages it without launching the editor.
The guest always builds with release optimizations, including when the host uses a
debug build. Omit `--wasm` to stage the ordinary native bundle.

The existing plugin settings, enable/disable, restart, and recovery paths handle its
ID. A restart creates a fresh generation; old matching handles remain retired.
No state migrates between matching instances.

## Boundaries

- `crates/plugin-wasm-api` defines the checked buffer representation and limits.
  It has no Wasmtime, native ABI, editor, or feature implementation dependency.
- `plugins/completion/matching` owns the transport-independent default algorithm.
  Both the native completion plugin and the Wasm guest use it.
- `plugins/completion-wasm` exports the core-Wasm matching functions.
- `crates/plugin-host/src/wasm.rs` owns Wasmtime execution and implements the same
  `velk_commands::completion::matching::Policy` used by native providers.

The Wasm matching ABI version is 1. Rebuild the host and guests together during
early development. This is a matching transport, not a port of the complete native
SDK: commands, host imports, filesystem access, WASI, settings, async work, and UI
contributions are not supported by this guest format.

A manifest entry uses the ordinary ID, library path, BLAKE3 digest, dependencies,
and install reason, plus:

```json
"runtime": {
  "kind": "wasm_matching",
  "version": "0.0.0-dev",
  "metadata": {
    "name": "My matching provider",
    "readme": "A Markdown description.",
    "commands": [],
    "capabilities": []
  }
}
```

Native entries default to `{"kind":"native"}`. Wasm metadata is distribution
metadata; the host rejects declarations of currently unsupported contributions.
Only the supplied bytes are hashed and compiled. Precompiled native `.cwasm`
files are not accepted. Each load compiles its module on the lifecycle worker;
the Wasmtime engine is reused, but persistent compiled-module caching is deferred.

## Buffer ABI

Exports are `memory`, `velk_abi_version() -> u32`,
`velk_allocate(length: u32) -> u32`, and
`velk_matching_v1(input: u32, length: u32, output: u32, capacity: u32) -> i32`.
The allocator is called once for `BUFFER_BYTES`, returning a nonzero offset to a
contiguous allocation. Input occupies `MAX_INPUT` bytes and output immediately
follows it. The matching call returns the candidate count, or a negative error.
The allocation belongs to the instance and is reclaimed when its store drops.
The guest must not free or relocate the allocation between calls.

All integers in memory are little endian. Input contains a u32 query byte length,
UTF-8 query, u32 candidate count, then u32 byte length and UTF-8 per candidate.
Output contains one 40-byte record per candidate: a u64 matched flag and four u64
scores. Matched flags are 0 or 1; unmatched score fields must all be zero. Scores
retain the native contract's lexicographic ordering. Rust struct layout and
process pointers are never used as wire representations.

The host validates at most 1,000 candidates and 4,096 UTF-8 bytes per string before
execution. Requests and result counts must match exactly. Guest helper types
validate UTF-8, lengths, counts, and trailing data and expose borrowed strings.

## Memory access and copying

This path avoids JSON and intermediate request buffers. The host writes lengths
and UTF-8 bytes **directly into reusable guest linear memory**. The guest borrows
those strings in place and writes fixed records into the output region. The host
reads records directly from guest memory and creates only the owned score vector
required by `Policy`. Borrows never survive execution, growth, or retirement.

There is still one copy of text from host-owned strings into the guest. Avoiding
that copy requires making a linear-memory region the original data backing store,
or changing the service to use cached snapshots/opaque resource handles. A Wasm
integer pointer cannot dereference an arbitrary native Rust allocation.

Wasmtime also supports importing one memory into multiple instances, including
shared memories for concurrent execution. This can avoid repeating transfers
between cooperating modules. It also lets every module with access mutate that
memory: it forms a shared trust group, not independent memory isolation. The host
must control allocation, aliasing, synchronization, and data lifetimes; passing
Rust String/Vec headers does not solve those requirements. This implementation
keeps instances isolated and has no imported memory. Shared arenas and cached
snapshot services should be added only for concrete measured pipelines.
See [Wasmtime Memory](https://docs.wasmtime.dev/api/wasmtime/struct.Memory.html),
[SharedMemory](https://docs.wasmtime.dev/api/wasmtime/struct.SharedMemory.html), and
[the security model](https://docs.wasmtime.dev/security.html).

## Limits and failure

Every store has a 32 MiB linear-memory limit, at most one memory, one instance,
one table with at most 4,096 elements, a 512 KiB Wasm stack, and 50 million fuel
units per matching call. Initialization, including start functions and allocation,
shares a separate 50 million fuel allowance. Modules are limited to 4 MiB and may
not import anything. No ambient filesystem, environment, process, or network
access is installed.

Fuel bounds guest computation; it is not a wall-clock deadline or a bound on
module compilation. A store mutex serializes execution. Busy calls return promptly
instead of waiting on another caller; existing completion prefix matching handles
errors. Invalid caller input does not retire a healthy instance. Guest traps,
invalid counts, and malformed output permanently drop the store, so later calls
fail until the plugin is restarted. A plugin can still return poor but structurally
valid rankings.

Retirement waits for any in-progress fuel-bounded call, then drops its store and
memory even if old provider handles survive. This implementation supplies no
privileged host imports that could block outside the fuel budget. Memory isolation
does not replace validation of results before applying them to editor state.

## Verification

```sh
cargo test -p velk-plugin-host -p velk-plugin-wasm-api -p velk-completion-matching
cargo xtask test-plugins --wasm
```

Unit tests exercise imports, ABI versions, missing exports, allocation bounds,
start loops, traps, invalid results, fuel, memory growth, contention, and retirement.
The real-guest smoke compares native and Wasm scores, covers Unicode, typos, NULs,
maximum input size, oversized caller input, restart, stale handles, unload, and
artifact digest failures. See [transport direction](plugin-transports.md) for
performance experiment scope and the native ABI alternatives.
