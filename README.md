# Seaport

Seaport is a CLI-first evaluation runner for software-agent tasks. It runs task
directories, local datasets, registry JSON files, and git-backed task sources
through a sandboxed execution backend, then writes structured job results that
can be inspected or benchmarked.

You do not need to write Rust to use Seaport. Tasks are ordinary directories
with Markdown instructions, shell scripts, and a small TOML config.

## Features

- `seaport` command-line interface
- local task and local dataset execution
- registry JSON resolution for datasets and individual tasks
- git-backed task checkout and caching
- `oracle`, `nop`, and sandboxed external command agents
- task filtering with include/exclude glob patterns
- multiple attempts per task with bounded concurrency
- Docker sandbox by default, with explicit `unsafe-local` mode for trusted development
- phase-specific agent and verifier environment variables
- JSON job, trial, trajectory, verifier, and reward output
- CI, release builds, installer script, unit tests, integration tests, and benchmarks

## Install

Install the latest released CLI:

```sh
curl -fsSL https://seaport.run/install | bash
```

Install a specific version:

```sh
VERSION=0.1.0 curl -fsSL https://seaport.run/install | bash
```

Installer environment variables:

- `VERSION`: version to install, without the leading `v`; defaults to latest
- `INSTALL_DIR`: install directory; defaults to `~/.local/bin`
- `BASE_URL`: release base URL; defaults to GitHub Releases

Install from source during development:

```sh
cargo install --path .
```

Confirm the CLI works:

```sh
seaport --help
```

## Upgrade

Update an installed CLI to the latest release:

```sh
seaport upgrade
```

Seaport also checks for new releases in the background and prints a one-line
notice when an upgrade is available. The check is throttled to once per day and
runs only on an interactive terminal. To opt out, set `SEAPORT_NO_UPDATE_CHECK`.

```sh
seaport upgrade --check          # report whether a newer version exists
seaport upgrade --force          # reinstall the latest release
seaport upgrade --version 0.1.0  # install a specific version
```

## Quick Start

Create a task skeleton:

```sh
seaport init --task acme/hello-world
```

Run the generated task with its oracle solution:

```sh
seaport run -p hello-world
```

Run the repository example:

```sh
seaport run -p examples/tasks/basic-evaluation
```

Run without installing the CLI:

```sh
cargo run -- run -p examples/tasks/basic-evaluation
```

## Task Format

A task is a directory like this:

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

`instruction.md` is the prompt given to the agent.

`task.toml` describes metadata, timeouts, and the execution environment:

```toml
schema_version = "1.0"

[task]
name = "acme/hello-world"
description = "Create the expected output file."

[agent]
timeout_sec = 120.0
user = "agent"

[verifier]
timeout_sec = 120.0

[environment]
docker_image = "ubuntu:24.04"
network_mode = "no-network"
build_timeout_sec = 600.0
```

`solution/solve.sh` is used by the `oracle` agent. It is optional for `nop` and
external agents.

`tests/test.sh` verifies the workspace after the agent phase. It should write
`1` or `0` to `$LOGS_DIR/reward.txt`:

```sh
#!/bin/bash
set -euo pipefail

mkdir -p "$LOGS_DIR"

if test -f "$APP_DIR/output.txt"; then
  echo 1 > "$LOGS_DIR/reward.txt"
else
  echo 0 > "$LOGS_DIR/reward.txt"
fi
```

During execution, Seaport provides:

- `APP_DIR`: writable application workspace, mounted as `/app` in Docker
- `LOGS_DIR`: verifier log directory, mounted as `/logs/verifier` in Docker
- `SEAPORT_TASK_DIR`: read-only task directory
- `SEAPORT_INSTRUCTION_PATH`: path to `instruction.md`
- `SEAPORT_AGENT_NAME`: set for external command agents
- `SEAPORT_MODEL`: set when `-m/--model` is provided

## Run Tasks

Run one local task:

```sh
seaport run -p path/to/task
```

Run every immediate task subdirectory in a local dataset:

```sh
seaport run -p path/to/dataset
```

Filter tasks by name:

```sh
seaport run -p path/to/dataset \
  -i 'suite/*' \
  -x 'suite/skip-*' \
  -l 100
```

Run two attempts per task with two workers:

```sh
seaport run -p path/to/dataset -k 2 -n 2
```

Write results somewhere other than `jobs/`:

```sh
seaport run -p path/to/task --jobs-dir /tmp/seaport-jobs
```

## Agents

`oracle` runs `solution/solve.sh`, then runs the verifier:

```sh
seaport run -p path/to/task -a oracle
```

`nop` skips the agent phase and runs only the verifier. This is useful for
baseline checks and tasks where the initial workspace already contains the
expected state:

```sh
seaport run -p path/to/task -a nop
```

External command agents run a shell command in the same sandboxed task workspace:

```sh
seaport run -p path/to/task \
  -a custom \
  --agent-command 'my-agent --task "$SEAPORT_INSTRUCTION_PATH" --workdir "$APP_DIR"'
```

Pass phase-specific environment variables with `--ae/--agent-env` and
`--ve/--verifier-env`:

```sh
seaport run -p path/to/task \
  -a custom \
  --agent-command 'my-agent --model "$SEAPORT_MODEL"' \
  -m provider/model \
  --ae API_KEY="$API_KEY" \
  --ve EXPECTED_OUTPUT=ok
```

Seaport also includes default external command templates for `codex` and
`claude-code`. In Docker mode, those CLIs must be available inside the task image:

