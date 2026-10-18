#!/usr/bin/env python3
"""Measure native and Wasm scaling with shared or distinct module identities."""

import argparse
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
from pathlib import Path
from profile import percentile, rss

ROOT = Path(__file__).resolve().parents[2]
MODES = ["native", "wasm_shared", "wasm_distinct"]
MIB = 1024 * 1024


def unsigned_leb(value):
    result = bytearray()
    while True:
        byte = value & 127
        value >>= 7
        result.append(byte | (128 if value else 0))
        if not value:
            return bytes(result)


def module_variant(source, index):
    name = b"velk.profile.variant"
    payload = unsigned_leb(len(name)) + name + index.to_bytes(4, "little")
    return source + b"\0" + unsigned_leb(len(payload)) + payload


def cpu_ns(measurement):
    return measurement["cpu"]["user_ns"] + measurement["cpu"]["system_ns"]


def summarize(rows):
    groups = {}
    for row in rows:
        groups.setdefault((row["mode"], row["count"], row["trial"]), {})[
            row["phase"]
        ] = row
    samples = {}
    for (mode, count, trial), phases in groups.items():
        if set(phases) != {"baseline", "loaded", "warmed", "runtime", "unloaded"}:
            raise ValueError(f"incomplete worker: {mode}/{count}/{trial}")
        if not phases["warmed"]["parity"]:
            raise ValueError("provider score mismatch")
        samples.setdefault((mode, count), []).append(phases)
    results = []
    for (mode, count), trials in sorted(samples.items()):
        median = statistics.median
        baseline = [p["baseline"]["rss_bytes"] for p in trials]
        warmed = [p["warmed"]["rss_bytes"] for p in trials]
        result = {
            "mode": mode,
            "count": count,
            "trials": len(trials),
            "baseline_rss_mib": median(baseline) / MIB,
            "warmed_rss_mib": median(warmed) / MIB,
            "incremental_rss_mib": median(
                p["warmed"]["rss_bytes"] - p["baseline"]["rss_bytes"] for p in trials
            )
            / MIB,
            "unloaded_incremental_rss_mib": median(
                p["unloaded"]["rss_bytes"] - p["baseline"]["rss_bytes"] for p in trials
            )
            / MIB,
            "load_wall_ms": median(p["loaded"]["load"]["wall_ns"] for p in trials)
            / 1e6,
            "load_cpu_ms": median(cpu_ns(p["loaded"]["load"]) for p in trials) / 1e6,
            "idle_cpu_ms": median(cpu_ns(p["runtime"]["idle"]) for p in trials) / 1e6,
            "idle_core_percent": median(
                100 * cpu_ns(p["runtime"]["idle"]) / p["runtime"]["idle"]["wall_ns"]
                for p in trials
            ),
            "disk_cache_hits": median(
                p["loaded"].get("cache", {}).get("hits", 0) for p in trials
            ),
            "disk_cache_misses": median(
                p["loaded"].get("cache", {}).get("misses", 0) for p in trials
            ),
        }
        for workload in ["single", "fanout"]:
            measured = [p["runtime"][workload] for p in trials]
            if any(
                m["calls"] != len(m["samples_ns"]) or not m["calls"] for m in measured
            ):
                raise ValueError("invalid runtime sample count")
            times = [time for m in measured for time in m["samples_ns"]]
            result[workload] = {
                "p50_us": percentile(times, 0.5) / 1000,
                "p95_us": percentile(times, 0.95) / 1000,
                "p99_us": percentile(times, 0.99) / 1000,
                "cpu_us_per_call": median(cpu_ns(m) / m["calls"] for m in measured)
                / 1000,
                "wall_ms_per_sweep": median(
                    m["wall_ns"] / (m["calls"] / count) for m in measured
                )
                / 1e6
                if workload == "fanout"
                else None,
                "cpu_ms_per_sweep": median(
                    cpu_ns(m) / (m["calls"] / count) for m in measured
                )
                / 1e6
                if workload == "fanout"
                else None,
                "samples": len(times),
            }
        results.append(result)
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path, default=ROOT / "target/wasm-profile")
    parser.add_argument(
        "--output", type=Path, default=ROOT / "target/wasm-profile/scaling"
    )
    parser.add_argument("--counts", type=int, nargs="+", default=[1, 5, 10, 25, 50])
    parser.add_argument("--trials", type=int, default=5)
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--cache", choices=["off", "cold", "warm"], default="off")
    args = parser.parse_args()
    if args.trials < 3 or not args.counts or any(n < 1 or n > 50 for n in args.counts):
        parser.error("use at least three trials and counts from 1 through 50")
    target = args.target_dir.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    artifacts = output / "artifacts"
    artifacts.mkdir(exist_ok=True)
    suffix = {"Darwin": "dylib", "Linux": "so"}.get(platform.system())
    if not suffix:
        parser.error("currently supports macOS and Linux")
    cargo = [
        "cargo",
        "build",
        "--release",
        "--manifest-path",
        str(ROOT / "tools/wasm-profile/Cargo.toml"),
        "--locked",
        "--target-dir",
        str(target),
    ]
    if not args.skip_build:
        with (output / "build.log").open("w") as log:
            subprocess.run(
                [
                    "cargo",
                    "build",
                    "--release",
                    "--target",
                    "wasm32-unknown-unknown",
                    "-p",
                    "velk-plugin-completion-wasm",
                    "--locked",
                ],
                cwd=ROOT,
                stdout=log,
                stderr=subprocess.STDOUT,
                check=True,
            )
            subprocess.run(
                cargo + ["--workspace"],
                stdout=log,
                stderr=subprocess.STDOUT,
                check=True,
            )
            for index in range(max(args.counts)):
                env = dict(
                    os.environ, VELK_PROFILE_PLUGIN_ID=f"velk.matching.scale{index:02}"
                )
                subprocess.run(
                    cargo + ["-p", "velk-matching-native-fixture"],
                    env=env,
                    stdout=log,
                    stderr=subprocess.STDOUT,
                    check=True,
                )
                shutil.copy2(
                    target / f"release/libvelk_matching_native_fixture.{suffix}",
                    artifacts / f"native_{index:02}.{suffix}",
                )
            env = dict(os.environ)
            env.pop("VELK_PROFILE_PLUGIN_ID", None)
            subprocess.run(
                cargo + ["-p", "velk-matching-native-fixture"],
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
                check=True,
            )
        source = (
            ROOT
            / "target/wasm32-unknown-unknown/release/velk_plugin_completion_wasm.wasm"
        ).read_bytes()
        for index in range(max(args.counts)):
            (artifacts / f"module_{index:02}.wasm").write_bytes(
                module_variant(source, index)
            )
    hashes = {
        p.name: hashlib.sha256(p.read_bytes()).hexdigest()
        for p in artifacts.iterdir()
        if p.is_file()
    }
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
        "counts": args.counts,
        "trials": args.trials,
        "sha256": hashes,
        "workload": "1000 short labels, fixed typo query pritn; single=1000 calls, fanout=50 sweeps",
        "distinct_wasm": "same matching module with unique custom sections; distinct digest/cache identity, equivalent code",
        "cache_mode": args.cache,
    }
    executable = target / "release/velk-wasm-profile"
    machine["worker_sha256"] = hashlib.sha256(executable.read_bytes()).hexdigest()
    if platform.system() == "Darwin":
        machine["cpu_model"] = subprocess.check_output(
            ["sysctl", "-n", "machdep.cpu.brand_string"], text=True
        ).strip()
        machine["physical_memory_bytes"] = int(
            subprocess.check_output(["sysctl", "-n", "hw.memsize"], text=True)
        )
    (output / "machine.json").write_text(json.dumps(machine, indent=2) + "\n")
    cache_directory = output / "cache"
    if args.cache == "warm":
        preparation = subprocess.check_output(
            [
                str(executable),
                "scaling",
                "wasm_distinct",
                str(max(args.counts)),
                str(artifacts),
                str(cache_directory),
                "prepare",
            ],
            text=True,
        )
        (output / "preparation.json").write_text(preparation)
    rows = []
    with (output / "raw.jsonl").open("w") as raw:
        for trial in range(args.trials):
            counts = args.counts if trial % 2 == 0 else list(reversed(args.counts))
            for count_index, count in enumerate(counts):
                offset = (trial + count_index) % len(MODES)
                for mode in MODES[offset:] + MODES[:offset]:
                    cache = (
                        "off"
                        if args.cache == "off"
                        else str(
                            cache_directory
                            if args.cache == "warm"
                            else cache_directory / f"{mode}-{count}-{trial}"
                        )
                    )
                    with (output / f"worker-{mode}-{count}-{trial}.log").open(
                        "w"
                    ) as log:
                        process = subprocess.Popen(
                            [
                                str(executable),
                                "scaling",
                                mode,
                                str(count),
                                str(artifacts),
                                cache,
                            ],
                            stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE,
                            stderr=log,
                            text=True,
                        )
                        try:
                            for line in process.stdout:
                                row = json.loads(line)
                                if (
                                    row["phase"] == "loaded"
                                    and mode != "native"
                                    and args.cache != "off"
                                ):
                                    stats = row["cache"]
                                    if not stats["enabled"] or (
                                        args.cache == "warm"
                                        and (stats["misses"] or not stats["hits"])
                                    ):
                                        raise RuntimeError(
                                            "compilation cache did not satisfy the requested profile mode"
                                        )
                                row.update(
                                    mode=mode,
                                    count=count,
                                    trial=trial,
                                    rss_bytes=rss(process.pid),
                                )
                                rows.append(row)
                                raw.write(json.dumps(row) + "\n")
                                raw.flush()
                                process.stdin.write("continue\n")
                                process.stdin.flush()
                            if process.wait(timeout=15):
                                raise RuntimeError(
                                    f"worker failed: {mode}/{count}/{trial}"
                                )
                        finally:
                            if process.poll() is None:
                                process.kill()
                    print(
                        f"Completed {mode}, {count} plugins, trial {trial + 1}",
                        flush=True,
                    )
    summary = {"rows": summarize(rows), "machine": machine}
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(f"Results: {output}")


if __name__ == "__main__":
    main()
