use std::collections::BTreeSet;

use crate::{telemetry_attributes, Agent, RuntimeError, TelemetryEvent, TelemetryRecorder};

/// A single prompt and expected answer used to evaluate an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCase {
    /// Stable identifier used for ordering, telemetry, and reporting.
    pub id: String,
    /// Prompt sent to the agent.
    pub prompt: String,
    /// Expected response used by the scorer.
    pub expected: String,
    /// Optional labels for filtering and reporting.
    pub tags: Vec<String>,
}

impl TestCase {
    /// Creates a test case without tags.
    pub fn new(
        id: impl Into<String>,
        prompt: impl Into<String>,
        expected: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            prompt: prompt.into(),
            expected: expected.into(),
            tags: Vec::new(),
        }
    }

    /// Adds tags to a test case.
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }
}

/// Runtime options that affect evaluation behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunConfig {
    /// Stops evaluation after the first failed case when enabled.
    pub stop_on_failure: bool,
    /// Optional maximum number of Unicode scalar values allowed in an answer.
    pub max_output_chars: Option<usize>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            stop_on_failure: false,
            max_output_chars: None,
        }
    }
}

impl RunConfig {
    fn validate(&self) -> Result<(), RuntimeError> {
        if self.max_output_chars == Some(0) {
            return Err(RuntimeError::InvalidConfig {
                message: "max_output_chars must be greater than zero".to_owned(),
            });
        }

        Ok(())
    }
}

/// Scores one agent answer against one expected answer.
pub trait Scorer {
    /// Stable scorer name used in run IDs and telemetry.
    fn name(&self) -> &str;

    /// Returns a score in the inclusive `0.0..=1.0` range.
    fn score(&self, expected: &str, actual: &str) -> f64;
}

/// Scorer that gives full credit only for exact string matches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExactMatchScorer;

impl Scorer for ExactMatchScorer {
    fn name(&self) -> &str {
        "exact_match"
    }

    fn score(&self, expected: &str, actual: &str) -> f64 {
        if expected == actual {
            1.0
        } else {
            0.0
        }
    }
}

/// Result for one evaluated case.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseResult {
    /// Test case ID.
    pub case_id: String,
    /// Expected response.
    pub expected: String,
    /// Actual agent response.
    pub actual: String,
    /// Whether the case received full credit.
    pub passed: bool,
    /// Score returned by the scorer.
    pub score: f64,
}

/// Aggregate metrics for an evaluation run.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationSummary {
    /// Number of evaluated cases.
    pub total_cases: usize,
    /// Number of cases with full credit.
    pub passed_cases: usize,
    /// Number of cases below full credit.
    pub failed_cases: usize,
    /// Mean score across evaluated cases.
    pub score: f64,
}

impl EvaluationSummary {
    fn from_results(results: &[CaseResult]) -> Self {
        let total_cases = results.len();
        let passed_cases = results.iter().filter(|result| result.passed).count();
        let failed_cases = total_cases.saturating_sub(passed_cases);
        let score = if total_cases == 0 {
            0.0
        } else {
            results.iter().map(|result| result.score).sum::<f64>() / total_cases as f64
        };

        Self {
            total_cases,
            passed_cases,
            failed_cases,
            score,
        }
    }
}

/// Full output of an evaluation run.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationReport {
    /// Deterministic ID derived from agent, scorer, and ordered test cases.
    pub run_id: String,
    /// Agent name captured at evaluation time.
    pub agent_name: String,
    /// Scorer name captured at evaluation time.
    pub scorer_name: String,
    /// Per-case results in deterministic case-ID order.
    pub results: Vec<CaseResult>,
    /// Aggregate result metrics.
    pub summary: EvaluationSummary,
    /// Telemetry events emitted during the run.
    pub telemetry: Vec<TelemetryEvent>,
}

