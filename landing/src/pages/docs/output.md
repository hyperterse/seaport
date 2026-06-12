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

A multi-step task lays its phases out under `steps/<name>/`, each with its own `agent/`, `verifier/`, and `artifacts/`:

```text
jobs/seaport-<run-id>/
└── acme-build-and-test/
    ├── result.json
    └── steps/
        ├── scaffold/
        │   ├── agent/
        │   ├── verifier/
        │   └── artifacts/
        └── tests/
            ├── agent/
            ├── verifier/
            └── artifacts/
```

Any [artifacts](/docs/tasks#artifacts) the task collects land in an `artifacts/` directory (per step for multi-step tasks) with a `manifest.json`.

With multiple attempts, each trial directory gets an attempt suffix:

```text
jobs/seaport-<run-id>/
└── acme-task-attempt-2/
```

## The files

| File | What it holds |
| --- | --- |
| `result.json` (job) | Aggregate pass or fail, average reward, a record per task attempt, and a Harbor-compatible `stats` block. |
| `config.json` (job) | The run configuration: agent, attempts, concurrency, backend, model, target. |
| `result.json` (trial) | Pass or fail, the reward, and the full named reward map for one attempt, plus an error if it failed and `steps` for multi-step tasks. |
| `trajectory.json` | The agent command, exit status, stdout, and stderr for the agent phase. |
| `reward.txt` | The raw reward the verifier wrote. A single number, often `1` or `0`. |
| `test-stdout.txt` / `test-stderr.txt` | The verifier's output streams. |

## The trial result

Each trial's `result.json` records the outcome of one attempt:

```json
{
  "passed": true,
  "reward": "1",
  "rewards": { "reward": 1 }
}
```

- `passed` is the pass-at-full-credit decision (see the [reward model](/docs/tasks#the-reward-model)).
- `reward` is a display string for the scalar reward.
- `rewards` is the full named reward map. For a one-dimensional reward it is `{"reward": 1}`; for a multi-key verifier it carries every named score, for example `{"core_pass_rate": 1, "verbosity": 0.18}`.
- `error` is added only when the trial failed for an infrastructure reason, carrying the failure message.

A [multi-step](/docs/tasks#multi-step-tasks) trial adds a `steps` array, one entry per step that ran:

```json
{
  "passed": true,
  "reward": "1",
  "rewards": { "reward": 1 },
  "steps": [
    { "name": "scaffold", "passed": true, "reward": "1", "rewards": { "reward": 1 } },
    { "name": "tests", "passed": true, "reward": "1", "rewards": { "reward": 1 } }
  ]
}
```

## The stats block

The job `result.json` keeps its existing fields (`passed`, `reward`, `tasks_total`, `tasks_passed`, `tasks_failed`, `tasks[]`) and adds a Harbor-compatible `stats` block:

```json
{
  "stats": {
    "n_completed_trials": 4,
    "n_errored_trials": 0,
    "n_running_trials": 0,
    "n_pending_trials": 0,
    "n_cancelled_trials": 0,
    "n_retries": 0,
    "evals": {
      "oracle__claude__acme-suite": {
        "n_trials": 4,
        "n_errors": 0,
        "metrics": [{ "mean": 1 }],
        "pass_at_k": { "2": 1 },
        "reward_stats": { "reward": { "1.0": ["acme-a", "acme-b"] } },
        "exception_stats": {}
      }
    }
  }
}
```

The top-level counters tally trials by state, plus `n_retries` for the whole job. `evals` is keyed by `"<agent>__<dataset>"`, or `"<agent>__<model>__<dataset>"` when a model is set. Each eval holds:

- `n_trials` and `n_errors` for that eval.
- `metrics` — a single `Mean` entry, wrapped in an array. For a one-dimensional reward it is `[{"mean": x}]`; for a multi-key reward the object carries a per-key mean.
- `pass_at_k` — Harbor's unbiased pass-at-k estimate. It is only emitted for single-key binary (0 or 1) rewards, and is empty with one attempt per task, since k starts at 2.
- `reward_stats` — per reward key, a map from each observed value to the list of trials that produced it.
- `exception_stats` — errored trials grouped by the first line of their error.

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
