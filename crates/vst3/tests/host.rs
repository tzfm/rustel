//! The host with a real plugin library: the fixture plugin in a VST3 bundle.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rustel_audio::tripwire::{self, TripwireAlloc, Violations};
use rustel_audio::{
    AudioBackend, InsertControls, InsertKey, InsertNote, InsertParam, InsertSlot, OnsetEvent,
    OrbitInsert, ScalarBackend,
};
use rustel_vst3::{FoundPreset, Host, Prepared, Resolved, Status, WantedCopy, WorkerProgram};
use rustel_vst3_fixture::{
    COST_ENV, NAME, PARAM_BYPASS, PARAM_GAIN, PARAM_GATE, PROCESS_MARK_ENV, TONE_LEVEL, TONE_NAME,
};

#[global_allocator]
static A: TripwireAlloc = TripwireAlloc;

const SAMPLE_RATE: u32 = 48_000;

/// A plugin folder with the fixture bundle, a preset folder, and a host
/// that knows the two folders.
struct Rig {
    root: PathBuf,
    host: Host,
}

impl Rig {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("rustel-vst3-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("plugins")).expect("plugin folder");
        rustel_vst3_fixture::install(&root.join("plugins"));
        let host = Host::new();
        host.set_preset_folder(root.join("presets"));
        let rig = Self { root, host };
        rig.scan();
        rig
    }

    fn scan(&self) {
        self.host.scan(&[self.root.join("plugins")]);
        self.host.wait_idle();
    }

    /// The number of the fixture plugin, loaded.
    fn plugin(&self) -> u32 {
        match self.host.resolve(NAME, true) {
            Resolved::Ready(id, _) => id,
            _ => panic!("the fixture plugin did not load"),
        }
    }

    fn insert(&self, preset: u32) -> Option<Box<dyn OrbitInsert>> {
        let key = InsertKey {
            plugin: self.plugin(),
            preset,
        };
        (self.host.provider(true))(key, SAMPLE_RATE, InsertSlot::effect(1, 0))
    }

