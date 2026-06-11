use std::collections::VecDeque;
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod logging;
mod registry;
mod sandbox;
mod target;
mod toml_doc;
mod upgrade;

use logging::{
    begin_progress_buffer, push_progress_line, take_progress_buffer, LogMode, ProgressLine,
};
use registry::{
    resolve_git_task_source, resolve_local_registry_dataset, resolve_local_registry_task,
    resolve_remote_registry_dataset, resolve_remote_registry_task,
    set_log_mode as set_registry_log_mode,
};
use sandbox::{
    cleanup_orphaned_trial_containers, ensure_sandbox_backend_available,
    prepare_container_writable_dir, run_task_scripts, set_log_mode as set_sandbox_log_mode,
    AgentStep, ExternalAgent, PhaseEnvs, SandboxAgent, SandboxBackend, ScriptOutputs, StepOutcome,
    TaskScriptRequest, TimeoutMultipliers, TrialScripts,
};
use target::{RunTarget, TaskRef, TaskSelection};

const EXIT_USAGE: i32 = 2;
const EXIT_UNIMPLEMENTED: i32 = 3;
const EXIT_TASK_FAILED: i32 = 4;
const PROGRESS_BAR_WIDTH: usize = 30;
const RETRY_MIN_WAIT_SECS: f64 = 1.0;
const RETRY_WAIT_MULTIPLIER: f64 = 1.0;
const RETRY_MAX_WAIT_SECS: f64 = 60.0;
const TASK_LABEL_WIDTH: usize = 56;
const FAILURE_TAIL_LINES: usize = 8;
const VERSION_TEXT: &str = concat!(env!("CARGO_PKG_NAME"), " ", env!("SEAPORT_VERSION"));
const CURRENT_VERSION: &str = env!("SEAPORT_VERSION");

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("seaport: {error}");
        process::exit(error.exit_code());
    }
}

fn run(args: Vec<String>) -> Result<(), CliError> {
    let command = args.first().map(String::as_str);

    // `__update-check` is the hidden background refresh spawned by the notice
    // itself, so it must never trigger another notice/respawn. `upgrade` skips
    // the notice because it already reports version status.
    if !matches!(command, Some("__update-check") | Some("upgrade")) {
        let update_check_started = Instant::now();
        upgrade::notify_if_outdated(CURRENT_VERSION);
        logging::log_timing(
            "run",
            "startup",
            "update notice check",
            update_check_started.elapsed(),
        );
    }

    match command {
        None | Some("-h") | Some("--help") => {
            print_help();
            Ok(())
        }
        Some("-v") | Some("-V") | Some("--version") => {
            print_version();
            Ok(())
        }
        Some("run") => run_eval(&args[1..]),
        Some("dataset") | Some("datasets") => dataset(&args[1..]),
        Some("init") => init(&args[1..]),
        Some("view") => view(&args[1..]),
        Some("upgrade") => upgrade::run(&args[1..], CURRENT_VERSION),
        Some("__update-check") => {
            upgrade::refresh_cache();
            Ok(())
        }
        Some(command) => Err(CliError::usage(format!("unknown command `{command}`"))),
    }
}

fn run_eval(args: &[String]) -> Result<(), CliError> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_run_help();
        return Ok(());
    }

    let options = RunOptions::parse(args)?;
    set_registry_log_mode(options.log_mode);
    set_sandbox_log_mode(options.log_mode);

    if !options.has_run_source() {
        return Err(CliError::usage(
            "run requires `-p <path>`, `-d <dataset>`, `-t <task>`, or `--task-git-url <url> -p <path>`",
        ));
    }

    options.validate_sources()?;

    let agent = options.agent.as_deref().unwrap_or("oracle");
    let agent = AgentKind::parse(agent)?;

    if agent.requires_model() && options.model.is_none() && options.agent_command.is_none() {
        return Err(CliError::usage(
            "run requires `-m <model>` for model-backed agents",
        ));
    }

    begin_progress_buffer();
    print_run_start(&options, agent)?;
    let resolve_started = Instant::now();
    let target = match resolve_run_target(&options) {
        Ok(target) => target,
        Err(error) => {
            print_resolution_progress(&options, take_progress_buffer())?;
            return Err(error);
        }
    };
    logging::log_timing(
        "run",
        "resolve",
        "target resolution (registry + task downloads)",
        resolve_started.elapsed(),
    );
    let resolution_progress = take_progress_buffer();

    run_target(&target, &options, agent, resolution_progress)
}

fn resolve_run_target(options: &RunOptions) -> Result<RunTarget, CliError> {
    if let Some(git_url) = options.task_git_url.as_deref() {
        let path = options
            .path
            .as_deref()
            .ok_or_else(|| CliError::usage("--task-git-url requires `-p <path-in-repo>`"))?;
        print_progress(options, &format!("resolving git task: {git_url} @ {path}"))?;
        let task_path =
            resolve_git_task_source(git_url, options.task_git_commit.as_deref(), Path::new(path))?;

        return RunTarget::from_path(&task_path, &options.selection);
    }

    if let Some(path) = options.path.as_deref() {
        print_progress(options, &format!("loading local target: {path}"))?;
        return RunTarget::from_path(Path::new(path), &options.selection);
    }

    if let Some(task) = options.task.as_deref() {
        print_progress(options, &format!("resolving registered task: {task}"))?;
        let resolved = match options.registry_path.as_deref() {
            Some(registry_path) => resolve_local_registry_task(task, Path::new(registry_path))?,
            None => resolve_remote_registry_task(task, options.registry_url.as_deref())?,
        };

        return RunTarget::from_registry_dataset(resolved, &options.selection);
    }

    let dataset = options
        .dataset
        .as_deref()
        .ok_or_else(|| CliError::usage("run requires either `-p <path>` or `-d <dataset>`"))?;
    print_progress(options, &format!("resolving dataset: {dataset}"))?;
    let resolved = match options.registry_path.as_deref() {
        Some(registry_path) => resolve_local_registry_dataset(dataset, Path::new(registry_path))?,
        None => resolve_remote_registry_dataset(dataset, options.registry_url.as_deref())?,
    };

    RunTarget::from_registry_dataset(resolved, &options.selection)
}

fn run_target(
    target: &RunTarget,
    options: &RunOptions,
    agent: AgentKind,
    resolution_progress: Vec<ProgressLine>,
) -> Result<(), CliError> {
    let run_started = Instant::now();
    let run_id = timestamp_id()?;
    let job_root = options
        .jobs_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("jobs"));
    let job_dir = job_root.join(format!("seaport-{run_id}"));
    let plans = trial_plans(target, options);
    let concurrency = RunPhase::Execution.concurrency(options.concurrency, plans.len());

    print_target_ready(target, &job_dir, plans.len(), concurrency, options, agent)?;
    print_resolution_progress(options, resolution_progress)?;

    if !target.tasks.is_empty() {
        ensure_sandbox_backend_available(options.backend)?;

        if options.backend == SandboxBackend::Docker {
            cleanup_orphaned_trial_containers();
        }
    }

    let execution_started = Instant::now();
    let outcomes = run_trial_plans(&plans, &job_dir, &run_id, options, agent, concurrency)?;
    logging::log_timing(
        "run",
        "execution",
        "all trials (solution + verifier)",
        execution_started.elapsed(),
    );

    fs::write(
        job_dir.join("config.json"),
        job_config_json(target, options, agent),
    )?;
    let eval_key = match options.model.as_deref() {
        Some(model) => format!("{}__{}__{}", agent.as_str(options), model, target.name),
        None => format!("{}__{}", agent.as_str(options), target.name),
    };
    fs::write(
        job_dir.join("result.json"),
        job_result_json(&outcomes, &eval_key),
    )?;

    let passed = outcomes.iter().all(|outcome| outcome.passed);
    let passed_count = outcomes.iter().filter(|outcome| outcome.passed).count();

    print_run_summary(RunSummary {
        passed,
        passed_count,
        total_count: outcomes.len(),
        reward: aggregate_reward(&outcomes),
        job_dir: &job_dir,
        total_elapsed: run_started.elapsed(),
        options,
    })?;

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

fn print_run_start(options: &RunOptions, agent: AgentKind) -> Result<(), CliError> {
    if options.log_mode == LogMode::Quiet {
        return Ok(());
    }

    let label = run_start_label(options, agent);

    if push_progress_line(ProgressLine::Banner(label.clone())) {
        return Ok(());
    }

    println!("{}", bold(&label));
    io::stdout().flush()?;

    Ok(())
}

fn run_start_label(options: &RunOptions, agent: AgentKind) -> String {
    format!(
        "{} {} · {} · {}",
        "Seaport",
        run_source_label(options),
        agent.as_str(options),
        options.backend.as_str()
    )
}

fn run_source_label(options: &RunOptions) -> String {
    if let Some(git_url) = options.task_git_url.as_deref() {
        format!("git {git_url}")
    } else if let Some(dataset) = options.dataset.as_deref() {
        format!("dataset {dataset}")
    } else if let Some(task) = options.task.as_deref() {
        format!("task {task}")
    } else if let Some(path) = options.path.as_deref() {
        format!("path {path}")
    } else {
        "unknown".to_owned()
    }
}

fn print_target_ready(
    target: &RunTarget,
    job_dir: &Path,
    trials: usize,
    concurrency: usize,
    options: &RunOptions,
    agent: AgentKind,
) -> Result<(), CliError> {
    if options.log_mode == LogMode::Quiet {
        return Ok(());
    }

    println!();
    print_run_box(RunBox {
        title: "Seaport",
        target: &target.name,
        agent: agent.as_str(options),
        backend: options.backend.as_str(),
        tasks: target.tasks.len(),
        trials,
        concurrency,
        job_dir,
    });
    io::stdout().flush()?;

    Ok(())
}

fn print_progress(options: &RunOptions, message: &str) -> Result<(), CliError> {
    if options.log_mode == LogMode::Quiet {
        return Ok(());
    }

    if push_progress_line(ProgressLine::Step(message.to_owned())) {
        return Ok(());
    }

    println!("  {} {message}", blue("->"));
    io::stdout().flush()?;

    Ok(())
}

fn print_resolution_progress(
    options: &RunOptions,
    lines: Vec<ProgressLine>,
) -> Result<(), CliError> {
    if options.log_mode == LogMode::Quiet || lines.is_empty() {
        return Ok(());
    }

    println!();
    println!("{}", bold("Progress"));

    for line in lines {
        match line {
            ProgressLine::Banner(message) => println!("  {}", bold(&message)),
            ProgressLine::Step(message) => println!("  {} {message}", blue("->")),
        }
    }

    io::stdout().flush()?;

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunPhase {
    Execution,
}

impl RunPhase {
    fn title(self) -> &'static str {
        "Execution"
    }

    fn label(self) -> &'static str {
        "build/solution/verifier"
    }

    fn concurrency(self, requested: usize, item_count: usize) -> usize {
        let _ = self;
        requested.max(1).min(item_count.max(1))
    }
}

#[derive(Clone, Copy)]
struct TrialPlan<'a> {
    task: &'a TaskRef,
    attempt: usize,
}

struct TrialEvent {
    index: usize,
    result: Result<TrialOutcome, CliError>,
}

fn trial_plans<'a>(target: &'a RunTarget, options: &RunOptions) -> Vec<TrialPlan<'a>> {
    let mut plans = Vec::with_capacity(target.tasks.len() * options.attempts);

    for task in &target.tasks {
        for attempt in 1..=options.attempts {
            plans.push(TrialPlan { task, attempt });
        }
    }

    plans
}

