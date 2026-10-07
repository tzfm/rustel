use std::env;

fn main() {
    // Cargo tracks the target and profile; this script reads no files.
    println!("cargo:rerun-if-changed=build.rs");

    export("RUSTEL_BUILD_TARGET", "TARGET");
    export("RUSTEL_BUILD_PROFILE", "PROFILE");
}

fn export(destination: &str, source: &str) {
    let value = env::var(source).unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env={destination}={value}");
}
