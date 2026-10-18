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
- `crates/plugin-wasm-sdk` owns safe guest authoring, exports, buffer ownership,
  and checked score output. The host does not depend on this authoring crate.
- `plugins/completion/matching` owns the transport-independent default algorithm.
  Both the native completion plugin and the Wasm guest use it.
- `plugins/completion-wasm` implements the safe matching trait and exports it
  through the SDK.
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
files are not accepted. Compilation runs on the lifecycle worker. Each manager
owns one engine and an LRU cache of at most eight compiled modules, keyed by the
BLAKE3 digest of their bytes. Hits reuse compiled code but create independent
stores and memories. The engine configuration is fixed for that cache's lifetime;
cache entries never cross engines. Every load still verifies the artifact digest.
Compiled modules can remain resident after store retirement until cache eviction
or manager destruction.

Wasmtime's persistent compilation cache is enabled by default in Velk's per-user
OS cache directory (`~/Library/Caches/velk/wasm` on macOS). Its keys account for
module bytes, compiler/runtime version, target and relevant engine settings, so
incompatible entries are not reused. Missing, unreadable or corrupt cache entries
fall back to compilation. Cache configuration failures disable persistence without
preventing plugin loading; individual write failures leave compilation usable. The disk cache has soft limits of 256 MiB and
2,048 files, maintained asynchronously by Wasmtime.

The cache contains locally generated native code and must stay in an app-owned,
trusted directory. Plugin manifests still reference `.wasm`, never cached native
artifacts; the host verifies their digest on every preparation and load. The
compiled-code disk cache does not share guest memory or mutable plugin state.

### Prepare compilation before activation

`build`, `plugins` and `run` with `--wasm` prepare staged Wasm artifacts before
launching the app. To prepare an already staged directory separately:

```sh
cargo xtask prepare-wasm
cargo xtask prepare-wasm --release
```

The API is `Manager::prepare_wasm()`. It verifies all Wasm artifacts, skips native
libraries, deduplicates identical digests and compiles without instantiating guests
or executing their start functions. It reports the distinct artifact count;
`wasm_cache_stats()` exposes disk-cache hits and misses. The first preparation
still pays compilation cost; later processes load the cached code.

Embedders and benchmarks can call `set_wasm_cache_directory(Some(absolute_path))`
before initialization to select an app-owned directory, or pass `None` to disable
persistence. Guest stores and the eight-entry in-memory module cache remain
independent of this setting. Cache deletion affects future loads, not live stores.

## Authoring a matching provider

Use a `cdylib` crate with `velk-plugin-wasm-sdk` as a dependency. Implement
`Matching` and invoke the export macro once, using a crate-qualified provider path:

```rust
use velk_plugin_wasm_sdk::{InvalidScores, Matching, Request, Scores};

struct PrefixMatching;

impl Matching for PrefixMatching {
    fn rank(request: Request<'_>, scores: &mut Scores<'_>) -> Result<(), InvalidScores> {
        for candidate in request.candidates() {
            let score = candidate.starts_with(request.query).then_some([0; 4]);
            scores.push(score)?;
        }
        Ok(())
    }
}

velk_plugin_wasm_sdk::export_matching!(crate::PrefixMatching);
```

Build with `cargo build --release --target wasm32-unknown-unknown`. Register the
artifact and its digest with the manifest format above; the bundled xtask stages
the bundled provider, rather than discovering arbitrary third-party guest crates.
The SDK supplies all ABI exports and reusable allocation, checks incoming ranges,
and rejects missing or excessive output scores. Plugin code uses borrowed UTF-8
and typed scores without raw pointers, offsets, allocation exports, or byte codecs.
It can use Rust's standard library, subject to the host's no-import restriction;
APIs requiring filesystem, threads, networking, or operating system services are
not available. Scores are lexicographically ordered: smaller scores rank first.

For tests, `velk_plugin_wasm_sdk::dispatch::<Provider>(input, output)` runs the
same checked request/response adapter on native slices. The low-level
`velk-plugin-wasm-api` crate supplies request encoding and response decoding for
fixtures. See `plugins/completion-wasm/src/lib.rs` for native algorithm parity
coverage and run `cargo xtask test-plugins --wasm` for real runtime coverage.
Native tests do not reproduce fuel, stack, memory, or import restrictions.