fn run_trial_plans(
    plans: &[TrialPlan<'_>],
    job_dir: &Path,
    run_id: &str,
    options: &RunOptions,
    agent: AgentKind,
    concurrency: usize,
) -> Result<Vec<TrialOutcome>, CliError> {
    let phase = RunPhase::Execution;

    print_phase_header(phase, plans.len(), concurrency, options)?;
    print_phase_progress(phase, 0, plans.len(), options)?;

    let work = Mutex::new(scheduled_trial_indices(plans));
    let (sender, receiver) = mpsc::channel();
    let mut outcomes = (0..plans.len()).map(|_| None).collect::<Vec<_>>();
    let mut completed = 0;
    // Attribute to each finishing trial the wall-clock interval since the
    // previous trial finished (the first since the phase began). With trials
    // running concurrently these intervals tile the execution timeline, so the
    // per-task durations sum to the execution wall-clock instead of each
    // measuring from a shared start (which made every row look additive).
    let phase_started = Instant::now();
    let mut last_finished = phase_started;

    thread::scope(|scope| -> Result<(), CliError> {
        for _ in 0..concurrency {
            let sender = sender.clone();
            let work = &work;

            scope.spawn(move || loop {
                let index = {
                    let mut work = work.lock().expect("trial queue");
                    work.pop_front()
                };
                let Some(index) = index else {
                    break;
                };
                let plan = plans[index];

                let result = run_trial(
                    plan.task,
                    plan.attempt,
                    job_dir,
                    run_id,
                    options,
                    agent,
                    concurrency,
                );

                if sender.send(TrialEvent { index, result }).is_err() {
                    break;
                }
            });
        }

        drop(sender);

        for TrialEvent { index, result } in receiver {
            let mut outcome = result?;
            let now = Instant::now();
            outcome.elapsed = now.duration_since(last_finished);
            last_finished = now;

            completed += 1;
            print_trial_finish(&outcome, options)?;
            print_phase_progress(phase, completed, plans.len(), options)?;

            outcomes[index] = Some(outcome);
        }

        Ok(())
    })?;

    outcomes
        .into_iter()
        .map(|outcome| outcome.ok_or_else(|| CliError::task_failed("trial worker stopped early")))
        .collect()
}

fn scheduled_trial_indices(plans: &[TrialPlan<'_>]) -> VecDeque<usize> {
    let weighted = plans
        .iter()
        .enumerate()
        .map(|(index, plan)| (index, task_schedule_weight(plan.task)))
        .collect::<Vec<_>>();

    weighted_indices(weighted)
}

fn weighted_indices(mut weighted: Vec<(usize, u32)>) -> VecDeque<usize> {
    weighted.sort_by(|(left_index, left_weight), (right_index, right_weight)| {
        right_weight
            .cmp(left_weight)
            .then_with(|| left_index.cmp(right_index))
    });

    weighted
        .into_iter()
        .map(|(index, _)| index)
        .collect::<VecDeque<_>>()
}

fn task_schedule_weight(task: &TaskRef) -> u32 {
    let mut weight = 100;

    if file_contains_any(
        &task.path.join("environment").join("Dockerfile"),
        &["zulu7-jdk", "openjdk-7-jdk", "openjdk-7-jre"],
    ) {
        weight += 1_000;
    }

    if file_contains_any(
        &task.path.join("solution").join("solve.sh"),
        &["cargo build --release", "mvn test", "gradle test", "javac"],
    ) {
        weight += 700;
    }

    if directory_contains_extension(&task.path.join("environment"), &["s", "asm"]) {
        weight += 250;
    }

    weight
}

fn file_contains_any(path: &Path, needles: &[&str]) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    let contents = contents.to_ascii_lowercase();

    needles.iter().any(|needle| contents.contains(needle))
}

fn directory_contains_extension(path: &Path, extensions: &[&str]) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };

    for entry in entries.flatten() {
        let entry_path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() && directory_contains_extension(&entry_path, extensions) {
            return true;
        }

        if file_type.is_file()
            && entry_path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extensions
                        .iter()
                        .any(|expected| extension.eq_ignore_ascii_case(expected))
                })
        {
            return true;
        }
    }

    false
}

/// Runs a trial with whole-trial retries. An errored trial (build/agent/
/// verifier infra failure) is retried up to `max_retries` times with a short
/// backoff, unless its error is non-retryable (timeouts and reward-file
/// problems, matching harbor's default exclude set) or filtered out by
/// `--retry-include`/`--retry-exclude`. Mirrors harbor's per-trial retry loop.
fn run_trial(
    task: &TaskRef,
    attempt: usize,
    job_dir: &Path,
    run_id: &str,
    options: &RunOptions,
    agent: AgentKind,
    concurrency: usize,
) -> Result<TrialOutcome, CliError> {
    let trial_name = trial_dir_name(&task.name, attempt, options.attempts);
    let trial_dir = job_dir.join(&trial_name);

    let mut retries = 0;
    loop {
        let mut outcome =
            run_trial_once(task, attempt, job_dir, run_id, options, agent, concurrency)?;

        let exhausted = retries >= options.max_retries;
        let retryable = outcome.errored
            && should_retry_error(
                outcome.error.as_deref(),
                &options.retry_include,
                &options.retry_exclude,
            );

        if !retryable || exhausted {
            outcome.retries = retries;
            return Ok(outcome);
        }

        // Discard the failed attempt's output and retry after a backoff, as
        // harbor does (rmtree the trial dir, then re-run).
        let _ = fs::remove_dir_all(&trial_dir);
        let delay = retry_backoff_delay(retries);
        if options.log_mode != LogMode::Quiet {
            eprintln!(
                "seaport: retrying {} (attempt {}/{}) in {}: {}",
                task.name,
                retries + 1,
                options.max_retries,
                format_duration(delay),
                outcome
                    .error
                    .as_deref()
                    .map(first_error_line)
                    .unwrap_or("trial failed")
            );
        }
        thread::sleep(delay);
        retries += 1;
    }
}

/// Whether an errored trial should be retried: non-retryable error substrings
/// (`--retry-exclude`, defaulting to timeouts and reward-file problems) win;
/// otherwise, if `--retry-include` is set the error must match one of them.
/// Seaport matches error message substrings where harbor matches exception
/// types.
fn should_retry_error(error: Option<&str>, include: &[String], exclude: &[String]) -> bool {
    let message = error.unwrap_or("");

    if exclude
        .iter()
        .any(|pattern| message.contains(pattern.as_str()))
    {
        return false;
    }

    if !include.is_empty()
        && !include
            .iter()
            .any(|pattern| message.contains(pattern.as_str()))
    {
        return false;
    }

    true
}

/// Backoff before a retry: `min_wait * multiplier^attempt`, capped, matching
/// harbor's defaults (1s, constant).
fn retry_backoff_delay(attempt: usize) -> Duration {
    let seconds =
        (RETRY_MIN_WAIT_SECS * RETRY_WAIT_MULTIPLIER.powi(attempt as i32)).min(RETRY_MAX_WAIT_SECS);
    Duration::from_secs_f64(seconds)
}

fn run_trial_once(
    task: &TaskRef,
    attempt: usize,
    job_dir: &Path,
    run_id: &str,
    options: &RunOptions,
    agent: AgentKind,
    concurrency: usize,
) -> Result<TrialOutcome, CliError> {
    let trial_started = Instant::now();
    let task_name = &task.name;
    let trial_name = trial_dir_name(task_name, attempt, options.attempts);
    let trial_run_id = format!("{run_id}-{trial_name}");
    let trial_dir = job_dir.join(&trial_name);
    let agent_dir = trial_dir.join("agent");
    let verifier_dir = trial_dir.join("verifier");
    // Created lazily by the backend only when artifacts are actually collected.
    let artifacts_dir = trial_dir.join("artifacts");
    // Per-step output root for multi-step tasks; per-step artifacts are written
    // here by the sandbox, per-step agent/verifier logs by `record_multi_step_trial`.
    let steps_dir = trial_dir.join("steps");
    let workspace = env::temp_dir().join(format!(
        "seaport-{}-{run_id}-{}",
        agent.as_str(options),
        trial_name
    ));
    let app_dir = workspace.join("app");
    let logs_dir = workspace.join("logs").join("verifier");

    fs::create_dir_all(&agent_dir)?;
    fs::create_dir_all(&verifier_dir)?;
    fs::create_dir_all(&app_dir)?;
    fs::create_dir_all(&logs_dir)?;

    print_trial_start(task_name, attempt, options, agent)?;

    prepare_container_writable_dir(&app_dir)?;
    prepare_container_writable_dir(&workspace.join("logs"))?;
    prepare_container_writable_dir(&logs_dir)?;

    let sandbox_agent = agent.sandbox_agent(options)?;
    let phase_envs = options.phase_envs();
    let execution: Result<TrialScripts, CliError> = run_task_scripts(TaskScriptRequest {
        task_label: task_name,
        task_path: &task.path,
        run_id: &trial_run_id,
        app_dir: &app_dir,
        logs_dir: &logs_dir,
        artifacts_dir: &artifacts_dir,
        steps_dir: &steps_dir,
        agent: &sandbox_agent,
        agent_setup: options.agent_setup.as_deref(),
        envs: &phase_envs,
        backend: options.backend,
        strict_resources: options.strict_resources,
        concurrency,
        timeout_multipliers: options.timeout_multipliers(),
    });

    // `elapsed` is assigned by the caller as trials finish, so each trial's
    // reported duration is its share of the execution timeline.
    let outcome = match execution {
        Ok(TrialScripts::Single(outputs)) => match read_reward(&logs_dir) {
            Ok(reward) => record_completed_trial(TrialRecord {
                task_name,
                attempt,
                agent,
                options,
                trial_dir: &trial_dir,
                agent_dir: &agent_dir,
                verifier_dir: &verifier_dir,
                outputs,
                reward,
            })?,
            // A verifier that wrote no reward is a per-trial failure (recorded,
            // dataset continues), not a fatal run error.
            Err(error) if error.is_task_failure() => record_failed_trial(TrialFailure {
                task_name,
                attempt,
                agent,
                options,
                trial_dir: &trial_dir,
                agent_dir: &agent_dir,
                verifier_dir: &verifier_dir,
                logs_dir: &logs_dir,
                message: error.to_string(),
            })?,
            Err(error) => return Err(error),
        },
        Ok(TrialScripts::MultiStep { steps, reward }) => {
            record_multi_step_trial(MultiStepRecord {
                task_name,
                attempt,
                agent,
                options,
                trial_dir: &trial_dir,
                steps_dir: &steps_dir,
                steps,
                reward,
            })?
        }
        Err(error) if error.is_task_failure() => record_failed_trial(TrialFailure {
            task_name,
            attempt,
            agent,
            options,
            trial_dir: &trial_dir,
            agent_dir: &agent_dir,
            verifier_dir: &verifier_dir,
            logs_dir: &logs_dir,
            message: error.to_string(),
        })?,
        Err(error) => return Err(error),
    };

    let cleanup_started = Instant::now();
    if let Err(error) = fs::remove_dir_all(&workspace) {
        eprintln!(
            "seaport: warning: could not remove workspace {}: {error}",
            workspace.display()
        );
    }
    logging::log_timing(
        task_name,
        "cleanup",
        "workspace removal",
        cleanup_started.elapsed(),
    );
    logging::log_timing(
        task_name,
        "trial",
        "total trial wall clock",
        trial_started.elapsed(),
    );

    Ok(outcome)
}

fn print_trial_start(
    task_name: &str,
    attempt: usize,
    options: &RunOptions,
    agent: AgentKind,
) -> Result<(), CliError> {
    if options.log_mode != LogMode::Quiet {
        if options.log_mode != LogMode::Verbose {
            return Ok(());
        }

        println!(
            "  {} {}  attempt {attempt}/{}  {}",
            blue("->"),
            fit_text(task_name, TASK_LABEL_WIDTH),
            options.attempts,
            dim(agent.as_str(options))
        );
        io::stdout().flush()?;
    }

    Ok(())
}

fn print_trial_finish(outcome: &TrialOutcome, options: &RunOptions) -> Result<(), CliError> {
    if options.log_mode == LogMode::Quiet {
        return Ok(());
    }

    clear_live_progress_line(options)?;

    let status = if outcome.passed {
        green("✓")
    } else {
        red("!")
    };
    let result = if outcome.passed {
        green("passed")
    } else {
        red("failed")
    };

    println!(
        "  {} {}  {}  reward {}  {}",
        status,
        fit_text(&outcome.task_name, TASK_LABEL_WIDTH),
        result,
        outcome.reward,
        dim(&format_duration(outcome.elapsed))
    );

    if !outcome.passed {
        print_failure_tail(outcome);
    }

    io::stdout().flush()?;

    Ok(())
}

fn first_error_line(error: &str) -> &str {
    error.lines().next().unwrap_or(error)
}

fn print_failure_tail(outcome: &TrialOutcome) {
    if let Some(error) = outcome.error.as_deref() {
        println!("    {} {}", red("error:"), first_error_line(error));
    }

    if !outcome.stderr_tail.is_empty() {
        println!("    {}", red("stderr tail"));
        for line in &outcome.stderr_tail {
            println!("      {}", dim(line));
        }
    } else if !outcome.stdout_tail.is_empty() {
        println!("    {}", blue("stdout tail"));
        for line in &outcome.stdout_tail {
            println!("      {}", dim(line));
        }
    }

    println!(
        "    {} {}/verifier/test-stderr.txt",
        dim("logs:"),
        outcome.trial_dir.display()
    );
}

fn print_phase_header(
    phase: RunPhase,
    tasks: usize,
    concurrency: usize,
    options: &RunOptions,
) -> Result<(), CliError> {
    if options.log_mode == LogMode::Quiet {
        return Ok(());
    }

    println!();
    println!(
        "{} {}  {} tasks  concurrency {}",
        bold(phase.title()),
        dim(phase.label()),
        tasks,
        concurrency
    );
    io::stdout().flush()?;

    Ok(())
}

fn print_phase_progress(
    phase: RunPhase,
    completed: usize,
    total: usize,
    options: &RunOptions,
) -> Result<(), CliError> {
    if options.log_mode == LogMode::Quiet {
        return Ok(());
    }

    let color = match phase {
        RunPhase::Execution => "32",
    };

    let line = format!(
        "{:<13} {}  {:>7}",
        phase.title(),
        progress_bar(completed, total, PROGRESS_BAR_WIDTH, color),
        format!("{completed}/{total}")
    );

    if live_progress_enabled(options) {
        print!("\r\x1b[2K{line}");
        if completed >= total {
            println!();
        }
    } else if completed >= total || options.log_mode == LogMode::Verbose {
        println!("{line}");
    }
    io::stdout().flush()?;

    Ok(())
}

fn clear_live_progress_line(options: &RunOptions) -> Result<(), CliError> {
    if live_progress_enabled(options) {
        print!("\r\x1b[2K");
        io::stdout().flush()?;
    }

    Ok(())
}