```sh
seaport run -p path/to/task -a codex -m openai/gpt-5 --ae OPENAI_API_KEY="$OPENAI_API_KEY"
seaport run -p path/to/task -a claude-code -m sonnet --ae ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY"
```

## Registry Inputs

Run a dataset from a local registry JSON file:

```sh
seaport run -d acme/suite@head --registry-path registry.json
```

Run one registered task:

```sh
seaport run -t acme/task --registry-path registry.json
```

Registry task paths are resolved relative to the registry file unless they are
absolute. Git-backed task entries are cloned into Seaport's registry cache and
checked out at `git_commit_id` when provided.

Example registry:

```json
[
  {
    "name": "acme/suite",
    "version": "head",
    "tasks": [
      {
        "name": "acme/local-task",
        "path": "tasks/local-task"
      },
      {
        "name": "acme/git-task",
        "path": "tasks/git-task",
        "git_url": "https://example.com/acme/tasks.git",
        "git_commit_id": "0123456789abcdef"
      }
    ]
  }
]
```

Run a task directly from git without a registry file:

```sh
seaport run \
  --task-git-url https://example.com/acme/tasks.git \
  --task-git-commit 0123456789abcdef \
  -p tasks/git-task
```

Set `SEAPORT_REGISTRY_CACHE` to control where git-backed task checkouts are
stored. By default Seaport uses the system temporary directory.

## Sandboxing

The Docker backend is the default:

```sh
seaport run -p path/to/task --backend docker
```

Docker execution uses:

- separate containers for agent and verifier phases
- read-only task mount at `/seaport/task`
- writable `/app`, `/logs`, `/tmp`, and `/run`
- dropped Linux capabilities
- `no-new-privileges`
- read-only container root filesystem
- non-root numeric user
- CPU, memory, swap, PID, and wall-clock limits (task limits are boosted to
  use idle host resources by default; pass `--strict-resources` to enforce
  the task's own `cpus`/`memory_mb` exactly)
- per-trial `/app` docker volume seeded from the environment image
- phase-specific network mode from `task.toml`

Use `network_mode = "no-network"` for isolated tasks and `network_mode =
"public"` when the task explicitly needs network access. Phase-specific
overrides can be set in `[agent]` or `[verifier]`.

For trusted local development only:

```sh
seaport run -p path/to/task --backend unsafe-local
```

`unsafe-local` runs task scripts as host subprocesses. It is convenient for
debugging, but it is not a sandbox.

## Job Output

By default Seaport writes jobs under `jobs/`:

```text
jobs/seaport-<run-id>/
|-- config.json
|-- result.json
`-- <task-name>/
    |-- config.json
    |-- result.json
    |-- agent/
    |   `-- trajectory.json
    `-- verifier/
        |-- reward.txt
        |-- test-stdout.txt
        `-- test-stderr.txt
```

With multiple attempts, trial directories include an attempt suffix:

```text
jobs/seaport-<run-id>/
`-- acme-task-attempt-2/
```

`result.json` contains aggregate pass/fail counts, average reward, and per-task
attempt records. `trajectory.json` records the command, exit status, stdout, and
stderr for the agent phase.

## Benchmarks

Run the built-in benchmark runner:

```sh
python3 benchmarks/run.py --iterations 5
```

Use the local backend for trusted harness-overhead checks:

```sh
python3 benchmarks/run.py --seaport-backend unsafe-local --iterations 5
```

The benchmark task is `benchmarks/tasks/basic-oracle`. The runner writes
machine-local output to:

- `benchmarks/results/latest.json`
- `benchmarks/results/latest.md`

The latest committed benchmark report is
[`benchmarks/results/2026-06-05-oracle.md`](benchmarks/results/2026-06-05-oracle.md).

## Development

Run the full local verification set:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets --locked
cargo test --doc --locked
bash -n install.sh
PYTHONPYCACHEPREFIX=/tmp/seaport-pycache python3 -m py_compile benchmarks/run.py
cargo bench --bench evaluation
```

Run the CLI locally:

```sh
cargo run -- --help
cargo run -- run --help
cargo run -- init --task acme/example
```

Create a release by running the `release` workflow from a branch:

```sh
gh workflow run release.yml --ref main
```

The release workflow installs the root Bun release tooling, uses `release-it`
with `@release-it/conventional-changelog` to bump `package.json`, write
`CHANGELOG.md`, commit, tag, and push as the configured GitHub App, syncs that
version to `Cargo.toml` and `Cargo.lock`, builds Linux, macOS, and Windows
archives, publishes them to GitHub Releases, and uploads `checksums.txt`.

## Project Layout

```text
src/                  CLI, registry resolution, sandbox runtime, library core
tests/                integration tests
benchmarks/           benchmark runner, benchmark tasks, committed reports
docs/                 ADRs, migration notes, operations notes
.github/workflows/    CI and release pipelines
install.sh            release installer
```

## Documentation

- [Changelog](CHANGELOG.md)
- [Code explanations](docs/code-explanations.md)
- [ADR 0001: Deterministic Evaluation Core](docs/adr/0001-deterministic-evaluation.md)
- [ADR 0002: Sandboxed Execution Backend](docs/adr/0002-sandboxed-execution-backend.md)
- [Initial migration guide](docs/migrations/0001-initial-seaport.md)
- [Seaport observability dashboard](docs/observability/seaport-dashboard.md)
- [Seaport ownership](docs/operations/ownership.md)

## License

MIT
