/*
rustel-core - production graph layering
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Keep QuickJS handles and higher runtime layers out of pattern nodes.
//! A non-Send mutation checks the private Node assertion; the manifest check
//! guards dependencies. Adding rquickjs here would violate that boundary.

use std::process::Command;

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

// Isolate mutation artifacts so this check does not invalidate ordinary builds.
fn compile_core(extra_rustflags: &str) -> (bool, String) {
    let root = workspace_root();
    let target = root.join("target/node-send-sync-mutation");
    let out = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(["build", "-p", "rustel-core", "--quiet"])
        .env("RUSTFLAGS", extra_rustflags)
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .expect("run cargo");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_non_send_payload_in_the_real_node_fails_to_compile() {
    // Prove that an unrelated build failure cannot make the mutation test pass.
    let (ok, stderr) = compile_core("");
    assert!(
        ok,
        "rustel-core must build without the mutation; stderr:\n{stderr}"
    );

    // Rc has the same Send/Sync restriction as a QuickJS handle.
    let (ok, stderr) = compile_core("--cfg non_send_node_mutation");
    assert!(
        !ok,
        "a `!Send + !Sync` payload in the production `Node` compiled cleanly. \
         The Send + Sync assertion is not load-bearing, and a QuickJS handle \
         could be stored in a pattern node unnoticed."
    );
    assert!(
        stderr.contains("cannot be sent between threads safely")
            || stderr.contains("cannot be shared between threads safely"),
        "the mutation failed to compile, but not on a `Send`/`Sync` bound.\n{stderr}"
    );
    assert!(
        stderr.contains("assert_send_sync"),
        "the failure did not come from the production assertion; it must \
         name `assert_send_sync`.\n{stderr}"
    );
}

#[test]
fn rustel_core_declares_no_quickjs_dependency() {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("read manifest");
    for forbidden in [
        "rquickjs",
        "quickjs",
        "rustel-jsruntime",
        "rustel-runtime",
        "rustel-scheduler",
        "rustel-audio",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "rustel-core must not depend on {forbidden}: L1 holds no JavaScript \
             and no upward layer"
        );
    }
}
