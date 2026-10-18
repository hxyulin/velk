#!/usr/bin/env python3
"""Run real-provider measurements and fail the predeclared adoption gate."""

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MIB = 1024 * 1024


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[round((len(ordered) - 1) * fraction)]


def make_gate(name, actual, maximum, unit):
    return {
        "name": name,
        "actual": actual,
        "maximum": maximum,
        "unit": unit,
        "passed": actual <= maximum,
    }


def relative_latency(native_ns, wasm_ns):
    return wasm_ns / max(native_ns, 1000)


def stress_failures(samples):
    expected = {
        "print",
        "pritn",
        "pront",
        "compleiton",
        "Completion",
        "界接",
        "zzzz",
        "",
        "create_workspace",
    }
    if len(samples) != 9 or {sample["query"] for sample in samples} != expected:
        raise ValueError(
            "stress workload must contain each of the nine required queries exactly once"
        )
    return sum(not sample["ok"] for sample in samples)


def rss(pid):
    # ps reports resident KiB on both macOS and Linux, not virtual address space.
    return (
        int(
            subprocess.check_output(
                ["ps", "-o", "rss=", "-p", str(pid)], text=True
            ).strip()
        )
        * 1024
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path, default=ROOT / "target/wasm-profile")
    parser.add_argument(
        "--native",
        type=Path,
        help="optional deployed native completion library instead of matching-only fixture",
    )
    parser.add_argument(
        "--output", type=Path, default=ROOT / "target/wasm-profile/results"
    )
    parser.add_argument(
        "--analyze-only",
        action="store_true",
        help="recompute summary from an existing output directory without measuring",
    )
    parser.add_argument(
        "--trials",
        type=int,
        default=20,
        help="isolated lifecycle trials per transport (minimum 20)",
    )
    args = parser.parse_args()
    if args.trials < 20:
        parser.error("at least 20 independent process trials are required")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    executable = args.target_dir.resolve() / "release/velk-wasm-profile"
    suffix = {"Darwin": "dylib", "Linux": "so"}.get(platform.system())
    if not suffix:
        parser.error("RSS/artifact fixture currently supports macOS and Linux")
    native = (
        args.native.resolve()
        if args.native
        else args.target_dir.resolve()
        / f"release/libvelk_matching_native_fixture.{suffix}"
    )
    wasm = (
        ROOT / "target/wasm32-unknown-unknown/release/velk_plugin_completion_wasm.wasm"
    )
    corpus_paths = sorted((ROOT / "crates/editor-model/src").rglob("*.rs")) + sorted(
        (ROOT / "plugins/completion/src").rglob("*.rs")
    )
    identifiers = sorted(
        {
            word
            for path in corpus_paths
            for word in re.findall(r"\b[A-Za-z_][A-Za-z_0-9]{3,}\b", path.read_text())
        }
    )
    if len(identifiers) > 1000:
        identifiers = [identifiers[i * len(identifiers) // 1000] for i in range(1000)]
    corpus_path = output / "corpus.json"
    corpus_path.write_text(json.dumps(identifiers) + "\n")
    base = [str(executable), "calls", str(native), str(wasm), str(corpus_path)]
    machine = {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "toolchain": subprocess.check_output(["rustc", "-Vv"], text=True),
        "commit": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
        ).strip(),
        "dirty": bool(
            subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT)
        ),
        "trials": args.trials,
    }
    if args.analyze_only:
        machine = json.loads((output / "machine.json").read_text())
    machine["logical_cpus"] = os.cpu_count()
    if platform.system() == "Darwin":
        machine["cpu"] = subprocess.check_output(
            ["sysctl", "-n", "machdep.cpu.brand_string"], text=True
        ).strip()
        machine["physical_memory_bytes"] = int(
            subprocess.check_output(["sysctl", "-n", "hw.memsize"], text=True)
        )
    (output / "machine.json").write_text(json.dumps(machine, indent=2) + "\n")
    if not args.analyze_only:
        with (output / "calls.jsonl").open("w") as raw:
            subprocess.run(base, stdout=raw, check=True, timeout=300)
    calls = [
        json.loads(line) for line in (output / "calls.jsonl").read_text().splitlines()
    ]
    lifecycle = (
        json.loads((output / "lifecycle.json").read_text()) if args.analyze_only else []
    )
    for trial in range(0 if args.analyze_only else args.trials):
        for mode in ["native", "wasm"] if trial % 2 == 0 else ["wasm", "native"]:
            process = subprocess.Popen(
                [str(executable), "lifecycle", str(native), str(wasm), mode],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                text=True,
            )
            try:
                for line in process.stdout:
                    sample = json.loads(line)
                    sample.update(mode=mode, trial=trial, rss_bytes=rss(process.pid))
                    lifecycle.append(sample)
                    process.stdin.write("continue\n")
                    process.stdin.flush()
                if process.wait(timeout=10):
                    raise RuntimeError("lifecycle worker failed")
            finally:
                if process.poll() is None:
                    process.kill()
    (output / "lifecycle.json").write_text(json.dumps(lifecycle, indent=2) + "\n")
    checks = []

    def gate(name, actual, maximum, unit):
        checks.append(make_gate(name, actual, maximum, unit))

    distributions = []
    for dataset, count in [
        ("synthetic", 10),
        ("synthetic", 100),
        ("synthetic", 1000),
        ("repository", len(identifiers)),
    ]:
        for fraction, limit in [(0.5, 1.5), (0.95, 2), (0.99, 2)]:
            times = {
                mode: percentile(
                    [
                        s["ns"]
                        for s in calls
                        if s.get("dataset") == dataset
                        and s.get("count") == count
                        and s.get("mode") == mode
                    ],
                    fraction,
                )
                for mode in ["linked", "native", "wasm"]
            }
            distributions.append(
                {"dataset": dataset, "count": count, "percentile": fraction, **times}
            )
            gate(
                f"{dataset} {count} labels p{int(fraction * 100)} ratio",
                relative_latency(times["native"], times["wasm"]),
                limit,
                "ratio (native denominator floor 1us)",
            )
            if count == 1000 and fraction == 0.99:
                gate(f"{dataset} 1000 labels p99", times["wasm"], 2_000_000, "ns")
    per_query = []
    for dataset, count in [
        ("synthetic", 10),
        ("synthetic", 100),
        ("synthetic", 1000),
        ("repository", len(identifiers)),
    ]:
        for query in range(9):
            for fraction in [0.5, 0.95, 0.99]:
                samples = {
                    mode: [
                        s["ns"]
                        for s in calls
                        if s.get("dataset") == dataset
                        and s.get("count") == count
                        and s.get("query") == query
                        and s.get("mode") == mode
                    ]
                    for mode in ["native", "wasm"]
                }
                times = {
                    mode: percentile(values, fraction)
                    for mode, values in samples.items()
                }
                per_query.append(
                    {
                        "dataset": dataset,
                        "count": count,
                        "query_index": query,
                        "percentile": fraction,
                        "samples_per_mode": {
                            mode: len(values) for mode, values in samples.items()
                        },
                        "ns": times,
                        "ratio": relative_latency(times["native"], times["wasm"]),
                    }
                )
    stress = [s for s in calls if s.get("kind") == "stress"]
    gate(
        "bounded long-label correctness/traps",
        stress_failures(stress),
        0,
        "failed queries",
    )
    memory = {}
    for mode in ["native", "wasm"]:
        rows = [s for s in lifecycle if s["mode"] == mode]
        baselines = {
            s["trial"]: s["rss_bytes"] for s in rows if s["phase"] == "baseline"
        }
        increments = {
            count: [
                s["rss_bytes"] - baselines[s["trial"]]
                for s in rows
                if s["phase"] == "loaded" and s["count"] == count
            ]
            for count in [1, 5, 10]
        }
        memory[mode] = {
            str(count): {
                "median": statistics.median(values),
                "p95": percentile(values, 0.95),
                "samples": values,
            }
            for count, values in increments.items()
        }
        first = [
            s["load_ns"] for s in rows if s["phase"] == "loaded" and s["count"] == 1
        ]
        additional = [
            s["load_ns"] for s in rows if s["phase"] == "loaded" and s["count"] > 1
        ]
        if mode == "wasm":
            gate("first Wasm load p95", percentile(first, 0.95), 250_000_000, "ns")
            gate(
                "additional Wasm load p95",
                percentile(additional, 0.95),
                50_000_000,
                "ns",
            )
        memory[mode]["startup_ns"] = {
            "first_p95": percentile(first, 0.95),
            "additional_p95": percentile(additional, 0.95),
        }
        retired = [
            s["rss_bytes"] - baselines[s["trial"]]
            for s in rows
            if s["phase"] == "retired" and s["cycle"] == 10
        ]
        unloaded = [
            s["rss_bytes"] - baselines[s["trial"]]
            for s in rows
            if s["phase"] == "unloaded"
        ]
        memory[mode]["unloaded_median"] = statistics.median(unloaded)
        memory[mode]["ten_retirement_cycles_median"] = statistics.median(retired)
    gate(
        "first provider resident memory",
        memory["wasm"]["1"]["median"],
        memory["native"]["1"]["median"] + 32 * MIB,
        "bytes (median baseline-subtracted RSS)",
    )
    for count in [5, 10]:
        wasm_marginal = (
            memory["wasm"][str(count)]["median"] - memory["wasm"]["1"]["median"]
        ) / (count - 1)
        native_marginal = max(
            0,
            (memory["native"][str(count)]["median"] - memory["native"]["1"]["median"])
            / (count - 1),
        )
        gate(
            f"marginal providers 2..{count} resident memory",
            wasm_marginal,
            max(2 * native_marginal, 8 * MIB),
            "bytes/provider (8MiB noise floor)",
        )
    artifacts = {}
    prior_artifacts = (
        json.loads((output / "summary.json").read_text())["artifacts"]
        if args.analyze_only
        else {}
    )
    for name, path in [
        ("native_deployed_override" if args.native else "native_matching_only", native),
        ("wasm_matching_only", wasm),
        (
            "native_full_completion",
            ROOT / f"target/release/examples/libvelk_plugin_completion.{suffix}",
        ),
        ("host_profile_with_wasmtime", executable),
    ]:
        if not path.exists():
            artifacts[name] = {"available": False}
            continue
        artifact_hash = hashlib.sha256(path.read_bytes()).hexdigest()
        if (
            name in prior_artifacts
            and prior_artifacts[name].get("sha256") != artifact_hash
        ):
            raise ValueError(
                f"{name} changed since measurement; rerun instead of reanalyzing"
            )
        copy = output / path.name
        shutil.copy2(path, copy)
        stripped = False
        if path.suffix in [".dylib", ".so"]:
            subprocess.run(
                ["strip", "-x" if suffix == "dylib" else "--strip-unneeded", str(copy)],
                check=True,
            )
            stripped = True
        artifacts[name] = {
            "sha256": artifact_hash,
            "original_bytes": path.stat().st_size,
            "measured_bytes": copy.stat().st_size,
            "stripped": stripped or path == executable,
            "notes": "Wasm release emits no debug info; custom name/producer sections retained"
            if path == wasm
            else "",
        }
        copy.unlink()
    summary = {
        "passed": all(c["passed"] for c in checks),
        "checks": checks,
        "latency_ns": distributions,
        "per_query_diagnostics": per_query,
        "memory": memory,
        "artifacts": artifacts,
        "limits": [
            "RSS is ps resident memory, baseline-subtracted per isolated process; allocator/page-cache noise remains",
            "first-load includes fresh engine and compile with warm filesystem, not a cold OS cache",
            "additional Wasm loads reuse engine but recompile each module; no compiled module cache",
            "native instances use independent Managers because native metadata fixes ID; Wasm instances share one Manager/engine as production",
            "host executable includes Wasmtime and other host services; incremental Wasmtime executable size is unavailable without a no-runtime build",
            "single machine synthetic matching service; not evidence all plugin services fit Wasm",
            "stress correctness and traps gate adoption; stress timings are first-call diagnostics, not warm distributions",
        ],
    }
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    for check in checks:
        print(
            f"{'PASS' if check['passed'] else 'FAIL'} {check['name']}: {check['actual']:.3f} <= {check['maximum']:.3f} {check['unit']}"
        )
    print(f"Raw results: {output}")
    raise SystemExit(0 if summary["passed"] else 1)


if __name__ == "__main__":
    main()
