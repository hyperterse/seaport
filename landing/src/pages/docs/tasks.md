---
layout: ../../layouts/DocsLayout.astro
title: Writing tasks
description: The task folder format, task.toml, and the verifier contract.
---

# Writing tasks

A task is an ordinary directory. There is no Rust and no boilerplate. If you can write a shell script, you can write a task.

## Layout

```text
hello-world/
├── instruction.md      # the prompt given to the agent
├── task.toml           # metadata, timeouts, environment
├── environment/
│   └── Dockerfile      # the image the task runs in
├── solution/
│   └── solve.sh        # optional oracle solution
└── tests/
    └── test.sh         # verifier: decides pass or fail
```

Only three files are required: `instruction.md`, `task.toml`, and `tests/test.sh`. The `environment/` and `solution/` folders are there when you need them.

The fastest way to get this structure is `seaport init --task <org>/<name>`.

## instruction.md

Plain Markdown describing what the agent should do. This is the prompt handed to the agent. Be specific about the expected result, since your verifier will check exactly that.

## task.toml

Metadata, timeouts, and the execution environment:

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

A few notes:

- `[environment].docker_image` is the base image. If `environment/Dockerfile` is present, it is used to build the task image.
- `network_mode` is `no-network` for isolated tasks or `public` when a task genuinely needs the network. You can override it per phase in `[agent]` or `[verifier]`.
- Timeouts are in seconds and apply to each phase independently.

## solution/solve.sh

The oracle solution. The built-in `oracle` agent runs this script, which lets you confirm a task is solvable before pointing a real agent at it. It is optional for the `nop` agent and for external agents.

## tests/test.sh

The verifier. It runs after the agent phase and decides pass or fail by writing `1` or `0` to `reward.txt` in the logs directory:

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

:::note
The verifier must write a reward to `$LOGS_DIR`. If it never writes one, the trial is recorded as a failure.
:::

## The reward model

The verifier writes its reward into `$LOGS_DIR` (`/logs/verifier`) as either `reward.json` or `reward.txt`. When both are present, `reward.json` wins.

- `reward.txt` is a single number, the one-dimensional `reward`. A plain `1` or `0` is the common case.
- `reward.json` is either a bare number, or an object of named scores:

```json
{ "core_pass_rate": 1.0, "strict_pass_rate": 1.0, "verbosity": 0.18 }
```

A trial passes at full credit. With a single `reward`, that means `reward` equals `1.0`. With a multi-key object and no `reward` key, every named score must equal `1.0`. Fractional rewards are preserved as written, not collapsed to pass or fail, so you can track partial-credit metrics across runs.

## Multi-step tasks

A task can run as a sequence of named steps inside one persistent container. State carries across steps: packages installed, `$HOME`, and files written in an earlier step are visible to later ones. Declare the steps with `[[steps]]` in `task.toml`:

```toml
schema_version = "1.0"

[task]
name = "acme/build-and-test"
description = "Scaffold a service, then add tests."

[environment]
docker_image = "node:20"
network_mode = "no-network"

multi_step_reward_strategy = "mean"

[[steps]]
name = "scaffold"
min_reward = 1.0

[steps.agent]
timeout_sec = 300.0
user = "agent"

[steps.verifier]
timeout_sec = 120.0

[[steps]]
name = "tests"

[steps.agent]
timeout_sec = 300.0

[steps.verifier]
timeout_sec = 120.0
```

The on-disk layout puts each step under `steps/<name>/`:

```text
build-and-test/
├── task.toml
├── environment/
│   └── Dockerfile
├── tests/                       # shared test files, mounted writable at /tests
│   └── helpers.sh
└── steps/
    ├── scaffold/
    │   ├── instruction.md        # the prompt for this step
    │   ├── solution/solve.sh     # this step's oracle solution
    │   ├── tests/test.sh         # this step's verifier
    │   └── workdir/setup.sh      # optional, runs before the agent
    └── tests/
        ├── instruction.md
        ├── solution/solve.sh
        └── tests/test.sh
```

The top-level `tests/` directory holds files shared across steps. Unlike a single-step task, it is mounted **writable** at `/tests` so steps can stage and update shared fixtures.

Each step runs in order:

