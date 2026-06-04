# ADR 0001: Deterministic Evaluation Core

## Status

Accepted

## Context

`seaport.md` requires deterministic execution, structured errors, full
telemetry, tests, examples, benchmarks, and public API coverage. The repository
started without an implementation, so the first architectural decision is the
shape of the core evaluation loop.

## Decision

Runtime evaluates cases in sorted case-ID order, rejects duplicate IDs, emits
sequence-based telemetry, and derives run IDs from stable FNV-1a hashing over
agent name, scorer name, and ordered case content.

## Consequences

- Equivalent case sets produce the same report order and run ID.
- Reports do not depend on wall-clock time.
- Callers must choose stable case IDs.
- The hash is for deterministic identity, not cryptographic security.
