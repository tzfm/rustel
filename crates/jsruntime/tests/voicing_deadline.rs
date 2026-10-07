use rustel_core::{Value, pure};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, QueryError, Slot};
use rustel_transpiler::TranspileOptions;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const CHILD: &str = "RUSTEL_VOICING_DEADLINE_CHILD";

fn runaway_registry_getter_child() {
    let runtime = JsRuntime::new().expect("runtime");
    runtime.install_semantic_bindings().expect("score bindings");
    runtime
        .install_voicings_prebake()
        .expect("voicing bindings");
    runtime
        .set_active(&runtime.builder(), pure(Value::Str("baseline".into())))
        .expect("last-good graph");
    runtime
        .eval(
            "Object.defineProperty(voicingRegistry, 'stall', { configurable: true, enumerable: true, get() { globalThis.registryGetterRan = 1; while (true) {} } })",
        )
        .expect("install score-controlled getter");

    let cancellation = AtomicBool::new(false);
    let error = runtime
        .evaluate_score_cancellable(
            "pure('replacement')",
            &TranspileOptions::default(),
            Duration::from_millis(40),
            &cancellation,
        )
        .expect_err("registry getter must obey score deadline");
    assert!(
        matches!(
            error,
            QueryError::Limit(rustel_core::QueryLimit::JsCpuDeadline { millis: 40 })
        ),
        "registry getter escaped the typed deadline: {error:?}"
    );
    assert_eq!(runtime.get_number("registryGetterRan"), Some(1.0));
    let active = runtime
        .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
        .expect("last-good graph remains active");
    assert_eq!(active[0].value.show(), "baseline");

    runtime
        .eval("delete voicingRegistry.stall")
        .expect("remove hostile getter");
    runtime
        .evaluate_score_cancellable(
            "pure('recovered')",
            &TranspileOptions::default(),
            Duration::from_secs(1),
            &cancellation,
        )
        .expect("next score can recover");
}

#[test]
fn runaway_voicing_registry_getter_obeys_score_deadline() {
    if std::env::var_os(CHILD).is_some() {
        runaway_registry_getter_child();
        return;
    }

    // Supervise the hostile getter so a regression cannot hang the whole suite.
    let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("runaway_voicing_registry_getter_obeys_score_deadline")
        .env(CHILD, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn deadline test child");
    let start = Instant::now();
    loop {
        if child
            .try_wait()
            .expect("poll deadline test child")
            .is_some()
        {
            break;
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("registry getter exceeded the supervised deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child
        .wait_with_output()
        .expect("collect deadline test child");
    assert!(
        output.status.success(),
        "deadline test child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
