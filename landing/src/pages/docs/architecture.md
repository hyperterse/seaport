---
layout: ../../layouts/DocsLayout.astro
title: Architecture
description: How Seaport is put together and the decisions behind it.
---

# Architecture

Seaport is a small, deterministic core with a sandboxed execution layer around it. This page explains the moving parts and the reasoning behind the main design choices.

## The run loop

A single `run` flows through a few clear stages:

1. **Resolve the target.** A local path, a registered dataset or task, or a git source becomes a concrete set of tasks.
2. **Preflight.** Before any agent runs, Seaport resolves, pulls, and builds every task environment up front, several at a time. Images are cached and reused, and identical images are never pulled twice. A preflight failure is not fatal: that task simply retries during execution.
3. **Plan the trials.** Each task is expanded into one trial per attempt. Trials are scheduled longest-looking first, so slow tasks start early and the run finishes sooner.
4. **Execute.** Trials run across a worker pool sized to the machine. Each trial runs the agent phase, then the verifier phase, inside the sandbox, restoring a fresh workspace from a snapshot rather than rebuilding it.
5. **Record.** Each trial writes its trajectory, verifier output, and reward. The job then writes an aggregate result.

Splitting preflight from execution is what keeps runs fast: all the slow, cacheable Docker work happens once, in parallel, before the clock that matters starts. See [Performance](/docs/performance) for the details.

## Determinism

The core is deterministic by design. Tasks are evaluated in a stable order, duplicate task IDs are rejected, and run identity is derived from stable hashing over the agent, the scorer, and the ordered task content.

In practice this means:

- The same set of tasks produces the same report order.
- Reports do not depend on wall-clock time.
- You choose stable task IDs, and runs stay comparable over time.

The hash exists for identity, not for security.

## Execution backends

The execution layer is pluggable, with two backends today:

- **Docker** is the default. It runs unmodified Linux task environments while providing process and filesystem isolation, network policy, and resource limits. See [Sandboxing](/docs/sandboxing) for the full control list.
- **unsafe-local** runs scripts as host subprocesses. It is fast and convenient for trusted debugging, but it is not a sandbox.

### Why Docker first

Tasks are ordinary Dockerfiles and shell scripts, so the backend has to run them as written. Docker does that and supplies the isolation primitives Seaport needs through documented runtime flags.

The alternatives were considered and set aside for the first milestone:

- **Landlock** is useful for unprivileged restrictions, but has ABI and special-filesystem gaps.
- **bubblewrap** is a strong building block, but leaves the security model to the caller.
- **WebAssembly** has an excellent sandbox, but only runs Wasm modules, not arbitrary task environments.

Stronger isolation fits as a future runtime choice on top of the same model:

- **gVisor** adds a userspace kernel between applications and the host, and integrates with Docker through `runsc`.
- **Firecracker** provides microVM isolation, at the cost of heavier VM and rootfs orchestration.

## Source layout

If you want to read the code, here is the shape of the repository:

```text
src/                  CLI, registry resolution, sandbox runtime, library core
tests/                integration tests
benchmarks/           benchmark runner, benchmark tasks, committed reports
docs/                 ADRs, migration notes, operations notes
.github/workflows/    CI and release pipelines
install.sh            release installer
```

The decisions above are recorded as ADRs in `docs/adr/`: one for the deterministic evaluation core, and one for the sandboxed execution backend.
