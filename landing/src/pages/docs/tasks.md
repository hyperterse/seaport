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
The verifier must write `1` or `0` to `$LOGS_DIR/reward.txt`. If it never writes the file, the trial is recorded as a failure.
:::

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