fn live_progress_enabled(options: &RunOptions) -> bool {
    options.log_mode == LogMode::Concise && io::stdout().is_terminal()
}

struct RunBox<'a> {
    title: &'a str,
    target: &'a str,
    agent: &'a str,
    backend: &'a str,
    tasks: usize,
    trials: usize,
    concurrency: usize,
    job_dir: &'a Path,
}

fn print_run_box(run: RunBox<'_>) {
    let width = 78;
    let inner = width - 4;
    let title = format!(" {} ", run.title);
    let top = format!("┌{title}{}┐", "─".repeat(inner - title.chars().count() + 2));
    let right = format!("{} · {}        {} tasks", run.agent, run.backend, run.tasks);
    let meta = format!("trials {} · concurrency {}", run.trials, run.concurrency);

    println!("{}", blue(&top));
    println!(
        "{}",
        box_line(&right_aligned_text(run.target, &right, inner), inner)
    );
    println!("{}", box_line(&run.job_dir.display().to_string(), inner));
    println!("{}", box_line(&meta, inner));
    println!("{}", blue(&format!("└{}┘", "─".repeat(inner + 2))));
}

fn box_line(content: &str, width: usize) -> String {
    format!("│ {} │", fit_text(content, width))
}

fn right_aligned_text(left: &str, right: &str, width: usize) -> String {
    let right_len = right.chars().count();
    let min_gap = 2;

    if right_len + min_gap >= width {
        return fit_text(left, width);
    }

    let left_width = width - right_len - min_gap;
    format!(
        "{}{}{}",
        fit_text(left, left_width),
        " ".repeat(min_gap),
        right
    )
}

struct RunSummary<'a> {
    passed: bool,
    passed_count: usize,
    total_count: usize,
    reward: String,
    job_dir: &'a Path,
    total_elapsed: Duration,
    options: &'a RunOptions,
}

fn print_run_summary(summary: RunSummary<'_>) -> Result<(), CliError> {
    if summary.options.log_mode == LogMode::Quiet {
        println!(
            "{} {}/{} reward {} total {}",
            if summary.passed { "passed" } else { "failed" },
            summary.passed_count,
            summary.total_count,
            summary.reward,
            format_duration(summary.total_elapsed)
        );
        return Ok(());
    }

    let status = if summary.passed {
        green("✓")
    } else {
        red("!")
    };
    let label = if summary.passed {
        green("passed")
    } else {
        red("failed")
    };

    println!();
    println!("{}", bold("Summary"));
    println!(
        "  {} {} {}/{}       reward {}       total time {}",
        status,
        label,
        summary.passed_count,
        summary.total_count,
        green(&summary.reward),
        bold(&format_duration(summary.total_elapsed))
    );
    println!("  {} {}", dim("job_dir:"), summary.job_dir.display());
    io::stdout().flush()?;

    Ok(())
}

fn progress_bar(completed: usize, total: usize, width: usize, color: &str) -> String {
    let total = total.max(1);
    let completed = completed.min(total);
    let filled = width * completed / total;
    let empty = width.saturating_sub(filled);

    format!(
        "{}{}",
        ansi(color, &"█".repeat(filled)),
        dim(&"░".repeat(empty))
    )
}

fn tail_lines_bytes(bytes: &[u8], limit: usize) -> Vec<String> {
    tail_lines_text(&String::from_utf8_lossy(bytes), limit)
}

fn tail_lines_text(text: &str, limit: usize) -> Vec<String> {
    let lines = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let start = lines.len().saturating_sub(limit);

    lines.into_iter().skip(start).collect()
}

fn failure_output_tail(message: &str, stream: &str, limit: usize) -> Option<Vec<String>> {
    let section = failure_output_section(message, stream)?;
    let tail = tail_lines_text(section, limit);

    if tail.is_empty() {
        None
    } else {
        Some(tail)
    }
}

fn failure_output_section<'a>(message: &'a str, stream: &str) -> Option<&'a str> {
    let marker = format!("\n{stream}:\n");
    let start = message.find(&marker)? + marker.len();
    let section = &message[start..];

    if stream == "stdout" {
        if let Some(end) = section.find("\nstderr:\n") {
            return Some(&section[..end]);
        }
    }

    Some(section)
}

