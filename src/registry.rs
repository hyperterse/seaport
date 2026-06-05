use std::env;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::CliError;

const DEFAULT_JSON_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/laude-institute/harbor/main/registry.json";
const PACKAGE_REGISTRY_URL: &str = "https://ofhuhcpkvzjlejydnvyd.supabase.co";
const PACKAGE_REGISTRY_KEY: &str = "sb_publishable_Z-vuQbpvpG-PStjbh4yE0Q_e-d3MTIH";
const PACKAGE_TASK_PAGE_SIZE: usize = 1000;
const PACKAGE_BUCKET: &str = "packages";
const DATASET_VERSION_TAG_SELECT: &str =
    "dataset_version:dataset_version_id(*),package:package_id!inner(name,type,org:org_id!inner(name))";
const DATASET_VERSION_SELECT: &str = "*,package:package_id!inner(name,type,org:org_id!inner(name))";
const DATASET_TASK_SELECT: &str =
    "task_version:task_version_id(content_hash,archive_path,package:package_id(name,org:org_id(name)))";

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

    resolve_json_registry_dataset(
        dataset,
        registry,
        registry_root,
        &registry_path.display().to_string(),
    )
}

pub(crate) fn resolve_remote_registry_dataset(
    dataset: &str,
    registry_url: Option<&str>,
) -> Result<ResolvedRegistryDataset, CliError> {
    if use_package_registry(registry_url) {
        return resolve_package_dataset(dataset);
    }

    let registry_url = registry_url.expect("custom registry URL");
    let registry = RegistryFile::from_url(registry_url)?;
    let registry_root = url_registry_root(registry_url);

    resolve_json_registry_dataset(dataset, registry, &registry_root, registry_url)
}