impl EvaluationReport {
    /// Returns failed case results in report order.
    pub fn failed_cases(&self) -> impl Iterator<Item = &CaseResult> {
        self.results.iter().filter(|result| !result.passed)
    }
}

/// Deterministic evaluator for a configured scorer.
#[derive(Debug, Clone)]
pub struct Evaluator<S = ExactMatchScorer> {
    config: RunConfig,
    scorer: S,
}

impl Default for Evaluator<ExactMatchScorer> {
    fn default() -> Self {
        Self {
            config: RunConfig::default(),
            scorer: ExactMatchScorer,
        }
    }
}

impl<S: Scorer> Evaluator<S> {
    /// Creates an evaluator after validating its configuration.
    pub fn new(config: RunConfig, scorer: S) -> Result<Self, RuntimeError> {
        config.validate()?;

        Ok(Self { config, scorer })
    }

    /// Evaluates an agent against test cases in deterministic case-ID order.
    pub fn evaluate<A: Agent>(
        &self,
        agent: &A,
        cases: &[TestCase],
    ) -> Result<EvaluationReport, RuntimeError> {
        let mut telemetry = TelemetryRecorder::new();

        self.evaluate_with_telemetry(agent, cases, &mut telemetry)
    }

    /// Evaluates an agent while writing events to a caller-owned recorder.
    pub fn evaluate_with_telemetry<A: Agent>(
        &self,
        agent: &A,
        cases: &[TestCase],
        telemetry: &mut TelemetryRecorder,
    ) -> Result<EvaluationReport, RuntimeError> {
        let agent_name = validate_agent_name(agent.name())?;
        let ordered_cases = validate_cases(cases)?;
        let run_id = deterministic_run_id(agent_name, self.scorer.name(), &ordered_cases);
        let first_event = telemetry.events().len();
        let mut results = Vec::with_capacity(ordered_cases.len());

        telemetry.info(
            "run.started",
            telemetry_attributes([
                ("agent", agent_name.to_owned()),
                ("case_count", ordered_cases.len().to_string()),
                ("run_id", run_id.clone()),
                ("scorer", self.scorer.name().to_owned()),
            ]),
        );

        for case in ordered_cases {
            telemetry.info(
                "case.started",
                telemetry_attributes([
                    ("case_id", case.id.clone()),
                    ("run_id", run_id.clone()),
                ]),
            );

            let actual = match agent.respond(&case.prompt) {
                Ok(actual) => actual,
                Err(error) => {
                    telemetry.error(
                        "case.failed",
                        telemetry_attributes([
                            ("case_id", case.id.clone()),
                            ("error_code", error.code().to_owned()),
                            ("run_id", run_id.clone()),
                        ]),
                    );

                    return Err(RuntimeError::AgentFailed {
                        agent: agent_name.to_owned(),
                        case_id: Some(case.id.clone()),
                        message: error.to_string(),
                    });
                }
            };

            if let Err(error) = validate_output_limit(&self.config, case, &actual) {
                record_case_error(telemetry, &run_id, case, &error);
                return Err(error);
            }

            let score = self.scorer.score(&case.expected, &actual);

            if let Err(error) = validate_score(self.scorer.name(), case, score) {
                record_case_error(telemetry, &run_id, case, &error);
                return Err(error);
            }

            let passed = score >= 1.0;
            results.push(CaseResult {
                case_id: case.id.clone(),
                expected: case.expected.clone(),
                actual,
                passed,
                score,
            });

            telemetry.info(
                "case.completed",
                telemetry_attributes([
                    ("case_id", case.id.clone()),
                    ("passed", passed.to_string()),
                    ("run_id", run_id.clone()),
                    ("score", stable_score(score)),
                ]),
            );

            if self.config.stop_on_failure && !passed {
                telemetry.info(
                    "run.stopped",
                    telemetry_attributes([
                        ("case_id", case.id.clone()),
                        ("reason", "first_failure".to_owned()),
                        ("run_id", run_id.clone()),
                    ]),
                );
                break;
            }
        }

        let summary = EvaluationSummary::from_results(&results);

        telemetry.info(
            "run.completed",
            telemetry_attributes([
                ("failed_cases", summary.failed_cases.to_string()),
                ("passed_cases", summary.passed_cases.to_string()),
                ("run_id", run_id.clone()),
                ("score", stable_score(summary.score)),
            ]),
        );

        Ok(EvaluationReport {
            run_id,
            agent_name: agent_name.to_owned(),
            scorer_name: self.scorer.name().to_owned(),
            results,
            summary,
            telemetry: telemetry.events()[first_event..].to_vec(),
        })
    }
}

