---
layout: ../../layouts/DocsLayout.astro
title: Getting started
description: Install Seaport and run your first evaluation.
---

# Getting started

## Install

Install the latest release:

```sh
curl -fsSL https://seaport.run/install | bash
```

Pin a specific version:

```sh
VERSION=0.1.0 curl -fsSL https://seaport.run/install | bash
```

The installer accepts a few environment variables:

| Variable | Meaning |
| --- | --- |
| `VERSION` | Version to install, without the leading `v`. Defaults to latest. |
| `INSTALL_DIR` | Where to put the binary. Defaults to `~/.local/bin`. |
| `BASE_URL` | Release base URL. Defaults to GitHub Releases. |

Prefer building from source? With a Rust toolchain installed:

```sh
cargo install --path .
```

Then confirm it works:

```sh
seaport --help
```

## Requirements

Seaport runs tasks in Docker by default, so you need Docker installed and running before your first run.

:::warning
The `unsafe-local` backend runs task scripts directly on your host, with your permissions. It is not a sandbox. Only use it for code you trust, never for untrusted agents.
:::

## Your first run

Create a task skeleton:

```sh
seaport init --task acme/hello-world
```

This writes a `hello-world/` folder with everything a task needs. Run it against its own oracle solution:

```sh
seaport run -p hello-world -a oracle
```

You will see the run start, the task build and execute, and a summary at the end. Results land under `jobs/`.

## Run the bundled example

The repository ships with a ready example you can run without writing anything:

```sh
seaport run -p examples/tasks/basic-evaluation -a oracle
```

## Run without installing

During development you can run straight from source:

```sh
cargo run -- run -p examples/tasks/basic-evaluation -a oracle
```

## Next steps

- Learn the [task format](/docs/tasks) so you can write your own.
- Point a real [agent](/docs/agents) at a task with `-a claude-code` or `-a codex`.
- Read the [CLI reference](/docs/cli) for every option.
