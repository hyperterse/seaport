use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

const EXIT_USAGE: i32 = 2;
const EXIT_UNIMPLEMENTED: i32 = 3;

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

    if options.agent.is_none() {
        return Err(CliError::usage("run requires `-a <agent>`"));
    }

    if options.model.is_none() {
        return Err(CliError::usage("run requires `-m <model>`"));
    }

    Err(CliError::unimplemented(
        "sandboxed task execution is not implemented yet; next step is wiring `seaport run` to local task directories",
    ))
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
      --env <provider>    Sandbox provider
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

#[derive(Debug, Default, PartialEq, Eq)]
struct RunOptions {
    path: Option<String>,
    dataset: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    concurrency: Option<String>,
    environment: Option<String>,
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
                "--env" => {
                    options.environment = Some(required_value(args, index, flag)?);
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
    fn from(error: std::io::Error) -> Self {
        Self {
            message: error.to_string(),
            exit_code: 1,
        }
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
        ]);

        let options = RunOptions::parse(&args).expect("options");

        assert_eq!(options.dataset.as_deref(), Some("bench/example@1.0"));
        assert_eq!(options.concurrency.as_deref(), Some("8"));
        assert_eq!(options.environment.as_deref(), Some("docker"));
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

    fn strings<const N: usize>(items: [&str; N]) -> Vec<String> {
        items.into_iter().map(str::to_owned).collect()
    }
}
