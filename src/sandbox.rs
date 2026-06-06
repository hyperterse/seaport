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
    pub(crate) agent: AgentStep,
    pub(crate) verifier: Output,
}

pub(crate) struct AgentStep {
    pub(crate) command: String,
    pub(crate) status: i32,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

impl AgentStep {
    fn from_output(command: impl Into<String>, output: Output) -> Self {
        Self {
            command: command.into(),
            status: output.status.code().unwrap_or_default(),
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }

    fn nop() -> Self {
        Self {
            command: "nop".to_owned(),
            status: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SandboxAgent {
    Oracle,
    Nop,
    External(ExternalAgent),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExternalAgent {
    pub(crate) name: String,
    pub(crate) command: String,
    pub(crate) model: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PhaseEnvs {
    pub(crate) agent: Vec<(String, String)>,
    pub(crate) verifier: Vec<(String, String)>,
}

pub(crate) fn run_task_scripts(
    task_path: &Path,
    run_id: &str,
    app_dir: &Path,
    logs_dir: &Path,
    agent: &SandboxAgent,
    envs: &PhaseEnvs,
    backend: SandboxBackend,
) -> Result<ScriptOutputs, CliError> {
    let environment = task_environment(task_path)?;
    prepare_task_file_workspace(task_path, app_dir)?;

    match backend {
        SandboxBackend::Docker => run_scripts_in_docker(
            task_path,
            run_id,
            app_dir,
            logs_dir,
            agent,
            envs,
            &environment,
        ),
        SandboxBackend::UnsafeLocal => {
            run_scripts_locally(task_path, app_dir, logs_dir, agent, envs, &environment)
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

fn prepare_task_file_workspace(task_path: &Path, app_dir: &Path) -> Result<(), CliError> {
    let source = task_path.join("environment").join("task_file");

    if !source.is_dir() {
        return Ok(());
    }

    let target = app_dir.join("task_file");

    if target.exists() {
        fs::remove_dir_all(&target)?;
    }

    copy_dir_all(&source, &target)?;
    prepare_container_writable_tree(&target)
}

fn copy_dir_all(source: &Path, target: &Path) -> Result<(), CliError> {
    fs::create_dir_all(target)?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            copy_dir_all(&source_path, &target_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &target_path)?;
        } else {
            return Err(CliError::usage(format!(
                "unsupported entry in environment/task_file: {}",
                source_path.display()
            )));
        }
    }

    Ok(())
}

#[cfg(unix)]
fn prepare_container_writable_tree(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path)?;
    let mut permissions = metadata.permissions();

    permissions.set_mode(if metadata.is_dir() { 0o777 } else { 0o666 });
    fs::set_permissions(path, permissions)?;

    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            prepare_container_writable_tree(&entry?.path())?;
        }
    }

    Ok(())
}

#[cfg(not(unix))]
fn prepare_container_writable_tree(_path: &Path) -> Result<(), CliError> {
    Ok(())
}

fn run_scripts_in_docker(
    task_path: &Path,
    run_id: &str,
    app_dir: &Path,
    logs_dir: &Path,
    agent_kind: &SandboxAgent,
    envs: &PhaseEnvs,
    environment: &TaskEnvironment,
) -> Result<ScriptOutputs, CliError> {
    ensure_docker_available()?;

    let image = prepare_docker_image(task_path, run_id, environment)?;
    let result = (|| {
        let agent = match agent_kind {
            SandboxAgent::Oracle => AgentStep::from_output(
                "solution/solve.sh",
                run_script_in_docker(DockerScriptRun {
                    image: &image.reference,
                    run_id,
                    task_path,
                    app_dir,
                    logs_dir,
                    script: "solution/solve.sh",
                    network: environment.agent_network,
                    env: &envs.agent,
                    timeout: environment.agent_timeout,
                })?,
            ),
            SandboxAgent::Nop => AgentStep::nop(),
            SandboxAgent::External(agent) => AgentStep::from_output(
                agent.command.clone(),
                run_shell_in_docker(DockerShellRun {
                    image: &image.reference,
                    run_id,
                    task_path,
                    app_dir,
                    logs_dir,
                    agent,
                    network: environment.agent_network,
                    env: &envs.agent,
                    timeout: environment.agent_timeout,
                })?,
            ),
        };
        let verifier = run_script_in_docker(DockerScriptRun {
            image: &image.reference,
            run_id,
            task_path,
            app_dir,
            logs_dir,
            script: "tests/test.sh",
            network: environment.verifier_network,
            env: &envs.verifier,
            timeout: environment.verifier_timeout,
        })?;

        Ok(ScriptOutputs { agent, verifier })
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
    prebuilt_image: bool,
    build_network: DockerNetwork,
    agent_network: DockerNetwork,
    verifier_network: DockerNetwork,
    build_timeout: Duration,
    agent_timeout: Duration,
    verifier_timeout: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    let explicit_image = toml_section_value(&task_toml, "environment", "docker_image")
        .or_else(|| toml_top_level_value(&task_toml, "docker_image"));
    let image = explicit_image
        .clone()
        .unwrap_or_else(|| DEFAULT_DOCKER_IMAGE.to_owned());
    let baseline_network = baseline_network(&task_toml)?;
    let build_timeout = toml_duration_value_with_default(
        &task_toml,
        "environment",
        "build_timeout_sec",
        DOCKER_BUILD_TIMEOUT,
    )?;
    let agent_timeout = toml_duration_value(&task_toml, "agent", "timeout_sec")?;
    let verifier_timeout = toml_duration_value(&task_toml, "verifier", "timeout_sec")?;
    let agent_network = phase_network(&task_toml, "agent")?.unwrap_or(baseline_network);
    let verifier_network = phase_network(&task_toml, "verifier")?.unwrap_or(baseline_network);

    reject_unsupported_task_os(&task_toml)?;

    Ok(TaskEnvironment {
        image,
        prebuilt_image: explicit_image.is_some(),
        build_network: baseline_network,
        agent_network,
        verifier_network,
        build_timeout,
        agent_timeout,
        verifier_timeout,
    })
}

fn baseline_network(contents: &str) -> Result<DockerNetwork, CliError> {
    if let Some(value) = toml_section_value(contents, "environment", "network_mode")
        .or_else(|| toml_top_level_value(contents, "network_mode"))
    {
        return parse_network_mode("environment.network_mode", &value);
    }

    if let Some(value) = toml_bool_value(contents, "environment", "allow_internet")? {
        return Ok(if value {
            DockerNetwork::Bridge
        } else {
            DockerNetwork::None
        });
    }

    Ok(DockerNetwork::Bridge)
}

fn phase_network(contents: &str, section: &str) -> Result<Option<DockerNetwork>, CliError> {
    match toml_section_value(contents, section, "network_mode") {
        Some(value) => parse_network_mode(&format!("{section}.network_mode"), &value).map(Some),
        None => Ok(None),
    }
}

fn parse_network_mode(field: &str, value: &str) -> Result<DockerNetwork, CliError> {
    match value {
        "no-network" | "none" => Ok(DockerNetwork::None),
        "public" | "bridge" => Ok(DockerNetwork::Bridge),
        "allowlist" => Err(CliError::unimplemented(format!(
            "{field} = `allowlist` is not implemented for the docker backend yet"
        ))),
        unknown => Err(CliError::usage(format!(
            "unsupported {field} `{unknown}`; use `public`, `no-network`, or `allowlist`"
        ))),
    }
}

fn reject_unsupported_task_os(contents: &str) -> Result<(), CliError> {
    let Some(os) = toml_section_value(contents, "environment", "os") else {
        return Ok(());
    };

    if os == "linux" {
        Ok(())
    } else {
        Err(CliError::unimplemented(format!(
            "[environment].os = `{os}` is not implemented by Seaport's docker backend yet"
        )))
    }
}

fn toml_top_level_value(contents: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} = ");
    let mut in_section = false;

    contents.lines().find_map(|line| {
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = true;
            return None;
        }

        if in_section {
            return None;
        }

        trimmed.strip_prefix(&prefix).map(toml_scalar_value)
    })
}

fn toml_duration_value(contents: &str, section: &str, key: &str) -> Result<Duration, CliError> {
    toml_duration_value_with_default(contents, section, key, Duration::from_secs(120))
}

fn toml_duration_value_with_default(
    contents: &str,
    section: &str,
    key: &str,
    default: Duration,
) -> Result<Duration, CliError> {
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
        None => Ok(default),
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
                return Some(toml_scalar_value(value));
            }
        }
    }

    None
}

fn toml_bool_value(contents: &str, section: &str, key: &str) -> Result<Option<bool>, CliError> {
    let Some(value) = toml_section_value(contents, section, key) else {
        return Ok(None);
    };

    match value.as_str() {
        "true" => Ok(Some(true)),
        "false" => Ok(Some(false)),
        unknown => Err(CliError::usage(format!(
            "[{section}].{key} must be true or false, got `{unknown}`"
        ))),
    }
}

fn toml_scalar_value(value: &str) -> String {
    strip_inline_comment(value.trim())
        .trim()
        .trim_matches('"')
        .to_owned()
}

fn strip_inline_comment(value: &str) -> &str {
    let mut in_quotes = false;
    let mut escaped = false;

    for (index, character) in value.char_indices() {
        if character == '"' && !escaped {
            in_quotes = !in_quotes;
        }

        if character == '#' && !in_quotes {
            return &value[..index];
        }

        escaped = character == '\\' && !escaped;
        if character != '\\' {
            escaped = false;
        }
    }

    value
}

fn prepare_docker_image(
    task_path: &Path,
    run_id: &str,
    environment: &TaskEnvironment,
) -> Result<DockerImage, CliError> {
    let dockerfile = task_path.join("environment").join("Dockerfile");

    if environment.prebuilt_image || !dockerfile.is_file() {
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
            environment.build_network.as_docker_arg(),
            "-q",
            "-t",
            &tag,
        ])
        .arg(environment_dir);
    let timed_output = run_command_with_timeout(command, environment.build_timeout)?;
    let output = timed_output.output;

