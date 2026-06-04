# Runtime

Runtime is a Rust-native agent evaluation platform built from the requirements in
`seaport.md`.

The crate is intentionally small at first: deterministic execution, structured
errors, telemetry, tests, examples, benchmarks, and operational documentation are
implemented as explicit subsystems instead of hidden framework behavior.

## Usage

```rust
use runtime::{EchoAgent, Evaluator, TestCase};

let evaluator = Evaluator::default();
let agent = EchoAgent::new("echo");
let cases = vec![TestCase::new("copy", "hello", "hello")];

let report = evaluator.evaluate(&agent, &cases)?;
assert_eq!(report.summary.score, 1.0);
# Ok::<(), runtime::RuntimeError>(())
```

## Verification

```sh
cargo test
cargo test --doc
cargo run --example basic_evaluation
cargo bench --bench evaluation
```
