use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

mod registry;
mod sandbox;
mod target;

use registry::resolve_local_registry_dataset;
use sandbox::{
    prepare_container_writable_dir, run_task_scripts, AgentStep, SandboxAgent, SandboxBackend,
};
use target::{RunTarget, TaskRef, TaskSelection};

const EXIT_USAGE: i32 = 2;
const EXIT_UNIMPLEMENTED: i32 = 3;
const EXIT_TASK_FAILED: i32 = 4;

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("seaport: {error}");
        process::exit(error.exit_code());
    }
}

fn run(args: Vec<String>) -> Result<(), CliError> {
    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") => {
            print_help();
            Ok(())
        }
        Some("run") => run_eval(&args[1..]),
        Some("dataset") | Some("datasets") => dataset(&args[1..]),
        Some("init") => init(&args[1..]),
        Some("view") => view(&args[1..]),
        Some(command) => Err(CliError::usage(format!("unknown command `{command}`"))),
    }
}

fn run_eval(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_run_help();
        return Ok(());
    }

    let options = RunOptions::parse(args)?;

    if options.dataset.is_none() && options.path.is_none() {
        return Err(CliError::usage(
            "run requires either `-p <path>` or `-d <dataset>`",
        ));
    }

    let agent = options.agent.as_deref().unwrap_or("oracle");
    let agent = AgentKind::parse(agent)?;

    if agent.requires_model() && options.model.is_none() {
        return Err(CliError::usage(
            "run requires `-m <model>` for model-backed agents",
        ));
    }

    let target = resolve_run_target(&options)?;
    run_target(&target, &options, agent)
}

fn resolve_run_target(options: &RunOptions) -> Result<RunTarget, CliError> {
    if let Some(path) = options.path.as_deref() {
        return RunTarget::from_path(Path::new(path), &options.selection);
    }

    let dataset = options
        .dataset
        .as_deref()
        .ok_or_else(|| CliError::usage("run requires either `-p <path>` or `-d <dataset>`"))?;
    let registry_path = options.registry_path.as_deref().ok_or_else(|| {
        CliError::unimplemented(
            "registered package datasets are not implemented yet; pass `--registry-path <registry.json>` for local Harbor registry datasets",
        )
    })?;
    let resolved = resolve_local_registry_dataset(dataset, Path::new(registry_path))?;

    RunTarget::from_registry_dataset(resolved, &options.selection)
}

fn run_target(target: &RunTarget, options: &RunOptions, agent: AgentKind) -> Result<(), CliError> {
    let run_id = timestamp_id()?;
    let job_root = options
        .jobs_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("jobs"));
    let job_dir = job_root.join(format!("seaport-{run_id}"));
    let mut outcomes = Vec::with_capacity(target.tasks.len());

    for task in &target.tasks {
        outcomes.push(run_trial(task, &job_dir, &run_id, options, agent)?);
    }

    fs::write(
        job_dir.join("config.json"),
        job_config_json(target, options, agent),
    )?;
    fs::write(job_dir.join("result.json"), job_result_json(&outcomes))?;

    let passed = outcomes.iter().all(|outcome| outcome.passed);
    let passed_count = outcomes.iter().filter(|outcome| outcome.passed).count();

    println!("job_dir: {}", job_dir.display());
    println!("target: {}", target.name);
    println!("tasks: {passed_count}/{}", outcomes.len());
    println!("passed: {passed}");

    if passed {
        Ok(())
    } else {
        Err(CliError::task_failed(format!(
            "{}/{} tasks failed",
            outcomes.len() - passed_count,
            outcomes.len()
        )))
    }
}

