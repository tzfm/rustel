//! The wait of a live edit for its plugin copies, through the doors of the
//! session.

#![cfg(all(feature = "vst", feature = "device-audio", feature = "test-support"))]

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use rustel_audio::LiveScalarDevice;
use rustel_fraction::Fraction;
use rustel_runtime::test_support::past_the_cover;
use rustel_runtime::{LiveReload, SAMPLE_AWAITED_DIAGNOSTIC, Session, TakeoverCut};

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

/// Waits while the session waits for the plugin copies of `source`.
fn wait_for_copies(session: &mut Session, source: &str) {
    let started = Instant::now();
    while session.waits_for_plugins(source) {
        assert!(started.elapsed() < Duration::from_secs(20), "no copy");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The waiting lines of the session since the last call.
fn waiting_lines(session: &mut Session) -> Vec<String> {
    let lines = session.take_diagnostics().into_iter();
    let lines = lines.filter(|line| line.kind == SAMPLE_AWAITED_DIAGNOSTIC);
    lines.map(|line| line.message).collect()
}

/// A text loads the plugin of each plugin call it ran, also a call with no
/// lane, after the copies of its lanes that play. A plugin in a folder the
/// host still reads loads at the next look. A muted lane gets its copy, and
/// nothing waits for it.
/// An edit with a plugin copy that is not ready waits, with one line,
/// and the score in play plays on. The landing evaluates the text again,
/// and the copy of the wait serves the note in play. A launch prepares
/// its copies and installs nothing. A launch with no time before its line
/// holds at once. A launch with the copies of its arm and no time before
/// its line reads nothing: the copies of its arm decide. A cancel, or a
/// text with no plan, ends the wait and its plan, and the plugin host ends
/// the copy only the plan made. With the wait off an edit plays at once.
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

    // A plugin name in a comment loads nothing. A call with a name from a
    // variable loads its plugin. The call has no lane, so no read reaches
    // its note and no copy builds. A muted lane gets its copy, and nothing
    // waits for it.
    let loaded = || {
        host.wait_idle();
        host.loaded(rustel_vst3_fixture::NAME).is_some()
    };
    session.schedule_through(0.05, 0.05).expect("cover");
    let comment = format!("{playing}\n// $: s(\"sine\").vst(\"rustel fixture\")");
    let prepared = session.prepare_reload(&comment, false, 0.0, &stopped, || 0.05);
    assert!(!prepared.expect("prepare a comment"));
    assert!(!loaded());
    assert!(!session.ran_plugin_calls());
    // The plugin list of a read of the folders is complete only at its
    // end. A look in the read keeps a name the host does not know, and the
    // next look loads the plugin. The slow load of the fixture holds the
    // plugin thread, so the read waits in the line.
    let new = tempfile::tempdir().expect("new plugin folder");
    let bundle = rustel_vst3_fixture::install(new.path());
    std::fs::rename(&bundle, new.path().join("rustel later.vst3")).expect("bundle name");
    // SAFETY: the plugin thread has no work, and the binary has one test.
    unsafe { std::env::set_var(rustel_vst3_fixture::SLOW_ENV, "600") };
    host.resolve(rustel_vst3_fixture::NAME, false);
    rustel_runtime::vst::set_user_folders(vec![new.path().to_path_buf()]);
    let unused =
        format!("const name = 'rustel later';\nconst unused = note(\"c2\").vsti(name);\n{playing}");
    let prepared = session.prepare_reload(&unused, false, 0.0, &stopped, || 0.05);
    assert!(!prepared.expect("prepare a call with no lane"));
    host.wait_idle();
    session.plugins_pending(0.0, 1.0, Duration::from_secs(1));
    host.wait_idle();
    assert!(host.loaded("rustel later").is_some());
    // SAFETY: as above.
    unsafe { std::env::remove_var(rustel_vst3_fixture::SLOW_ENV) };
    rustel_runtime::vst::set_user_folders(Vec::new());
    assert!(session.ran_plugin_calls());
    assert_eq!(copies(), 0, "a call with no lane");
    let muted = format!(
        "const name = 'rustel fixture tone';\n{playing}\n_$: note(\"c2\").s(\"sine\").vsti(name)"
    );
    let prepared = session.prepare_reload(&muted, false, 0.0, &stopped, || 0.05);
    assert!(!prepared.expect("prepare a muted lane"));
    assert_eq!(copies(), 1, "the copy of the muted lane");

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
    let loading = "plugin \"rustel fixture\" is still loading";
    assert_eq!(waiting_lines(&mut session), [loading]);

    // The landing: the copy is ready, and the text evaluates again.
    wait_for_copies(&mut session, &edit);
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

    // A launch with 5 ms before its line reads nothing at its fire: the
    // copies of its arm decide, also a copy of cycle 3. The session keeps
    // them with each copy ready too. At a new rate no copy is ready, and
    // the launch holds with one line. With each copy ready, it commits on
    // its line.
    let later = format!("{edit}\n$: note(\"<~!3 c6>\").s(\"sine\").vst(\"rustel fixture\")");
    session.schedule_through(0.39, 0.39).expect("cover");
    let prepared = session.prepare_reload(&later, false, 0.0, &stopped, || 0.39);
    assert!(prepared.expect("prepare a lane of cycle 3"));
    wait_for_copies(&mut session, &later);
    let prepared = session.prepare_reload(&later, false, 0.0, &stopped, || 0.39);
    assert!(!prepared.expect("prepare with each copy ready"));
    assert_eq!(session.plugins_waited_for(&later), Some((0, 2)));
    let fire = |session: &mut Session| {
        session.start_next_from_zero();
        session.set_next_takeover_time(0.395);
        session.reload_live(&later, false, &stopped, || 0.39)
    };
    waiting_lines(&mut session);
    session.set_plugin_rate(44_100);
    assert_eq!(fire(&mut session).expect("fire"), LiveReload::Waiting);
    assert_eq!(waiting_lines(&mut session), [loading], "a new wait");
    assert_eq!(session.active_source(), Some(commented.as_str()));
    assert_eq!(session.plugins_waited_for(&later), Some((2, 2)));
    session.clear_next_takeover_time();
    session.clear_next_from_zero();
    wait_for_copies(&mut session, &later);
    let landed = fire(&mut session).expect("fire with each copy ready");
    assert_eq!(landed, LiveReload::Installed(generation + 2));
    let takeover = session.take_requery_takeover();
    assert_eq!(takeover, Some((0.395, TakeoverCut::AtTakeover)));
    session.set_plugin_rate(48_000);

    // With the wait off, an edit plays at once. Its commit reads a clock
    // past the cover, so it is the last edit here.
    session.set_plugin_wait(false);
    let late = format!("{edit}{}", lane(5));
    session.schedule_through(0.4, 0.4).expect("cover");
    let reload = session.reload_live(&late, false, &stopped, past_the_cover(0.45));
    assert_eq!(reload.expect("edit"), LiveReload::Installed(generation + 3));
}
