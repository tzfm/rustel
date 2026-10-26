use super::{run, rustel};

#[test]
fn human_trace_reports_the_scheduled_onset_count() {
    let output = run(&["trace", "-e", "s('sine*2')", "--duration", "2"]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).expect("utf-8 output");
    assert!(text.contains(", 2 onsets"), "{text}");
}

#[test]
fn score_cache_reports_empty_for_missing_and_empty_directories() {
    let base = tempfile::tempdir().expect("tempdir created");
    let cache = base.path().join("samples");
    for existing in [false, true] {
        if existing {
            std::fs::create_dir_all(cache.join("score")).expect("the directory is created");
        }
        let output = rustel()
            .args(["clear-score-cache", "--force"])
            .env("RUSTEL_CONFIG_DIR", base.path())
            .env(rustel_runtime::product::SAMPLE_CACHE_ENV, &cache)
            .output()
            .expect("the command runs");
        assert!(output.status.success(), "{output:?}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
        assert_eq!(report["score_sample_cache"]["status"], "empty");
        assert!(cache.join(".score-cache-no-legacy").is_file());
    }
}

#[cfg(feature = "midi")]
#[test]
fn midi_monitor_input_failures_name_midi_instead_of_audio() {
    let output = run(&["midi-monitor", "18446744073709551615", "--duration", "0.01"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let text = String::from_utf8(output.stderr).expect("utf-8 output");
    assert!(text.contains("MIDI:"), "{text}");
    assert!(!text.contains("audio:"), "{text}");
}

#[cfg(feature = "studio")]
#[test]
fn doc_resolves_the_installed_tempo_alias() {
    let output = run(&["doc", "setcps", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let entry: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(entry["name"], "setCps");
    assert!(
        entry["description"]
            .as_str()
            .expect("the field is a string")
            .contains("cycles per second")
    );
}

#[cfg(feature = "studio")]
#[test]
fn doc_preserves_score_names_and_resolves_exact_graph_names() {
    for (name, graph) in [("sine", false), ("s", false), ("S", true), ("sFreq", true)] {
        let output = run(&["doc", name, "--json"]);
        assert!(output.status.success(), "{name}: {output:?}");
        let entry: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
        assert_eq!(
            entry["tags"]
                .as_array()
                .expect("the field is an array")
                .iter()
                .any(|tag| tag == "kabelsalat"),
            graph,
            "{name}: {entry}"
        );
        if graph {
            assert_eq!(entry["name"], name);
        }
    }
}

#[cfg(all(feature = "studio", feature = "hydra"))]
#[test]
fn doc_hydra_names_link_to_setup_in_human_and_json_modes() {
    for name in ["diff", "kaleid", "out"] {
        let output = run(&["doc", name]);
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).expect("utf-8 output");
        assert!(text.contains("initHydra"), "{text}");

        let output = run(&["doc", name, "--json"]);
        assert!(output.status.success(), "{output:?}");
        let entry: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
        assert_eq!(entry["name"], name);
        assert!(
            entry["description"]
                .as_str()
                .expect("the field is a string")
                .contains("initHydra")
        );
    }

    let output = run(&["doc", "noise", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let entry: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert!(
        !entry["tags"]
            .as_array()
            .expect("the field is an array")
            .iter()
            .any(|tag| tag == "hydra"),
        "a shared name must retain its musical reference entry: {entry}"
    );
}
