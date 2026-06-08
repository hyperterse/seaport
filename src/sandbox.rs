use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::CliError;

const DEFAULT_DOCKER_IMAGE: &str = "ubuntu:24.04";
const DEFAULT_CONTAINER_MEMORY: &str = "1g";
const DEFAULT_CONTAINER_CPUS: &str = "1.0";
const CONTAINER_PIDS_LIMIT: &str = "256";
const DEFAULT_TMPFS_SIZE: &str = "256m";
const DEFAULT_COMPAT_DOCKER_PLATFORM: &str = "linux/amd64";
const DOCKER_BUILD_ATTEMPTS: usize = 3;
const DOCKER_BUILD_RETRY_DELAY: Duration = Duration::from_secs(2);
const DOCKER_BUILD_TIMEOUT: Duration = Duration::from_secs(600);
const DOCKER_WORKSPACE_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) struct ScriptOutputs {
    pub(crate) agent: AgentStep,
    pub(crate) verifier: Output,
}

pub(crate) struct TaskScriptRequest<'a> {
    pub(crate) task_label: &'a str,
    pub(crate) task_path: &'a Path,
    pub(crate) run_id: &'a str,
    pub(crate) app_dir: &'a Path,
    pub(crate) logs_dir: &'a Path,
    pub(crate) agent: &'a SandboxAgent,
    pub(crate) envs: &'a PhaseEnvs,
    pub(crate) backend: SandboxBackend,
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

pub(crate) fn run_task_scripts(run: TaskScriptRequest<'_>) -> Result<ScriptOutputs, CliError> {
    let environment = task_environment(run.task_path)?;
    let runtime = TaskRuntime {
        task_label: run.task_label,
        task_path: run.task_path,
        run_id: run.run_id,
        app_dir: run.app_dir,
        logs_dir: run.logs_dir,
    };

    match run.backend {
        SandboxBackend::Docker => run_scripts_in_docker(runtime, run.agent, run.envs, &environment),
        SandboxBackend::UnsafeLocal => {
            prepare_task_file_workspace(run.task_path, run.app_dir)?;
            run_scripts_locally(runtime, run.agent, run.envs, &environment)
        }
    }
}

#[derive(Clone, Copy)]
struct TaskRuntime<'a> {
    task_label: &'a str,
    task_path: &'a Path,
    run_id: &'a str,
    app_dir: &'a Path,
    logs_dir: &'a Path,
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

fn prepare_cobol_copybook_aliases(app_dir: &Path) -> Result<(), CliError> {
    let mut copybooks = Vec::new();
    collect_copybooks(app_dir, &mut copybooks)?;

    for copybook in copybooks {
        create_copybook_aliases(&copybook)?;

        if copybook.parent().is_some_and(|parent| parent != app_dir) {
            create_copybook_aliases_in_dir(&copybook, app_dir)?;
        }
    }

    Ok(())
}

fn collect_copybooks(path: &Path, copybooks: &mut Vec<PathBuf>) -> Result<(), CliError> {
    if !path.is_dir() {
        return Ok(());
    }

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            collect_copybooks(&entry_path, copybooks)?;
        } else if file_type.is_file()
            && entry_path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("cpy"))
        {
            copybooks.push(entry_path);
        }
    }

    Ok(())
}

fn create_copybook_aliases(copybook: &Path) -> Result<(), CliError> {
    let parent = copybook
        .parent()
        .ok_or_else(|| CliError::usage("copybook path has no parent directory"))?;

    create_copybook_aliases_in_dir(copybook, parent)
}

fn create_copybook_aliases_in_dir(copybook: &Path, target_dir: &Path) -> Result<(), CliError> {
    let stem = copybook
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| CliError::usage("copybook path has no valid UTF-8 file stem"))?;
    let stems = [
        stem.to_owned(),
        stem.to_ascii_uppercase(),
        stem.to_ascii_lowercase(),
    ];
    let extensions = ["", ".cpy", ".CPY", ".cob", ".COB"];

    for stem in stems {
        for extension in extensions {
            let alias = target_dir.join(format!("{stem}{extension}"));

            if alias != copybook && !alias.exists() {
                fs::copy(copybook, alias)?;
            }
        }
    }

    Ok(())
}

