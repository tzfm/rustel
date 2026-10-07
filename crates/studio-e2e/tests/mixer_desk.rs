//! Check mixer strips, faders, layout and focus. The silent harness cannot
//! validate meter levels; engine tests cover gain commands and retries.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic, row_containing};

/// Exclude the editor and title rule. Both can repeat fader names and values.
fn desk_rows(studio: &mut rustel_studio_e2e::Hermetic) -> Vec<String> {
    let rows = studio.rows();
    let at = rows
        .iter()
        .position(|row| row.contains("── mixer ──"))
        .expect("the desk's rule is on screen");
    rows[at + 1..].to_vec()
}

/// F4 shows the desk with the keyboard on it and names its keys. Esc returns
/// the keyboard to the score and the desk stays on screen. F4 again hides
/// the desk.
#[test]
fn the_desk_opens_with_the_keys_and_esc_leaves_it_standing() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(4), KeyModifiers::NONE);
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Mixer),
        "the desk takes the keys"
    );
    assert!(
        studio.status().starts_with("mixer: ←/→ strip"),
        "the desk says what its keys do: {}",
        studio.status()
    );
    // The master strip is always there; the desk's own hint row (with
    // `+/- size`, which the status line does not say) is on screen.
    row_containing(&studio.rows(), "+/- size");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None, "Esc gives the keys back");
    // The standing desk now speaks to the hand, not the keyboard: its
    // hint swaps to the pointer contract.
    row_containing(&studio.rows(), "a press takes the keys");

    studio.press(KeyCode::F(4), KeyModifiers::NONE);
    assert_eq!(studio.focus(), None, "F4 hides the desk outright");
    assert_eq!(studio.status(), "mixer hidden");
    assert!(
        !studio
            .rows()
            .iter()
            .any(|row| row.contains("takes the keys")),
        "the rows are the score's again"
    );
}

/// The desk opens with the master selected: ↑ raises its fader one decibel
/// and the status names the new level; `0` restores unity. An orbit strip is
/// a meter, not a fader: ↑ there gives a status message and changes nothing.
#[test]
fn the_fader_keys_move_the_selected_strip_and_zero_bring_it_back() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(4), KeyModifiers::NONE);
    assert!(
        studio.status().starts_with("mixer: ←/→ strip"),
        "the desk opens on its banner: {}",
        studio.status()
    );

    studio.press(KeyCode::Up, KeyModifiers::NONE);
    let raised = studio.status().to_owned();
    assert!(
        raised.contains("master") && raised.contains('+') && raised.contains("dB"),
        "the raise names the strip, the direction and the size: {raised}"
    );
    studio.press(KeyCode::Char('0'), KeyModifiers::NONE);
    assert!(
        studio.status().contains("master"),
        "unity is said on the strip it reset: {}",
        studio.status()
    );

    // With no limiter on the set, the desk's own strips are just the
    // master - the strips that come and go with the score never move it.
    // So → from the master lands on the first orbit.
    studio.set_score("$: s(\"bd\").orbit(2)");
    studio.settle();
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    // The strip names are the score's orbit numbers: orbit 1 is the default
    // for every line, and `.orbit(2)` adds `orbit 2`.
    assert_eq!(studio.status(), "mixer: orbit 1", "→ walks to the orbit");
    studio.press(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "orbit 1 is a meter; set its level with gain() or postgain() in the score",
        "an orbit's ↑ is told where its level really lives"
    );
    // ← walks back to the master, and one more wraps to the desk's last
    // strip: the walk is a ring, and without a limiter the orbit is where
    // it closes.
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "mixer: the master",
        "← lands back on the master"
    );
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "mixer: orbit 2",
        "← wraps to the desk's last strip, no limiter in the ring"
    );
}

/// The desk's device feed comes from the frame the harness pins. On a host
/// with a MIDI keyboard or a gamepad attached, the feed still shows the two
/// pinned chips and no live device line.
#[test]
fn the_desk_device_feed_is_pinned_to_the_fixture_not_the_host() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(4), KeyModifiers::NONE);
    studio.settle();

    row_containing(&studio.rows(), "⌁ no midi");
    row_containing(&studio.rows(), "▣ no gamepad");
    for row in desk_rows(&mut studio) {
        assert!(
            !row.contains("gamepad(") && !row.contains("aftertouch"),
            "a live device line leaked into the desk: {row:?}"
        );
    }
}

