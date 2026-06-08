use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=package.json");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let package_json_path = manifest_dir.join("package.json");
    let package_json = fs::read_to_string(&package_json_path).expect("read package.json");
    let package_json: serde_json::Value =
        serde_json::from_str(&package_json).expect("parse package.json");
    let version = package_json
        .get("version")
        .and_then(serde_json::Value::as_str)
        .expect("package.json version");

    println!("cargo:rustc-env=SEAPORT_VERSION={version}");
}
