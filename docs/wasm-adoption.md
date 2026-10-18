# Wasm adoption gate

Wasm migration is conditional on measured behavior and a usable authoring API.
Passing the completion matching gate permits adopting that provider; it does not
establish that syntax, terminals, language servers, or document services can be
migrated. Each new service needs representative workloads and the same ownership,
validation, cancellation, and retirement semantics as its native implementation.

## Acceptance criteria

These thresholds were selected before the expanded profiling run. Compare release
builds using the actual native and Wasm provider leases, the same matching source,
and production fuel and memory limits. A failure must produce a nonzero exit code;
a missing measurement is inconclusive, not a pass.

| Measurement | Required result |
| --- | --- |
| Correctness | Identical scores for all acceptance queries; no traps on the declared valid workloads |
| Warm median | At most 1.5 times native |
| Warm p95 and p99 | At most 2 times native |
| Interactive p99 | Below 2 ms for 1,000 typical candidates |
| First-provider resident memory | At most native plus 32 MiB, after subtracting the corresponding process baseline |
| Additional-provider resident memory | At most the larger of twice native marginal memory and 8 MiB per provider |
| First load p95 | At most 250 ms, including engine creation, compilation, and instantiation |
| Additional load p95 | At most 50 ms, using the existing engine and the current production loading path |

Use a 1 microsecond denominator floor for latency ratios so timer noise around
trivial native calls does not dominate the decision. Report the unfloored times
alongside the ratios. Memory must be resident physical memory, not the virtual
address space reserved for Wasm memories. Report baseline and loaded process
measurements separately, repeat fresh processes, and inspect 1, 5, and 10 providers.
A fixed runtime cost must not be mistaken for a cost multiplied by every plugin.

Artifact size is a reported tradeoff rather than a blocking gate. Compare both
the shipped artifacts and implementations with equivalent behavior, using the
same stripping policy. A matching-only Wasm module is not directly comparable to
a native completion plugin that also supplies sessions, commands, and UI policy.
Report host runtime code separately: downloading smaller plugins does not imply a
smaller editor executable or lower resident memory.

## Workloads and interpretation

The acceptance set includes 10, 100, and 1,000 ordinary completion labels, exact
matches, case changes, typos, Unicode, no matches, and empty queries. It also
includes a valid batch of 100 labels around 3 KiB each, which previously exhausted
the original flat 50-million-fuel budget. Count any failure explicitly; do not drop
that case. Runtime-policy changes must be explicit and recorded before rerunning
the same acceptance thresholds.

Measure individual warm calls over multiple trials, rotate implementation order,
and validate parity before timing. Keep cold load and warm calls separate. Record
the machine, toolchain, module hashes, sample counts, raw samples, and production
limits. Run performance measurements without concurrent compilation. Generated
measurements belong under `target/` or a temporary directory, not in Git.

This is a provider-level gate on the measured machine. It does not measure the
editor event loop, LSP response time, UI presentation, concurrent contention, or
browser execution. Scaling memory to ten instances tests this pure provider's
cost; it is not a prediction of ten arbitrary plugins with different state.

## Developer experience

Plugin authors should implement safe Rust service logic and use the SDK for
exports, allocation ownership, buffer checks, and wire layout. Each service still
requires an explicit contract and a host adapter. A pleasant matching SDK alone
does not provide commands, host services, settings, async tasks, or UI APIs.

Generated WIT bindings remain an alternative to evaluate before multiplying
handwritten transport adapters. Their benefit is generating typed imports,
exports, records, and resource bindings from an interface definition. Their
Canonical ABI does not guarantee zero-copy strings or lists. Borrowing existing
guest buffers and using opaque handles for retained data remain separate design
choices that need workload measurements.

## Reproduce the decision

See [the profiling tool](../tools/wasm-profile/README.md) for release builds and
commands. The runner measures actual Manager leases, checks parity, records raw
individual calls and fresh-process lifecycle samples, and exits 1 on a failed
gate. Its source and fixture are tracked; generated measurements are ignored.

The first expanded run on arm64 macOS with Rust 1.99.0 failed the adoption gate:
seven of nine valid long-label queries exhausted the flat fuel budget, and repeated
module compilation exceeded the additional-load budget. Normal pooled latency
and the absolute memory budgets passed.

The follow-up implementation reuses an engine-scoped LRU cache of eight compiled
modules, grows transfer buffers from 4 KiB on demand, and replaces the flat fuel
allowance with an explicit input-aware allowance capped at 250 million units.
The same acceptance thresholds and queries pass against both the matching-only
native fixture and the deployed native completion plugin. The original failing
measurements remain separate; the runtime policy was changed before remeasurement.

The gate covers pooled distributions for its declared workloads. Per-query
breakdowns still expose an empty-query fast path slower than the pooled ratio
limits. Memory meets an absolute budget, not a strict two-times-native ratio;
Wasmtime retains a material first-use cost. Smaller guest artifacts also exclude
the runtime's code size and resident memory.

A separate boundary diagnostic tests 1,000 labels around 3 KiB each, beyond the
100-label long-input acceptance case. Eight of nine queries hit the 250-million
fuel ceiling and retire the guest, while the empty query completes. These are
transport-valid inputs; count/byte limits do not guarantee completion within a
CPU budget. The normal acceptance pass must not be interpreted as support for
all maximum-size requests or arbitrary plugin workloads. Native remains the
default and Wasm remains opt-in. Broader adoption requires an execution strategy
for expensive work that preserves responsiveness, rather than just a larger
synchronous fuel allowance. See the tool's boundary command for reproduction.
