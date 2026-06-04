# Runtime Ownership

## Owner

Runtime is owned by the engineering team responsible for agent evaluation
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
- `cargo test --all-targets`
- `cargo test --doc`
- `cargo run --example basic_evaluation`
- `cargo bench --bench evaluation`