    fn write_preset(&self, name: &str, bytes: &[u8]) {
        let folder = self.root.join("presets").join(NAME);
        std::fs::create_dir_all(&folder).expect("preset folder");
        std::fs::write(folder.join(name), bytes).expect("preset file");
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// One block of a constant level through the insert. Returns the output.
fn block(insert: &mut dyn OrbitInsert, level: f32, frames: usize) -> Vec<f32> {
    let mut left = vec![level; frames];
    let mut right = vec![level; frames];
    insert.process(&mut left, &mut right);
    assert_eq!(left, right);
    left
}

fn until<T>(mut ready: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = ready() {
            return value;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the plugin thread is too slow"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_plugin_runs_with_the_values_a_score_names() {
    let rig = Rig::new("run");
    let found = rig.host.plugins();
    assert_eq!(found.len(), 1);
    assert_eq!(
        (found[0].name.as_str(), &found[0].status),
        (NAME, &Status::Found)
    );

    // A score writes the name in its own way.
    let Resolved::Ready(id, plugin) = rig.host.resolve("rustel-fixture", true) else {
        panic!("the fixture plugin did not load");
    };
    let keys: Vec<&str> = plugin
        .params()
        .iter()
        .map(|param| param.key.as_str())
        .collect();
    assert_eq!(keys, ["gain", "beatgate", "bypass", "delay"]);
    assert_eq!(plugin.param("Beat Gate"), Some(PARAM_GATE));
    assert_eq!(plugin.param("100"), Some(PARAM_GAIN));
    assert_eq!(plugin.param("mix"), None);
    assert!(!plugin.is_instrument());
    let loaded = &rig.host.plugins()[0];
    assert_eq!(
        (loaded.status.clone(), loaded.vendor.as_str()),
        (Status::Ready, "rustel")
    );
    // No copy of the plugin ran, so the list has no latency for it.
    assert_eq!(loaded.latency, None);
    assert_eq!(loaded.params[0].default_text, "1.00");
    // The list shows the number a score writes, and the text of the
    // plugin when the text is a different thing.
    assert_eq!(loaded.params[0].default_shown(), "1");
    assert_eq!(loaded.params[2].default_shown(), "0 = off");
    assert!(matches!(
        rig.host.resolve("no such plugin", true),
        Resolved::Missing
    ));

    // The call of a score names the key of its insert. This copy spins
    // 200 µs in each process call, as a heavy plugin does, and notes the
    // thread of each call.
    let key = InsertKey {
        plugin: id,
        preset: 0,
    };
    let called = rig.host.key("rustel-fixture", None, false);
    assert_eq!(called.map(|found| found.map(|(key, _)| key)), Ok(Some(key)));
    let mark = rig.root.join("process");
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variables in the build below. No other thread reads
    // a variable now.
    unsafe {
        std::env::set_var(COST_ENV, "200");
        std::env::set_var(PROCESS_MARK_ENV, &mark);
    }
    let mut insert = rig.insert(0).expect("insert");
    // SAFETY: as above. The copy read the variables at its build.
    unsafe {
        std::env::remove_var(COST_ENV);
        std::env::remove_var(PROCESS_MARK_ENV);
    }
    assert_eq!(insert.key(), key);
    // The list has the latency of the copy now: the plugin with no delay
    // gives 0.
    assert_eq!(rig.host.plugins()[0].latency, Some(0));
    // An engine runs the blocks of a copy on any thread, one thread at a
    // time. A plugin on the audio-thread list keeps its copies in the
    // callback. The list holds plugin names, so the instrument of the same
    // bundle still runs on any thread.
    assert!(insert.any_thread());
    rig.host
        .set_audio_thread_plugins(&["rustel-fixture".to_owned()]);
    assert!(!rig.insert(0).expect("insert").any_thread());
    let listed = rig.host.plugins().into_iter();
    let listed: Vec<(String, bool)> = listed.map(|row| (row.name, row.audio_thread)).collect();
    assert_eq!(
        listed,
        [(NAME.to_owned(), true), (TONE_NAME.to_owned(), false)]
    );
    let Resolved::Ready(tone, _) = rig.host.resolve(TONE_NAME, true) else {
        panic!("the instrument did not load");
    };
    let tone = InsertKey {
        plugin: tone,
        preset: 0,
    };
    let tone = (rig.host.provider(true))(tone, SAMPLE_RATE, InsertSlot::instrument(1));
    assert!(tone.expect("instrument").any_thread());
    let start = Instant::now();
    assert_eq!(block(insert.as_mut(), 1.0, 128), vec![1.0; 128]);
    assert!(start.elapsed() >= Duration::from_micros(200));

    // The audio callback sets a value and runs the plugin with no
    // allocation, for each block size.
    let mut left = [1.0f32; 128];
    let mut right = [1.0f32; 128];
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        insert.set_param(
            InsertParam {
                id: PARAM_GAIN,
                value: 0.25,
            },
            0,
        );
        for frames in [128, 1, 37] {
            insert.process(&mut left[..frames], &mut right[..frames]);
        }
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "the plugin call allocated or freed: {delta:?}"
    );
    // Frame 0 went through 3 calls, the next 36 frames through 2.
    assert_eq!(left[0], 0.25 * 0.25 * 0.25);
    assert_eq!(left[1..37], [0.25 * 0.25; 36]);
    assert_eq!(left[37..], [0.25; 91]);

    // Two notes in one block give two values. Each value starts at the
    // frame of its note.
    let gain = |value| InsertParam {
        id: PARAM_GAIN,
        value,
    };
    insert.set_param(gain(0.0), 64);
    insert.set_param(gain(1.0), 0);
    insert.set_param(gain(0.5), 200);
    let output = [
        block(insert.as_mut(), 1.0, 128),
        block(insert.as_mut(), 1.0, 128),
    ]
    .concat();
    assert_eq!(output[..64], [1.0; 64]);
    assert_eq!(output[64..200], [0.0; 136]);
    assert_eq!(output[200..], [0.5; 56]);

    // A rewind takes away a value that waits for a frame after the rewind.
    insert.set_param(gain(0.0), 100);
    insert.cut_notes(40);
    assert_eq!(block(insert.as_mut(), 1.0, 128), vec![0.5; 128]);

    // The plugin with its bypass on writes no output: the input passes.
    let bypass = |value| InsertParam {
        id: PARAM_BYPASS,
        value,
    };
    insert.set_param(bypass(1.0), 0);
    assert_eq!(block(insert.as_mut(), 0.75, 128), vec![0.75; 128]);
    insert.set_param(bypass(0.0), 0);
    assert_eq!(block(insert.as_mut(), 0.75, 128), vec![0.375; 128]);

    // A note with no gain puts the gain back to its value at the start.
    insert.restore_params(0);
    assert_eq!(block(insert.as_mut(), 0.75, 128), vec![0.75; 128]);

    // A note drops the value of a key that is no parameter, and each value
    // after the first 16. The plugin runs with the other values.
    let (controls, dropped) = plugin.controls(key, &[("mix", 0.9), ("gain", 0.25)]);
    assert_eq!(
        dropped,
        [format!("{NAME} has no parameter with the name 'mix'")]
    );
    for param in controls.params() {
        insert.set_param(*param, 0);
    }
    assert_eq!(block(insert.as_mut(), 1.0, 128), vec![0.25; 128]);
    let (controls, dropped) = plugin.controls(key, &[("gain", 0.5); 17]);
    assert_eq!(controls.params().len(), 16);
    assert_eq!(
        dropped,
        [format!(
            "a note carries 16 values for {NAME} at most, and 'gain' is one more"
        )]
    );

    // The copy writes its mark at its end, on the plugin thread. That
    // thread ran the first blocks at the build, and this thread ran the
    // others, one call at a time.
    drop(insert);
    rig.host.wait_idle();
    let line = std::fs::read_to_string(&mark).expect("process mark");
    assert_eq!(line, "process threads 2, overlap no\n");
}

/// The watch of the preset folders reads the disk again when a file comes
/// in or goes out: a preset the player saves while the studio runs counts
/// with no rescan and no ask. Each read takes the next number.
#[test]
fn the_watch_of_the_preset_folders_reads_the_disk_again() {
    let rig = Rig::new("watch");
    // The load gives the plugin its class, so the names have an answer,
    // and the read of the folders ends with the watch on.
    rig.plugin();
    let names = |wanted: &[&str]| {
        rig.host.presets(NAME).is_some_and(|names| {
            names
                == wanted
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<Vec<_>>()
        })
    };
    let wait = |wanted: &[&str]| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !names(wanted) {
            assert!(
                Instant::now() < deadline,
                "presets of the watch: {wanted:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    wait(&[]);
    let version = rig.host.presets_version();
    rig.write_preset(
        "Late Save.vstpreset",
        &rustel_vst3_fixture::preset(0.5, 0.0),
    );
    wait(&["Late Save"]);
    // A file that goes out leaves the list the same way.
    std::fs::remove_file(
        rig.root
            .join("presets")
            .join(NAME)
            .join("Late Save.vstpreset"),
    )
    .expect("preset file");
    wait(&[]);
    assert!(rig.host.presets_version() > version);
    // The quiet stays quiet: no read of the watch with no change behind
    // it, or the watch would read the disk for good.
    let settled = rig.host.presets_version();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(rig.host.presets_version(), settled);
}

#[test]
fn a_preset_file_sets_the_state_of_the_plugin() {
    let rig = Rig::new("preset");
    rig.write_preset(
        "Half Level.vstpreset",
        &rustel_vst3_fixture::preset(0.5, 0.0),
    );
    rig.write_preset(
        "default.vstpreset",
        &rustel_vst3_fixture::preset_with_delay(0.5, 0.0, 0.25),
    );
    let mut other = rustel_vst3_fixture::preset(0.5, 0.0);
    other[8] = b'0';
    rig.write_preset("Other Plugin.vstpreset", &other);
    // A prepare before the load of the plugin builds the copy of the
    // default preset.
    let parked = InsertSlot::effect(0, 0);
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, parked);
    rig.host.wait_idle();
    let plugin = rig.plugin();
    let names = rig.host.presets(NAME).expect("the load read the presets");
    assert_eq!(names, ["Half Level", "Other Plugin", "default"]);
    assert_eq!(rig.host.preset(plugin, "missing"), FoundPreset::Missing);
    assert_eq!(
        rig.host.key(NAME, Some("missing"), false).err(),
        Some(format!("{NAME} has no preset with the name 'missing'"))
    );

    let number = |name: &str| match rig.host.preset(plugin, name) {
        FoundPreset::Number(number) => number,
        other => panic!("preset {name}: {other:?}"),
    };
    let preset = number("half level");
    assert_eq!(number("HalfLevel"), preset);
    // A call with no preset plays the file `default`.
    let default = InsertKey {
        plugin,
        preset: number("default"),
    };
    let key = rig
        .host
        .key(NAME, None, false)
        .map(|key| key.map(|(key, _)| key));
    assert_eq!(key, Ok(Some(default)));
    // The copy the prepare parked has the latency the plugin gave at its
    // build: the delay of the preset, 0.25 of 4096 frames. The host built
    // no copy of the key at another rate.
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, parked), Some(1024));
    assert_eq!(rig.host.latency(default, 44_100, parked), None);
    let mut insert = rig.insert(preset).expect("insert");
    // The copy of the Half Level preset has a latency of its own. Its state
    // of 16 bytes has no delay.
    let half = InsertKey { plugin, preset };
    let slot = InsertSlot::effect(1, 0);
    assert_eq!(rig.host.latency(half, SAMPLE_RATE, slot), Some(0));
    // The plugin list has the largest latency of the copies.
    assert_eq!(rig.host.plugins()[0].latency, Some(1024));
    assert_eq!(block(insert.as_mut(), 1.0, 64), vec![0.5; 64]);
    // The start values of the copy are the values of the preset.
    let full = InsertParam {
        id: PARAM_GAIN,
        value: 1.0,
    };
    insert.set_param(full, 0);
    assert_eq!(block(insert.as_mut(), 1.0, 64), vec![1.0; 64]);
    insert.restore_params(0);
    assert_eq!(block(insert.as_mut(), 1.0, 64), vec![0.5; 64]);

    // A preset of a different plugin gives no insert, and the user gets
    // the reason.
    let wrong = number("other plugin");
    assert!(rig.insert(wrong).is_none());
    let errors = rig.host.take_errors();
    assert_eq!(
        errors,
        [format!(
            "vst skipped: {NAME} did not start: the preset is for a different plugin"
        )]
    );

    // Failed owners share the parked-copy bound. Forget the oldest
    // failure first, keep the ready copy and suppress a recent failure.
    std::fs::remove_file(
        rig.root
            .join("presets")
            .join(NAME)
            .join("Other Plugin.vstpreset"),
    )
    .expect("remove the preset");
    let key = InsertKey {
        plugin,
        preset: wrong,
    };
    let provider = rig.host.provider(true);
    let capacity = rustel_audio::INSERT_SLOTS as u64;
    let mut said = Vec::new();
    for chain in 2..=capacity {
        assert!(provider(key, SAMPLE_RATE, InsertSlot::effect(chain, 0)).is_none());
        said = rig.host.take_errors();
        assert_eq!(said.len(), 1);
    }
    let prepared = |chain| {
        rig.host.prepared(
            NAME,
            Some("Other Plugin"),
            false,
            SAMPLE_RATE,
            InsertSlot::effect(chain, 0),
        )
    };
    assert_eq!(prepared(1), Prepared::Unbuilt(key));
    let Prepared::Unavailable(reason) = prepared(capacity) else {
        panic!("the failed build is not the answer");
    };
    assert!(
        reason.starts_with(&format!("{NAME} did not start: ")),
        "{reason}"
    );
    // The line of the host is the line a caller makes from the reason.
    assert_eq!(said, [format!("vst skipped: {reason}")]);
    assert_eq!(
        rig.host.prepared(NAME, None, false, SAMPLE_RATE, parked),
        Prepared::Ready(default)
    );
    assert!(provider(key, SAMPLE_RATE, InsertSlot::effect(capacity, 0)).is_none());
    assert!(rig.host.take_errors().is_empty());

    // After the failures the bound ends the oldest copy of a chain no
    // caller names, and never a copy of a named chain. The first caller
    // names the chain of the parked copy: its name and its preset resolve
    // as a call does, so the copy of the file `default` is the copy the
    // chain wants. A second caller names one more chain, and unnames no
    // chain of the first.
    let wanted = WantedCopy {
        slot: parked,
        name: "fixture".into(),
        preset: None,
    };
    rig.host.keep(1, vec![wanted.clone()], vec![0], Vec::new());
    let bare = InsertKey { plugin, preset: 0 };
    assert!(
        provider(bare, SAMPLE_RATE, parked).is_none(),
        "the start state"
    );
    let first = capacity + 1;
    rig.host.keep(2, Vec::new(), vec![first], Vec::new());
    let ready = |chain| {
        rig.host
            .prepared(NAME, None, false, SAMPLE_RATE, InsertSlot::effect(chain, 0))
    };
    for chain in first..=capacity * 2 {
        rig.host
            .prepare(NAME, None, false, SAMPLE_RATE, InsertSlot::effect(chain, 0));
        rig.host.wait_idle();
    }
    assert_eq!(prepared(capacity), Prepared::Unbuilt(key), "a failure");
    for (chain, kept) in [
        (0, true),
        (first, true),
        (first + 1, false),
        (first + 2, true),
    ] {
        let answer = if kept {
            Prepared::Ready(default)
        } else {
            Prepared::Unbuilt(default)
        };
        assert_eq!(ready(chain), answer, "chain {chain}");
    }
    // With each chain named, a new build does not start. Two such builds
    // give one line.
    let mut named = vec![0];
    named.extend(first + 2..=capacity * 2);
    rig.host.keep(1, vec![wanted], named, Vec::new());
    let more = capacity * 2 + 1;
    for chain in [more, more + 1] {
        rig.host
            .prepare(NAME, None, false, SAMPLE_RATE, InsertSlot::effect(chain, 0));
    }
    rig.host.wait_idle();
    assert_eq!(ready(more), Prepared::Unbuilt(default));
    assert_eq!(
        rig.host.take_errors(),
        [format!(
            "vst skipped: {NAME} did not start: the host keeps {capacity} copies ready at most"
        )]
    );
    // A note takes the parked copy with no second build. The slot gives the
    // latency of the copy while the engine holds it, and after. Through the
    // copy, the input comes out 1024 frames late.
    let mut held =
        (rig.host.provider(false))(default, SAMPLE_RATE, parked).expect("the parked copy");
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, parked), Some(1024));
    let mut output = Vec::new();
    for _ in 0..16 {
        output.extend(block(held.as_mut(), 1.0, 128));
    }
    assert_eq!(output[..1024], [0.0; 1024]);
    assert_eq!(output[1024..], [0.5; 1024]);
    drop(held);
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, parked), Some(1024));
    // The take made room for one more copy. A new file `default` gives the
    // next copy of the key a delay of 2048 frames. The copy that waits for
    // its slot in the chain `first` keeps the latency of its own build. A
    // slot with no copy gives the latency of the last copy.
    rig.write_preset(
        "default.vstpreset",
        &rustel_vst3_fixture::preset_with_delay(0.5, 0.0, 0.5),
    );
    rig.host
        .prepare(NAME, None, false, SAMPLE_RATE, InsertSlot::effect(more, 0));
    rig.host.wait_idle();
    let waiting = InsertSlot::effect(first, 0);
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, waiting), Some(1024));
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, parked), Some(2048));
    // A note takes the copy that waits in the chain `first`. While the
    // engine holds it, the slot gives the latency of that copy. After the
    // drop, the slot gives the latency of the last copy.
    let held = (rig.host.provider(false))(default, SAMPLE_RATE, waiting).expect("the waiting copy");
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, waiting), Some(1024));
    drop(held);
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, waiting), Some(2048));

    // A preset file from elsewhere goes to the folder of the plugin of its
    // class, one time. A file of a class no plugin has goes nowhere.
    let dropped = rig.root.join("Drop.vstpreset");
    std::fs::write(&dropped, rustel_vst3_fixture::preset(0.5, 0.0)).expect("dropped file");
    let added = rig.host.add_preset(&dropped);
    assert_eq!(added, Ok((NAME.to_owned(), "Drop".to_owned())));
    assert!(rig.host.add_preset(&dropped).is_err());
    rig.host.presets(NAME);
    rig.host.wait_idle();
    assert!(matches!(
        rig.host.preset(plugin, "drop"),
        FoundPreset::Number(_)
    ));
    std::fs::write(&dropped, &other).expect("file of another class");
    let reason = rig
        .host
        .add_preset(&dropped)
        .expect_err("no plugin has the class");
    assert!(reason.starts_with("no plugin of the scan"), "{reason}");
}