fn run_trial(
    task: &TaskRef,
    job_dir: &Path,
    run_id: &str,
    options: &RunOptions,
    agent: AgentKind,
) -> Result<TrialOutcome, CliError> {
    let task_name = &task.name;
    let trial_dir = job_dir.join(sanitize_name(task_name));
    let agent_dir = trial_dir.join("agent");
    let verifier_dir = trial_dir.join("verifier");
    let workspace = env::temp_dir().join(format!(
        "seaport-{}-{run_id}-{}",
        agent.as_str(),
        sanitize_name(&task.name)
    ));
    let app_dir = workspace.join("app");
    let logs_dir = workspace.join("logs").join("verifier");

    fs::create_dir_all(&agent_dir)?;
    fs::create_dir_all(&verifier_dir)?;
    fs::create_dir_all(&app_dir)?;
    fs::create_dir_all(&logs_dir)?;

    prepare_container_writable_dir(&app_dir)?;
    prepare_container_writable_dir(&workspace.join("logs"))?;
    prepare_container_writable_dir(&logs_dir)?;

    let outputs = run_task_scripts(
        &task.path,
        run_id,
        &app_dir,
        &logs_dir,
        agent.sandbox_agent(),
        options.backend,
    )?;
    let reward = read_reward(&logs_dir)?;
    let passed = reward.trim() == "1" || reward.trim() == "1.0";

    fs::write(
        agent_dir.join("trajectory.json"),
        trajectory_json(&outputs.agent),
    )?;
    fs::write(
        verifier_dir.join("test-stdout.txt"),
        &outputs.verifier.stdout,
    )?;
    fs::write(
        verifier_dir.join("test-stderr.txt"),
        &outputs.verifier.stderr,
    )?;
    fs::write(verifier_dir.join("reward.txt"), &reward)?;
    fs::write(
        trial_dir.join("config.json"),
        trial_config_json(task_name, agent),
    )?;
    fs::write(
        trial_dir.join("result.json"),
        trial_result_json(passed, reward.trim()),
    )?;

    if let Err(error) = fs::remove_dir_all(&workspace) {
        eprintln!(
            "seaport: warning: could not remove workspace {}: {error}",
            workspace.display()
        );
    }

    println!("task: {task_name}");
    println!("reward: {}", reward.trim());
    println!("passed: {passed}");

    Ok(TrialOutcome {
        task_name: task.name.clone(),
        reward: reward.trim().to_owned(),
        passed,
    })
}

