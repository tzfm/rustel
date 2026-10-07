//! Library builds can omit the MP3 encoder while retaining explicit errors.

#![cfg(not(feature = "mp3-export"))]

use rustel_runtime::{RenderFormat, RuntimeError, Session};

fn assert_unsupported(error: RuntimeError) {
    assert_eq!(error.kind(), "unsupported", "got {error}");
    assert!(
        error.to_string().contains("`mp3-export` feature"),
        "got {error}"
    );
}

#[test]
fn mp3_render_is_rejected_before_playback_or_output() {
    let directory = tempfile::tempdir().expect("render directory");
    let path = directory.path().join("audio.mp3");
    let staged = path.with_extension("rendering.wav");
    std::fs::write(&path, b"existing export").expect("existing export");
    std::fs::write(&staged, b"existing WAV").expect("existing WAV");
    let mut session = Session::new().expect("session");

    assert_unsupported(
        session
            .render(0.125, &path, RenderFormat::ScalarMp3)
            .expect_err("MP3 is unavailable even before a pattern is installed"),
    );

    let mut observed = false;
    let mut observer = |_: rustel_audio::RenderTick<'_>| observed = true;
    assert_unsupported(
        session
            .render_controlled(
                0.125,
                &path,
                RenderFormat::ScalarMp3,
                false,
                Some(&mut observer),
                None,
            )
            .expect_err("controlled MP3 is unavailable"),
    );
    assert!(!observed, "unsupported export must not render audio");
    assert_eq!(std::fs::read(path).unwrap(), b"existing export");
    assert_eq!(std::fs::read(staged).unwrap(), b"existing WAV");
}

#[test]
fn mp3_replay_is_rejected_before_installing_a_saved_score() {
    let directory = tempfile::tempdir().expect("render directory");
    let path = directory.path().join("replay.mp3");
    let mut session = Session::new().expect("session");
    let saves = [(0.0, "throw new Error('must not evaluate')".to_owned())];
    assert_unsupported(
        session
            .render_session(&saves, 0.0, Some(0.125), &path, true)
            .expect_err("MP3 replay is unavailable"),
    );
    assert!(!path.exists());
}
