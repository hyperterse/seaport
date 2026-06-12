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
- multi-step tasks, declared artifacts, healthchecks, and clean-room verifiers
- retries and timeout multipliers for slow or emulated hosts
- Docker sandbox by default, with explicit `unsafe-local` mode for trusted development
- phase-specific agent and verifier environment variables, with host-env passthrough
- JSON job, trial, trajectory, verifier, and reward output with harbor-compatible stats
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

`tests/test.sh` verifies the workspace after the agent phase. It writes a reward
into `$LOGS_DIR` as either `reward.json` (preferred) or `reward.txt`:

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

### Reward model

The verifier writes its reward into `$LOGS_DIR` (`/logs/verifier`):

- `reward.txt`: a single number, the 1-D `reward`.
- `reward.json`: a bare number, or an object of named scores, for example:

  ```json
  { "core_pass_rate": 1.0, "strict_pass_rate": 1.0, "verbosity": 0.18 }
  ```

A trial passes at full credit when the `reward` key equals `1.0`, or — for a
multi-key reward with no `reward` key — when every named score equals `1.0`.
Fractional rewards are preserved, not collapsed to pass/fail.

A non-zero exit from the agent or verifier script does not by itself fail the
trial; the reward decides. Fatal failures are a timeout, a verifier that writes
no reward, or a container that will not start.

During execution, Seaport provides:

- `APP_DIR`: writable application workspace, mounted as `/app` in Docker
- `LOGS_DIR`: verifier log directory, mounted as `/logs/verifier` in Docker
- `SEAPORT_TASK_DIR`: read-only task directory
- `SEAPORT_INSTRUCTION_PATH`: path to `instruction.md`
- `SEAPORT_AGENT_NAME`: set for external command agents
- `SEAPORT_MODEL`: set when `-m/--model` is provided

### Multi-step tasks

A task that declares one or more `[[steps]]` entries runs as an ordered
sequence of named steps inside a single persistent container, so state carries
across steps. The on-disk layout is:

```text
multi-step-task/
|-- task.toml
|-- tests/                       # shared test files, mounted writable at /tests
`-- steps/
    |-- first/
    |   |-- instruction.md
    |   |-- solution/
    |   |   `-- solve.sh         # oracle solution for this step
    |   |-- tests/
    |   |   `-- test.sh          # this step's verifier
    |   `-- workdir/
    |       `-- setup.sh         # optional, runs before the step's agent
    `-- second/
        |-- instruction.md
        |-- solution/
        |   `-- solve.sh
        `-- tests/
            `-- test.sh
```

For each step, in order: the optional `workdir/setup.sh` runs first (a non-zero
exit aborts the remaining steps), then the optional per-step healthcheck, then
the agent (the oracle runs that step's `solve.sh`), then per-step artifact
collection into `steps/<name>/artifacts/`, then the step verifier. The step's
reward is read and a `min_reward` gate aborts the remaining steps if unmet.

```toml
schema_version = "1.0"

[task]
name = "acme/multi-step"
description = "Build, then test."

multi_step_reward_strategy = "mean"   # "mean" (default) or "final"

[[steps]]
name = "build"
min_reward = 1.0                       # gates the `reward` key

[steps.agent]
timeout_sec = 300.0
user = "agent"

[steps.verifier]
timeout_sec = 120.0

[[steps]]
name = "test"
min_reward = { core_pass_rate = 1.0, strict_pass_rate = 0.5 }
```

- `min_reward` is a number (gates the `reward` key) or a table of per-key
  thresholds; each named score must meet its threshold, and a missing key fails.
- `multi_step_reward_strategy` (task top level) selects the trial-level reward:
  `"mean"` (default — per-key mean across steps that produced a reward) or
  `"final"` (the last step's reward verbatim).
- Each `[[steps]]` entry takes `name`, optional `[steps.agent]` and
  `[steps.verifier]` blocks (`timeout_sec`, `user`), optional `min_reward`,
  optional `artifacts`, and an optional `healthcheck`.

### Artifacts

The `artifacts` array declares paths to copy out of the container after the
agent runs. It can appear at the task top level and/or per step. Each entry is
either a source-path string or a table:

```toml
artifacts = [
  "/app/build.log",
  { source = "/app", destination = "app", exclude = ["**/node_modules", "**/.git"] },
]
```

`destination` defaults to the source's basename, and `exclude` is a list of glob
patterns applied to directories via `tar --exclude` inside the container.
Collected artifacts land in the trial's `artifacts/` directory (per-step:
`steps/<name>/artifacts/`), and a `manifest.json` records what was collected.
The conventional `/logs/artifacts` drop directory is always collected when
non-empty. Tasks that declare no artifacts and leave `/logs/artifacts` empty
produce no `artifacts/` directory.

### Healthcheck

`[environment.healthcheck]` (and a per-step `healthcheck`) polls a command until
the environment is ready, with Docker `HEALTHCHECK` semantics:

```toml
[environment.healthcheck]
command = "curl -fsS http://localhost:8080/healthz"
interval_sec = 5         # default 5
timeout_sec = 30         # default 30
start_period_sec = 0     # default 0
start_interval_sec = 5   # default 5
retries = 3              # default 3
```

`command` is required. The command is polled until it exits `0`; failures during
the start period do not count toward retries, and after it consecutive failures
count, failing the check once `retries` is reached. The environment healthcheck
runs after the container starts and before the agent; a per-step healthcheck
runs before each step's agent. Failure fails the trial (per-step: aborts the
remaining steps).

### Separate verifier

By default the verifier runs in the agent's container, sharing its installed
packages and filesystem changes. Declaring `[verifier.environment]` (or
`[verifier].environment_mode = "separate"`) instead runs the verifier in a
fresh clean-room container with its own image, platform, and resources:

```toml
[verifier.environment]
docker_image = "ubuntu:24.04"
network_mode = "no-network"
```

The clean-room verifier is seeded with only the task-declared artifacts (plus
the conventional `/logs/artifacts`), uploaded back at their source paths. It is
isolated from the agent's installed packages, `$HOME`, and any filesystem
changes outside those artifacts, so separate-verifier tasks must declare their
work product (e.g. `/app`) as an artifact.

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

`-n` sets concurrency. It defaults to roughly `host_cpus / 3`, clamped to the
range `2..16` — deliberately conservative, since trials are heavy and often run
emulated containers.

Write results somewhere other than `jobs/`:

```sh
seaport run -p path/to/task --jobs-dir /tmp/seaport-jobs
```

### Retries

Retry trials that error on infrastructure failures (build, pull, agent, or
verifier setup), discarding the failed attempt:

```sh
seaport run -p path/to/dataset --max-retries 2
```

`--max-retries` defaults to `0`. Timeouts and reward-file errors (a verifier
that did not write a reward, or an unparseable reward) are never retried,
because retrying is pointless. Narrow what is retried with substring matches on
the error message:

```sh
seaport run -p path/to/dataset --max-retries 2 \
  --retry-include 'connection reset' \
  --retry-exclude 'out of disk'
