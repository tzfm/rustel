//! The host with a real plugin library: the fixture plugin in a VST3 bundle.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rustel_audio::tripwire::{self, TripwireAlloc, Violations};
use rustel_audio::{InsertKey, InsertNote, InsertParam, OrbitInsert};
use rustel_vst3::{FoundPreset, Host, Prepared, Resolved, Status, WorkerProgram};
use rustel_vst3_fixture::{NAME, PARAM_BYPASS, PARAM_GAIN, PARAM_GATE, TONE_LEVEL, TONE_NAME};

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
        (self.host.provider(true))(key, SAMPLE_RATE, 1)
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
    assert_eq!(keys, ["gain", "beatgate", "bypass"]);
    assert_eq!(plugin.param("Beat Gate"), Some(PARAM_GATE));
    assert_eq!(plugin.param("100"), Some(PARAM_GAIN));
    assert_eq!(plugin.param("mix"), None);
    assert!(!plugin.is_instrument());
    let loaded = &rig.host.plugins()[0];
    assert_eq!(
        (loaded.status.clone(), loaded.vendor.as_str()),
        (Status::Ready, "rustel")
    );
    assert_eq!(loaded.params[0].default_text, "1.00");
    // The list shows the number a score writes, and the text of the
    // plugin when the text is a different thing.
    assert_eq!(loaded.params[0].default_shown(), "1");
    assert_eq!(loaded.params[2].default_shown(), "0 = off");
    assert!(matches!(
        rig.host.resolve("no such plugin", true),
        Resolved::Missing
    ));

    let mut insert = rig.insert(0).expect("insert");
    assert_eq!(
        insert.key(),
        InsertKey {
            plugin: id,
            preset: 0
        }
    );
    assert_eq!(block(insert.as_mut(), 1.0, 128), vec![1.0; 128]);

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
}

#[test]
fn a_preset_file_sets_the_state_of_the_plugin() {
    let rig = Rig::new("preset");
    rig.write_preset(
        "Half Level.vstpreset",
        &rustel_vst3_fixture::preset(0.5, 0.0),
    );
    let mut other = rustel_vst3_fixture::preset(0.5, 0.0);
    other[8] = b'0';
    rig.write_preset("Other Plugin.vstpreset", &other);
    let plugin = rig.plugin();
    let names = rig.host.presets(NAME).expect("the load read the presets");
    assert_eq!(names, ["Half Level", "Other Plugin"]);
    assert_eq!(rig.host.preset(plugin, "missing"), FoundPreset::Missing);

    let number = |name: &str| match rig.host.preset(plugin, name) {
        FoundPreset::Number(number) => number,
        other => panic!("preset {name}: {other:?}"),
    };
    let preset = number("half level");
    assert_eq!(number("HalfLevel"), preset);
    let mut insert = rig.insert(preset).expect("insert");
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
        [format!("vst {NAME}: the preset is for a different plugin")]
    );
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
    let mut insert = (rig.host.provider(true))(key, SAMPLE_RATE, 1).expect("insert");
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
}

/// Not a test: the entry of a plugin worker. The worker test gives this
/// binary to the host as its worker program, so the bundle runs in a
/// process of its own, as in the product. The host adds the bundle path as
/// the last argument. With the argument `fault`, the effect stops the
/// worker in its first audio block.
#[test]
fn plugin_worker() {
    let args: Vec<String> = std::env::args().collect();
    // An ordinary run of the tests has no bundle path.
    let Some(bundle) = args.last().filter(|arg| arg.ends_with(".vst3")) else {
        return;
    };
    if args.iter().any(|arg| arg == "fault") {
        let (name, value) = (
            rustel_vst3_fixture::ABORT_ENV,
            rustel_vst3_fixture::ABORT_IN_AUDIO,
        );
        // SAFETY: this process is the worker, and no other thread of it
        // reads a variable now.
        unsafe { std::env::set_var(name, value) };
    }
    std::process::exit(rustel_vst3::serve(std::path::Path::new(bundle)));
}

fn worker_program(fault: bool) -> WorkerProgram {
    let mut args = vec!["plugin_worker".into(), "--exact".into()];
    args.extend(fault.then(|| "fault".into()));
    WorkerProgram {
        program: std::env::current_exe().expect("test program path"),
        args,
    }
}