fn validate_task_path(task_path: &Path) -> Result<(), CliError> {
    if !task_path.is_dir() {
        return Err(CliError::usage(format!(
            "task path is not a directory: {}",
            task_path.display()
        )));
    }

    for relative in ["instruction.md", "task.toml", "tests/test.sh"] {
        let path = task_path.join(relative);

        if !path.is_file() {
            return Err(CliError::usage(format!(
                "task is missing required file: {}",
                path.display()
            )));
        }
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentKind {
    Oracle,
    Nop,
}

impl AgentKind {
    fn parse(value: &str) -> Result<Self, CliError> {
        match value {
            "oracle" => Ok(Self::Oracle),
            "nop" => Ok(Self::Nop),
            unsupported => Err(CliError::unimplemented(format!(
                "agent `{unsupported}` is not implemented yet"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Oracle => "oracle",
            Self::Nop => "nop",
        }
    }

    fn requires_model(self) -> bool {
        false
    }

    fn sandbox_agent(self) -> SandboxAgent {
        match self {
            Self::Oracle => SandboxAgent::Oracle,
            Self::Nop => SandboxAgent::Nop,
        }
    }
}

struct TrialOutcome {
    task_name: String,
    reward: String,
    passed: bool,
}

fn read_reward(logs_dir: &Path) -> Result<String, CliError> {
    let reward_path = logs_dir.join("reward.txt");

    if !reward_path.is_file() {
        return Err(CliError::task_failed(format!(
            "verifier did not write {}",
            reward_path.display()
        )));
    }

    Ok(fs::read_to_string(reward_path)?)
}

fn task_name(task_path: &Path) -> Result<String, CliError> {
    let task_toml = fs::read_to_string(task_path.join("task.toml"))?;

    if let Some(name) = toml_section_value(&task_toml, "task", "name") {
        return Ok(name);
    }

    Ok(task_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("task")
        .to_owned())
}

fn toml_section_value(contents: &str, section: &str, key: &str) -> Option<String> {
    let section_header = format!("[{section}]");
    let prefix = format!("{key} = ");
    let mut in_section = false;

    for line in contents.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == section_header;
            continue;
        }

        if in_section {
            if let Some(value) = trimmed.strip_prefix(&prefix) {
                return Some(value.trim().trim_matches('"').to_owned());
            }
        }
    }

    None
}

fn timestamp_id() -> Result<String, CliError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CliError::io(format!("system clock before Unix epoch: {error}")))?;

    Ok(format!("{}-{}", process::id(), duration.as_nanos()))
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn job_config_json(target: &RunTarget, options: &RunOptions, agent: AgentKind) -> String {
    format!(
        "{{\n  \"agent\": \"{}\",\n  \"backend\": \"{}\",\n  \"model\": {},\n  \"target\": \"{}\",\n  \"tasks\": {}\n}}\n",
        agent.as_str(),
        options.backend.as_str(),
        json_option(options.model.as_deref()),
        json_escape(&target.name),
        json_array(
            &target
                .tasks
                .iter()
                .map(|task| task.name.as_str())
                .collect::<Vec<_>>()
        )
    )
}

fn job_result_json(outcomes: &[TrialOutcome]) -> String {
    let passed_count = outcomes.iter().filter(|outcome| outcome.passed).count();
    let reward = aggregate_reward(outcomes);

    format!(
        "{{\n  \"passed\": {},\n  \"reward\": \"{}\",\n  \"tasks_total\": {},\n  \"tasks_passed\": {},\n  \"tasks_failed\": {},\n  \"tasks\": {}\n}}\n",
        passed_count == outcomes.len(),
        json_escape(&reward),
        outcomes.len(),
        passed_count,
        outcomes.len() - passed_count,
        trial_outcomes_json(outcomes)
    )
}

fn trial_result_json(passed: bool, reward: &str) -> String {
    format!(
        "{{\n  \"passed\": {},\n  \"reward\": \"{}\"\n}}\n",
        passed,
        json_escape(reward)
    )
}

fn trial_config_json(task_name: &str, agent: AgentKind) -> String {
    format!(
        "{{\n  \"task\": \"{}\",\n  \"agent\": \"{}\"\n}}\n",
        json_escape(task_name),
        agent.as_str()
    )
}

fn aggregate_reward(outcomes: &[TrialOutcome]) -> String {
    if outcomes.is_empty() {
        return "0".to_owned();
    }

    let mut total = 0.0;

    for outcome in outcomes {
        let Ok(reward) = outcome.reward.parse::<f64>() else {
            return if outcomes.iter().all(|outcome| outcome.passed) {
                "1".to_owned()
            } else {
                "0".to_owned()
            };
        };

        total += reward;
    }

    format!("{:.6}", total / outcomes.len() as f64)
}

fn trial_outcomes_json(outcomes: &[TrialOutcome]) -> String {
    let items = outcomes
        .iter()
        .map(|outcome| {
            format!(
                "{{\"task\":\"{}\",\"passed\":{},\"reward\":\"{}\"}}",
                json_escape(&outcome.task_name),
                outcome.passed,
                json_escape(&outcome.reward)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    format!("[{items}]")
}

fn json_array(values: &[&str]) -> String {
    let items = values
        .iter()
        .map(|value| format!("\"{}\"", json_escape(value)))
        .collect::<Vec<_>>()
        .join(", ");

    format!("[{items}]")
}

fn trajectory_json(step: &AgentStep) -> String {
    format!(
        "{{\n  \"steps\": [\n    {{\n      \"command\": \"{}\",\n      \"status\": {},\n      \"stdout\": \"{}\",\n      \"stderr\": \"{}\"\n    }}\n  ]\n}}\n",
        json_escape(&step.command),
        step.status,
        json_escape(&String::from_utf8_lossy(&step.stdout)),
        json_escape(&String::from_utf8_lossy(&step.stderr))
    )
}

fn json_option(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("\"{}\"", json_escape(value)),
        None => "null".to_owned(),
    }
}

fn json_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect(),
            '\n' => "\\n".chars().collect(),
            '\r' => "\\r".chars().collect(),
            '\t' => "\\t".chars().collect(),
            other => vec![other],
        })
        .collect()
}

fn dataset(args: &[String]) -> Result<(), CliError> {
    match args.first().map(String::as_str) {
        Some("list") => {
            println!("No dataset registry is configured yet.");
            Ok(())
        }
        Some("-h") | Some("--help") | None => {
            print_dataset_help();
            Ok(())
        }
        Some(command) => Err(CliError::usage(format!(
            "unknown dataset command `{command}`"
        ))),
    }
}

fn init(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_init_help();
        return Ok(());
    }

    let task_name = parse_named_value(args, "--task")?
        .ok_or_else(|| CliError::usage("init requires `--task <org/name>`"))?;
    let task_dir = task_name
        .split('/')
        .next_back()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| CliError::usage("task name must look like `<org>/<name>`"))?;
    let root = PathBuf::from(task_dir);

    create_task_skeleton(&root, &task_name)?;
    println!("Created task skeleton at {}", root.display());

    Ok(())
}

