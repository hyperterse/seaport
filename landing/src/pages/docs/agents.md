---
layout: ../../layouts/DocsLayout.astro
title: Agents
description: Built-in agents, external commands, and phase environment variables.
---

# Agents

An agent is the thing Seaport evaluates. You pick one with `-a/--agent`. If you leave it off, Seaport uses `oracle`.

## Built-in agents

### oracle

Runs `solution/solve.sh`, then the verifier. Use it to confirm a task is solvable and well formed:

```sh
seaport run -p path/to/task -a oracle
```

### nop

Skips the agent phase and runs only the verifier. Handy for baseline checks, or for tasks where the starting workspace is already in the expected state:

```sh
seaport run -p path/to/task -a nop
```

## External command agents

Any command that runs in a terminal can be an agent. Point Seaport at it with `--agent-command`. The command runs inside the same sandboxed task workspace:

```sh
seaport run -p path/to/task \
  -a custom \
  --agent-command 'my-agent --task "$SEAPORT_INSTRUCTION_PATH" --workdir "$APP_DIR"'
```

The command can read `SEAPORT_INSTRUCTION_PATH`, `APP_DIR`, and the other [environment variables](/docs/tasks#environment-variables).

## Claude Code and Codex

Seaport ships default command templates for `claude-code` and `codex`, so you do not need to write `--agent-command` for them. In Docker mode the CLI must be available inside the task image.

```sh
seaport run -p path/to/task -a claude-code -m sonnet \
  --ae ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY"

seaport run -p path/to/task -a codex -m openai/gpt-5 \
  --ae OPENAI_API_KEY="$OPENAI_API_KEY"
```

:::note
Model-backed agents require a model with `-m/--model`, unless you supply your own `--agent-command`. In Docker mode the agent CLI must already exist inside the task image.
:::

## Passing secrets and config

Use `--ae/--agent-env` for the agent phase and `--ve/--verifier-env` for the verifier phase. Each takes a `KEY=VALUE` pair and can be repeated:

```sh
seaport run -p path/to/task \
  -a custom \
  --agent-command 'my-agent --model "$SEAPORT_MODEL"' \
  -m provider/model \
  --ae API_KEY="$API_KEY" \
  --ve EXPECTED_OUTPUT=ok
```

:::tip
Keeping agent and verifier environments separate means the thing being graded never sees the answer key.
:::

## Multiple attempts

Run an agent against each task several times to measure consistency, not just a single lucky pass. Use `-k` for attempts and `-n` for how many run at once:

```sh
seaport run -p path/to/dataset -a claude-code -m sonnet -k 5 -n 4
```
