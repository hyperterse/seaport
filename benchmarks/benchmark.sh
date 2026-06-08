#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  benchmarks/benchmark.sh <seaport|harbor> <command> [args...]

Runs one benchmark target repeatedly. The first argument selects the tool.
All remaining arguments are forwarded to that tool unchanged.

Examples:
  benchmarks/benchmark.sh seaport run -d factory-ai/legacy-bench
  benchmarks/benchmark.sh harbor run -d factory-ai/legacy-bench
  benchmarks/benchmark.sh seaport run -d terminal-bench/terminal-bench-2 -n 32

Environment:
  SEAPORT_BENCH_ITERATIONS   Number of runs. Default: 10
  SEAPORT_BENCH_OUTPUT_DIR   Directory for logs and reports. Default: benchmarks/results
  SEAPORT_BIN                Seaport executable. Default: target/release/seaport, then seaport
  HARBOR_BIN                 Harbor executable. Default: harbor
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ $# -lt 2 ]]; then
  usage >&2
  exit 2
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOL="$1"
shift
FORWARDED_ARGS=("$@")
ITERATIONS="${SEAPORT_BENCH_ITERATIONS:-10}"
OUTPUT_ROOT="${SEAPORT_BENCH_OUTPUT_DIR:-${ROOT}/benchmarks/results}"

if ! [[ "${ITERATIONS}" =~ ^[1-9][0-9]*$ ]]; then
  echo "SEAPORT_BENCH_ITERATIONS must be a positive integer, got: ${ITERATIONS}" >&2
  exit 2
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required for timing and statistics" >&2
  exit 2
fi

case "${TOOL}" in
  seaport)
    if [[ -n "${SEAPORT_BIN:-}" ]]; then
      TOOL_BIN="${SEAPORT_BIN}"
    elif [[ -x "${ROOT}/target/release/seaport" ]]; then
      TOOL_BIN="${ROOT}/target/release/seaport"
    elif command -v seaport >/dev/null 2>&1; then
      TOOL_BIN="$(command -v seaport)"
    else
      echo "target/release/seaport was not found; building release binary" >&2
      cargo build --release --locked --manifest-path "${ROOT}/Cargo.toml"
      TOOL_BIN="${ROOT}/target/release/seaport"
    fi
    ;;
  harbor)
    TOOL_BIN="${HARBOR_BIN:-harbor}"
    if ! command -v "${TOOL_BIN}" >/dev/null 2>&1; then
      echo "harbor executable was not found: ${TOOL_BIN}" >&2
      exit 2
    fi
    ;;
  *)
    echo "unknown benchmark tool: ${TOOL}; expected seaport or harbor" >&2
    exit 2
    ;;
esac

RUN_ID="benchmark-${TOOL}-$(date -u +%Y%m%dT%H%M%SZ)-$$"
OUTPUT_DIR="${OUTPUT_ROOT%/}/${RUN_ID}"
mkdir -p "${OUTPUT_DIR}"

python3 - "${TOOL}" "${TOOL_BIN}" "${ITERATIONS}" "${OUTPUT_DIR}" "${ROOT}" "${FORWARDED_ARGS[@]}" <<'PY'
from __future__ import annotations

import json
import shlex
import statistics
import subprocess
import sys
import time
from pathlib import Path


tool = sys.argv[1]
tool_bin = sys.argv[2]
iterations = int(sys.argv[3])
output_dir = Path(sys.argv[4])
root = Path(sys.argv[5])
forwarded_args = sys.argv[6:]
command = [tool_bin, *forwarded_args]


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


print(f"== {tool} ==", flush=True)
runs = []

for iteration in range(1, iterations + 1):
    log_path = output_dir / f"{tool}-{iteration:02d}.log"
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
        f"{tool} run {iteration}/{iterations}: {elapsed:.3f}s rc={completed.returncode}",
        flush=True,
    )

successful_seconds = [run["seconds"] for run in runs if run["returncode"] == 0]
report = {
    "tool": tool,
    "iterations": iterations,
    "forwarded_args": forwarded_args,
    "command": command,
    "runs": runs,
    "success_count": len(successful_seconds),
    "mean_seconds": mean(successful_seconds),
    "stddev_seconds": stddev(successful_seconds),
    "variance_seconds2": variance(successful_seconds),
}

report_path = output_dir / "report.json"
markdown_path = output_dir / "report.md"
report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

lines = [
    f"# {tool.capitalize()} Benchmark",
    "",
    f"Iterations: `{iterations}`",
    f"Forwarded args: `{' '.join(shlex.quote(arg) for arg in forwarded_args)}`",
    "",
    "| Tool | Successful runs | Mean | Std dev | Variance |",
    "| --- | ---: | ---: | ---: | ---: |",
    "| {tool} | {success_count}/{iterations} | {mean} | {stddev} | {variance} |".format(
        tool=tool,
        success_count=report["success_count"],
        iterations=iterations,
        mean=seconds(report["mean_seconds"]),
        stddev=seconds(report["stddev_seconds"]),
        variance=scalar(report["variance_seconds2"], "s^2"),
    ),
    "",
    "## Logs",
    "",
]

for run in runs:
    lines.append(
        "- run {iteration}: `{seconds:.3f}s`, rc `{returncode}`, log `{log}`".format(
            **run
        )
    )

markdown_path.write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8")

print("== summary ==", flush=True)
print(
    "{tool}: successes={success_count}/{iterations} mean={mean} stddev={stddev} variance={variance}".format(
        tool=tool,
        success_count=report["success_count"],
        iterations=iterations,
        mean=seconds(report["mean_seconds"]),
        stddev=seconds(report["stddev_seconds"]),
        variance=scalar(report["variance_seconds2"], "s^2"),
    ),
    flush=True,
)
print(f"report={report_path}", flush=True)
print(f"markdown={markdown_path}", flush=True)
PY
