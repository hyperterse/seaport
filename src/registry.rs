use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::CliError;

#[derive(Debug)]
pub(crate) struct ResolvedRegistryDataset {
    pub(crate) name: String,
    pub(crate) task_paths: Vec<PathBuf>,
}

pub(crate) fn resolve_local_registry_dataset(
    dataset: &str,
    registry_path: &Path,
) -> Result<ResolvedRegistryDataset, CliError> {
    let registry_root = registry_path.parent().unwrap_or_else(|| Path::new("."));
    let registry = RegistryFile::from_path(registry_path)?;
    let (dataset_name, requested_version) = parse_dataset_ref(dataset);
    let specs = registry
        .datasets()
        .iter()
        .filter(|spec| spec.name == dataset_name)
        .collect::<Vec<_>>();

    if specs.is_empty() {
        return Err(CliError::usage(format!(
            "dataset `{dataset_name}` was not found in {}",
            registry_path.display()
        )));
    }

    let version = requested_version
        .map(str::to_owned)
        .unwrap_or_else(|| resolve_version(specs.iter().map(|spec| spec.version.as_str())));
    let spec = specs
        .into_iter()
        .find(|spec| spec.version == version)
        .ok_or_else(|| {
            CliError::usage(format!(
                "dataset `{dataset_name}` has no version `{version}` in {}",
                registry_path.display()
            ))
        })?;

    let mut task_paths = Vec::with_capacity(spec.tasks.len());

    for task in &spec.tasks {
        task_paths.push(resolve_registry_task_path(registry_root, task)?);
    }

    Ok(ResolvedRegistryDataset {
        name: spec.name.clone(),
        task_paths,
    })
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RegistryFile {
    Array(Vec<DatasetSpec>),
    Object { datasets: Vec<DatasetSpec> },
}

impl RegistryFile {
    fn from_path(path: &Path) -> Result<Self, CliError> {
        let contents = std::fs::read_to_string(path)?;
        let registry = serde_json::from_str::<Self>(&contents).map_err(|error| {
            CliError::usage(format!(
                "could not parse registry JSON {}: {error}",
                path.display()
            ))
        })?;

        Ok(registry)
    }

    fn datasets(&self) -> &[DatasetSpec] {
        match self {
            Self::Array(datasets) | Self::Object { datasets } => datasets,
        }
    }
}

#[derive(Debug, Deserialize)]
struct DatasetSpec {
    name: String,
    version: String,
    #[serde(default)]
    tasks: Vec<RegistryTask>,
}

#[derive(Debug, Deserialize)]
struct RegistryTask {
    path: PathBuf,
    #[serde(default)]
    git_url: Option<String>,
    #[serde(default)]
    git_commit_id: Option<String>,
}

fn parse_dataset_ref(dataset: &str) -> (&str, Option<&str>) {
    dataset
        .rsplit_once('@')
        .map_or((dataset, None), |(name, version)| (name, Some(version)))
}

fn resolve_version<'a>(versions: impl Iterator<Item = &'a str>) -> String {
    let mut versions = versions.collect::<Vec<_>>();

    if versions.contains(&"head") {
        return "head".to_owned();
    }

    versions.sort_unstable();
    versions.last().copied().unwrap_or("head").to_owned()
}

fn resolve_task_path(registry_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        registry_root.join(path)
    }
}

fn resolve_registry_task_path(
    registry_root: &Path,
    task: &RegistryTask,
) -> Result<PathBuf, CliError> {
    match task.git_url.as_deref() {
        Some(git_url) => resolve_git_task_path(registry_root, task, git_url),
        None => Ok(resolve_task_path(registry_root, &task.path)),
    }
}

fn resolve_git_task_path(
    registry_root: &Path,
    task: &RegistryTask,
    git_url: &str,
) -> Result<PathBuf, CliError> {
    if task.path.is_absolute() {
        return Err(CliError::usage(
            "git-backed registry task paths must be relative to the repository root",
        ));
    }

    let git_source = resolve_git_source(registry_root, git_url);
    let revision = task.git_commit_id.as_deref().unwrap_or("HEAD");
    let checkout_dir = git_checkout_dir(&git_source, revision);

    ensure_git_checkout(&git_source, revision, &checkout_dir)?;

    Ok(checkout_dir.join(&task.path))
}