fn resolve_json_registry_dataset(
    dataset: &str,
    registry: RegistryFile,
    registry_root: &Path,
    registry_label: &str,
) -> Result<ResolvedRegistryDataset, CliError> {
    let (dataset_name, requested_version) = parse_dataset_ref(dataset);
    let specs = registry
        .datasets()
        .iter()
        .filter(|spec| spec.name == dataset_name)
        .collect::<Vec<_>>();

    if specs.is_empty() {
        return Err(CliError::usage(format!(
            "dataset `{dataset_name}` was not found in {}",
            registry_label
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
                registry_label
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

pub(crate) fn resolve_local_registry_task(
    task_name: &str,
    registry_path: &Path,
) -> Result<ResolvedRegistryDataset, CliError> {
    let registry_root = registry_path.parent().unwrap_or_else(|| Path::new("."));
    let registry = RegistryFile::from_path(registry_path)?;

    resolve_json_registry_task(
        task_name,
        registry,
        registry_root,
        &registry_path.display().to_string(),
    )
}

pub(crate) fn resolve_remote_registry_task(
    task_name: &str,
    registry_url: Option<&str>,
) -> Result<ResolvedRegistryDataset, CliError> {
    if use_package_registry(registry_url) {
        return resolve_package_task(task_name);
    }

    let registry_url = registry_url.expect("custom registry URL");
    let registry = RegistryFile::from_url(registry_url)?;
    let registry_root = url_registry_root(registry_url);

    resolve_json_registry_task(task_name, registry, &registry_root, registry_url)
}

fn resolve_json_registry_task(
    task_name: &str,
    registry: RegistryFile,
    registry_root: &Path,
    registry_label: &str,
) -> Result<ResolvedRegistryDataset, CliError> {
    for dataset in registry.datasets() {
        for task in &dataset.tasks {
            if registry_task_matches(task_name, task) {
                return Ok(ResolvedRegistryDataset {
                    name: task_name.to_owned(),
                    task_paths: vec![resolve_registry_task_path(registry_root, task)?],
                });
            }
        }
    }

    Err(CliError::usage(format!(
        "task `{task_name}` was not found in {}",
        registry_label
    )))
}

pub(crate) fn resolve_git_task_source(
    git_url: &str,
    git_commit_id: Option<&str>,
    path: &Path,
) -> Result<PathBuf, CliError> {
    let task = RegistryTask {
        name: None,
        path: path.to_path_buf(),
        git_url: Some(git_url.to_owned()),
        git_commit_id: git_commit_id.map(str::to_owned),
    };

    resolve_git_task_path(Path::new("."), &task, git_url)
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

    fn from_url(url: &str) -> Result<Self, CliError> {
        let contents = read_url_to_string(url)?;
        let registry = serde_json::from_str::<Self>(&contents).map_err(|error| {
            CliError::usage(format!("could not parse registry JSON {url}: {error}"))
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
    #[serde(default)]
    name: Option<String>,
    path: PathBuf,
    #[serde(default)]
    git_url: Option<String>,
    #[serde(default)]
    git_commit_id: Option<String>,
}

#[derive(Debug)]
struct PackageReference {
    org: String,
    name: String,
    reference: String,
}

#[derive(Debug, Deserialize)]
struct DatasetVersionTagRow {
    dataset_version: DatasetVersion,
}

#[derive(Debug, Deserialize)]
struct DatasetVersion {
    id: String,
}

#[derive(Debug, Deserialize)]
struct DatasetVersionTaskRow {
    task_version: PackageTaskVersion,
}

#[derive(Debug, Deserialize)]
struct PackageTaskVersion {
    archive_path: String,
    content_hash: String,
    package: PackageInfo,
}

#[derive(Debug, Deserialize)]
struct ResolvedPackageTaskVersion {
    archive_path: String,
    content_hash: String,
}

#[derive(Debug, Deserialize)]
struct PackageInfo {
    name: String,
    org: PackageOrg,
}

#[derive(Debug, Deserialize)]
struct PackageOrg {
    name: String,
}

fn parse_dataset_ref(dataset: &str) -> (&str, Option<&str>) {
    dataset
        .rsplit_once('@')
        .map_or((dataset, None), |(name, version)| (name, Some(version)))
}

fn use_package_registry(registry_url: Option<&str>) -> bool {
    registry_url.is_none() || registry_url == Some(DEFAULT_JSON_REGISTRY_URL)
}

fn resolve_package_dataset(dataset: &str) -> Result<ResolvedRegistryDataset, CliError> {
    let reference = PackageReference::parse(dataset)?;
    let dataset_version = resolve_package_dataset_version(&reference)?;
    let task_versions = package_dataset_tasks(&dataset_version.id)?;

    if task_versions.is_empty() {
        return Err(CliError::usage(format!(
            "dataset `{}` has no package tasks",
            reference.display_name()
        )));
    }

    let mut task_paths = Vec::with_capacity(task_versions.len());

    for task in &task_versions {
        task_paths.push(download_package_task(task)?);
    }

    Ok(ResolvedRegistryDataset {
        name: reference.display_name(),
        task_paths,
    })
}

fn resolve_package_task(task_name: &str) -> Result<ResolvedRegistryDataset, CliError> {
    let reference = PackageReference::parse(task_name)?;
    let task_version = resolve_package_task_version(&reference)?.ok_or_else(|| {
        CliError::usage(format!(
            "task `{}` was not found in the package registry",
            reference.display_with_ref()
        ))
    })?;

    Ok(ResolvedRegistryDataset {
        name: reference.display_name(),
        task_paths: vec![download_package_task(&task_version)?],
    })
}

impl PackageReference {
    fn parse(value: &str) -> Result<Self, CliError> {
        let (name, reference) = value.rsplit_once('@').unwrap_or((value, "latest"));
        let (org, short_name) = name.split_once('/').ok_or_else(|| {
            CliError::usage(format!(
                "package registry references must use `org/name`, got `{value}`"
            ))
        })?;

        if org.is_empty() || short_name.is_empty() {
            return Err(CliError::usage(format!(
                "package registry references must use `org/name`, got `{value}`"
            )));
        }

        Ok(Self {
            org: org.to_owned(),
            name: short_name.to_owned(),
            reference: if reference.is_empty() {
                "latest".to_owned()
            } else {
                reference.to_owned()
            },
        })
    }

    fn display_name(&self) -> String {
        format!("{}/{}", self.org, self.name)
    }

    fn display_with_ref(&self) -> String {
        format!("{}@{}", self.display_name(), self.reference)
    }
}

fn resolve_package_dataset_version(
    reference: &PackageReference,
) -> Result<DatasetVersion, CliError> {
    match VersionReference::parse(&reference.reference) {
        VersionReference::Tag(tag) => {
            let rows = package_get_json::<Vec<DatasetVersionTagRow>>(&format!(
                "dataset_version_tag?select={DATASET_VERSION_TAG_SELECT}&tag=eq.{}&{}&limit=1",
                url_component(&tag),
                package_dataset_filter(reference)
            ))?;

            rows.into_iter().next().map(|row| row.dataset_version)
        }
        VersionReference::Revision(revision) => {
            package_dataset_version_by_query(reference, &format!("revision=eq.{revision}"))?
        }
        VersionReference::Digest(digest) => package_dataset_version_by_query(
            reference,
            &format!("content_hash=eq.{}", url_component(&digest)),
        )?,
    }
    .ok_or_else(|| {
        CliError::usage(format!(
            "dataset `{}` was not found in the package registry",
            reference.display_with_ref()
        ))
    })
}

fn package_dataset_version_by_query(
    reference: &PackageReference,
    version_filter: &str,
) -> Result<Option<DatasetVersion>, CliError> {
    let rows = package_get_json::<Vec<DatasetVersion>>(&format!(
        "dataset_version?select={DATASET_VERSION_SELECT}&{}&{}&limit=1",
        version_filter,
        package_dataset_filter(reference)
    ))?;

    Ok(rows.into_iter().next())
}

fn package_dataset_tasks(dataset_version_id: &str) -> Result<Vec<PackageTaskVersion>, CliError> {
    let mut tasks = Vec::new();
    let mut offset = 0;

    loop {
        let rows = package_get_json::<Vec<DatasetVersionTaskRow>>(&format!(
            "dataset_version_task?select={DATASET_TASK_SELECT}&dataset_version_id=eq.{}&order=task_version_id&limit={PACKAGE_TASK_PAGE_SIZE}&offset={offset}",
            url_component(dataset_version_id)
        ))?;
        let row_count = rows.len();

        tasks.extend(rows.into_iter().map(|row| row.task_version));

        if row_count < PACKAGE_TASK_PAGE_SIZE {
            break;
        }

        offset += PACKAGE_TASK_PAGE_SIZE;
    }

    Ok(tasks)
}

fn resolve_package_task_version(
    reference: &PackageReference,
) -> Result<Option<PackageTaskVersion>, CliError> {
    let body = serde_json::json!({
        "p_org": reference.org,
        "p_name": reference.name,
        "p_ref": reference.reference,
    })
    .to_string();

    let resolved =
        package_post_json::<Option<ResolvedPackageTaskVersion>>("resolve_task_version", &body)?;

    Ok(resolved.map(|task| PackageTaskVersion {
        archive_path: task.archive_path,
        content_hash: task.content_hash,
        package: PackageInfo {
            name: reference.name.clone(),
            org: PackageOrg {
                name: reference.org.clone(),
            },
        },
    }))
}

fn download_package_task(task: &PackageTaskVersion) -> Result<PathBuf, CliError> {
    let target_dir = package_task_cache_dir(task);

    if target_dir.join("task.toml").is_file() {
        return Ok(target_dir);
    }

    let parent = target_dir
        .parent()
        .ok_or_else(|| CliError::io("package task cache path has no parent"))?;
    fs::create_dir_all(parent)?;

    let staging = parent.join(format!(
        ".{}-{}.tmp",
        target_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("task"),
        std::process::id()
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;

    let archive_path = staging.join("archive.harbor");
    let unpack_dir = staging.join("unpacked");
    fs::create_dir_all(&unpack_dir)?;

    let result = (|| {
        download_package_archive(&task.archive_path, &archive_path)?;
        validate_archive_paths(&archive_path)?;
        extract_archive(&archive_path, &unpack_dir)?;

        if !unpack_dir.join("task.toml").is_file() {
            return Err(CliError::usage(format!(
                "package task `{}/{}` did not unpack to a task root",
                task.package.org.name, task.package.name
            )));
        }

        if target_dir.exists() {
            fs::remove_dir_all(&target_dir)?;
        }
        fs::rename(&unpack_dir, &target_dir)?;

        Ok(target_dir.clone())
    })();

    if let Err(error) = fs::remove_dir_all(&staging) {
        eprintln!(
            "seaport: warning: could not remove package staging directory {}: {error}",
            staging.display()
        );
    }

    result
}

fn package_task_cache_dir(task: &PackageTaskVersion) -> PathBuf {
    package_cache_root()
        .join(&task.package.org.name)
        .join(&task.package.name)
        .join(normalized_digest(&task.content_hash))
}

fn normalized_digest(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("sha256:")
        .to_ascii_lowercase()
}

fn package_cache_root() -> PathBuf {
    env::var_os("SEAPORT_PACKAGE_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| seaport_cache_root().join("tasks").join("packages"))
}

fn seaport_cache_root() -> PathBuf {
    env::var_os("SEAPORT_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/seaport")))
        .unwrap_or_else(|| env::temp_dir().join("seaport-cache"))
}

enum VersionReference {
    Tag(String),
    Revision(String),
    Digest(String),
}

impl VersionReference {
    fn parse(value: &str) -> Self {
        if value.is_empty() || value == "latest" {
            return Self::Tag("latest".to_owned());
        }

        if value.chars().all(|character| character.is_ascii_digit()) {
            return Self::Revision(value.to_owned());
        }

        if let Some(digest) = value.strip_prefix("sha256:") {
            return Self::Digest(digest.to_owned());
        }

        Self::Tag(value.to_owned())
    }
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

fn registry_task_matches(task_name: &str, task: &RegistryTask) -> bool {
    task.name.as_deref() == Some(task_name)
        || task
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == task_name.rsplit('/').next().unwrap_or(task_name))
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

fn read_url_to_string(url: &str) -> Result<String, CliError> {
    if let Some(path) = url.strip_prefix("file://") {
        return Ok(fs::read_to_string(path)?);
    }

    let mut command = curl_base_command(url);
    let output = run_command_output(&mut command, "fetch registry URL")?;

    String::from_utf8(output.stdout)
        .map_err(|error| CliError::usage(format!("registry response was not UTF-8: {error}")))
}

fn package_get_json<T>(path_and_query: &str) -> Result<T, CliError>
where
    T: for<'de> Deserialize<'de>,
{
    let url = format!(
        "{}/rest/v1/{}",
        package_registry_url().trim_end_matches('/'),
        path_and_query
    );
    let body = package_get_text(&url)?;

    serde_json::from_str(&body).map_err(|error| {
        CliError::usage(format!(
            "could not parse package registry response: {error}"
        ))
    })
}

fn package_post_json<T>(rpc_name: &str, body: &str) -> Result<T, CliError>
where
    T: for<'de> Deserialize<'de>,
{
    let url = format!(
        "{}/rest/v1/rpc/{}",
        package_registry_url().trim_end_matches('/'),
        rpc_name
    );
    let mut command = curl_base_command(&url);
    add_package_headers(&mut command);
    command.args(["-H", "Content-Type: application/json", "-d", body]);
    let output = run_command_output(&mut command, "query package registry")?;
    let body = String::from_utf8(output.stdout).map_err(|error| {
        CliError::usage(format!("package registry response was not UTF-8: {error}"))
    })?;

    serde_json::from_str(&body).map_err(|error| {
        CliError::usage(format!(
            "could not parse package registry response: {error}"
        ))
    })
}

fn package_get_text(url: &str) -> Result<String, CliError> {
    let mut command = curl_base_command(url);
    add_package_headers(&mut command);
    let output = run_command_output(&mut command, "query package registry")?;

    String::from_utf8(output.stdout).map_err(|error| {
        CliError::usage(format!("package registry response was not UTF-8: {error}"))
    })
}

fn download_package_archive(remote_path: &str, archive_path: &Path) -> Result<(), CliError> {
    let url = format!(
        "{}/storage/v1/object/{}/{}",
        package_registry_url().trim_end_matches('/'),
        PACKAGE_BUCKET,
        remote_path
    );
    let mut command = curl_base_command(&url);
    add_package_headers(&mut command);
    command.arg("-o").arg(archive_path);
    run_command_status(&mut command, "download package archive")
}

fn package_registry_url() -> String {
    env::var("SEAPORT_PACKAGE_REGISTRY_URL").unwrap_or_else(|_| PACKAGE_REGISTRY_URL.to_owned())
}

fn package_registry_key() -> String {
    env::var("SEAPORT_PACKAGE_REGISTRY_KEY").unwrap_or_else(|_| PACKAGE_REGISTRY_KEY.to_owned())
}

fn add_package_headers(command: &mut Command) {
    let key = package_registry_key();
    let api_key_header = format!("apikey: {key}");
    let bearer_header = format!("Authorization: Bearer {key}");
    command
        .args(["-H", api_key_header.as_str()])
        .args(["-H", bearer_header.as_str()]);
}

fn curl_base_command(url: &str) -> Command {
    let mut command = Command::new("curl");
    command.args(["-fsSL", "--retry", "3", "--retry-delay", "1", url]);
    command
}

fn validate_archive_paths(archive_path: &Path) -> Result<(), CliError> {
    let mut command = Command::new("tar");
    command.arg("-tzf").arg(archive_path);
    let output = run_command_output(&mut command, "inspect package archive")?;
    let entries = String::from_utf8(output.stdout).map_err(|error| {
        CliError::usage(format!("package archive listing was not UTF-8: {error}"))
    })?;

    for entry in entries.lines() {
        let path = Path::new(entry);

        if path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(CliError::usage(format!(
                "package archive contains unsafe path `{entry}`"
            )));
        }
    }

    Ok(())
}

fn extract_archive(archive_path: &Path, destination: &Path) -> Result<(), CliError> {
    let mut command = Command::new("tar");
    command
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(destination);
    run_command_status(&mut command, "extract package archive")
}

fn run_command_status(command: &mut Command, action: &str) -> Result<(), CliError> {
    run_command_output(command, action).map(|_| ())
}

fn run_command_output(
    command: &mut Command,
    action: &str,
) -> Result<std::process::Output, CliError> {
    let output = command.output();

    match output {
        Ok(output) if output.status.success() => Ok(output),
        Ok(output) => Err(CliError::task_failed(format!(
            "{action} failed (status: {})\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(CliError::usage(format!(
            "{action} requires `{}` on PATH",
            command.get_program().to_string_lossy()
        ))),
        Err(error) => Err(CliError::io(error.to_string())),
    }
}

fn url_registry_root(url: &str) -> PathBuf {
    url.strip_prefix("file://")
        .and_then(|path| Path::new(path).parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn url_component(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

fn package_dataset_filter(reference: &PackageReference) -> String {
    format!(
        "package.name=eq.{}&package.type=eq.dataset&package.org.name=eq.{}",
        url_component(&reference.name),
        url_component(&reference.org)
    )
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
    fn resolves_remote_json_registry_from_file_url() {
        let root = temp_dir("remote-json-registry");
        let task_dir = root.join("tasks").join("demo-task");
        create_task(&task_dir, "acme/demo-task");

        let registry_path = root.join("registry.json");
        fs::write(
            &registry_path,
            r#"{"datasets":[{"name":"acme/demo","version":"1.0","tasks":[{"name":"acme/demo-task","path":"tasks/demo-task"}]}]}"#,
        )
        .expect("write registry");

        let resolved = resolve_remote_registry_dataset(
            "acme/demo",
            Some(&format!("file://{}", registry_path.display())),
        )
        .expect("resolved dataset");

        assert_eq!(resolved.name, "acme/demo");
        assert_eq!(resolved.task_paths, vec![task_dir]);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parses_package_references_with_default_latest_ref() {
        let reference =
            PackageReference::parse("terminal-bench/terminal-bench-2").expect("package reference");

        assert_eq!(reference.org, "terminal-bench");
        assert_eq!(reference.name, "terminal-bench-2");
        assert_eq!(reference.reference, "latest");
    }

    #[test]
    fn rejects_package_references_without_org() {
        let error = PackageReference::parse("terminal-bench-2").expect_err("error");

        assert_eq!(error.exit_code(), crate::EXIT_USAGE);
    }

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

    fn create_task(path: &Path, name: &str) {
        fs::create_dir_all(path.join("tests")).expect("tests dir");
        fs::write(path.join("instruction.md"), "Do the task.\n").expect("instruction");
        fs::write(
            path.join("task.toml"),
            format!("[task]\nname = \"{name}\"\n"),
        )
        .expect("task toml");
        fs::write(path.join("tests").join("test.sh"), "#!/bin/sh\n").expect("test");
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
