#!/usr/bin/env python3
"""Run Seaport and Harbor on the same oracle task and report timings."""

from __future__ import annotations

import argparse
import json
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_TASK = ROOT / "benchmarks" / "tasks" / "basic-oracle"
DEFAULT_RESULTS = ROOT / "benchmarks" / "results" / "latest.json"


@dataclass
class CommandResult:
    name: str
    command: list[str]
    durations_sec: list[float]
    successes: int
    failures: list[str]

    @property
    def mean_sec(self) -> float | None:
        if not self.durations_sec:
            return None
        return statistics.mean(self.durations_sec)


def main() -> int:
    args = parse_args()
    task = args.task.resolve()
    output = args.output.resolve()
    iterations = args.iterations

    if iterations < 1:
        print("--iterations must be at least 1", file=sys.stderr)
        return 2

    if not task.is_dir():
        print(f"task directory does not exist: {task}", file=sys.stderr)
        return 2

    seaport_bin = resolve_seaport(args.seaport_bin)
    harbor_bin = shutil.which(args.harbor_bin) if args.harbor_bin else None
    results = []

    results.append(run_seaport(seaport_bin, task, iterations))

    if harbor_bin:
        results.append(run_harbor(harbor_bin, task, iterations))
    else:
        results.append(
            CommandResult(
                name="harbor",
                command=[args.harbor_bin or "harbor", "run", "-p", str(task), "-a", "oracle"],
                durations_sec=[],
                successes=0,
                failures=["harbor binary was not found on PATH"],
            )
        )

    report = build_report(task, iterations, results)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n")
    output.with_suffix(".md").write_text(markdown_report(report))

    print(markdown_report(report))
    print(f"\nWrote {output}")
    print(f"Wrote {output.with_suffix('.md')}")

    return 0 if comparable(report) else 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--task", type=Path, default=DEFAULT_TASK)
    parser.add_argument("--iterations", type=int, default=5)
    parser.add_argument("--output", type=Path, default=DEFAULT_RESULTS)
    parser.add_argument("--seaport-bin", type=Path)
    parser.add_argument("--harbor-bin", default="harbor")
    return parser.parse_args()


def resolve_seaport(explicit: Path | None) -> Path:
    if explicit:
        return explicit.resolve()

    binary = ROOT / "target" / "release" / "seaport"
    subprocess.run(["cargo", "build", "--release", "--locked"], cwd=ROOT, check=True)

    return binary


def run_seaport(binary: Path, task: Path, iterations: int) -> CommandResult:
    with tempfile.TemporaryDirectory(prefix="seaport-bench-") as temporary:
        jobs_dir = Path(temporary) / "jobs"
        command = [
            str(binary),
            "run",
            "-p",
            str(task),
            "-a",
            "oracle",
            "--jobs-dir",
            str(jobs_dir),
        ]
        return run_command("seaport", command, iterations, cwd=ROOT)


def run_harbor(binary: str, task: Path, iterations: int) -> CommandResult:
    with tempfile.TemporaryDirectory(prefix="harbor-bench-") as temporary:
        command = [binary, "run", "-p", str(task), "-a", "oracle"]
        return run_command("harbor", command, iterations, cwd=Path(temporary))


def run_command(name: str, command: list[str], iterations: int, cwd: Path) -> CommandResult:
    durations = []
    failures = []

    for index in range(iterations):
        started = time.perf_counter()
        completed = subprocess.run(
            command,
            cwd=cwd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        elapsed = time.perf_counter() - started

        if completed.returncode == 0:
            durations.append(elapsed)
        else:
            failures.append(
                "\n".join(
                    [
                        f"iteration {index + 1} exited {completed.returncode}",
                        "stdout:",
                        completed.stdout.strip(),
                        "stderr:",
                        completed.stderr.strip(),
                    ]
                )
            )

    return CommandResult(
        name=name,
        command=command,
        durations_sec=durations,
        successes=len(durations),
        failures=failures,
    )


def build_report(task: Path, iterations: int, results: list[CommandResult]) -> dict:
    serialized = [serialize_result(result) for result in results]
    by_name = {result["name"]: result for result in serialized}
    seaport = by_name.get("seaport", {})
    harbor = by_name.get("harbor", {})
    seaport_mean = seaport.get("mean_sec")
    harbor_mean = harbor.get("mean_sec")
    speedup = None

    if seaport_mean and harbor_mean:
        speedup = harbor_mean / seaport_mean

    return {
        "task": str(task),
        "iterations_requested": iterations,
        "results": serialized,
        "comparison": {
            "comparable": speedup is not None,
            "seaport_faster": speedup is not None and speedup > 1.0,
            "speedup_ratio": speedup,
        },
    }


def serialize_result(result: CommandResult) -> dict:
    return {
        "name": result.name,
        "command": result.command,
        "successes": result.successes,
        "failures": result.failures,
        "durations_sec": result.durations_sec,
        "mean_sec": result.mean_sec,
    }


def markdown_report(report: dict) -> str:
    lines = [
        "# Seaport vs Harbor Benchmark",
        "",
        f"Task: `{report['task']}`",
        f"Iterations requested: `{report['iterations_requested']}`",
        "",
        "| Tool | Successful runs | Mean seconds |",
        "| --- | ---: | ---: |",
    ]

    for result in report["results"]:
        mean = result["mean_sec"]
        mean_text = f"{mean:.6f}" if mean is not None else "n/a"
        lines.append(f"| {result['name']} | {result['successes']} | {mean_text} |")

    comparison = report["comparison"]
    lines.extend(["", "## Comparison", ""])

    if comparison["comparable"]:
        ratio = comparison["speedup_ratio"]
        verdict = "yes" if comparison["seaport_faster"] else "no"
        lines.append(f"Seaport faster: `{verdict}`")
        lines.append(f"Speedup ratio: `{ratio:.3f}x`")
    else:
        lines.append("The tools were not comparable in this run.")

    lines.extend(["", "## Commands", ""])

    for result in report["results"]:
        lines.append(f"### {result['name']}")
        lines.append("")
        lines.append("```sh")
        lines.append(" ".join(shell_quote(part) for part in result["command"]))
        lines.append("```")
        lines.append("")
        if result["failures"]:
            lines.append("Failures:")
            lines.append("")
            for failure in result["failures"]:
                lines.append("```text")
                lines.append(failure)
                lines.append("```")
                lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def comparable(report: dict) -> bool:
    return bool(report["comparison"]["comparable"])


def shell_quote(value: str) -> str:
    if all(character.isalnum() or character in "/._:-" for character in value):
        return value
    return "'" + value.replace("'", "'\"'\"'") + "'"


if __name__ == "__main__":
    raise SystemExit(main())
