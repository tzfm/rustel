use std::path::{Path, PathBuf};

fn extension_sources(path: &Path, rust_sources: &mut Vec<PathBuf>, scripts: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(path).expect("read extension source tree") {
        let entry = entry.expect("read extension source entry");
        let path = entry.path();
        if path.is_dir() {
            extension_sources(&path, rust_sources, scripts);
            continue;
        }
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("rs") => rust_sources.push(path),
            Some("js" | "mjs") => scripts.push(path),
            _ => {}
        }
    }
}

#[test]
fn compiled_extensions_contain_only_native_rust_implementations() {
    let mut rust_sources = Vec::new();
    let mut scripts = Vec::new();
    extension_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut rust_sources,
        &mut scripts,
    );

    assert!(
        scripts.is_empty(),
        "JavaScript assets escaped the native extension boundary: {scripts:?}"
    );
    for path in rust_sources {
        let source = std::fs::read_to_string(&path).expect("read extension Rust source");
        for forbidden in [".eval(", "setup_source", "javascript_source"] {
            assert!(
                !source.contains(forbidden),
                "{} embeds an extension script through `{forbidden}`",
                path.display()
            );
        }
    }
}