#[test]
fn a_plugin_gets_the_beat_position_of_each_block() {
    let rig = Rig::new("clock");
    let mut insert = rig.insert(0).expect("insert");
    insert.set_param(
        InsertParam {
            id: PARAM_GATE,
            value: 1.0,
        },
        0,
    );
    // 120 beats in one minute: one beat is 24 000 frames. The gate lets
    // the first half of each beat through.
    let mut output = Vec::new();
    while output.len() < 48_000 {
        output.extend(block(insert.as_mut(), 1.0, 128));
    }
    for beat in [0, 24_000] {
        assert!(
            output[beat + 10..beat + 11_990]
                .iter()
                .all(|sample| *sample == 1.0)
        );
        assert!(
            output[beat + 12_010..beat + 23_990]
                .iter()
                .all(|sample| *sample == 0.0)
        );
    }

    // A note gives the clock of the score: 64 frames from now the position
    // is beat 2.5, at 60 beats in one minute. One beat is now 48 000 frames,
    // and the second half of the beat is silent.
    insert.sync(2.5, 60.0, 64);
    let mut output = Vec::new();
    while output.len() < 48_000 {
        output.extend(block(insert.as_mut(), 1.0, 128));
    }
    assert!(output[..54].iter().all(|sample| *sample == 1.0));
    assert!(output[74..23_990].iter().all(|sample| *sample == 0.0));
    assert!(output[24_074..47_990].iter().all(|sample| *sample == 1.0));
}

/// The second plugin of the bundle is an instrument. A note starts at its
/// frame, sounds at its pitch and level, and ends after its length.
#[test]
fn an_instrument_plays_the_note_it_gets() {
    let rig = Rig::new("tone");
    let Resolved::Ready(plugin, tone) = rig.host.resolve(TONE_NAME, true) else {
        panic!("the instrument did not load");
    };
    assert!(tone.is_instrument());
    assert!(matches!(
        rig.host.resolve("rustel fixture organ", true),
        Resolved::Missing
    ));
    let key = InsertKey { plugin, preset: 0 };
    let mut insert =
        (rig.host.provider(true))(key, SAMPLE_RATE, InsertSlot::instrument(1)).expect("insert");
    // A 4 800 Hz note: 10 frames for one period. The note starts 16 frames
    // from now and lasts 100 frames.
    let pitch = 69.0 + 12.0 * (4_800.0f32 / 440.0).log2();
    let note = InsertNote {
        pitch,
        velocity: 0.5,
        frames: 100,
    };
    insert.note(note, 16);
    let mut output = Vec::new();
    for frames in [10, 128, 62] {
        output.extend(block(insert.as_mut(), 0.0, frames));
    }
    assert!(output[..16].iter().all(|sample| *sample == 0.0));
    assert!(output[116..].iter().all(|sample| *sample == 0.0));
    let peak = output.iter().fold(0.0f32, |peak, sample| peak.max(*sample));
    assert!((peak - 0.5 * TONE_LEVEL).abs() < 0.01, "peak {peak}");
    let rises = output
        .windows(2)
        .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
        .count();
    assert_eq!(rises, 10, "100 frames hold 10 periods");

    // A rewind 40 frames from now ends the note that sounds there, and a
    // note that starts before the rewind. A note after the rewind plays.
    insert.note(note, 0);
    insert.cut_notes(40);
    insert.note(note, 20);
    insert.note(note, 300);
    let output = [
        block(insert.as_mut(), 0.0, 128),
        block(insert.as_mut(), 0.0, 128),
        block(insert.as_mut(), 0.0, 128),
        block(insert.as_mut(), 0.0, 128),
    ]
    .concat();
    assert!(output[1..40].iter().any(|sample| *sample != 0.0));
    assert!(output[40..300].iter().all(|sample| *sample == 0.0));
    assert!(output[301..400].iter().any(|sample| *sample != 0.0));
    assert!(output[400..].iter().all(|sample| *sample == 0.0));

    // A note 1 frame longer than the time to the next note on its key ends
    // where the next note starts. The plugin ends a note by its key, so an
    // end after that start would end the new note.
    insert.note(
        InsertNote {
            frames: 101,
            ..note
        },
        0,
    );
    insert.note(note, 100);
    let output = [
        block(insert.as_mut(), 0.0, 128),
        block(insert.as_mut(), 0.0, 128),
    ]
    .concat();
    assert!(output[102..200].iter().any(|sample| *sample != 0.0));
    assert!(output[200..].iter().all(|sample| *sample == 0.0));

    // Coincident notes on one key keep the longer duration without
    // starting two voices whose first note-off would end both.
    for lengths in [[100, 200], [200, 100]] {
        for frames in lengths {
            insert.note(InsertNote { frames, ..note }, 0);
        }
        let output = [
            block(insert.as_mut(), 0.0, 128),
            block(insert.as_mut(), 0.0, 128),
        ]
        .concat();
        assert!(output[102..200].iter().any(|sample| *sample != 0.0));
        assert!(output[200..].iter().all(|sample| *sample == 0.0));
        assert!(output.iter().all(|sample| sample.abs() <= 0.5 * TONE_LEVEL));
    }
    // Extending a coincident note still stops at the next note on its key.
    insert.note(note, 0);
    insert.note(note, 150);
    insert.note(
        InsertNote {
            frames: 300,
            ..note
        },
        0,
    );
    let output = [
        block(insert.as_mut(), 0.0, 128),
        block(insert.as_mut(), 0.0, 128),
    ]
    .concat();
    assert!(output[202..250].iter().any(|sample| *sample != 0.0));
    assert!(output[250..].iter().all(|sample| *sample == 0.0));

    // A reset ends a note that sounds and drops a note that did not start.
    insert.note(note, 0);
    insert.note(note, 500);
    assert!(
        block(insert.as_mut(), 0.0, 32)
            .iter()
            .any(|sample| *sample != 0.0)
    );
    insert.reset();
    let mut after = Vec::new();
    for _ in 0..8 {
        after.extend(block(insert.as_mut(), 0.0, 128));
    }
    assert!(after.iter().all(|sample| *sample == 0.0));

    // A note on each of the 128 keys starts and ends in one block: 256
    // events. Each note sounds for its 100 frames, and each one ends.
    let (mut left, mut right) = ([0.0f32; 128], [0.0f32; 128]);
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        for key in 0..128u8 {
            let note = InsertNote {
                pitch: f32::from(key),
                velocity: 1.0,
                frames: 100,
            };
            insert.note(note, 16);
        }
        insert.process(&mut left, &mut right);
    });
    let delta = Violations::capture().since(before);
    assert!(delta.clean(), "the notes allocated or freed: {delta:?}");
    let step = |key: i32| {
        let hertz = 440.0 * (f64::from(key - 69) / 12.0).exp2();
        std::f64::consts::TAU * hertz / f64::from(SAMPLE_RATE)
    };
    for (frame, sample) in left.iter().enumerate() {
        let expected: f32 = match frame {
            16..116 => (0..128)
                .map(|key| ((frame - 16) as f64 * step(key)).sin() as f32 * TONE_LEVEL)
                .sum(),
            _ => 0.0,
        };
        assert!((sample - expected).abs() < 1e-3, "frame {frame}: {sample}");
    }
    assert_eq!(right, left);
    assert!(
        block(insert.as_mut(), 0.0, 128)
            .iter()
            .all(|sample| *sample == 0.0)
    );

    // An engine with 2 plugin workers plays the note on a copy, and an
    // engine with no workers plays it on a second copy: the samples are the
    // same. The first engine leaves each block of 32 frames or more to the
    // workers, and the workers allocate and free nothing. The first copy
    // notes the threads of its blocks.
    let mark = rig.root.join("process");
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variable in the build below. No other thread reads a
    // variable now.
    unsafe { std::env::set_var(PROCESS_MARK_ENV, &mark) };
    let pooled_copy = (rig.host.provider(true))(key, SAMPLE_RATE, InsertSlot::instrument(2));
    // SAFETY: as above. The copy read the variable at its build.
    unsafe { std::env::remove_var(PROCESS_MARK_ENV) };
    let serial_copy = (rig.host.provider(true))(key, SAMPLE_RATE, InsertSlot::instrument(3));
    let mut pooled = ScalarBackend::prepared(SAMPLE_RATE, 1).expect("engine");
    let workers = pooled.start_test_pool(2).expect("plugin workers");
    pooled.leave_plugin_blocks_to_workers();
    let mut serial = ScalarBackend::prepared(SAMPLE_RATE, 1).expect("engine");
    for (engine, copy) in [(&mut pooled, pooled_copy), (&mut serial, serial_copy)] {
        let slot = InsertSlot::instrument(1);
        assert!(engine.install_insert(slot, copy.expect("copy")).is_none());
        let mut event = OnsetEvent::new(16, 440.0, 1.0, 0.01);
        event.controls.insert_chain = Some(slot.chain);
        event.controls.instrument = Some(InsertControls::new(key).with_note(note));
        assert!(engine.try_note_prepared(event));
    }
    let (mut on_workers, mut in_callback) = ([0.0f32; 2 * 480], [0.0f32; 2 * 480]);
    let before = Violations::capture();
    // The first block is under 32 frames, so it stays in the callback.
    for (index, frames) in [16, 480, 480, 480].into_iter().enumerate() {
        let (on_workers, in_callback) = (
            &mut on_workers[..2 * frames],
            &mut in_callback[..2 * frames],
        );
        tripwire::audio_scope(|| {
            pooled.process_block(on_workers, frames);
            serial.process_block(in_callback, frames);
        });
        assert_eq!(on_workers, in_callback, "block {index}");
        assert_eq!(
            in_callback.iter().any(|sample| *sample != 0.0),
            index == 1,
            "block {index}"
        );
    }
    let delta = Violations::capture().since(before);
    assert!(delta.clean(), "a block allocated or freed: {delta:?}");
    // The copy writes its mark at its end, on the plugin thread. That
    // thread ran the first blocks at the build, this thread ran the short
    // block, and 1 or 2 workers ran the others, one call at a time.
    drop((workers, pooled, serial));
    rig.host.wait_idle();
    let line = std::fs::read_to_string(&mark).expect("process mark");
    assert!(
        [
            "process threads 3, overlap no\n",
            "process threads 4, overlap no\n"
        ]
        .contains(&line.as_str()),
        "{line}"
    );
}

