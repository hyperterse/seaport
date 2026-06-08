---
layout: ../../layouts/DocsLayout.astro
title: Performance
description: How Seaport keeps runs fast, and how to benchmark it.
---

# Performance

Seaport is built to be Harbor compatible and faster on the same work. The speed comes from the execution layer doing the slow, repeatable parts once instead of every time.

## Preflight, then execute

A run is split into two phases.

The **preflight** phase resolves every task source, then pulls and builds every environment up front, several at a time. Because it runs before any agent, all the slow Docker work is front-loaded and parallel rather than scattered through the run.

The **execution** phase then spends its time on agents and verifiers, not setup. Trials run across a worker pool sized to your machine.

:::note
A preflight failure is never fatal. If an image fails to build or pull, that task is simply retried during execution, so a single flaky environment does not stop the run.
:::

## What makes it fast

- **Cached task environments.** Built and pulled images are cached and reused across trials, attempts, and runs. The second run of a task skips the build entirely.
- **Deduplicated pulls.** Tasks that share a base image pull it once, not once per task.
- **Managed BuildKit builder.** Dockerfiles build on a managed BuildKit builder, on the native platform, with streamed progress.
- **Workspace snapshots.** Between attempts, Seaport restores a fresh workspace from a snapshot instead of rebuilding it from scratch.
- **Longest-first scheduling.** Trials that look slow, such as ones that compile, are scheduled first so they are not the long pole at the end.
- **Adaptive concurrency.** The default worker count adapts to the machine's available parallelism. Override it any time with `-n`.

## Tuning a run

| Flag | Effect |
| --- | --- |
| `-n <count>` | Worker count for the execution phase. Defaults to the machine's parallelism, clamped to 1 through 16. |
| `-k <count>` | Attempts per task. Higher values measure consistency, at a linear cost in trials. |
| `-l <count>` | Cap the number of tasks discovered, handy for a quick smoke run. |

Preflight runs at most four environments in parallel regardless of `-n`, since it is bound by Docker and the network rather than CPU.

## Benchmark results

The same command, run through both tools on the same machine. Two shapes tell the story: a tiny oracle task that isolates pure harness overhead, and a full legacy dataset where real task work dominates.

| Benchmark | Seaport | Harbor | Speedup |
| --- | ---: | ---: | ---: |
| Oracle task (harness overhead) | **1.67s** | 15.99s | **9.6×** |
| `factory-ai/legacy-bench`, full dataset | **54.7s** | 100.7s | **1.8×** |

Mean wall-clock per run. Measured on macOS arm64, Docker 29.4.0 via OrbStack, Harbor 0.13.1, against Seaport's hardened Docker backend.

:::note
The oracle task is intentionally tiny, so it measures harness overhead, not model quality or image complexity. That is where Seaport pulls furthest ahead. On a full dataset the agent and verifier work is shared by both tools, so the gap narrows to the part Seaport actually controls. Numbers depend on cache warmth and your machine, so run them yourself.
:::

## Benchmark them side by side

Seaport ships a harness that runs the same command through each tool and reports the comparison. The first argument selects the tool; everything after it is forwarded unchanged.

```sh
benchmarks/benchmark.sh seaport run -d factory-ai/legacy-bench
benchmarks/benchmark.sh harbor  run -d factory-ai/legacy-bench
```

By default it runs each tool ten times and writes per-run logs plus `report.json` and `report.md` under `benchmarks/results/`. A few environment variables tune it:

| Variable | Effect |
| --- | --- |
| `SEAPORT_BENCH_ITERATIONS` | Runs per tool. Defaults to `10`. |
| `SEAPORT_BENCH_OUTPUT_DIR` | Where reports are written. |
| `SEAPORT_BIN` | Path to the Seaport binary. |
| `HARBOR_BIN` | Path to the Harbor binary. |

:::note
The report only declares Seaport faster when both tools complete at least one successful run on the same task. If Harbor is not installed, Docker is unavailable, or the task fails, it records that instead of fabricating a comparison.
:::

There is also a manual `benchmark` GitHub Actions workflow that runs Seaport and Harbor as separate jobs on a dataset you choose, and uploads both sets of logs.
