use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::CliError;

const DEFAULT_DOCKER_IMAGE: &str = "ubuntu:24.04";
const CONTAINER_USER: &str = "1000:1000";
const CONTAINER_MEMORY: &str = "1g";
const CONTAINER_CPUS: &str = "1.0";
const CONTAINER_PIDS_LIMIT: &str = "256";
const DOCKER_BUILD_TIMEOUT: Duration = Duration::from_secs(600);

pub(crate) struct ScriptOutputs {
    pub(crate) solution: Output,
    pub(crate) verifier: Output,
}

pub(crate) fn run_task_scripts(
    task_path: &Path,
    run_id: &str,
    app_dir: &Path,
    logs_dir: &Path,
    backend: SandboxBackend,
) -> Result<ScriptOutputs, CliError> {
    let environment = task_environment(task_path)?;

    match backend {
        SandboxBackend::Docker => {
            run_scripts_in_docker(task_path, run_id, app_dir, logs_dir, &environment)
        }
        SandboxBackend::UnsafeLocal => {
            run_scripts_locally(task_path, app_dir, logs_dir, &environment)
        }
    }
}

#[cfg(unix)]
pub(crate) fn prepare_container_writable_dir(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o777);
    fs::set_permissions(path, permissions)?;

    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn prepare_container_writable_dir(_path: &Path) -> Result<(), CliError> {
    Ok(())
}

fn run_scripts_in_docker(
    task_path: &Path,
    run_id: &str,
    app_dir: &Path,
    logs_dir: &Path,
    environment: &TaskEnvironment,
) -> Result<ScriptOutputs, CliError> {
    ensure_docker_available()?;

    let image = prepare_docker_image(task_path, run_id, environment)?;
    let result = (|| {
        let solution = run_script_in_docker(DockerScriptRun {
            image: &image.reference,
            environment,
            run_id,
            task_path,
            app_dir,
            logs_dir,
            script: "solution/solve.sh",
            timeout: environment.agent_timeout,
        })?;
        let verifier = run_script_in_docker(DockerScriptRun {
            image: &image.reference,
            environment,
            run_id,
            task_path,
            app_dir,
            logs_dir,
            script: "tests/test.sh",
            timeout: environment.verifier_timeout,
        })?;

        Ok(ScriptOutputs { solution, verifier })
    })();

    if image.remove_after_run {
        cleanup_docker_image(&image.reference);
    }

    result
}

struct DockerImage {
    reference: String,
    remove_after_run: bool,
}

struct TaskEnvironment {
    image: String,
    network: DockerNetwork,
    agent_timeout: Duration,
    verifier_timeout: Duration,
}

#[derive(Clone, Copy)]
enum DockerNetwork {
    None,
    Bridge,
}

impl DockerNetwork {
    fn as_docker_arg(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bridge => "bridge",
        }
    }
}

fn task_environment(task_path: &Path) -> Result<TaskEnvironment, CliError> {
    let task_toml = fs::read_to_string(task_path.join("task.toml"))?;
    let image = toml_string_value(&task_toml, "docker_image")
        .unwrap_or_else(|| DEFAULT_DOCKER_IMAGE.to_owned());
    let agent_timeout = toml_duration_value(&task_toml, "agent", "timeout_sec")?;
    let verifier_timeout = toml_duration_value(&task_toml, "verifier", "timeout_sec")?;
    let network = match toml_string_value(&task_toml, "network_mode")
        .unwrap_or_else(|| "no-network".to_owned())
        .as_str()
    {
        "no-network" | "none" => DockerNetwork::None,
        "bridge" => DockerNetwork::Bridge,
        value => {
            return Err(CliError::usage(format!(
                "unsupported environment.network_mode `{value}`; use `no-network` or `bridge`"
            )));
        }
    };

    Ok(TaskEnvironment {
        image,
        network,
        agent_timeout,
        verifier_timeout,
    })
}

fn toml_string_value(contents: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} = ");

    contents.lines().find_map(|line| {
        line.trim()
            .strip_prefix(&prefix)
            .map(|value| value.trim().trim_matches('"').to_owned())
    })
}