fn resolve_git_source(registry_root: &Path, git_url: &str) -> String {
    if git_url.contains("://") || git_url.starts_with("git@") || Path::new(git_url).is_absolute() {
        git_url.to_owned()
    } else {
        registry_root.join(git_url).to_string_lossy().into_owned()
    }
}

fn git_checkout_dir(git_source: &str, revision: &str) -> PathBuf {
    registry_cache_root().join(cache_key(&[git_source, revision]))
}

fn registry_cache_root() -> PathBuf {
    env::var_os("SEAPORT_REGISTRY_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join("seaport-registry-cache"))
}

fn ensure_git_checkout(
    git_source: &str,
    revision: &str,
    checkout_dir: &Path,
) -> Result<(), CliError> {
    if !checkout_dir.join(".git").is_dir() {
        if checkout_dir.exists() {
            fs::remove_dir_all(checkout_dir)?;
        }

        let parent = checkout_dir
            .parent()
            .ok_or_else(|| CliError::io("git cache path has no parent"))?;
        fs::create_dir_all(parent)?;

        let mut clone = Command::new("git");
        clone
            .args(["clone", "--quiet"])
            .arg(git_source)
            .arg(checkout_dir);
        run_git(clone, "clone git-backed registry task")?;
    }

    let mut checkout = Command::new("git");
    checkout
        .arg("-C")
        .arg(checkout_dir)
        .args(["checkout", "--quiet"])
        .arg(revision);
    run_git(checkout, "check out git-backed registry task")?;

    Ok(())
}

fn run_git(mut command: Command, action: &str) -> Result<(), CliError> {
    let output = command.output();

    match output {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(CliError::task_failed(format!(
            "{action} failed (status: {})\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(CliError::usage(
            "git-backed registry tasks require `git` on PATH",
        )),
        Err(error) => Err(CliError::io(error.to_string())),
    }
}

fn cache_key(values: &[&str]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;

    for value in values {
        for byte in value.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }

        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100000001b3);
    }

    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_git_backed_registry_task() {
        let root = temp_dir("git-registry-task");
        let repo = root.join("repo");
        let task = repo.join("tasks").join("one");
        let registry = root.join("registry.json");

        fs::create_dir_all(task.join("tests")).expect("tests dir");
        fs::write(task.join("instruction.md"), "Do the task.\n").expect("instruction");
        fs::write(
            task.join("task.toml"),
            "[task]\nname = \"acme/one\"\n\n[environment]\ndocker_image = \"ubuntu:24.04\"\n",
        )
        .expect("task toml");
        fs::write(task.join("tests").join("test.sh"), "#!/bin/bash\n").expect("test");

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
        let repo_url = repo.to_string_lossy();
        let checkout_dir = git_checkout_dir(&repo_url, &commit);

        fs::write(
            &registry,
            format!(
                "[{{\"name\":\"acme/suite\",\"version\":\"head\",\"tasks\":[{{\"path\":\"tasks/one\",\"git_url\":\"{}\",\"git_commit_id\":\"{}\"}}]}}]\n",
                json_escape(&repo_url),
                commit
            ),
        )
        .expect("registry");

        let resolved =
            resolve_local_registry_dataset("acme/suite@head", &registry).expect("dataset");

        assert_eq!(resolved.task_paths.len(), 1);
        assert!(resolved.task_paths[0].join("instruction.md").is_file());
        assert!(resolved.task_paths[0].starts_with(&checkout_dir));

        let _ = fs::remove_dir_all(checkout_dir);
        let _ = fs::remove_dir_all(root);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();

        env::temp_dir().join(format!("seaport-{name}-{id}"))
    }

    fn run_test_git<const N: usize>(cwd: &Path, args: [&str; N]) {
        let output = Command::new("git")
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
        let output = Command::new("git")
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

    fn json_escape(value: &str) -> String {
        value.replace('\\', "\\\\").replace('"', "\\\"")
    }
}
