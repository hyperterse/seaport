---
layout: ../../layouts/DocsLayout.astro
title: Performance
description: How Seaport keeps runs fast, and how to benchmark it.
---

# Performance

Seaport is built to be Harbor compatible and faster on the same work. The speed comes from the execution layer doing the slow, repeatable parts once and reusing them, and from a lean per-trial container instead of a heavy harness.

## On-demand environments

There is no preflight barrier. Each trial pulls or builds its own environment on demand and runs in a single long-lived container. Identical image builds and pulls are deduplicated — concurrent identical builds serialize and share the result — so tasks that share an image only pay for it once. The first trial starts as soon as its image is ready, even on a cold cache, rather than waiting for every other environment to finish.

:::note
A build or pull failure is never fatal to the whole run. With `--max-retries`, an errored trial is retried up to that many times, so a single flaky environment does not stop everything.
:::

## What makes it fast

- **Cached task environments.** Built and pulled images are cached and reused across trials, attempts, and runs. The second run of a task skips the build entirely.
- **Deduplicated builds and pulls.** Tasks that share an image pull or build it once, not once per task.
- **Managed BuildKit builder.** Dockerfiles build on a managed BuildKit builder, on the native platform, with streamed progress.
- **Lean per-trial container.** Each trial runs the agent and verifier in one container via `docker exec`, with no Python import overhead per phase, so the harness itself stays out of the way.
- **Longest-first scheduling.** Trials that look slow, such as ones that compile, are scheduled first so they are not the long pole at the end.
- **Conservative default concurrency.** Trials are heavy and often run emulated containers, so the default worker count is about a third of the host's CPUs, clamped to 2 through 16. Override it any time with `-n`.

## Tuning a run

| Flag | Effect |
| --- | --- |
| `-n <count>` | Number of trials run concurrently. Defaults to about `host_cpus / 3`, clamped to 2 through 16. |
| `-k <count>` | Attempts per task. Higher values measure consistency, at a linear cost in trials. |
| `-l <count>` | Cap the number of tasks discovered, handy for a quick smoke run. |
| `--strict-resources` | Enforce each task's declared `cpus`/`memory_mb` exactly, matching Harbor. By default each trial instead gets a fair share of host CPUs. |

By default each trial gets a fair share of the host's CPUs (`host_cpus / concurrency`, clamped and capped at 8) rather than the task's declared `cpus`; memory stays at the task's declared `memory_mb`. Pass `--strict-resources` to enforce the task's exact `cpus` and `memory_mb` instead.

## Benchmark results

Three datasets of increasing size, run through both tools on the same machine. The pattern is consistent: as real task work grows, the wall-clock gap narrows toward the part Seaport actually controls — but the memory advantage stays flat at roughly **8× lighter** the whole way up, because a Seaport run is one small Rust process while a Harbor run carries a Python interpreter.

| Dataset | Tasks | Seaport wall | Harbor wall | Faster | Seaport RSS | Harbor RSS | Lighter | CPU time (S → H) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `basic-oracle` (harness overhead) | 1 | **0.40s** | 16.67s | **41×** | **28 MB** | 231 MB | **8.3×** | 0.07s → 2.07s |
| `factory-ai/legacy-bench` | 10 | **39.0s** | 60.8s | **1.6×** | **28 MB** | 232 MB | **8.2×** | 0.9s → 11.1s |
| `terminal-bench/terminal-bench-2` | 89 | **102 min** | 150 min | **1.5×** | **28 MB** | 234 MB | **8.3×** | 24s → 79s |

Wall-clock is the mean across iterations (oracle ×8, legacy-bench ×3, terminal-bench-2 ×1), each after a discarded warm-up except terminal-bench-2, which is a single cold pass of all 89 tasks. Measured on macOS arm64, Docker via OrbStack, Harbor 0.13.1, against Seaport's Docker backend.

### Harness overhead, in detail

The oracle task is a single trivial task, so it isolates pure harness overhead — startup, orchestration, memory, CPU — with no real image or agent work to share. This is where a static Rust binary pulls furthest ahead of a Python CLI.

| Factor | Seaport | Harbor | Seaport advantage |
| --- | ---: | ---: | ---: |
| Wall-clock (mean) | **0.40s** | 16.67s | **41× faster** |
| Peak memory (RSS) | **28 MB** | 231 MB | **8.3× lighter** |
| CPU time consumed | **0.07s** | 2.07s | **29× less** |
| Instructions retired | **0.05 B** | 12.7 B | **262× fewer** |
| Cold first run | **4.7s** | 13.5s | **2.8× faster** |
| Install footprint | **2 MB** | 647 MB | **411× smaller** |

