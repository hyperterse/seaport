use std::process::Command;

fn seaport() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_seaport"));
    // Keep the startup update check from touching the network or spawning a
    // background refresh during tests.
    command.env("SEAPORT_NO_UPDATE_CHECK", "1");
    command
}

#[test]
fn upgrade_help_describes_the_command() {
    let output = seaport()
        .args(["upgrade", "--help"])
        .output()
        .expect("run seaport upgrade --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("seaport upgrade [options]"));
    assert!(stdout.contains("--check"));
    assert!(stdout.contains("--force"));
}

#[test]
fn upgrade_rejects_unknown_options() {
    let output = seaport()
        .args(["upgrade", "--bogus"])
        .output()
        .expect("run seaport upgrade --bogus");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown upgrade option"));
}

#[test]
fn upgrade_version_flag_requires_a_value() {
    let output = seaport()
        .args(["upgrade", "--version"])
        .output()
        .expect("run seaport upgrade --version");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--version requires a value"));
}
