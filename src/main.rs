use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{self, Output};
use std::time::{SystemTime, UNIX_EPOCH};

mod sandbox;

use sandbox::{prepare_container_writable_dir, run_task_scripts, SandboxBackend};

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

    let agent = options
        .agent
        .as_deref()
        .ok_or_else(|| CliError::usage("run requires `-a <agent>`"))?;

    if agent != "oracle" && options.model.is_none() {
        return Err(CliError::usage(
            "run requires `-m <model>` unless `-a oracle` is used",
        ));
    }

    if options.dataset.is_some() {
        return Err(CliError::unimplemented(
            "registered datasets are not implemented yet; use `-p <path>` for local tasks",
        ));
    }

    let task_path = options
        .path
        .as_deref()
        .ok_or_else(|| CliError::usage("run requires `-p <path>` for local tasks"))?;

    if agent != "oracle" {
        return Err(CliError::unimplemented(
            "only the oracle agent is implemented for local task execution",
        ));
    }

    run_oracle_task(Path::new(task_path), &options)
}

fn run_oracle_task(task_path: &Path, options: &RunOptions) -> Result<(), CliError> {
    validate_task_path(task_path)?;

    let task_path = task_path.canonicalize()?;
    let task_name = task_name(&task_path)?;
    let run_id = timestamp_id()?;
    let job_root = options
        .jobs_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("jobs"));
    let job_dir = job_root.join(format!("seaport-{run_id}"));
    let trial_dir = job_dir.join(sanitize_name(&task_name));
    let agent_dir = trial_dir.join("agent");
    let verifier_dir = trial_dir.join("verifier");
    let workspace = env::temp_dir().join(format!("seaport-oracle-{run_id}"));
    let app_dir = workspace.join("app");
    let logs_dir = workspace.join("logs").join("verifier");

    fs::create_dir_all(&agent_dir)?;
    fs::create_dir_all(&verifier_dir)?;
    fs::create_dir_all(&app_dir)?;
    fs::create_dir_all(&logs_dir)?;

    prepare_container_writable_dir(&app_dir)?;
    prepare_container_writable_dir(&workspace.join("logs"))?;
    prepare_container_writable_dir(&logs_dir)?;

    let outputs = run_task_scripts(&task_path, &run_id, &app_dir, &logs_dir, options.backend)?;
    let reward = read_reward(&logs_dir)?;
    let passed = reward.trim() == "1" || reward.trim() == "1.0";

    fs::write(
        agent_dir.join("trajectory.json"),
        trajectory_json(&outputs.solution),
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
        job_dir.join("config.json"),
        job_config_json(&task_name, options),
    )?;
    fs::write(
        job_dir.join("result.json"),
        job_result_json(passed, reward.trim()),
    )?;
    fs::write(trial_dir.join("config.json"), trial_config_json(&task_name))?;
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

    println!("job_dir: {}", job_dir.display());
    println!("task: {task_name}");
    println!("reward: {}", reward.trim());
    println!("passed: {passed}");

    if passed {
        Ok(())
    } else {
        Err(CliError::task_failed(format!(
            "oracle task failed with reward {}",
            reward.trim()
        )))
    }
}

fn validate_task_path(task_path: &Path) -> Result<(), CliError> {
    if !task_path.is_dir() {
        return Err(CliError::usage(format!(
            "task path is not a directory: {}",
            task_path.display()
        )));
    }

    for relative in [
        "instruction.md",
        "task.toml",
        "solution/solve.sh",
        "tests/test.sh",
    ] {
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

    for line in task_toml.lines() {
        let trimmed = line.trim();

        if let Some(value) = trimmed.strip_prefix("name = ") {
            return Ok(value.trim_matches('"').to_owned());
        }
    }

    Ok(task_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("task")
        .to_owned())
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

fn job_config_json(task_name: &str, options: &RunOptions) -> String {
    format!(
        "{{\n  \"agent\": \"oracle\",\n  \"backend\": \"{}\",\n  \"model\": {},\n  \"task\": \"{}\"\n}}\n",
        options.backend.as_str(),
        json_option(options.model.as_deref()),
        json_escape(task_name)
    )
}

fn job_result_json(passed: bool, reward: &str) -> String {
    format!(
        "{{\n  \"passed\": {},\n  \"reward\": \"{}\"\n}}\n",
        passed,
        json_escape(reward)
    )
}

fn trial_config_json(task_name: &str) -> String {
    format!("{{\n  \"task\": \"{}\"\n}}\n", json_escape(task_name))
}

fn trial_result_json(passed: bool, reward: &str) -> String {
    job_result_json(passed, reward)
}

fn trajectory_json(output: &Output) -> String {
    format!(
        "{{\n  \"steps\": [\n    {{\n      \"command\": \"solution/solve.sh\",\n      \"status\": {},\n      \"stdout\": \"{}\",\n      \"stderr\": \"{}\"\n    }}\n  ]\n}}\n",
        output.status.code().unwrap_or_default(),
        json_escape(&String::from_utf8_lossy(&output.stdout)),
        json_escape(&String::from_utf8_lossy(&output.stderr))
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
  seaport run -p <path> -a <agent> -m <model> [options]
  seaport run -d <dataset> -a <agent> -m <model> [options]

Options:
  -p, --path <path>       Local task or dataset directory
  -d, --dataset <name>    Registered dataset name
  -a, --agent <agent>     Agent adapter name
  -m, --model <model>     Model identifier
  -n <count>              Concurrency
      --jobs-dir <path>   Directory where job results are written
      --backend <name>    Execution backend: docker or unsafe-local
      --env <name>        Alias for --backend
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
    agent: Option<String>,
    model: Option<String>,
    concurrency: Option<String>,
    backend: SandboxBackend,
    jobs_dir: Option<String>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            path: None,
            dataset: None,
            agent: None,
            model: None,
            concurrency: None,
            backend: SandboxBackend::Docker,
            jobs_dir: None,
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
        ]);

        let options = RunOptions::parse(&args).expect("options");

        assert_eq!(options.dataset.as_deref(), Some("bench/example@1.0"));
        assert_eq!(options.concurrency.as_deref(), Some("8"));
        assert_eq!(options.backend, SandboxBackend::Docker);
        assert_eq!(options.jobs_dir.as_deref(), Some("jobs/custom"));
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
    fn run_requires_agent_and_model() {
        let args = strings(["run", "-p", "tasks/example"]);

        let error = run(args).expect_err("error");

        assert_eq!(error.exit_code(), EXIT_USAGE);
    }

    #[test]
    fn oracle_run_does_not_require_model() {
        let args = strings(["run", "-p", "missing", "-a", "oracle"]);

        let error = run(args).expect_err("error");

        assert_eq!(error.exit_code(), EXIT_USAGE);
    }

    fn strings<const N: usize>(items: [&str; N]) -> Vec<String> {
        items.into_iter().map(str::to_owned).collect()
    }
}
