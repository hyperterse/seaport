# Code Explanations

This document explains the lines that carry behavior or design intent. Straight
Rust struct literals, field assignments, imports, and obvious test assertions are
left to the code unless they hide a Seaport-specific decision.

## `src/lib.rs`

- `mod agent;`, `mod error;`, `mod evaluation;`, and `mod telemetry;` keep the
  implementation split by responsibility while preserving one public crate API.
- The `pub use ...` lines re-export the intended public surface so callers do
  not need to know the internal file layout.
- `SEAPORT_NAME` gives diagnostics and examples one stable crate identifier.

## `src/main.rs`

- `main` delegates to `run` and exits with command-specific status codes so the
  CLI can be tested without terminating the test process.
- `run` dispatches the top-level `seaport` commands.
- `run_eval` parses the expected local and registered dataset flags, then
  returns a clear not-implemented error until sandbox execution is wired.
- `dataset` supports both `dataset list` and the `datasets list` alias.
- `init` creates the first task skeleton format users can edit without writing
  Rust.
- `CliError` carries an exit code alongside the message so usage errors and
  unimplemented commands are distinguishable.

## `src/agent.rs`

- `Agent::name` is required because reports, run IDs, and telemetry need a
  stable agent identity.
- `Agent::respond` returns `Result<String, SeaportError>` so agent failures stay
  structured instead of becoming plain strings.
- `EchoAgent` and `StaticAgent` are intentionally simple test agents. They make
  examples and tests deterministic without adding mock frameworks.

## `src/error.rs`

- `ErrorKind` separates routing-level categories from exact error codes.
- `SeaportError` variants carry the data needed to debug each failure without
  parsing formatted text.
- `SeaportError::kind` maps variants to stable categories for metrics and alert
  grouping.
- `SeaportError::code` returns stable machine-readable codes for telemetry,
  logs, and external integrations.
- `Display` is implemented for humans; callers should use `code` and `kind` for
  programmatic decisions.

## `src/telemetry.rs`

- `TelemetryEvent::sequence` avoids timestamps in evaluation reports, keeping
  telemetry deterministic.
- `TelemetryRecorder::new` starts `next_sequence` at one, making event order
  easy to read.
- `TelemetryRecorder::record` is private so every event goes through the same
  sorting and sequencing path.
- `telemetry_attributes` accepts a fixed array to keep call sites compact while
  still sorting attributes deterministically.

## `src/evaluation.rs`

- `BTreeSet` is used for duplicate detection because it has deterministic
  ordering and no randomized hashing.
- `TestCase::new` does not validate immediately; validation happens at run
  boundaries so batches can report errors consistently.
- `RunConfig::validate` rejects `Some(0)` because a zero-character answer limit
  is almost always a configuration mistake.
- `Scorer` is a trait so exact match is the default, not the only scoring model.
- `ExactMatchScorer` returns only `1.0` or `0.0`, making the initial evaluator
  deterministic and easy to audit.
- `Evaluator::evaluate` creates an internal telemetry recorder for the common
  success path.
- `Evaluator::evaluate_with_telemetry` lets callers keep failure telemetry when
  the evaluation returns an error.
- `validate_cases` rejects empty IDs, rejects duplicates, then sorts by ID so
  equivalent case sets produce the same report order and run ID.
- `validate_output_limit` counts `chars` instead of bytes so limits match what
  humans see in Unicode text.
- `validate_score` rejects NaN and values outside `0.0..=1.0`, keeping summary
  math stable.
- `deterministic_run_id` hashes the agent, scorer, and ordered case content so
  the same logical run gets the same ID.
- `StableHash` uses fixed FNV-1a constants instead of `std` randomized hashing.
- `write_str` appends `0xff` as a field separator so adjacent strings cannot
  accidentally collapse into the same byte stream.
