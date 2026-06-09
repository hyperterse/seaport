use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::CliError;

/// GitHub repository the released binaries are published under.
const REPO: &str = "hyperterse/seaport";
/// Canonical installer URL; redirects to the raw `install.sh` in `main`.
const INSTALL_URL: &str = "https://seaport.run/install";
/// How long a recorded update check stays fresh before we refresh it.
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Cached result of the most recent background update check.
#[derive(Debug, Default, Serialize, Deserialize)]
struct UpdateCache {
    /// Unix timestamp (seconds) of the last completed check.
    checked_at: u64,
    /// Latest released version observed, without a leading `v`.
    latest: Option<String>,
}

/// Print a one-line notice to stderr when a newer release is available, and
/// refresh the cached version in a detached background process when the cache
/// has gone stale. This never blocks on the network and never fails the CLI:
/// the comparison is served from cache so normal commands stay fast.
pub fn notify_if_outdated(current: &str) {
    if env::var_os("SEAPORT_NO_UPDATE_CHECK").is_some() {
        return;
    }

    // Only nag on an interactive terminal; scripts, pipes, and CI stay quiet.
    if !io::stderr().is_terminal() {
        return;
    }

    let cache = read_cache();

    if let Some(latest) = cache.as_ref().and_then(|cache| cache.latest.as_deref()) {
        if is_newer(latest, current) {
            print_notice(current, latest);
        }
    }

    let fresh = cache
        .as_ref()
        .is_some_and(|cache| now().saturating_sub(cache.checked_at) < CHECK_INTERVAL_SECS);

    if !fresh {
        spawn_background_refresh();
    }
}

/// Body of the hidden `__update-check` command: fetch the latest version and
/// persist it to the cache. Runs detached and silent; failures are swallowed so
/// a flaky network never leaves noise behind.
pub fn refresh_cache() {
    let cache = UpdateCache {
        checked_at: now(),
        latest: fetch_latest_version(),
    };

    let _ = write_cache(&cache);
}

/// `seaport upgrade` — re-run the installer to fetch the latest release.
pub fn run(args: &[String], current: &str) -> Result<(), CliError> {
    let mut check_only = false;
    let mut force = false;
    let mut requested_version: Option<String> = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => {
                print_upgrade_help();
                return Ok(());
            }
            "--check" => {
                check_only = true;
                index += 1;
            }
            "--force" => {
                force = true;
                index += 1;
            }
            "--version" => {
                requested_version = Some(
                    args.get(index + 1)
                        .cloned()
                        .ok_or_else(|| CliError::usage("--version requires a value"))?
                        .trim_start_matches('v')
                        .to_owned(),
                );
                index += 2;
            }
            unknown => {
                return Err(CliError::usage(format!(
                    "unknown upgrade option `{unknown}`"
                )));
            }
        }
    }

    let latest = fetch_latest_version();

    println!("seaport upgrade");
    println!("  current: {current}");
    match latest.as_deref() {
        Some(latest) => println!("  latest:  {latest}"),
        None => println!("  latest:  (unknown)"),
    }

    // We are up to date only when we resolved a latest version, the user did
    // not pin a specific version, and the current build is not older than it.
    let up_to_date = requested_version.is_none()
        && latest
            .as_deref()
            .is_some_and(|latest| !is_newer(latest, current));

    if check_only {
        match latest.as_deref() {
            Some(latest) if is_newer(latest, current) => {
                println!("A newer version is available ({latest}). Run `seaport upgrade`.");
            }
            Some(_) => println!("seaport is up to date."),
            None => println!("Could not determine the latest version."),
        }
        return Ok(());
    }

    if up_to_date && !force {
        println!("Already on the latest version ({current}). Use --force to reinstall.");
        return Ok(());
    }

    println!();
    run_installer(requested_version.as_deref())?;

    // Record success so the startup notice stops firing without waiting for the
    // next background refresh.
    let _ = write_cache(&UpdateCache {
        checked_at: now(),
        latest: requested_version.or(latest),
    });

    println!();
    println!("Upgrade complete. Run `seaport --version` to confirm.");

    Ok(())
}

pub fn print_upgrade_help() {
    println!(
        "\
Usage:
  seaport upgrade [options]

Re-runs the Seaport installer to download and install the latest release.

Options:
      --check             Report whether a newer version is available, without installing
      --force             Reinstall even if already on the latest version
      --version <ver>     Install a specific version (without the leading `v`)
  -h, --help              Show this help

Environment:
  INSTALL_DIR             Install directory; defaults to ~/.local/bin
  SEAPORT_NO_UPDATE_CHECK Set to disable the automatic update check on startup"
    );
}

