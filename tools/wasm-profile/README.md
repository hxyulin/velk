# Actual-provider Wasm adoption gate

This standalone workspace profiles Velk's real `Manager::matching_provider`
leases. It compares a matching-only native SDK fixture, the production Wasm
module and linked matching algorithm. Both plugins use the shared algorithm.
The optional `--native` path repeats the same test against the deployed full
completion plugin, which has additional frontend, reduction and settings code.

Build from the repository root with its pinned Rust toolchain:

```sh
cargo build --release --target wasm32-unknown-unknown -p velk-plugin-completion-wasm --locked
cargo build --release -p velk-plugin-completion --example velk_plugin_completion --locked
cargo build --release --manifest-path tools/wasm-profile/Cargo.toml --workspace --locked --target-dir target/wasm-profile
python3 -m unittest discover -s tools/wasm-profile -p 'test_*.py'
python3 tools/wasm-profile/profile.py
```

The runner exits 1 if any adoption check fails. Raw per-call samples, lifecycle
samples, exact corpus, machine/toolchain, artifact hashes, summaries and limits
are written under `target/wasm-profile/results`, which is ignored. Run without
other compilation or CPU-heavy processes. This is currently supported on macOS
and Linux; the RSS reader uses `ps` resident KiB, not virtual address space.

To profile the deployed native completion implementation on macOS:

```sh
python3 tools/wasm-profile/profile.py --native target/release/examples/libvelk_plugin_completion.dylib --output target/wasm-profile/deployed-results
```

Use `.so` on Linux. `--target-dir` can reuse a different compiled harness target;
`--output` independently controls local results. The root workspace deliberately
keeps this benchmark tooling separate from application targets.

`--analyze-only` recomputes summaries from existing raw samples without measuring
again. It preserves the original measurement metadata and rejects changed
artifact hashes. Normal gates use the pooled workload distributions; per-query
p50/p95/p99 and sample counts are also reported as diagnostics. A cheap query can
have a higher slowdown ratio than the pooled distribution suggests.

## Predeclared checks

- Warm p50 at most 1.5 times native; p95 and p99 at most 2 times native. The
  native denominator has a 1 microsecond floor to avoid unstable submicrosecond
  ratios. These limits apply independently to every normal corpus.
- 1000 ordinary labels: Wasm p99 at most 2 milliseconds.
- First-provider incremental RSS at most native plus 32 MiB. Additional warmed
  providers at most the greater of twice native marginal RSS or 8 MiB/provider,
  evaluated separately at 5 and 10 providers. These use medians of at least 20
  independent process trials with each process's own pre-load RSS subtracted.
- First load p95 at most 250 milliseconds; additional load p95 at most 50
  milliseconds. A fresh engine compiles on first load; later loads reuse the
  engine and compile another module. These are warm-filesystem timings, not
  cold-disk measurements or pure instantiation timings.
- Exact score parity for every query in each normal corpus. Every one of the
  nine bounded long-label stress queries must return correct scores without
  trapping. Missing stress data is an error. The production 50-million-fuel
  limit remains enabled. Current valid long-label requests can fail this check;
  that failure must remain visible before adoption.

The timing loop has 3 trials, 100 warmup rounds per workload and rotating mode
order. It measures 3000/1000/400 calls per trial for 10/100/1000 synthetic labels,
plus 400 calls per trial for up to 1000 deterministic identifiers extracted from
editor-model and completion Rust sources. The repository workload derives exact,
prefix and typo queries from its corpus. Samples are buffered until all timed
loops finish; result destruction happens after each timer stops. Tracing has no
subscriber. Every query is checked for full-vector equality before measurement.

Lifecycle workers are isolated subprocesses. They sample pre-load RSS, warmed
providers 1 through 10, unload and 10 reload/retirement cycles. Providers receive
10 ordinary 1000-label calls before each RSS observation. Native instances use
separate Managers because the native plugin metadata fixes its ID; Wasm instances
share one Manager/engine, matching production. Pre-load Manager/manifest setup is
outside both timing and incremental RSS. The measured extra native Managers are
small but are an acknowledged difference. RSS includes engine, compiled code,
allocator retention and touched guest memory; reserved address space is excluded.
The unload/retirement data are diagnostic and have no leak threshold yet.

Stripped native matching-only and full completion artifacts are reported beside
the release Wasm module. Wasm has no debug info but retains custom name/producer
sections. The host harness executable is stripped and reported separately: it
includes Wasmtime and host services. It is not the incremental cost of Wasmtime;
that requires a comparable host build without the runtime, which this tool does
not provide. A small module alone is insufficient evidence of a smaller total
application. File sizes currently have no adoption gate.

This is a single-service, single-machine experiment. It does not measure UI
scheduling, provider contention, asynchronous imports, large document snapshots,
LSP work or other plugin feature surfaces. Passing it would justify this matching
provider, not migrating every plugin automatically.