fn validate_agent_name(name: &str) -> Result<&str, RuntimeError> {
    let trimmed = name.trim();

    if trimmed.is_empty() {
        Err(RuntimeError::EmptyAgentName)
    } else {
        Ok(trimmed)
    }
}

fn validate_cases(cases: &[TestCase]) -> Result<Vec<&TestCase>, RuntimeError> {
    let mut seen = BTreeSet::new();

    for case in cases {
        if case.id.trim().is_empty() {
            return Err(RuntimeError::EmptyCaseId);
        }

        if !seen.insert(case.id.as_str()) {
            return Err(RuntimeError::DuplicateCaseId {
                case_id: case.id.clone(),
            });
        }
    }

    let mut ordered = cases.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.id.cmp(&right.id));

    Ok(ordered)
}

fn validate_output_limit(
    config: &RunConfig,
    case: &TestCase,
    actual: &str,
) -> Result<(), RuntimeError> {
    if let Some(limit) = config.max_output_chars {
        let actual_chars = actual.chars().count();

        if actual_chars > limit {
            return Err(RuntimeError::OutputTooLong {
                case_id: case.id.clone(),
                limit,
                actual: actual_chars,
            });
        }
    }

    Ok(())
}

fn validate_score(scorer: &str, case: &TestCase, score: f64) -> Result<(), RuntimeError> {
    if score.is_nan() || !(0.0..=1.0).contains(&score) {
        return Err(RuntimeError::InvalidScore {
            scorer: scorer.to_owned(),
            case_id: case.id.clone(),
            score,
        });
    }

    Ok(())
}

fn record_case_error(
    telemetry: &mut TelemetryRecorder,
    run_id: &str,
    case: &TestCase,
    error: &RuntimeError,
) {
    telemetry.error(
        "case.failed",
        telemetry_attributes([
            ("case_id", case.id.clone()),
            ("error_code", error.code().to_owned()),
            ("error_kind", format!("{:?}", error.kind())),
            ("run_id", run_id.to_owned()),
        ]),
    );
}

fn deterministic_run_id(agent: &str, scorer: &str, cases: &[&TestCase]) -> String {
    let mut hash = StableHash::new();

    hash.write_str("runtime.v1");
    hash.write_str(agent);
    hash.write_str(scorer);

    for case in cases {
        hash.write_str(&case.id);
        hash.write_str(&case.prompt);
        hash.write_str(&case.expected);

        for tag in &case.tags {
            hash.write_str(tag);
        }
    }

    format!("{:016x}", hash.finish())
}

fn stable_score(score: f64) -> String {
    format!("{score:.6}")
}

struct StableHash {
    value: u64,
}