fn view(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_view_help();
        return Ok(());
    }

    let jobs_dir = args.first().map(String::as_str).unwrap_or("jobs");

    Err(CliError::unimplemented(format!(
        "viewer is not implemented yet; expected jobs directory: {jobs_dir}"
    )))
}

fn create_task_skeleton(root: &Path, task_name: &str) -> Result<(), CliError> {
    if root.exists() {
        return Err(CliError::usage(format!(
            "target directory already exists: {}",
            root.display()
        )));
    }

    fs::create_dir_all(root.join("environment"))?;
    fs::create_dir_all(root.join("solution"))?;
    fs::create_dir_all(root.join("tests"))?;
    fs::write(
        root.join("instruction.md"),
        "# Task\n\nDescribe the task the agent must complete.\n",
    )?;
    fs::write(root.join("task.toml"), task_toml(task_name))?;
    fs::write(
        root.join("environment").join("Dockerfile"),
        "FROM ubuntu:24.04\nWORKDIR /app\n",
    )?;
    let solve_script = root.join("solution").join("solve.sh");
    let test_script = root.join("tests").join("test.sh");

    fs::write(
        &solve_script,
        "#!/bin/bash\nset -euo pipefail\n\n# Add an oracle solution here.\n",
    )?;
    fs::write(
        &test_script,
        "#!/bin/bash\nset -euo pipefail\n\nmkdir -p /logs/verifier\necho 0 > /logs/verifier/reward.txt\n",
    )?;
    make_executable(&solve_script)?;
    make_executable(&test_script)?;

    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;

    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), CliError> {
    Ok(())
}

fn task_toml(task_name: &str) -> String {
    format!(
        r#"schema_version = "1.0"

[task]
name = "{task_name}"
description = "Describe this task."

[agent]
timeout_sec = 120.0
user = "agent"

[verifier]
timeout_sec = 120.0

[environment]
docker_image = "ubuntu:24.04"
network_mode = "no-network"
"#
    )
}

fn parse_named_value(args: &[String], name: &str) -> Result<Option<String>, CliError> {
    let mut index = 0;

    while index < args.len() {
        if args[index] == name {
            return args
                .get(index + 1)
                .cloned()
                .map(Some)
                .ok_or_else(|| CliError::usage(format!("{name} requires a value")));
        }

        index += 1;
    }

    Ok(None)
}

fn print_help() {
    println!(
        "\
Seaport

Usage:
  seaport <command> [options]

Commands:
  run                 Run a local or registered eval dataset
  dataset list        List registered datasets
  datasets list       Alias for `dataset list`
  init --task <name>  Create a task skeleton
  view [jobs-dir]     View job results

Run `seaport <command> --help` for command-specific help."
    );
}

fn print_run_help() {
    println!(
        "\
Usage:
  seaport run -p <path> [options]
  seaport run -d <dataset> [options]

Options:
  -p, --path <path>       Local task or dataset directory
  -d, --dataset <name>    Registered dataset name
      --registry-path <path>
                          Harbor registry JSON for -d datasets
  -a, --agent <agent>     Agent adapter name; defaults to oracle
  -m, --model <model>     Model identifier
  -n <count>              Concurrency
      --jobs-dir <path>   Directory where job results are written
      --backend <name>    Execution backend: docker or unsafe-local
      --env <name>        Alias for --backend
  -i, --include-task-name <glob>
                          Include only matching task names
  -x, --exclude-task-name <glob>
                          Exclude matching task names
  -l, --n-tasks <count>   Limit number of discovered tasks
      --help              Show this help"
    );
}

fn print_dataset_help() {
    println!(
        "\
Usage:
  seaport dataset list
  seaport datasets list"
    );
}

fn print_init_help() {
    println!(
        "\
Usage:
  seaport init --task <org/name>

Creates:
  instruction.md
  task.toml
  environment/Dockerfile
  solution/solve.sh
  tests/test.sh"
    );
}

fn print_view_help() {
    println!(
        "\
Usage:
  seaport view [jobs-dir]"
    );
}

