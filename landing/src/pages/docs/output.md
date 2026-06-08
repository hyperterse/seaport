---
layout: ../../layouts/DocsLayout.astro
title: Job output
description: Where results land and what is inside each file.
---

# Job output

Every run writes a job directory you can inspect, diff, or feed into CI. By default jobs land under `jobs/`. Change that with `--jobs-dir`.

## Layout

```text
jobs/seaport-<run-id>/
├── config.json
├── result.json
└── <task-name>/
    ├── config.json
    ├── result.json
    ├── agent/
    │   └── trajectory.json
    └── verifier/
        ├── reward.txt
        ├── test-stdout.txt
        └── test-stderr.txt
```

With multiple attempts, each trial directory gets an attempt suffix:

```text
jobs/seaport-<run-id>/
└── acme-task-attempt-2/
```

## The files

| File | What it holds |
| --- | --- |
| `result.json` (job) | Aggregate pass or fail, average reward, and a record per task attempt. |
| `config.json` (job) | The run configuration: agent, attempts, concurrency, backend, model, target. |
| `result.json` (trial) | Pass or fail and the reward for one attempt, plus an error if it failed. |
| `trajectory.json` | The agent command, exit status, stdout, and stderr for the agent phase. |
| `reward.txt` | The raw reward the verifier wrote, `1` or `0`. |
| `test-stdout.txt` / `test-stderr.txt` | The verifier's output streams. |

## Reading results

The end of every run prints a short summary: the job directory, the target, how many tasks passed, and whether the whole run passed. For anything deeper, the JSON is plain and stable, so `jq`, a spreadsheet, or a CI step can read it directly.

```sh
# overall pass count and average reward
jq '{passed, reward, tasks_passed, tasks_total}' jobs/seaport-*/result.json
```

## Exit codes

:::tip
`run` exits non-zero when a run does not fully pass, so you can gate CI on it directly without parsing any JSON.
:::

The exit codes are:

| Code | Meaning |
| --- | --- |
| `0` | Every task passed. |
| `2` | Usage error, such as a bad flag or missing source. |
| `3` | A requested feature is not implemented yet. |
| `4` | One or more tasks failed. |
