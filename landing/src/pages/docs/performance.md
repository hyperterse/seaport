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

## Benchmarking against Harbor

Seaport ships a head-to-head benchmark that runs the same command through both tools. See [Harbor compatibility](/docs/harbor) for the full walkthrough.

```sh
benchmarks/benchmark.sh seaport run -d factory-ai/legacy-bench
benchmarks/benchmark.sh harbor  run -d factory-ai/legacy-bench
```

Each writes per-run logs plus `report.json` and `report.md` under `benchmarks/results/`. There is also a manual `benchmark` GitHub Actions workflow that runs both tools on a dataset you choose.