/// Not a test: the entry of a scan process. The scan test gives this binary
/// to the host as its worker program, so the bundle loads in a process of
/// its own, as in the product. The host adds the bundle path as the last
/// argument. With the argument `fault=<value>`, the process stops as the
/// fixture variable of that value asks. With the argument `mark=<path>`,
/// the process writes one line to the file before its load, so a test
/// counts the runs of the worker. With the argument `broken=tone`, the
/// instrument of the bundle does not start.
#[test]
fn plugin_worker() {
    let args: Vec<String> = std::env::args().collect();
    // An ordinary run of the tests has no bundle path.
    let Some(bundle) = args.last().filter(|arg| arg.ends_with(".vst3")) else {
        return;
    };
    if let Some(mark) = args.iter().find_map(|arg| arg.strip_prefix("mark=")) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(mark)
            .expect("mark file");
        writeln!(file, "{bundle}").expect("mark line");
    }
    if let Some(fault) = args.iter().find_map(|arg| arg.strip_prefix("fault=")) {
        // SAFETY: this process is the scan process, and no other thread of
        // it reads a variable now.
        unsafe {
            std::env::set_var(rustel_vst3_fixture::ABORT_ENV, fault);
        }
    }
    if args.iter().any(|arg| arg == "broken=tone") {
        // SAFETY: as for the fault above.
        unsafe {
            std::env::set_var(rustel_vst3_fixture::BROKEN_TONE_ENV, "1");
        }
    }
    std::process::exit(rustel_vst3::serve(std::path::Path::new(bundle)));
}

fn worker_program(fault: Option<&str>) -> WorkerProgram {
    let mut args = vec!["plugin_worker".into(), "--exact".into()];
    args.extend(fault.map(|fault| format!("fault={fault}").into()));
    WorkerProgram {
        program: std::env::current_exe().expect("test program path"),
        args,
    }
}

#[test]
fn repeated_resets_cancel_old_events_and_keep_the_first_resumed_events() {
    let rig = Rig::new("reset");
    let mut effect = rig.insert(0).expect("effect");
    effect.set_param(
        InsertParam {
            id: PARAM_GAIN,
            value: 0.0,
        },
        500,
    );
    effect.cut_notes(1000);
    assert_eq!(block(effect.as_mut(), 1.0, 1), [1.0]);
    // Stopped callbacks do not process audio. Many resets must leave
    // room for the first resumed value.
    for _ in 0..600 {
        effect.reset();
    }
    effect.set_param(
        InsertParam {
            id: PARAM_GAIN,
            value: 0.75,
        },
        0,
    );
    for _ in 0..8 {
        assert_eq!(block(effect.as_mut(), 1.0, 128), [0.75; 128]);
    }

    // Canceling future values and restores keeps the last delivered
    // value available for the first resumed note to restore.
    for reset in [false, true] {
        effect.set_param(
            InsertParam {
                id: PARAM_GAIN,
                value: 0.5,
            },
            0,
        );
        assert_eq!(block(effect.as_mut(), 1.0, 1), [0.5]);
        effect.set_param(
            InsertParam {
                id: PARAM_GAIN,
                value: 0.25,
            },
            500,
        );
        effect.restore_params(700);
        if reset {
            effect.reset();
        } else {
            effect.cut_notes(40);
        }
        effect.restore_params(0);
        assert_eq!(block(effect.as_mut(), 1.0, 1), [1.0]);
        effect.set_param(
            InsertParam {
                id: PARAM_GAIN,
                value: 0.75,
            },
            0,
        );
        for _ in 0..8 {
            assert_eq!(block(effect.as_mut(), 1.0, 128), [0.75; 128]);
        }
    }

    let Resolved::Ready(plugin, _) = rig.host.resolve(TONE_NAME, true) else {
        panic!("the instrument did not load");
    };
    let key = InsertKey { plugin, preset: 0 };
    let mut instrument =
        (rig.host.provider(true))(key, SAMPLE_RATE, InsertSlot::instrument(1)).expect("instrument");
    let note = InsertNote {
        pitch: 69.0,
        velocity: 0.5,
        frames: 1000,
    };
    instrument.note(note, 0);
    instrument.cut_notes(500);
    assert!(
        block(instrument.as_mut(), 0.0, 32)
            .iter()
            .any(|sample| *sample != 0.0)
    );
    instrument.note(note, 700);
    for _ in 0..600 {
        instrument.reset();
    }
    instrument.note(note, 0);
    let mut output = Vec::new();
    for _ in 0..8 {
        output.extend(block(instrument.as_mut(), 0.0, 128));
    }
    assert!(output[1..500].iter().any(|sample| *sample != 0.0));
    assert!(output[500..1000].iter().any(|sample| *sample != 0.0));
    assert!(output[1000..].iter().all(|sample| *sample == 0.0));

    // A cut keeps sounding and earlier pending notes until its frame,
    // cancels later pending notes, and accepts notes queued afterward.
    instrument.note(note, 0);
    assert!(
        block(instrument.as_mut(), 0.0, 32)
            .iter()
            .any(|sample| *sample != 0.0)
    );
    instrument.note(note, 20);
    instrument.note(note, 40);
    instrument.note(note, 500);
    instrument.cut_notes(40);
    instrument.note(
        InsertNote {
            frames: 100,
            ..note
        },
        300,
    );
    let mut output = Vec::new();
    for _ in 0..8 {
        output.extend(block(instrument.as_mut(), 0.0, 128));
    }
    assert!(output[1..20].iter().any(|sample| *sample != 0.0));
    assert!(output[21..40].iter().any(|sample| *sample != 0.0));
    assert!(output[40..300].iter().all(|sample| *sample == 0.0));
    assert!(output[301..400].iter().any(|sample| *sample != 0.0));
    assert!(output[400..].iter().all(|sample| *sample == 0.0));
}

