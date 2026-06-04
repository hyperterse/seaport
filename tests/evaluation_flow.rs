use runtime::{
    Agent, EchoAgent, ErrorKind, EvaluationReport, Evaluator, RuntimeError, StaticAgent,
    TelemetryLevel, TelemetryRecorder, TestCase,
};

struct FailingAgent;

impl Agent for FailingAgent {
    fn name(&self) -> &str {
        "failing"
    }

    fn respond(&self, _prompt: &str) -> Result<String, RuntimeError> {
        Err(RuntimeError::AgentFailed {
            agent: "failing".to_owned(),
            case_id: None,
            message: "intentional test failure".to_owned(),
        })
    }
}

#[test]
fn exact_match_report_contains_summary_and_telemetry() {
    let evaluator = Evaluator::default();
    let agent = EchoAgent::new("echo");
    let cases = vec![
        TestCase::new("math.add", "2 + 2", "2 + 2"),
        TestCase::new("text.copy", "copy me", "copy me"),
    ];

    let report = evaluator.evaluate(&agent, &cases).expect("report");

    assert_eq!(report.summary.total_cases, 2);
    assert_eq!(report.summary.passed_cases, 2);
    assert_eq!(report.summary.failed_cases, 0);
    assert_eq!(report.summary.score, 1.0);
    assert!(report.telemetry.iter().any(|event| event.name == "run.completed"));
}

#[test]
fn failed_cases_iterator_returns_only_failures() {
    let evaluator = Evaluator::default();
    let agent = StaticAgent::new("static", "wrong");
    let cases = vec![
        TestCase::new("first", "prompt", "right"),
        TestCase::new("second", "prompt", "wrong"),
    ];

    let report = evaluator.evaluate(&agent, &cases).expect("report");
    let failed = failed_case_ids(&report);

    assert_eq!(failed, vec!["first"]);
}

#[test]
fn agent_failure_returns_structured_error_and_records_telemetry() {
    let evaluator = Evaluator::default();
    let cases = vec![TestCase::new("case", "prompt", "answer")];
    let mut telemetry = TelemetryRecorder::new();

    let error = evaluator
        .evaluate_with_telemetry(&FailingAgent, &cases, &mut telemetry)
        .expect_err("error");

    assert_eq!(error.kind(), ErrorKind::Agent);
    assert_eq!(error.code(), "runtime.agent.failed");
    assert!(telemetry.events().iter().any(|event| {
        event.level == TelemetryLevel::Error && event.name == "case.failed"
    }));
}

fn failed_case_ids(report: &EvaluationReport) -> Vec<&str> {
    report
        .failed_cases()
        .map(|result| result.case_id.as_str())
        .collect()
}
