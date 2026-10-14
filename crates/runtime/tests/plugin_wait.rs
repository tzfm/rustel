//! The wait of a live edit for its plugin copies, through the doors of the
//! session.

#![cfg(all(feature = "vst", feature = "device-audio", feature = "test-support"))]

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use rustel_audio::LiveScalarDevice;
use rustel_fraction::Fraction;
use rustel_runtime::test_support::past_the_cover;
use rustel_runtime::{LiveReload, SAMPLE_AWAITED_DIAGNOSTIC, Session};

/// The fixture plugin in a new folder, as the only plugin folder of the
/// process: no test reads the plugins or the presets of the machine. Its
/// first copy is not ready at once.
fn fixture_plugins() -> tempfile::TempDir {
    let folder = tempfile::tempdir().expect("plugin folder");
    rustel_vst3_fixture::install(folder.path());
    rustel_runtime::vst::pin_standard_folders(vec![folder.path().to_path_buf()]);
    rustel_runtime::vst::host().set_preset_folder(folder.path().join("presets"));
    folder
}

/// An edit with a plugin copy that is not ready waits, with one line,
/// and the score in play plays on. The landing evaluates the text again,
/// and the copy of the wait serves the note in play. A launch prepares
/// its copies and installs nothing. A launch with no time before its line
/// holds at once. A cancel, or a text with no plan, ends the wait and its
/// plan, and the plugin host ends the copy only the plan made. With the
/// wait off an edit plays at once.
#[test]
fn an_edit_waits_for_its_plugin_copies() {
    let _plugins = fixture_plugins();
    let host = rustel_runtime::vst::host();
    let mut session = Session::new().expect("session");
    session.set_direct_diagnostic_logging(false);
    let playing = r#"$: s("sine").gain(0)"#;
    session.evaluate(playing).expect("score in play");
    let output =
        LiveScalarDevice::start_silent(48_000, session.generation()).expect("silent output");
    session.attach_plugins(&output);
    let stopped = AtomicBool::new(false);
    let lane = |note: u8| format!("\n$: note(\"c{note}\").s(\"sine\").vst(\"rustel fixture\")");
    // The copies of the plugin host, after the work of its thread.
    let copies = || {
        host.wait_idle();
        host.plugins()
            .iter()
            .map(|plugin| plugin.running)
            .sum::<usize>()
    };

    let edit = format!("{playing}{}", lane(3));
    session.schedule_through(0.1, 0.1).expect("cover");
    let generation = session.generation();
    let reload = session.reload_live(&edit, false, &stopped, past_the_cover(0.15));
    assert_eq!(reload.expect("edit"), LiveReload::Waiting);
    assert_eq!(session.active_source(), Some(playing));
    assert_eq!(session.generation(), generation);
    let notes = |session: &Session| {
        let haps = session.query(Fraction::ZERO, Fraction::ONE);
        haps.expect("query").len()
    };
    assert_eq!(notes(&session), 1, "the edit is in play");
    let diagnostics = session.take_diagnostics().into_iter();
    let waiting = diagnostics.filter(|line| line.kind == SAMPLE_AWAITED_DIAGNOSTIC);
    let waiting: Vec<String> = waiting.map(|line| line.message).collect();
    assert_eq!(waiting, ["plugin \"rustel fixture\" is still loading"]);

    // The landing: the copy is ready, and the text evaluates again.
    let started = Instant::now();
    while session.waits_for_plugins(&edit) {
        assert!(started.elapsed() < Duration::from_secs(20), "no copy");
        std::thread::sleep(Duration::from_millis(5));
    }
    session.schedule_through(0.2, 0.2).expect("cover");
    let landed = session.reload_live(&edit, false, &stopped, || 0.25);
    assert_eq!(
        landed.expect("landing"),
        LiveReload::Installed(generation + 1)
    );
    assert_eq!(session.active_source(), Some(edit.as_str()));
    assert!(session.plugins_waited_for(&edit).is_none());
    let ceiling = Duration::from_secs(1);
    let pending = session.plugins_pending(0.0, 1.0, ceiling);
    assert_eq!(pending, 0, "the copy of the wait");

    // A launch prepares its copies before its line. The cancel ends the
    // plan, and the plugin host ends the copy only the plan made. The next
    // plan gives the lane a new chain, with no copy.
    let launch = format!("{edit}{}", lane(4));
    let generation = session.generation();
    session.schedule_through(0.3, 0.3).expect("cover");
    let prepared = session.prepare_reload(&launch, false, 0.0, &stopped, || 0.3);
    assert!(prepared.expect("prepare"));
    assert_eq!(session.active_source(), Some(edit.as_str()));
    assert_eq!(session.generation(), generation);
    assert_eq!(notes(&session), 2, "the launch is in play");
    assert!(session.plugins_waited_for(&launch).is_some());
    let built = copies();
    session.cancel_plugin_hold();
    assert!(session.plugins_waited_for(&launch).is_none());
    assert_eq!(copies(), built - 1, "the copy only the plan made");
    let prepared = session.prepare_reload(&launch, false, 0.0, &stopped, || 0.35);
    assert!(prepared.expect("prepare again"));
    session.cancel_plugin_hold();

    // A launch with a copy not ready and no time before its line holds
    // at once. The caller then gives it a later line.
    session.schedule_through(0.36, 0.36).expect("cover");
    session.set_next_takeover_time(0.37);
    let reload = session.reload_live(&launch, false, &stopped, || 0.36);
    assert_eq!(reload.expect("launch"), LiveReload::Waiting);
    assert_eq!(session.active_source(), Some(edit.as_str()));
    session.clear_next_takeover_time();
    session.cancel_plugin_hold();

    // A text with each lane in a comment goes in play in place of the
    // text that waits. Its wait and its plan end, and the plugin host ends
    // the copy only the plan made. The next plan gives the lane a new chain.
    session.schedule_through(0.38, 0.38).expect("cover");
    let reload = session.reload_live(&launch, false, &stopped, past_the_cover(0.38));
    assert_eq!(reload.expect("launch"), LiveReload::Waiting);
    let built = copies();
    let commented: Vec<String> = launch.lines().map(|line| format!("// {line}")).collect();
    let commented = commented.join("\n");
    let reload = session.reload_live(&commented, false, &stopped, || 0.39);
    assert_eq!(
        reload.expect("comments"),
        LiveReload::Installed(generation + 1)
    );
    assert!(session.plugins_waited_for(&launch).is_none());
    assert_eq!(copies(), built - 1, "the copy only the plan made");
    let prepared = session.prepare_reload(&launch, false, 0.0, &stopped, || 0.39);
    assert!(prepared.expect("prepare after the comments"));
    // A launch with no pattern prepares nothing, and the plan before ends.
    let built = copies();
    let prepared = session.prepare_reload(&commented, false, 0.0, &stopped, || 0.39);
    assert!(!prepared.expect("prepare the comments"));
    assert_eq!(copies(), built - 1, "the copy of the plan before");
    let prepared = session.prepare_reload(&launch, false, 0.0, &stopped, || 0.39);
    assert!(prepared.expect("prepare after a launch with no pattern"));
    session.cancel_plugin_hold();

    // With the wait off, an edit plays at once. Its commit reads a clock
    // past the cover, so it is the last edit here.
    session.set_plugin_wait(false);
    let late = format!("{edit}{}", lane(5));
    session.schedule_through(0.4, 0.4).expect("cover");
    let reload = session.reload_live(&late, false, &stopped, past_the_cover(0.45));
    assert_eq!(reload.expect("edit"), LiveReload::Installed(generation + 2));
}
