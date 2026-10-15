#!/usr/bin/env python3
"""Normalize Astrelis stage logs or summarize responsiveness CSV samples."""
import argparse
import csv
import math
import re
import sys
from collections import defaultdict


def native(args):
    rows = []
    with open(args.path) as source:
        for line in source:
            if not line.startswith("Astrelis frame:"):
                continue
            fields = dict(re.findall(r"(\w+)=(Some\([^)]+\)|None|[\d.]+)", line))
            frame = int(fields["frame"])
            if args.first_frame <= frame <= args.last_frame:
                rows.append({"capture": args.capture, **{
                    key: "" if value == "None" else value.removeprefix("Some(").removesuffix(")")
                    for key, value in fields.items()
                }})
    if not rows:
        raise SystemExit("No native frames matched the requested range")
    writer = csv.DictWriter(sys.stdout, fieldnames=list(rows[0]), lineterminator="\n")
    writer.writeheader()
    writer.writerows(rows)


def summary(args):
    groups = defaultdict(lambda: defaultdict(list))
    with open(args.path) as source:
        for row in csv.DictReader(source):
            if args.input_only and row.get("inputs") == "0":
                continue
            group = "/".join(row[key] for key in ("variant", "capture", "case") if key in row)
            for metric, value in row.items():
                if metric.endswith("_ms") and value:
                    groups[group][metric].append(float(value))
    writer = csv.writer(sys.stdout, lineterminator="\n")
    writer.writerow(["group", "metric", "samples", "p50_ms", "p95_ms", "p99_ms", "max_ms"])
    for group, metrics in groups.items():
        for metric, values in metrics.items():
            values.sort()
            percentile = lambda q: values[max(0, math.ceil(q * len(values)) - 1)]
            writer.writerow([group, metric, len(values), *[
                f"{value:.6f}" for value in [percentile(.5), percentile(.95), percentile(.99), values[-1]]
            ]])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    parse = commands.add_parser("native", help="convert native log frames to CSV")
    parse.add_argument("path")
    parse.add_argument("--capture", required=True)
    parse.add_argument("--first-frame", type=int, default=0)
    parse.add_argument("--last-frame", type=int, default=sys.maxsize)
    summarize = commands.add_parser("summary", help="summarize numeric timing columns")
    summarize.add_argument("path")
    summarize.add_argument("--input-only", action="store_true", help="exclude native frames without invalidating input")
    args = parser.parse_args()
    (native if args.command == "native" else summary)(args)


if __name__ == "__main__":
    main()