/// The desk shows the evaluated score's sliders beside the strips: named
/// by the call each sits in, reading its value, with both ends of its
/// travel and the slot that drives it. A fader whose range you cannot see
/// is one you have to guess at - the desk says it all.
#[test]
fn the_desk_shows_the_scores_faders_as_the_score_names_them() {
    let mut studio = hermetic();

    // One frequency slider inside `.lpf(...)`, one plain inside `.gain(...)`
    // - the desk's label is the call it sits in where the score gives it
    // one, and `slider N` where it does not.
    studio.set_score("$: s(\"bd\").lpf(slider(800,100,4000)).gain(slider(0.8,0,1,0.1))");
    // The desk reads the *evaluated* score's live controls, so the settle
    // that matters is the one after the evaluation, not the edit.
    studio.chord("ctrl+s");
    studio.settle();

    studio.press(KeyCode::F(4), KeyModifiers::NONE);
    assert!(
        studio.status().starts_with("mixer: ←/→ strip"),
        "the desk opens first: {}",
        studio.status()
    );

    let rows = desk_rows(&mut studio);
    // The fader's own two rows: its name (the call it sits in where the
    // score gave one) with its reading hard right; then the range's ends
    // either side of the rail. No slot number: a slot nobody has bound
    // drives nothing, and saying one would promise a knob that is not
    // there.
    row_containing(&rows, "lpf");
    row_containing(&rows, "800");
    row_containing(&rows, "4000");
    row_containing(&rows, "gain");
    row_containing(&rows, "0.8");
    // Where the knob sits on its rail is the travel's business (unit-tested
    // with the gamma); the desk's promise is a rail drawn with a knob on it
    // between the ends the score gave it.
    let lpf_rail = rows
        .iter()
        .find(|row| row.contains("4000"))
        .expect("the lpf fader's rail row");
    assert!(
        lpf_rail.contains('█') && lpf_rail.contains('─'),
        "the rail is the score's own glyphs with the knob on it: {lpf_rail:?}"
    );
    let gain_rail = rows
        .iter()
        .find(|row| row.contains(" 1") && row.contains('█'))
        .expect("the gain fader's rail row");
    assert!(
        gain_rail.contains("0"),
        "both ends of the gain fader's travel are on its rail: {gain_rail:?}"
    );

    // The source-order promise: `.lpf` is written first, so it is the
    // desk's first fader - its rows sit above the next fader's.
    let lpf_at = rows
        .iter()
        .position(|row| row.contains("lpf"))
        .expect("the lpf fader's row");
    let gain_at = rows
        .iter()
        .position(|row| row.contains("gain"))
        .expect("the gain fader's row");
    assert!(
        lpf_at < gain_at,
        "source order is the desk's order: lpf above gain ({lpf_at} < {gain_at})"
    );
}

/// ⇧M (the legacy chord) toggles the desk too, and the choice outlives
/// the studio: the desk is back, standing but keyless, on the next open.
#[test]
fn the_desk_is_remembered_by_the_next_open() {
    let mut studio = hermetic();

    studio.chord("ctrl+shift+m");
    assert_eq!(studio.focus(), Some(PanelKind::Mixer));
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None);

    // Close the studio the way the shell would and open over the same
    // set: the desk comes back standing, and takes no keys. The pref is
    // written on a debounce, so the wait is what makes the quit honest -
    // the same bargain the audio-in pick makes.
    std::thread::sleep(std::time::Duration::from_millis(700));
    studio.pump();
    studio.quit();
    let mut studio = rustel_studio_e2e::reopen_over(studio);
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("takes the keys")),
        "the desk came back with the set"
    );
    assert_eq!(studio.focus(), None, "furniture takes no keys on return");
}

