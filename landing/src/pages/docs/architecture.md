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
2. **Plan the trials.** Each task is expanded into one trial per attempt. Trials are scheduled longest-looking first, so slow tasks start early and the run finishes sooner.
3. **Execute.** Trials run across a worker pool sized to the machine. There is no preflight barrier: each trial pulls or builds its own environment on demand, then runs in one long-lived container. Identical image builds and pulls are deduplicated, so tasks sharing an image only pay for it once, and the first trial starts as soon as its image is ready rather than waiting for the rest.
4. **Run the phases.** Inside that container, the agent phase runs first, then the verifier phase runs in the same container via `docker exec`. State the agent creates — installed packages, `$HOME`, files anywhere — carries into the verifier.
5. **Record.** Each trial writes its trajectory, verifier output, and reward. The job then writes an aggregate result.

Caching the slow Docker work and reusing it across trials, attempts, and runs is what keeps things fast. See [Performance](/docs/performance) for the details.

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
