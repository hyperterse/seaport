# Runtime Observability Dashboard

## Core Signals

- `run.started`: count by `agent`, `scorer`, and `case_count`.
- `case.started`: count by `case_id`.
- `case.completed`: count by `case_id`, `passed`, and `score`.
- `case.failed`: count by `case_id`, `error_code`, and `error_kind`.
- `run.stopped`: count by `reason`.
- `run.completed`: count by `passed_cases`, `failed_cases`, and `score`.

## Suggested Panels

- Runs by agent.
- Mean run score.
- Failed cases by error code.
- Most frequently failed case IDs.
- Stop-on-failure usage.

## Alert Candidates

- Any `runtime.config.invalid` event in automated runs.
- A sudden increase in `runtime.agent.failed`.
- A mean run score below the release threshold chosen by the owning team.
