# Seaport

Seaport is a CLI-first framework for agent evals. The project goal is to let
users create task directories, run agents against those tasks, and inspect job
results from the terminal with the `seaport` command.

The Rust crate is implementation detail. Users should not need to write Rust to
create or run evals.

## Current Status

Seaport currently includes:

- the `seaport` CLI entry point
- `seaport --help`
- `seaport run --help`
- `seaport dataset list`
- `seaport datasets list`
- `seaport init --task <org/name>`
- `seaport view --help`
- a deterministic in-memory evaluation core
- structured errors, telemetry, unit tests, integration tests, examples, and CI

The next required milestone is sandboxed task execution for `seaport run -p`.
The command surface exists now, but full Docker task execution, verifier log
collection, registry datasets, and the results viewer still need to be wired.

## Installation

Install from this repository:

```sh
cargo install --path .
```

Confirm the CLI is available:

```sh
seaport --help
```

For local development without installing:

```sh
cargo run -- --help
```

## CLI Overview

```text
seaport <command> [options]
```

Commands:

- `seaport run`: run a local or registered eval dataset
- `seaport dataset list`: list registered datasets
- `seaport datasets list`: alias for `dataset list`
- `seaport init --task <org/name>`: create a task skeleton
- `seaport view [jobs-dir]`: view job results

## Create an Eval Task

Create a task skeleton:

```sh
seaport init --task acme/hello-world
```

This creates:

```text
hello-world/
|-- instruction.md
|-- task.toml
|-- environment/
|   `-- Dockerfile
|-- solution/
|   `-- solve.sh
`-- tests/
    `-- test.sh
```

`instruction.md` contains the instruction the agent should complete.

`task.toml` contains task metadata, timeouts, agent settings, verifier settings,
and environment settings.

`environment/Dockerfile` defines the container image used for the task.

`solution/solve.sh` is an optional oracle solution.

`tests/test.sh` verifies whether the agent completed the task. The verifier
should write a reward file under `/logs/verifier/`.

## Task Configuration

The generated `task.toml` starts with this shape:

```toml
schema_version = "1.0"

[task]
name = "acme/hello-world"
description = "Describe this task."

[agent]
timeout_sec = 120.0
user = "agent"

[verifier]
timeout_sec = 120.0

[environment]
docker_image = "ubuntu:24.04"
network_mode = "no-network"
```

Future task execution work should extend this format with resource settings,
environment variables, network policy, separate verifier environments, artifacts,
and multi-step tasks.

## Write a Verifier

The verifier is a shell script in `tests/test.sh`.

For pass/fail tasks, write `1` or `0` to `/logs/verifier/reward.txt`:

```sh
#!/bin/bash
set -euo pipefail

mkdir -p /logs/verifier

if test -f /app/output.txt; then
  echo 1 > /logs/verifier/reward.txt
else
  echo 0 > /logs/verifier/reward.txt
fi
```

For richer metrics, the planned format is `/logs/verifier/reward.json`:

```json
{
  "accuracy": 1.0,
  "style": 0.75
}
```

## Run an Eval

The intended local-task command is:

```sh
seaport run -p hello-world -a codex -m openai/gpt-5
```

The intended registered-dataset command is:

```sh
seaport run -d acme/hello-world@1.0 -a codex -m openai/gpt-5
```

The CLI currently parses these flags and fails with a clear not-implemented
message. The next implementation stage should make `seaport run -p <task>` build
or pull the task environment, run the agent phase, run the verifier phase, and
write a job directory.

## Expected Job Output

Seaport should write runs under `jobs/<job-name>/`:

```text
jobs/job-name/
|-- config.json
|-- result.json
|-- trial-name/
|   |-- config.json
|   |-- result.json
|   |-- agent/
|   |   |-- trajectory.json
|   |   `-- recording.cast
|   `-- verifier/
|       |-- reward.txt
|       |-- reward.json
|       |-- test-stdout.txt
|       `-- test-stderr.txt
`-- ...
```

This layout is the compatibility target for `seaport view`.

## View Results

The intended command is:

```sh
seaport view jobs
```

The viewer should eventually start a local web server for browsing jobs, trials,
rewards, trajectories, verifier output, and artifacts. The command currently has
help text and an explicit not-implemented response.

## Dataset Registry

List configured datasets:

```sh
seaport dataset list
```

The current implementation reports that no registry is configured. The next
registry stage should add local registry files, remote registry resolution,
dataset artifact downloads, and cache management.

## Internal Library

The repository also contains a Rust library used by the CLI implementation. It
currently provides:

- `Agent`
- `TestCase`
- `Evaluator`
- `Scorer`
- `EvaluationReport`
- `SeaportError`
- deterministic telemetry events

This API is useful for internal tests and future engine code, but it is not the
primary user interface.

## Development

Run the full local verification set:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo test --doc
cargo run -- --help
cargo run --example basic_evaluation
cargo bench --bench evaluation
```

Run only tests:

```sh
cargo test
```

Run the CLI locally:

```sh
cargo run -- --help
cargo run -- init --task acme/example
cargo run -- run -p example -a codex -m openai/gpt-5
```

## Documentation

Additional project documentation:

- [Code explanations](docs/code-explanations.md)
- [ADR 0001: Deterministic Evaluation Core](docs/adr/0001-deterministic-evaluation.md)
- [Initial migration guide](docs/migrations/0001-initial-seaport.md)
- [Seaport observability dashboard](docs/observability/seaport-dashboard.md)
- [Seaport ownership](docs/operations/ownership.md)
- [Changelog](CHANGELOG.md)