### Caveats

These numbers measure harness overhead, not model quality or image complexity. The oracle agent just replays a known solution, so the agent and verifier work — which both tools hand to the same Docker daemon — is as small as it gets on the oracle task and dominant on terminal-bench-2. That is why the speedup slides from 41× to ~1.5× as the dataset grows, while memory stays pinned near 8× lighter throughout: the per-run cost Seaport removes is fixed harness weight, not the shared container work.

Clean-exit counts reflect exit-code semantics, not per-task pass rates — Seaport returns non-zero if any task fails, while Harbor returns success regardless. On terminal-bench-2's 89-task cold pass at least one task failed under Seaport's oracle, so its run exited non-zero; the timing and memory samples are still valid.

Numbers depend on cache warmth and your machine, so run them yourself.

## What the numbers mean

Every figure above is glossary-defined here, with a plain-language gloss so you do not need to be a systems engineer to read the table.

| Term | What it measures | In plain words |
| --- | --- | --- |
| **Wall-clock** | Real elapsed time for one run, like a stopwatch from start to finish. | How long you actually wait. |
| **Mean / Median** | Mean is the average of all runs; median is the middle run once they are sorted. | The "typical" time. The median shrugs off one weird slow run. |
| **Min / Max** | The fastest and slowest single run. | Best case and worst case. |
| **Std dev (σ)** | How much the runs vary around the mean. | Small means steady and predictable; large means jumpy. |
| **Peak memory (RSS)** | The most live memory the CLI process held at once (resident set size). | The biggest the program got while running. Smaller is lighter on your machine. |
| **Peak footprint** | macOS's high-water mark of total memory the process touched. | Another "how much RAM did it grab" number. |
| **CPU time consumed** | Processor time used (user + system), summed across cores. | How much actual work the program made the CPU do. Less is more efficient. |
| **CPU utilisation** | CPU time divided by wall-clock. | How busy the CPU was. Low means it spent most of the time waiting on Docker, not computing. |
| **Instructions retired** | Count of CPU instructions actually executed (an Apple Silicon hardware counter). | Literally how many tiny steps the chip ran. Fewer is less work. |
| **Cold first run** | The very first run, before any image is cached. | How long the first run from scratch takes, image build and all. |
| **Warm-up / steady-state** | A discarded first run that warms the caches; the rest are "steady-state". | We throw away the first slow attempt and measure the normal, repeated speed. |
| **Install footprint** | Disk size of the installed tool. | How much space it eats. Seaport is one small file; Harbor is a whole Python environment. |
| **Clean exits** | How many runs finished with a success exit code. | How often it ran end-to-end without erroring. |
| **Speedup / × faster / × lighter** | Harbor's number divided by Seaport's. | "8× lighter" means Seaport used an eighth of the memory; "2× faster" means it finished in half the time. |

## Benchmark them side by side

Seaport ships a benchmark harness that runs one or more datasets through both tools and reports the full picture — memory, CPU, instructions, and install footprint, not just time. It wraps each invocation in `/usr/bin/time -l`, discards a warm-up so the numbers are steady-state, and writes a dated `*-datasets.json` / `*-datasets.md` report under `benchmarks/results/`.

```sh
bun run benchmark -- \
  -p benchmarks/tasks/basic-oracle@8 \
  -d factory-ai/legacy-bench@3 \
  -d terminal-bench/terminal-bench-2@1@0
```

Each target is `<spec>[@iterations[@warmup]]`. `-p` is a local task or dataset path, `-d` is a registered dataset name. The trailing numbers set the per-target iteration and warm-up counts, while `-i` and `--warmup` set the defaults. So `legacy-bench@3` measures three iterations after one discarded warm-up, while `terminal-bench-2@1@0` runs a single cold pass with no warm-up — handy for a large dataset you do not want to run repeatedly.

For a quick wall-clock-only comparison of any command, the lighter `benchmarks/benchmark.sh` runs one tool N times and forwards every argument unchanged:

```sh
benchmarks/benchmark.sh seaport run -d factory-ai/legacy-bench
benchmarks/benchmark.sh harbor  run -d factory-ai/legacy-bench
```

There is also a manual `benchmark` GitHub Actions workflow that runs Seaport and Harbor as separate jobs on a dataset you choose, and uploads both sets of logs.
