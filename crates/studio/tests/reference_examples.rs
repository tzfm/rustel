//! Every reference example actually evaluates.
//!
//! A documentation example that errors when a reader pastes it is worse than
//! no example at all. This guard evaluates each entry's examples through a
//! real session and asserts each one evaluates without error. Examples that
//! make no sound (hydra helpers, MIDI output, `silence`, control-only) are
//! excused from producing haps, but they must still evaluate.

use rustel_runtime::{ScoreSampleAccess, Session, SessionConfig};
use rustel_studio::reference::Reference;

fn session() -> Session {
    let mut access = ScoreSampleAccess::denied();
    access.permit_public_cors_origins();
    let config = SessionConfig::default().with_score_sample_access(access);
    let mut session = Session::with_config(config).expect("session");
    session.set_direct_diagnostic_logging(false);
    let _ = session.enable_default_samples();
    session
}

/// Examples that deliberately cannot evaluate in a headless guard. Each is
/// paired with the reason it is excused, so a new exclusion must be argued.
fn excused(example: &str) -> bool {
    // Remote-sample examples fetch a URL; the guard runs offline.
    if example.contains("https://") || example.contains("http://") {
        return true;
    }
    false
}

#[test]
fn every_reference_example_evaluates() {
    let reference = Reference::load_all();
    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for index in 0..reference.len() {
        let entry = reference.entry(index).expect("entry");
        for example in &entry.examples {
            if excused(example) {
                continue;
            }
            checked += 1;
            // A fresh session per example isolates state and any panic, so one
            // broken example cannot poison or abort the rest of the sweep.
            let example = example.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut session = session();
                session.evaluate(&example)
            }));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => failures.push(format!("`{}` {e}", entry.name)),
                Err(_) => failures.push(format!("`{}` panicked", entry.name)),
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {checked} examples do not evaluate:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