fn run_installer(version: Option<&str>) -> Result<(), CliError> {
    if cfg!(windows) {
        return Err(CliError::unimplemented(format!(
            "automatic upgrade is not supported on Windows yet; reinstall from {INSTALL_URL}"
        )));
    }

    let install_url = env::var("SEAPORT_INSTALL_URL").unwrap_or_else(|_| INSTALL_URL.to_owned());

    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(format!("set -e; curl -fsSL '{install_url}' | bash"))
        // Skip the installer's interactive confirmation; this is a deliberate upgrade.
        .env("SKIP_INSTALL_PROMPT", "1");

    if let Some(version) = version {
        command.env("VERSION", version);
    }

    let status = command.status().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            CliError::usage("upgrade requires `bash` and `curl` on PATH")
        } else {
            CliError::io(error.to_string())
        }
    })?;

    if !status.success() {
        return Err(CliError::task_failed(format!(
            "installer exited with status {status}"
        )));
    }

    Ok(())
}

fn fetch_latest_version() -> Option<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "5",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: seaport-cli",
            &url,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let body = String::from_utf8(output.stdout).ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tag = json.get("tag_name")?.as_str()?;
    let version = tag.trim_start_matches('v').trim();

    if version.is_empty() {
        None
    } else {
        Some(version.to_owned())
    }
}

fn spawn_background_refresh() {
    let Ok(exe) = env::current_exe() else {
        return;
    };

    // Detach the refresh so it outlives this process and never writes to our
    // streams. We do not wait on it.
    let _ = Command::new(exe)
        .arg("__update-check")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn print_notice(current: &str, latest: &str) {
    let _ = writeln!(io::stderr());
    let _ = writeln!(
        io::stderr(),
        "{}",
        yellow(&format!(
            "A new version of seaport is available: {current} -> {latest}"
        ))
    );
    let _ = writeln!(io::stderr(), "{}", dim("Run `seaport upgrade` to update."));
}

fn read_cache() -> Option<UpdateCache> {
    let path = cache_path()?;
    let contents = fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn write_cache(cache: &UpdateCache) -> io::Result<()> {
    let Some(path) = cache_path() else {
        return Ok(());
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let body = serde_json::to_string(cache).map_err(io::Error::other)?;
    fs::write(path, body)
}

fn cache_path() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("SEAPORT_CACHE_DIR") {
        return Some(PathBuf::from(dir).join("update-check.json"));
    }

    let base = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;

    Some(base.join("seaport").join("update-check.json"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Returns true when `candidate` is a strictly newer release than `current`.
/// Compares the dotted numeric core; any pre-release/build suffix is ignored,
/// which is sufficient for the `x.y.z` tags we publish.
fn is_newer(candidate: &str, current: &str) -> bool {
    let candidate = version_components(candidate);
    let current = version_components(current);
    let len = candidate.len().max(current.len());

    for index in 0..len {
        let left = candidate.get(index).copied().unwrap_or(0);
        let right = current.get(index).copied().unwrap_or(0);

        if left != right {
            return left > right;
        }
    }

    false
}

fn version_components(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or("")
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn color_enabled() -> bool {
    io::stderr().is_terminal()
        && env::var_os("NO_COLOR").is_none()
        && env::var_os("SEAPORT_NO_COLOR").is_none()
}

fn yellow(text: &str) -> String {
    ansi("1;33", text)
}

fn dim(text: &str) -> String {
    ansi("2", text)
}

fn ansi(code: &str, text: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_newer_versions() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.2.1", "0.2"));
    }

    #[test]
    fn rejects_same_or_older_versions() {
        assert!(!is_newer("0.2.0", "0.2.0"));
        // Trailing zeros are equal, not newer.
        assert!(!is_newer("0.2.0", "0.2"));
        assert!(!is_newer("0.2.0", "0.3.0"));
        assert!(!is_newer("0.2.0", "0.2.1"));
        assert!(!is_newer("0.9.9", "1.0.0"));
    }

    #[test]
    fn ignores_leading_v_and_suffixes() {
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(!is_newer("0.2.0-rc.1", "0.2.0"));
        assert_eq!(version_components("v1.2.3-beta+build"), vec![1, 2, 3]);
    }

    #[test]
    fn cache_round_trips() {
        let cache = UpdateCache {
            checked_at: 42,
            latest: Some("9.9.9".to_owned()),
        };
        let body = serde_json::to_string(&cache).expect("serialize");
        let parsed: UpdateCache = serde_json::from_str(&body).expect("deserialize");

        assert_eq!(parsed.checked_at, 42);
        assert_eq!(parsed.latest.as_deref(), Some("9.9.9"));
    }
}