#[derive(Debug, PartialEq, Eq)]
struct RunOptions {
    path: Option<String>,
    dataset: Option<String>,
    registry_path: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    concurrency: Option<String>,
    backend: SandboxBackend,
    jobs_dir: Option<String>,
    selection: TaskSelection,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            path: None,
            dataset: None,
            registry_path: None,
            agent: None,
            model: None,
            concurrency: None,
            backend: SandboxBackend::Docker,
            jobs_dir: None,
            selection: TaskSelection::default(),
        }
    }
}

impl RunOptions {
    fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut options = Self::default();
        let mut index = 0;

        while index < args.len() {
            let flag = args[index].as_str();

            match flag {
                "-p" | "--path" => {
                    options.path = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "-d" | "--dataset" => {
                    options.dataset = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "--registry-path" => {
                    options.registry_path = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "-a" | "--agent" => {
                    options.agent = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "-m" | "--model" => {
                    options.model = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "-n" => {
                    options.concurrency = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "--backend" | "--env" => {
                    let value = required_value(args, index, flag)?;
                    options.backend = SandboxBackend::parse(&value)?;
                    index += 2;
                }
                "--jobs-dir" => {
                    options.jobs_dir = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "-i" | "--include-task-name" => {
                    options
                        .selection
                        .include_task_names
                        .push(required_value(args, index, flag)?);
                    index += 2;
                }
                "-x" | "--exclude-task-name" => {
                    options
                        .selection
                        .exclude_task_names
                        .push(required_value(args, index, flag)?);
                    index += 2;
                }
                "-l" | "--n-tasks" => {
                    let value = required_value(args, index, flag)?;
                    options.selection.task_limit =
                        Some(value.parse::<usize>().map_err(|error| {
                            CliError::usage(format!("{flag} must be a positive integer: {error}"))
                        })?);
                    index += 2;
                }
                unknown => {
                    return Err(CliError::usage(format!("unknown run option `{unknown}`")));
                }
            }
        }

        Ok(options)
    }
}

fn required_value(args: &[String], index: usize, flag: &str) -> Result<String, CliError> {
    args.get(index + 1)
        .cloned()
        .ok_or_else(|| CliError::usage(format!("{flag} requires a value")))
}

#[derive(Debug)]
struct CliError {
    message: String,
    exit_code: i32,
}

impl CliError {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: EXIT_USAGE,
        }
    }

    fn unimplemented(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: EXIT_UNIMPLEMENTED,
        }
    }

    fn task_failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: EXIT_TASK_FAILED,
        }
    }

    fn io(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: 1,
        }
    }

