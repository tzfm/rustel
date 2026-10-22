//! The switch `audio thread only` of a plugin, from the user settings to the
//! copies an output takes.

#![cfg(feature = "vst")]

use rustel_audio::{InsertKey, InsertSlot, OrbitInsert};
use rustel_runtime::vst::{self, Resolved};
use rustel_vst3_fixture::{NAME, TONE_NAME};

/// A copy of a plugin of the fixture bundle, as an output takes it for a
/// slot.
fn copy(name: &str, slot: InsertSlot) -> Box<dyn OrbitInsert> {
    let host = vst::host();
    let Resolved::Ready(plugin, _) = host.resolve(name, true) else {
        panic!("{name} did not load");
    };
    let key = InsertKey { plugin, preset: 0 };
    (host.provider(true))(key, 48_000, slot).expect("a copy")
}

/// The host reads the switches of `rustel.json` at its start, so a play
/// with no Studio keeps the plugins of the Studio switches on the audio
/// thread. A copy an output takes after a change follows the change. The
/// variable takes the place of the switches. A switch still turns on or off
/// in the settings while the variable is set.
#[test]
fn the_switches_of_the_user_settings_reach_each_new_copy() {
    let config = tempfile::tempdir().expect("config folder");
    let settings = config.path().join("rustel.json");
    let switches = r#"{"plugin_audio_thread": ["rustel fixture"]}"#;
    std::fs::write(&settings, switches).expect("settings");
    let plugins = tempfile::tempdir().expect("plugin folder");
    rustel_vst3_fixture::install(plugins.path());
    // SAFETY: the binary has one test, and no other thread runs yet.
    unsafe {
        std::env::set_var(rustel_runtime::config_dir::CONFIG_DIR_VAR, config.path());
        std::env::set_var(vst::FOLDERS_ENV, plugins.path());
    }
    let effect = |chain| copy(NAME, InsertSlot::effect(chain, 0));
    let tone = |chain| copy(TONE_NAME, InsertSlot::instrument(chain));
    assert!(!effect(1).any_thread());
    assert!(tone(1).any_thread());

    // The switches of the vst page in Studio.
    let saved = || {
        let saved = std::fs::read(&settings).expect("settings");
        let saved: serde_json::Value = serde_json::from_slice(&saved).expect("settings");
        saved["plugin_audio_thread"].clone()
    };
    let toggle = |name| vst::toggle_audio_thread_only(name).expect("saved");
    assert_eq!(toggle(TONE_NAME), Some(true));
    assert_eq!(toggle(NAME), Some(false));
    assert!(effect(2).any_thread());
    assert!(!tone(2).any_thread());
    assert_eq!(saved(), serde_json::json!([TONE_NAME]));

    vst::host().wait_idle();
    // SAFETY: the plugin thread has no work, so no other thread reads a
    // variable now.
    unsafe { std::env::set_var(vst::AUDIO_THREAD_ENV, NAME) };
    // The switch turns on in the settings while the variable decides.
    assert_eq!(toggle(NAME), None);
    assert_eq!(saved(), serde_json::json!([TONE_NAME, NAME]));
    assert!(!effect(3).any_thread());
    assert!(tone(3).any_thread());
}
