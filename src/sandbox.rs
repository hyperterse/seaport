use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::logging::{self, LogMode};
use crate::CliError;

const DEFAULT_DOCKER_IMAGE: &str = "ubuntu:24.04";
const DEFAULT_CONTAINER_MEMORY_MB: u64 = 1024;
const DEFAULT_CONTAINER_CPUS: &str = "1.0";
const BOOSTED_CONTAINER_CPUS_MAX: usize = 8;
const CONTAINER_PIDS_LIMIT: &str = "4096";
const DEFAULT_COMPAT_DOCKER_PLATFORM: &str = "linux/amd64";
const DOCKER_BUILD_ATTEMPTS: usize = 3;
const DOCKER_BUILD_RETRY_DELAY: Duration = Duration::from_secs(2);
const DOCKER_PULL_ATTEMPTS: usize = 3;
const DOCKER_PULL_RETRY_DELAY: Duration = Duration::from_secs(2);
const DOCKER_BUILD_TIMEOUT: Duration = Duration::from_secs(600);
const DOCKER_BUILDER_TIMEOUT: Duration = Duration::from_secs(60);
const DOCKER_WORKSPACE_TIMEOUT: Duration = Duration::from_secs(120);
const DOCKER_IMAGE_CACHE_NAMESPACE: &str = "seaport-env-cache-v1";
const DEFAULT_BUILDKIT_BUILDER: &str = "seaport-builder";
const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
static DOCKER_AVAILABLE: OnceLock<()> = OnceLock::new();
static DOCKER_IMAGE_PULLS: OnceLock<ImagePullState> = OnceLock::new();
static SANDBOX_LOG_MODE: AtomicU8 = AtomicU8::new(LogMode::Concise as u8);

pub(crate) fn set_log_mode(mode: LogMode) {
    SANDBOX_LOG_MODE.store(mode as u8, Ordering::Relaxed);
}

fn log_mode() -> LogMode {
    LogMode::from_u8(SANDBOX_LOG_MODE.load(Ordering::Relaxed))
}

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
    pub(crate) strict_resources: bool,
    pub(crate) concurrency: usize,
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
        SandboxBackend::Docker => run_scripts_in_docker(
            runtime,
            run.agent,
            run.envs,
            &environment,
            run.strict_resources,
            run.concurrency,
        ),
        SandboxBackend::UnsafeLocal => {
            prepare_task_file_workspace(run.task_path, run.app_dir)?;
            run_scripts_locally(runtime, run.agent, run.envs, &environment)
        }
    }
}