fn toml_duration_value(contents: &str, section: &str, key: &str) -> Result<Duration, CliError> {
    match toml_section_value(contents, section, key) {
        Some(value) => {
            let seconds = value.parse::<f64>().map_err(|error| {
                CliError::usage(format!("[{section}].{key} must be a number: {error}"))
            })?;

            if seconds <= 0.0 {
                return Err(CliError::usage(format!(
                    "[{section}].{key} must be greater than zero"
                )));
            }

            Ok(Duration::from_secs_f64(seconds))
        }
        None => Ok(Duration::from_secs(120)),
    }
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

fn prepare_docker_image(
    task_path: &Path,
    run_id: &str,
    environment: &TaskEnvironment,
) -> Result<DockerImage, CliError> {
    let dockerfile = task_path.join("environment").join("Dockerfile");

    if !dockerfile.is_file() {
        return Ok(DockerImage {
            reference: environment.image.clone(),
            remove_after_run: false,
        });
    }

    let tag = format!("seaport-task-{run_id}");
    let environment_dir = dockerfile
        .parent()
        .ok_or_else(|| CliError::usage("environment/Dockerfile has no parent directory"))?;
    let mut command = Command::new("docker");
    command
        .args([
            "build",
            "--pull=false",
            "--network",
            environment.network.as_docker_arg(),
            "-q",
            "-t",
            &tag,
        ])
        .arg(environment_dir);
    let timed_output = run_command_with_timeout(command, DOCKER_BUILD_TIMEOUT)?;
    let output = timed_output.output;

    if timed_output.timed_out {
        cleanup_docker_image(&tag);
        return Err(CliError::task_failed(format!(
            "docker image build timed out after {:.3}s for {}",
            DOCKER_BUILD_TIMEOUT.as_secs_f64(),
            dockerfile.display()
        )));
    }

    if !output.status.success() {
        return Err(CliError::task_failed(format!(
            "docker image build failed for {} (status: {})\nstdout:\n{}\nstderr:\n{}",
            dockerfile.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(DockerImage {
        reference: tag,
        remove_after_run: true,
    })
}

fn ensure_docker_available() -> Result<(), CliError> {
    let output = Command::new("docker")
        .arg("version")
        .arg("--format")
        .arg("{{.Server.Version}}")
        .output();

    match output {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(CliError::task_failed(format!(
            "docker backend could not reach the Docker daemon (status: {})\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(CliError::usage(
            "docker backend requires Docker on PATH; install Docker or pass `--backend unsafe-local` for trusted local development",
        )),
        Err(error) => Err(CliError::io(error.to_string())),
    }
}

struct DockerScriptRun<'a> {
    image: &'a str,
    environment: &'a TaskEnvironment,
    run_id: &'a str,
    task_path: &'a Path,
    app_dir: &'a Path,
    logs_dir: &'a Path,
    script: &'a str,
    timeout: Duration,
}

fn run_script_in_docker(run: DockerScriptRun<'_>) -> Result<Output, CliError> {
    let logs_root = run
        .logs_dir
        .parent()
        .ok_or_else(|| CliError::usage("logs directory has no parent"))?;
    let container_name = docker_container_name(run.run_id, run.script);
    let command = docker_run_command(
        run.image,
        run.environment,
        &container_name,
        run.task_path,
        run.app_dir,
        logs_root,
        run.script,
    );
    let timed_output = run_command_with_timeout(command, run.timeout)?;
    let output = timed_output.output;

    if timed_output.timed_out {
        cleanup_docker_container(&container_name);
        return Err(CliError::task_failed(format!(
            "sandboxed docker command timed out after {:.3}s: {}",
            run.timeout.as_secs_f64(),
            run.script
        )));
    }

    if !output.status.success() {
        return Err(CliError::task_failed(format!(
            "sandboxed docker command failed: {} (status: {})\nstdout:\n{}\nstderr:\n{}",
            run.script,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(output)
}

fn docker_run_command(
    image: &str,
    environment: &TaskEnvironment,
    container_name: &str,
    task_path: &Path,
    app_dir: &Path,
    logs_root: &Path,
    script: &str,
) -> Command {
    let mut command = Command::new("docker");
    command
        .args([
            "run",
            "--rm",
            "--name",
            container_name,
            "--network",
            environment.network.as_docker_arg(),
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--pids-limit",
            CONTAINER_PIDS_LIMIT,
            "--memory",
            CONTAINER_MEMORY,
            "--memory-swap",
            CONTAINER_MEMORY,
            "--cpus",
            CONTAINER_CPUS,
            "--read-only",
            "--user",
            CONTAINER_USER,
            "--workdir",
            "/app",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,nodev,size=64m",
            "--tmpfs",
            "/run:rw,nosuid,nodev,size=16m",
            "--env",
            "APP_DIR=/app",
            "--env",
            "LOGS_DIR=/logs/verifier",
            "--env",
            "SEAPORT_TASK_DIR=/seaport/task",
        ])
        .arg("--mount")
        .arg(format!(
            "type=bind,source={},target=/app",
            app_dir.display()
        ))
        .arg("--mount")
        .arg(format!(
            "type=bind,source={},target=/logs",
            logs_root.display()
        ))
        .arg("--mount")
        .arg(format!(
            "type=bind,source={},target=/seaport/task,readonly",
            task_path.display()
        ))
        .arg(image)
        .arg("bash")
        .arg(format!("/seaport/task/{script}"));

    command
}

fn docker_container_name(run_id: &str, script: &str) -> String {
    let phase = script
        .split('/')
        .next()
        .map(sanitize_name)
        .unwrap_or_else(|| "script".to_owned());

    format!("seaport-{phase}-{run_id}")
}

fn cleanup_docker_container(container_name: &str) {
    match Command::new("docker")
        .args(["container", "rm", "-f", container_name])
        .output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            eprintln!(
                "seaport: warning: could not remove docker container {container_name} (status: {})\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(error) => {
            eprintln!(
                "seaport: warning: could not remove docker container {container_name}: {error}"
            );
        }
    }
}

fn cleanup_docker_image(image: &str) {
    match Command::new("docker")
        .args(["image", "rm", "-f", image])
        .output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            eprintln!(
                "seaport: warning: could not remove docker image {image} (status: {})\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(error) => {
            eprintln!("seaport: warning: could not remove docker image {image}: {error}");
        }
    }
}

fn run_scripts_locally(
    task_path: &Path,
    app_dir: &Path,
    logs_dir: &Path,
    environment: &TaskEnvironment,
) -> Result<ScriptOutputs, CliError> {
    let solution = task_path.join("solution").join("solve.sh");
    let verifier = task_path.join("tests").join("test.sh");
    let solution = run_script_locally(
        &solution,
        task_path,
        app_dir,
        logs_dir,
        environment.agent_timeout,
    )?;
    let verifier = run_script_locally(
        &verifier,
        task_path,
        app_dir,
        logs_dir,
        environment.verifier_timeout,
    )?;

    Ok(ScriptOutputs { solution, verifier })
}

fn run_script_locally(
    script: &Path,
    task_path: &Path,
    app_dir: &Path,
    logs_dir: &Path,
    timeout: Duration,
) -> Result<Output, CliError> {
    let mut command = Command::new("bash");
    command
        .arg(script)
        .current_dir(app_dir)
        .env("APP_DIR", app_dir)
        .env("LOGS_DIR", logs_dir)
        .env("SEAPORT_TASK_DIR", task_path);
    let timed_output = run_command_with_timeout(command, timeout)?;
    let output = timed_output.output;

    if timed_output.timed_out {
        return Err(CliError::task_failed(format!(
            "unsafe local script timed out after {:.3}s: {}",
            timeout.as_secs_f64(),
            script.display()
        )));
    }

    if !output.status.success() {
        return Err(CliError::task_failed(format!(
            "script failed: {} (status: {})\nstdout:\n{}\nstderr:\n{}",
            script.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(output)
}

struct TimedOutput {
    output: Output,
    timed_out: bool,
}

fn run_command_with_timeout(
    mut command: Command,
    timeout: Duration,
) -> Result<TimedOutput, CliError> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let started = Instant::now();

    loop {
        if child.try_wait()?.is_some() {
            return Ok(TimedOutput {
                output: child.wait_with_output()?,
                timed_out: false,
            });
        }

        if started.elapsed() >= timeout {
            let _ = child.kill();
            return Ok(TimedOutput {
                output: child.wait_with_output()?,
                timed_out: true,
            });
        }

        thread::sleep(Duration::from_millis(25));
    }
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SandboxBackend {
    #[default]
    Docker,
    UnsafeLocal,
}

impl SandboxBackend {
    pub(crate) fn parse(value: &str) -> Result<Self, CliError> {
        match value {
            "docker" => Ok(Self::Docker),
            "unsafe-local" => Ok(Self::UnsafeLocal),
            "local" => Err(CliError::usage(
                "`local` is not a safe backend name; use `unsafe-local` for trusted development only",
            )),
            unknown => Err(CliError::usage(format!(
                "unknown backend `{unknown}`; use `docker` or `unsafe-local`"
            ))),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::UnsafeLocal => "unsafe-local",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_command_uses_sandbox_flags() {
        let environment = TaskEnvironment {
            image: "ubuntu:24.04".to_owned(),
            network: DockerNetwork::None,
            agent_timeout: Duration::from_secs(1),
            verifier_timeout: Duration::from_secs(1),
        };
        let command = docker_run_command(
            "seaport-task-test",
            &environment,
            "seaport-test-container",
            Path::new("/tmp/task"),
            Path::new("/tmp/app"),
            Path::new("/tmp/logs"),
            "tests/test.sh",
        );
        let args = command_args(command);

        assert!(args
            .windows(2)
            .any(|window| window == ["--network", "none"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--cap-drop", "ALL"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--security-opt", "no-new-privileges"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--pids-limit", "256"]));
        assert!(args.windows(2).any(|window| window == ["--memory", "1g"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--memory-swap", "1g"]));
        assert!(args.windows(2).any(|window| window == ["--cpus", "1.0"]));
        assert!(args.iter().any(|arg| arg == "--read-only"));
        assert!(args
            .windows(2)
            .any(|window| window == ["--user", "1000:1000"]));
        assert!(args
            .iter()
            .any(|arg| arg == "type=bind,source=/tmp/task,target=/seaport/task,readonly"));
    }

    fn command_args(command: Command) -> Vec<String> {
        command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }
}
