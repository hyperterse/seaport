# Seaport vs Harbor Oracle Benchmark

This benchmark runs the same oracle-solvable task through Seaport and Harbor.

The task is intentionally tiny. It measures harness overhead for the oracle path,
not model quality, agent reasoning, or Docker image complexity.

## Task

```text
benchmarks/tasks/basic-oracle/
|-- instruction.md
|-- task.toml
|-- environment/
|   `-- Dockerfile
|-- solution/
|   `-- solve.sh
`-- tests/
    `-- test.sh
```

The oracle solution writes `hello seaport` to the task app directory. The
verifier checks that file and writes reward `1` on success.

The scripts support both environments:

- Harbor-style container paths: `/app` and `/logs/verifier`
- Seaport local benchmark paths: `APP_DIR` and `LOGS_DIR`

## Rich comparison (recommended)

`benchmarks/measure.ts` runs one or more datasets through both tools and reports
the factors that distinguish a static Rust binary from a Python CLI, not just
time:

- wall-clock (mean / median / min / max / stddev)
- CLI-process peak memory (RSS) and peak memory footprint
- CPU time consumed and CPU utilisation
- instructions retired and cycles elapsed (Apple Silicon perf counters)
- clean-exit rate, install footprint, and cold first-run cost

```sh
bun run benchmark -- \
  -p benchmarks/tasks/basic-oracle@8 \
  -d factory-ai/legacy-bench@3 \
  -d terminal-bench/terminal-bench-2@1@0
```

Each target is `<spec>[@iterations[@warmup]]`: `-p` is a local task/dataset path,
`-d` is a registered dataset name. The trailing numbers set per-target iteration
and warm-up counts (`-i` / `--warmup` set the defaults). So `legacy-bench@3` runs
three measured iterations after one discarded warm-up, and `terminal-bench-2@1@0`
runs a single cold pass with no warm-up (handy for an 89-task dataset).

Each invocation is wrapped in `/usr/bin/time -l`; warm-up runs are discarded so
the numbers are steady-state rather than first-run cold builds. The heavy
container work runs in the shared Docker daemon for both tools, so the rusage of
each CLI process isolates harness overhead — the part each tool controls. Results
are written to a dated `benchmarks/results/<date>-datasets.{json,md}`.

A representative run (macOS arm64, Harbor 0.13.1):

| Dataset | Tasks | Seaport wall | Harbor wall | Speedup | Seaport RSS | Harbor RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `basic-oracle` (harness overhead) | 1 | 0.40s | 16.67s | 41× | 28 MB | 231 MB |
| `factory-ai/legacy-bench` | 10 | 39.0s | 60.8s | 1.6× | 28 MB | 232 MB |
| `terminal-bench/terminal-bench-2` | 89 | 102 min | 150 min | 1.5× | 28 MB | 234 MB |

The wall-clock advantage shrinks as real task work (shared by both tools via the
Docker daemon) dominates, but Seaport's peak memory stays ~8× lighter at every
dataset size. Its install footprint is a single 2 MB binary versus Harbor's
~647 MB Python environment.

## Run

Build Seaport and run five sandboxed Docker-backend iterations (wall-clock only):

```sh
python3 benchmarks/run.py --iterations 5
```

Use explicit binaries:

```sh
python3 benchmarks/run.py \
  --seaport-bin target/release/seaport \
  --harbor-bin harbor \
  --iterations 5
```

Measure the trusted local harness path instead:

```sh
python3 benchmarks/run.py --seaport-backend unsafe-local --iterations 5
```

## Benchmark Seaport And Harbor

Use `benchmarks/benchmark.sh` to run the same CLI command through Seaport or
Harbor. The first argument selects the tool, and every argument after that is
forwarded unchanged.

```sh
benchmarks/benchmark.sh seaport run -d factory-ai/legacy-bench
benchmarks/benchmark.sh harbor run -d factory-ai/legacy-bench
```

By default it runs the selected tool 10 times and writes per-run logs plus
`report.json` and `report.md` under `benchmarks/results/`. Run it once for
Seaport and once for Harbor when you want an apples-to-apples local comparison.

Useful environment variables:

- `SEAPORT_BENCH_ITERATIONS=3` changes the number of runs per tool.
- `SEAPORT_BENCH_OUTPUT_DIR=/tmp/seaport-bench` changes where reports are written.
- `SEAPORT_BIN=target/release/seaport` selects the Seaport executable.
- `HARBOR_BIN=harbor` selects the Harbor executable.

There is also a manual GitHub Actions workflow named `benchmark`. Run it from
the Actions tab, provide a dataset name, and it will run separate Seaport and
Harbor jobs:

```sh
benchmarks/benchmark.sh seaport run -d "$DATASET"
benchmarks/benchmark.sh harbor run -d "$DATASET"
```

The runner writes:

- `benchmarks/results/latest.json`
- `benchmarks/results/latest.md`

The latest committed local reports are:

- `benchmarks/results/2026-06-12-rich.md` (full memory/CPU/footprint comparison)
- `benchmarks/results/2026-06-05-oracle.md` (earlier wall-clock-only run)

## Interpreting Results

The default report only declares Seaport faster when both commands complete at
least one successful run on the same task.

If Harbor is not installed, Docker is unavailable, or Harbor fails the task, the
report records that condition instead of fabricating a comparison.

`--seaport-backend unsafe-local` is not a sandboxed comparison. It exists only to
measure Seaport's trusted local harness overhead.
