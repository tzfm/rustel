//! Test the fork's private Unix parser on every host without vendoring it.

use std::{fs, path::Path, process::Command};

#[test]
fn resolved_crossterm_decoder_regressions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cargo = env!("CARGO");
    let rustc = Command::new("rustc").arg("-vV").output().unwrap();
    assert!(rustc.status.success(), "rustc -vV failed");
    let version = String::from_utf8(rustc.stdout).unwrap();
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("rustc host triple");
    let metadata = Command::new(cargo)
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--filter-platform",
            host,
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        metadata.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout).unwrap();
    let packages: Vec<_> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|package| package["name"] == "crossterm")
        .collect();
    assert_eq!(
        packages.len(),
        1,
        "expected one resolved Crossterm dependency"
    );
    let dependency = Path::new(packages[0]["manifest_path"].as_str().unwrap())
        .parent()
        .unwrap();
    let parser = dependency.join("src/event/sys/unix/parse.rs");
    assert!(
        parser.is_file(),
        "Crossterm parser moved: {}",
        parser.display()
    );
    eprintln!("Testing {}", packages[0]["id"]);

    let project = tempfile::tempdir().unwrap();
    fs::create_dir(project.path().join("src")).unwrap();
    fs::write(
        project.path().join("src/lib.rs"),
        include_str!("support/terminal_key_decoder.rs"),
    )
    .unwrap();
    fs::copy(root.join("Cargo.lock"), project.path().join("Cargo.lock")).unwrap();
    // JSON strings also quote Windows paths correctly in TOML basic strings.
    let dependency = serde_json::to_string(dependency.to_str().unwrap()).unwrap();
    fs::write(project.path().join("Cargo.toml"), format!(
        "[package]\nname = \"rustel-decoder-check\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\ncrossterm = {{ path = {dependency} }}\n"
    )).unwrap();
    let status = Command::new(cargo)
        .args(["test", "--offline", "-j", "1", "--manifest-path"])
        .arg(project.path().join("Cargo.toml"))
        .env("RUSTEL_CROSSTERM_PARSER", parser)
        // A separate target avoids locking the parent Cargo test's target.
        .env("CARGO_TARGET_DIR", project.path().join("target"))
        .status()
        .unwrap();
    assert!(status.success(), "Crossterm decoder regressions failed");
}