fn run_scripts_in_docker(
    runtime: TaskRuntime<'_>,
    agent_kind: &SandboxAgent,
    envs: &PhaseEnvs,
    environment: &TaskEnvironment,
) -> Result<ScriptOutputs, CliError> {
    ensure_docker_available()?;

    let image = prepare_docker_image(
        runtime.task_label,
        runtime.task_path,
        runtime.run_id,
        environment,
    )?;
    let image_platform = image.platform.as_deref();
    let result = (|| {
        seed_docker_app_workspace(
            runtime.task_label,
            runtime.run_id,
            &image.reference,
            runtime.app_dir,
            image_platform,
        )?;
        prepare_task_file_workspace(runtime.task_path, runtime.app_dir)?;
        prepare_cobol_copybook_aliases(runtime.app_dir)?;

        let agent = match agent_kind {
            SandboxAgent::Oracle => AgentStep::from_output(
                "solution/solve.sh",
                run_script_in_docker(DockerScriptRun {
                    image: &image.reference,
                    run_id: runtime.run_id,
                    task_path: runtime.task_path,
                    app_dir: runtime.app_dir,
                    logs_dir: runtime.logs_dir,
                    task_label: runtime.task_label,
                    script: "solution/solve.sh",
                    network: environment.agent_network,
                    platform: image_platform,
                    resources: &environment.resources,
                    env: &envs.agent,
                    timeout: environment.agent_timeout,
                })?,
            ),
            SandboxAgent::Nop => AgentStep::nop(),
            SandboxAgent::External(agent) => AgentStep::from_output(
                agent.command.clone(),
                run_shell_in_docker(DockerShellRun {
                    image: &image.reference,
                    run_id: runtime.run_id,
                    task_path: runtime.task_path,
                    app_dir: runtime.app_dir,
                    logs_dir: runtime.logs_dir,
                    task_label: runtime.task_label,
                    agent,
                    network: environment.agent_network,
                    platform: image_platform,
                    resources: &environment.resources,
                    env: &envs.agent,
                    timeout: environment.agent_timeout,
                })?,
            ),
        };
        let verifier = run_script_in_docker(DockerScriptRun {
            image: &image.reference,
            run_id: runtime.run_id,
            task_path: runtime.task_path,
            app_dir: runtime.app_dir,
            logs_dir: runtime.logs_dir,
            task_label: runtime.task_label,
            script: "tests/test.sh",
            network: environment.verifier_network,
            platform: image_platform,
            resources: &environment.resources,
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
    platform: Option<String>,
}

struct TaskEnvironment {
    image: String,
    prebuilt_image: bool,
    platform: Option<String>,
    resources: DockerResources,
    build_network: DockerNetwork,
    agent_network: DockerNetwork,
    verifier_network: DockerNetwork,
    build_timeout: Duration,
    agent_timeout: Duration,
    verifier_timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DockerResources {
    cpus: String,
    memory: String,
    tmpfs_size: String,
}

impl Default for DockerResources {
    fn default() -> Self {
        Self {
            cpus: DEFAULT_CONTAINER_CPUS.to_owned(),
            memory: DEFAULT_CONTAINER_MEMORY.to_owned(),
            tmpfs_size: DEFAULT_TMPFS_SIZE.to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockerNetwork {
    None,
    Bridge,
}

impl DockerNetwork {
    fn as_docker_run_arg(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bridge => "bridge",
        }
    }

    fn as_docker_build_arg(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bridge => "default",
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
    let prebuilt_image = explicit_image.is_some();
    let platform = docker_platform(&task_toml);
    let resources = docker_resources(&task_toml)?;
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
        prebuilt_image,
        platform,
        resources,
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

fn docker_platform(contents: &str) -> Option<String> {
    if let Ok(platform) = env::var("SEAPORT_DOCKER_PLATFORM") {
        return docker_platform_value(&platform);
    }

    toml_section_value(contents, "environment", "docker_platform")
        .or_else(|| toml_section_value(contents, "environment", "platform"))
        .or_else(|| toml_top_level_value(contents, "docker_platform"))
}

fn docker_platform_value(platform: &str) -> Option<String> {
    let platform = platform.trim();

    if platform.is_empty() || platform == "host" || platform == "native" {
        None
    } else {
        Some(platform.to_owned())
    }
}

fn docker_resources(contents: &str) -> Result<DockerResources, CliError> {
    let mut resources = DockerResources::default();

    if let Some(cpus) = toml_section_value(contents, "environment", "cpus") {
        let parsed = cpus.parse::<f64>().map_err(|error| {
            CliError::usage(format!("[environment].cpus must be a number: {error}"))
        })?;

        if parsed <= 0.0 {
            return Err(CliError::usage(
                "[environment].cpus must be greater than zero",
            ));
        }

        resources.cpus = cpus;
    }

    if let Some(memory_mb) = toml_section_value(contents, "environment", "memory_mb") {
        let parsed = memory_mb.parse::<u64>().map_err(|error| {
            CliError::usage(format!(
                "[environment].memory_mb must be a positive integer: {error}"
            ))
        })?;

        if parsed == 0 {
            return Err(CliError::usage(
                "[environment].memory_mb must be greater than zero",
            ));
        }

        resources.memory = format!("{parsed}m");
    }

    Ok(resources)
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

fn seed_docker_app_workspace(
    task_label: &str,
    run_id: &str,
    image: &str,
    app_dir: &Path,
    platform: Option<&str>,
) -> Result<(), CliError> {
    let container_name = docker_container_name(run_id, "workspace");
    let create_output = run_command_with_timeout(
        docker_create_workspace_command(&container_name, image, platform),
        DOCKER_WORKSPACE_TIMEOUT,
        Some(CommandLog::new(task_label, "workspace")),
    )?;

    if create_output.timed_out {
        cleanup_docker_container(&container_name);
        return Err(CliError::task_failed(format!(
            "docker workspace container creation timed out after {:.3}s",
            DOCKER_WORKSPACE_TIMEOUT.as_secs_f64()
        )));
    }

    if !create_output.output.status.success() {
        cleanup_docker_container(&container_name);
        return Err(CliError::task_failed(format!(
            "docker workspace container creation failed (status: {})\nstdout:\n{}\nstderr:\n{}",
            create_output.output.status,
            String::from_utf8_lossy(&create_output.output.stdout),
            String::from_utf8_lossy(&create_output.output.stderr)
        )));
    }

    let copy_output = run_command_with_timeout(
        docker_copy_workspace_command(&container_name, app_dir),
        DOCKER_WORKSPACE_TIMEOUT,
        Some(CommandLog::new(task_label, "workspace")),
    )?;
    cleanup_docker_container(&container_name);

    if copy_output.timed_out {
        return Err(CliError::task_failed(format!(
            "docker workspace copy timed out after {:.3}s",
            DOCKER_WORKSPACE_TIMEOUT.as_secs_f64()
        )));
    }

    if !copy_output.output.status.success() {
        if docker_copy_missing_app(&copy_output.output) {
            return Ok(());
        }

        return Err(CliError::task_failed(format!(
            "docker workspace copy failed (status: {})\nstdout:\n{}\nstderr:\n{}",
            copy_output.output.status,
            String::from_utf8_lossy(&copy_output.output.stdout),
            String::from_utf8_lossy(&copy_output.output.stderr)
        )));
    }

    prepare_container_writable_tree(app_dir)
}

fn docker_create_workspace_command(
    container_name: &str,
    image: &str,
    platform: Option<&str>,
) -> Command {
    let mut command = Command::new("docker");
    command.args(["create", "--name", container_name]);

    if let Some(platform) = platform {
        command.args(["--platform", platform]);
    }

    command.arg(image);
    command
}

fn docker_copy_workspace_command(container_name: &str, app_dir: &Path) -> Command {
    let mut command = Command::new("docker");
    command
        .arg("cp")
        .arg(format!("{container_name}:/app/."))
        .arg(app_dir);
    command
}

fn docker_copy_missing_app(output: &Output) -> bool {
    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();

    output.contains("/app")
        && (output.contains("could not find")
            || output.contains("no such file")
            || output.contains("not found"))
}

fn prepare_docker_image(
    task_label: &str,
    task_path: &Path,
    run_id: &str,
    environment: &TaskEnvironment,
) -> Result<DockerImage, CliError> {
    let dockerfile = task_path.join("environment").join("Dockerfile");

    if environment.prebuilt_image || !dockerfile.is_file() {
        return Ok(DockerImage {
            reference: environment.image.clone(),
            remove_after_run: false,
            platform: environment.platform.clone(),
        });
    }

    let tag = format!("seaport-task-{run_id}");
    let environment_dir = dockerfile
        .parent()
        .ok_or_else(|| CliError::usage("environment/Dockerfile has no parent directory"))?;
    let mut build_platform = environment.platform.clone();
    let mut timed_output = run_docker_build_with_retries(
        task_label,
        &tag,
        environment_dir,
        environment,
        build_platform.as_deref(),
    )?;

    if should_retry_build_with_compat_platform(&timed_output, build_platform.as_deref()) {
        build_platform = Some(DEFAULT_COMPAT_DOCKER_PLATFORM.to_owned());
        println!(
            "[{task_label} | build] native docker build is not available for this image; retrying with {DEFAULT_COMPAT_DOCKER_PLATFORM}"
        );
        timed_output = run_docker_build_with_retries(
            task_label,
            &tag,
            environment_dir,
            environment,
            build_platform.as_deref(),
        )?;
    }

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
        platform: build_platform,
    })
}

fn run_docker_build_with_retries(
    task_label: &str,
    tag: &str,
    environment_dir: &Path,
    environment: &TaskEnvironment,
    platform: Option<&str>,
) -> Result<TimedOutput, CliError> {
    let mut attempt = 1;

    loop {
        let timed_output = run_command_with_timeout(
            docker_build_command(tag, environment_dir, environment, platform),
            environment.build_timeout,
            Some(CommandLog::new(task_label, "build")),
        )?;

        if timed_output.output.status.success()
            || timed_output.timed_out
            || attempt >= DOCKER_BUILD_ATTEMPTS
            || !docker_build_transient_failure(&timed_output.output)
        {
            return Ok(timed_output);
        }

        attempt += 1;
        println!(
            "[{task_label} | build] transient docker build failure; retrying attempt {attempt}/{DOCKER_BUILD_ATTEMPTS} in {}",
            format_duration(DOCKER_BUILD_RETRY_DELAY)
        );
        thread::sleep(DOCKER_BUILD_RETRY_DELAY);
    }
}

fn should_retry_build_with_compat_platform(
    timed_output: &TimedOutput,
    platform: Option<&str>,
) -> bool {
    platform.is_none()
        && compat_docker_platform_available()
        && !timed_output.timed_out
        && !timed_output.output.status.success()
        && docker_build_needs_compat_platform(&timed_output.output)
}

fn compat_docker_platform_available() -> bool {
    cfg!(target_arch = "aarch64")
}

fn docker_build_needs_compat_platform(output: &Output) -> bool {
    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();

    [
        "no matching manifest for linux/arm64",
        "no match for platform",
        "does not support platform",
        "package zulu7-jdk has no installation candidate",
        "unable to locate package zulu7-jdk",
    ]
    .iter()
    .any(|pattern| output.contains(pattern))
}

fn docker_build_transient_failure(output: &Output) -> bool {
    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();

    [
        "502 bad gateway",
        "503 service unavailable",
        "504 gateway timeout",
        "failed to fetch anonymous token",
        "tls handshake timeout",
        "i/o timeout",
        "temporary failure",
        "connection reset",
        "unexpected status from get request",
    ]
    .iter()
    .any(|pattern| output.contains(pattern))
}

fn docker_build_command(
    tag: &str,
    environment_dir: &Path,
    environment: &TaskEnvironment,
    platform: Option<&str>,
) -> Command {
    let mut command = Command::new("docker");
    command.args([
        "build",
        "--progress=plain",
        "--pull=false",
        "--network",
        environment.build_network.as_docker_build_arg(),
    ]);

    if let Some(platform) = platform {
        command.args(["--platform", platform]);
    }

    command.args(["-t", tag]).arg(environment_dir);
    command
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
    task_label: &'a str,
    script: &'a str,
    network: DockerNetwork,
    platform: Option<&'a str>,
    resources: &'a DockerResources,
    env: &'a [(String, String)],
    timeout: Duration,
}

struct DockerShellRun<'a> {
    image: &'a str,
    run_id: &'a str,
    task_path: &'a Path,
    app_dir: &'a Path,
    logs_dir: &'a Path,
    task_label: &'a str,
    agent: &'a ExternalAgent,
    network: DockerNetwork,
    platform: Option<&'a str>,
    resources: &'a DockerResources,
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
        platform: run.platform,
        resources: run.resources,
        extra_env: &extra_env,
    });
    let timed_output = run_command_with_timeout(
        command,
        run.timeout,
        Some(CommandLog::new(run.task_label, script_phase(run.script))),
    )?;
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
        platform: run.platform,
        resources: run.resources,
        extra_env: &extra_env,
    });
    let timed_output = run_command_with_timeout(
        command,
        run.timeout,
        Some(CommandLog::new(run.task_label, "agent")),
    )?;
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
    platform: Option<&'a str>,
    resources: &'a DockerResources,
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
            run.network.as_docker_run_arg(),
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--pids-limit",
            CONTAINER_PIDS_LIMIT,
            "--memory",
            run.resources.memory.as_str(),
            "--memory-swap",
            run.resources.memory.as_str(),
            "--cpus",
            run.resources.cpus.as_str(),
            "--read-only",
            "--workdir",
            "/app",
            "--tmpfs",
            &format!("/tmp:rw,nosuid,nodev,size={}", run.resources.tmpfs_size),
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
            "--env",
            "COBCPY=/app/copybooks:/app/COPYBOOKS:/app/src/copybooks:/app/src/COPYBOOKS",
        ])
        .args(
            run.extra_env
                .iter()
                .flat_map(|(name, value)| ["--env".to_owned(), format!("{name}={value}")]),
        );

    if let Some(platform) = run.platform {
        command.args(["--platform", platform]);
    }

    command
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
            "type=bind,source={},target=/tests,readonly",
            run.task_path.join("tests").display()
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

fn script_phase(script: &str) -> &'static str {
    if script.starts_with("solution/") {
        "solution"
    } else if script.starts_with("tests/") {
        "verifier"
    } else {
        "script"
    }
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
    runtime: TaskRuntime<'_>,
    agent_kind: &SandboxAgent,
    envs: &PhaseEnvs,
    environment: &TaskEnvironment,
) -> Result<ScriptOutputs, CliError> {
    let verifier = runtime.task_path.join("tests").join("test.sh");
    let agent = match agent_kind {
        SandboxAgent::Oracle => AgentStep::from_output(
            "solution/solve.sh",
            run_script_locally(LocalScriptRun {
                script: &runtime.task_path.join("solution").join("solve.sh"),
                task_path: runtime.task_path,
                app_dir: runtime.app_dir,
                logs_dir: runtime.logs_dir,
                task_label: runtime.task_label,
                phase: "solution",
                env: &envs.agent,
                timeout: environment.agent_timeout,
            })?,
        ),
        SandboxAgent::Nop => AgentStep::nop(),
        SandboxAgent::External(agent) => AgentStep::from_output(
            agent.command.clone(),
            run_shell_locally(
                agent,
                runtime.task_path,
                runtime.app_dir,
                runtime.logs_dir,
                runtime.task_label,
                &envs.agent,
                environment.agent_timeout,
            )?,
        ),
    };
    let verifier = run_script_locally(LocalScriptRun {
        script: &verifier,
        task_path: runtime.task_path,
        app_dir: runtime.app_dir,
        logs_dir: runtime.logs_dir,
        task_label: runtime.task_label,
        phase: "verifier",
        env: &envs.verifier,
        timeout: environment.verifier_timeout,
    })?;

    Ok(ScriptOutputs { agent, verifier })
}

fn run_shell_locally(
    agent: &ExternalAgent,
    task_path: &Path,
    app_dir: &Path,
    logs_dir: &Path,
    task_label: &str,
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

    let timed_output =
        run_command_with_timeout(command, timeout, Some(CommandLog::new(task_label, "agent")))?;
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

struct LocalScriptRun<'a> {
    script: &'a Path,
    task_path: &'a Path,
    app_dir: &'a Path,
    logs_dir: &'a Path,
    task_label: &'a str,
    phase: &'a str,
    env: &'a [(String, String)],
    timeout: Duration,
}

fn run_script_locally(run: LocalScriptRun<'_>) -> Result<Output, CliError> {
    let mut command = Command::new("bash");
    command
        .arg(run.script)
        .current_dir(run.app_dir)
        .env("APP_DIR", run.app_dir)
        .env("LOGS_DIR", run.logs_dir)
        .env("SEAPORT_TASK_DIR", run.task_path)
        .env(
            "SEAPORT_INSTRUCTION_PATH",
            run.task_path.join("instruction.md"),
        );
    apply_env(&mut command, run.env);
    let timed_output = run_command_with_timeout(
        command,
        run.timeout,
        Some(CommandLog::new(run.task_label, run.phase)),
    )?;
    let output = timed_output.output;

    if timed_output.timed_out {
        return Err(CliError::task_failed(format!(
            "unsafe local script timed out after {:.3}s: {}",
            run.timeout.as_secs_f64(),
            run.script.display()
        )));
    }

    if !output.status.success() {
        return Err(CliError::task_failed(format!(
            "script failed: {} (status: {})\nstdout:\n{}\nstderr:\n{}",
            run.script.display(),
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

#[derive(Clone)]
struct CommandLog {
    task: String,
    phase: String,
}

impl CommandLog {
    fn new(task: &str, phase: &str) -> Self {
        Self {
            task: task.to_owned(),
            phase: phase.to_owned(),
        }
    }

    fn stream(&self, name: &'static str) -> StreamLog {
        StreamLog {
            task: self.task.clone(),
            phase: self.phase.clone(),
            stream: name,
        }
    }
}

struct StreamLog {
    task: String,
    phase: String,
    stream: &'static str,
}

fn run_command_with_timeout(
    mut command: Command,
    timeout: Duration,
    log: Option<CommandLog>,
) -> Result<TimedOutput, CliError> {
    if let Some(log) = &log {
        print_phase_start(log, timeout);
    }

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CliError::io("command stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CliError::io("command stderr was not piped"))?;
    let stdout_log = log.as_ref().map(|log| log.stream("stdout"));
    let stderr_log = log.as_ref().map(|log| log.stream("stderr"));
    let stdout_reader = thread::spawn(move || read_stream(stdout, stdout_log));
    let stderr_reader = thread::spawn(move || read_stream(stderr, stderr_log));
    let started = Instant::now();
    let mut timed_out = false;

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }

        if started.elapsed() >= timeout {
            let _ = child.kill();
            timed_out = true;
            break child.wait()?;
        }

        thread::sleep(Duration::from_millis(25));
    };
    let stdout = join_stream_reader(stdout_reader)?;
    let stderr = join_stream_reader(stderr_reader)?;

    Ok(TimedOutput {
        output: Output {
            status,
            stdout,
            stderr,
        },
        timed_out,
    })
}

fn read_stream<R: Read>(stream: R, log: Option<StreamLog>) -> io::Result<Vec<u8>> {
    let mut reader = BufReader::new(stream);
    let mut output = Vec::new();
    let mut line = Vec::new();

    loop {
        line.clear();
        let bytes_read = reader.read_until(b'\n', &mut line)?;

        if bytes_read == 0 {
            break;
        }

        output.extend_from_slice(&line);

        if let Some(log) = &log {
            print_stream_line(log, &line);
        }
    }

    Ok(output)
}

fn join_stream_reader(
    handle: thread::JoinHandle<io::Result<Vec<u8>>>,
) -> Result<Vec<u8>, CliError> {
    handle
        .join()
        .map_err(|_| CliError::io("command stream reader panicked"))?
        .map_err(CliError::from)
}

fn print_stream_line(log: &StreamLog, line: &[u8]) {
    let text = String::from_utf8_lossy(line);
    let text = text.trim_end_matches(['\r', '\n']);

    println!("[{} | {} | {}] {}", log.task, log.phase, log.stream, text);
}

fn print_phase_start(log: &CommandLog, timeout: Duration) {
    println!(
        "[{} | {}] starting; timeout {}",
        log.task,
        log.phase,
        format_duration(timeout)
    );
    let _ = io::stdout().flush();
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();

    if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
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
            platform: Some("linux/amd64"),
            resources: &DockerResources::default(),
            extra_env: &[],
        });
        let args = command_args(command);

        assert!(args
            .windows(2)
            .any(|window| window == ["--network", "none"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--platform", "linux/amd64"]));
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
        assert!(!args.iter().any(|arg| arg == "--user"));
        assert!(args
            .windows(2)
            .any(|window| window == ["--tmpfs", "/tmp:rw,nosuid,nodev,size=256m"]));
        assert!(args.windows(2).any(|window| window
            == [
                "--env",
                "COBCPY=/app/copybooks:/app/COPYBOOKS:/app/src/copybooks:/app/src/COPYBOOKS"
            ]));
        assert!(args
            .iter()
            .any(|arg| arg == "type=bind,source=/tmp/task,target=/seaport/task,readonly"));
        assert!(args
            .iter()
            .any(|arg| arg == "type=bind,source=/tmp/task/tests,target=/tests,readonly"));
    }

    #[test]
    fn docker_build_command_streams_plain_progress() {
        let environment = TaskEnvironment {
            image: "ubuntu:24.04".to_owned(),
            prebuilt_image: false,
            platform: None,
            resources: DockerResources::default(),
            build_network: DockerNetwork::Bridge,
            agent_network: DockerNetwork::Bridge,
            verifier_network: DockerNetwork::Bridge,
            build_timeout: Duration::from_secs(60),
            agent_timeout: Duration::from_secs(60),
            verifier_timeout: Duration::from_secs(60),
        };
        let command = docker_build_command(
            "seaport-task-test",
            Path::new("/tmp/env"),
            &environment,
            None,
        );
        let args = command_args(command);

        assert!(args
            .windows(2)
            .any(|window| window == ["--progress=plain", "--pull=false"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--network", "default"]));
        assert!(!args.iter().any(|arg| arg == "-q"));
    }

    #[test]
    fn docker_build_transient_failure_detects_registry_gateway_errors() {
        let output = Command::new("bash")
            .arg("-lc")
            .arg("printf 'failed to fetch anonymous token: 502 Bad Gateway\\n' >&2; exit 1")
            .output()
            .expect("output");

        assert!(docker_build_transient_failure(&output));
    }

    #[test]
    fn docker_build_needs_compat_platform_detects_arm_incompatible_toolchain() {
        let output = Command::new("bash")
            .arg("-lc")
            .arg("printf 'E: Package zulu7-jdk has no installation candidate\\n' >&2; exit 1")
            .output()
            .expect("output");

        assert!(docker_build_needs_compat_platform(&output));
    }

    #[test]
    fn docker_workspace_commands_copy_image_app_tree() {
        let create = docker_create_workspace_command(
            "seaport-workspace-test",
            "seaport-task-test",
            Some("linux/amd64"),
        );
        let copy = docker_copy_workspace_command("seaport-workspace-test", Path::new("/tmp/app"));

        let create_args = command_args(create);
        let copy_args = command_args(copy);

        assert!(create_args
            .windows(2)
            .any(|window| window == ["--platform", "linux/amd64"]));
        assert_eq!(
            create_args.last().map(String::as_str),
            Some("seaport-task-test")
        );
        assert_eq!(
            copy_args,
            ["cp", "seaport-workspace-test:/app/.", "/tmp/app"]
        );
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
    fn prepare_cobol_copybook_aliases_materializes_common_names() {
        let app = temp_task_dir("copybook-aliases");
        let copybook_dir = app.join("copybooks");
        fs::create_dir_all(&copybook_dir).expect("copybook dir");
        fs::write(copybook_dir.join("RECLAIM.cpy"), "01 RECLAIM-REC.\n").expect("copybook");

        prepare_cobol_copybook_aliases(&app).expect("aliases");

        assert_eq!(
            fs::read_to_string(copybook_dir.join("RECLAIM")).expect("extensionless"),
            "01 RECLAIM-REC.\n"
        );
        assert_eq!(
            fs::read_to_string(copybook_dir.join("RECLAIM.COB")).expect("cob alias"),
            "01 RECLAIM-REC.\n"
        );
        assert_eq!(
            fs::read_to_string(app.join("RECLAIM.COB")).expect("workspace root cob alias"),
            "01 RECLAIM-REC.\n"
        );
        assert_eq!(
            fs::read_to_string(copybook_dir.join("reclaim.CPY")).expect("case alias"),
            "01 RECLAIM-REC.\n"
        );

        let _ = fs::remove_dir_all(app);
    }

    #[test]
    fn command_runner_streams_and_captures_output() {
        let mut command = Command::new("bash");
        command
            .arg("-lc")
            .arg("printf 'hello stdout\\n'; printf 'hello stderr\\n' >&2");

        let output = run_command_with_timeout(command, Duration::from_secs(2), None)
            .expect("command")
            .output;

        assert!(output.status.success());
        assert_eq!(output.stdout, b"hello stdout\n");
        assert_eq!(output.stderr, b"hello stderr\n");
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
docker_platform = "linux/arm64"
network_mode = "public"
build_timeout_sec = 7.5
cpus = 2
memory_mb = 2048
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
        assert_eq!(environment.platform.as_deref(), Some("linux/arm64"));
        assert_eq!(environment.build_network, DockerNetwork::Bridge);
        assert_eq!(environment.agent_network, DockerNetwork::None);
        assert_eq!(environment.verifier_network, DockerNetwork::Bridge);
        assert_eq!(environment.resources.cpus, "2");
        assert_eq!(environment.resources.memory, "2048m");
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

    #[test]
    fn task_environment_defaults_to_native_platform() {
        let task = temp_task_dir("default-docker-platform");
        fs::create_dir_all(&task).expect("task dir");
        fs::write(
            task.join("task.toml"),
            r#"
[environment]
build_timeout_sec = 7.5
"#,
        )
        .expect("task toml");

        let environment = task_environment(&task).expect("environment");

        assert!(!environment.prebuilt_image);
        assert_eq!(environment.platform, None);

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