impl StableHash {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Self {
            value: Self::OFFSET,
        }
    }

    fn write_str(&mut self, value: &str) {
        for byte in value.as_bytes() {
            self.write_byte(*byte);
        }

        self.write_byte(0xff);
    }

    fn write_byte(&mut self, byte: u8) {
        self.value ^= u64::from(byte);
        self.value = self.value.wrapping_mul(Self::PRIME);
    }

    fn finish(self) -> u64 {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorKind, StaticAgent, TelemetryLevel};

    struct BadScorer;

    impl Scorer for BadScorer {
        fn name(&self) -> &str {
            "bad"
        }

        fn score(&self, _expected: &str, _actual: &str) -> f64 {
            2.0
        }
    }

    #[test]
    fn evaluator_sorts_cases_by_id() {
        let evaluator = Evaluator::default();
        let agent = StaticAgent::new("static", "yes");
        let cases = vec![
            TestCase::new("b", "second", "yes"),
            TestCase::new("a", "first", "yes"),
        ];

        let report = evaluator.evaluate(&agent, &cases).expect("report");
        let case_ids = report
            .results
            .iter()
            .map(|result| result.case_id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(case_ids, vec!["a", "b"]);
    }

    #[test]
    fn run_id_is_stable_for_equivalent_case_orderings() {
        let evaluator = Evaluator::default();
        let agent = StaticAgent::new("static", "yes");
        let first = vec![
            TestCase::new("b", "second", "yes"),
            TestCase::new("a", "first", "yes"),
        ];
        let second = vec![
            TestCase::new("a", "first", "yes"),
            TestCase::new("b", "second", "yes"),
        ];

        let first_report = evaluator.evaluate(&agent, &first).expect("first report");
        let second_report = evaluator.evaluate(&agent, &second).expect("second report");

        assert_eq!(first_report.run_id, second_report.run_id);
    }

    #[test]
    fn duplicate_case_ids_are_rejected() {
        let evaluator = Evaluator::default();
        let agent = StaticAgent::new("static", "yes");
        let cases = vec![
            TestCase::new("same", "first", "yes"),
            TestCase::new("same", "second", "yes"),
        ];

        let error = evaluator.evaluate(&agent, &cases).expect_err("error");

        assert_eq!(error.kind(), ErrorKind::Validation);
        assert_eq!(error.code(), "runtime.validation.duplicate_case_id");
    }

    #[test]
    fn stop_on_failure_stops_after_first_failed_case() {
        let evaluator = Evaluator::new(
            RunConfig {
                stop_on_failure: true,
                max_output_chars: None,
            },
            ExactMatchScorer,
        )
        .expect("evaluator");
        let agent = StaticAgent::new("static", "no");
        let cases = vec![
            TestCase::new("a", "first", "yes"),
            TestCase::new("b", "second", "yes"),
        ];

        let report = evaluator.evaluate(&agent, &cases).expect("report");

        assert_eq!(report.summary.total_cases, 1);
        assert_eq!(report.summary.failed_cases, 1);
        assert!(report.telemetry.iter().any(|event| event.name == "run.stopped"));
    }

    #[test]
    fn invalid_scorer_output_is_structured() {
        let evaluator = Evaluator::new(RunConfig::default(), BadScorer).expect("evaluator");
        let agent = StaticAgent::new("static", "yes");
        let cases = vec![TestCase::new("a", "first", "yes")];

        let error = evaluator.evaluate(&agent, &cases).expect_err("error");

        assert_eq!(error.kind(), ErrorKind::Scoring);
        assert_eq!(error.code(), "runtime.scoring.invalid_score");
    }

    #[test]
    fn caller_owned_telemetry_keeps_error_events() {
        let evaluator = Evaluator::new(
            RunConfig {
                stop_on_failure: false,
                max_output_chars: Some(2),
            },
            ExactMatchScorer,
        )
        .expect("evaluator");
        let agent = StaticAgent::new("static", "toolong");
        let cases = vec![TestCase::new("a", "first", "yes")];
        let mut telemetry = TelemetryRecorder::new();

        let error = evaluator
            .evaluate_with_telemetry(&agent, &cases, &mut telemetry)
            .expect_err("error");

        assert_eq!(error.code(), "runtime.agent.output_too_long");
        assert!(telemetry.events().iter().any(|event| {
            event.level == TelemetryLevel::Error && event.name == "case.failed"
        }));
    }
}
