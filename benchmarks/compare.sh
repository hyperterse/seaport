#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  benchmarks/compare.sh <command> [args...]

Runs the same command through Seaport and Harbor, ten times by default.
All arguments after the script name are forwarded to both tools.

Examples:
  benchmarks/compare.sh run -d factory-ai/legacy-bench
  benchmarks/compare.sh run -d terminal-bench/terminal-bench-2 -n 32

Environment:
  SEAPORT_BENCH_ITERATIONS   Number of runs per tool. Default: 10
  SEAPORT_BENCH_OUTPUT_DIR   Directory for logs and reports. Default: benchmarks/results
  SEAPORT_BIN                Seaport executable. Default: target/release/seaport, then seaport
  HARBOR_BIN                 Harbor executable. Default: harbor
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ "${1:-}" == "--" ]]; then
  shift
fi

if [[ $# -eq 0 ]]; then
  usage >&2
  exit 2
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ITERATIONS="${SEAPORT_BENCH_ITERATIONS:-10}"
OUTPUT_ROOT="${SEAPORT_BENCH_OUTPUT_DIR:-${ROOT}/benchmarks/results}"
HARBOR_BIN="${HARBOR_BIN:-harbor}"
FORWARDED_ARGS=("$@")

if ! [[ "${ITERATIONS}" =~ ^[1-9][0-9]*$ ]]; then
  echo "SEAPORT_BENCH_ITERATIONS must be a positive integer, got: ${ITERATIONS}" >&2
  exit 2
fi

if [[ -n "${SEAPORT_BIN:-}" ]]; then
  RESOLVED_SEAPORT_BIN="${SEAPORT_BIN}"
elif [[ -x "${ROOT}/target/release/seaport" ]]; then
  RESOLVED_SEAPORT_BIN="${ROOT}/target/release/seaport"
elif command -v seaport >/dev/null 2>&1; then
  RESOLVED_SEAPORT_BIN="$(command -v seaport)"
else
  echo "target/release/seaport was not found; building release binary" >&2
  cargo build --release --locked --manifest-path "${ROOT}/Cargo.toml"
  RESOLVED_SEAPORT_BIN="${ROOT}/target/release/seaport"
fi

if ! command -v "${HARBOR_BIN}" >/dev/null 2>&1; then
  echo "harbor executable was not found: ${HARBOR_BIN}" >&2
  exit 2
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required for timing and statistics" >&2
  exit 2
fi

RUN_ID="compare-$(date -u +%Y%m%dT%H%M%SZ)-$$"
OUTPUT_DIR="${OUTPUT_ROOT%/}/${RUN_ID}"
mkdir -p "${OUTPUT_DIR}"

python3 - "${ITERATIONS}" "${OUTPUT_DIR}" "${ROOT}" "${RESOLVED_SEAPORT_BIN}" "${HARBOR_BIN}" "${FORWARDED_ARGS[@]}" <<'PY'
from __future__ import annotations

import json
import shlex
import statistics
import subprocess
import sys
import time
from pathlib import Path


iterations = int(sys.argv[1])
output_dir = Path(sys.argv[2])
root = Path(sys.argv[3])
seaport_bin = sys.argv[4]
harbor_bin = sys.argv[5]
forwarded_args = sys.argv[6:]


def run_tool(name: str, command: list[str]) -> dict:
    print(f"== {name} ==", flush=True)
    runs = []

    for iteration in range(1, iterations + 1):
        log_path = output_dir / f"{name}-{iteration:02d}.log"
        started = time.perf_counter()

        with log_path.open("w", encoding="utf-8") as log:
            log.write("$ " + " ".join(shlex.quote(part) for part in command) + "\n\n")
            log.flush()
            completed = subprocess.run(
                command,
                cwd=root,
                stdout=log,
                stderr=subprocess.STDOUT,
                text=True,
            )

        elapsed = time.perf_counter() - started
        runs.append(
            {
                "iteration": iteration,
                "seconds": elapsed,
                "returncode": completed.returncode,
                "log": str(log_path),
            }
        )
        print(
            f"{name} run {iteration}/{iterations}: {elapsed:.3f}s rc={completed.returncode}",
            flush=True,
        )

    successful_seconds = [
        run["seconds"] for run in runs if run["returncode"] == 0
    ]

    return {
        "name": name,
        "command": command,
        "runs": runs,
        "success_count": len(successful_seconds),
        "mean_seconds": mean(successful_seconds),
        "stddev_seconds": stddev(successful_seconds),
        "variance_seconds2": variance(successful_seconds),
    }


def mean(values: list[float]) -> float | None:
    return statistics.mean(values) if values else None


def stddev(values: list[float]) -> float | None:
    if not values:
        return None
    return statistics.stdev(values) if len(values) > 1 else 0.0


def variance(values: list[float]) -> float | None:
    if not values:
        return None
    return statistics.variance(values) if len(values) > 1 else 0.0


def seconds(value: float | None) -> str:
    return "n/a" if value is None else f"{value:.3f}s"


def scalar(value: float | None, suffix: str = "") -> str:
    return "n/a" if value is None else f"{value:.3f}{suffix}"


results = [
    run_tool("seaport", [seaport_bin, *forwarded_args]),
    run_tool("harbor", [harbor_bin, *forwarded_args]),
]

comparison = {"comparable": False, "speedup_ratio": None}
seaport_mean = results[0]["mean_seconds"]
harbor_mean = results[1]["mean_seconds"]

if seaport_mean and harbor_mean:
    comparison = {
        "comparable": True,
        "speedup_ratio": harbor_mean / seaport_mean,
    }

report = {
    "iterations": iterations,
    "forwarded_args": forwarded_args,
    "results": results,
    "comparison": comparison,
}

report_path = output_dir / "report.json"
markdown_path = output_dir / "report.md"
report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

lines = [
    "# Seaport vs Harbor Benchmark",
    "",
    f"Iterations: `{iterations}`",
    f"Forwarded args: `{' '.join(shlex.quote(arg) for arg in forwarded_args)}`",
    "",
    "| Tool | Successful runs | Mean | Std dev | Variance |",
    "| --- | ---: | ---: | ---: | ---: |",
]

for result in results:
    lines.append(
        "| {name} | {success_count}/{iterations} | {mean} | {stddev} | {variance} |".format(
            name=result["name"],
            success_count=result["success_count"],
            iterations=iterations,
            mean=seconds(result["mean_seconds"]),
            stddev=seconds(result["stddev_seconds"]),
            variance=scalar(result["variance_seconds2"], "s^2"),
        )
    )

lines.extend(["", "## Comparison", ""])

if comparison["comparable"]:
    lines.append(f"Speedup: `{comparison['speedup_ratio']:.3f}x`")
else:
    lines.append("The commands were not comparable because at least one tool had no successful runs.")

lines.extend(["", "## Logs", ""])

for result in results:
    lines.append(f"### {result['name']}")
    lines.append("")
    for run in result["runs"]:
        lines.append(
            "- run {iteration}: `{seconds:.3f}s`, rc `{returncode}`, log `{log}`".format(
                **run
            )
        )
    lines.append("")

markdown_path.write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8")

print("== summary ==", flush=True)
for result in results:
    print(
        "{name}: successes={success_count}/{iterations} mean={mean} stddev={stddev} variance={variance}".format(
            name=result["name"],
            success_count=result["success_count"],
            iterations=iterations,
            mean=seconds(result["mean_seconds"]),
            stddev=seconds(result["stddev_seconds"]),
            variance=scalar(result["variance_seconds2"], "s^2"),
        ),
        flush=True,
    )

if comparison["comparable"]:
    print(f"speedup={comparison['speedup_ratio']:.3f}x", flush=True)

print(f"report={report_path}", flush=True)
print(f"markdown={markdown_path}", flush=True)
PY
