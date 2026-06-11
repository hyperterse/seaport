use std::fs;
use std::path::{Path, PathBuf};

use crate::registry::ResolvedRegistryDataset;
use crate::{task_name, validate_task_path, CliError, EXIT_USAGE};

#[derive(Debug)]
pub(crate) struct RunTarget {
    pub(crate) name: String,
    pub(crate) tasks: Vec<TaskRef>,
}

impl RunTarget {
    pub(crate) fn from_path(path: &Path, selection: &TaskSelection) -> Result<Self, CliError> {
        if path.join("task.toml").is_file() {
            let task = TaskRef::from_path(path)?;
            return Ok(Self {
                name: task.name.clone(),
                tasks: vec![task],
            });
        }

        Self::local_dataset(path, selection)
    }

    pub(crate) fn from_registry_dataset(
        dataset: ResolvedRegistryDataset,
        selection: &TaskSelection,
    ) -> Result<Self, CliError> {
        let mut tasks = dataset
            .task_paths
            .iter()
            .map(|path| TaskRef::from_path(path))
            .collect::<Result<Vec<_>, _>>()?;

        selection.apply(&mut tasks)?;

        if tasks.is_empty() {
            return Err(CliError::usage(
                "task filters removed every task from the registry dataset",
            ));
        }

        Ok(Self {
            name: dataset.name,
            tasks,
        })
    }

    fn local_dataset(path: &Path, selection: &TaskSelection) -> Result<Self, CliError> {
        if !path.is_dir() {
            return Err(CliError::usage(format!(
                "dataset path is not a directory: {}",
                path.display()
            )));
        }

        let mut tasks = Vec::new();

        for entry in sorted_child_dirs(path)? {
            if entry.join("task.toml").is_file() {
                match TaskRef::from_path(&entry) {
                    Ok(task) => tasks.push(task),
                    Err(error) if error.exit_code() == EXIT_USAGE => {}
                    Err(error) => return Err(error),
                }
            }
        }

        if tasks.is_empty() {
            return Err(CliError::usage(format!(
                "path is neither a task nor a local dataset with task subdirectories: {}",
                path.display()
            )));
        }

        selection.apply(&mut tasks)?;

        if tasks.is_empty() {
            return Err(CliError::usage(
                "task filters removed every task from the local dataset",
            ));
        }

        Ok(Self {
            name: dataset_name(path)?,
            tasks,
        })
    }
}

#[derive(Debug)]
pub(crate) struct TaskRef {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

impl TaskRef {
    fn from_path(path: &Path) -> Result<Self, CliError> {
        validate_task_path(path)?;

        let path = path.canonicalize()?;
        let name = task_name(&path)?;

        Ok(Self { name, path })
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TaskSelection {
    pub(crate) include_task_names: Vec<String>,
    pub(crate) exclude_task_names: Vec<String>,
    pub(crate) task_limit: Option<usize>,
}

impl TaskSelection {
    fn apply(&self, tasks: &mut Vec<TaskRef>) -> Result<(), CliError> {
        if !self.include_task_names.is_empty() {
            tasks.retain(|task| {
                self.include_task_names
                    .iter()
                    .any(|pattern| glob_matches(pattern, &task.name))
            });
        }

        if !self.exclude_task_names.is_empty() {
            tasks.retain(|task| {
                !self
                    .exclude_task_names
                    .iter()
                    .any(|pattern| glob_matches(pattern, &task.name))
            });
        }

        if let Some(limit) = self.task_limit {
            if limit == 0 {
                return Err(CliError::usage("-l/--n-tasks must be greater than zero"));
            }

            tasks.truncate(limit);
        }

        Ok(())
    }
}

fn sorted_child_dirs(path: &Path) -> Result<Vec<PathBuf>, CliError> {
    let mut dirs = Vec::new();

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();

        if entry_path.is_dir() {
            dirs.push(entry_path);
        }
    }

    dirs.sort();

    Ok(dirs)
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    fn inner(pattern: &[u8], value: &[u8]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some((&b'*', rest)) => {
                inner(rest, value) || (!value.is_empty() && inner(pattern, &value[1..]))
            }
            Some((&b'?', rest)) => !value.is_empty() && inner(rest, &value[1..]),
            Some((&expected, rest)) => value
                .split_first()
                .is_some_and(|(&actual, value_rest)| actual == expected && inner(rest, value_rest)),
        }
    }

    inner(pattern.as_bytes(), value.as_bytes())
}

fn dataset_name(path: &Path) -> Result<String, CliError> {
    let manifest_path = path.join("dataset.toml");

    if manifest_path.is_file() {
        let manifest = fs::read_to_string(manifest_path)?;

        // Best-effort: a malformed manifest falls back to the directory name
        // rather than failing dataset resolution.
        if let Some(name) = crate::toml_doc::parse(&manifest)
            .ok()
            .and_then(|doc| crate::toml_doc::section_value(&doc, "dataset", "name"))
        {
            return Ok(name);
        }
    }

    Ok(path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dataset")
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matching_supports_harbor_task_filters() {
        assert!(glob_matches("swe-bench/*", "swe-bench/django-123"));
        assert!(glob_matches("*/django-?", "swe-bench/django-1"));
        assert!(!glob_matches("*/django-?", "swe-bench/django-12"));
    }
}
