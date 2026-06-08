---
layout: ../../layouts/DocsLayout.astro
title: Harbor compatibility
description: The Harbor surface Seaport supports, where the two differ, and how to run them side by side.
---

# Harbor compatibility

Seaport speaks Harbor's task format. If you already have Harbor tasks and datasets, they run on Seaport unchanged. There is no migration step, no rewrite, and no new config to learn.

The benchmark suite proves this in the most direct way possible: it runs the exact same command through both tools and compares the results.

```sh
seaport run -p path/to/task -a oracle
harbor  run -p path/to/task -a oracle
```

## The compatible surface

Everything you use to specify and run a task carries over.

| Surface | What carries over |
| --- | --- |
| Task layout | The `instruction.md`, `task.toml`, `environment/`, `solution/`, and `tests/` folder structure |
| `task.toml` | The `[task]`, `[agent]`, `[verifier]`, and `[environment]` sections |
| Filesystem contract | The `/app` workspace and `/logs/verifier` output directory, with the task mounted read-only |
| Verifier contract | A reward of `1` or `0` written to `reward.txt` |
| CLI verbs | `run -p <path>`, `run -d <dataset>`, and agent selection with `-a` |
| Datasets | The same dataset names, resolved locally or from a registry |
| Agents | The `oracle` and `nop` baselines, plus external commands |

### Tasks

A task is the same directory it always was. See [Writing tasks](/docs/tasks) for the full layout. The `[environment]` section still names a `docker_image`, sets a `network_mode`, and bounds the build with `build_timeout_sec`. The `[agent]` and `[verifier]` sections still carry their `timeout_sec`.

### The container contract

Inside the sandbox, a task sees the same filesystem it expects:

- `/app` is the writable workspace.
- `/logs/verifier` is where the verifier writes its output and `reward.txt`.
- The task directory itself is mounted read-only.

Scripts that hardcode those paths work as written.

:::tip
If a script reads `APP_DIR` and `LOGS_DIR` instead of hardcoding `/app` and `/logs/verifier`, it runs identically under both tools and under Seaport's local backend. See the [environment variables](/docs/tasks#environment-variables).
:::

### Datasets and agents

Run a registered dataset by name with `run -d`, the same way you would with Harbor, and Seaport resolves it locally or from a registry. The built-in `oracle` agent runs a task's `solution/solve.sh`, and `nop` runs only the verifier, so the baseline checks you rely on behave the same.

## Design differences

Same inputs and outputs, different engine underneath.

| Area | Harbor | Seaport |
| --- | --- | --- |
| Distribution | Installed as a Python package | A single self-contained Rust binary, installed in one line |
| Setup | Resolves and builds environments during the run | A preflight phase builds, pulls, and caches every environment up front |
| Sandbox | Docker-backed execution | Hardened Docker by default: no network, dropped capabilities, a read-only root filesystem, non-root execution, and CPU, memory, PID, and wall-clock limits |

Seaport also adds a few things on top of the shared format:

- **Deterministic core.** Stable task ordering and run identity, so results stay comparable across runs.
- **Structured output.** Every job, trial, trajectory, and reward lands as plain JSON. See [Job output](/docs/output).
- **A local fast path.** The `unsafe-local` backend runs scripts directly for trusted debugging, with no container overhead.

## Not at parity yet

Seaport has focused on making the execution path correct and fast first, so a few of Harbor's surfaces are not covered yet. Being straight about the edges:

- **Results viewer.** `seaport view` is a placeholder today. Every job still lands as plain JSON on disk, so nothing is lost (see [Job output](/docs/output)), but there is no built-in browser for those results yet. The runner came first; the viewer is next.
- **Full registry surface.** Seaport resolves datasets and tasks from a registry JSON file and from git-backed sources, which covers the common cases. It does not yet implement Harbor's complete registry feature set.
- **Native agent integrations.** Seaport ships the `oracle` and `nop` baselines plus templates for `codex` and `claude-code`. Any other agent runs through `--agent-command` rather than a built-in adapter, so deeper per-agent integrations are not there yet.

If your workflow depends on one of these, track the [changelog](https://github.com/hyperterse/seaport/blob/main/CHANGELOG.md).