1. Optional `workdir/setup.sh` runs before the agent. A non-zero exit aborts the remaining steps.
2. An optional per-step [healthcheck](#healthcheck) runs.
3. The agent runs (the `oracle` agent runs that step's `solution/solve.sh`).
4. Per-step [artifacts](#artifacts) are collected into `steps/<name>/artifacts/`.
5. The step verifier (`tests/test.sh`) runs and writes its reward.

### Per-step configuration

Each `[[steps]]` entry takes a `name` and a handful of optional keys:

- `[steps.agent]` with `timeout_sec` and `user`.
- `[steps.verifier]` with `timeout_sec` and `user`.
- `min_reward` to gate the rest of the run.
- `artifacts` to collect work product after the step's agent.
- `healthcheck` to run before the step's agent.

### Gating with `min_reward`

`min_reward` stops a run early when a step underperforms. It is either a number that gates the `reward` key, or a table of per-key thresholds where each named score must meet its threshold:

```toml
[[steps]]
name = "scaffold"
min_reward = { core_pass_rate = 1.0, lint = 0.8 }
```

A missing key fails the gate. When a step does not meet its `min_reward`, the remaining steps are aborted.

### Trial reward

Set how the per-step rewards roll up into the trial reward at the task top level:

- `multi_step_reward_strategy = "mean"` (default) takes the per-key mean across the steps that produced a reward.
- `multi_step_reward_strategy = "final"` uses the last step's reward verbatim.

## Artifacts

`artifacts` declares files or directories to copy out of the container after the agent runs. Add it at the task top level, per step, or both. Each entry is either a source-path string or a table:

```toml
artifacts = [
  "/app/dist",
  { source = "/app", destination = "app", exclude = ["node_modules", "*.log"] },
]
```

- `source` is the path inside the container.
- `destination` defaults to the source's basename.
- `exclude` is a list of glob patterns, applied with `tar --exclude` inside the container (the same mechanism Harbor uses).

Collected artifacts land in the trial's `artifacts/` directory (per step, in `steps/<name>/artifacts/`), alongside a `manifest.json` recording what was collected. The conventional `/logs/artifacts` directory is always collected when it is non-empty. A task that declares no artifacts and writes nothing to `/logs/artifacts` produces no `artifacts/` directory.

## Separate verifier

By default the verifier runs in the same container as the agent, so it sees everything the agent left behind. A task can instead run its verifier in a fresh, clean-room container by declaring a verifier environment:

```toml
[verifier.environment]
docker_image = "ubuntu:24.04"

artifacts = [{ source = "/app", destination = "app" }]
```

You can also opt in with `[verifier].environment_mode = "separate"`.

In separate mode the verifier container is seeded with **only** the task-declared artifacts (plus the conventional `/logs/artifacts`), uploaded back to their source paths. It does not inherit the agent's installed packages, `$HOME`, or any filesystem change outside the declared artifacts. Because of that, a separate-verifier task **must** declare its work product (for example `/app`) as an artifact, or the verifier will not see it. The verifier environment carries its own image, platform, and resources.

## Healthcheck

A healthcheck holds the agent until the environment is ready. It runs after the container starts and before the agent. Declare it under `[environment.healthcheck]`, or per step:

```toml
[environment.healthcheck]
command = "pg_isready -h localhost"
interval_sec = 5
timeout_sec = 30
start_period_sec = 0
start_interval_sec = 5
retries = 3
```

Only `command` is required; the rest show their defaults above. The semantics match Docker's `HEALTHCHECK`: the command is polled until it exits `0`. Failures during the start period do not count toward `retries`; after it, consecutive failures count and the check fails once `retries` is reached. A failed healthcheck fails the trial (for a per-step healthcheck, it aborts the remaining steps).

## Environment variables

During execution, Seaport provides:

| Variable | Meaning |
| --- | --- |
| `APP_DIR` | Writable workspace, mounted as `/app` in Docker. |
| `LOGS_DIR` | Verifier log directory, mounted as `/logs/verifier`. |
| `SEAPORT_TASK_DIR` | Read-only task directory. |
| `SEAPORT_INSTRUCTION_PATH` | Path to `instruction.md`. |
| `SEAPORT_AGENT_NAME` | Set for external command agents. |
| `SEAPORT_MODEL` | Set when `-m/--model` is provided. |

## Trying it out

Validate a task by running it with its oracle:

```sh
seaport run -p path/to/task -a oracle
```

A passing oracle run means the task is well formed and solvable. From there, swap in a real [agent](/docs/agents).
