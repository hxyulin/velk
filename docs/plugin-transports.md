# Plugin transport direction

Interfaces describe behavior, ownership, cancellation, and validation independently
of transport. The current implementation uses trusted native libraries, C tables,
and owned/bounded message data. Internal versions remain 1 during development.
The first bounded Wasmtime adapter is implemented for completion matching; see
[Wasm plugins](wasm-plugins.md) for building, ABI, memory behavior, and limits.
This document records the broader transport direction and remaining experiments.
The [Wasm adoption gate](wasm-adoption.md) defines measurable acceptance criteria
before migrating a provider.

## Native options

- **Linked Rust adapters:** ordinary Rust traits are suitable for components built
  together. They can share borrowed values without serialization. This is a useful
  opt-in path for bundled implementations after benchmarking, while retaining the
  same semantic contract and tests. It does not provide independently compiled
  plugin compatibility or isolation.
- **Borrowed C views:** `repr(C)` records, borrowed slices, explicit tags, opaque
  resource handles, and caller-owned output avoid many allocations. Matching
  already uses this approach. It is a strong option for measured native hot paths.
- **ABI-safe Rust types:** [abi_stable](https://docs.rs/abi_stable/latest/abi_stable/)
  supplies layout checking and FFI-safe equivalents such as RString/RVec.
  [stabby](https://docs.rs/stabby/latest/stabby/) supplies ABI-stable types and
  representations. Both require designing the exposed type surface around their
  guarantees; neither makes arbitrary std types safe to copy across a boundary.
- **Layout-aware fast paths:**
  [savefile-abi](https://docs.rs/savefile-abi/latest/savefile_abi/) is especially
  relevant to runtime layout checking plus serialization fallback. It checks
  layouts, including String/Vec heuristics, rather than relying only on a shared
  toolchain label. Its ownership and supported-type rules would need evaluation
  against Velk's callbacks and unload requirements before adoption.
- **Archived or compact data:** [rkyv](https://rkyv.org/architecture/archive.html)
  gives values a separate archived representation. Validated archived access can
  avoid rebuilding owned object graphs. It still requires encoding, alignment,
  lifetime, schema, and validation policies. Compact Serde formats are another
  benchmark candidate for owned messages; reduced bytes alone do not establish
  lower end-to-end latency.

The initial recommendation is to retain JSON for metadata and infrequent control
messages, benchmark C views and an archived/compact encoding for hot or large
messages, and consider linked adapters for bundled providers where they remove
measurable work. Avoid introducing a replacement standard library across every
plugin merely to optimize a few callbacks.

## Why a std version and memcpy are insufficient

A matching compiler/std version does not describe all layout, target, feature,
allocator, validity, ownership, and destruction obligations. Size and alignment
assertions do not check field offsets, enum representation, nested allocations,
resource ownership, or callback lifetimes. Copying a String or Vec record copies
its allocation pointer, not its contents, and can create multiple owners.

A deliberate same-build borrowed fast path is possible when all these obligations
are part of the contract. It should never deserialize arbitrary bytes into ordinary
Rust values or let the receiving image destroy allocations with an unrelated
allocator. Flat value records can be designed for byte copying; even then padding,
endianness, invalid bit patterns, and pointers matter. The
[bytemuck Pod requirements](https://docs.rs/bytemuck/latest/bytemuck/trait.Pod.html)
are a useful reference for how narrow a safe plain-data subset is.

Native process addresses cannot serve as Wasm pointers. Efficient native bindings
and Wasm bindings should share semantics while using different representations.

## Wasm matching and further experiments

The optional `velk.completion.wasm` provider now runs the shared matching algorithm
with direct buffer writes and borrowed guest-side strings. Filesystem access, UI
objects, async Rust futures, and document authority remain outside this adapter.

Compare the current native provider, a linked adapter, and a Wasmtime guest for:

1. Cold compilation, cached startup, and steady-state calls separately.
2. End-to-end p50/p95/p99 latency, allocations, and bytes copied at realistic and
   maximum request sizes, with tracing overhead accounted for.
3. Malformed results, memory limits, traps, deliberate infinite loops, retirement,
   and fallback behavior.
4. Browser execution of the same guest algorithm through an appropriate browser
   adapter; Wasmtime itself is not the browser runtime.

Use explicit memory/resource limits and fuel or epoch interruption. A JIT does not
inherently remove sandboxing. Host imports remain privileged and their work needs
its own bounds: guest interruption does not automatically cancel blocking host I/O.
See [Wasmtime interruption](https://docs.wasmtime.dev/examples-interrupting-wasm.html)
and [security model](https://docs.wasmtime.dev/security.html).

Evaluate WIT/component bindings against a small core-Wasm buffer protocol. The
[Canonical ABI](https://component-model.bytecodealliance.org/advanced/canonical-abi.html)
defines lifting/lowering and cross-language representations; it does not promise
zero copies. Keep interface IDs, provider selection, document revisions, and
failure semantics above either adapter. Source Rust futures/executors are not a
shared binary interface.

## Execution and lifecycle work

Pure interactive decisions run synchronously and may not perform I/O or host calls.
Background backends must queue/poll promptly, observe cancellation, and join owned
workers. Stateful sessions must define whether they survive logical retirement.
Native callbacks remain cooperative: payload bounds and panic containment cannot
interrupt a hung native function or isolate an abort/memory error.

The host now records scopes for editing, matching, snippet, and completion
presentation adapters, including serialization and validation. General requests
already record queue wait. Small snippet/presentation results start with 4 KiB
caller storage and permit one bounded capacity retry instead of allocating their
full 2 MiB/1 MiB limits on every call. Providers must remain pure/reentrant across
that retry.

Further work should add per-provider slow-call attribution, lifecycle/queue stress
scenarios, and measurements of full-document syntax transfer. Required-service
fallback policy, particularly the linked undo fallback, also remains a separate
follow-up. Do not treat a timeout observer as a safe way to kill a native thread.