#[test]
fn repeated_resets_cancel_old_events_and_keep_the_first_resumed_events() {
    for worker in [false, true] {
        let rig = Rig::new("reset");
        if worker {
            rig.host.set_worker(Some(worker_program(false)));
        }
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
        // Stopped callbacks do not process audio. More than one worker
        // packet of resets must leave room for the first resumed value.
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
        let mut instrument = (rig.host.provider(true))(key, SAMPLE_RATE, 1).expect("instrument");
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
}

#[test]
fn parameter_restores_follow_note_time_and_survive_a_full_queue() {
    for worker in [false, true] {
        let rig = Rig::new("restore");
        if worker {
            rig.host.set_worker(Some(worker_program(false)));
        }
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

        // The later restore can arrive before the value it must clear.
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

        // A restore rejected by the bounded queue must not forget the
        // delivered values that a later restore still needs to clear.
        for frame in 128..192 {
            insert.set_param(
                InsertParam {
                    id: PARAM_GATE,
                    value: 0.0,
                },
                frame,
            );
        }
        insert.restore_params(0);
        for _ in 0..2 {
            assert_eq!(block(insert.as_mut(), 1.0, 128), [0.5; 128]);
        }
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
    }
}

/// With a worker program, a bundle runs in a process of its own. The
/// plugin gives the same sound as in this process, and the audio callback
/// still allocates nothing. A plugin fault in an audio block ends the
/// worker and not the host: the effect passes its input, the host says
/// what happened, and the next use starts a new worker with a new plugin
/// number.
#[test]
fn a_plugin_fault_in_a_worker_stays_out_of_the_host() {
    let rig = Rig::new("worker");
    rig.host.set_worker(Some(worker_program(false)));
    let mut insert = rig.insert(0).expect("insert");
    let (mut left, mut right) = ([1.0f32; 128], [1.0f32; 128]);
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
        "the call to the worker allocated or freed: {delta:?}"
    );
    assert_eq!(left[0], 0.25 * 0.25 * 0.25);
    assert_eq!(left[37..], [0.25; 91]);

    // The next packet has more events, then two have none. A value waiting
    // across those blocks must arrive once at its original frame.
    insert.cut_notes(0);
    insert.reset();
    insert.set_param(
        InsertParam {
            id: PARAM_GAIN,
            value: 0.5,
        },
        64,
    );
    assert_eq!(block(insert.as_mut(), 1.0, 37), vec![0.25; 37]);
    assert_eq!(block(insert.as_mut(), 1.0, 1), vec![0.25]);
    let output = block(insert.as_mut(), 1.0, 128);
    assert_eq!(output[..26], [0.25; 26]);
    assert_eq!(output[26..], [0.5; 102]);
    assert_eq!(rig.host.plugins()[0].running, 1);
    drop(insert);

    let rig = Rig::new("worker-fault");
    rig.host.set_worker(Some(worker_program(true)));
    let first = rig.plugin();
    let mut insert = rig.insert(0).expect("insert");
    for _ in 0..4 {
        assert_eq!(block(insert.as_mut(), 0.5, 128), vec![0.5; 128]);
    }
    let said = until(|| {
        let mut errors = rig.host.take_errors().into_iter();
        errors.find(|error| error.contains("the plugin process ended"))
    });
    assert!(said.contains(NAME), "{said}");
    assert_ne!(rig.plugin(), first);
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
    let running = (rig.host.provider(true))(key, SAMPLE_RATE, 1).expect("insert");
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, 2);
    rig.host.wait_idle();
    unload(&[]);
    assert_eq!(loaded(&rig), 2);
    assert_eq!(rig.host.plugins()[0].running, 1);
    drop(running);
    unload(&[]);
    assert_eq!(loaded(&rig), 0);
    assert_eq!(rig.host.plugins().len(), 2, "the two rows stay in the list");

    assert_eq!((number(TONE_NAME), number(NAME)), (tone, effect));
    assert_eq!(loaded(&rig), 2);
}

#[test]
fn live_play_does_not_wait_for_a_plugin() {
    let rig = Rig::new("live");
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
    assert!(provider(key, SAMPLE_RATE, 3).is_none());
    let mut insert = until(|| provider(key, SAMPLE_RATE, 3));
    assert_eq!(block(insert.as_mut(), 0.5, 16), vec![0.5; 16]);
    // The orbit took the plugin, so the next request builds a new one.
    assert!(provider(key, SAMPLE_RATE, 3).is_none());

    // A start that waits for its plugins reads how far each one is. A
    // prepared plugin is ready for its slot before a note asks.
    let prepared = |name: &str, instrument| rig.host.prepared(name, None, instrument, 44_100, 5);
    assert_eq!(prepared(NAME, false), Prepared::Unbuilt(key));
    assert_eq!(prepared(NAME, true), Prepared::Unavailable);
    assert_eq!(prepared("no such plugin", false), Prepared::Unavailable);
    rig.host.prepare(NAME, None, false, 44_100, 5);
    until(|| (prepared(NAME, false) == Prepared::Ready(key)).then_some(()));
    let held = provider(key, 44_100, 5).expect("the prepared plugin");

    // While the engine holds the plugin of the slot, a start has nothing
    // to wait for, and one more prepare builds no spare copy.
    assert_eq!(prepared(NAME, false), Prepared::Ready(key));
    rig.host.prepare(NAME, None, false, 44_100, 5);
    rig.host.wait_idle();
    drop(held);
    assert_eq!(prepared(NAME, false), Prepared::Unbuilt(key));
}

#[test]
fn different_plugins_prepared_for_one_slot_stay_ready() {
    let rig = Rig::new("shared-slot");
    rig.plugin();
    let requests = [(NAME, false), (TONE_NAME, true)];
    for (name, instrument) in requests {
        rig.host.prepare(name, None, instrument, SAMPLE_RATE, 1);
    }
    rig.host.wait_idle();
    let prepared = |name, instrument| rig.host.prepared(name, None, instrument, SAMPLE_RATE, 1);
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
            rig.host.prepare(name, None, instrument, SAMPLE_RATE, 1);
        }
    }
    rig.host.wait_idle();
    let provider = rig.host.provider(false);
    let held = keys.map(|key| provider(key, SAMPLE_RATE, 1).expect("parked copy"));
    for ((name, instrument), key) in requests.into_iter().zip(keys) {
        assert_eq!(prepared(name, instrument), Prepared::Ready(key));
        rig.host.prepare(name, None, instrument, SAMPLE_RATE, 1);
    }
    rig.host.wait_idle();
    assert_eq!(
        rig.host
            .plugins()
            .iter()
            .map(|plugin| plugin.running)
            .sum::<usize>(),
        2
    );
    drop(held);
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
    let Resolved::Failed(error) = rig.host.resolve("broken", true) else {
        panic!("a text file loaded as a plugin");
    };
    assert_eq!(rig.host.take_errors(), [format!("vst Broken: {error}")]);
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
