use std::process::Command;

#[test]
fn cli_version_flags_report_package_version() {
    let expected = format!("seaport {}\n", env!("SEAPORT_VERSION"));

    for flag in ["--version", "-v"] {
        let output = Command::new(env!("CARGO_BIN_EXE_seaport"))
            .arg(flag)
            .output()
            .expect("run seaport version flag");

        assert!(
            output.status.success(),
            "{flag} failed with status {:?}",
            output.status.code()
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
        assert!(output.stderr.is_empty());
    }
}