fn fit_text(value: &str, width: usize) -> String {
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

fn bold(text: &str) -> String {
    ansi("1", text)
}

fn dim(text: &str) -> String {
    ansi("2", text)
}

fn green(text: &str) -> String {
    ansi("32", text)
}

fn red(text: &str) -> String {
    ansi("31", text)
}

fn blue(text: &str) -> String {
    ansi("34", text)
}

fn ansi(code: &str, text: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

fn color_enabled() -> bool {
    io::stdout().is_terminal()
        && env::var_os("NO_COLOR").is_none()
        && env::var_os("SEAPORT_NO_COLOR").is_none()
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let millis = duration.subsec_millis();

    if seconds >= 60 {
        format!("{}m {:02}.{:03}s", seconds / 60, seconds % 60, millis)
    } else {
        format!("{seconds}.{millis:03}s")
    }
}

fn trial_dir_name(task_name: &str, attempt: usize, attempts: usize) -> String {
    let base = sanitize_name(task_name);

    if attempts == 1 {
        base
    } else {
        format!("{base}-attempt-{attempt}")
    }
}

fn validate_task_path(task_path: &Path) -> Result<(), CliError> {
    if !task_path.is_dir() {
        return Err(CliError::usage(format!(
            "task path is not a directory: {}",
            task_path.display()
        )));
    }

    let task_toml = task_path.join("task.toml");
    if !task_toml.is_file() {
        return Err(CliError::usage(format!(
            "task is missing required file: {}",
            task_toml.display()
        )));
    }

    // A multi-step task ([[steps]]) carries its instruction and verifier per
    // step under steps/<name>/, so the top-level instruction.md / tests/test.sh
    // are not required; a single-step task requires both.
    if task_is_multi_step(&task_toml) {
        let steps_dir = task_path.join("steps");
        if !steps_dir.is_dir() {
            return Err(CliError::usage(format!(
                "multi-step task is missing its steps directory: {}",
                steps_dir.display()
            )));
        }
        return Ok(());
    }

    for relative in ["instruction.md", "tests/test.sh"] {
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

/// Whether `task.toml` declares `[[steps]]` (a multi-step task). Best-effort: a
/// malformed manifest is treated as single-step here; `task_environment`
/// surfaces the parse error authoritatively.
fn task_is_multi_step(task_toml: &Path) -> bool {
    fs::read_to_string(task_toml)
        .ok()
        .and_then(|contents| toml_doc::parse(&contents).ok())
        .and_then(|doc| {
            doc.get("steps")
                .and_then(|steps| steps.as_array())
                .map(|steps| !steps.is_empty())
        })
        .unwrap_or(false)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentKind {
    Oracle,
    Nop,
    External,
}

impl AgentKind {
    fn parse(value: &str) -> Result<Self, CliError> {
        match value {
            "oracle" => Ok(Self::Oracle),
            "nop" => Ok(Self::Nop),
            _ => Ok(Self::External),
        }
    }

    fn as_str(self, options: &RunOptions) -> &str {
        match self {
            Self::Oracle => "oracle",
            Self::Nop => "nop",
            Self::External => options.agent.as_deref().unwrap_or("agent"),
        }
    }

    fn requires_model(self) -> bool {
        matches!(self, Self::External)
    }

    fn sandbox_agent(self, options: &RunOptions) -> Result<SandboxAgent, CliError> {
        match self {
            Self::Oracle => Ok(SandboxAgent::Oracle),
            Self::Nop => Ok(SandboxAgent::Nop),
            Self::External => {
                let name = options.agent.as_deref().unwrap_or("agent");
                let command = match options.agent_command.as_deref() {
                    Some(command) => command.to_owned(),
                    None => default_agent_command(name, options.model.as_deref())?,
                };

                Ok(SandboxAgent::External(ExternalAgent {
                    name: name.to_owned(),
                    command,
                    model: options.model.clone(),
                }))
            }
        }
    }
}

fn default_agent_command(agent: &str, model: Option<&str>) -> Result<String, CliError> {
    match agent {
        "codex" => {
            let model = model
                .map(|model| format!(" --model {}", shell_quote(model)))
                .unwrap_or_default();

            Ok(format!(
                "codex exec --dangerously-bypass-approvals-and-sandbox --cd /app{model} \"$(cat \\\"$SEAPORT_INSTRUCTION_PATH\\\")\""
            ))
        }
        "claude-code" | "claude" => {
            let model = model
                .map(|model| format!(" --model {}", shell_quote(model)))
                .unwrap_or_default();

            Ok(format!(
                "claude --print --dangerously-skip-permissions{model} \"$(cat \\\"$SEAPORT_INSTRUCTION_PATH\\\")\""
            ))
        }
        unsupported => Err(CliError::unimplemented(format!(
            "agent `{unsupported}` requires `--agent-command <shell-command>` until a native adapter is implemented"
        ))),
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

struct TrialOutcome {
    task_name: String,
    attempt: usize,
    reward: String,
    rewards: Rewards,
    passed: bool,
    /// Number of whole-trial retries before this outcome was produced.
    retries: usize,
    /// True only for trials that errored before/while producing a reward
    /// (build/pull/agent/verifier failure, timeout, or no reward written).
    /// Harbor excludes these from reward stats and counts them as errors.
    errored: bool,
    error: Option<String>,
    stdout_tail: Vec<String>,
    stderr_tail: Vec<String>,
    trial_dir: PathBuf,
    elapsed: Duration,
}

struct TrialRecord<'a> {
    task_name: &'a str,
    attempt: usize,
    agent: AgentKind,
    options: &'a RunOptions,
    trial_dir: &'a Path,
    agent_dir: &'a Path,
    verifier_dir: &'a Path,
    outputs: ScriptOutputs,
    reward: Rewards,
}

struct TrialFailure<'a> {
    task_name: &'a str,
    attempt: usize,
    agent: AgentKind,
    options: &'a RunOptions,
    trial_dir: &'a Path,
    agent_dir: &'a Path,
    verifier_dir: &'a Path,
    logs_dir: &'a Path,
    message: String,
}

fn record_completed_trial(record: TrialRecord<'_>) -> Result<TrialOutcome, CliError> {
    let rewards = record.reward;
    let reward = rewards.display();
    let passed = rewards.passed();
    let stdout_tail = tail_lines_bytes(&record.outputs.verifier.stdout, FAILURE_TAIL_LINES);
    let stderr_tail = tail_lines_bytes(&record.outputs.verifier.stderr, FAILURE_TAIL_LINES);
    let error = if passed {
        None
    } else {
        Some(format!("verifier returned reward {reward}"))
    };

    fs::write(
        record.agent_dir.join("trajectory.json"),
        trajectory_json(&record.outputs.agent),
    )?;
    fs::write(
        record.verifier_dir.join("test-stdout.txt"),
        &record.outputs.verifier.stdout,
    )?;
    fs::write(
        record.verifier_dir.join("test-stderr.txt"),
        &record.outputs.verifier.stderr,
    )?;
    fs::write(record.verifier_dir.join("reward.txt"), &reward)?;
    write_trial_metadata(TrialMetadata {
        trial_dir: record.trial_dir,
        task_name: record.task_name,
        attempt: record.attempt,
        agent: record.agent,
        options: record.options,
        passed,
        rewards: &rewards,
        error: error.as_deref(),
    })?;

    Ok(TrialOutcome {
        task_name: record.task_name.to_owned(),
        attempt: record.attempt,
        reward,
        rewards,
        passed,
        retries: 0,
        errored: false,
        error,
        stdout_tail,
        stderr_tail,
        trial_dir: record.trial_dir.to_path_buf(),
        elapsed: Duration::ZERO,
    })
}

struct MultiStepRecord<'a> {
    task_name: &'a str,
    attempt: usize,
    agent: AgentKind,
    options: &'a RunOptions,
    trial_dir: &'a Path,
    steps_dir: &'a Path,
    steps: Vec<StepOutcome>,
    reward: Rewards,
}

/// Records a completed multi-step trial: per-step agent/verifier logs under
/// `<trial_dir>/steps/<name>/`, plus a trial-level `config.json`/`result.json`
/// whose `result.json` carries the aggregated reward and a `steps` array.
fn record_multi_step_trial(record: MultiStepRecord<'_>) -> Result<TrialOutcome, CliError> {
    let rewards = record.reward;
    let reward = rewards.display();
    let passed = rewards.passed();

    for step in &record.steps {
        let step_dir = record.steps_dir.join(&step.name);
        let step_agent_dir = step_dir.join("agent");
        let step_verifier_dir = step_dir.join("verifier");
        fs::create_dir_all(&step_agent_dir)?;
        fs::create_dir_all(&step_verifier_dir)?;

        fs::write(
            step_agent_dir.join("trajectory.json"),
            trajectory_json(&step.agent),
        )?;
        fs::write(
            step_verifier_dir.join("test-stdout.txt"),
            &step.verifier.stdout,
        )?;
        fs::write(
            step_verifier_dir.join("test-stderr.txt"),
            &step.verifier.stderr,
        )?;
        fs::write(step_verifier_dir.join("reward.txt"), step.rewards.display())?;
        fs::write(
            step_verifier_dir.join("reward.json"),
            step.rewards.to_json(),
        )?;
    }

    fs::write(
        record.trial_dir.join("config.json"),
        trial_config_json(
            record.task_name,
            record.attempt,
            record.agent,
            record.options,
        ),
    )?;
    fs::write(
        record.trial_dir.join("result.json"),
        multi_step_result_json(passed, &reward, &rewards, &record.steps),
    )?;

    // The trial's surfaced tail comes from the last step's verifier, the step
    // that decided the final/last reward.
    let (stdout_tail, stderr_tail) = record
        .steps
        .last()
        .map(|step| {
            (
                tail_lines_bytes(&step.verifier.stdout, FAILURE_TAIL_LINES),
                tail_lines_bytes(&step.verifier.stderr, FAILURE_TAIL_LINES),
            )
        })
        .unwrap_or_default();

    let error = if passed {
        None
    } else {
        Some(format!("multi-step reward {reward}"))
    };

    Ok(TrialOutcome {
        task_name: record.task_name.to_owned(),
        attempt: record.attempt,
        reward,
        rewards,
        passed,
        retries: 0,
        errored: false,
        error,
        stdout_tail,
        stderr_tail,
        trial_dir: record.trial_dir.to_path_buf(),
        elapsed: Duration::ZERO,
    })
}

/// Renders a multi-step trial's `result.json`: the aggregated pass/reward/rewards
/// plus a per-step breakdown.
fn multi_step_result_json(
    passed: bool,
    reward: &str,
    rewards: &Rewards,
    steps: &[StepOutcome],
) -> String {
    let steps_json = steps
        .iter()
        .map(|step| {
            format!(
                "{{\"name\":\"{}\",\"passed\":{},\"reward\":\"{}\",\"rewards\":{}}}",
                json_escape(&step.name),
                step.rewards.passed(),
                json_escape(&step.rewards.display()),
                step.rewards.to_json()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "{{\n  \"passed\": {},\n  \"reward\": \"{}\",\n  \"rewards\": {},\n  \"steps\": [{}]\n}}\n",
        passed,
        json_escape(reward),
        rewards.to_json(),
        steps_json
    )
}

fn record_failed_trial(failure: TrialFailure<'_>) -> Result<TrialOutcome, CliError> {
    let rewards = Rewards::single(0.0);
    let reward = rewards.display();
    let failed_agent = AgentStep {
        command: "execution failed".to_owned(),
        status: 1,
        stdout: Vec::new(),
        stderr: failure.message.as_bytes().to_vec(),
    };

    fs::write(
        failure.agent_dir.join("trajectory.json"),
        trajectory_json(&failed_agent),
    )?;
    fs::write(failure.verifier_dir.join("test-stdout.txt"), [])?;
    fs::write(
        failure.verifier_dir.join("test-stderr.txt"),
        failure.message.as_bytes(),
    )?;
    fs::write(failure.verifier_dir.join("reward.txt"), "0\n")?;
    fs::write(failure.logs_dir.join("reward.txt"), "0\n")?;
    write_trial_metadata(TrialMetadata {
        trial_dir: failure.trial_dir,
        task_name: failure.task_name,
        attempt: failure.attempt,
        agent: failure.agent,
        options: failure.options,
        passed: false,
        rewards: &rewards,
        error: Some(&failure.message),
    })?;

    Ok(TrialOutcome {
        task_name: failure.task_name.to_owned(),
        attempt: failure.attempt,
        reward,
        rewards,
        passed: false,
        retries: 0,
        errored: true,
        stdout_tail: failure_output_tail(&failure.message, "stdout", FAILURE_TAIL_LINES)
            .unwrap_or_default(),
        stderr_tail: failure_output_tail(&failure.message, "stderr", FAILURE_TAIL_LINES)
            .unwrap_or_else(|| tail_lines_text(&failure.message, FAILURE_TAIL_LINES)),
        trial_dir: failure.trial_dir.to_path_buf(),
        elapsed: Duration::ZERO,
        error: Some(failure.message),
    })
}

struct TrialMetadata<'a> {
    trial_dir: &'a Path,
    task_name: &'a str,
    attempt: usize,
    agent: AgentKind,
    options: &'a RunOptions,
    passed: bool,
    rewards: &'a Rewards,
    error: Option<&'a str>,
}

fn write_trial_metadata(metadata: TrialMetadata<'_>) -> Result<(), CliError> {
    fs::write(
        metadata.trial_dir.join("config.json"),
        trial_config_json(
            metadata.task_name,
            metadata.attempt,
            metadata.agent,
            metadata.options,
        ),
    )?;
    fs::write(
        metadata.trial_dir.join("result.json"),
        trial_result_json(metadata.passed, metadata.rewards, metadata.error),
    )?;

    Ok(())
}

/// Reads the verifier reward, preferring `reward.json` over `reward.txt`
/// (matching harbor). `reward.json` may be a bare number or an object with a
/// numeric `reward` field; `reward.txt` is parsed as a single float.
/// A verifier reward: a set of named scores in a stable order. The `reward`
/// key is the 1-D convention used by single-step tasks; multi-key rewards (for
/// example `{core_pass_rate, strict_pass_rate}`) are preserved whole for output
/// fidelity and per-key gating.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Rewards {
    entries: Vec<(String, f64)>,
}

impl Rewards {
    /// The 1-D reward: a single `reward` score.
    pub(crate) fn single(value: f64) -> Self {
        Self {
            entries: vec![("reward".to_owned(), value)],
        }
    }

    /// Builds a reward map from explicit `(key, value)` entries, used by
    /// multi-step aggregation.
    pub(crate) fn from_entries(entries: Vec<(String, f64)>) -> Self {
        Self { entries }
    }

    /// The named scores in stable order, used by multi-step aggregation.
    pub(crate) fn entries(&self) -> &[(String, f64)] {
        &self.entries
    }

    pub(crate) fn get(&self, key: &str) -> Option<f64> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| *value)
    }

    /// The scalar reward used for display and cross-trial aggregation: the
    /// `reward` key when present (the 1-D convention), otherwise the mean of all
    /// named scores.
    pub(crate) fn primary(&self) -> f64 {
        if let Some(value) = self.get("reward") {
            return value;
        }

        if self.entries.is_empty() {
            return 0.0;
        }

        self.entries.iter().map(|(_, value)| value).sum::<f64>() / self.entries.len() as f64
    }

    /// A reward passes only at full credit: the `reward` key equals 1.0, or, for
    /// a multi-key reward with no `reward` key, every named score equals 1.0.
    pub(crate) fn passed(&self) -> bool {
        if let Some(value) = self.get("reward") {
            return reward_is_full(value);
        }

        !self.entries.is_empty() && self.entries.iter().all(|(_, value)| reward_is_full(*value))
    }

    /// The reward rendered for display/storage (the primary scalar).
    pub(crate) fn display(&self) -> String {
        format_reward(self.primary())
    }

    /// The full reward map as a JSON object, for trial result output.
    pub(crate) fn to_json(&self) -> String {
        let body = self
            .entries
            .iter()
            .map(|(key, value)| format!("\"{}\":{}", json_escape(key), format_reward(*value)))
            .collect::<Vec<_>>()
            .join(",");

        format!("{{{body}}}")
    }
}

pub(crate) fn read_reward(logs_dir: &Path) -> Result<Rewards, CliError> {
    let json_path = logs_dir.join("reward.json");
    let text_path = logs_dir.join("reward.txt");

    if json_path.is_file() {
        let contents = fs::read_to_string(&json_path)?;
        return parse_reward_json(&contents).ok_or_else(|| {
            CliError::task_failed(format!(
                "verifier wrote an unparseable reward to {}",
                json_path.display()
            ))
        });
    }

    if text_path.is_file() {
        let contents = fs::read_to_string(&text_path)?;
        let value = contents.trim().parse::<f64>().map_err(|_| {
            CliError::task_failed(format!(
                "verifier wrote an unparseable reward to {}",
                text_path.display()
            ))
        })?;

        return Ok(Rewards::single(value));
    }

    Err(CliError::task_failed(format!(
        "verifier did not write {}",
        text_path.display()
    )))
}

/// Parses `reward.json`: either a bare number (the 1-D convention) or an object
/// of named numeric scores. Returns `None` when there are no numeric rewards.
fn parse_reward_json(contents: &str) -> Option<Rewards> {
    let value: serde_json::Value = serde_json::from_str(contents).ok()?;

    match value {
        serde_json::Value::Number(number) => number.as_f64().map(Rewards::single),
        serde_json::Value::Object(map) => {
            let entries = map
                .iter()
                .filter_map(|(key, value)| value.as_f64().map(|value| (key.clone(), value)))
                .collect::<Vec<_>>();

            if entries.is_empty() {
                None
            } else {
                Some(Rewards { entries })
            }
        }
        _ => None,
    }
}

/// Renders a reward for display/storage: integral values stay integer-shaped
/// ("0", "1") while fractional values keep their decimal form ("0.5").
fn format_reward(reward: f64) -> String {
    if reward.fract() == 0.0 && reward.is_finite() {
        format!("{}", reward as i64)
    } else {
        let mut formatted = format!("{reward:.6}");
        while formatted.ends_with('0') {
            formatted.pop();
        }
        if formatted.ends_with('.') {
            formatted.pop();
        }
        formatted
    }
}

/// Whether a single score is full credit (1.0), using an epsilon comparison to
/// tolerate float representation noise.
fn reward_is_full(reward: f64) -> bool {
    (reward - 1.0).abs() < f64::EPSILON
}

fn task_name(task_path: &Path) -> Result<String, CliError> {
    let task_toml = fs::read_to_string(task_path.join("task.toml"))?;

    // Best-effort: a malformed task.toml falls back to the directory name here;
    // the authoritative parse in `task_environment` surfaces the parse error.
    if let Some(name) = toml_doc::parse(&task_toml)
        .ok()
        .and_then(|doc| toml_doc::section_value(&doc, "task", "name"))
    {
        return Ok(name);
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

fn job_config_json(target: &RunTarget, options: &RunOptions, agent: AgentKind) -> String {
    format!(
        "{{\n  \"agent\": \"{}\",\n  \"agent_command\": {},\n  \"attempts\": {},\n  \"concurrency\": {},\n  \"backend\": \"{}\",\n  \"model\": {},\n  \"target\": \"{}\",\n  \"tasks\": {}\n}}\n",
        agent.as_str(options),
        json_option(options.agent_command.as_deref()),
        options.attempts,
        options.concurrency,
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

fn job_result_json(outcomes: &[TrialOutcome], eval_key: &str) -> String {
    let passed_count = outcomes.iter().filter(|outcome| outcome.passed).count();
    let reward = aggregate_reward(outcomes);

    format!(
        "{{\n  \"passed\": {},\n  \"reward\": \"{}\",\n  \"tasks_total\": {},\n  \"tasks_passed\": {},\n  \"tasks_failed\": {},\n  \"tasks\": {},\n  \"stats\": {}\n}}\n",
        passed_count == outcomes.len(),
        json_escape(&reward),
        outcomes.len(),
        passed_count,
        outcomes.len() - passed_count,
        trial_outcomes_json(outcomes),
        job_stats_json(outcomes, eval_key)
    )
}

/// Harbor-compatible per-eval statistics: for the eval (agent[/model]/dataset),
/// `reward_stats` groups trials by each reward key's value, `metrics` is the
/// per-key mean (or `{"mean": x}` for a 1-D reward), and `pass_at_k` is the
/// unbiased estimate (only for single-key binary rewards). Errored trials
/// (no reward) are excluded from reward stats and counted in `exception_stats`.
fn job_stats_json(outcomes: &[TrialOutcome], eval_key: &str) -> String {
    let completed = outcomes.len();
    let errored = outcomes.iter().filter(|outcome| outcome.errored).count();
    let retries: usize = outcomes.iter().map(|outcome| outcome.retries).sum();

    format!(
        "{{\n    \"n_completed_trials\": {completed},\n    \"n_errored_trials\": {errored},\n    \"n_running_trials\": 0,\n    \"n_pending_trials\": 0,\n    \"n_cancelled_trials\": 0,\n    \"n_retries\": {retries},\n    \"evals\": {{\"{}\": {}}}\n  }}",
        json_escape(eval_key),
        eval_stats_json(outcomes)
    )
}

/// The stats for one eval: see `job_stats_json`.
fn eval_stats_json(outcomes: &[TrialOutcome]) -> String {
    // Harbor counts only trials that produced a reward (non-errored) in n_trials
    // and reward_stats; errored trials go to exception_stats.
    let scored: Vec<&TrialOutcome> = outcomes.iter().filter(|outcome| !outcome.errored).collect();
    let n_trials = scored.len();
    let n_errors = outcomes.len() - n_trials;

    format!(
        "{{\"n_trials\": {n_trials}, \"n_errors\": {n_errors}, \"metrics\": {}, \"pass_at_k\": {}, \"reward_stats\": {}, \"exception_stats\": {}}}",
        reward_metrics_json(&scored),
        pass_at_k_json(&scored),
        reward_stats_json(&scored),
        exception_stats_json(outcomes)
    )
}

fn trial_identifier(outcome: &TrialOutcome) -> String {
    outcome
        .trial_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| outcome.task_name.clone())
}

/// Renders a float the way harbor's reward_stats keys appear (shortest
/// round-trippable form, e.g. `1.0`, `0.5`, `0.182625`).
fn reward_value_key(value: f64) -> String {
    format!("{value:?}")
}

/// The sorted union of reward keys across the scored trials.
fn reward_key_union(scored: &[&TrialOutcome]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for outcome in scored {
        for (key, _) in outcome.rewards.entries() {
            if !keys.iter().any(|seen| seen == key) {
                keys.push(key.clone());
            }
        }
    }
    keys.sort();
    keys
}

/// `metrics`: a single Mean entry. With <=1 reward key it is `{"mean": x}`
/// (1-D convention); otherwise a per-key mean. Mirrors harbor's
/// `aggregate_reward_dicts`.
fn reward_metrics_json(scored: &[&TrialOutcome]) -> String {
    let keys = reward_key_union(scored);

    let mean = |values: Vec<f64>| -> f64 {
        if values.is_empty() {
            0.0
        } else {
            values.iter().sum::<f64>() / values.len() as f64
        }
    };

    if keys.len() <= 1 {
        let values = scored
            .iter()
            .map(|outcome| {
                outcome
                    .rewards
                    .entries()
                    .first()
                    .map(|(_, v)| *v)
                    .unwrap_or(0.0)
            })
            .collect::<Vec<_>>();
        return format!("[{{\"mean\": {}}}]", json_number(mean(values)));
    }

    let entries = keys
        .iter()
        .map(|key| {
            let values = scored
                .iter()
                .map(|outcome| outcome.rewards.get(key).unwrap_or(0.0))
                .collect::<Vec<_>>();
            format!("\"{}\": {}", json_escape(key), json_number(mean(values)))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{{{entries}}}]")
}

/// `reward_stats`: per reward key, a map from each observed value to the list of
/// trials that got it.
fn reward_stats_json(scored: &[&TrialOutcome]) -> String {
    let keys = reward_key_union(scored);
    if keys.is_empty() {
        return "{}".to_owned();
    }

    let per_key = keys
        .iter()
        .map(|key| {
            // Preserve first-seen value order, grouping trials per value.
            let mut values: Vec<(String, Vec<String>)> = Vec::new();
            for outcome in scored {
                if let Some(value) = outcome.rewards.get(key) {
                    let value_key = reward_value_key(value);
                    let name = trial_identifier(outcome);
                    match values.iter_mut().find(|(seen, _)| *seen == value_key) {
                        Some((_, names)) => names.push(name),
                        None => values.push((value_key, vec![name])),
                    }
                }
            }
            let body = values
                .iter()
                .map(|(value, names)| format!("\"{}\": {}", value, json_string_array(names)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("\"{}\": {{{body}}}", json_escape(key))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{per_key}}}")
}

/// `pass_at_k`: harbor's unbiased estimate, but only when every scored trial has
/// a single binary (0/1) reward; otherwise empty. Empty at one attempt per task
/// (k starts at 2).
fn pass_at_k_json(scored: &[&TrialOutcome]) -> String {
    let mut successes: Vec<(String, Vec<u32>)> = Vec::new();
    for outcome in scored {
        let entries = outcome.rewards.entries();
        if entries.len() != 1 {
            return "{}".to_owned();
        }
        let value = entries[0].1;
        let bit = if value == 0.0 {
            0
        } else if value == 1.0 {
            1
        } else {
            return "{}".to_owned();
        };
        match successes
            .iter_mut()
            .find(|(task, _)| *task == outcome.task_name)
        {
            Some((_, bits)) => bits.push(bit),
            None => successes.push((outcome.task_name.clone(), vec![bit])),
        }
    }

    if successes.is_empty() {
        return "{}".to_owned();
    }

    let min_trials = successes
        .iter()
        .map(|(_, bits)| bits.len())
        .min()
        .unwrap_or(0);
    let ks = eligible_k_values(min_trials);
    if ks.is_empty() {
        return "{}".to_owned();
    }

    let task_count = successes.len() as f64;
    let entries = ks
        .iter()
        .map(|k| {
            let total: f64 = successes
                .iter()
                .map(|(_, bits)| {
                    pass_at_k_for_task(bits.len(), bits.iter().filter(|b| **b == 1).count(), *k)
                })
                .sum();
            format!("\"{k}\": {}", json_number(total / task_count))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{entries}}}")
}

fn eligible_k_values(max_k: usize) -> Vec<usize> {
    let mut ks: Vec<usize> = Vec::new();
    let mut k = 2;
    while k <= max_k {
        ks.push(k);
        k *= 2;
    }
    let mut k = 5;
    while k <= max_k {
        if !ks.contains(&k) {
            ks.push(k);
        }
        k += 5;
    }
    ks.sort_unstable();
    ks
}

fn pass_at_k_for_task(n: usize, c: usize, k: usize) -> f64 {
    if n - c < k {
        return 1.0;
    }
    let mut product = 1.0;
    for i in 0..k {
        product *= (n - c - i) as f64 / (n - i) as f64;
    }
    1.0 - product
}

/// `exception_stats`: errored trials grouped by the first line of their error,
/// the closest analog to harbor's exception-type grouping.
fn exception_stats_json(outcomes: &[TrialOutcome]) -> String {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for outcome in outcomes.iter().filter(|outcome| outcome.errored) {
        let kind = outcome
            .error
            .as_deref()
            .map(|error| first_error_line(error).to_owned())
            .unwrap_or_else(|| "error".to_owned());
        let name = trial_identifier(outcome);
        match groups.iter_mut().find(|(seen, _)| *seen == kind) {
            Some((_, names)) => names.push(name),
            None => groups.push((kind, vec![name])),
        }
    }
    let body = groups
        .iter()
        .map(|(kind, names)| format!("\"{}\": {}", json_escape(kind), json_string_array(names)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{body}}}")
}

fn json_string_array(items: &[String]) -> String {
    let body = items
        .iter()
        .map(|item| format!("\"{}\"", json_escape(item)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{body}]")
}

/// Renders a reward/metric number: integral values stay integer-shaped, others
/// use the shortest round-trippable float form.
fn json_number(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() {
        format!("{}", value as i64)
    } else {
        format!("{value:?}")
    }
}

fn trial_result_json(passed: bool, rewards: &Rewards, error: Option<&str>) -> String {
    let reward = rewards.display();
    let rewards_json = rewards.to_json();

    match error {
        Some(error) => format!(
            "{{\n  \"passed\": {},\n  \"reward\": \"{}\",\n  \"rewards\": {},\n  \"error\": \"{}\"\n}}\n",
            passed,
            json_escape(&reward),
            rewards_json,
            json_escape(error)
        ),
        None => format!(
            "{{\n  \"passed\": {},\n  \"reward\": \"{}\",\n  \"rewards\": {}\n}}\n",
            passed,
            json_escape(&reward),
            rewards_json
        ),
    }
}

fn trial_config_json(
    task_name: &str,
    attempt: usize,
    agent: AgentKind,
    options: &RunOptions,
) -> String {
    format!(
        "{{\n  \"task\": \"{}\",\n  \"attempt\": {},\n  \"agent\": \"{}\"\n}}\n",
        json_escape(task_name),
        attempt,
        agent.as_str(options)
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

    format_reward(total / outcomes.len() as f64)
}

fn trial_outcomes_json(outcomes: &[TrialOutcome]) -> String {
    let items = outcomes
        .iter()
        .map(|outcome| {
            let error = outcome
                .error
                .as_deref()
                .map(|error| format!(",\"error\":\"{}\"", json_escape(error)))
                .unwrap_or_default();

            format!(
                "{{\"task\":\"{}\",\"attempt\":{},\"passed\":{},\"reward\":\"{}\",\"rewards\":{}{}}}",
                json_escape(&outcome.task_name),
                outcome.attempt,
                outcome.passed,
                json_escape(&outcome.reward),
                outcome.rewards.to_json(),
                error
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
  upgrade             Update Seaport to the latest release

Options:
  -h, --help          Show this help
  -v, --version       Show the version

Run `seaport <command> --help` for command-specific help."
    );
}

fn print_version() {
    println!("{VERSION_TEXT}");
}

fn print_run_help() {
    println!(
        "\
Usage:
  seaport run -p <path> [options]
  seaport run -d <dataset> [options]
  seaport run -t <task> [options]
  seaport run --task-git-url <url> -p <path-in-repo> [options]

Options:
  -p, --path <path>       Local task or dataset directory
  -d, --dataset <name>    Registered dataset name
  -t, --task <name>       Registered task name
      --task-git-url <url>
                          Git URL for a task repository
      --task-git-commit <commit>
                          Git commit for --task-git-url
      --registry-path <path>
                          Local registry JSON for -d datasets and -t tasks
      --registry-url <url>
                          Remote registry URL; defaults to the package registry
  -a, --agent <agent>     Agent adapter name; defaults to oracle
      --agent-setup <shell>   Command run in the container before the agent (e.g.
                          to install the agent CLI); runs as the agent user
      --agent-command <shell>
                          Shell command for custom or not-yet-native agents
      --ae, --agent-env KEY=VALUE
                          Environment variable for the agent phase
      --ve, --verifier-env KEY=VALUE
                          Environment variable for the verifier phase
  -m, --model <model>     Model identifier
  -n <count>              Concurrency
  -k, --n-attempts <count>
                          Number of attempts per task
      --strict-resources  Enforce the task's declared cpus/memory exactly
                          (harbor-compatible), instead of the default fair CPU
                          share
      --max-retries <count>
                          Retry an errored trial up to this many times (default
                          0). Timeouts and reward-file errors are never retried.
      --retry-include <substr>
                          Only retry errors whose message contains this string
                          (repeatable)
      --retry-exclude <substr>
                          Never retry errors whose message contains this string
                          (repeatable; defaults to timeout/reward-file errors)
      --timeout-multiplier <factor>
                          Scale all phase timeouts for slow/emulated hosts
                          (default: 1.0)
      --agent-timeout-multiplier <factor>
                          Scale only the agent timeout (falls back to
                          --timeout-multiplier)
      --verifier-timeout-multiplier <factor>
                          Scale only the verifier timeout (falls back to
                          --timeout-multiplier)
      --build-timeout-multiplier <factor>
                          Scale only the build/image-pull timeout (falls back
                          to --timeout-multiplier)
      --jobs-dir <path>   Directory where job results are written
      --backend <name>    Execution backend: docker or unsafe-local
      --env <name>        Alias for --backend
      --verbose           Stream raw command stdout/stderr
      --quiet             Print only the final summary
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

#[derive(Debug, PartialEq)]
struct RunOptions {
    path: Option<String>,
    dataset: Option<String>,
    task: Option<String>,
    task_git_url: Option<String>,
    task_git_commit: Option<String>,
    registry_path: Option<String>,
    registry_url: Option<String>,
    agent: Option<String>,
    agent_command: Option<String>,
    agent_setup: Option<String>,
    agent_env: Vec<(String, String)>,
    verifier_env: Vec<(String, String)>,
    model: Option<String>,
    concurrency: usize,
    attempts: usize,
    backend: SandboxBackend,
    strict_resources: bool,
    max_retries: usize,
    retry_include: Vec<String>,
    retry_exclude: Vec<String>,
    timeout_multiplier: f64,
    agent_timeout_multiplier: Option<f64>,
    verifier_timeout_multiplier: Option<f64>,
    build_timeout_multiplier: Option<f64>,
    jobs_dir: Option<String>,
    log_mode: LogMode,
    selection: TaskSelection,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            path: None,
            dataset: None,
            task: None,
            task_git_url: None,
            task_git_commit: None,
            registry_path: None,
            registry_url: None,
            agent: None,
            agent_command: None,
            agent_setup: None,
            agent_env: Vec::new(),
            verifier_env: Vec::new(),
            model: None,
            concurrency: default_concurrency(),
            attempts: 1,
            strict_resources: false,
            max_retries: 0,
            retry_include: Vec::new(),
            retry_exclude: default_retry_exclude(),
            timeout_multiplier: 1.0,
            agent_timeout_multiplier: None,
            verifier_timeout_multiplier: None,
            build_timeout_multiplier: None,
            backend: SandboxBackend::Docker,
            jobs_dir: None,
            log_mode: LogMode::Concise,
            selection: TaskSelection::default(),
        }
    }
}

/// Error substrings that are never retried by default, mirroring harbor's
/// exclude set: agent/verifier timeouts and reward-file problems.
fn default_retry_exclude() -> Vec<String> {
    ["timed out", "did not write", "unparseable reward"]
        .iter()
        .map(|pattern| (*pattern).to_owned())
        .collect()
}

fn default_concurrency() -> usize {
    // Roughly one trial per three host CPUs. Trials are heavy containers —
    // frequently emulating a foreign architecture and often memory-hungry — so
    // packing one per core starves each of CPU (pushing slow tasks past their
    // timeout) and overcommits the docker VM's memory. A third of the cores
    // leaves each trial enough headroom while still running several at once.
    // Override with `-j` when the host or the dataset can take more.
    let host_cpus = thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(4);

    (host_cpus / 3).clamp(2, 16)
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
                "-t" | "--task" => {
                    options.task = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "--task-git-url" => {
                    options.task_git_url = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "--task-git-commit" => {
                    options.task_git_commit = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "--registry-path" => {
                    options.registry_path = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "--registry-url" => {
                    options.registry_url = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "-a" | "--agent" => {
                    options.agent = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "--agent-command" => {
                    options.agent_command = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "--agent-setup" => {
                    options.agent_setup = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "--ae" | "--agent-env" => {
                    let value = required_value(args, index, flag)?;
                    options.agent_env.push(parse_env_assignment(flag, &value)?);
                    index += 2;
                }
                "--ve" | "--verifier-env" => {
                    let value = required_value(args, index, flag)?;
                    options
                        .verifier_env
                        .push(parse_env_assignment(flag, &value)?);
                    index += 2;
                }
                "-m" | "--model" => {
                    options.model = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "-n" => {
                    let value = required_value(args, index, flag)?;
                    options.concurrency = parse_positive_usize(flag, &value)?;
                    index += 2;
                }
                "-k" | "--n-attempts" => {
                    let value = required_value(args, index, flag)?;
                    options.attempts = parse_positive_usize(flag, &value)?;
                    index += 2;
                }
                "--backend" | "--env" => {
                    let value = required_value(args, index, flag)?;
                    options.backend = SandboxBackend::parse(&value)?;
                    index += 2;
                }
                "--strict-resources" => {
                    options.strict_resources = true;
                    index += 1;
                }
                "--max-retries" => {
                    let value = required_value(args, index, flag)?;
                    options.max_retries = value.parse::<usize>().map_err(|_| {
                        CliError::usage(format!("{flag} requires a non-negative integer"))
                    })?;
                    index += 2;
                }
                "--retry-include" => {
                    options
                        .retry_include
                        .push(required_value(args, index, flag)?);
                    index += 2;
                }
                "--retry-exclude" => {
                    options
                        .retry_exclude
                        .push(required_value(args, index, flag)?);
                    index += 2;
                }
                "--timeout-multiplier" => {
                    let value = required_value(args, index, flag)?;
                    options.timeout_multiplier = parse_positive_f64(flag, &value)?;
                    index += 2;
                }
                "--agent-timeout-multiplier" => {
                    let value = required_value(args, index, flag)?;
                    options.agent_timeout_multiplier = Some(parse_positive_f64(flag, &value)?);
                    index += 2;
                }
                "--verifier-timeout-multiplier" => {
                    let value = required_value(args, index, flag)?;
                    options.verifier_timeout_multiplier = Some(parse_positive_f64(flag, &value)?);
                    index += 2;
                }
                "--build-timeout-multiplier" => {
                    let value = required_value(args, index, flag)?;
                    options.build_timeout_multiplier = Some(parse_positive_f64(flag, &value)?);
                    index += 2;
                }
                "--jobs-dir" => {
                    options.jobs_dir = Some(required_value(args, index, flag)?);
                    index += 2;
                }
                "--verbose" => {
                    options.log_mode = LogMode::Verbose;
                    index += 1;
                }
                "--quiet" => {
                    options.log_mode = LogMode::Quiet;
                    index += 1;
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
                    options.selection.task_limit = Some(parse_positive_usize(flag, &value)?);
                    index += 2;
                }
                unknown => {
                    return Err(CliError::usage(format!("unknown run option `{unknown}`")));
                }
            }
        }

        Ok(options)
    }

    fn has_run_source(&self) -> bool {
        self.path.is_some()
            || self.dataset.is_some()
            || self.task.is_some()
            || self.task_git_url.is_some()
    }

    fn validate_sources(&self) -> Result<(), CliError> {
        if self.task_git_commit.is_some() && self.task_git_url.is_none() {
            return Err(CliError::usage(
                "`--task-git-commit` requires `--task-git-url`",
            ));
        }

        if self.task_git_url.is_some() {
            if self.path.is_none() {
                return Err(CliError::usage(
                    "`--task-git-url` requires `-p <path-in-repo>`",
                ));
            }

            if self.dataset.is_some() || self.task.is_some() {
                return Err(CliError::usage(
                    "`--task-git-url` cannot be combined with `-d` or `-t`",
                ));
            }

            return Ok(());
        }

        let source_count = usize::from(self.path.is_some())
            + usize::from(self.dataset.is_some())
            + usize::from(self.task.is_some());

        if source_count > 1 {
            return Err(CliError::usage(
                "run accepts one task source: `-p`, `-d`, `-t`, or `--task-git-url`",
            ));
        }

        Ok(())
    }

    fn phase_envs(&self) -> PhaseEnvs {
        PhaseEnvs {
            agent: self.agent_env.clone(),
            verifier: self.verifier_env.clone(),
        }
    }

    fn timeout_multipliers(&self) -> TimeoutMultipliers {
        TimeoutMultipliers::resolve(
            self.timeout_multiplier,
            self.agent_timeout_multiplier,
            self.verifier_timeout_multiplier,
            self.build_timeout_multiplier,
        )
    }
}

fn required_value(args: &[String], index: usize, flag: &str) -> Result<String, CliError> {
    args.get(index + 1)
        .cloned()
        .ok_or_else(|| CliError::usage(format!("{flag} requires a value")))
}

fn parse_env_assignment(flag: &str, value: &str) -> Result<(String, String), CliError> {
    // `KEY=VALUE` sets an explicit value; a bare `KEY` forwards that variable
    // from the host environment (harbor reads e.g. ANTHROPIC_API_KEY from the
    // host), so secrets need not appear on the command line.
    match value.split_once('=') {
        Some(("", _)) => Err(CliError::usage(format!("{flag} requires a non-empty KEY"))),
        Some((name, value)) => Ok((name.to_owned(), value.to_owned())),
        None if value.is_empty() => {
            Err(CliError::usage(format!("{flag} requires a non-empty KEY")))
        }
        None => {
            let resolved = env::var(value).map_err(|_| {
                CliError::usage(format!(
                    "{flag} {value}: variable is not set in the environment"
                ))
            })?;
            Ok((value.to_owned(), resolved))
        }
    }
}

fn parse_positive_usize(flag: &str, value: &str) -> Result<usize, CliError> {
    let parsed = value
        .parse::<usize>()
        .map_err(|error| CliError::usage(format!("{flag} must be a positive integer: {error}")))?;

    if parsed == 0 {
        return Err(CliError::usage(format!("{flag} must be greater than zero")));
    }

    Ok(parsed)
}

fn parse_positive_f64(flag: &str, value: &str) -> Result<f64, CliError> {
    let parsed = value
        .parse::<f64>()
        .map_err(|error| CliError::usage(format!("{flag} must be a positive number: {error}")))?;

    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(CliError::usage(format!("{flag} must be greater than zero")));
    }

    Ok(parsed)
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

    fn is_task_failure(&self) -> bool {
        self.exit_code == EXIT_TASK_FAILED
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

    fn reward_outcome(reward: f64) -> TrialOutcome {
        let rewards = Rewards::single(reward);
        TrialOutcome {
            task_name: "acme/task".to_owned(),
            attempt: 1,
            reward: rewards.display(),
            passed: rewards.passed(),
            rewards,
            retries: 0,
            errored: false,
            error: None,
            stdout_tail: Vec::new(),
            stderr_tail: Vec::new(),
            trial_dir: PathBuf::new(),
            elapsed: Duration::ZERO,
        }
    }

    #[test]
    fn reward_json_object_fractional_is_not_passed() {
        let dir = temp_test_dir("reward-json-fractional");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("reward.json"), "{\"reward\": 0.5}").expect("reward.json");

        let reward = read_reward(&dir).expect("reward");

        assert_eq!(reward.primary(), 0.5);
        assert!(!reward.passed());
        assert_eq!(reward.display(), "0.5");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reward_json_bare_number_full_credit_passes() {
        let dir = temp_test_dir("reward-json-bare");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("reward.json"), "1.0").expect("reward.json");

        let reward = read_reward(&dir).expect("reward");

        assert!(reward.passed());
        assert_eq!(reward.display(), "1");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reward_json_takes_precedence_over_reward_txt() {
        let dir = temp_test_dir("reward-json-precedence");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("reward.json"), "0.25").expect("reward.json");
        fs::write(dir.join("reward.txt"), "1\n").expect("reward.txt");

        let reward = read_reward(&dir).expect("reward");

        assert_eq!(reward.primary(), 0.25);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reward_txt_integer_one_still_passes() {
        let dir = temp_test_dir("reward-txt-one");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("reward.txt"), "1\n").expect("reward.txt");

        let reward = read_reward(&dir).expect("reward");

        assert!(reward.passed());
        assert_eq!(reward.display(), "1");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reward_json_named_scores_pass_only_when_all_full() {
        let dir = temp_test_dir("reward-json-named");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(
            dir.join("reward.json"),
            "{\"core_pass_rate\": 1.0, \"strict_pass_rate\": 0.5}",
        )
        .expect("reward.json");

        let reward = read_reward(&dir).expect("reward");

        // No 1-D `reward` key: every named score must be full to pass.
        assert!(!reward.passed());
        assert_eq!(reward.get("core_pass_rate"), Some(1.0));
        assert_eq!(reward.get("strict_pass_rate"), Some(0.5));
        // Primary scalar is the mean of named scores; full map is preserved.
        assert_eq!(reward.primary(), 0.75);
        assert_eq!(
            reward.to_json(),
            "{\"core_pass_rate\":1,\"strict_pass_rate\":0.5}"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reward_json_named_scores_all_full_passes() {
        let dir = temp_test_dir("reward-json-named-full");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(
            dir.join("reward.json"),
            "{\"core_pass_rate\": 1.0, \"strict_pass_rate\": 1.0}",
        )
        .expect("reward.json");

        let reward = read_reward(&dir).expect("reward");

        assert!(reward.passed());
        assert_eq!(reward.display(), "1");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn read_reward_errors_when_no_reward_file_present() {
        let dir = temp_test_dir("reward-missing");
        fs::create_dir_all(&dir).expect("dir");

        let error = read_reward(&dir).expect_err("missing reward");

        assert!(error.is_task_failure());

        let _ = fs::remove_dir_all(dir);
    }

    fn scored_outcome(task: &str, dir: &str, entries: Vec<(&str, f64)>) -> TrialOutcome {
        let rewards = Rewards::from_entries(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        );
        TrialOutcome {
            task_name: task.to_owned(),
            attempt: 1,
            reward: rewards.display(),
            passed: rewards.passed(),
            rewards,
            retries: 0,
            errored: false,
            error: None,
            stdout_tail: Vec::new(),
            stderr_tail: Vec::new(),
            trial_dir: PathBuf::from(dir),
            elapsed: Duration::ZERO,
        }
    }

    #[test]
    fn reward_stats_groups_trials_by_per_key_value() {
        let outcomes = [
            scored_outcome(
                "acme/a",
                "acme-a",
                vec![("core_pass_rate", 1.0), ("verbosity", 0.25)],
            ),
            scored_outcome(
                "acme/b",
                "acme-b",
                vec![("core_pass_rate", 1.0), ("verbosity", 0.5)],
            ),
        ];
        let scored: Vec<&TrialOutcome> = outcomes.iter().collect();
        let stats = reward_stats_json(&scored);

        // core_pass_rate is 1.0 for both trials; verbosity differs per trial.
        assert!(stats.contains("\"core_pass_rate\": {\"1.0\": [\"acme-a\", \"acme-b\"]}"));
        assert!(stats.contains("\"verbosity\": {\"0.25\": [\"acme-a\"], \"0.5\": [\"acme-b\"]}"));
    }

    #[test]
    fn metrics_are_per_key_mean_or_one_d_mean() {
        // Multi-key -> per-key mean.
        let multi = [
            scored_outcome(
                "acme/a",
                "a",
                vec![("core_pass_rate", 1.0), ("verbosity", 0.25)],
            ),
            scored_outcome(
                "acme/b",
                "b",
                vec![("core_pass_rate", 0.0), ("verbosity", 0.75)],
            ),
        ];
        let scored: Vec<&TrialOutcome> = multi.iter().collect();
        assert_eq!(
            reward_metrics_json(&scored),
            "[{\"core_pass_rate\": 0.5, \"verbosity\": 0.5}]"
        );

        // Single 1-D reward -> {"mean": x}.
        let single = [
            scored_outcome("acme/a", "a", vec![("reward", 1.0)]),
            scored_outcome("acme/b", "b", vec![("reward", 0.0)]),
        ];
        let scored: Vec<&TrialOutcome> = single.iter().collect();
        assert_eq!(reward_metrics_json(&scored), "[{\"mean\": 0.5}]");
    }

    #[test]
    fn pass_at_k_empty_for_single_attempt_and_for_multi_key() {
        // Single binary reward, one attempt per task -> no eligible k -> {}.
        let single = [scored_outcome("acme/a", "a", vec![("reward", 1.0)])];
        let scored: Vec<&TrialOutcome> = single.iter().collect();
        assert_eq!(pass_at_k_json(&scored), "{}");

        // Multi-key reward -> never produces pass@k.
        let multi = [scored_outcome(
            "acme/a",
            "a",
            vec![("core_pass_rate", 1.0), ("x", 0.5)],
        )];
        let scored: Vec<&TrialOutcome> = multi.iter().collect();
        assert_eq!(pass_at_k_json(&scored), "{}");
    }

    #[test]
    fn pass_at_k_two_attempts_binary() {
        // Two attempts of one task, one success one failure -> pass@2 = 1.0.
        let outcomes = [
            scored_outcome("acme/a", "a-1", vec![("reward", 1.0)]),
            scored_outcome("acme/a", "a-2", vec![("reward", 0.0)]),
        ];
        let scored: Vec<&TrialOutcome> = outcomes.iter().collect();
        assert_eq!(pass_at_k_json(&scored), "{\"2\": 1}");
    }

    #[test]
    fn aggregate_reward_averages_fractional_values() {
        let outcomes = [reward_outcome(0.5), reward_outcome(1.0)];

        assert_eq!(aggregate_reward(&outcomes), "0.75");
    }

    #[test]
    fn parses_local_run_options() {
        let args = strings(["-p", "tasks/example", "-a", "codex", "-m", "openai/gpt-5"]);

        let options = RunOptions::parse(&args).expect("options");

        assert_eq!(options.path.as_deref(), Some("tasks/example"));
        assert_eq!(options.agent.as_deref(), Some("codex"));
        assert_eq!(options.model.as_deref(), Some("openai/gpt-5"));
        assert_eq!(options.backend, SandboxBackend::Docker);
        assert_eq!(options.log_mode, LogMode::Concise);
    }

    #[test]
    fn parses_registered_dataset_options() {
        let args = strings([
            "-d",
            "bench/example@1.0",
            "--registry-path",
            "registry.json",
            "--registry-url",
            "https://example.test/registry.json",
            "-a",
            "claude-code",
            "-m",
            "anthropic/claude",
            "-n",
            "8",
            "-k",
            "2",
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
        assert_eq!(
            options.registry_url.as_deref(),
            Some("https://example.test/registry.json")
        );
        assert_eq!(options.concurrency, 8);
        assert_eq!(options.attempts, 2);
        assert_eq!(options.backend, SandboxBackend::Docker);
        assert_eq!(options.jobs_dir.as_deref(), Some("jobs/custom"));
        assert_eq!(options.selection.include_task_names, ["bench/*"]);
        assert_eq!(options.selection.exclude_task_names, ["bench/skip-*"]);
        assert_eq!(options.selection.task_limit, Some(5));
    }

    #[test]
    fn parses_timeout_multiplier_options() {
        let args = strings([
            "-p",
            "tasks/example",
            "--timeout-multiplier",
            "2",
            "--agent-timeout-multiplier",
            "3.5",
            "--verifier-timeout-multiplier",
            "1.5",
            "--build-timeout-multiplier",
            "4",
        ]);

        let options = RunOptions::parse(&args).expect("options");

        assert_eq!(options.timeout_multiplier, 2.0);
        assert_eq!(options.agent_timeout_multiplier, Some(3.5));
        assert_eq!(options.verifier_timeout_multiplier, Some(1.5));
        assert_eq!(options.build_timeout_multiplier, Some(4.0));

        let resolved = options.timeout_multipliers();
        assert_eq!(resolved.agent, 3.5);
        assert_eq!(resolved.verifier, 1.5);
        assert_eq!(resolved.build, 4.0);
    }

    #[test]
    fn timeout_multiplier_defaults_to_one() {
        let args = strings(["-p", "tasks/example"]);

        let options = RunOptions::parse(&args).expect("options");

        assert_eq!(options.timeout_multiplier, 1.0);
        assert_eq!(options.agent_timeout_multiplier, None);
        let resolved = options.timeout_multipliers();
        assert_eq!(resolved.agent, 1.0);
        assert_eq!(resolved.verifier, 1.0);
        assert_eq!(resolved.build, 1.0);
    }

    #[test]
    fn per_phase_timeout_multiplier_falls_back_to_global() {
        let args = strings([
            "-p",
            "tasks/example",
            "--timeout-multiplier",
            "2.5",
            "--agent-timeout-multiplier",
            "5",
        ]);

        let options = RunOptions::parse(&args).expect("options");
        let resolved = options.timeout_multipliers();

        assert_eq!(resolved.agent, 5.0);
        // Unset per-phase multipliers fall back to the global value.
        assert_eq!(resolved.verifier, 2.5);
        assert_eq!(resolved.build, 2.5);
    }

    #[test]
    fn rejects_non_positive_timeout_multiplier() {
        for value in ["0", "-1", "-2.5"] {
            let args = strings(["-p", "tasks/example", "--timeout-multiplier", value]);
            assert!(RunOptions::parse(&args).is_err());
        }

        let args = strings(["-p", "tasks/example", "--agent-timeout-multiplier", "0"]);
        assert!(RunOptions::parse(&args).is_err());
    }

    #[test]
    fn env_assignment_supports_explicit_and_host_passthrough() {
        // Explicit KEY=VALUE.
        assert_eq!(
            parse_env_assignment("--ae", "API_KEY=secret").unwrap(),
            ("API_KEY".to_owned(), "secret".to_owned())
        );
        // Bare KEY forwards from the host environment.
        std::env::set_var("SEAPORT_TEST_PASSTHROUGH", "from-host");
        assert_eq!(
            parse_env_assignment("--ae", "SEAPORT_TEST_PASSTHROUGH").unwrap(),
            (
                "SEAPORT_TEST_PASSTHROUGH".to_owned(),
                "from-host".to_owned()
            )
        );
        std::env::remove_var("SEAPORT_TEST_PASSTHROUGH");
        // Bare KEY that is unset is an error.
        assert!(parse_env_assignment("--ae", "SEAPORT_DEFINITELY_UNSET_VAR").is_err());
        // Empty is rejected.
        assert!(parse_env_assignment("--ae", "").is_err());
        assert!(parse_env_assignment("--ae", "=v").is_err());
    }

    #[test]
    fn retry_filter_excludes_timeouts_and_reward_errors_by_default() {
        let exclude = default_retry_exclude();
        // Default-excluded (non-retryable) errors.
        assert!(!should_retry_error(
            Some("sandboxed docker command timed out after 120.000s"),
            &[],
            &exclude
        ));
        assert!(!should_retry_error(
            Some("verifier did not write reward.txt"),
            &[],
            &exclude
        ));
        // A transient infra error is retryable by default.
        assert!(should_retry_error(
            Some("docker image build failed: 503 service unavailable"),
            &[],
            &exclude
        ));
    }

    #[test]
    fn retry_include_filter_restricts_to_matching_errors() {
        let include = vec!["build failed".to_owned()];
        assert!(should_retry_error(
            Some("docker image build failed"),
            &include,
            &[]
        ));
        assert!(!should_retry_error(Some("agent crashed"), &include, &[]));
    }

    #[test]
    fn retry_exclude_takes_precedence_over_include() {
        let include = vec!["build failed".to_owned()];
        let exclude = vec!["timed out".to_owned()];
        assert!(!should_retry_error(
            Some("docker image build failed; then timed out"),
            &include,
            &exclude
        ));
    }

    #[test]
    fn retry_backoff_is_capped() {
        assert_eq!(retry_backoff_delay(0), Duration::from_secs_f64(1.0));
        // With the default constant multiplier it stays at the min wait.
        assert_eq!(retry_backoff_delay(10), Duration::from_secs_f64(1.0));
    }

    #[test]
    fn default_concurrency_is_positive_and_bounded() {
        let concurrency = default_concurrency();

        assert!((1..=16).contains(&concurrency));
    }

    #[test]
    fn run_phase_concurrency_honors_execution_requests() {
        assert_eq!(RunPhase::Execution.concurrency(16, 10), 10);
        assert_eq!(RunPhase::Execution.concurrency(32, 64), 32);
        assert_eq!(RunPhase::Execution.concurrency(0, 0), 1);
    }

    #[test]
    fn parses_registered_task_options() {
        let args = strings([
            "-t",
            "acme/task",
            "--registry-path",
            "registry.json",
            "-a",
            "nop",
        ]);

        let options = RunOptions::parse(&args).expect("options");

        assert_eq!(options.task.as_deref(), Some("acme/task"));
        assert_eq!(options.registry_path.as_deref(), Some("registry.json"));
        assert_eq!(options.agent.as_deref(), Some("nop"));
    }

    #[test]
    fn scheduler_prioritizes_long_running_task_shapes() {
        let root = temp_test_dir("schedule");
        let fast = root.join("fast");
        let asm = root.join("asm");
        let rust = root.join("rust");
        let java = root.join("java");

        write_schedule_fixture(&fast, "", "");
        write_schedule_fixture(&asm, "", "");
        fs::write(
            asm.join("environment").join("boot.s"),
            ".intel_syntax noprefix\n",
        )
        .expect("asm");
        write_schedule_fixture(&rust, "", "cargo build --release\n");
        write_schedule_fixture(&java, "RUN apt-get install -y zulu7-jdk\n", "");

        let tasks = [
            TaskRef {
                name: "fast".to_owned(),
                path: fast,
            },
            TaskRef {
                name: "asm".to_owned(),
                path: asm,
            },
            TaskRef {
                name: "rust".to_owned(),
                path: rust,
            },
            TaskRef {
                name: "java".to_owned(),
                path: java,
            },
        ];
        let plans = tasks
            .iter()
            .map(|task| TrialPlan { task, attempt: 1 })
            .collect::<Vec<_>>();
        let scheduled = scheduled_trial_indices(&plans)
            .into_iter()
            .map(|index| plans[index].task.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(scheduled, ["java", "rust", "asm", "fast"]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_agent_command_option() {
        let args = strings([
            "-p",
            "tasks/example",
            "-a",
            "custom",
            "--agent-command",
            "printf ok > \"$APP_DIR/output.txt\"",
        ]);

        let options = RunOptions::parse(&args).expect("options");

        assert_eq!(options.agent.as_deref(), Some("custom"));
        assert_eq!(
            options.agent_command.as_deref(),
            Some("printf ok > \"$APP_DIR/output.txt\"")
        );
    }

    #[test]
    fn parses_phase_environment_options() {
        let args = strings([
            "-p",
            "tasks/example",
            "-a",
            "custom",
            "--ae",
            "OPENAI_API_KEY=test-key",
            "--verifier-env",
            "EXPECTED=ok",
        ]);

        let options = RunOptions::parse(&args).expect("options");

        assert_eq!(
            options.agent_env,
            [("OPENAI_API_KEY".to_owned(), "test-key".to_owned())]
        );
        assert_eq!(
            options.verifier_env,
            [("EXPECTED".to_owned(), "ok".to_owned())]
        );
    }

    #[test]
    fn parses_log_mode_options() {
        let verbose = RunOptions::parse(&strings(["-p", "tasks/example", "--verbose"]))
            .expect("verbose options");
        let quiet =
            RunOptions::parse(&strings(["-p", "tasks/example", "--quiet"])).expect("quiet options");

        assert_eq!(verbose.log_mode, LogMode::Verbose);
        assert_eq!(quiet.log_mode, LogMode::Quiet);
    }

    #[test]
    fn tail_lines_uses_last_non_empty_lines() {
        let tail = tail_lines_text("one\n\n two \nthree\nfour\n", 2);

        assert_eq!(tail, ["three", "four"]);
    }

    #[test]
    fn failure_output_tail_reads_stream_sections() {
        let message = "script failed\nstdout:\nfirst\nsecond\nstderr:\nboom\nlast\n";

        assert_eq!(
            failure_output_tail(message, "stdout", 8).expect("stdout"),
            ["first", "second"]
        );
        assert_eq!(
            failure_output_tail(message, "stderr", 1).expect("stderr"),
            ["last"]
        );
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
    fn rejects_task_git_commit_without_git_url() {
        let args = strings(["run", "-p", "tasks/example", "--task-git-commit", "abc123"]);

        let error = run(args).expect_err("error");

        assert_eq!(error.exit_code(), EXIT_USAGE);
    }

    #[test]
    fn codex_agent_requires_model_without_custom_command() {
        let args = strings(["run", "-p", "missing", "-a", "codex"]);

        let error = run(args).expect_err("error");

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
    fn runs_multiple_attempts_with_concurrency() {
        let root = temp_test_dir("attempts");
        let task = root.join("task");
        let jobs = root.join("jobs");

        write_oracle_task(&task, "acme/attempts");

        let args = vec![
            "run".to_owned(),
            "-p".to_owned(),
            task.display().to_string(),
            "-a".to_owned(),
            "oracle".to_owned(),
            "-k".to_owned(),
            "2".to_owned(),
            "-n".to_owned(),
            "2".to_owned(),
            "--backend".to_owned(),
            "unsafe-local".to_owned(),
            "--jobs-dir".to_owned(),
            jobs.display().to_string(),
        ];

        run(args).expect("attempted run");

        let job_dir = single_child_dir(&jobs);
        let result = fs::read_to_string(job_dir.join("result.json")).expect("job result");
        let config = fs::read_to_string(job_dir.join("config.json")).expect("job config");

        assert!(result.contains("\"tasks_total\": 2"));
        assert!(result.contains("\"tasks_passed\": 2"));
        assert!(result.contains("\"attempt\":1"));
        assert!(result.contains("\"attempt\":2"));
        assert!(config.contains("\"attempts\": 2"));
        assert!(config.contains("\"concurrency\": 2"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_trial_durations_sum_to_wall_clock() {
        let root = temp_test_dir("trial-durations");
        let jobs = root.join("jobs");

        // Three trials that each sleep the same amount, all run concurrently, so
        // the execution wall-clock is ~one sleep, not the sum of the three.
        let sleep_secs = 0.30;
        let task_dirs = ["acme/a", "acme/b", "acme/c"]
            .iter()
            .map(|name| {
                let dir = root.join(name.replace('/', "-"));
                write_sleeping_oracle_task(&dir, name, "0.30");
                (name.to_string(), dir)
            })
            .collect::<Vec<_>>();

        let tasks = task_dirs
            .iter()
            .map(|(name, dir)| TaskRef {
                name: name.clone(),
                path: dir.clone(),
            })
            .collect::<Vec<_>>();
        let plans = tasks
            .iter()
            .map(|task| TrialPlan { task, attempt: 1 })
            .collect::<Vec<_>>();
        let options = RunOptions {
            backend: SandboxBackend::UnsafeLocal,
            log_mode: LogMode::Quiet,
            ..RunOptions::default()
        };

        let wall_started = Instant::now();
        let outcomes = run_trial_plans(
            &plans,
            &jobs.join("run"),
            "duration-test",
            &options,
            AgentKind::Oracle,
            tasks.len(),
        )
        .expect("trial outcomes");
        let wall_clock = wall_started.elapsed();

        let total: Duration = outcomes.iter().map(|outcome| outcome.elapsed).sum();
        let sleep = Duration::from_secs_f64(sleep_secs);

        // The per-task durations tile the timeline, so they sum to the execution
        // wall-clock rather than to three independent sleeps.
        assert!(
            total <= wall_clock + Duration::from_millis(50),
            "per-task durations should sum to wall-clock: total={total:?} wall={wall_clock:?}"
        );
        assert!(
            total < sleep * 2,
            "per-task durations must not be additive: total={total:?} sleep={sleep:?}"
        );
        assert!(
            total >= sleep,
            "per-task durations should cover the run: total={total:?} sleep={sleep:?}"
        );

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
    fn runs_external_agent_command_without_rust_code() {
        let root = temp_test_dir("external-agent-command");
        let task = root.join("task");
        let jobs = root.join("jobs");

        write_agent_task(&task, "acme/external-agent");

        let args = vec![
            "run".to_owned(),
            "-p".to_owned(),
            task.display().to_string(),
            "-a".to_owned(),
            "custom".to_owned(),
            "--agent-command".to_owned(),
            "printf '%s\\n' \"$SEAPORT_TEST_VALUE\" > \"$APP_DIR/output.txt\"".to_owned(),
            "--ae".to_owned(),
            "SEAPORT_TEST_VALUE=ok".to_owned(),
            "--backend".to_owned(),
            "unsafe-local".to_owned(),
            "--jobs-dir".to_owned(),
            jobs.display().to_string(),
        ];

        run(args).expect("external command run");

        let job_dir = single_child_dir(&jobs);
        let result = fs::read_to_string(job_dir.join("result.json")).expect("job result");
        let config = fs::read_to_string(job_dir.join("config.json")).expect("job config");

        assert!(result.contains("\"tasks_passed\": 1"));
        assert!(config.contains("\"agent\": \"custom\""));
        assert!(config.contains("\"agent_command\": \"printf '%s\\\\n'"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn passes_environment_to_verifier_phase() {
        let root = temp_test_dir("verifier-env");
        let task = root.join("task");
        let jobs = root.join("jobs");

        write_verifier_env_task(&task, "acme/verifier-env");

        let args = vec![
            "run".to_owned(),
            "-p".to_owned(),
            task.display().to_string(),
            "-a".to_owned(),
            "nop".to_owned(),
            "--ve".to_owned(),
            "SEAPORT_EXPECTED=ok".to_owned(),
            "--backend".to_owned(),
            "unsafe-local".to_owned(),
            "--jobs-dir".to_owned(),
            jobs.display().to_string(),
        ];

        run(args).expect("verifier env run");

        let job_dir = single_child_dir(&jobs);
        let result = fs::read_to_string(job_dir.join("result.json")).expect("job result");

        assert!(result.contains("\"tasks_passed\": 1"));

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
    fn records_task_execution_errors_without_stopping_dataset() {
        let root = temp_test_dir("task-execution-error");
        let dataset = root.join("suite");
        let jobs = root.join("jobs");

        fs::create_dir_all(&dataset).expect("dataset dir");
        fs::write(
            dataset.join("dataset.toml"),
            "[dataset]\nname = \"acme/errors\"\ndescription = \"error suite\"\n",
        )
        .expect("dataset manifest");
        write_oracle_task(&dataset.join("good"), "acme/good");
        write_failing_oracle_task(&dataset.join("bad"), "acme/bad");

        let args = vec![
            "run".to_owned(),
            "-p".to_owned(),
            dataset.display().to_string(),
            "-a".to_owned(),
            "oracle".to_owned(),
            "-n".to_owned(),
            "1".to_owned(),
            "--backend".to_owned(),
            "unsafe-local".to_owned(),
            "--jobs-dir".to_owned(),
            jobs.display().to_string(),
        ];

        let error = run(args).expect_err("dataset has one failed task");

        assert_eq!(error.exit_code(), EXIT_TASK_FAILED);

        let job_dir = single_child_dir(&jobs);
        let result = fs::read_to_string(job_dir.join("result.json")).expect("job result");
        let failed_result =
            fs::read_to_string(job_dir.join("acme-bad").join("result.json")).expect("bad result");

        assert!(result.contains("\"tasks_total\": 2"));
        assert!(result.contains("\"tasks_passed\": 1"));
        assert!(result.contains("\"tasks_failed\": 1"));
        assert!(result.contains("\"task\":\"acme/bad\""));
        // The solution script exits non-zero, but that no longer fails the
        // trial on its own (matching harbor): the verifier runs and its
        // reward.txt of 0 is what marks the task failed.
        assert!(result.contains("\"error\":\"verifier returned reward 0\""));
        assert!(!result.contains("script failed:"));
        assert!(failed_result.contains("\"passed\": false"));
        assert!(failed_result.contains("\"error\": \"verifier returned reward 0\""));

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

    #[test]
    fn runs_local_registry_task() {
        let root = temp_test_dir("registry-task");
        let tasks = root.join("tasks");
        let jobs = root.join("jobs");
        let registry = root.join("registry.json");

        write_oracle_task(&tasks.join("one"), "acme/one");
        write_oracle_task(&tasks.join("two"), "acme/two");
        fs::write(
            &registry,
            format!(
                "[{{\"name\":\"acme/suite\",\"version\":\"head\",\"tasks\":[{{\"name\":\"acme/one\",\"path\":\"{}\"}},{{\"name\":\"acme/two\",\"path\":\"{}\"}}]}}]\n",
                tasks.join("one").display(),
                tasks.join("two").display()
            ),
        )
        .expect("registry");

        let args = vec![
            "run".to_owned(),
            "-t".to_owned(),
            "acme/two".to_owned(),
            "--registry-path".to_owned(),
            registry.display().to_string(),
            "-a".to_owned(),
            "oracle".to_owned(),
            "--backend".to_owned(),
            "unsafe-local".to_owned(),
            "--jobs-dir".to_owned(),
            jobs.display().to_string(),
        ];

        run(args).expect("registry task run");

        let job_dir = single_child_dir(&jobs);
        let result = fs::read_to_string(job_dir.join("result.json")).expect("job result");
        let config = fs::read_to_string(job_dir.join("config.json")).expect("job config");

        assert!(result.contains("\"tasks_total\": 1"));
        assert!(result.contains("\"tasks_passed\": 1"));
        assert!(config.contains("\"target\": \"acme/two\""));
        assert!(config.contains("\"acme/two\""));
        assert!(!config.contains("\"acme/one\""));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runs_direct_git_task_source() {
        let root = temp_test_dir("direct-git-task");
        let repo = root.join("repo");
        let jobs = root.join("jobs");

        write_oracle_task(&repo.join("tasks").join("one"), "acme/git-one");
        run_test_git(&repo, ["init", "--quiet"]);
        run_test_git(&repo, ["add", "."]);
        run_test_git(
            &repo,
            [
                "-c",
                "user.name=Seaport Test",
                "-c",
                "user.email=seaport@example.com",
                "commit",
                "--quiet",
                "-m",
                "add task",
            ],
        );
        let commit = git_stdout(&repo, ["rev-parse", "HEAD"]);

        let args = vec![
            "run".to_owned(),
            "--task-git-url".to_owned(),
            repo.display().to_string(),
            "--task-git-commit".to_owned(),
            commit,
            "-p".to_owned(),
            "tasks/one".to_owned(),
            "-a".to_owned(),
            "oracle".to_owned(),
            "--backend".to_owned(),
            "unsafe-local".to_owned(),
            "--jobs-dir".to_owned(),
            jobs.display().to_string(),
        ];

        run(args).expect("direct git task run");

        let job_dir = single_child_dir(&jobs);
        let result = fs::read_to_string(job_dir.join("result.json")).expect("job result");

        assert!(result.contains("\"tasks_total\": 1"));
        assert!(result.contains("\"tasks_passed\": 1"));

        let _ = fs::remove_dir_all(root);
    }

    fn strings<const N: usize>(items: [&str; N]) -> Vec<String> {
        items.into_iter().map(str::to_owned).collect()
    }

    fn temp_test_dir(name: &str) -> PathBuf {
        let id = timestamp_id().expect("timestamp");
        env::temp_dir().join(format!("seaport-{name}-{id}"))
    }

    fn write_schedule_fixture(root: &Path, dockerfile_body: &str, solution_body: &str) {
        fs::create_dir_all(root.join("environment")).expect("environment dir");
        fs::create_dir_all(root.join("solution")).expect("solution dir");
        fs::write(
            root.join("environment").join("Dockerfile"),
            format!("FROM ubuntu:24.04\n{dockerfile_body}"),
        )
        .expect("dockerfile");
        fs::write(root.join("solution").join("solve.sh"), solution_body).expect("solution");
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

    fn write_sleeping_oracle_task(root: &Path, name: &str, seconds: &str) {
        write_oracle_task(root, name);

        let solve = root.join("solution").join("solve.sh");
        fs::write(
            &solve,
            format!(
                "#!/bin/bash\nset -euo pipefail\nsleep {seconds}\nprintf 'ok\\n' > \"$APP_DIR/output.txt\"\n"
            ),
        )
        .expect("sleeping solve");
        make_executable(&solve).expect("solve executable");
    }

    fn write_failing_oracle_task(root: &Path, name: &str) {
        fs::create_dir_all(root.join("solution")).expect("solution dir");
        fs::create_dir_all(root.join("tests")).expect("tests dir");
        fs::write(
            root.join("instruction.md"),
            "This task fails during execution.\n",
        )
        .expect("instruction");
        fs::write(root.join("task.toml"), task_toml(name)).expect("task toml");

        let solve = root.join("solution").join("solve.sh");
        let test = root.join("tests").join("test.sh");

        fs::write(&solve, "#!/bin/bash\nset -euo pipefail\nexit 17\n").expect("solve");
        fs::write(
            &test,
            "#!/bin/bash\nset -euo pipefail\nmkdir -p \"$LOGS_DIR\"\necho 0 > \"$LOGS_DIR/reward.txt\"\n",
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

    fn write_agent_task(root: &Path, name: &str) {
        fs::create_dir_all(root.join("tests")).expect("tests dir");
        fs::write(root.join("instruction.md"), "Create output.txt with ok.\n")
            .expect("instruction");
        fs::write(root.join("task.toml"), task_toml(name)).expect("task toml");

        let test = root.join("tests").join("test.sh");

        fs::write(
            &test,
            "#!/bin/bash\nset -euo pipefail\nmkdir -p \"$LOGS_DIR\"\nif [ \"$(cat \"$APP_DIR/output.txt\")\" = \"ok\" ]; then echo 1 > \"$LOGS_DIR/reward.txt\"; else echo 0 > \"$LOGS_DIR/reward.txt\"; fi\n",
        )
        .expect("test");

        make_executable(&test).expect("test executable");
    }

    fn write_verifier_env_task(root: &Path, name: &str) {
        fs::create_dir_all(root.join("tests")).expect("tests dir");
        fs::write(root.join("instruction.md"), "Verifier env task.\n").expect("instruction");
        fs::write(root.join("task.toml"), task_toml(name)).expect("task toml");

        let test = root.join("tests").join("test.sh");

        fs::write(
            &test,
            "#!/bin/bash\nset -euo pipefail\nmkdir -p \"$LOGS_DIR\"\nif [ \"$SEAPORT_EXPECTED\" = \"ok\" ]; then echo 1 > \"$LOGS_DIR/reward.txt\"; else echo 0 > \"$LOGS_DIR/reward.txt\"; fi\n",
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

    fn run_test_git<const N: usize>(cwd: &Path, args: [&str; N]) {
        let output = process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git");

        assert!(
            output.status.success(),
            "git failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout<const N: usize>(cwd: &Path, args: [&str; N]) -> String {
        let output = process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git");

        assert!(
            output.status.success(),
            "git failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
}
