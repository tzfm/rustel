//! Check real camera acquisition. Requires hardware and permission to use it.
//! Run with `cargo test -p rustel-runtime --test hydra_camera_opens -- --ignored`.
//!
//! macOS can restore a different capture format when starting a session, even
//! after accepting the requested mode. Acquisition must find a working mode
//! instead of remaining in Opening while mismatched frames are discarded.
#![cfg(feature = "hydra")]

use std::time::{Duration, Instant};

use rustel_runtime::HydraBridge;
use rustel_runtime::hydra::HydraWebcamState;

/// Generous enough for the macOS permission prompt plus one fallback mode.
const READY_TIMEOUT: Duration = Duration::from_secs(90);

/// How long a ready camera is given to expose. The first frames off a sensor
/// are routinely black, which is not the failure this test is looking for.
const SETTLE: Duration = Duration::from_secs(5);

#[test]
#[ignore = "needs a real camera and permission for this terminal"]
fn an_allowed_camera_reaches_ready_with_a_preview() {
    let mut bridge = HydraBridge::new();
    let policy = bridge.input_policy();
    // The Settings preview is the one camera slot that needs no window and no
    // renderer generation, so this exercises acquisition on its own.
    policy.set_webcam_allowed(true);
    policy.set_settings_preview_requested(true);

    let started = Instant::now();
    let deadline = started + READY_TIMEOUT;
    let mut seen = Vec::new();
    let status = loop {
        bridge
            .tick(Instant::now(), 0.0, 1.0)
            .expect("no signals to sample");
        let status = policy.webcam_status();
        if seen.last() != Some(&status.state) {
            seen.push(status.state);
            eprintln!(
                "{:>6.1}s  {}",
                started.elapsed().as_secs_f32(),
                status.detail
            );
        }
        if status.state == HydraWebcamState::Ready || Instant::now() >= deadline {
            break status;
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    assert_eq!(
        status.state,
        HydraWebcamState::Ready,
        "camera never delivered a frame; states seen: {seen:?} - {}",
        status.detail
    );
    let lit = |preview: &rustel_runtime::hydra::HydraWebcamPreview| {
        usize::from(preview.width) * usize::from(preview.height) * 3 == preview.rgb.len()
            && preview.rgb.iter().any(|&channel| channel != 0)
    };
    let settled = Instant::now() + SETTLE;
    let mut preview = status
        .preview
        .expect("a ready camera carries its thumbnail");
    while !lit(&preview) && Instant::now() < settled {
        std::thread::sleep(Duration::from_millis(50));
        bridge
            .tick(Instant::now(), 0.0, 1.0)
            .expect("no signals to sample");
        let status = policy.webcam_status();
        assert_eq!(
            status.state,
            HydraWebcamState::Ready,
            "a camera that reached Ready must stay there: {}",
            status.detail
        );
        preview = status
            .preview
            .expect("a ready camera carries its thumbnail");
    }
    assert_eq!(
        usize::from(preview.width) * usize::from(preview.height) * 3,
        preview.rgb.len(),
        "the thumbnail must be a complete RGB square"
    );
    assert!(
        lit(&preview),
        "the thumbnail stayed black for {SETTLE:?}: a camera that opened but saw \
         nothing (or a covered lens)"
    );
}
