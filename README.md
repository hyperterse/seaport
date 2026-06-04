# Runtime

Runtime is a Rust-native agent evaluation platform. It provides a small,
deterministic core for defining eval cases, running an agent against those cases,
scoring the answers, and inspecting structured reports, errors, and telemetry.

The project is built from the requirements in [`seaport.md`](seaport.md):
deterministic execution, structured errors, full telemetry, tests, examples,
benchmarks, Rustdoc, ADRs, migration notes, CI, and operational documentation.

## What Runtime Does

Runtime evaluates an `Agent` against a list of `TestCase` values.

For each run, it:

- validates agent and case metadata
- sorts cases by stable case ID
- sends each prompt to the agent
- scores each answer with a `Scorer`
- emits deterministic telemetry events
- returns an `EvaluationReport`
- exposes structured `RuntimeError` values when something fails

The default scorer is exact match: an answer receives `1.0` only when it exactly
equals the expected string.

## Design Goals

- Deterministic reports: case ordering, run IDs, scores, and telemetry ordering
  are stable for the same inputs.
- Explicit public API: important behavior is represented by named Rust types,
  not hidden framework conventions.
- Structured failure handling: errors expose both a broad `ErrorKind` and a
  stable machine-readable error code.
- Lightweight integration: the current crate uses only the Rust standard
  library.
- Testable eval logic: examples, unit tests, integration tests, and a benchmark
  are included.

## Project Layout

```text
.
|-- src/
|   |-- agent.rs          # Agent trait and simple built-in agents
|   |-- error.rs          # RuntimeError and ErrorKind
|   |-- evaluation.rs     # TestCase, Evaluator, Scorer, reports, summaries
|   |-- lib.rs            # Public crate exports
|   `-- telemetry.rs      # Deterministic telemetry recorder and events
|-- tests/
|   `-- evaluation_flow.rs
|-- examples/
|   `-- basic_evaluation.rs
|-- benches/
|   `-- evaluation.rs
|-- docs/
|   |-- adr/
|   |-- migrations/
|   |-- observability/
|   |-- operations/
|   `-- code-explanations.md
|-- .github/workflows/ci.yml
|-- Cargo.toml
|-- CHANGELOG.md
`-- seaport.md
```

## Requirements

- Rust stable toolchain
- Cargo

Install Rust with `rustup` if it is not already available:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable
```

Check your local toolchain:

```sh
rustc --version
cargo --version
```

## Installation

Runtime is currently a local crate in this repository.

Use it from this repository by running Cargo commands at the project root:

```sh
cargo test
cargo run --example basic_evaluation
```

Use it from another local Rust project with a path dependency:

```toml
[dependencies]
runtime = { path = "../seaport" }
```

If this repository is published or hosted as a dependency later, replace the
path dependency with the appropriate crates.io version or Git dependency.

## Quick Start

This example evaluates the built-in `EchoAgent`, which returns the prompt
unchanged.

```rust
use runtime::{EchoAgent, Evaluator, TestCase};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let evaluator = Evaluator::default();
    let agent = EchoAgent::new("echo");
    let cases = vec![
        TestCase::new("copy.short", "hello", "hello"),
        TestCase::new("copy.question", "what is runtime?", "what is runtime?"),
    ];

    let report = evaluator.evaluate(&agent, &cases)?;

    println!("run_id: {}", report.run_id);
    println!("score: {:.3}", report.summary.score);
    println!("passed: {}", report.summary.passed_cases);
    println!("failed: {}", report.summary.failed_cases);

    Ok(())
}
```

Run the included version:

```sh
cargo run --example basic_evaluation
```

Example output:

```text
run_id: 2b504c9d2149464b
score: 1.000
passed: 2
failed: 0
```

## Core Concepts

`Agent` is the system being evaluated. It has a stable name and returns one
answer per prompt.

`TestCase` is one eval case. It contains a stable ID, prompt, expected answer,
and optional tags.

`Scorer` compares an expected answer with an actual answer and returns a score
from `0.0` through `1.0`.

`Evaluator` runs the agent against the test cases using a scorer and
configuration.

`EvaluationReport` contains the deterministic run ID, case results, aggregate
summary, and telemetry events for a successful run.

`RuntimeError` represents validation, agent, scoring, and configuration
failures with stable error codes.

## Creating Evals

An eval is a vector of `TestCase` values.

```rust
use runtime::TestCase;