    fn exit_code(&self) -> i32 {
        self.exit_code
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CliError {}

impl From<std::io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self::io(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_run_options() {
        let args = strings(["-p", "tasks/example", "-a", "codex", "-m", "openai/gpt-5"]);

        let options = RunOptions::parse(&args).expect("options");

        assert_eq!(options.path.as_deref(), Some("tasks/example"));
        assert_eq!(options.agent.as_deref(), Some("codex"));
        assert_eq!(options.model.as_deref(), Some("openai/gpt-5"));
        assert_eq!(options.backend, SandboxBackend::Docker);
    }

    #[test]
    fn parses_registered_dataset_options() {
        let args = strings([
            "-d",
            "bench/example@1.0",
            "--registry-path",
            "registry.json",
            "-a",
            "claude-code",
            "-m",
            "anthropic/claude",
            "-n",
            "8",
            "--env",
            "docker",
            "--jobs-dir",
            "jobs/custom",
            "-i",
            "bench/*",
            "-x",
            "bench/skip-*",
            "-l",
            "5",
        ]);

        let options = RunOptions::parse(&args).expect("options");

        assert_eq!(options.dataset.as_deref(), Some("bench/example@1.0"));
        assert_eq!(options.registry_path.as_deref(), Some("registry.json"));
        assert_eq!(options.concurrency.as_deref(), Some("8"));
        assert_eq!(options.backend, SandboxBackend::Docker);
        assert_eq!(options.jobs_dir.as_deref(), Some("jobs/custom"));
        assert_eq!(options.selection.include_task_names, ["bench/*"]);
        assert_eq!(options.selection.exclude_task_names, ["bench/skip-*"]);
        assert_eq!(options.selection.task_limit, Some(5));
    }

    #[test]
    fn parses_unsafe_local_backend() {
        let args = strings([
            "-p",
            "tasks/example",
            "-a",
            "oracle",
            "--backend",
            "unsafe-local",
        ]);

        let options = RunOptions::parse(&args).expect("options");

        assert_eq!(options.backend, SandboxBackend::UnsafeLocal);
    }

    #[test]
    fn rejects_ambiguous_local_backend() {
        let args = strings(["--backend", "local"]);

        let error = RunOptions::parse(&args).expect_err("error");

        assert_eq!(error.exit_code(), EXIT_USAGE);
    }

    #[test]
    fn rejects_unknown_run_options() {
        let args = strings(["--wat"]);

        let error = RunOptions::parse(&args).expect_err("error");

        assert_eq!(error.exit_code(), EXIT_USAGE);
    }

    #[test]
    fn run_defaults_to_oracle_agent() {
        let root = temp_test_dir("default-agent");
        let task = root.join("task");
        let jobs = root.join("jobs");

        write_oracle_task(&task, "acme/default-agent");

        let args = vec![
            "run".to_owned(),
            "-p".to_owned(),
            task.display().to_string(),
            "--backend".to_owned(),
            "unsafe-local".to_owned(),
            "--jobs-dir".to_owned(),
            jobs.display().to_string(),
        ];

        run(args).expect("default oracle run");

        let job_dir = single_child_dir(&jobs);
        let config = fs::read_to_string(job_dir.join("config.json")).expect("job config");

        assert!(config.contains("\"agent\": \"oracle\""));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn oracle_run_does_not_require_model() {
        let args = strings(["run", "-p", "missing", "-a", "oracle"]);

        let error = run(args).expect_err("error");

        assert_eq!(error.exit_code(), EXIT_USAGE);
    }

    #[test]
    fn runs_nop_agent_without_model_or_solution() {
        let root = temp_test_dir("nop-agent");
        let task = root.join("task");
        let jobs = root.join("jobs");

        write_nop_task(&task, "acme/nop");

        let args = vec![
            "run".to_owned(),
            "-p".to_owned(),
            task.display().to_string(),
            "-a".to_owned(),
            "nop".to_owned(),
            "--backend".to_owned(),
            "unsafe-local".to_owned(),
            "--jobs-dir".to_owned(),
            jobs.display().to_string(),
        ];

        run(args).expect("nop run");

        let job_dir = single_child_dir(&jobs);
        let trial_dir = job_dir.join("acme-nop");
        let result = fs::read_to_string(job_dir.join("result.json")).expect("job result");
        let trajectory = fs::read_to_string(trial_dir.join("agent").join("trajectory.json"))
            .expect("trajectory");

        assert!(result.contains("\"tasks_passed\": 1"));
        assert!(trajectory.contains("\"command\": \"nop\""));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runs_harbor_style_local_dataset() {
        let root = temp_test_dir("local-dataset");
        let dataset = root.join("suite");
        let jobs = root.join("jobs");

        fs::create_dir_all(&dataset).expect("dataset dir");
        fs::write(
            dataset.join("dataset.toml"),
            "[dataset]\nname = \"acme/suite\"\ndescription = \"test suite\"\n",
        )
        .expect("dataset manifest");
        write_oracle_task(&dataset.join("one"), "acme/one");
        write_oracle_task(&dataset.join("two"), "acme/two");

        let args = vec![
            "run".to_owned(),
            "-p".to_owned(),
            dataset.display().to_string(),
            "-a".to_owned(),
            "oracle".to_owned(),
            "--backend".to_owned(),
            "unsafe-local".to_owned(),
            "--jobs-dir".to_owned(),
            jobs.display().to_string(),
            "-i".to_owned(),
            "acme/*".to_owned(),
            "-x".to_owned(),
            "acme/two".to_owned(),
        ];

        run(args).expect("dataset run");

        let job_dir = single_child_dir(&jobs);
        let result = fs::read_to_string(job_dir.join("result.json")).expect("job result");
        let config = fs::read_to_string(job_dir.join("config.json")).expect("job config");

        assert!(result.contains("\"tasks_total\": 1"));
        assert!(result.contains("\"tasks_passed\": 1"));
        assert!(config.contains("\"target\": \"acme/suite\""));
        assert!(config.contains("\"acme/one\""));
        assert!(!config.contains("\"acme/two\""));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runs_local_registry_dataset() {
        let root = temp_test_dir("registry-dataset");
        let tasks = root.join("tasks");
        let jobs = root.join("jobs");
        let registry = root.join("registry.json");

        write_oracle_task(&tasks.join("one"), "acme/one");
        write_oracle_task(&tasks.join("two"), "acme/two");
        fs::write(
            &registry,
            format!(
                "[{{\"name\":\"acme/suite\",\"version\":\"head\",\"description\":\"suite\",\"tasks\":[{{\"name\":\"acme/one\",\"path\":\"{}\"}},{{\"name\":\"acme/two\",\"path\":\"{}\"}}]}}]\n",
                tasks.join("one").display(),
                tasks.join("two").display()
            ),
        )
        .expect("registry");

        let args = vec![
            "run".to_owned(),
            "-d".to_owned(),
            "acme/suite".to_owned(),
            "--registry-path".to_owned(),
            registry.display().to_string(),
            "-a".to_owned(),
            "oracle".to_owned(),
            "--backend".to_owned(),
            "unsafe-local".to_owned(),
            "--jobs-dir".to_owned(),
            jobs.display().to_string(),
            "-l".to_owned(),
            "1".to_owned(),
        ];

        run(args).expect("registry run");

        let job_dir = single_child_dir(&jobs);
        let result = fs::read_to_string(job_dir.join("result.json")).expect("job result");
        let config = fs::read_to_string(job_dir.join("config.json")).expect("job config");

        assert!(result.contains("\"tasks_total\": 1"));
        assert!(result.contains("\"tasks_passed\": 1"));
        assert!(config.contains("\"target\": \"acme/suite\""));

        let _ = fs::remove_dir_all(root);
    }

    fn strings<const N: usize>(items: [&str; N]) -> Vec<String> {
        items.into_iter().map(str::to_owned).collect()
    }

    fn temp_test_dir(name: &str) -> PathBuf {
        let id = timestamp_id().expect("timestamp");
        env::temp_dir().join(format!("seaport-{name}-{id}"))
    }

    fn write_oracle_task(root: &Path, name: &str) {
        fs::create_dir_all(root.join("solution")).expect("solution dir");
        fs::create_dir_all(root.join("tests")).expect("tests dir");
        fs::write(root.join("instruction.md"), "Create output.txt.\n").expect("instruction");
        fs::write(root.join("task.toml"), task_toml(name)).expect("task toml");

        let solve = root.join("solution").join("solve.sh");
        let test = root.join("tests").join("test.sh");

        fs::write(
            &solve,
            "#!/bin/bash\nset -euo pipefail\nprintf 'ok\\n' > \"$APP_DIR/output.txt\"\n",
        )
        .expect("solve");
        fs::write(
            &test,
            "#!/bin/bash\nset -euo pipefail\nmkdir -p \"$LOGS_DIR\"\nif [ \"$(cat \"$APP_DIR/output.txt\")\" = \"ok\" ]; then echo 1 > \"$LOGS_DIR/reward.txt\"; else echo 0 > \"$LOGS_DIR/reward.txt\"; fi\n",
        )
        .expect("test");

        make_executable(&solve).expect("solve executable");
        make_executable(&test).expect("test executable");
    }

    fn write_nop_task(root: &Path, name: &str) {
        fs::create_dir_all(root.join("tests")).expect("tests dir");
        fs::write(root.join("instruction.md"), "No-op task.\n").expect("instruction");
        fs::write(root.join("task.toml"), task_toml(name)).expect("task toml");

        let test = root.join("tests").join("test.sh");

        fs::write(
            &test,
            "#!/bin/bash\nset -euo pipefail\nmkdir -p \"$LOGS_DIR\"\necho 1 > \"$LOGS_DIR/reward.txt\"\n",
        )
        .expect("test");

        make_executable(&test).expect("test executable");
    }

    fn single_child_dir(path: &Path) -> PathBuf {
        let children = fs::read_dir(path)
            .expect("read jobs")
            .map(|entry| entry.expect("entry").path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();

        assert_eq!(children.len(), 1);
        children.into_iter().next().expect("job dir")
    }
}
