use std::time::Instant;

use seaport::{Evaluator, StaticAgent, TestCase};

fn main() {
    let evaluator = Evaluator::default();
    let agent = StaticAgent::new("benchmark-static", "ok");
    let cases = (0..1_000)
        .map(|index| TestCase::new(format!("case.{index:04}"), "prompt", "ok"))
        .collect::<Vec<_>>();

    let started = Instant::now();

    for _ in 0..100 {
        let report = evaluator.evaluate(&agent, &cases).expect("report");
        assert_eq!(report.summary.passed_cases, cases.len());
    }

    let elapsed = started.elapsed();

    println!("evaluated 100000 cases in {elapsed:?}");
}