This SDK covers matching only. Migrating other plugins requires concrete typed
service contracts and deliberately granted host imports, including lifetimes and
cancellation; adding a matching implementation does not provide those services.

### Generated bindings and WIT

The current SDK wraps Velk's custom core-Wasm buffer ABI; it does not generate
WIT bindings or load Component Model artifacts. WIT describes typed interfaces,
imports, exports, and resource handles. `wit-bindgen` generates guest traits and
export glue, while Wasmtime's component bindings generate typed host calls.
This could replace hand-maintained transport code as additional services emerge.
See the [official Rust binding tutorial](https://component-model.bytecodealliance.org/language-support/building-a-simple-component/rust.html)
and [Wasmtime component binding generator](https://docs.wasmtime.dev/api/wasmtime/component/macro.bindgen.html).

Generated bindings still obey the
[Canonical ABI](https://component-model.bytecodealliance.org/advanced/canonical-abi.html).
Owned strings and lists can introduce allocations and transfers; code generation
does not promise zero-copy. Before adopting that transport, compare generated
bindings with this borrowed adapter using the same workload and resource limits.

An isolated authoring experiment used `wit-bindgen` 0.57.1 with a WIT matching
interface and the same algorithm crate. Its generated `Guest` implementation
passed native score-parity tests and compiled for `wasm32-unknown-unknown`. The
generated API accepted owned `String` and `Vec<String>` values and returned an
owned score vector. Its release core module was 273,991 bytes, including component
type metadata; it was not componentized or executed by a component runtime, so
that number does not measure packaged component size or runtime overhead.
Native test linking required gating the generated export macro to `wasm32`;
the export macro also required a locally imported provider identifier.

## Buffer ABI

Exports are `memory`, `velk_abi_version() -> u32`,
`velk_allocate(length: u32) -> u32`, and
`velk_matching_v1(input: u32, length: u32, output: u32, capacity: u32) -> i32`.
The allocator returns a nonzero offset to at least the requested number of bytes,
up to `BUFFER_BYTES`. The host initially requests 4 KiB and grows geometrically
when the encoded request plus result records need more space. Calling the
allocator again may relocate the allocation; the host validates and replaces its
offset before accessing it. The SDK retains its largest buffer until retirement.
For each call, input occupies exactly `length` bytes and output immediately
follows it, with `capacity` set to the required result byte count. The guest checks
that both regions are disjoint and belong to its current allocation. The matching
call returns the candidate count, or a negative error. The allocation belongs to
the instance and is reclaimed when its store drops; it must not move during a
matching call.

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
one table with at most 4,096 elements, and a 512 KiB Wasm stack. A matching call
receives `min(250_000_000, 50_000_000 + 256 * encoded_input_bytes)` fuel units,
including any buffer growth. The input allowance lets larger valid batches do
more work while the hard ceiling bounds hostile or unusually expensive requests.
The entire candidate list is still scored in one invocation, preserving providers
that use batch context. This policy replaces the original flat 50-million budget;
it is an explicit runtime change, not a relaxed latency or correctness gate.
Initialization, including start functions and initial allocation, shares a separate
50 million fuel allowance. Modules are limited to 4 MiB and may not import anything. No ambient filesystem, environment, process, or network
access is installed.

Fuel bounds guest computation; it is not a wall-clock deadline or a bound on
module compilation. A store mutex serializes execution. Busy calls return promptly
instead of waiting on another caller; existing completion prefix matching handles
errors. Invalid caller input does not retire a healthy instance. Guest traps,
invalid counts, and malformed output permanently drop the store, so later calls
fail until the plugin is restarted. Valid requests with large batches of long
labels can exhaust fuel even when they fit the byte/count limits; those limits do
not promise that every allowed request completes within the execution budget.
The profiling boundary diagnostic demonstrates this with 1,000 roughly 3 KiB
labels. Fuel exhaustion still retires the generation; this is not successful
ranking parity, even when the completion UI can fall back to prefix matching.
A plugin can still return poor but structurally valid rankings.

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
