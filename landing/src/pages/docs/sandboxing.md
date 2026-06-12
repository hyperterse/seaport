---
layout: ../../layouts/DocsLayout.astro
title: Sandboxing
description: How Seaport isolates each run, and the unsafe-local backend.
---

# Sandboxing

You are handing real code to an AI and letting it run. Seaport keeps every run sealed off from your machine, so a misbehaving agent cannot do damage. You just see the result and move on.

## The Docker backend

Docker is the default. Pick it explicitly with `--backend docker`. Each trial gets one long-lived container: the agent phase runs in it, then the verifier phase runs in the same container via `docker exec`. State the agent creates — installed packages, files in `$HOME`, files anywhere on disk — persists into the verifier, matching how Harbor runs. The setup is:

- One container per trial, isolated from your machine and from other trials, swept away when the run ends.
- A writable root filesystem with the image's default Linux capabilities, so tasks can install packages and write anywhere they need at runtime. This is a deliberate trade-off for task-environment fidelity and Harbor parity, not a hardened jail.
- Read-only mounts for task inputs: `/seaport/task` (the whole task directory), `/tests` (the task's tests; writable for multi-step tasks), and `/solution` when the task ships a `solution/` directory. `/logs` is writable; artifacts conventionally land in `/logs/artifacts`.
- A `--pids-limit` of 4096, plus limits on CPU and memory.
- A per-phase network mode taken from `task.toml`.
- Per-phase and per-trial wall-clock timeouts.

Scripts run as the image's default user; an internal prep step runs as root.

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