pub(crate) fn ensure_sandbox_backend_available(backend: SandboxBackend) -> Result<(), CliError> {
    match backend {
        SandboxBackend::Docker => ensure_docker_available(),
        SandboxBackend::UnsafeLocal => Ok(()),
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

fn run_scripts_in_docker(
    runtime: TaskRuntime<'_>,
    agent_kind: &SandboxAgent,
    envs: &PhaseEnvs,
    environment: &TaskEnvironment,
    strict_resources: bool,
    concurrency: usize,
) -> Result<ScriptOutputs, CliError> {
    ensure_docker_available()?;
    let resources = if strict_resources {
        environment.resources.clone()
    } else {
        environment.resources.boosted(concurrency)
    };

    let prepare_started = Instant::now();
    let image = prepare_docker_image(runtime.task_label, runtime.task_path, environment)?;
    logging::log_timing(
        runtime.task_label,
        "image",
        &format!("prepare_docker_image -> {}", image.reference),
        prepare_started.elapsed(),
    );
    let logs_root = runtime
        .logs_dir
        .parent()
        .ok_or_else(|| CliError::usage("logs directory has no parent"))?;

    // One long-lived container hosts the whole trial and scripts run in it
    // via `docker exec`, so state the solution creates outside /app (installed
    // packages, tool caches, $HOME) is still present for the verifier and the
    // root filesystem stays writable for tasks that install at runtime. This
    // matches how tasks behave under harbor.
    let container = docker_container_name(runtime.run_id, "trial");
    let start_started = Instant::now();
    start_trial_container(
        StartTrialContainer {
            container_name: &container,
            image: &image.reference,
            task_path: runtime.task_path,
            logs_root,
            network: environment.agent_network,
            platform: image.platform.as_deref(),
            resources: &resources,
        },
        runtime.task_label,
    )?;
    logging::log_timing(
        runtime.task_label,
        "container",
        "start trial container",
        start_started.elapsed(),
    );

    let result = (|| {
        prep_container_workspace(&container, runtime.task_label)?;

        let agent = match agent_kind {
            SandboxAgent::Oracle => {
                let agent_env = env_refs(&envs.agent);

                AgentStep::from_output(
                    "solution/solve.sh",
                    exec_in_container(ContainerExec {
                        container: &container,
                        task_label: runtime.task_label,
                        phase: "solution",
                        label: "solution/solve.sh",
                        invocation: ContainerInvocation::TaskScript("solution/solve.sh"),
                        env: &agent_env,
                        timeout: environment.agent_timeout,
                    })?,
                )
            }
            SandboxAgent::Nop => AgentStep::nop(),
            SandboxAgent::External(agent) => {
                let mut agent_env = env_refs(&envs.agent);
                agent_env.push(("SEAPORT_AGENT_NAME", agent.name.as_str()));

                if let Some(model) = agent.model.as_deref() {
                    agent_env.push(("SEAPORT_MODEL", model));
                }

                AgentStep::from_output(
                    agent.command.clone(),
                    exec_in_container(ContainerExec {
                        container: &container,
                        task_label: runtime.task_label,
                        phase: "agent",
                        label: &agent.name,
                        invocation: ContainerInvocation::ShellCommand(&agent.command),
                        env: &agent_env,
                        timeout: environment.agent_timeout,
                    })?,
                )
            }
        };

        if environment.verifier_network != environment.agent_network {
            switch_container_network(
                &container,
                environment.agent_network,
                environment.verifier_network,
            )?;
        }

        let verifier_env = env_refs(&envs.verifier);
        let verifier = exec_in_container(ContainerExec {
            container: &container,
            task_label: runtime.task_label,
            phase: "verifier",
            label: "tests/test.sh",
            invocation: ContainerInvocation::TaskScript("tests/test.sh"),
            env: &verifier_env,
            timeout: environment.verifier_timeout,
        })?;

        Ok(ScriptOutputs { agent, verifier })
    })();

    cleanup_docker_container(&container);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompatPlatformInference {
    platform: &'static str,
    reason: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DockerResources {
    cpus: Option<String>,
    memory_mb: Option<u64>,
}

impl Default for DockerResources {
    fn default() -> Self {
        Self {
            cpus: Some(DEFAULT_CONTAINER_CPUS.to_owned()),
            memory_mb: Some(DEFAULT_CONTAINER_MEMORY_MB),
        }
    }
}

impl DockerResources {
    /// Gives each trial a fair share of the host's CPUs rather than the task's
    /// native `cpus` cap. Task authors size `cpus` for native execution; on
    /// this runner (often emulating amd64) honoring it strictly starves the
    /// workload. The share is the host divided by how many trials run at once,
    /// so a single trial can use many cores while a full slate of concurrent
    /// trials each gets roughly one — it never promises more CPU than exists.
    /// Memory is left at the task's declared limit: memory is incompressible,
    /// and inflating it would let concurrent trials overcommit the docker VM.
    fn boosted(&self, concurrency: usize) -> Self {
        Self {
            cpus: Some(fair_cpu_share(concurrency)),
            memory_mb: self.memory_mb,
        }
    }
}

fn fair_cpu_share(concurrency: usize) -> String {
    let host_cpus = thread::available_parallelism()
        .map(|cpus| cpus.get())
        .unwrap_or(4);
    let concurrency = concurrency.max(1);

    let share = (host_cpus / concurrency)
        .clamp(1, BOOSTED_CONTAINER_CPUS_MAX)
        .min(host_cpus);

    share.to_string()
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

        resources.cpus = Some(cpus);
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

        resources.memory_mb = Some(parsed);
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

/// Materializes packaged task files and COBOL copybook aliases inside the
/// trial container's /app, then makes the tree world-writable so scripts can
/// run as any user the image selects.
const PREP_WORKSPACE_SCRIPT: &str = r#"set -e
mkdir -p /app
if [ -d /seaport/task/environment/task_file ]; then
  rm -rf /app/task_file
  mkdir -p /app/task_file
  cp -a /seaport/task/environment/task_file/. /app/task_file/
fi
find /app -type f -iname '*.cpy' > /tmp/seaport-copybooks 2>/dev/null || true
while IFS= read -r copybook; do
  dir=$(dirname "$copybook")
  base=$(basename "$copybook")
  stem="${base%.*}"
  upper=$(printf '%s' "$stem" | tr '[:lower:]' '[:upper:]')
  lower=$(printf '%s' "$stem" | tr '[:upper:]' '[:lower:]')
  for s in "$stem" "$upper" "$lower"; do
    for ext in '' .cpy .CPY .cob .COB; do
      for target_dir in "$dir" /app; do
        alias_path="$target_dir/$s$ext"
        if [ "$alias_path" != "$copybook" ] && [ ! -e "$alias_path" ]; then
          cp "$copybook" "$alias_path"
        fi
      done
    done
  done
done < /tmp/seaport-copybooks
chmod -R a+rwX /app
"#;

struct StartTrialContainer<'a> {
    container_name: &'a str,
    image: &'a str,
    task_path: &'a Path,
    logs_root: &'a Path,
    network: DockerNetwork,
    platform: Option<&'a str>,
    resources: &'a DockerResources,
}

fn start_trial_container(start: StartTrialContainer<'_>, task_label: &str) -> Result<(), CliError> {
    let container_name = start.container_name;
    let timed_output = run_command_with_timeout(
        docker_start_command(&start),
        DOCKER_WORKSPACE_TIMEOUT,
        Some(CommandLog::new(task_label, "container")),
    )?;

    if timed_output.timed_out {
        cleanup_docker_container(container_name);
        return Err(CliError::task_failed(format!(
            "docker trial container start timed out after {:.3}s",
            DOCKER_WORKSPACE_TIMEOUT.as_secs_f64()
        )));
    }

    if !timed_output.output.status.success() {
        cleanup_docker_container(container_name);
        return Err(CliError::task_failed(format!(
            "docker trial container start failed (status: {})\nstdout:\n{}\nstderr:\n{}",
            timed_output.output.status,
            String::from_utf8_lossy(&timed_output.output.stdout),
            String::from_utf8_lossy(&timed_output.output.stderr)
        )));
    }

    Ok(())
}

fn docker_start_command(start: &StartTrialContainer<'_>) -> Command {
    let mut command = Command::new("docker");
    command.args([
        "run",
        "-d",
        "--name",
        start.container_name,
        "--network",
        start.network.as_docker_run_arg(),
        "--pids-limit",
        CONTAINER_PIDS_LIMIT,
        "--workdir",
        "/app",
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
    ]);

    if let Some(memory_mb) = start.resources.memory_mb {
        let memory = format!("{memory_mb}m");
        command.args(["--memory", &memory, "--memory-swap", &memory]);
    }

    if let Some(cpus) = start.resources.cpus.as_deref() {
        command.args(["--cpus", cpus]);
    }

    if let Some(platform) = start.platform {
        command.args(["--platform", platform]);
    }

    command
        .arg("--mount")
        .arg(format!(
            "type=bind,source={},target=/logs",
            start.logs_root.display()
        ))
        .arg("--mount")
        .arg(format!(
            "type=bind,source={},target=/tests,readonly",
            start.task_path.join("tests").display()
        ))
        .arg("--mount")
        .arg(format!(
            "type=bind,source={},target=/seaport/task,readonly",
            start.task_path.display()
        ))
        .arg(start.image)
        .args(["bash", "-c", "while true; do sleep 3600; done"]);
    command
}

fn prep_container_workspace(container: &str, task_label: &str) -> Result<(), CliError> {
    let mut command = Command::new("docker");
    command
        .args(["exec", "--user", "0:0", container, "bash", "-c"])
        .arg(PREP_WORKSPACE_SCRIPT);

    let timed_output = run_command_with_timeout(
        command,
        DOCKER_WORKSPACE_TIMEOUT,
        Some(CommandLog::new(task_label, "workspace")),
    )?;

    if timed_output.timed_out {
        cleanup_docker_container(container);
        return Err(CliError::task_failed(format!(
            "docker workspace preparation timed out after {:.3}s",
            DOCKER_WORKSPACE_TIMEOUT.as_secs_f64()
        )));
    }

    if !timed_output.output.status.success() {
        return Err(CliError::task_failed(format!(
            "docker workspace preparation failed (status: {})\nstdout:\n{}\nstderr:\n{}",
            timed_output.output.status,
            String::from_utf8_lossy(&timed_output.output.stdout),
            String::from_utf8_lossy(&timed_output.output.stderr)
        )));
    }

    Ok(())
}

fn switch_container_network(
    container: &str,
    from: DockerNetwork,
    to: DockerNetwork,
) -> Result<(), CliError> {
    for (action, network) in [
        ("disconnect", from.as_docker_run_arg()),
        ("connect", to.as_docker_run_arg()),
    ] {
        let output = Command::new("docker")
            .args(["network", action, network, container])
            .output()?;

        if !output.status.success() {
            return Err(CliError::task_failed(format!(
                "docker network {action} {network} failed for {container} (status: {})\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
    }

    Ok(())
}
fn ensure_docker_image_available(
    task_label: &str,
    image: &str,
    platform: Option<&str>,
    timeout: Duration,
) -> Result<(), CliError> {
    if docker_image_exists(image) {
        print_backend_event(task_label, "pull", &format!("cache hit: {image}"));
        return Ok(());
    }

    let _pull = DockerImagePull::start(image);

    if docker_image_exists(image) {
        print_backend_event(task_label, "pull", &format!("cache hit: {image}"));
        return Ok(());
    }

    let timed_output = run_docker_pull_with_retries(task_label, image, platform, timeout)?;

    if timed_output.timed_out {
        return Err(CliError::task_failed(format!(
            "docker image pull timed out after {:.3}s for {image}",
            timeout.as_secs_f64()
        )));
    }

    if !timed_output.output.status.success() {
        return Err(CliError::task_failed(format!(
            "docker image pull failed for {image} (status: {})\nstdout:\n{}\nstderr:\n{}",
            timed_output.output.status,
            String::from_utf8_lossy(&timed_output.output.stdout),
            String::from_utf8_lossy(&timed_output.output.stderr)
        )));
    }

    Ok(())
}

struct ImagePullState {
    active: Mutex<HashSet<String>>,
    ready: Condvar,
}

impl ImagePullState {
    fn shared() -> &'static Self {
        DOCKER_IMAGE_PULLS.get_or_init(|| Self {
            active: Mutex::new(HashSet::new()),
            ready: Condvar::new(),
        })
    }
}

struct DockerImagePull {
    image: String,
    state: &'static ImagePullState,
}

impl DockerImagePull {
    fn start(image: &str) -> Self {
        let state = ImagePullState::shared();
        let mut active = state.active.lock().expect("docker image pull state");

        while active.contains(image) {
            active = state.ready.wait(active).expect("docker image pull wait");
        }

        active.insert(image.to_owned());

        Self {
            image: image.to_owned(),
            state,
        }
    }
}

impl Drop for DockerImagePull {
    fn drop(&mut self) {
        let mut active = self.state.active.lock().expect("docker image pull state");
        active.remove(&self.image);
        self.state.ready.notify_all();
    }
}

fn prepare_docker_image(
    task_label: &str,
    task_path: &Path,
    environment: &TaskEnvironment,
) -> Result<DockerImage, CliError> {
    let dockerfile = task_path.join("environment").join("Dockerfile");
    let inferred_platform = if environment.platform.is_none() {
        infer_compat_platform(task_path)?
    } else {
        None
    };
    let platform = environment.platform.clone().or_else(|| {
        inferred_platform
            .as_ref()
            .map(|inference| inference.platform.to_owned())
    });

    if environment.prebuilt_image || !dockerfile.is_file() {
        ensure_docker_image_available(
            task_label,
            &environment.image,
            platform.as_deref(),
            environment.build_timeout,
        )?;

        return Ok(DockerImage {
            reference: environment.image.clone(),
            remove_after_run: false,
            platform,
        });
    }

    let environment_dir = dockerfile
        .parent()
        .ok_or_else(|| CliError::usage("environment/Dockerfile has no parent directory"))?;
    if let Some(inference) = inferred_platform {
        print_backend_notice(
            task_label,
            "build",
            &format!("{}; using {}", inference.reason, inference.platform),
        );
    }

    let mut build_platform = platform;
    let mut cached_image =
        cached_docker_image(environment_dir, environment, build_platform.as_deref())?;
    let _build_guard = DockerImagePull::start(&format!("build:{}", cached_image.reference));

    if docker_image_exists(&cached_image.reference) {
        print_backend_event(
            task_label,
            "build",
            &format!("cache hit: {}", cached_image.reference),
        );

        return Ok(DockerImage {
            reference: cached_image.reference,
            remove_after_run: false,
            platform: build_platform,
        });
    }

    let mut timed_output = run_docker_build_with_retries(
        task_label,
        &cached_image.reference,
        environment_dir,
        environment,
        build_platform.as_deref(),
    )?;

    if should_retry_build_with_compat_platform(&timed_output, build_platform.as_deref()) {
        build_platform = Some(DEFAULT_COMPAT_DOCKER_PLATFORM.to_owned());
        cached_image =
            cached_docker_image(environment_dir, environment, build_platform.as_deref())?;
        print_backend_notice(
            task_label,
            "build",
            &format!(
                "native docker build is not available for this image; retrying with {DEFAULT_COMPAT_DOCKER_PLATFORM}"
            ),
        );

        if docker_image_exists(&cached_image.reference) {
            print_backend_event(
                task_label,
                "build",
                &format!("cache hit: {}", cached_image.reference),
            );

            return Ok(DockerImage {
                reference: cached_image.reference,
                remove_after_run: false,
                platform: build_platform,
            });
        } else {
            timed_output = run_docker_build_with_retries(
                task_label,
                &cached_image.reference,
                environment_dir,
                environment,
                build_platform.as_deref(),
            )?;
        }
    }

    let output = timed_output.output;

    if timed_output.timed_out {
        cleanup_docker_image(&cached_image.reference);
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
        reference: cached_image.reference,
        remove_after_run: false,
        platform: build_platform,
    })
}

struct CachedDockerImage {
    reference: String,
}

fn cached_docker_image(
    environment_dir: &Path,
    environment: &TaskEnvironment,
    platform: Option<&str>,
) -> Result<CachedDockerImage, CliError> {
    let cache_key = docker_environment_cache_key(environment_dir, environment, platform)?;

    Ok(CachedDockerImage {
        reference: format!("seaport-env-cache:{cache_key}"),
    })
}

fn docker_environment_cache_key(
    environment_dir: &Path,
    environment: &TaskEnvironment,
    platform: Option<&str>,
) -> Result<String, CliError> {
    let mut hash = FNV_OFFSET_BASIS;

    hash_cache_str(&mut hash, DOCKER_IMAGE_CACHE_NAMESPACE);
    hash_cache_str(&mut hash, platform.unwrap_or("native"));
    hash_cache_str(&mut hash, environment.build_network.as_docker_build_arg());
    hash_directory(&mut hash, environment_dir, environment_dir)?;

    Ok(format!("{hash:016x}"))
}

fn hash_directory(hash: &mut u64, root: &Path, directory: &Path) -> Result<(), CliError> {
    let mut entries = fs::read_dir(directory)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();

    for entry_path in entries {
        let metadata = fs::symlink_metadata(&entry_path)?;
        let file_type = metadata.file_type();
        let relative_path = cache_relative_path(root, &entry_path)?;

        if file_type.is_dir() {
            hash_cache_str(hash, "dir");
            hash_cache_str(hash, &relative_path);
            hash_directory(hash, root, &entry_path)?;
        } else if file_type.is_file() {
            hash_cache_str(hash, "file");
            hash_cache_str(hash, &relative_path);
            hash_cache_bytes(hash, &fs::read(&entry_path)?);
        } else if file_type.is_symlink() {
            hash_cache_str(hash, "symlink");
            hash_cache_str(hash, &relative_path);
            hash_cache_str(hash, &fs::read_link(&entry_path)?.to_string_lossy());
        } else {
            return Err(CliError::usage(format!(
                "unsupported file in docker build context: {}",
                entry_path.display()
            )));
        }
    }

    Ok(())
}

fn cache_relative_path(root: &Path, path: &Path) -> Result<String, CliError> {
    let relative = path.strip_prefix(root).map_err(|error| {
        CliError::io(format!(
            "could not compute cache path for {} relative to {}: {error}",
            path.display(),
            root.display()
        ))
    })?;

    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn hash_cache_str(hash: &mut u64, value: &str) {
    hash_cache_bytes(hash, value.as_bytes());
}

fn hash_cache_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }

    *hash ^= 0xff;
    *hash = hash.wrapping_mul(FNV_PRIME);
}

fn docker_image_exists(reference: &str) -> bool {
    if let Some(exists) = docker_api_image_exists(reference) {
        return exists;
    }

    Command::new("docker")
        .args(["image", "inspect", reference])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn infer_compat_platform(task_path: &Path) -> Result<Option<CompatPlatformInference>, CliError> {
    if !compat_docker_platform_available() {
        return Ok(None);
    }

    if task_contains_x86_assembly(task_path)? {
        return Ok(Some(CompatPlatformInference {
            platform: DEFAULT_COMPAT_DOCKER_PLATFORM,
            reason: "x86 assembly detected",
        }));
    }

    if task_uses_legacy_java7_toolchain(task_path)? {
        return Ok(Some(CompatPlatformInference {
            platform: DEFAULT_COMPAT_DOCKER_PLATFORM,
            reason: "legacy Java 7 toolchain requires amd64 on this host",
        }));
    }

    Ok(None)
}

fn task_contains_x86_assembly(task_path: &Path) -> Result<bool, CliError> {
    path_contains_x86_assembly(&task_path.join("environment"))
}

fn task_uses_legacy_java7_toolchain(task_path: &Path) -> Result<bool, CliError> {
    dockerfile_uses_legacy_java7_toolchain(&task_path.join("environment").join("Dockerfile"))
}

fn dockerfile_uses_legacy_java7_toolchain(dockerfile: &Path) -> Result<bool, CliError> {
    if !dockerfile.is_file() {
        return Ok(false);
    }

    let contents = fs::read(dockerfile)?;
    let contents = String::from_utf8_lossy(&contents).to_ascii_lowercase();

    Ok(["zulu7-jdk", "openjdk-7-jdk", "openjdk-7-jre"]
        .iter()
        .any(|package| contents.contains(package)))
}

fn path_contains_x86_assembly(path: &Path) -> Result<bool, CliError> {
    if !path.is_dir() {
        return Ok(false);
    }

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            if path_contains_x86_assembly(&entry_path)? {
                return Ok(true);
            }
        } else if file_type.is_file() && is_assembly_source(&entry_path) {
            let source = fs::read(&entry_path)?;

            if assembly_source_mentions_x86(&source) {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn is_assembly_source(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("s") || extension.eq_ignore_ascii_case("asm")
        })
}

fn assembly_source_mentions_x86(source: &[u8]) -> bool {
    let source = String::from_utf8_lossy(source).to_ascii_lowercase();

    [
        ".intel_syntax",
        "%rax",
        "%eax",
        "%rip",
        " rax",
        " eax",
        " syscall",
    ]
    .iter()
    .any(|marker| source.contains(marker))
}

fn run_docker_build_with_retries(
    task_label: &str,
    tag: &str,
    environment_dir: &Path,
    environment: &TaskEnvironment,
    platform: Option<&str>,
) -> Result<TimedOutput, CliError> {
    ensure_seaport_buildkit_builder(task_label)?;

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
        print_backend_notice(
            task_label,
            "build",
            &format!(
                "transient docker build failure; retrying attempt {attempt}/{DOCKER_BUILD_ATTEMPTS} in {}",
                format_duration(DOCKER_BUILD_RETRY_DELAY)
            ),
        );
        thread::sleep(DOCKER_BUILD_RETRY_DELAY);
    }
}

fn run_docker_pull_with_retries(
    task_label: &str,
    image: &str,
    platform: Option<&str>,
    timeout: Duration,
) -> Result<TimedOutput, CliError> {
    let mut attempt = 1;

    loop {
        let timed_output = run_command_with_timeout(
            docker_pull_command(image, platform),
            timeout,
            Some(CommandLog::new(task_label, "pull")),
        )?;

        if timed_output.output.status.success()
            || timed_output.timed_out
            || attempt >= DOCKER_PULL_ATTEMPTS
            || !docker_build_transient_failure(&timed_output.output)
        {
            return Ok(timed_output);
        }

        attempt += 1;
        print_backend_notice(
            task_label,
            "pull",
            &format!(
                "transient docker pull failure; retrying attempt {attempt}/{DOCKER_PULL_ATTEMPTS} in {}",
                format_duration(DOCKER_PULL_RETRY_DELAY)
            ),
        );
        thread::sleep(DOCKER_PULL_RETRY_DELAY);
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
        "buildx",
        "build",
        "--builder",
        &buildkit_builder_name(),
        "--progress=plain",
        "--pull=false",
        "--load",
        "--network",
        environment.build_network.as_docker_build_arg(),
    ]);

    if let Some(platform) = platform {
        command.args(["--platform", platform]);
    }

    command.args(["-t", tag]).arg(environment_dir);
    command
}

fn ensure_seaport_buildkit_builder(task_label: &str) -> Result<(), CliError> {
    let builder = buildkit_builder_name();

    if docker_buildx_builder_exists(&builder) {
        return Ok(());
    }

    print_backend_notice(
        task_label,
        "builder",
        &format!("creating BuildKit builder: {builder}"),
    );
    let timed_output = run_command_with_timeout(
        docker_buildx_create_command(&builder),
        DOCKER_BUILDER_TIMEOUT,
        Some(CommandLog::new(task_label, "builder")),
    )?;

    if timed_output.timed_out {
        return Err(CliError::task_failed(format!(
            "docker buildx builder creation timed out after {:.3}s for {builder}",
            DOCKER_BUILDER_TIMEOUT.as_secs_f64()
        )));
    }

    if timed_output.output.status.success() || docker_buildx_builder_exists(&builder) {
        return Ok(());
    }

    Err(CliError::task_failed(format!(
        "docker buildx builder creation failed for {builder} (status: {})\nstdout:\n{}\nstderr:\n{}",
        timed_output.output.status,
        String::from_utf8_lossy(&timed_output.output.stdout),
        String::from_utf8_lossy(&timed_output.output.stderr)
    )))
}

fn docker_buildx_builder_exists(builder: &str) -> bool {
    Command::new("docker")
        .args(["buildx", "inspect", builder])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn docker_buildx_create_command(builder: &str) -> Command {
    let mut command = Command::new("docker");
    command.args([
        "buildx",
        "create",
        "--name",
        builder,
        "--driver",
        "docker-container",
        "--driver-opt",
        "network=host",
    ]);
    command
}

fn buildkit_builder_name() -> String {
    env::var("SEAPORT_BUILDKIT_BUILDER")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BUILDKIT_BUILDER.to_owned())
}

fn docker_pull_command(image: &str, platform: Option<&str>) -> Command {
    let mut command = Command::new("docker");
    command.arg("pull");

    if let Some(platform) = platform {
        command.args(["--platform", platform]);
    }

    command.arg(image);
    command
}

fn ensure_docker_available() -> Result<(), CliError> {
    if DOCKER_AVAILABLE.get().is_some() {
        return Ok(());
    }

    if docker_api_ping() {
        let _ = DOCKER_AVAILABLE.set(());
        return Ok(());
    }

    let output = Command::new("docker")
        .arg("version")
        .arg("--format")
        .arg("{{.Server.Version}}")
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let _ = DOCKER_AVAILABLE.set(());
            Ok(())
        }
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

enum ContainerInvocation<'a> {
    TaskScript(&'a str),
    ShellCommand(&'a str),
}

struct ContainerExec<'a> {
    container: &'a str,
    task_label: &'a str,
    phase: &'static str,
    label: &'a str,
    invocation: ContainerInvocation<'a>,
    env: &'a [(&'a str, &'a str)],
    timeout: Duration,
}

fn exec_in_container(exec: ContainerExec<'_>) -> Result<Output, CliError> {
    let timed_output = run_command_with_timeout(
        docker_exec_command(&exec),
        exec.timeout,
        Some(CommandLog::new(exec.task_label, exec.phase)),
    )?;
    let output = timed_output.output;

    if timed_output.timed_out {
        // Killing the docker exec client does not stop the process inside the
        // container, so tear the container down to enforce the timeout.
        cleanup_docker_container(exec.container);
        return Err(CliError::task_failed(format!(
            "sandboxed docker command timed out after {:.3}s: {}",
            exec.timeout.as_secs_f64(),
            exec.label
        )));
    }

    if !output.status.success() {
        return Err(CliError::task_failed(format!(
            "sandboxed docker command failed: {} (status: {})\nstdout:\n{}\nstderr:\n{}",
            exec.label,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(output)
}

fn docker_exec_command(exec: &ContainerExec<'_>) -> Command {
    let mut command = Command::new("docker");
    command.args(["exec", "--workdir", "/app"]).args(
        exec.env
            .iter()
            .flat_map(|(name, value)| ["--env".to_owned(), format!("{name}={value}")]),
    );
    command.arg(exec.container).arg("bash");

    match exec.invocation {
        ContainerInvocation::TaskScript(script) => {
            command.arg(format!("/seaport/task/{script}"));
        }
        ContainerInvocation::ShellCommand(shell_command) => {
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
    if docker_api_remove_container(container_name) {
        return;
    }

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
    if docker_api_remove_image(image) {
        return;
    }

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

fn docker_api_ping() -> bool {
    matches!(
        docker_api_request("GET", "/_ping"),
        Some(Ok(response)) if response.status == 200
    )
}

fn docker_api_image_exists(reference: &str) -> Option<bool> {
    let response = docker_api_request("GET", &docker_api_image_json_path(reference))?.ok()?;

    match response.status {
        200 => Some(true),
        404 => Some(false),
        _ => None,
    }
}

fn docker_api_remove_container(container_name: &str) -> bool {
    docker_api_delete_success(&format!("/containers/{container_name}?force=true&v=false"))
}

fn docker_api_remove_image(image: &str) -> bool {
    docker_api_delete_success(&format!("/images/{image}?force=true"))
}

fn docker_api_delete_success(path: &str) -> bool {
    matches!(
        docker_api_request("DELETE", path),
        Some(Ok(response)) if response.status == 204 || response.status == 404
    )
}

fn docker_api_image_json_path(reference: &str) -> String {
    format!("/images/{reference}/json")
}

struct DockerApiResponse {
    status: u16,
}

#[cfg(unix)]
fn docker_api_request(method: &str, path: &str) -> Option<io::Result<DockerApiResponse>> {
    use std::os::unix::net::UnixStream;

    let socket = docker_socket_path()?;
    let mut stream = UnixStream::connect(socket).ok()?;
    let request = format!("{method} {path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\r\n");

    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    if let Err(error) = stream.write_all(request.as_bytes()) {
        return Some(Err(error));
    }

    let mut response = Vec::new();

    match stream.read_to_end(&mut response) {
        Ok(_) => Some(parse_docker_api_response(&response)),
        Err(error) => Some(Err(error)),
    }
}

#[cfg(not(unix))]
fn docker_api_request(_method: &str, _path: &str) -> Option<io::Result<DockerApiResponse>> {
    None
}

#[cfg(unix)]
fn docker_socket_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("SEAPORT_DOCKER_SOCKET") {
        return Some(PathBuf::from(path));
    }

    if let Ok(host) = env::var("DOCKER_HOST") {
        if let Some(path) = host.strip_prefix("unix://") {
            return Some(PathBuf::from(path));
        }
    }

    let mut candidates = vec![PathBuf::from("/var/run/docker.sock")];

    if let Some(home) = env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".docker/run/docker.sock"));
    }

    candidates
        .iter()
        .find(|candidate| candidate.exists())
        .cloned()
        .or_else(|| candidates.into_iter().next())
}

#[cfg(unix)]
fn parse_docker_api_response(response: &[u8]) -> io::Result<DockerApiResponse> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP headers"))?;
    let headers = String::from_utf8_lossy(&response[..header_end]);
    let status_line = headers
        .lines()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP status"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP status code"))?
        .parse::<u16>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    Ok(DockerApiResponse { status })
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
    mode: LogMode,
}

impl CommandLog {
    fn new(task: &str, phase: &str) -> Self {
        Self {
            task: task.to_owned(),
            phase: phase.to_owned(),
            mode: log_mode(),
        }
    }

    fn stream(&self, name: &'static str) -> StreamLog {
        StreamLog {
            task: self.task.clone(),
            phase: self.phase.clone(),
            stream: name,
            mode: self.mode,
        }
    }
}

struct StreamLog {
    task: String,
    phase: String,
    stream: &'static str,
    mode: LogMode,
}

fn run_command_with_timeout(
    mut command: Command,
    timeout: Duration,
    log: Option<CommandLog>,
) -> Result<TimedOutput, CliError> {
    if let Some(log) = &log {
        print_phase_start(log, timeout);
    }

    let command_line = if logging::timings_enabled() {
        Some(format_command_line(&command))
    } else {
        None
    };
    let command_started = Instant::now();

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

    if let Some(command_line) = command_line {
        let (task, phase) = log
            .as_ref()
            .map(|log| (log.task.as_str(), log.phase.as_str()))
            .unwrap_or(("-", "command"));
        logging::log_timing(
            task,
            phase,
            &format!(
                "status={}{} cmd: {command_line}",
                status
                    .code()
                    .map_or_else(|| "?".to_owned(), |code| code.to_string()),
                if timed_out { " timed-out" } else { "" }
            ),
            command_started.elapsed(),
        );
    }

    Ok(TimedOutput {
        output: Output {
            status,
            stdout,
            stderr,
        },
        timed_out,
    })
}

fn format_command_line(command: &Command) -> String {
    let mut line = command.get_program().to_string_lossy().into_owned();

    for arg in command.get_args() {
        line.push(' ');
        line.push_str(&arg.to_string_lossy());

        if line.len() > 160 {
            line.truncate(157);
            line.push_str("...");
            break;
        }
    }

    line
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
    if !log.mode.is_verbose() {
        return;
    }

    let text = String::from_utf8_lossy(line);
    let text = text.trim_end_matches(['\r', '\n']);

    println!(
        "    {:<44} {:<9} {:<6} {}",
        fit_log_text(&log.task, 44),
        log.phase,
        log.stream,
        text
    );
}

fn print_phase_start(log: &CommandLog, timeout: Duration) {
    if !log.mode.is_verbose() {
        return;
    }

    println!(
        "  · {:<44} {:<9} timeout {}",
        fit_log_text(&log.task, 44),
        log.phase,
        format_duration(timeout)
    );
    let _ = io::stdout().flush();
}

fn print_backend_event(task_label: &str, phase: &str, message: &str) {
    if log_mode().is_verbose() {
        println!(
            "    {:<44} {:<9} {}",
            fit_log_text(task_label, 44),
            phase,
            message
        );
    }
}

fn print_backend_notice(task_label: &str, phase: &str, message: &str) {
    if log_mode().is_verbose() {
        println!(
            "  · {:<44} {:<9} {}",
            fit_log_text(task_label, 44),
            phase,
            message
        );
    }
}

fn fit_log_text(value: &str, width: usize) -> String {
    let length = value.chars().count();

    if length <= width {
        format!("{value:<width$}")
    } else if width <= 3 {
        value.chars().take(width).collect()
    } else {
        let mut trimmed = value.chars().take(width - 3).collect::<String>();
        trimmed.push_str("...");
        trimmed
    }
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
    fn docker_start_command_configures_trial_container() {
        let command = docker_start_command(&StartTrialContainer {
            container_name: "seaport-test-container",
            image: "seaport-task-test",
            task_path: Path::new("/tmp/task"),
            logs_root: Path::new("/tmp/logs"),
            network: DockerNetwork::None,
            platform: Some("linux/amd64"),
            resources: &DockerResources::default(),
        });
        let args = command_args(command);

        assert_eq!(args.first().map(String::as_str), Some("run"));
        assert_eq!(args.get(1).map(String::as_str), Some("-d"));
        assert!(args
            .windows(2)
            .any(|window| window == ["--network", "none"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--platform", "linux/amd64"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--pids-limit", "4096"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--memory", "1024m"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--memory-swap", "1024m"]));
        assert!(args.windows(2).any(|window| window == ["--cpus", "1.0"]));
        // Tasks install packages and write outside /app at runtime, so the
        // container root filesystem must stay writable with default
        // capabilities, matching harbor's execution environment.
        assert!(!args.iter().any(|arg| arg == "--read-only"));
        assert!(!args.iter().any(|arg| arg == "--cap-drop"));
        assert!(!args.iter().any(|arg| arg == "--security-opt"));
        assert!(!args.iter().any(|arg| arg == "--tmpfs"));
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
        assert!(args
            .iter()
            .any(|arg| arg == "type=bind,source=/tmp/logs,target=/logs"));
        assert!(args
            .windows(2)
            .any(|window| window == ["--workdir", "/app"]));
        assert_eq!(
            args.last().map(String::as_str),
            Some("while true; do sleep 3600; done")
        );
    }

    #[test]
    fn docker_exec_command_runs_task_scripts_with_phase_env() {
        let command = docker_exec_command(&ContainerExec {
            container: "seaport-test-container",
            task_label: "acme/demo",
            phase: "verifier",
            label: "tests/test.sh",
            invocation: ContainerInvocation::TaskScript("tests/test.sh"),
            env: &[("CHECK", "1")],
            timeout: Duration::from_secs(60),
        });
        let args = command_args(command);

        assert_eq!(args.first().map(String::as_str), Some("exec"));
        assert!(args
            .windows(2)
            .any(|window| window == ["--workdir", "/app"]));
        assert!(args.windows(2).any(|window| window == ["--env", "CHECK=1"]));
        assert!(args.iter().any(|arg| arg == "seaport-test-container"));
        assert_eq!(
            args.last().map(String::as_str),
            Some("/seaport/task/tests/test.sh")
        );
    }

    #[test]
    fn docker_exec_command_runs_shell_agents_via_login_shell() {
        let command = docker_exec_command(&ContainerExec {
            container: "seaport-test-container",
            task_label: "acme/demo",
            phase: "agent",
            label: "claude",
            invocation: ContainerInvocation::ShellCommand("claude --run"),
            env: &[],
            timeout: Duration::from_secs(60),
        });
        let args = command_args(command);

        assert_eq!(args.last().map(String::as_str), Some("claude --run"));
        assert!(args.iter().any(|arg| arg == "-lc"));
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

        assert_eq!(args.first().map(String::as_str), Some("buildx"));
        assert_eq!(args.get(1).map(String::as_str), Some("build"));
        assert!(args
            .windows(2)
            .any(|window| window == ["--builder", "seaport-builder"]));
        assert!(args
            .windows(2)
            .any(|window| window == ["--progress=plain", "--pull=false"]));
        assert!(args.iter().any(|arg| arg == "--load"));
        assert!(args
            .windows(2)
            .any(|window| window == ["--network", "default"]));
        assert!(!args.iter().any(|arg| arg == "-q"));
    }

    #[test]
    fn docker_buildx_create_command_configures_persistent_builder() {
        let command = docker_buildx_create_command("seaport-builder");
        let args = command_args(command);

        assert_eq!(
            args,
            [
                "buildx",
                "create",
                "--name",
                "seaport-builder",
                "--driver",
                "docker-container",
                "--driver-opt",
                "network=host"
            ]
        );
    }

    #[test]
    fn docker_pull_command_honors_platform() {
        let command = docker_pull_command("ubuntu:24.04", Some("linux/amd64"));
        let args = command_args(command);

        assert_eq!(args, ["pull", "--platform", "linux/amd64", "ubuntu:24.04"]);
    }

    #[test]
    fn docker_image_pull_guard_releases_image_key() {
        let image = "seaport-test/image:pull-guard";

        {
            let _pull = DockerImagePull::start(image);
        }

        let _pull = DockerImagePull::start(image);
    }

    #[test]
    fn docker_api_image_path_keeps_registry_reference() {
        assert_eq!(
            docker_api_image_json_path("ghcr.io/acme/image:latest"),
            "/images/ghcr.io/acme/image:latest/json"
        );
    }

    #[cfg(unix)]
    #[test]
    fn parse_docker_api_response_reads_status_and_body() {
        let response =
            parse_docker_api_response(b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\n{}")
                .expect("response");

        assert_eq!(response.status, 404);
    }

    #[test]
    fn docker_environment_cache_key_tracks_platform_and_context() {
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
        let task = temp_task_dir("docker-cache-key");
        let environment_dir = task.join("environment");
        fs::create_dir_all(&environment_dir).expect("environment dir");
        fs::write(
            environment_dir.join("Dockerfile"),
            "FROM ubuntu:24.04\nWORKDIR /app\n",
        )
        .expect("dockerfile");

        let native_key =
            docker_environment_cache_key(&environment_dir, &environment, None).expect("native key");
        let same_native_key =
            docker_environment_cache_key(&environment_dir, &environment, None).expect("same key");
        let amd64_key =
            docker_environment_cache_key(&environment_dir, &environment, Some("linux/amd64"))
                .expect("amd64 key");

        fs::write(
            environment_dir.join("Dockerfile"),
            "FROM ubuntu:24.04\nWORKDIR /workspace\n",
        )
        .expect("updated dockerfile");
        let changed_key = docker_environment_cache_key(&environment_dir, &environment, None)
            .expect("changed key");

        assert_eq!(native_key, same_native_key);
        assert_ne!(native_key, amd64_key);
        assert_ne!(native_key, changed_key);

        let _ = fs::remove_dir_all(task);
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
    fn infer_compat_platform_detects_x86_assembly_on_arm_hosts() {
        let task = temp_task_dir("x86-assembly-platform");
        let source_dir = task.join("environment").join("src");
        fs::create_dir_all(&source_dir).expect("source dir");
        fs::write(
            source_dir.join("main.s"),
            ".intel_syntax noprefix\nmov rax, 60\nsyscall\n",
        )
        .expect("assembly source");

        let platform = infer_compat_platform(&task).expect("platform");

        if cfg!(target_arch = "aarch64") {
            assert_eq!(
                platform.map(|inference| inference.platform),
                Some(DEFAULT_COMPAT_DOCKER_PLATFORM)
            );
        } else {
            assert_eq!(platform, None);
        }

        let _ = fs::remove_dir_all(task);
    }

    #[test]
    fn infer_compat_platform_detects_legacy_java7_toolchains_on_arm_hosts() {
        let task = temp_task_dir("java7-platform");
        let environment_dir = task.join("environment");
        fs::create_dir_all(&environment_dir).expect("environment dir");
        fs::write(
            environment_dir.join("Dockerfile"),
            "FROM ubuntu:24.04\nRUN apt-get update && apt-get install -y zulu7-jdk\n",
        )
        .expect("dockerfile");

        let platform = infer_compat_platform(&task).expect("platform");

        if cfg!(target_arch = "aarch64") {
            let inference = platform.expect("compat platform");
            assert_eq!(inference.platform, DEFAULT_COMPAT_DOCKER_PLATFORM);
            assert_eq!(
                inference.reason,
                "legacy Java 7 toolchain requires amd64 on this host"
            );
        } else {
            assert_eq!(platform, None);
        }

        let _ = fs::remove_dir_all(task);
    }

    #[test]
    fn dockerfile_uses_legacy_java7_toolchain_detects_java7_packages() {
        let task = temp_task_dir("java7-dockerfile");
        let dockerfile = task.join("environment").join("Dockerfile");
        fs::create_dir_all(dockerfile.parent().expect("dockerfile parent")).expect("task dir");
        fs::write(
            &dockerfile,
            "FROM ubuntu:24.04\nRUN apt-get install -y openjdk-7-jdk\n",
        )
        .expect("dockerfile");

        assert!(dockerfile_uses_legacy_java7_toolchain(&dockerfile).expect("detect"));

        let _ = fs::remove_dir_all(task);
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
        assert_eq!(environment.resources.cpus.as_deref(), Some("2"));
        assert_eq!(environment.resources.memory_mb, Some(2048));
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
