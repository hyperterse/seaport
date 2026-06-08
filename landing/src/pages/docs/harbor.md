---
layout: ../../layouts/DocsLayout.astro
title: Harbor compatibility
description: Run your existing Harbor tasks and datasets on Seaport, and benchmark the two.
---

# Harbor compatibility

Seaport speaks Harbor's task format. If you already have Harbor tasks and datasets, they run on Seaport unchanged. There is no migration step and no rewrite.

## What carries over

- **Task layout.** The same `instruction.md`, `task.toml`, `environment/`, `solution/`, and `tests/` structure. See [Writing tasks](/docs/tasks).
- **Datasets.** The same `run -d <dataset>` invocation against the same dataset names.
- **Scripts.** Task scripts that use Harbor's container paths, `/app` for the workspace and `/logs/verifier` for verifier output, work as written.

In short, point Seaport at what you already have:

```sh
seaport run -d factory-ai/legacy-bench
```

:::tip
If a script reads `APP_DIR` and `LOGS_DIR` instead of hardcoding `/app` and `/logs/verifier`, it runs identically under both tools and in Seaport's local backend. See the [environment variables](/docs/tasks#environment-variables).
:::

## The difference

Same inputs, same outputs, faster runs. Seaport front-loads the slow Docker work into a parallel preflight phase, caches and reuses environments, and restores workspaces from snapshots. See [Performance](/docs/performance) for how.

## Benchmark them side by side

Seaport ships a benchmark that runs the same command through each tool and reports the comparison. The first argument selects the tool; everything after it is forwarded unchanged.

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

There is also a manual `benchmark` GitHub Actions workflow. Trigger it from the Actions tab with a dataset name, and it runs Seaport and Harbor as separate jobs and uploads both sets of logs.
