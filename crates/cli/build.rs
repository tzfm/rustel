//! Writes `build_features.rs`: the sorted Cargo features this build of the
//! package enables, as a Rust slice expression the binary includes.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    // Cargo reruns this script when the package's features change; it reads
    // no files.
    println!("cargo:rerun-if-changed=build.rs");

    // Cargo sets `CARGO_FEATURE_<NAME>` for each enabled feature, upper-cased
    // with `-` turned into `_`. `default` names a set, not a capability.
    let mut features = env::vars_os()
        .filter_map(|(variable, _)| {
            let name = variable.to_str()?.strip_prefix("CARGO_FEATURE_")?;
            Some(name.to_ascii_lowercase().replace('_', "-"))
        })
        .filter(|feature| feature != "default")
        .collect::<Vec<_>>();
    features.sort_unstable();

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    fs::write(out_dir.join("build_features.rs"), format!("&{features:?}"))
        .expect("write build_features.rs");
}
