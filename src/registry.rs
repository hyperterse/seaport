use std::path::{Path, PathBuf};

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
        if task.git_url.is_some() {
            return Err(CliError::unimplemented(
                "git-backed registry tasks are not implemented yet",
            ));
        }

        task_paths.push(resolve_task_path(registry_root, &task.path));
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