    if timed_output.timed_out {
        cleanup_docker_image(&tag);
        return Err(CliError::task_failed(format!(
            "docker image build timed out after {:.3}s for {}",
            environment.build_timeout.as_secs_f64(),
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
    run_id: &'a str,
    task_path: &'a Path,
    app_dir: &'a Path,
    logs_dir: &'a Path,
    script: &'a str,
    network: DockerNetwork,
    env: &'a [(String, String)],
    timeout: Duration,
}

struct DockerShellRun<'a> {
    image: &'a str,
    run_id: &'a str,
    task_path: &'a Path,
    app_dir: &'a Path,
    logs_dir: &'a Path,
    agent: &'a ExternalAgent,
    network: DockerNetwork,
    env: &'a [(String, String)],
    timeout: Duration,
}

fn run_script_in_docker(run: DockerScriptRun<'_>) -> Result<Output, CliError> {
    let logs_root = run
        .logs_dir
        .parent()
        .ok_or_else(|| CliError::usage("logs directory has no parent"))?;
    let container_name = docker_container_name(run.run_id, run.script);
    let extra_env = env_refs(run.env);
    let command = docker_run_command(DockerRunCommand {
        image: run.image,
        container_name: &container_name,
        task_path: run.task_path,
        app_dir: run.app_dir,
        logs_root,
        invocation: DockerInvocation::TaskScript(run.script),
        network: run.network,
        extra_env: &extra_env,
    });
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

fn run_shell_in_docker(run: DockerShellRun<'_>) -> Result<Output, CliError> {
    let logs_root = run
        .logs_dir
        .parent()
        .ok_or_else(|| CliError::usage("logs directory has no parent"))?;
    let container_name = docker_container_name(run.run_id, "agent");
    let mut extra_env = env_refs(run.env);
    extra_env.push(("SEAPORT_AGENT_NAME", run.agent.name.as_str()));

    if let Some(model) = run.agent.model.as_deref() {
        extra_env.push(("SEAPORT_MODEL", model));
    }

    let command = docker_run_command(DockerRunCommand {
        image: run.image,
        container_name: &container_name,
        task_path: run.task_path,
        app_dir: run.app_dir,
        logs_root,
        invocation: DockerInvocation::ShellCommand(&run.agent.command),
        network: run.network,
        extra_env: &extra_env,
    });
    let timed_output = run_command_with_timeout(command, run.timeout)?;
    let output = timed_output.output;

    if timed_output.timed_out {
        cleanup_docker_container(&container_name);
        return Err(CliError::task_failed(format!(
            "sandboxed docker agent timed out after {:.3}s: {}",
            run.timeout.as_secs_f64(),
            run.agent.name
        )));
    }

    if !output.status.success() {
        return Err(CliError::task_failed(format!(
            "sandboxed docker agent failed: {} (status: {})\nstdout:\n{}\nstderr:\n{}",
            run.agent.name,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(output)
}

enum DockerInvocation<'a> {
    TaskScript(&'a str),
    ShellCommand(&'a str),
}

struct DockerRunCommand<'a> {
    image: &'a str,
    container_name: &'a str,
    task_path: &'a Path,
    app_dir: &'a Path,
    logs_root: &'a Path,
    invocation: DockerInvocation<'a>,
    network: DockerNetwork,
    extra_env: &'a [(&'a str, &'a str)],
}

fn docker_run_command(run: DockerRunCommand<'_>) -> Command {
    let mut command = Command::new("docker");
    command
        .args([
            "run",
            "--rm",
            "--name",
            run.container_name,
            "--network",
            run.network.as_docker_arg(),
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
            "--env",
            "SEAPORT_INSTRUCTION_PATH=/seaport/task/instruction.md",
        ])
        .args(
            run.extra_env
                .iter()
                .flat_map(|(name, value)| ["--env".to_owned(), format!("{name}={value}")]),
        )
        .arg("--mount")
        .arg(format!(
            "type=bind,source={},target=/app",
            run.app_dir.display()
        ))
        .arg("--mount")
        .arg(format!(
            "type=bind,source={},target=/logs",
            run.logs_root.display()
        ))
        .arg("--mount")
        .arg(format!(
            "type=bind,source={},target=/seaport/task,readonly",
            run.task_path.display()
        ))
        .arg(run.image)
        .arg("bash");

    match run.invocation {
        DockerInvocation::TaskScript(script) => {
            command.arg(format!("/seaport/task/{script}"));
        }
        DockerInvocation::ShellCommand(shell_command) => {
            command.arg("-lc").arg(shell_command);
        }
    }

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
    agent_kind: &SandboxAgent,
    envs: &PhaseEnvs,
    environment: &TaskEnvironment,
) -> Result<ScriptOutputs, CliError> {
    let verifier = task_path.join("tests").join("test.sh");
    let agent = match agent_kind {
        SandboxAgent::Oracle => AgentStep::from_output(
            "solution/solve.sh",
            run_script_locally(
                &task_path.join("solution").join("solve.sh"),
                task_path,
                app_dir,
                logs_dir,
                &envs.agent,
                environment.agent_timeout,
            )?,
        ),
        SandboxAgent::Nop => AgentStep::nop(),
        SandboxAgent::External(agent) => AgentStep::from_output(
            agent.command.clone(),
            run_shell_locally(
                agent,
                task_path,
                app_dir,
                logs_dir,
                &envs.agent,
                environment.agent_timeout,
            )?,
        ),
    };
    let verifier = run_script_locally(
        &verifier,
        task_path,
        app_dir,
        logs_dir,
        &envs.verifier,
        environment.verifier_timeout,
    )?;

    Ok(ScriptOutputs { agent, verifier })
}

fn run_shell_locally(
    agent: &ExternalAgent,
    task_path: &Path,
    app_dir: &Path,
    logs_dir: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<Output, CliError> {
    let mut command = Command::new("bash");
    command
        .arg("-lc")
        .arg(&agent.command)
        .current_dir(app_dir)
        .env("APP_DIR", app_dir)
        .env("LOGS_DIR", logs_dir)
        .env("SEAPORT_TASK_DIR", task_path)
        .env("SEAPORT_INSTRUCTION_PATH", task_path.join("instruction.md"))
        .env("SEAPORT_AGENT_NAME", &agent.name);

    apply_env(&mut command, env);

    if let Some(model) = agent.model.as_deref() {
        command.env("SEAPORT_MODEL", model);
    }

    let timed_output = run_command_with_timeout(command, timeout)?;
    let output = timed_output.output;

    if timed_output.timed_out {
        return Err(CliError::task_failed(format!(
            "unsafe local agent timed out after {:.3}s: {}",
            timeout.as_secs_f64(),
            agent.name
        )));
    }

    if !output.status.success() {
        return Err(CliError::task_failed(format!(
            "unsafe local agent failed: {} (status: {})\nstdout:\n{}\nstderr:\n{}",
            agent.name,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(output)
}

fn run_script_locally(
    script: &Path,
    task_path: &Path,
    app_dir: &Path,
    logs_dir: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<Output, CliError> {
    let mut command = Command::new("bash");
    command
        .arg(script)
        .current_dir(app_dir)
        .env("APP_DIR", app_dir)
        .env("LOGS_DIR", logs_dir)
        .env("SEAPORT_TASK_DIR", task_path)
        .env("SEAPORT_INSTRUCTION_PATH", task_path.join("instruction.md"));
    apply_env(&mut command, env);
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

fn env_refs(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

fn apply_env(command: &mut Command, env: &[(String, String)]) {
    for (name, value) in env {
        command.env(name, value);
    }
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
        let command = docker_run_command(DockerRunCommand {
            image: "seaport-task-test",
            container_name: "seaport-test-container",
            task_path: Path::new("/tmp/task"),
            app_dir: Path::new("/tmp/app"),
            logs_root: Path::new("/tmp/logs"),
            invocation: DockerInvocation::TaskScript("tests/test.sh"),
            network: DockerNetwork::None,
            extra_env: &[],
        });
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

    #[test]
    fn prepare_task_file_workspace_copies_packaged_task_files() {
        let task = temp_task_dir("task-file-source");
        let app = temp_task_dir("task-file-app");
        let input_dir = task
            .join("environment")
            .join("task_file")
            .join("input_data");

        fs::create_dir_all(&input_dir).expect("input dir");
        fs::create_dir_all(app.join("task_file")).expect("stale task file dir");
        fs::write(input_dir.join("requests.jsonl"), "{}\n").expect("input file");
        fs::write(app.join("task_file").join("stale.txt"), "old").expect("stale file");

        prepare_task_file_workspace(&task, &app).expect("workspace");

        assert_eq!(
            fs::read_to_string(app.join("task_file/input_data/requests.jsonl")).expect("input"),
            "{}\n"
        );
        assert!(!app.join("task_file/stale.txt").exists());

        let _ = fs::remove_dir_all(task);
        let _ = fs::remove_dir_all(app);
    }

    #[test]
    fn task_environment_reads_harbor_environment_sections() {
        let task = temp_task_dir("harbor-environment-sections");
        fs::create_dir_all(&task).expect("task dir");
        fs::write(
            task.join("task.toml"),
            r#"
[environment]
docker_image = "python:3.12"
network_mode = "public"
build_timeout_sec = 7.5
os = "linux"

[agent]
timeout_sec = 3
network_mode = "no-network"

[verifier]
timeout_sec = 5
network_mode = "public"
"#,
        )
        .expect("task toml");

        let environment = task_environment(&task).expect("environment");

        assert_eq!(environment.image, "python:3.12");
        assert!(environment.prebuilt_image);
        assert_eq!(environment.build_network, DockerNetwork::Bridge);
        assert_eq!(environment.agent_network, DockerNetwork::None);
        assert_eq!(environment.verifier_network, DockerNetwork::Bridge);
        assert_eq!(environment.build_timeout, Duration::from_secs_f64(7.5));
        assert_eq!(environment.agent_timeout, Duration::from_secs(3));
        assert_eq!(environment.verifier_timeout, Duration::from_secs(5));

        let _ = fs::remove_dir_all(task);
    }

    #[test]
    fn task_environment_defaults_to_harbor_public_network() {
        let task = temp_task_dir("harbor-public-default");
        fs::create_dir_all(&task).expect("task dir");
        fs::write(
            task.join("task.toml"),
            r#"
[environment]
docker_image = "ubuntu:24.04" # explicit prebuilt image
"#,
        )
        .expect("task toml");

        let environment = task_environment(&task).expect("environment");

        assert_eq!(environment.build_network, DockerNetwork::Bridge);
        assert_eq!(environment.agent_network, DockerNetwork::Bridge);
        assert_eq!(environment.verifier_network, DockerNetwork::Bridge);
        assert_eq!(environment.build_timeout, DOCKER_BUILD_TIMEOUT);

        let _ = fs::remove_dir_all(task);
    }

    fn command_args(command: Command) -> Vec<String> {
        command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    fn temp_task_dir(name: &str) -> std::path::PathBuf {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();

        std::env::temp_dir().join(format!("seaport-{name}-{id}"))
    }
}
