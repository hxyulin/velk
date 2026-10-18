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
the current 50-million-fuel budget. Count that failure explicitly; do not drop the
case, silently increase fuel, or change limits during measurement to obtain a pass.

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

The first expanded run on arm64 macOS with Rust 1.99.0 failed the adoption gate.
Pooled normal matching latency, first load, and the declared resident-memory
budgets passed. Per-query diagnostics exposed an exception: the empty query with
1,000 labels took about 2.7 times native at median (roughly 47 versus 17
microseconds), despite the pooled distribution passing. The runner records both
views; a pooled pass does not establish that every query meets the ratio limits. Two checks failed: seven of nine valid long-label queries exhausted fuel,
and additional module loads exceeded the 50 ms p95 budget because each load
recompiled the module. Native remains the default; the Wasm provider stays opt-in.

Memory passes an absolute budget, not a strict two-times-native ratio. The runtime
has a material first-use cost and this guest reserves and initializes a roughly
4 MiB transfer buffer even for small requests. Future work should measure smaller
or demand-sized transfer storage as well as compiled-module reuse. The matching
module is smaller than the equivalent native SDK fixture, but that comparison
excludes the host runtime's code size and resident memory.

Before reconsidering adoption, address valid-request fuel exhaustion with a
bounded execution policy that preserves responsiveness, and reuse safely compiled
modules where their bytes and engine configuration match. Keep the thresholds
fixed and rerun the gate after those changes. Do not treat catching a trap and
silently degrading to prefix matching as successful ranking parity.
