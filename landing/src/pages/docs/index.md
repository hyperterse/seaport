---
layout: ../../layouts/DocsLayout.astro
title: Introduction
description: What Seaport is and how it works.
---

# Introduction

Seaport tells you whether your coding agent actually works. You write a task, point Seaport at it, and get back a clear pass or fail with a full record of what happened.

It runs entirely from the command line. Tasks are plain folders, not Rust code, so anyone on your team can write one. Every run happens inside an isolated sandbox, so you can hand real code to an AI without worrying about your machine.

## The idea in one minute

An evaluation has three pieces:

1. A **task**: instructions for the agent and a test that checks the result.
2. An **agent**: the thing being evaluated. Claude Code, Codex, your own command, or a built-in baseline.
3. A **score**: did the agent's work pass the test? Seaport runs the test and records a reward of `1` (pass) or `0` (fail).

Seaport ties these together, runs them safely, and writes structured results you can track over time.

## Why Seaport

Harbor got the hard part right. Specifying an eval as a plain folder, with an instruction, a Docker environment, and a verifier script, is a genuinely good model. The friction was never the format. It was the harness: cold Docker builds on every run, image pulls repeated per task, environments rebuilt between attempts, and setup work scattered through the run instead of done up front.

Seaport keeps the format and rebuilds the runner. It adopts Harbor's task layout wholesale, so your existing tasks and datasets run unchanged, then makes the parts you wait on fast:

- **Setup is on-demand, never a barrier.** Each trial builds or pulls its environment as it starts, with identical images built once and shared, so the first trial begins on a cold cache instead of waiting behind a setup phase.
- **Nothing is built twice.** Environments are cached and reused across trials, attempts, and runs, and identical images are built or pulled only once.
- **One lean container per trial.** The agent and verifier run in a single long-lived container over `docker exec`, with no per-phase container churn, across a worker pool sized to your machine.

Two more things follow from being written in Rust:

- **One binary, one command.** Seaport installs as a single static binary with no language runtime or environment to manage.
- **Deterministic and inspectable.** Stable task ordering and run identity, plus structured JSON for every job, trial, and reward, so results stay comparable over time.

The result is the same evaluations you already run, with far less time spent waiting on the harness. See [Harbor compatibility](/docs/harbor) and [Performance](/docs/performance) for the details.

## A first run

```sh
# scaffold a task and run it against its own solution
seaport init --task acme/hello-world
seaport run -p hello-world -a oracle
```

That last command builds the task environment, runs the oracle solution, grades it, and writes everything to `jobs/`.

:::tip
New here? Start with [Getting started](/docs/getting-started) for install and a guided first run.
:::

## Already using Harbor?

Seaport speaks Harbor's task format. Your existing tasks and datasets run unchanged, just faster. See [Harbor compatibility](/docs/harbor).

## What to read next

- [Getting started](/docs/getting-started) walks through install and your first real run.
- [Writing tasks](/docs/tasks) covers the task folder format.
- [Agents](/docs/agents) explains how to plug in Claude Code, Codex, or your own.
- [Performance](/docs/performance) explains how runs stay fast.
- [CLI reference](/docs/cli) lists every command and flag.

## Good to know

- Seaport is written in Rust and ships as a single binary.
- It runs the same tasks and datasets as Harbor, on a faster execution core.
- The default backend is Docker. A faster `unsafe-local` backend exists for trusted local debugging.
- It is open source under the MIT license.