#[test]
fn parameter_restores_follow_note_time_and_survive_a_full_queue() {
    let rig = Rig::new("restore");
    let mut insert = rig.insert(0).expect("effect");
    let gain = |value| InsertParam {
        id: PARAM_GAIN,
        value,
    };
    insert.set_param(gain(0.5), 0);
    assert_eq!(block(insert.as_mut(), 1.0, 128), [0.5; 128]);

    // A future value does not hide the value an earlier note restores.
    insert.set_param(gain(0.25), 128);
    insert.restore_params(0);
    assert_eq!(block(insert.as_mut(), 1.0, 128), [1.0; 128]);
    assert_eq!(block(insert.as_mut(), 1.0, 128), [0.25; 128]);

    // The later restore sometimes arrives before the value to clear.
    insert.restore_params(64);
    insert.set_param(gain(0.5), 32);
    let output = block(insert.as_mut(), 1.0, 128);
    assert_eq!(output[..32], [0.25; 32]);
    assert_eq!(output[32..64], [0.5; 32]);
    assert_eq!(output[64..], [1.0; 64]);

    // Coincident notes share all of their values, regardless of which
    // note queues its restore first.
    insert.set_param(gain(0.5), 0);
    insert.restore_params(0);
    insert.set_param(
        InsertParam {
            id: PARAM_BYPASS,
            value: 1.0,
        },
        0,
    );
    insert.restore_params(0);
    assert_eq!(block(insert.as_mut(), 1.0, 128), [1.0; 128]);
    insert.set_param(
        InsertParam {
            id: PARAM_BYPASS,
            value: 0.0,
        },
        0,
    );
    assert_eq!(block(insert.as_mut(), 1.0, 128), [0.5; 128]);

    // The queue holds 512 values. A restore rejected by the full queue
    // must not forget the delivered values a later restore still needs to
    // clear.
    let gate = InsertParam {
        id: PARAM_GATE,
        value: 0.0,
    };
    let (mut left, mut right) = ([1.0; 128], [1.0; 128]);
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        for frame in 128..639 {
            insert.set_param(gate, frame);
        }
        insert.set_param(gain(0.25), 639);
        insert.restore_params(0);
        insert.process(&mut left, &mut right);
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "the full queue allocated or freed: {delta:?}"
    );
    assert_eq!(left, [0.5; 128]);
    for _ in 0..3 {
        assert_eq!(block(insert.as_mut(), 1.0, 128), [0.5; 128]);
    }
    // The last of the 512 values arrives at its frame.
    let output = block(insert.as_mut(), 1.0, 128);
    assert_eq!(output[..127], [0.5; 127]);
    assert_eq!(output[127], 0.25);
    let (mut left, mut right) = ([1.0; 128], [1.0; 128]);
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        insert.restore_params(0);
        insert.process(&mut left, &mut right);
    });
    let delta = Violations::capture().since(before);
    assert!(delta.clean(), "the restore allocated or freed: {delta:?}");
    assert_eq!(left, [1.0; 128]);
    assert_eq!(right, left);

    // A value set now sounds from the next block, and a value that waits
    // for its frame takes it too. A later restore puts the start value back.
    // A value that is not finite changes nothing.
    insert.set_param(gain(0.5), 64);
    insert.restore_params(96);
    let (mut left, mut right) = ([1.0; 128], [1.0; 128]);
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        insert.set_param_now(gain(0.25));
        insert.set_param_now(gain(f32::NAN));
        insert.process(&mut left, &mut right);
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "the value set now allocated or freed: {delta:?}"
    );
    assert_eq!(left[..96], [0.25; 96]);
    assert_eq!(left[96..], [1.0; 32]);
    // A value past 1 is 1.
    insert.set_param(gain(0.5), 0);
    insert.set_param_now(gain(4.0));
    assert_eq!(block(insert.as_mut(), 1.0, 128), [1.0; 128]);
}

/// The fixture instrument through the fixture effect, and the output of
/// its last block.
struct Chain {
    tone: Box<dyn OrbitInsert>,
    effect: Box<dyn OrbitInsert>,
    left: [f32; 128],
    right: [f32; 128],
}

impl Chain {
    /// Plays block `index` of a fixed score: blocks of 4 sizes, a note
    /// every 4 blocks and a gain every 3 blocks, each at a frame inside the
    /// block. Returns the frames of the block.
    fn play(&mut self, index: usize) -> usize {
        let frames = [128, 37, 91, 1][index % 4];
        let frame = |step: usize| (index * step % frames) as u32;
        if index.is_multiple_of(4) {
            let note = InsertNote {
                pitch: 60.0 + (index % 12) as f32,
                velocity: 0.5,
                frames: 300,
            };
            self.tone.note(note, frame(7));
        }
        if index.is_multiple_of(3) {
            let gain = InsertParam {
                id: PARAM_GAIN,
                value: 0.25 * (index % 4 + 1) as f32,
            };
            self.effect.set_param(gain, frame(13));
        }
        let (left, right) = (&mut self.left[..frames], &mut self.right[..frames]);
        self.tone.process(left, right);
        self.effect.process(left, right);
        frames
    }
}

/// An engine moves a copy between threads: the audio callback and the
/// threads that serve it. Each one runs the block calls of the copy in
/// its turn, one call at a time, and the copy gives the output of a copy
/// on one thread.
#[test]
fn a_copy_processed_by_three_threads_in_turn_gives_the_output_of_one_thread() {
    let rig = Rig::new("turns");
    let Resolved::Ready(tone, _) = rig.host.resolve(TONE_NAME, true) else {
        panic!("the instrument did not load");
    };
    let effect = rig.plugin();
    let provider = rig.host.provider(true);
    let chain = |chain: u64| {
        let copy = |plugin, slot| {
            let key = InsertKey { plugin, preset: 0 };
            provider(key, SAMPLE_RATE, slot).expect("copy")
        };
        Chain {
            tone: copy(tone, InsertSlot::instrument(chain)),
            effect: copy(effect, InsertSlot::effect(chain, 0)),
            left: [0.0; 128],
            right: [0.0; 128],
        }
    };
    // Each process call spins 100 µs. The copies of each chain note their
    // threads in a file of their own.
    let marks = [rig.root.join("one-thread"), rig.root.join("three-threads")];
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variables in the builds below. No other thread reads
    // a variable now.
    unsafe {
        std::env::set_var(COST_ENV, "100");
        std::env::set_var(PROCESS_MARK_ENV, &marks[0]);
    }
    let mut alone = chain(1);
    // SAFETY: as above.
    unsafe { std::env::set_var(PROCESS_MARK_ENV, &marks[1]) };
    let turns = chain(2);
    // SAFETY: as above. The copies read the variables at their build.
    unsafe {
        std::env::remove_var(COST_ENV);
        std::env::remove_var(PROCESS_MARK_ENV);
    }

    // This thread hands the chain to each of 3 threads in turn through a
    // channel, and plays the same block on the chain it keeps.
    let before = Violations::capture();
    let turns = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..3)
            .map(|_| {
                let (work, blocks) = std::sync::mpsc::channel::<(Chain, usize)>();
                let (done, played) = std::sync::mpsc::channel();
                scope.spawn(move || {
                    for (mut chain, index) in blocks {
                        let frames = tripwire::audio_scope(|| chain.play(index));
                        done.send((chain, frames)).expect("the test thread");
                    }
                });
                (work, played)
            })
            .collect();
        let mut turns = turns;
        for index in 0..60 {
            let (work, played) = &threads[index % 3];
            work.send((turns, index)).expect("a thread of the test");
            let frames;
            (turns, frames) = played.recv().expect("the block");
            let start = Instant::now();
            assert_eq!(alone.play(index), frames);
            // The instrument and the effect spin one after the other.
            assert!(start.elapsed() >= Duration::from_micros(200));
            assert_eq!(turns.left[..frames], alone.left[..frames], "block {index}");
            assert_eq!(
                turns.right[..frames],
                alone.right[..frames],
                "block {index}"
            );
        }
        turns
    });
    let delta = Violations::capture().since(before);
    assert!(delta.clean(), "a block allocated or freed: {delta:?}");

    // Each copy writes its mark at its end, on the plugin thread. That
    // thread ran the first blocks of each copy at its build: one thread
    // more than the threads of the blocks above.
    drop((alone, turns));
    rig.host.wait_idle();
    let marks = marks.map(|mark| std::fs::read_to_string(mark).expect("process mark"));
    assert_eq!(marks[0], "process threads 2, overlap no\n".repeat(2));
    assert_eq!(marks[1], "process threads 4, overlap no\n".repeat(2));
}

