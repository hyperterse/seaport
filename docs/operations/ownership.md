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
- `cargo test --all-targets --locked`
- `cargo test --doc --locked`
- `bash -n install.sh`
- `cargo run -- --help`
- `test -f examples/tasks/basic-evaluation/task.toml`
- `test -x examples/tasks/basic-evaluation/solution/solve.sh`
- `test -x examples/tasks/basic-evaluation/tests/test.sh`
- `cargo bench --bench evaluation`

## Release Checklist

1. Update `CHANGELOG.md`.
2. Confirm `Cargo.toml` version matches the intended tag without the leading
   `v`.
3. Run the release verification commands above.
4. Push a semantic version tag such as `v0.1.0`.
5. Confirm GitHub Releases contains platform archives and `checksums.txt`.
6. Smoke-test the installer with `VERSION=<version>`.