let cases = vec![
    TestCase::new("math.addition", "What is 2 + 2?", "4")
        .with_tags(["math", "smoke"]),
    TestCase::new("copy.greeting", "hello", "hello")
        .with_tags(["copy"]),
];
```

Case IDs matter. Runtime sorts cases by ID before evaluating them, rejects empty
IDs, and rejects duplicate IDs. Stable IDs make reports and run IDs stable.

Good case IDs:

- `math.addition.basic`
- `support.refund.policy`
- `tool.weather.current`

Avoid IDs that depend on timestamps, random numbers, or local file ordering.

## Implementing an Agent

Implement `Agent` for the system you want to evaluate.

```rust
use runtime::{Agent, RuntimeError};

struct MyAgent;

impl Agent for MyAgent {
    fn name(&self) -> &str {
        "my-agent"
    }

    fn respond(&self, prompt: &str) -> Result<String, RuntimeError> {
        Ok(format!("answer for: {prompt}"))
    }
}
```

If your agent fails, return a structured `RuntimeError`:

```rust
use runtime::{Agent, RuntimeError};

struct FailingAgent;

impl Agent for FailingAgent {
    fn name(&self) -> &str {
        "failing-agent"
    }

    fn respond(&self, _prompt: &str) -> Result<String, RuntimeError> {
        Err(RuntimeError::AgentFailed {
            agent: self.name().to_owned(),
            case_id: None,
            message: "upstream model call failed".to_owned(),
        })
    }
}
```

Runtime adds the concrete case ID when an agent error occurs during evaluation.

## Running Evals

Use the default evaluator for exact-match scoring:

```rust
use runtime::{Evaluator, StaticAgent, TestCase};

let evaluator = Evaluator::default();
let agent = StaticAgent::new("static", "yes");
let cases = vec![
    TestCase::new("approval.simple", "Should this pass?", "yes"),
    TestCase::new("approval.other", "Should this also pass?", "yes"),
];

let report = evaluator.evaluate(&agent, &cases).expect("report");

assert_eq!(report.summary.total_cases, 2);
assert_eq!(report.summary.passed_cases, 2);
assert_eq!(report.summary.failed_cases, 0);
assert_eq!(report.summary.score, 1.0);
```

Inspect failed cases:

```rust
for failure in report.failed_cases() {
    eprintln!(
        "{} expected {:?}, got {:?}",
        failure.case_id,
        failure.expected,
        failure.actual
    );
}
```

## Configuring a Run

`RunConfig` controls evaluator behavior.

```rust
use runtime::{Evaluator, ExactMatchScorer, RunConfig};

let evaluator = Evaluator::new(
    RunConfig {
        stop_on_failure: true,
        max_output_chars: Some(4_000),
    },
    ExactMatchScorer,
)
.expect("evaluator");
```

Available options:

- `stop_on_failure`: stops after the first case that scores below `1.0`
- `max_output_chars`: rejects agent answers longer than the configured number
  of Unicode scalar values

`max_output_chars: Some(0)` is rejected as invalid configuration.

## Custom Scorers

Implement `Scorer` when exact match is too strict.

```rust
use runtime::Scorer;

struct ContainsScorer;

impl Scorer for ContainsScorer {
    fn name(&self) -> &str {
        "contains"
    }

