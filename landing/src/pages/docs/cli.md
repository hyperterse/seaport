---
layout: ../../layouts/DocsLayout.astro
title: CLI reference
description: Every Seaport command and flag.
---

# CLI reference

```text
seaport <command> [options]
```

| Command | What it does |
| --- | --- |
| `run` | Run a local or registered evaluation. |
| `dataset list` | List registered datasets. |
| `init --task <name>` | Create a task skeleton. |
| `view [jobs-dir]` | View job results. |

Run `seaport <command> --help` for command-specific help.

## seaport run

Run a task or dataset against an agent.

```text
seaport run -p <path> [options]
seaport run -d <dataset> [options]
seaport run -t <task> [options]
seaport run --task-git-url <url> -p <path-in-repo> [options]
```

:::note
Provide exactly one task source: `-p`, `-d`, `-t`, or `--task-git-url`. Combining them is an error.
:::

### Task source

| Flag | Description |
| --- | --- |
| `-p`, `--path <path>` | Local task or dataset directory. |
| `-d`, `--dataset <name>` | Registered dataset name. |
| `-t`, `--task <name>` | Registered task name. |
| `--task-git-url <url>` | Git URL for a task repository. Requires `-p` for the path inside the repo. |
| `--task-git-commit <commit>` | Git commit to check out. Requires `--task-git-url`. |
| `--registry-path <path>` | Local registry JSON for `-d` datasets and `-t` tasks. |
| `--registry-url <url>` | Remote registry URL. Defaults to the package registry. |

### Agent

| Flag | Description |
| --- | --- |
| `-a`, `--agent <agent>` | Agent adapter name. Defaults to `oracle`. |
| `--agent-command <shell>` | Shell command for a custom or not-yet-native agent. |
| `-m`, `--model <model>` | Model identifier. Required for model-backed agents. |
| `--ae`, `--agent-env KEY=VALUE` | Environment variable for the agent phase. Repeatable. |
| `--ve`, `--verifier-env KEY=VALUE` | Environment variable for the verifier phase. Repeatable. |

### Execution

| Flag | Description |
| --- | --- |
| `-n <count>` | How many trials run at once. Defaults to the machine's parallelism, clamped to 1 through 16. |
| `-k`, `--n-attempts <count>` | Number of attempts per task. Defaults to `1`. |
| `--backend <name>` | Execution backend: `docker` or `unsafe-local`. Defaults to `docker`. |
| `--env <name>` | Alias for `--backend`. |
| `--jobs-dir <path>` | Directory where job results are written. Defaults to `jobs/`. |

### Task selection

| Flag | Description |
| --- | --- |
| `-i`, `--include-task-name <glob>` | Include only matching task names. Repeatable. |
| `-x`, `--exclude-task-name <glob>` | Exclude matching task names. Repeatable. |
| `-l`, `--n-tasks <count>` | Limit the number of discovered tasks. |

### Examples

Run one local task with its oracle:

```sh
seaport run -p path/to/task -a oracle
```

Run a whole local dataset, filtered and limited:

```sh
seaport run -p path/to/dataset \
  -i 'suite/*' \
  -x 'suite/skip-*' \
  -l 100
```

Two attempts per task, two at a time:

```sh
seaport run -p path/to/dataset -a oracle -k 2 -n 2
```

Run a registered dataset from a local registry file:

```sh
seaport run -d acme/suite@head --registry-path registry.json -a oracle
```

Run a task straight from git:

```sh
seaport run \
  --task-git-url https://example.com/acme/tasks.git \
  --task-git-commit 0123456789abcdef \
  -p tasks/git-task \
  -a oracle
```

## seaport init

Create a task skeleton in a new directory named after the task:

```sh
seaport init --task acme/hello-world
```

This creates `instruction.md`, `task.toml`, `environment/Dockerfile`, `solution/solve.sh`, and `tests/test.sh`. See [Writing tasks](/docs/tasks) for what each file does.

## seaport dataset list

List registered datasets:

```sh
seaport dataset list
```

`datasets list` is an alias for the same command.

## seaport view

View job results from a jobs directory:

```sh
seaport view [jobs-dir]
```

## Registry files

A registry is a JSON file that maps dataset and task names to local paths or git sources. Task paths are resolved relative to the registry file unless they are absolute. Git-backed entries are cloned into a cache and checked out at `git_commit_id` when given.

```json
[
  {
    "name": "acme/suite",
    "version": "head",
    "tasks": [
      { "name": "acme/local-task", "path": "tasks/local-task" },
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

Set `SEAPORT_REGISTRY_CACHE` to control where git-backed checkouts are stored. By default Seaport uses the system temporary directory.
