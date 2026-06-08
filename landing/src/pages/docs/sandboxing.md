---
layout: ../../layouts/DocsLayout.astro
title: Sandboxing
description: How Seaport isolates each run, and the unsafe-local backend.
---

# Sandboxing

You are handing real code to an AI and letting it run. Seaport keeps every run sealed off from your machine, so a misbehaving agent cannot do damage. You just see the result and move on.

## The Docker backend

Docker is the default. Pick it explicitly with `--backend docker`. The agent and verifier phases each run in their own container, with:

- A read-only task mount, so the task inputs cannot be modified.
- Writable `/app`, `/logs`, `/tmp`, and `/run`, and nothing else.
- Dropped Linux capabilities and `no-new-privileges`.
- A read-only container root filesystem.
- A non-root user.
- Limits on CPU, memory, swap, PID count, and wall-clock time.
- A per-phase network mode taken from `task.toml`.

## Network policy

Set `network_mode` in the `[environment]` section of `task.toml`:

- `no-network` for isolated tasks. This is the safe default.
- `public` when a task genuinely needs network access.

You can override the network mode per phase in `[agent]` or `[verifier]`, so an agent can fetch dependencies while the verifier stays offline, or the reverse.

## The unsafe-local backend

For trusted local debugging, Seaport can run task scripts as plain host subprocesses:

```sh
seaport run -p path/to/task -a oracle --backend unsafe-local
```

:::danger
`unsafe-local` is not a sandbox. Scripts run with your user's permissions on your machine. Only use it for code you wrote or fully trust, and never for untrusted agents.
:::

## Why Docker

Tasks are ordinary Linux environments: Dockerfiles, shell scripts, and verifier scripts. Docker runs them as written, with no rewrite into Rust or WebAssembly, while still providing process isolation, filesystem isolation, network policy, and resource limits. Stronger isolation layers such as gVisor and Firecracker fit as future runtime choices on top of the same model. See [Architecture](/docs/architecture) for the reasoning.