    fn score(&self, expected: &str, actual: &str) -> f64 {
        if actual.contains(expected) {
            1.0
        } else {
            0.0
        }
    }
}
```

Use it with `Evaluator::new`:

```rust
use runtime::{Evaluator, RunConfig};

let evaluator = Evaluator::new(RunConfig::default(), ContainsScorer)
    .expect("evaluator");
```

Scorers must return values in the inclusive `0.0..=1.0` range. Runtime rejects
NaN, negative scores, and scores above `1.0`.

## Telemetry

Successful reports include telemetry events:

```rust
let report = evaluator.evaluate(&agent, &cases).expect("report");

for event in &report.telemetry {
    println!("{} {:?}", event.name, event.attributes);
}
```

Telemetry is deterministic:

- events use monotonic sequence numbers instead of timestamps
- attributes are sorted by key and value
- event names are stable strings

Current evaluator event names:

- `run.started`
- `case.started`
- `case.completed`
- `case.failed`
- `run.stopped`
- `run.completed`

Use `evaluate_with_telemetry` when you need telemetry even if evaluation returns
an error:

```rust
use runtime::TelemetryRecorder;

let mut telemetry = TelemetryRecorder::new();
let result = evaluator.evaluate_with_telemetry(&agent, &cases, &mut telemetry);

if let Err(error) = result {
    eprintln!("{}: {}", error.code(), error);
    eprintln!("events captured: {}", telemetry.events().len());
}
```

## Error Handling

Runtime errors expose both a broad kind and a stable code.

```rust
match evaluator.evaluate(&agent, &cases) {
    Ok(report) => println!("score: {:.3}", report.summary.score),
    Err(error) => {
        eprintln!("kind: {:?}", error.kind());
        eprintln!("code: {}", error.code());
        eprintln!("message: {}", error);
    }
}
```

Current error codes:

- `runtime.validation.empty_case_id`
- `runtime.validation.duplicate_case_id`
- `runtime.validation.empty_agent_name`
- `runtime.agent.failed`
- `runtime.agent.output_too_long`
- `runtime.scoring.invalid_score`
- `runtime.config.invalid`

## Determinism Rules

Runtime is designed so equivalent inputs produce equivalent reports.

- Cases are evaluated in sorted case-ID order.
- Duplicate and empty case IDs are rejected.
- Run IDs are derived from the agent name, scorer name, and ordered case
  content.
- Telemetry uses sequence numbers, not wall-clock timestamps.
- Standard-library randomized hashing is not used for run IDs.

To preserve determinism in your own evals:

- keep agent names stable
- keep scorer names stable
- keep case IDs stable
- avoid agent behavior that depends on random state unless that state is seeded
- avoid expected answers that include timestamps or environment-specific values

## Development

Run the full local verification set:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo test --doc
cargo run --example basic_evaluation
cargo bench --bench evaluation
```

Run only the tests:

```sh
cargo test
```

Run integration tests:

```sh
cargo test --test evaluation_flow
```

Run the benchmark harness:

```sh
cargo bench --bench evaluation
```

## CI

The GitHub Actions workflow in [`.github/workflows/ci.yml`](.github/workflows/ci.yml)
runs formatting, clippy, tests, doc tests, the example, and the benchmark.

## Documentation

Additional project documentation:

- [Code explanations](docs/code-explanations.md)
- [ADR 0001: Deterministic Evaluation Core](docs/adr/0001-deterministic-evaluation.md)
- [Initial migration guide](docs/migrations/0001-initial-runtime.md)
- [Runtime observability dashboard](docs/observability/runtime-dashboard.md)
- [Runtime ownership](docs/operations/ownership.md)
- [Changelog](CHANGELOG.md)

## Current Scope

Runtime `0.1.0` is intentionally focused on the in-memory evaluation core. It
does not yet provide persistence, external model clients, dataset loading,
distributed execution, or dashboard rendering. Those pieces can be added around
the current deterministic core without changing the basic eval flow.
