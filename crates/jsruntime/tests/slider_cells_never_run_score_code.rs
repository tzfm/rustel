//! Host slider-cell traffic must never execute score JavaScript.
//!
//! The `sliderValues` compatibility global hands the score a reference to the
//! private query-time cell object. A hostile or accidental
//! `Object.defineProperty` on it can plant an accessor whose getter or setter
//! loops forever. Host reads and writes prove each cell is a plain writable
//! finite-number data property before touching it, so planted accessors are
//! treated as unknown cells and their code never runs.

use rustel_jsruntime::JsRuntime;
use rustel_transpiler::TranspileOptions;
use std::time::{Duration, Instant};

const PLANTED_ACCESSOR: &str = r#"
    Object.defineProperty(sliderValues, 'evil:1', {
      get() { while (true) {} },
      set(v) { while (true) {} },
      enumerable: true,
      configurable: false,
    });
    Object.defineProperty(sliderValues, 'getter-only:1', {
      get() { return 0.5; },
      enumerable: true,
      configurable: false,
    });
    $: s("bd")
"#;

fn evaluate_planting_score(runtime: &JsRuntime) {
    runtime.install_semantic_bindings().expect("bindings");
    runtime
        .evaluate_score(PLANTED_ACCESSOR, &TranspileOptions::default())
        .expect("the planting score itself evaluates");
}

fn elapsed_within(start: Instant) -> bool {
    start.elapsed() < Duration::from_secs(3)
}

#[test]
fn host_read_of_a_planted_accessor_is_refused_without_running_its_getter() {
    let runtime = JsRuntime::new().expect("runtime");
    evaluate_planting_score(&runtime);

    let started = Instant::now();
    let value = runtime.slider_value("evil:1").expect("host read succeeds");

    assert_eq!(value, None, "a planted accessor is not a readable cell");
    assert!(
        elapsed_within(started),
        "host read hung on a planted getter"
    );
}

#[test]
fn host_write_to_a_planted_accessor_is_refused_without_running_its_setter() {
    let runtime = JsRuntime::new().expect("runtime");
    evaluate_planting_score(&runtime);

    let started = Instant::now();
    let written = runtime
        .set_slider_value("evil:1", 0.75)
        .expect("host write decision succeeds");

    assert!(!written, "a planted accessor is not a writable cell");
    assert!(
        elapsed_within(started),
        "host write hung on a planted setter"
    );
}

#[test]
fn a_getter_only_cell_is_not_advertised_as_a_writable_slider() {
    let runtime = JsRuntime::new().expect("runtime");
    evaluate_planting_score(&runtime);

    let read = runtime.slider_value("getter-only:1").expect("read");
    let write = runtime
        .set_slider_value("getter-only:1", 0.25)
        .expect("write decision");

    assert_eq!(read, None);
    assert!(!write);
}

#[test]
fn patching_the_probe_globals_cannot_run_score_code_on_host_traffic() {
    // The probe must consult helpers captured at install time: if it looked
    // up `Object.getOwnPropertyDescriptor` dynamically, a patched global
    // would wedge host reads/writes exactly like a planted accessor.
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
        .evaluate_score(
            r#"
              Object.getOwnPropertyDescriptor = () => { while (true) {} };
              Number.isFinite = () => { while (true) {} };
              globalThis.sliderWithID('patched:1', 0.5, 0, 1); $: s('bd')
            "#,
            &TranspileOptions::default(),
        )
        .expect("score patching the probe globals");

    let started = Instant::now();
    let read = runtime
        .slider_value("patched:1")
        .expect("host read succeeds");
    let written = runtime
        .set_slider_value("patched:1", 0.75)
        .expect("host write decision succeeds");

    assert_eq!(
        read,
        Some(0.5),
        "a plain cell still reads through the probe"
    );
    assert!(written);
    assert!(
        elapsed_within(started),
        "host traffic ran a patched probe helper"
    );
}

#[test]
fn ordinary_registered_cells_still_read_and_write_through_the_probe() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("bindings");
    runtime
        .evaluate_score(
            "globalThis.sliderWithID('plain:1', 0.5, 0, 1); $: s('bd')",
            &TranspileOptions::default(),
        )
        .expect("score registering a plain cell");

    let read = runtime.slider_value("plain:1").expect("read");
    let written = runtime.set_slider_value("plain:1", 0.75).expect("write");
    let reread = runtime.slider_value("plain:1").expect("reread");

    assert_eq!(read, Some(0.5));
    assert!(written);
    assert_eq!(reread, Some(0.75));
}