/// The limiter strip belongs to the set. A fresh set has none; the Transport
/// menu adds or removes one, and Backspace also removes it. While the set
/// has one, the strip sits beside the master: `c` steps the mode, ↑/↓ move
/// the ceiling between its floor and unity, `0` restores the studio's
/// default, and Enter or `b` switches the bypass.
#[test]
fn the_limiter_strip_takes_its_keys_and_enter_is_its_switch() {
    let mut studio = hermetic();

    // A fresh set has no limiter slot, so the desk has no limiter strip:
    // ← from the master wraps to the desk's last strip, never landing on
    // one that would be invented.
    studio.press(KeyCode::F(4), KeyModifiers::NONE);
    assert_eq!(studio.focus(), Some(PanelKind::Mixer));
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert!(
        !studio.status().starts_with("mixer: the limiter"),
        "no limiter strip on a fresh set: {}",
        studio.status()
    );

    // Transport ▸ Add limiter to set puts one on, at the ceiling and
    // mode the studio hands out - and the new strip is what the keys
    // then aim at.
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('t'), KeyModifiers::NONE); // Transport, dropped
    row_containing(&studio.rows(), "Add limiter to set");
    studio.press(KeyCode::Char('m'), KeyModifiers::NONE); // the row's mnemonic
    let added = studio.status().to_owned();
    assert!(
        added.starts_with("limiter added \u{b7} transparent at -1.0 dBFS"),
        "adding gives the set the studio's defaults: {added}"
    );

    // The slot comes with the limiter switched in and selected; the ring
    // reads left to right into the output, so → from the limiter is the
    // master and ← comes back to the strip beside it.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "mixer: the master",
        "→ from the limiter is the master: the strip sits against it"
    );
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    let selected = studio.status().to_owned();
    assert!(
        selected.starts_with("mixer: the limiter, in \u{b7} transparent at -1.0 dBFS"),
        "the strip is in the ring beside the master: {selected}"
    );
    let rows = desk_rows(&mut studio);
    row_containing(&rows, "clean");

    // `c` steps the mode from transparent to punchy. The status line and
    // the strip's label show the new mode. The sound and the set take the
    // change after the key rests, so the wait below makes the next `c` a
    // new step and not a debounced repeat.
    studio.press(KeyCode::Char('c'), KeyModifiers::NONE);
    let cycled = studio.status().to_owned();
    assert!(
        cycled.starts_with("limiter punchy at -1.0 dBFS \u{b7} c for the next mode"),
        "the mode walked round its ladder: {cycled}"
    );
    let rows = desk_rows(&mut studio);
    row_containing(&rows, "punch");
    std::thread::sleep(std::time::Duration::from_millis(300));

    // ↑ raises the ceiling a decibel and says where it went; the strip's
    // label is the mode, the number the ceiling. For a ceiling, unity is
    // the top of travel - 0 dBFS lets the whole signal through.
    studio.press(KeyCode::Up, KeyModifiers::NONE);
    let raised = studio.status().to_owned();
    assert!(
        raised.starts_with("limiter punchy at 0.0 dBFS"),
        "a ceiling move names the limiter and its number: {raised}"
    );

    // ↓ stops at the floor and does not turn the limiter off. The limiter
    // stays in at -24.0 dBFS.
    for _ in 0..30 {
        studio.press(KeyCode::Down, KeyModifiers::NONE);
        if studio.status().contains("-24.0 dBFS") {
            break;
        }
    }
    assert!(
        studio.status().starts_with("limiter punchy at -24.0 dBFS"),
        "the floor is a ceiling stop, not an off switch: {}",
        studio.status()
    );

    // `0` restores the studio's own numbers. It does not remove the
    // limiter: the strip stays in the ring.
    studio.press(KeyCode::Char('0'), KeyModifiers::NONE);
    let deferred = studio.status().to_owned();
    assert!(
        deferred
            .starts_with("limiter back to the studio's default \u{b7} transparent at -1.0 dBFS"),
        "`0` hands the numbers back to the studio: {deferred}"
    );

    // After `0` the mode is `transparent` again, so the next `c` selects
    // `punchy`. The wait lets the held-key debounce pass first.
    std::thread::sleep(std::time::Duration::from_millis(300));
    studio.press(KeyCode::Char('c'), KeyModifiers::NONE);
    let chosen = studio.status().to_owned();
    assert!(
        chosen.starts_with("limiter punchy at -1.0 dBFS \u{b7} c for the next mode"),
        "`c` walks the ladder and the set keeps the opinion: {chosen}"
    );

    // Enter switches it out - the bypass keeps the ceiling and the mode
    // it is keeping, said on the same line so the strip's `byp` is never
    // a mystery about what it holds.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    let back = studio.status().to_owned();
    assert!(
        back.starts_with("limiter bypassed \u{b7} punchy at -1.0 dBFS is kept"),
        "Enter switches it out, keeping what it held: {back}"
    );
    let rows = desk_rows(&mut studio);
    row_containing(&rows, "byp");

    // Enter once more switches it back in at what it kept.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    let switched = studio.status().to_owned();
    assert!(
        switched.starts_with("limiter in \u{b7} punchy at -1.0 dBFS"),
        "Enter switches it back in: {switched}"
    );

    // Backspace on the strip removes the limiter from this set, and the
    // selection leaves the strip that is gone.
    studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(
        studio.status(),
        "limiter removed from this set",
        "Backspace removes the set's limiter"
    );
    let rows = desk_rows(&mut studio);
    assert!(
        !rows.iter().any(|row| row.contains("punch")),
        "the strip is gone from the desk:\n{}",
        rows.join("\n")
    );
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert!(
        !studio.status().starts_with("mixer: the limiter"),
        "no strip, no keys aimed at it: {}",
        studio.status()
    );
}
