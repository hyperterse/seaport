//! Thin helpers over a parsed `toml::Value` document.
//!
//! Seaport reads a handful of scalar fields out of `task.toml`/`dataset.toml`.
//! These helpers replace the previous hand-rolled line scanner with a real TOML
//! parser, so dotted sections, inline tables, arrays, and arrays-of-tables (used
//! by multi-step tasks) parse correctly while the downstream typed parsing and
//! validation stay unchanged. Callers read scalars as normalized strings and
//! parse/validate them exactly as before.

use crate::CliError;

/// Parses TOML text into a document, surfacing a usage error on malformed input.
pub(crate) fn parse(contents: &str) -> Result<toml::Value, CliError> {
    contents
        .parse::<toml::Value>()
        .map_err(|error| CliError::usage(format!("could not parse TOML: {error}")))
}

/// Reads `[section].key` (section may be dotted, e.g. `verifier.environment`) as
/// a normalized scalar string, or `None` when absent or not a scalar.
pub(crate) fn section_value(doc: &toml::Value, section: &str, key: &str) -> Option<String> {
    scalar_to_string(navigate(doc, section)?.get(key)?)
}

/// Reads a top-level (pre-section) scalar `key`, or `None` when absent or a table.
pub(crate) fn top_level_value(doc: &toml::Value, key: &str) -> Option<String> {
    scalar_to_string(doc.get(key)?)
}

/// Whether a (possibly dotted) `[section]` table is present. Distinguishes an
/// empty-but-present table from an absent one.
pub(crate) fn has_section(doc: &toml::Value, section: &str) -> bool {
    navigate(doc, section).is_some_and(toml::Value::is_table)
}

fn navigate<'a>(doc: &'a toml::Value, section: &str) -> Option<&'a toml::Value> {
    let mut current = doc;
    for part in section.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Renders a scalar TOML value the way the previous line scanner did: strings
/// verbatim, numbers/bools via their textual form. Non-scalars yield `None` so
/// callers fall through to their defaults.
fn scalar_to_string(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(value) => Some(value.clone()),
        toml::Value::Integer(value) => Some(value.to_string()),
        toml::Value::Float(value) => Some(value.to_string()),
        toml::Value::Boolean(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(contents: &str) -> toml::Value {
        parse(contents).expect("valid toml")
    }

    #[test]
    fn reads_section_scalars_of_each_type() {
        let doc = doc(r#"
[environment]
docker_image = "ubuntu:24.04" # trailing comment is ignored
cpus = 2
build_timeout_sec = 7.5
allow_internet = true
"#);

        assert_eq!(
            section_value(&doc, "environment", "docker_image").as_deref(),
            Some("ubuntu:24.04")
        );
        assert_eq!(
            section_value(&doc, "environment", "cpus").as_deref(),
            Some("2")
        );
        assert_eq!(
            section_value(&doc, "environment", "build_timeout_sec").as_deref(),
            Some("7.5")
        );
        assert_eq!(
            section_value(&doc, "environment", "allow_internet").as_deref(),
            Some("true")
        );
        assert_eq!(section_value(&doc, "environment", "missing"), None);
        assert_eq!(section_value(&doc, "absent", "docker_image"), None);
    }

    #[test]
    fn reads_top_level_scalar_but_not_tables() {
        let doc = doc(r#"
docker_image = "top-level:latest"

[environment]
cpus = 1
"#);

        assert_eq!(
            top_level_value(&doc, "docker_image").as_deref(),
            Some("top-level:latest")
        );
        // A section header is a table, not a scalar.
        assert_eq!(top_level_value(&doc, "environment"), None);
    }

    #[test]
    fn navigates_dotted_sections_and_detects_presence() {
        let doc = doc(r#"
[verifier]
environment_mode = "separate"

[verifier.environment]
docker_image = "verifier:latest"
"#);

        assert!(has_section(&doc, "verifier.environment"));
        assert!(!has_section(&doc, "agent.environment"));
        assert_eq!(
            section_value(&doc, "verifier.environment", "docker_image").as_deref(),
            Some("verifier:latest")
        );
        assert_eq!(
            section_value(&doc, "verifier", "environment_mode").as_deref(),
            Some("separate")
        );
    }

    #[test]
    fn parses_arrays_of_tables_that_the_line_scanner_could_not() {
        // Multi-step tasks use [[steps]]; the old scanner could not represent
        // these. They must at least parse without error now.
        let doc = doc(r#"
[[steps]]
name = "checkpoint_1"
artifacts = [{ source = "/app", exclude = [".git"] }]

[[steps]]
name = "checkpoint_2"
"#);

        let steps = doc
            .get("steps")
            .and_then(toml::Value::as_array)
            .expect("steps");
        assert_eq!(steps.len(), 2);
        assert_eq!(
            steps[0].get("name").and_then(toml::Value::as_str),
            Some("checkpoint_1")
        );
    }

    #[test]
    fn rejects_malformed_toml() {
        assert!(parse("this is = = not toml").is_err());
    }
}
