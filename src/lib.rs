//! Seaport is a Rust-native agent evaluation platform.
//!
//! The public API is intentionally explicit: callers provide an [`Agent`],
//! a list of [`TestCase`] values, and an [`Evaluator`] returns an
//! [`EvaluationReport`] with structured errors and deterministic telemetry.

mod agent;
mod error;
mod evaluation;
mod telemetry;

pub use agent::{Agent, EchoAgent, StaticAgent};
pub use error::{ErrorKind, SeaportError};
pub use evaluation::{
    CaseResult, EvaluationReport, EvaluationSummary, Evaluator, ExactMatchScorer, RunConfig,
    Scorer, TestCase,
};
pub use telemetry::{telemetry_attributes, TelemetryEvent, TelemetryLevel, TelemetryRecorder};

/// Stable crate name used by examples, diagnostics, and documentation.
pub const SEAPORT_NAME: &str = "seaport";
