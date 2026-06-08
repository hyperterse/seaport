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

## Run

Build Seaport and run five sandboxed Docker-backend iterations:

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

The latest committed local report is:

- `benchmarks/results/2026-06-05-oracle.md`

## Interpreting Results

The default report only declares Seaport faster when both commands complete at
least one successful run on the same task.

If Harbor is not installed, Docker is unavailable, or Harbor fails the task, the
report records that condition instead of fabricating a comparison.

`--seaport-backend unsafe-local` is not a sandboxed comparison. It exists only to
measure Seaport's trusted local harness overhead.
