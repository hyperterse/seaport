# Seaport Ownership

## Owner

Seaport is owned by the engineering team responsible for agent evaluation
quality gates.

## Responsibilities

- Keep public API changes documented in `CHANGELOG.md`.
- Add an ADR for each architectural change that affects determinism,
  telemetry, scoring, or error shape.
- Keep migration notes current for every breaking change.
- Maintain unit tests, integration tests, examples, and benchmarks.
- Review telemetry names and error codes as compatibility-sensitive API.

## Release Checklist

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --all-targets`
- `cargo test --doc`
- `cargo run -- --help`
- `test -f examples/tasks/basic-evaluation/task.toml`
- `test -x examples/tasks/basic-evaluation/solution/solve.sh`
- `test -x examples/tasks/basic-evaluation/tests/test.sh`
- `cargo bench --bench evaluation`