```

`--retry-include` (repeatable) limits retries to errors matching one of the
substrings; `--retry-exclude` (repeatable) suppresses retries for matching
errors, and already covers the timeout/reward-file cases by default.

### Timeout multipliers

Scale phase timeouts for slow or emulated hosts:

```sh
seaport run -p path/to/dataset --timeout-multiplier 2.0
```

`--timeout-multiplier` (default `1.0`) scales all phase timeouts. Per-phase
multipliers scale a single phase and fall back to the global multiplier; the
build multiplier also covers image pulls:

```sh
seaport run -p path/to/dataset \
  --agent-timeout-multiplier 3.0 \
  --verifier-timeout-multiplier 1.5 \
  --build-timeout-multiplier 2.0
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

Provision the container before the agent runs with `--agent-setup`, for example
to install the agent CLI or its dependencies. The command runs once per trial,
as the agent user with the agent environment; a non-zero exit fails the trial:

```sh
seaport run -p path/to/task \
  -a custom \
  --agent-setup 'pip install --quiet my-agent-cli' \
  --agent-command 'my-agent --task "$SEAPORT_INSTRUCTION_PATH"'
```

Pass phase-specific environment variables with `--ae/--agent-env` and
`--ve/--verifier-env`. Each accepts `KEY=VALUE`, or a bare `KEY` that forwards
that variable from the host environment, so secrets need not appear on the
command line:

```sh
seaport run -p path/to/task \
  -a custom \
  --agent-command 'my-agent --model "$SEAPORT_MODEL"' \
  -m provider/model \
  --ae ANTHROPIC_API_KEY \
  --ae API_KEY="$API_KEY" \
  --ve EXPECTED_OUTPUT=ok
```

Both flags are repeatable.

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

- one container per trial; the agent and verifier run in it via `docker exec`,
  so runtime state the solution creates (installed packages, tool caches)
  is still present for the verifier
- a writable container filesystem with default Linux capabilities, so tasks
  can install packages at runtime
- a read-only task mount at `/seaport/task`, the test directory at `/tests`
  (read-only for single-step, a writable copy for multi-step), an optional
  read-only `/solution` (when the task ships one), and a writable `/logs`
- CPU, memory, swap, PID, and wall-clock limits; by default each trial gets a
  fair share of host CPUs (`host_cpus / concurrency`, capped) rather than the
  task's declared `cpus`, while memory stays at the task's `memory_mb`. Pass
  `--strict-resources` to enforce the task's exact `cpus`/`memory_mb`
- phase-specific network mode from `task.toml`, switched between phases when
  they differ

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

The job `result.json` contains aggregate pass/fail counts, average reward, and
per-task attempt records (`passed`, `reward`, `tasks_total`, `tasks_passed`,
`tasks_failed`, `tasks[]`). `trajectory.json` records the command, exit status,
stdout, and stderr for the agent phase.

Each per-trial `result.json` includes `passed`, `reward` (a scalar display
string), `rewards` (the full named-score map as a JSON object), and `error`
when the trial failed. A multi-step trial also has a `steps` array (`name`,
`passed`, `reward`, `rewards` per step) and per-step directories
`steps/<name>/{agent,verifier,artifacts}`.

The job `result.json` also carries a harbor-compatible `stats` block with
trial counts (`n_completed_trials`, `n_errored_trials`, `n_running_trials`,
`n_pending_trials`, `n_cancelled_trials`, `n_retries`) and an `evals` map keyed
by `"<agent>[__<model>]__<dataset>"`. Each eval reports `n_trials`, `n_errors`,
`metrics` (per-key reward means), `pass_at_k` (an unbiased estimate, only for
single-key binary `0`/`1` rewards), `reward_stats` (per reward key, each
observed value mapped to the trials that produced it), and `exception_stats`
(errored trials grouped by the first line of their error).

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