/// The scan loads a bundle in a process of its own. A bundle in order gives
/// the names and the kinds of its plugins, with no load in this process. A
/// bundle with a fault at its load fails, and the host goes on.
#[test]
fn the_scan_names_the_plugins_and_keeps_a_fault_out_of_the_host() {
    let scanned = |rig: &Rig, fault: Option<&str>| {
        rig.host.set_worker(Some(worker_program(fault)));
        rig.host.scan_plugins();
        // The plugin thread starts the scan thread.
        rig.host.wait_idle();
        until(|| rig.host.scan_progress().is_none().then_some(()));
        rig.host.plugins()
    };
    let rig = Rig::new("scan");
    let plugins = scanned(&rig, None);
    let rows: Vec<_> = plugins
        .iter()
        .map(|plugin| (plugin.name.as_str(), plugin.instrument, &plugin.status))
        .collect();
    assert_eq!(
        rows,
        [
            (NAME, false, &Status::Found),
            (TONE_NAME, true, &Status::Found)
        ]
    );
    assert!(rig.root.join("presets").join("scan.json").is_file());
    // Each plugin the scan names has a preset folder with its name.
    for name in [NAME, TONE_NAME] {
        assert!(rig.root.join("presets").join(name).is_dir(), "{name}");
    }

    let rig = Rig::new("scan-fault");
    let plugins = scanned(&rig, Some("load"));
    assert_eq!(plugins.len(), 1);
    assert!(!rig.root.join("presets").join(&plugins[0].name).exists());
    let Status::Failed(reason) = &plugins[0].status else {
        panic!("the bundle did not fail: {:?}", plugins[0].status);
    };
    assert!(reason.contains("the plugin process ended"), "{reason}");
    // A bundle that failed its scan does not load in this process.
    assert!(matches!(rig.host.resolve(NAME, true), Resolved::Failed(_)));

    // A worker program that does not start fails no bundle: the bundle
    // loads in this process when a score names it.
    let rig = Rig::new("scan-no-program");
    rig.host.set_worker(Some(rustel_vst3::WorkerProgram {
        program: rig.root.join("no-such-program"),
        args: Vec::new(),
    }));
    rig.host.scan_plugins();
    rig.host.wait_idle();
    until(|| rig.host.scan_progress().is_none().then_some(()));
    let plugins = rig.host.plugins();
    assert!(
        plugins.iter().all(|plugin| plugin.status == Status::Found),
        "{:?}",
        plugins
            .iter()
            .map(|plugin| &plugin.status)
            .collect::<Vec<_>>()
    );
    assert!(matches!(rig.host.resolve(NAME, true), Resolved::Ready(..)));
}

/// A bundle with one plugin that does not start at its scan keeps its
/// other plugins. The cache keeps the fault of that one plugin, so a host
/// that starts again shows the same list with no new scan.
#[test]
fn a_plugin_that_fails_its_scan_leaves_the_others_of_its_bundle() {
    let rig = Rig::new("scan-broken-tone");
    let mut program = worker_program(None);
    program.args.push("broken=tone".into());
    rig.host.set_worker(Some(program));
    rig.host.scan_plugins();
    rig.host.wait_idle();
    until(|| rig.host.scan_progress().is_none().then_some(()));
    let rows = |host: &Host| -> Vec<(String, Status)> {
        let plugins = host.plugins().into_iter();
        plugins.map(|plugin| (plugin.name, plugin.status)).collect()
    };
    let failed = Status::Failed("the plugin processor did not start".into());
    let expected = vec![
        (NAME.to_owned(), Status::Found),
        (TONE_NAME.to_owned(), failed),
    ];
    assert_eq!(rows(&rig.host), expected);

    let again = Host::new();
    again.set_preset_folder(rig.root.join("presets"));
    again.scan(&[rig.root.join("plugins")]);
    again.wait_idle();
    assert_eq!(rows(&again), expected);
}

/// A bundle that faults its scan keeps the fault in the cache file, with
/// the stamp of its files. A host that starts again reads the fault and
/// runs no new test of the bundle. A rescan empties the cache, so the
/// bundle gets its new test.
#[test]
fn a_scan_fault_stays_in_the_cache_until_a_rescan() {
    let root = std::env::temp_dir().join(format!("rustel-vst3-stay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("plugins")).expect("plugin folder");
    rustel_vst3_fixture::install(&root.join("plugins"));
    let folders = [root.join("plugins")];
    let mark = root.join("mark");
    let runs = || {
        std::fs::read_to_string(&mark)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let with_mark = |fault: Option<&str>| {
        let mut program = worker_program(fault);
        program.args.push(format!("mark={}", mark.display()).into());
        program
    };
    let start = || {
        let host = Host::new();
        host.set_preset_folder(root.join("presets"));
        host.set_worker(Some(with_mark(Some("load"))));
        host.scan(&folders);
        host.scan_plugins();
        host.wait_idle();
        host
    };

    let host = start();
    until(|| host.scan_progress().is_none().then_some(()));
    assert_eq!(runs(), 1);
    let plugins = host.plugins();
    let Status::Failed(reason) = &plugins[0].status else {
        panic!("the bundle did not fail: {:?}", plugins[0].status);
    };
    assert!(reason.contains("the plugin process ended"), "{reason}");

    // A host that starts again reads the fault of the cache file and runs
    // no new test of the bundle with the same files.
    let host = start();
    until(|| host.scan_progress().is_none().then_some(()));
    assert_eq!(runs(), 1);
    let plugins = host.plugins();
    assert!(
        matches!(plugins[0].status, Status::Failed(_)),
        "{plugins:?}"
    );

    // A rescan with no fault gives the bundle its plugins again.
    host.set_worker(Some(with_mark(None)));
    host.rescan(&folders);
    host.wait_idle();
    until(|| host.scan_progress().is_none().then_some(()));
    assert_eq!(runs(), 2);
    let names: Vec<String> = host.plugins().into_iter().map(|row| row.name).collect();
    assert_eq!(names, [NAME, TONE_NAME]);
    let _ = std::fs::remove_dir_all(&root);
}

/// A bundle with no plugin a score names and no running copy unloads. A
/// new load gives each plugin the number it had.
#[test]
fn an_unused_bundle_unloads_and_keeps_its_plugin_numbers() {
    let rig = Rig::new("unload");
    let number = |name: &str| match rig.host.resolve(name, true) {
        Resolved::Ready(number, _) => number,
        _ => panic!("{name} did not load"),
    };
    let (effect, tone) = (number(NAME), number(TONE_NAME));
    // A whole name or a bundle name counts, and a part of a name does not.
    let names = ["rustel fixture tone", "Rustel Fixture", "rustel", "e"].map(str::to_owned);
    assert_eq!(
        rig.host.whole_names(&names),
        ["rustel fixture tone", "Rustel Fixture"]
    );
    let loaded = |rig: &Rig| {
        let plugins = rig.host.plugins();
        plugins
            .iter()
            .filter(|plugin| plugin.status == Status::Ready)
            .count()
    };
    let unload = |keep: &[&str]| {
        let keep: Vec<String> = keep.iter().map(|name| (*name).to_owned()).collect();
        rig.host.unload_unused(&keep);
        rig.host.wait_idle();
    };
    // A name of one plugin keeps its bundle, and so the two plugins.
    unload(&["rustel fixture tone"]);
    assert_eq!(loaded(&rig), 2);
    // A running copy keeps the bundle. A copy that waits for a slot ends.
    let key = InsertKey {
        plugin: effect,
        preset: 0,
    };
    let slot = InsertSlot::effect(1, 0);
    let running = (rig.host.provider(true))(key, SAMPLE_RATE, slot).expect("insert");
    rig.host
        .prepare(NAME, None, false, SAMPLE_RATE, InsertSlot::effect(2, 0));
    rig.host.wait_idle();
    // The engine holds the running copy, and a name of its plugin keeps it.
    assert!(rig.host.engine_holds_unkept(&[]));
    assert!(!rig.host.engine_holds_unkept(&[NAME.to_owned()]));
    unload(&[]);
    assert_eq!(loaded(&rig), 2);
    assert_eq!(rig.host.plugins()[0].running, 1);
    drop(running);
    assert!(!rig.host.engine_holds_unkept(&[]));
    // The slot keeps the latency of its last copy until the plugin unloads.
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, slot), Some(0));
    unload(&[]);
    assert_eq!(loaded(&rig), 0);
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, slot), None);
    assert_eq!(rig.host.plugins().len(), 2, "the two rows stay in the list");

    assert_eq!((number(TONE_NAME), number(NAME)), (tone, effect));
    assert_eq!(loaded(&rig), 2);
}

