# Migration 0001: Initial Runtime Adoption

## Applies To

Projects adopting Runtime `0.1.0`.

## Steps

1. Create stable test case IDs before constructing `TestCase` values.
2. Implement the `Agent` trait for the system under evaluation.
3. Start with `Evaluator::default()` and `ExactMatchScorer`.
4. Use `evaluate_with_telemetry` when failure telemetry must be retained after
   an error.
5. Add custom `Scorer` implementations only after exact-match behavior is
   covered by tests.

## Rollback

Remove the Runtime dependency and replace evaluator calls with the previous
test harness. No data migration is required because Runtime `0.1.0` keeps all
state in memory.
