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