#[test]
fn live_play_does_not_wait_for_a_plugin() {
    let rig = Rig::new("live");
    // A prepare before the load of the plugin: the build goes in the line
    // behind the load, one time for the slot.
    let early = InsertSlot::effect(4, 0);
    for _ in 0..2 {
        rig.host.prepare(NAME, None, false, SAMPLE_RATE, early);
    }
    let id = until(|| match rig.host.resolve(NAME, false) {
        Resolved::Ready(id, _) => Some(id),
        Resolved::Pending => None,
        _ => panic!("the fixture plugin did not load"),
    });
    let provider = rig.host.provider(false);
    let key = InsertKey {
        plugin: id,
        preset: 0,
    };
    // The first call starts the work and returns at once.
    assert!(provider(key, SAMPLE_RATE, InsertSlot::effect(3, 0)).is_none());
    let mut insert = until(|| provider(key, SAMPLE_RATE, InsertSlot::effect(3, 0)));
    assert_eq!(block(insert.as_mut(), 0.5, 16), vec![0.5; 16]);
    // The orbit took the plugin, so the next request builds a new one.
    assert!(provider(key, SAMPLE_RATE, InsertSlot::effect(3, 0)).is_none());

    // A start that waits for its plugins reads how far each one is. A
    // prepared plugin is ready for its slot before a note asks.
    let prepared = |name: &str, instrument| {
        rig.host
            .prepared(name, None, instrument, 44_100, InsertSlot::effect(5, 0))
    };
    assert_eq!(prepared(NAME, false), Prepared::Unbuilt(key));
    assert_eq!(
        prepared(NAME, true),
        Prepared::Unavailable(format!("{NAME} is an effect: use .vst()"))
    );
    assert_eq!(
        prepared("no such plugin", false),
        Prepared::Unavailable("no plugin has the name 'no such plugin'".into())
    );
    // A prepare marks its build at the call, also behind a build that
    // runs. A second look then reads a build that waits, and a second
    // prepare adds no job.
    let behind = InsertSlot::effect(6, 0);
    for slot in [InsertSlot::effect(5, 0), behind, behind] {
        rig.host.prepare(NAME, None, false, 44_100, slot);
    }
    let waits = rig.host.prepared(NAME, None, false, 44_100, behind);
    assert_ne!(waits, Prepared::Unbuilt(key));
    until(|| (prepared(NAME, false) == Prepared::Ready(key)).then_some(()));
    // The orbit has 2 copies, and each of the 3 prepared slots has one.
    rig.host.wait_idle();
    let built = rig.host.prepared(NAME, None, false, SAMPLE_RATE, early);
    assert_eq!(built, Prepared::Ready(key));
    assert_eq!(rig.host.plugins()[0].running, 5);
    let held = provider(key, 44_100, InsertSlot::effect(5, 0)).expect("the prepared plugin");

    // While the engine holds the plugin of the slot, a start has nothing
    // to wait for, and one more prepare builds no second copy.
    assert_eq!(prepared(NAME, false), Prepared::Ready(key));
    rig.host
        .prepare(NAME, None, false, 44_100, InsertSlot::effect(5, 0));
    rig.host.wait_idle();

    // keep() gives the slot and the key of a copy an engine holds that its
    // chain does not want, also when it ends a parked copy, and nothing for
    // a copy the chain wants. A second caller hears nothing of the chains of
    // the first. A copy the engine gave back is not held.
    let slot = InsertSlot::effect(5, 0);
    let running = || {
        rig.host.wait_idle();
        rig.host.plugins()[0].running
    };
    rig.host
        .prepare(NAME, None, false, 44_100, InsertSlot::effect(5, 2));
    let parked = running();
    let other = WantedCopy {
        slot: InsertSlot::effect(5, 1),
        name: NAME.to_owned(),
        preset: None,
    };
    let wanted = WantedCopy {
        slot,
        ..other.clone()
    };
    let keep = |copy: &WantedCopy| rig.host.keep(1, vec![copy.clone()], vec![5], Vec::new());
    assert_eq!(keep(&other), [(slot, key)]);
    assert_eq!(running(), parked - 1);
    assert_eq!(keep(&other), [(slot, key)]);
    assert!(rig.host.keep(2, Vec::new(), vec![6], Vec::new()).is_empty());
    rig.host.keep(2, Vec::new(), Vec::new(), Vec::new());
    assert!(keep(&wanted).is_empty());
    drop(held);
    // The slot with no copy gives the latency of the last copy of its key
    // at the rate.
    assert_eq!(rig.host.latency(key, 44_100, slot), Some(0));
    assert_eq!(rig.host.latency(key, 22_050, slot), None);
    assert!(keep(&other).is_empty());
    rig.host.keep(1, Vec::new(), Vec::new(), Vec::new());
    assert_eq!(prepared(NAME, false), Prepared::Unbuilt(key));
}

#[test]
fn different_plugins_prepared_for_one_slot_stay_ready() {
    let rig = Rig::new("shared-slot");
    rig.plugin();
    let slot = InsertSlot::effect(1, 0);
    let requests = [(NAME, false), (TONE_NAME, true)];
    // The chain of the slot wants both plugins, as a chain does a short
    // time after an edit swapped the plugin of the slot.
    let wanted = requests.map(|(name, _)| WantedCopy {
        slot,
        name: name.to_owned(),
        preset: None,
    });
    rig.host.keep(1, wanted.to_vec(), vec![1], Vec::new());
    for (name, instrument) in requests {
        rig.host.prepare(name, None, instrument, SAMPLE_RATE, slot);
    }
    rig.host.wait_idle();
    let prepared = |name, instrument| rig.host.prepared(name, None, instrument, SAMPLE_RATE, slot);
    let keys = requests.map(|(name, instrument)| {
        let Prepared::Ready(key) = prepared(name, instrument) else {
            panic!("the plugin was not prepared");
        };
        key
    });
    assert_ne!(keys[0], keys[1]);

    // Upcoming notes sometimes ask for different plugins at one place in the
    // chain. Checking all of them must leave each parked copy available.
    for _ in 0..16 {
        for ((name, instrument), key) in requests.into_iter().zip(keys) {
            assert_eq!(prepared(name, instrument), Prepared::Ready(key));
            rig.host.prepare(name, None, instrument, SAMPLE_RATE, slot);
        }
    }
    rig.host.wait_idle();
    let provider = rig.host.provider(false);
    let held = keys.map(|key| provider(key, SAMPLE_RATE, slot).expect("parked copy"));
    for ((name, instrument), key) in requests.into_iter().zip(keys) {
        assert_eq!(prepared(name, instrument), Prepared::Ready(key));
        rig.host.prepare(name, None, instrument, SAMPLE_RATE, slot);
    }
    let running = || {
        rig.host.wait_idle();
        let plugins = rig.host.plugins().into_iter();
        plugins.map(|plugin| plugin.running).sum::<usize>()
    };
    assert_eq!(running(), 2);
    drop(held);

    // The chain drops the effect: its parked copy ends, and a prepare of
    // the effect builds nothing.
    for (name, instrument) in requests {
        rig.host.prepare(name, None, instrument, SAMPLE_RATE, slot);
    }
    assert_eq!(running(), 2);
    rig.host.keep(1, wanted[1..].to_vec(), vec![1], Vec::new());
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, slot);
    assert_eq!(running(), 1);
    assert_eq!(prepared(NAME, false), Prepared::Unbuilt(keys[0]));
    assert_eq!(prepared(TONE_NAME, true), Prepared::Ready(keys[1]));
}

