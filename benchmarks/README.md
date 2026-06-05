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