#[test]
fn a_bundle_that_does_not_load_gives_the_reason() {
    let rig = Rig::new("broken");
    let bundle = rig.root.join("plugins/Broken.vst3");
    let good = rig.host.plugins()[0].bundle.clone();
    // The same layout as the fixture bundle, with a file that is no library.
    for entry in walk(&good) {
        let target = bundle.join(entry.strip_prefix(&good).unwrap());
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let name = target
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(NAME, "Broken");
        std::fs::write(target.with_file_name(name), "not a library").unwrap();
    }
    rig.scan();
    // A chain wants a copy of the bundle before its load: the name has no
    // answer yet, so the slot keeps each key. After the failed load the
    // name has no copy, and the copy of the slot ends.
    let key = InsertKey {
        plugin: rig.plugin(),
        preset: 0,
    };
    let slot = InsertSlot::effect(1, 0);
    let wanted = WantedCopy {
        slot,
        name: "broken".into(),
        preset: None,
    };
    rig.host.keep(1, vec![wanted.clone()], vec![1], Vec::new());
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, slot);
    rig.host.wait_idle();
    let copy = || rig.host.prepared(NAME, None, false, SAMPLE_RATE, slot);
    assert_eq!(copy(), Prepared::Ready(key));
    let Resolved::Failed(error) = rig.host.resolve("broken", true) else {
        panic!("a text file loaded as a plugin");
    };
    rig.host.keep(1, vec![wanted], vec![1], Vec::new());
    rig.host.wait_idle();
    assert_eq!(copy(), Prepared::Unbuilt(key));
    // The call of a score gives the reason, so the host adds no line.
    let reason = format!("broken did not load: {error}");
    assert_eq!(
        rig.host.key("broken", None, false).err(),
        Some(reason.clone())
    );
    assert!(rig.host.take_errors().is_empty());
    // A caller that waits for the plugin of a slot gets the reason too.
    assert_eq!(
        rig.host.prepared("broken", None, false, SAMPLE_RATE, slot),
        Prepared::Unavailable(reason)
    );
    let status = |rig: &Rig| rig.host.plugins()[0].status.clone();
    assert_eq!(status(&rig), Status::Failed(error));
    // A new scan gives the bundle a new try.
    rig.scan();
    assert_eq!(status(&rig), Status::Found);
}

fn walk(folder: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

/// A copy made ready for a chain that left the score ends, and the chain
/// gets no new copy. A live chain keeps the copies its slots ask for: a
/// copy of a key the chain dropped ends, and no build starts for a key the
/// chain does not want. A key the chain wants again before the end of its
/// copy keeps the copy. A chain no caller lists keeps its copies, and a
/// second caller changes no chain of the first.
#[test]
fn a_copy_no_chain_of_the_score_takes_ends() {
    let rig = Rig::new("ended");
    // A build with the preset of a different plugin fails.
    let mut other = rustel_vst3_fixture::preset(0.5, 0.0);
    other[8] = b'0';
    rig.write_preset("Other Plugin.vstpreset", &other);
    for half in ["Half.vstpreset", "Late.vstpreset"] {
        rig.write_preset(half, &rustel_vst3_fixture::preset(0.5, 0.0));
    }
    let fails = Some("Other Plugin");
    let key = InsertKey {
        plugin: rig.plugin(),
        preset: 0,
    };
    let running = || {
        rig.host.wait_idle();
        rig.host.plugins()[0].running
    };

    // A copy that waits for the slot of a chain.
    let slot = InsertSlot::effect(9, 0);
    let prepared = || rig.host.prepared(NAME, None, false, SAMPLE_RATE, slot);
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, slot);
    until(|| (prepared() == Prepared::Ready(key)).then_some(()));
    assert_eq!(running(), 1);
    rig.host.keep(1, Vec::new(), Vec::new(), vec![8]);
    assert_eq!(running(), 1);
    rig.host.keep(1, Vec::new(), Vec::new(), vec![9]);
    assert_eq!(running(), 0);
    assert_eq!(prepared(), Prepared::Unbuilt(key));

    // A chain that ends with its build in the line keeps no copy, also
    // when the build is done first. No later call builds a copy for the
    // chain: a build with this preset fails, and `Host::take_errors` gives
    // nothing. No chain number comes a second time.
    let (stays, ends) = (InsertSlot::effect(10, 0), InsertSlot::effect(11, 0));
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, stays);
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, ends);
    rig.host.keep(1, Vec::new(), Vec::new(), vec![11]);
    rig.host.prepare(NAME, fails, false, SAMPLE_RATE, ends);
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, ends);
    assert_eq!(running(), 1);
    assert!(rig.host.take_errors().is_empty());

    // A live chain wants a copy with a preset no call asked for yet. Its
    // slot keeps each key until the preset has an answer. A name resolves
    // as the name of a call does: a part of a name finds the plugin.
    let live = InsertSlot::effect(12, 0);
    let wanted = |slot, preset: Option<&str>| WantedCopy {
        slot,
        name: "fixture".into(),
        preset: preset.map(str::to_owned),
    };
    rig.host
        .keep(1, vec![wanted(live, Some("Half"))], vec![12], Vec::new());
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, live);
    assert_eq!(running(), 2);
    rig.host
        .prepare(NAME, Some("Half"), false, SAMPLE_RATE, live);
    assert_eq!(running(), 3);
    // The chain wants 2 keys of the slot, as it does a short time after it
    // dropped a key, and a copy that fails at its build. A slot of the
    // chain with no copy takes no build, also for a key another slot asks
    // for. Then the chain wants one key: the parked copy of the other key
    // ends, the failure goes, and no build starts for either key. Nor does
    // a build start for a slot whose copy has a preset with no file.
    let (failing, missing, empty) = (
        InsertSlot::effect(12, 1),
        InsertSlot::effect(12, 2),
        InsertSlot::effect(12, 3),
    );
    let both = vec![
        wanted(live, None),
        wanted(live, Some("Half")),
        wanted(failing, fails),
    ];
    rig.host.keep(1, both, vec![12], Vec::new());
    rig.host.prepare(NAME, fails, false, SAMPLE_RATE, failing);
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, empty);
    assert_eq!(running(), 3);
    assert_eq!(rig.host.take_errors().len(), 1, "the failed build");
    let Ok(Some((wrong, _))) = rig.host.key(NAME, fails, false) else {
        panic!("the key of the preset of a different plugin");
    };
    let failed = || rig.host.prepared(NAME, fails, false, SAMPLE_RATE, failing);
    assert!(matches!(failed(), Prepared::Unavailable(_)));
    assert!(rig.host.key(NAME, Some("Gone"), false).is_err());
    let one = vec![wanted(live, Some("Half")), wanted(missing, Some("Gone"))];
    rig.host.keep(1, one, vec![12], Vec::new());
    assert_eq!(running(), 2);
    assert_eq!(failed(), Prepared::Unbuilt(wrong));
    rig.host.prepare(NAME, fails, false, SAMPLE_RATE, failing);
    for slot in [live, missing, empty] {
        rig.host.prepare(NAME, None, false, SAMPLE_RATE, slot);
    }
    assert_eq!(running(), 2);
    assert!(rig.host.take_errors().is_empty());
    let ready = |slot| rig.host.prepared(NAME, None, false, SAMPLE_RATE, slot);
    assert_eq!(ready(live), Prepared::Unbuilt(key));
    assert_eq!(
        ready(stays),
        Prepared::Ready(key),
        "a chain no caller lists"
    );

    // A second caller changes no chain of the first.
    let other = InsertSlot::effect(13, 0);
    rig.host
        .keep(2, vec![wanted(other, None)], vec![13], Vec::new());
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, live);
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, other);
    assert_eq!(running(), 3);
    assert_eq!(ready(live), Prepared::Unbuilt(key));

    // While a slow build runs, a key a live chain does not want gets no
    // build in the line. A build in the line ends when its key leaves the
    // chain before the build ends: here the preset of the chain gets its
    // answer. A name the host does not know while it reads the folders
    // keeps each key of its slot until the read ends.
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variable in the builds below. No other thread reads
    // a variable now.
    unsafe { std::env::set_var(rustel_vst3_fixture::SLOW_CLASS_ENV, "600") };
    let (slow, late) = (InsertSlot::effect(14, 0), InsertSlot::effect(15, 0));
    let unknown = InsertSlot::effect(16, 0);
    let mut copies = vec![wanted(live, Some("Half")), wanted(late, Some("Late"))];
    copies.push(WantedCopy {
        name: "no such plugin".into(),
        ..wanted(unknown, None)
    });
    rig.host.keep(1, copies, vec![12, 15, 16], Vec::new());
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, slow);
    // The end of a key the chain drops waits behind the slow build. A key
    // the chain wants again before that end keeps its copy.
    rig.host
        .keep(2, vec![wanted(other, Some("Half"))], vec![13], Vec::new());
    rig.host
        .keep(2, vec![wanted(other, None)], vec![13], Vec::new());
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, empty);
    assert_eq!(ready(empty), Prepared::Unbuilt(key));
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, late);
    let answer = rig.host.key(NAME, Some("Late"), false);
    assert!(answer.is_ok_and(|key| key.is_some()));
    rig.host.scan(&[rig.root.join("plugins")]);
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, unknown);
    assert_eq!(ready(unknown), Prepared::Pending);
    assert_eq!(running(), 4);
    assert_eq!(ready(other), Prepared::Ready(key));
    // SAFETY: as above. The builds read the variable.
    unsafe { std::env::remove_var(rustel_vst3_fixture::SLOW_CLASS_ENV) };
    assert_eq!(ready(late), Prepared::Unbuilt(key));
    assert_eq!(ready(unknown), Prepared::Unbuilt(key));

    // A caller with no copy and no chain lists no chain: the slots of its
    // chains take each key again.
    rig.host.keep(1, Vec::new(), Vec::new(), Vec::new());
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, live);
    assert_eq!(running(), 5);
}
