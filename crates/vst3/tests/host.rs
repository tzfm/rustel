//! The host with a real plugin library: the fixture plugin in a VST3 bundle.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rustel_audio::tripwire::{self, TripwireAlloc, Violations};
use rustel_audio::{
    AudioBackend, InsertControls, InsertKey, InsertNote, InsertParam, InsertSlot, OnsetEvent,
    OrbitInsert, ScalarBackend,
};
use rustel_vst3::{
    FoundPreset, Host, NotQueued, Prepared, RenderFaults, Resolved, Status, WantedCopy,
    WorkerProgram,
};
use rustel_vst3_fixture::{
    BROKEN_TONE_ENV, COST_ENV, EARLY_FAULT_ENV, ECHO_ENV, LATE_LATENCY_ENV, MODE_MARK_ENV, NAME,
    PARAM_BYPASS, PARAM_DELAY, PARAM_FAULT, PARAM_GAIN, PARAM_GATE, PROCESS_MARK_ENV,
    REALTIME_ONLY_ENV, SLOW_ACTIVATE_ENV, SLOW_CLASS_ENV, THREAD_MARK_ENV, TONE_LEVEL, TONE_NAME,
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

/// 0.3 s of silence through the insert: past the quiet time of a plugin
/// with no tail, 0.25 s.
fn silence(insert: &mut dyn OrbitInsert) {
    for _ in 0..SAMPLE_RATE / 10 * 3 / 128 + 1 {
        block(insert, 0.0, 128);
    }
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
    assert_eq!(keys, ["gain", "beatgate", "bypass", "delay", "fault"]);
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
    // allocation, for each block size, also through a copy for live play.
    // The drain of a stopped output allocates nothing either.
    let live = rig.host.provider(false);
    let lent_slot = InsertSlot::effect(2, 0);
    let mut lent = until(|| live(key, SAMPLE_RATE, lent_slot));
    let mut left = [1.0f32; 128];
    let mut right = [1.0f32; 128];
    let (mut lent_left, mut lent_right) = ([1.0f32; 128], [1.0f32; 128]);
    let (mut silent_left, mut silent_right) = ([0.0f32; 128], [0.0f32; 128]);
    let quarter = InsertParam {
        id: PARAM_GAIN,
        value: 0.25,
    };
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        insert.set_param(quarter, 0);
        for frames in [128, 1, 37] {
            insert.process(&mut left[..frames], &mut right[..frames]);
        }
        lent.set_param(quarter, 0);
        lent.process(&mut lent_left, &mut lent_right);
        lent.drain(&mut silent_left, &mut silent_right);
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
    assert_eq!((lent_left, lent_right), ([0.25; 128], [0.25; 128]));
    // The live copy goes home at its drop, and the next take of its slot
    // builds nothing. Its next note puts back the start value of each
    // parameter an earlier note set.
    let running = || {
        rig.host.wait_idle();
        rig.host.plugins()[0].running
    };
    let copies = running();
    drop(lent);
    let home = |slot, rate| {
        let ready = rig.host.prepared(NAME, None, false, rate, slot) == Prepared::Ready(key);
        ready.then_some(())
    };
    until(|| home(lent_slot, SAMPLE_RATE));
    let mut lent = live(key, SAMPLE_RATE, lent_slot).expect("the copy that came home");
    assert_eq!(running(), copies);
    lent.restore_params(0);
    assert_eq!(block(lent.as_mut(), 1.0, 128), vec![1.0; 128]);

    // The park turns a copy off and on only when the copy has a tail to
    // play out. Each setActive call of this copy sleeps 300 ms, so a
    // restart holds the park 600 ms or more, and the long flush 1.2 s or
    // more. A copy that ran no frame since its build skips the flush. A copy
    // loud at its drop takes one restart and the quiet check. A copy the
    // drain made quiet for its quiet time, 0.25 s here, skips the flush.
    let slow_slot = InsertSlot::effect(4, 0);
    // SAFETY: as above. The plugin thread reads the variable in the build
    // below.
    unsafe { std::env::set_var(SLOW_ACTIVATE_ENV, "300") };
    let slow = until(|| live(key, SAMPLE_RATE, slow_slot));
    // SAFETY: as above. The copy read the variable at its build.
    unsafe { std::env::remove_var(SLOW_ACTIVATE_ENV) };
    let park = |slow: Box<dyn OrbitInsert>| {
        let start = Instant::now();
        drop(slow);
        until(|| home(slow_slot, SAMPLE_RATE));
        start.elapsed()
    };
    let restart = Duration::from_millis(600);
    let parked = park(slow);
    assert!(
        parked < restart,
        "the copy after its build parked in {parked:?}"
    );
    let mut slow = live(key, SAMPLE_RATE, slow_slot).expect("the copy that came home");
    assert_eq!(block(slow.as_mut(), 1.0, 128), vec![1.0; 128]);
    let parked = park(slow);
    assert!(
        (restart..2 * restart).contains(&parked),
        "the loud copy parked in {parked:?}"
    );
    let mut slow = live(key, SAMPLE_RATE, slow_slot).expect("the copy that came home");
    assert_eq!(block(slow.as_mut(), 1.0, 128), vec![1.0; 128]);
    // 94 blocks of 128 frames: 12 032 frames, more than 0.25 s at 48 kHz.
    for _ in 0..94 {
        let (mut left, mut right) = ([0.0f32; 128], [0.0f32; 128]);
        slow.drain(&mut left, &mut right);
    }
    let parked = park(slow);
    assert!(parked < restart, "the drained copy parked in {parked:?}");

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

    // A copy with an echo of 500 ms has a tail of 24 000 frames. Its input
    // comes back one time, 24 000 frames later. At a new rate the copy
    // keeps its echo of 500 ms: 22 050 frames at 44.1 kHz.
    let echo_slot = InsertSlot::effect(3, 0);
    // SAFETY: as above. The plugin thread reads the variable in the build
    // below.
    unsafe { std::env::set_var(ECHO_ENV, "500") };
    let mut echo = until(|| live(key, SAMPLE_RATE, echo_slot));
    // SAFETY: as above. The copy read the variable at its build.
    unsafe { std::env::remove_var(ECHO_ENV) };
    assert_eq!(echo.tail_frames(), 24_000);
    let mut output = block(echo.as_mut(), 1.0, 128);
    while output.len() < 24_256 {
        output.extend(block(echo.as_mut(), 0.0, 128));
    }
    assert_eq!(output[..128], [1.0; 128]);
    assert!(output[128..24_000].iter().all(|sample| *sample == 0.0));
    assert_eq!(output[24_000..24_128], [1.0; 128]);
    assert!(output[24_128..].iter().all(|sample| *sample == 0.0));
    drop(echo);
    until(|| home(echo_slot, SAMPLE_RATE));
    assert!(live(key, 44_100, echo_slot).is_none(), "the retune runs");
    let echo = until(|| live(key, 44_100, echo_slot));
    assert_eq!(echo.tail_frames(), 22_050);

    // The copy writes its mark at its end, on the plugin thread. That
    // thread ran the first blocks at the build, and this thread ran the
    // others, one call at a time.
    drop(insert);
    rig.host.wait_idle();
    let line = std::fs::read_to_string(&mark).expect("process mark");
    assert_eq!(line, "process threads 2, overlap no\n");

    // A copy of a render takes the values of the first note of its slot
    // before its read: the Delay of 0.25 gives a latency of 1024 frames. A
    // parked copy gives the latency of its slot. A copy with no park ends
    // in its job, and its slot has no latency: a slot of a render takes no
    // latency of another copy.
    let delay = |value| InsertParam {
        id: PARAM_DELAY,
        value,
    };
    let render_slot = InsertSlot::effect(20, 0);
    let parks = rig
        .host
        .prepare_render(key, SAMPLE_RATE, render_slot, &[delay(0.25)], true);
    assert_eq!(parks.expect("the render build").recv(), Ok(Ok(1024)));
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, render_slot), Some(1024));
    let copies = running();
    let ended_slot = InsertSlot::effect(21, 0);
    let ends = rig
        .host
        .prepare_render(key, SAMPLE_RATE, ended_slot, &[delay(0.25)], false);
    assert_eq!(ends.expect("the render build").recv(), Ok(Ok(1024)));
    assert_eq!(rig.host.plugins()[0].running, copies, "the copy ended");
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, ended_slot), None);
    let unbuilt = InsertSlot::effect(24, 0);
    assert_eq!(
        rig.host.latency(key, SAMPLE_RATE, unbuilt),
        Some(0),
        "a live slot takes no latency of a render"
    );

    // A live copy asks for a restart when a value gives it a new latency,
    // with no allocation: the engine then gives it back. Its park restarts
    // it and reads the latency. The copy comes back with its new latency,
    // and with no new build.
    let latent_slot = InsertSlot::effect(22, 0);
    rig.host
        .prepare(NAME, None, false, SAMPLE_RATE, latent_slot);
    let mut latent = until(|| live(key, SAMPLE_RATE, latent_slot));
    assert!(rig.host.restarts().is_empty());
    let (mut left, mut right) = ([0.0f32; 128], [0.0f32; 128]);
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        latent.set_param(delay(0.5), 0);
        latent.process(&mut left, &mut right);
    });
    let delta = Violations::capture().since(before);
    assert!(delta.clean(), "the request allocated or freed: {delta:?}");
    assert_eq!(rig.host.restarts(), [(latent_slot, key)]);
    let copies = running();
    drop(latent);
    until(|| home(latent_slot, SAMPLE_RATE));
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, latent_slot), Some(2048));
    let mut latent = live(key, SAMPLE_RATE, latent_slot).expect("the copy that came home");
    assert_eq!(running(), copies);
    assert!(rig.host.restarts().is_empty());
    // A note that brings the delay in use asks for nothing: its restore and
    // its value at one frame give the plugin one value. The plugin takes the
    // last value of a block. The next service comes 1 s after the read of a
    // new latency.
    latent.restore_params(0);
    latent.set_param(delay(0.5), 0);
    block(latent.as_mut(), 0.0, 128);
    latent.set_param(delay(0.25), 0);
    latent.set_param(delay(0.5), 64);
    block(latent.as_mut(), 0.0, 128);
    std::thread::sleep(Duration::from_millis(1_100));
    assert!(
        rig.host.restarts().is_empty(),
        "a note with the delay in use"
    );
    latent.set_param(delay(0.50001), 0);
    block(latent.as_mut(), 0.0, 128);
    assert_eq!(rig.host.restarts(), [(latent_slot, key)]);
    // A read of the latency the copy had before puts the next service 10 s
    // after it. A new latency sets the count of such reads to 0 and puts the
    // next service 1 s after its read. The second read in a row of the
    // latency the copy had before ends the services while the copy plays,
    // and the host gives one line for the plugin.
    let kept = format!(
        "vst {NAME}: the plugin reports latency changes and keeps its latency, so its copy \
         restarts only when it leaves the output"
    );
    drop(latent);
    until(|| home(latent_slot, SAMPLE_RATE));
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, latent_slot), Some(2048));
    let mut latent = live(key, SAMPLE_RATE, latent_slot).expect("the copy that came home");
    latent.set_param(delay(0.5), 0);
    block(latent.as_mut(), 0.0, 128);
    std::thread::sleep(Duration::from_millis(1_100));
    assert!(
        rig.host.restarts().is_empty(),
        "a copy that kept its latency"
    );
    latent.set_param(delay(0.75), 0);
    block(latent.as_mut(), 0.0, 128);
    drop(latent);
    until(|| home(latent_slot, SAMPLE_RATE));
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, latent_slot), Some(3072));
    let mut latent = live(key, SAMPLE_RATE, latent_slot).expect("the copy that came home");
    latent.set_param(delay(0.75001), 0);
    block(latent.as_mut(), 0.0, 128);
    assert!(rig.host.restarts().is_empty(), "a service before its time");
    std::thread::sleep(Duration::from_millis(1_100));
    assert_eq!(rig.host.restarts(), [(latent_slot, key)]);
    drop(latent);
    until(|| home(latent_slot, SAMPLE_RATE));
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, latent_slot), Some(3072));
    assert!(
        !rig.host.take_errors().contains(&kept),
        "a new latency sets the count to 0"
    );
    let mut latent = live(key, SAMPLE_RATE, latent_slot).expect("the copy that came home");
    latent.set_param(delay(0.75), 0);
    block(latent.as_mut(), 0.0, 128);
    drop(latent);
    until(|| home(latent_slot, SAMPLE_RATE));
    assert!(rig.host.take_errors().contains(&kept));

    // A plugin that finds its latency in its first blocks after it turns on
    // asks for a restart again after the first one. The park restarts it
    // one time more, and reads the latency then.
    let late_slot = InsertSlot::effect(23, 0);
    // SAFETY: as above. The plugin thread reads the variable in the build
    // below.
    unsafe { std::env::set_var(LATE_LATENCY_ENV, "1") };
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, late_slot);
    let mut late = until(|| live(key, SAMPLE_RATE, late_slot));
    // SAFETY: as above. The copy read the variable at its build.
    unsafe { std::env::remove_var(LATE_LATENCY_ENV) };
    late.set_param(delay(0.5), 0);
    block(late.as_mut(), 0.0, 128);
    assert_eq!(rig.host.restarts(), [(late_slot, key)]);
    drop(late);
    until(|| home(late_slot, SAMPLE_RATE));
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, late_slot), Some(2048));
    let late = live(key, SAMPLE_RATE, late_slot).expect("the copy that came home");
    std::thread::sleep(Duration::from_millis(1_100));
    assert!(rig.host.restarts().is_empty(), "the late copy");
    drop(late);
    until(|| home(late_slot, SAMPLE_RATE));
    rig.host.wait_idle();

    // A copy judges its own blocks. A sample of the input that is no number
    // is silence, and no fault of the plugin: the plugin with its bypass on
    // gives it back as 0, with no line. A block the plugin refuses with a
    // good output gives no line either.
    let _ = rig.host.take_errors();
    let fault = |value| InsertParam {
        id: PARAM_FAULT,
        value,
    };
    let mut render = rig.insert(0).expect("insert");
    render.set_param(bypass(1.0), 0);
    assert_eq!(block(render.as_mut(), f32::NAN, 128), vec![0.0; 128]);
    assert!(rig.host.take_errors().is_empty());
    render.set_param(bypass(0.0), 0);
    render.set_param(fault(0.3), 0);
    assert_eq!(block(render.as_mut(), 1.0, 128), vec![1.0; 128]);
    assert!(rig.host.take_errors().is_empty());
    // A drain counts nothing, so 1.5 s of drain blocks with no number in
    // the output give no fault and no line.
    render.set_param(fault(1.0), 0);
    for _ in 0..SAMPLE_RATE * 3 / 2 / 128 + 1 {
        let (mut left, mut right) = ([1.0f32; 128], [1.0f32; 128]);
        render.drain(&mut left, &mut right);
        assert_eq!((left, right), ([0.0; 128], [0.0; 128]));
    }
    assert!(!render.faulted());
    assert!(rig.host.take_errors().is_empty());
    // 2 s of blocks of play with silence in and no number out give one
    // line for each count, and no fault: nothing went in. So a plugin that
    // gives no number only in silence plays its notes.
    render.set_param(fault(1.0), 0);
    for _ in 0..2 * SAMPLE_RATE / 128 {
        assert_eq!(block(render.as_mut(), 0.0, 128), vec![0.0; 128]);
    }
    assert!(!render.faulted());
    let not_a_number =
        format!("vst {NAME}: a sample of the output was not a number and played as silence");
    let refused = format!("vst {NAME}: the plugin refused a block");
    assert_eq!(
        rig.host.take_errors(),
        [not_a_number.clone(), refused.clone()]
    );
    render.set_param(fault(0.0), 0);
    assert_eq!(block(render.as_mut(), 1.0, 128), vec![1.0; 128]);
    // 1 s of blocks of play with sound in and no number out faults the
    // copy at its last block, with no allocation: the copy drops its note
    // and writes silence from then on. Its line comes one time, as the line
    // of a render take.
    let long = InsertNote {
        pitch: 60.0,
        velocity: 1.0,
        frames: 10 * SAMPLE_RATE,
    };
    let (mut left, mut right) = ([1.0f32; 128], [1.0f32; 128]);
    let before = Violations::capture();
    let (early, silent) = tripwire::audio_scope(|| {
        render.note(long, 0);
        render.set_param(fault(1.0), 0);
        let (mut early, mut silent) = (false, true);
        for _ in 0..SAMPLE_RATE / 128 {
            early |= render.faulted();
            left.fill(1.0);
            right.fill(1.0);
            render.process(&mut left, &mut right);
            silent &= left.iter().chain(&right).all(|sample| *sample == 0.0);
        }
        (early, silent)
    });
    let delta = Violations::capture().since(before);
    assert!(delta.clean(), "the fault allocated or freed: {delta:?}");
    assert!(!early, "the copy faulted before 1 s");
    assert!(silent, "a sample of the copy was not 0");
    assert!(render.faulted() && !render.busy());
    let render_fault = format!(
        "vst {NAME}: no number in the output for 1 s, so the stage plays silence to the end"
    );
    assert_eq!(
        rig.host.take_errors(),
        [not_a_number.clone(), refused.clone(), render_fault.clone()]
    );
    assert!(rig.host.take_errors().is_empty());
    // A copy that faulted takes no value, no note and no cut, and makes no
    // plugin call: no block counts.
    render.set_param(fault(0.0), 0);
    render.set_param_now(fault(0.0));
    render.restore_params(0);
    render.cut_notes(0);
    render.note(long, 0);
    assert_eq!(block(render.as_mut(), 1.0, 128), vec![0.0; 128]);
    assert!(!render.busy());
    assert!(rig.host.take_errors().is_empty());
    drop(render);

    // A block is fed when the copy holds a note, also with silence in, as
    // for an instrument with no input. A block that is not fed leaves the
    // count of the fault. So 1 s of fed blocks with no number out faults
    // the copy, also with silence between them.
    let mut fed = rig.insert(0).expect("insert");
    fed.set_param(fault(1.0), 0);
    for _ in 0..200 {
        assert_eq!(block(fed.as_mut(), 1.0, 128), vec![0.0; 128]);
    }
    for _ in 0..100 {
        assert_eq!(block(fed.as_mut(), 0.0, 128), vec![0.0; 128]);
    }
    fed.note(long, 0);
    for _ in 200..SAMPLE_RATE / 128 {
        assert!(!fed.faulted(), "the copy faulted before 1 s of fed blocks");
        assert_eq!(block(fed.as_mut(), 0.0, 128), vec![0.0; 128]);
    }
    assert!(fed.faulted());
    drop(fed);
    rig.host.wait_idle();
    assert_eq!(
        rig.host.take_errors(),
        [not_a_number.clone(), refused.clone(), render_fault]
    );
    // A fed block with no number in less than half of its output gives the
    // line and counts for no fault. An echo of 8 ms, 3 blocks, adds the
    // input of 3 blocks before to the output, so a loud first quarter of
    // each block sums to no number in that quarter.
    // SAFETY: as above. The plugin thread reads the variable in the build
    // below.
    unsafe { std::env::set_var(ECHO_ENV, "8") };
    let mut partial = rig.insert(0).expect("insert");
    // SAFETY: as above. The copy read the variable at its build.
    unsafe { std::env::remove_var(ECHO_ENV) };
    for _ in 0..2 * SAMPLE_RATE / 128 {
        let (mut left, mut right) = ([0.0f32; 128], [0.0f32; 128]);
        left[..32].fill(f32::MAX);
        right[..32].fill(f32::MAX);
        partial.process(&mut left, &mut right);
    }
    assert!(!partial.faulted());
    drop(partial);
    assert_eq!(rig.host.take_errors(), std::slice::from_ref(&not_a_number));

    // The silent blocks of a build count no fault: a copy that gives no
    // number and refuses its first blocks plays with no line.
    // SAFETY: as above. The plugin thread reads the variable in the build
    // below.
    unsafe { std::env::set_var(EARLY_FAULT_ENV, "1") };
    let mut early = rig.insert(0).expect("insert");
    // SAFETY: as above. The copy read the variable at its build.
    unsafe { std::env::remove_var(EARLY_FAULT_ENV) };
    assert_eq!(block(early.as_mut(), 1.0, 128), vec![1.0; 128]);
    drop(early);
    rig.host.wait_idle();
    assert!(rig.host.take_errors().is_empty());

    // A park keeps the counts of a copy for the next take: the value a full
    // queue lost right before the park gives its line one time.
    let full_slot = InsertSlot::effect(29, 0);
    let mut full = until(|| live(key, SAMPLE_RATE, full_slot));
    assert_eq!(block(full.as_mut(), 1.0, 128), vec![1.0; 128]);
    for frame in 0..513 {
        full.set_param(gain(1.0), frame);
    }
    drop(full);
    until(|| home(full_slot, SAMPLE_RATE));
    let lost_value =
        format!("vst {NAME}: too many parameter values at one time, and a value was lost");
    assert_eq!(rig.host.take_errors(), std::slice::from_ref(&lost_value));
    assert!(rig.host.take_errors().is_empty());

    // A block with no number in its output reads as loud, also when it is
    // not fed. So a live copy that gives no number in silence does not park
    // at its drop: the plugin thread plays out its tail. That copy turns its
    // plugin off and on slowly, so its job still runs at the look. Its
    // counts stay through the park: it gives the line of no number and the
    // line of the refused blocks, and no fault.
    let unfed_slot = InsertSlot::effect(30, 0);
    // SAFETY: as above. The plugin thread reads the variable in the build
    // below.
    unsafe { std::env::set_var(SLOW_ACTIVATE_ENV, "200") };
    let mut unfed = until(|| live(key, SAMPLE_RATE, unfed_slot));
    // SAFETY: as above. The copy read the variable at its build.
    unsafe { std::env::remove_var(SLOW_ACTIVATE_ENV) };
    unfed.set_param(fault(1.0), 0);
    silence(unfed.as_mut());
    assert!(!unfed.faulted(), "the copy was not fed");
    drop(unfed);
    let unfed_home = || {
        rig.host
            .prepared(NAME, None, false, SAMPLE_RATE, unfed_slot)
    };
    assert_eq!(unfed_home(), Prepared::Returning);
    rig.host.wait_idle();
    assert_eq!(unfed_home(), Prepared::Ready(key));
    assert_eq!(
        rig.host.take_errors(),
        [not_a_number.clone(), refused.clone()]
    );
    assert!(rig.host.take_errors().is_empty());

    // A live copy that faulted asks for no restart. It reads as quiet, and
    // ends at its drop, in a job of the line.
    let faulty_slot = InsertSlot::effect(25, 0);
    rig.host
        .prepare(NAME, None, false, SAMPLE_RATE, faulty_slot);
    let mut faulty = until(|| live(key, SAMPLE_RATE, faulty_slot));
    faulty.set_param(delay(0.5), 0);
    assert_eq!(block(faulty.as_mut(), 1.0, 128), vec![1.0; 128]);
    assert_eq!(rig.host.restarts(), [(faulty_slot, key)]);
    faulty.set_param(fault(1.0), 0);
    for _ in 0..SAMPLE_RATE / 128 {
        assert_eq!(block(faulty.as_mut(), 1.0, 128), vec![0.0; 128]);
    }
    assert!(faulty.faulted());
    assert!(rig.host.restarts().is_empty());
    silence(faulty.as_mut());
    let copies = running();
    drop(faulty);
    assert_eq!(running(), copies - 1);
    let live_fault = format!(
        "vst {NAME}: no number in the output for 1 s, so the stage plays silence until a new \
         copy takes over"
    );
    let live_lines = [not_a_number, refused, live_fault];
    assert_eq!(rig.host.take_errors(), live_lines);
    assert!(rig.host.take_errors().is_empty());
    rig.host.wait_idle();

    // A live copy that faulted gets a new copy at its first try, with no
    // wait. The new copy builds while the copy that faulted plays silence,
    // and the slot takes it at its next take. The copy that faulted then
    // ends at its drop, and the new copy keeps the slot: a take builds
    // nothing more. A new copy gets no restart service while the copy that
    // faulted is in an engine. A new copy that faults too waits for the time
    // of its build before its next try.
    let _ = rig.host.take_errors();
    let replaced_slot = InsertSlot::effect(26, 0);
    let replaced = WantedCopy {
        slot: replaced_slot,
        name: NAME.to_owned(),
        preset: None,
    };
    let retry = || rig.host.retry(std::slice::from_ref(&replaced), SAMPLE_RATE);
    let faults = |copy: &mut dyn OrbitInsert| {
        copy.set_param(fault(1.0), 0);
        for _ in 0..SAMPLE_RATE / 128 {
            assert_eq!(block(copy, 1.0, 128), vec![0.0; 128]);
        }
        assert!(copy.faulted());
    };
    rig.host
        .prepare(NAME, None, false, SAMPLE_RATE, replaced_slot);
    let mut faulty = until(|| live(key, SAMPLE_RATE, replaced_slot));
    assert_eq!(retry(), 0, "a copy with no fault");
    // The row of the plugin says the fault while it lasts, and what the new
    // copy does.
    let row_fault = || rig.host.plugins()[0].fault.clone();
    let silent = "a copy plays silence: no number in its output for 1 s";
    assert_eq!(row_fault(), None);
    faults(faulty.as_mut());
    assert_eq!(rig.host.take_errors(), live_lines);
    assert_eq!(row_fault().as_deref(), Some(silent));
    // SAFETY: as above. The plugin thread reads the variable in the builds
    // below.
    unsafe { std::env::set_var(SLOW_CLASS_ENV, "300") };
    let start = Instant::now();
    assert_eq!(retry(), 1, "a first fault waits for nothing");
    assert_eq!(
        rig.host
            .prepared(NAME, None, false, SAMPLE_RATE, replaced_slot),
        Prepared::Pending
    );
    assert_eq!(row_fault(), Some(format!("{silent} · a new copy loads")));
    rig.host.wait_idle();
    let built = start.elapsed();
    assert_eq!(rig.host.replacements(), [(replaced_slot, key)]);
    let mut new = live(key, SAMPLE_RATE, replaced_slot).expect("the new copy");
    assert!(!new.faulted());
    assert_eq!(block(new.as_mut(), 1.0, 128), vec![1.0; 128]);
    assert!(rig.host.replacements().is_empty());
    // A new copy that asks for a restart gets no service while the copy
    // that faulted is in an engine.
    new.set_param(delay(0.5), 0);
    assert_eq!(block(new.as_mut(), 1.0, 128), vec![1.0; 128]);
    assert!(!rig.host.restarts_pending());
    assert!(
        rig.host.restarts().is_empty(),
        "the copy that faulted is held"
    );
    assert_eq!(retry(), 0, "the copy of the slot is the new copy");
    assert_eq!(
        row_fault(),
        Some(format!("{silent} · a new copy waits for the next note"))
    );
    // The copy that faulted reads as quiet, and ends in a job of the line.
    silence(faulty.as_mut());
    let copies = running();
    drop(faulty);
    assert_eq!(running(), copies - 1);
    assert_eq!(row_fault(), None, "the copy that faulted ended");
    assert!(live(key, SAMPLE_RATE, replaced_slot).is_none());
    assert_eq!(running(), copies - 1, "the new copy keeps its slot");
    // The new copy gets its service after the copy that faulted ended.
    assert!(rig.host.restarts_pending());
    assert_eq!(rig.host.restarts(), [(replaced_slot, key)]);
    faults(new.as_mut());
    assert_eq!(retry(), 0, "the new copy waits for the line of its fault");
    assert_eq!(rig.host.take_errors(), live_lines);
    assert_eq!(retry(), 0, "the new copy waits for the time of its build");
    std::thread::sleep(built + Duration::from_millis(200));
    assert_eq!(retry(), 1);
    rig.host.wait_idle();
    // SAFETY: as above. The builds read the variable.
    unsafe { std::env::remove_var(SLOW_CLASS_ENV) };
    let third = live(key, SAMPLE_RATE, replaced_slot).expect("the third copy");
    assert!(rig.host.replacements().is_empty());
    drop(new);
    drop(third);
    rig.host.wait_idle();

    // A copy of a render chain gives its lines to the take of its render
    // alone, each one time. Its fault and a lost note refuse a note of the
    // render, and its other counts refuse none. The copies of the chain
    // leave the counts at the take, and the end of a chain drops the counts
    // no take read. The render runs at a rate of its own, so the wait of the
    // key at 48 kHz from the part before does not stop a try there.
    let _ = rig.host.take_errors();
    let render_rate = 44_100;
    let render_copy = |slot| {
        let built = rig.host.prepare_render(key, render_rate, slot, &[], true);
        assert_eq!(built.expect("the render build").recv(), Ok(Ok(0)));
        let copy = rig.host.insert_render(key, render_rate, slot, None);
        copy.expect("the render copy")
    };
    let render_chain = 27;
    let mut silent = render_copy(InsertSlot::effect(render_chain, 0));
    let mut counted = render_copy(InsertSlot::effect(render_chain, 1));
    faults(silent.as_mut());
    assert_eq!(row_fault(), None, "a copy of a render has no row fault");
    counted.set_param(fault(1.0), 0);
    for frame in 0..513 {
        counted.set_param(gain(1.0), 200 + frame);
    }
    for frame in 0..=128 {
        let note = InsertNote {
            pitch: 60.0,
            velocity: 1.0,
            frames: 1,
        };
        counted.note(note, frame);
    }
    assert_eq!(block(counted.as_mut(), 1.0, 128), vec![0.0; 128]);
    assert!(rig.host.take_errors().is_empty());
    assert!(rig.host.take_render_errors(&[render_chain]).is_empty());
    let [not_a_number, refused, _] = live_lines.clone();
    let faulted = rig.host.take_render_faults(&[render_chain]);
    assert_eq!(
        faulted.refusals,
        [
            format!(
                "vst {NAME}: no number in the output for 1 s, so the stage plays silence to the end"
            ),
            format!("vst {NAME}: a copy holds 128 notes, and a note was lost"),
        ]
    );
    assert_eq!(
        faulted.warnings,
        [
            not_a_number,
            refused,
            format!("vst {NAME}: too many parameter values at one time, and a value was lost"),
        ]
    );
    assert_eq!(
        rig.host.take_render_faults(&[render_chain]),
        RenderFaults::default()
    );
    assert_eq!(block(counted.as_mut(), 1.0, 128), vec![0.0; 128]);
    assert_eq!(
        rig.host.take_render_faults(&[render_chain]),
        RenderFaults::default(),
        "a copy after the take of its render"
    );
    assert!(rig.host.take_errors().is_empty());
    let copy = WantedCopy {
        slot: InsertSlot::effect(render_chain, 0),
        name: NAME.to_owned(),
        preset: None,
    };
    assert_eq!(
        rig.host.retry(&[copy], render_rate),
        0,
        "a copy of a render gets no try"
    );
    let mut unread = render_copy(InsertSlot::effect(render_chain + 1, 0));
    unread.set_param(fault(1.0), 0);
    assert_eq!(block(unread.as_mut(), 1.0, 128), vec![0.0; 128]);
    drop(silent);
    drop(counted);
    drop(unread);
    let ended = vec![render_chain, render_chain + 1];
    rig.host.keep(1, Vec::new(), Vec::new(), Vec::new(), ended);
    assert!(
        rig.host.take_errors().is_empty(),
        "a render chain that ended"
    );
    rig.host.wait_idle();

    // A retune moves the copy to the build at its new rate. So a copy that
    // waited at 48 kHz and faults in an engine at 44.1 kHz gets a new copy
    // there: the row says that the new copy loads, and the new copy gets no
    // restart service while the copy that faulted is in the engine.
    let _ = rig.host.take_errors();
    let retuned_slot = InsertSlot::effect(31, 0);
    let retuned = until(|| live(key, SAMPLE_RATE, retuned_slot));
    drop(retuned);
    until(|| home(retuned_slot, SAMPLE_RATE));
    let mut retuned = until(|| live(key, 44_100, retuned_slot));
    faults(retuned.as_mut());
    assert_eq!(rig.host.take_errors(), live_lines);
    let wanted = WantedCopy {
        slot: retuned_slot,
        name: NAME.to_owned(),
        preset: None,
    };
    // SAFETY: as above. The plugin thread reads the variable in the build
    // below.
    unsafe { std::env::set_var(SLOW_CLASS_ENV, "300") };
    assert_eq!(rig.host.retry(std::slice::from_ref(&wanted), 44_100), 1);
    let loads = "a copy plays silence: no number in its output for 1 s · a new copy loads";
    assert_eq!(row_fault().as_deref(), Some(loads));
    rig.host.wait_idle();
    // SAFETY: as above. The build read the variable.
    unsafe { std::env::remove_var(SLOW_CLASS_ENV) };
    assert_eq!(rig.host.replacements(), [(retuned_slot, key)]);
    let mut new = live(key, 44_100, retuned_slot).expect("the new copy");
    new.set_param(delay(0.5), 0);
    assert_eq!(block(new.as_mut(), 1.0, 128), vec![1.0; 128]);
    assert!(
        rig.host.restarts().is_empty(),
        "the copy that faulted is held"
    );
    // The copy that faulted reads as quiet and ends in a job of the line.
    // The new copy then gets its service.
    silence(retuned.as_mut());
    drop(retuned);
    rig.host.wait_idle();
    assert_eq!(rig.host.restarts(), [(retuned_slot, key)]);
    drop(new);
    rig.host.wait_idle();
}

/// The watch of the preset folders reads the disk again when a file comes
/// in or goes out: a preset the player saves while the studio runs counts
/// with no rescan and no ask. Each read takes the next number. A file saved
/// again ends the copies of its old state that wait for their slot.
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
    // The read of the watch ends the copy that waits for its slot when the
    // player saves the file again. The next take builds a copy of the saved
    // file.
    let slot = InsertSlot::effect(1, 0);
    let late = Some("Late Save");
    let prepared = || rig.host.prepared(NAME, late, false, SAMPLE_RATE, slot);
    rig.host.prepare(NAME, late, false, SAMPLE_RATE, slot);
    rig.host.wait_idle();
    let Prepared::Ready(key) = prepared() else {
        panic!("no copy waits for its slot");
    };
    // Each save gives the file a new length, so 2 saves in one tick of the
    // file clock (1 s on HFS+, 2 s on FAT) still change the stamp. With a
    // delay of 0, the state of 24 bytes plays as the state of 16 bytes.
    let save = |bytes: &[u8]| rig.write_preset("Late Save.vstpreset", bytes);
    save(&rustel_vst3_fixture::preset_with_delay(1.0, 0.0, 0.0));
    until(|| (prepared() == Prepared::Unbuilt(key)).then_some(()));
    let provider = rig.host.provider(false);
    let mut held = until(|| provider(key, SAMPLE_RATE, slot));
    assert_eq!(block(held.as_mut(), 1.0, 16), vec![1.0; 16]);
    silence(held.as_mut());
    // A copy that plays keeps the state of its file, and a read of the
    // folders marks it. The mark takes the park at the drop away, also from
    // a quiet copy: the copy goes home in a job of the plugin thread, and
    // the park there ends it. A slow build for another slot holds that
    // thread, so the job still waits at the look.
    save(&rustel_vst3_fixture::preset(0.5, 0.0));
    rig.host.set_preset_folders(Vec::new());
    let running = || {
        rig.host.wait_idle();
        rig.host.plugins()[0].running
    };
    let copies = running();
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variable in the build below. No other thread reads a
    // variable now.
    unsafe { std::env::set_var(SLOW_ACTIVATE_ENV, "200") };
    rig.host
        .prepare(NAME, late, false, SAMPLE_RATE, InsertSlot::effect(2, 0));
    drop(held);
    let home = prepared();
    let after = running();
    // SAFETY: as above. The copy read the variable at its build.
    unsafe { std::env::remove_var(SLOW_ACTIVATE_ENV) };
    assert_eq!(home, Prepared::Returning);
    // The build adds one copy, and the park ends the marked copy.
    assert_eq!(after, copies);
    assert_eq!(prepared(), Prepared::Unbuilt(key));
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
    // it, or the watch would read the disk for good. Windows can say one
    // event of the removal late, behind the quiet of the rest, so the
    // reads settle first: a read that follows the last change drains, and
    // the quiet after it reads nothing.
    let mut settled = rig.host.presets_version();
    let drain = Instant::now() + Duration::from_secs(2);
    while Instant::now() < drain {
        std::thread::sleep(Duration::from_millis(100));
        settled = rig.host.presets_version();
    }
    assert_eq!(rig.host.presets_version(), settled);
}

/// A preset folder that is not there yet watches the folder above it, and
/// that folder alone: a file written deeper in it reads nothing.
#[test]
fn a_missing_preset_folder_watches_only_the_folder_above_it() {
    let rig = Rig::new("missing-folder");
    let deeper = rig.root.join("elsewhere").join("deeper");
    std::fs::create_dir_all(&deeper).expect("a folder beside");
    rig.host.set_preset_folders(vec![rig.root.join("not-yet")]);
    rig.host.wait_idle();
    let mut settled = rig.host.presets_version();
    let drain = Instant::now() + Duration::from_secs(2);
    while Instant::now() < drain {
        std::thread::sleep(Duration::from_millis(100));
        settled = rig.host.presets_version();
    }

    std::fs::write(deeper.join("notes.txt"), "not a preset").expect("a file elsewhere");
    std::thread::sleep(Duration::from_secs(1));

    assert_eq!(
        rig.host.presets_version(),
        settled,
        "a write elsewhere read the preset folders"
    );
}

/// A missing folder inside a folder of the list leaves the watch of that
/// folder whole: a preset saved deep in it still reads the folders.
#[test]
fn a_missing_folder_inside_a_listed_folder_keeps_its_watch_whole() {
    let rig = Rig::new("missing-inside");
    rig.plugin();
    let bank = rig.root.join("bank");
    let deep = bank.join("Lead").join("Saws");
    std::fs::create_dir_all(&deep).expect("a bank with folders");
    rig.host
        .set_preset_folders(vec![bank.clone(), bank.join("gone")]);
    rig.host.wait_idle();
    // The reads the change asked for settle first.
    std::thread::sleep(Duration::from_secs(2));

    std::fs::write(
        deep.join("Deep Save.vstpreset"),
        rustel_vst3_fixture::preset(0.5, 0.0),
    )
    .expect("a preset deep in the bank");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !rig
        .host
        .presets(NAME)
        .is_some_and(|names| names.iter().any(|name| name == "Deep Save"))
    {
        assert!(
            Instant::now() < deadline,
            "the save deep in the bank was not read"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A change of the preset folders reads them again on the plugin thread:
/// the read runs from the change itself, no caller waits for it, and the
/// presets of the folder the change brought are there when it ends.
#[test]
fn a_change_of_the_preset_folders_reads_them_in_the_background() {
    let rig = Rig::new("folder-change");
    // The load gives the plugin its class, so the names have an answer.
    rig.plugin();
    rig.host.wait_idle();
    assert!(!rig.host.presets_reading());

    // A folder the player added holds any structure: the header of each
    // file names its plugin.
    let added = rig.root.join("added");
    std::fs::create_dir_all(&added).expect("folder");
    std::fs::write(
        added.join("Added Bass.vstpreset"),
        rustel_vst3_fixture::preset(0.5, 0.0),
    )
    .expect("preset file");
    rig.host.set_preset_folders(vec![added]);
    assert!(
        rig.host.presets_reading(),
        "the read runs from the change, on the plugin thread"
    );
    rig.host.wait_idle();
    assert!(!rig.host.presets_reading());
    assert_eq!(
        rig.host.presets(NAME),
        Some(vec!["Added Bass".to_owned()]),
        "the read of the change lists the presets of the folder"
    );
}

/// A preset name of the root and of a folder the player added plays the
/// file of the root, also in another case. A name keeps its number while
/// the folders give its file, and resolves again when they do not: a file
/// of the root takes the name, the file moves in its folder, or the folder
/// leaves the list. A copy of the number before runs on.
#[test]
fn a_preset_name_plays_the_file_the_folders_give_now() {
    let rig = Rig::new("preset-names");
    let plugin = rig.plugin();
    let added = rig.root.join("added");
    std::fs::create_dir_all(added.join("bank")).expect("folder");
    let (half, quarter) = (
        rustel_vst3_fixture::preset(0.5, 0.0),
        rustel_vst3_fixture::preset(0.25, 0.0),
    );
    std::fs::write(added.join("Pad.vstpreset"), &half).expect("preset file");
    std::fs::write(added.join("Lead.vstpreset"), &half).expect("preset file");
    rig.write_preset("pad.vstpreset", &quarter);
    // A read of the preset folders, done when the call returns.
    let read = |folders: Vec<PathBuf>| {
        rig.host.set_preset_folders(folders);
        rig.host.wait_idle();
    };
    read(vec![added.clone()]);
    let number = |name: &str| match rig.host.preset(plugin, name) {
        FoundPreset::Number(number) => number,
        other => panic!("preset {name}: {other:?}"),
    };
    // The level of a copy of a preset: the gain of its file.
    let level = |preset: u32| {
        let mut insert = rig.insert(preset).expect("insert");
        block(insert.as_mut(), 1.0, 64)[0]
    };
    let names = rig.host.presets(NAME).expect("the read");
    assert_eq!(names, ["Lead", "pad"]);
    assert_eq!(level(number("Pad")), 0.25);

    // The player saves a file of the root with the name of an added file.
    let lead = number("lead");
    let mut running = rig.insert(lead).expect("a copy of the added file");
    rig.write_preset("LEAD.vstpreset", &quarter);
    read(vec![added.clone()]);
    let saved = number("lead");
    assert_ne!(saved, lead);
    assert_eq!(level(saved), 0.25);
    assert_eq!(block(running.as_mut(), 1.0, 64), vec![0.5; 64]);

    // A file that moves in its folder: its copy builds from the new place.
    std::fs::write(added.join("Bass.vstpreset"), &half).expect("preset file");
    read(vec![added.clone()]);
    let bass = number("bass");
    let moved = added.join("bank").join("Bass.vstpreset");
    std::fs::rename(added.join("Bass.vstpreset"), moved).expect("move");
    read(vec![added.clone()]);
    let after = number("bass");
    assert_ne!(after, bass);
    assert_eq!(level(after), 0.5);

    // The folder leaves the list, and its presets with it.
    read(Vec::new());
    assert_eq!(rig.host.preset(plugin, "bass"), FoundPreset::Missing);
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
    // Before its copy, the key of the Half Level preset has the latency of
    // the copy of another preset of its plugin: the copy of default. The
    // host built no copy of the plugin at another rate, and no copy of
    // another plugin.
    let half = InsertKey { plugin, preset };
    let slot = InsertSlot::effect(1, 0);
    assert_eq!(rig.host.latency(half, SAMPLE_RATE, slot), Some(1024));
    assert_eq!(rig.host.latency(half, 44_100, slot), None);
    let another = InsertKey {
        plugin: plugin + 1,
        preset,
    };
    assert_eq!(rig.host.latency(another, SAMPLE_RATE, slot), None);
    let mut insert = rig.insert(preset).expect("insert");
    // The copy of the Half Level preset has a latency of its own. Its state
    // of 16 bytes has no delay.
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
    // Each build of the preset of the other plugin fails.
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
    // A build of a render that fails gives its reason to the render, and
    // its line goes to the render only: the live session takes no line of
    // a render. The bound counts no build of a render, so the full bound
    // ends no failure for it.
    let refused = capacity * 4;
    let read = rig
        .host
        .prepare_render(key, SAMPLE_RATE, InsertSlot::effect(refused, 0), &[], true)
        .expect("the render build");
    let Ok(Err(reason)) = read.recv() else {
        panic!("the build of the preset of the other plugin did not fail");
    };
    assert!(
        reason.starts_with(&format!("{NAME} did not start: ")),
        "{reason}"
    );
    assert!(rig.host.take_errors().is_empty());
    assert_eq!(
        rig.host.take_render_errors(&[refused]),
        [format!("vst skipped: {reason}")]
    );
    assert_eq!(prepared(refused), Prepared::Unavailable(reason));

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
    rig.host
        .keep(1, vec![wanted.clone()], vec![0], Vec::new(), Vec::new());
    let bare = InsertKey { plugin, preset: 0 };
    assert!(
        provider(bare, SAMPLE_RATE, parked).is_none(),
        "the start state"
    );
    let first = capacity + 1;
    rig.host
        .keep(2, Vec::new(), vec![first], Vec::new(), Vec::new());
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
    // The bound ends a copy a caller keeps at rest after each copy of a
    // chain no caller names: the lowest rest order first, then the oldest
    // copy. Caller 2 keeps 20 parked copies at rest, the older 10 with
    // order 2 and the newer 10 with order 1, and the oldest of order 2 with
    // order 1 too: the higher order counts. It keeps the copy caller 1
    // wants too, and that copy stays. Caller 1 names the chains of order 1
    // and each newer parked chain, and the new chains before their builds: a
    // named copy that only a copy at rest asks for ends too.
    let copy = |chain| WantedCopy {
        slot: InsertSlot::effect(chain, 0),
        ..wanted.clone()
    };
    let mut rest = vec![(wanted.clone(), 1)];
    rest.extend((first + 2..first + 12).map(|chain| (copy(chain), 2)));
    rest.extend((first + 12..first + 22).map(|chain| (copy(chain), 1)));
    let whole = WantedCopy {
        name: NAME.to_owned(),
        ..copy(first + 2)
    };
    rest.push((whole, 1));
    rig.host
        .keep(2, Vec::new(), vec![first], rest.clone(), Vec::new());
    let new = capacity * 2 + 3;
    let extra = capacity * 3;
    let mut named = vec![0];
    named.extend(first + 12..=capacity * 2);
    named.extend(new..new + 14);
    named.push(extra);
    rig.host
        .keep(1, vec![wanted.clone()], named, Vec::new(), Vec::new());
    let ending = (first + 12..first + 22).chain([first + 2]);
    for (chain, ends) in (new..new + 11).zip(ending) {
        assert_eq!(ready(ends), Prepared::Ready(default), "chain {ends}");
        rig.host
            .prepare(NAME, None, false, SAMPLE_RATE, InsertSlot::effect(chain, 0));
        rig.host.wait_idle();
        assert_eq!(ready(chain), Prepared::Ready(default), "chain {chain}");
        assert_eq!(ready(ends), Prepared::Unbuilt(default), "chain {ends}");
    }
    // A render ends no copy at rest: with only copies at rest and copies of
    // named chains parked, it builds past the bound. The bound does not
    // count the build of the render and does not end it before the render
    // takes its copy, so a live build in that time ends one copy at rest.
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variable in the builds below. No other thread reads a
    // variable now.
    unsafe { std::env::set_var(SLOW_ACTIVATE_ENV, "300") };
    let render = std::thread::scope(|scope| {
        let render =
            scope.spawn(|| provider(default, SAMPLE_RATE, InsertSlot::effect(new + 11, 0)));
        // A slow test thread finds the render done, and the live build
        // comes after the take.
        until(|| (ready(new + 11) == Prepared::Pending || render.is_finished()).then_some(()));
        rig.host
            .prepare(NAME, None, false, SAMPLE_RATE, InsertSlot::effect(extra, 0));
        render.join().expect("the render")
    });
    rig.host.wait_idle();
    // SAFETY: as above. The copies read the variable at their builds.
    unsafe { std::env::remove_var(SLOW_ACTIVATE_ENV) };
    assert!(render.is_some(), "a render past the bound");
    assert_eq!(ready(extra), Prepared::Ready(default));
    assert_eq!(
        ready(first + 3),
        Prepared::Unbuilt(default),
        "the live build"
    );
    for chain in first + 4..first + 12 {
        assert_eq!(ready(chain), Prepared::Ready(default), "chain {chain}");
    }
    drop(render);
    // A copy that comes home ends a build of a lower rank only, and the
    // order of the copies does not count. An engine takes the copy of the
    // newest chain at rest with order 2, and a build for a new named chain
    // fills the bound again. No copy at rest has a lower order, so the copy
    // ends when it comes home. It played a block, so it goes home in a job
    // of the plugin thread.
    let live = rig.host.provider(false);
    let running = || {
        rig.host.wait_idle();
        rig.host.plugins()[0].running
    };
    let take = |chain| live(default, SAMPLE_RATE, InsertSlot::effect(chain, 0));
    let build = |chain| {
        rig.host
            .prepare(NAME, None, false, SAMPLE_RATE, InsertSlot::effect(chain, 0));
    };
    let mut held = take(first + 11).expect("a copy at rest");
    build(new + 12);
    let copies = running();
    assert_eq!(ready(new + 12), Prepared::Ready(default));
    block(held.as_mut(), 1.0, 128);
    drop(held);
    assert_eq!(running(), copies - 1);
    assert_eq!(ready(first + 11), Prepared::Unbuilt(default));
    // Caller 2 gives one other chain at rest the order 1. A quiet copy of a
    // chain at rest with order 2 then parks at its drop in place of the copy
    // of order 1, which ends.
    for (kept, order) in &mut rest {
        if kept.slot.chain == first + 10 {
            *order = 1;
        }
    }
    rig.host.keep(2, Vec::new(), vec![first], rest, Vec::new());
    let held = take(first + 9).expect("a copy at rest");
    build(new + 13);
    let copies = running();
    drop(held);
    assert_eq!(running(), copies - 1);
    assert_eq!(ready(first + 9), Prepared::Ready(default));
    assert_eq!(ready(first + 10), Prepared::Unbuilt(default));
    // Caller 2 keeps no copy at rest now: a named copy that only a copy at
    // rest asks for has a rank. No caller names the chains it kept at rest
    // yet. A copy of a chain no caller names ends a failed build only: with
    // none, the copy of the newest such chain ends when it comes home, and
    // the oldest stays.
    rig.host
        .keep(2, Vec::new(), vec![first], Vec::new(), Vec::new());
    let held = take(first + 9).expect("a copy of no named chain");
    build(new + 14);
    let copies = running();
    drop(held);
    assert_eq!(running(), copies - 1);
    assert_eq!(ready(first + 9), Prepared::Unbuilt(default));
    assert_eq!(ready(first + 4), Prepared::Ready(default));
    // The bound is full. A render builds and parks its copy past it, and
    // ends no copy of it. Later builds at the full bound end no render copy.
    let chains = [0].into_iter().chain(first..=capacity * 2);
    let chains: Vec<u64> = chains.chain(new..new + 15).chain([extra]).collect();
    let parked_chains = || {
        let parked = chains.iter().copied();
        let parked = parked.filter(|chain| ready(*chain) == Prepared::Ready(default));
        parked.collect::<Vec<u64>>()
    };
    let before = parked_chains();
    let copies = running();
    let render_chain = capacity * 4 + 1;
    let read = rig
        .host
        .prepare_render(
            default,
            SAMPLE_RATE,
            InsertSlot::effect(render_chain, 0),
            &[],
            true,
        )
        .expect("the render build");
    assert_eq!(read.recv(), Ok(Ok(1024)));
    assert_eq!(ready(render_chain), Prepared::Ready(default));
    assert_eq!(running(), copies + 1);
    assert_eq!(parked_chains(), before);
    // With each chain named, a new build does not start. Two such builds
    // give one line.
    let more = capacity * 2 + 1;
    let mut named = vec![0];
    named.extend(first + 2..=more);
    named.extend(new..new + 15);
    named.push(extra);
    rig.host
        .keep(1, vec![wanted], named, Vec::new(), Vec::new());
    build(more);
    build(more + 1);
    rig.host.wait_idle();
    assert_eq!(ready(more), Prepared::Unbuilt(default));
    assert_eq!(
        rig.host.take_errors(),
        [format!(
            "vst skipped: {NAME} did not start: the host keeps {capacity} copies ready at most"
        )]
    );
    assert_eq!(ready(render_chain), Prepared::Ready(default), "the render");
    // keep() ends the render chain and its copy.
    let copies = running();
    rig.host
        .keep(3, Vec::new(), Vec::new(), Vec::new(), vec![render_chain]);
    assert_eq!(running(), copies - 1);
    assert_eq!(ready(render_chain), Prepared::Unbuilt(default));
    // A note takes the parked copy with no second build, and a build of the
    // named chain `more` takes the place the take freed. The slot gives the
    // latency of the copy while the engine holds it, and after. Through the
    // copy, the input comes out 1024 frames late.
    let mut held = live(default, SAMPLE_RATE, parked).expect("the parked copy");
    build(more);
    rig.host.wait_idle();
    assert_eq!(ready(more), Prepared::Ready(default));
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, parked), Some(1024));
    let mut output = Vec::new();
    for _ in 0..16 {
        output.extend(block(held.as_mut(), 1.0, 128));
    }
    assert_eq!(output[..1024], [0.0; 1024]);
    assert_eq!(output[1024..], [0.5; 1024]);
    drop(held);
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, parked), Some(1024));
    // The copy goes home at its drop. Each parked copy is a copy of a named
    // chain, so no build ranks: the copy of a named chain parks past the
    // bound. It waits for its slot again with the latency of its build.
    until(|| (ready(0) == Prepared::Ready(default)).then_some(()));
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, parked), Some(1024));
    // A new file `default` gives the next copy of the key a delay of 2048
    // frames. The next read of the preset folders ends each copy of the
    // file before that waits for its slot. The watch reads the folders 400
    // ms after the write, and the read here comes first.
    let read = || {
        let version = rig.host.presets_version();
        rig.host.set_preset_folders(Vec::new());
        until(|| (rig.host.presets_version() > version).then_some(()));
    };
    rig.write_preset(
        "default.vstpreset",
        &rustel_vst3_fixture::preset_with_delay(0.5, 0.0, 0.5),
    );
    read();
    until(|| (ready(first) == Prepared::Unbuilt(default)).then_some(()));
    assert_eq!(ready(0), Prepared::Unbuilt(default));
    // The slot of the chain `first` builds a copy from the new file. A slot
    // with no copy gives the latency of the last copy.
    let waiting = InsertSlot::effect(first, 0);
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, waiting);
    rig.host.wait_idle();
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, waiting), Some(2048));
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, parked), Some(2048));
    // A note takes that copy. A copy that plays keeps the state of its
    // file: a file saved again changes nothing while the engine holds the
    // copy, and the copy ends at its drop. After the drop, the slot gives
    // the latency of the last copy.
    let held = live(default, SAMPLE_RATE, waiting).expect("the waiting copy");
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, waiting), Some(2048));
    rig.write_preset("default.vstpreset", &rustel_vst3_fixture::preset(0.5, 0.0));
    read();
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, waiting), Some(2048));
    let copies = running();
    drop(held);
    assert_eq!(running(), copies - 1);
    assert_eq!(ready(first), Prepared::Unbuilt(default));
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, waiting), Some(2048));
    // A copy that waits for its slot keeps the state of its file at its
    // build. A take at another rate sets the copy up again, and a file
    // saved since its build gives a new copy from the file.
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, waiting);
    rig.host.wait_idle();
    assert_eq!(rig.host.latency(default, SAMPLE_RATE, waiting), Some(0));
    rig.write_preset(
        "default.vstpreset",
        &rustel_vst3_fixture::preset_with_delay(0.5, 0.0, 0.25),
    );
    rig.host.prepare(NAME, None, false, 44_100, waiting);
    rig.host.wait_idle();
    assert_eq!(rig.host.latency(default, 44_100, waiting), Some(1024));

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
    // The copy is a take for live play, so the clock of a note goes
    // through the box of the take.
    let live = rig.host.provider(false);
    let key = InsertKey {
        plugin: rig.plugin(),
        preset: 0,
    };
    let mut insert = until(|| live(key, SAMPLE_RATE, InsertSlot::effect(1, 0)));
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

    // A stopped output drains a copy for live play with silence, and the
    // position stands. After 11 904 frames of play and a drain of 12 032
    // frames, the gate is open for 96 more frames.
    let mut lent = until(|| live(key, SAMPLE_RATE, InsertSlot::effect(2, 0)));
    lent.set_param(
        InsertParam {
            id: PARAM_GATE,
            value: 1.0,
        },
        0,
    );
    for _ in 0..93 {
        block(lent.as_mut(), 1.0, 128);
    }
    for _ in 0..94 {
        let (mut left, mut right) = ([0.0f32; 128], [0.0f32; 128]);
        lent.drain(&mut left, &mut right);
    }
    let output = block(lent.as_mut(), 1.0, 128);
    assert!(output[..86].iter().all(|sample| *sample == 1.0));
    assert!(output[106..].iter().all(|sample| *sample == 0.0));
}

/// The second plugin of the bundle is an instrument. A note starts at its
/// frame, sounds at its pitch and level, and ends after its length. A copy
/// for live play goes home with its note ended.
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
    // events. Each note sounds for its 100 frames, and each one ends. One
    // note more is lost, with no allocation, and the host gives its line.
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
        let lost = InsertNote {
            pitch: 0.0,
            velocity: 1.0,
            frames: 100,
        };
        insert.note(lost, 20);
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
    assert_eq!(
        rig.host.take_errors(),
        [format!(
            "vsti {TONE_NAME}: a copy holds 128 notes, and a note was lost"
        )]
    );
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

    // A copy for live play goes home at its drop with its note ended, and
    // the next take of its slot builds nothing.
    let live = rig.host.provider(false);
    let slot = InsertSlot::instrument(4);
    let mut lent = until(|| live(key, SAMPLE_RATE, slot));
    let long = InsertNote {
        frames: SAMPLE_RATE,
        ..note
    };
    lent.note(long, 0);
    let sounds = block(lent.as_mut(), 0.0, 128);
    assert!(sounds.iter().any(|sample| *sample != 0.0));
    let running = || {
        rig.host.wait_idle();
        let plugins = rig.host.plugins().into_iter();
        let tone = plugins.filter(|plugin| plugin.name == TONE_NAME);
        tone.map(|plugin| plugin.running).sum::<usize>()
    };
    let copies = running();
    drop(lent);
    let home = || rig.host.prepared(TONE_NAME, None, true, SAMPLE_RATE, slot);
    until(|| (home() == Prepared::Ready(key)).then_some(()));
    let mut lent = live(key, SAMPLE_RATE, slot).expect("the copy that came home");
    assert_eq!(running(), copies);
    assert!(
        block(lent.as_mut(), 0.0, 128)
            .iter()
            .all(|sample| *sample == 0.0)
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
    // The copies are takes for live play, so each reset and each cut goes
    // through the box of the take.
    let live = rig.host.provider(false);
    let key = InsertKey {
        plugin: rig.plugin(),
        preset: 0,
    };
    let mut effect = until(|| live(key, SAMPLE_RATE, InsertSlot::effect(1, 0)));
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
    let mut instrument = until(|| live(key, SAMPLE_RATE, InsertSlot::instrument(1)));
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
    // The copy is a take for live play, so a value set now goes through
    // the box of the take.
    let live = rig.host.provider(false);
    let key = InsertKey {
        plugin: rig.plugin(),
        preset: 0,
    };
    let mut insert = until(|| live(key, SAMPLE_RATE, InsertSlot::effect(1, 0)));
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
    // The restore the full queue rejected is a lost value, and the host
    // gives its line one time.
    assert_eq!(
        rig.host.take_errors(),
        [format!(
            "vst {NAME}: too many parameter values at one time, and a value was lost"
        )]
    );

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
    // Its copy gets no new try: the copy has no key, and no load runs.
    let copy = WantedCopy {
        slot: InsertSlot::effect(1, 0),
        name: NAME.to_owned(),
        preset: None,
    };
    assert_eq!(rig.host.retry(&[copy], SAMPLE_RATE), 0);
    assert!(rig.host.loads().is_empty());
    rig.host.wait_idle();
    assert!(matches!(rig.host.plugins()[0].status, Status::Failed(_)));

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
/// runs no new test of the bundle. A rescan empties the cache and its file,
/// so the bundle gets its new test, also at the next start when the host
/// ends before that test.
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

    // A rescan with no scan process runs no test. A host that starts after
    // it reads no fault of the cache file: the bundle waits for its test.
    host.set_worker(None);
    host.rescan(&folders);
    host.wait_idle();
    let again = Host::new();
    again.set_preset_folder(root.join("presets"));
    again.scan(&folders);
    again.wait_idle();
    assert_eq!(again.plugins()[0].status, Status::Found);
    assert_eq!(runs(), 1);

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
    let live = rig.host.provider(false);
    let running = until(|| live(key, SAMPLE_RATE, slot));
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
    // The copy ran no block, so it parks at its drop, and the unload ends
    // it.
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, slot), Some(0));
    unload(&[]);
    assert_eq!(loaded(&rig), 0);
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, slot), None);
    assert_eq!(rig.host.plugins().len(), 2, "the two rows stay in the list");

    assert_eq!((number(TONE_NAME), number(NAME)), (tone, effect));
    assert_eq!(loaded(&rig), 2);

    // A render keeps its parked copy and its bundle until its chain ends,
    // and so does a render whose copy ended in its job: the render builds
    // its key again at a note.
    let parks = rig
        .host
        .prepare_render(key, SAMPLE_RATE, InsertSlot::effect(3, 0), &[], true)
        .expect("the render build");
    assert_eq!(parks.recv(), Ok(Ok(0)));
    unload(&[]);
    assert_eq!(loaded(&rig), 2);
    assert_eq!(rig.host.plugins()[0].running, 1);
    let ends = rig
        .host
        .prepare_render(key, SAMPLE_RATE, InsertSlot::effect(4, 0), &[], false)
        .expect("the render build");
    assert_eq!(ends.recv(), Ok(Ok(0)));
    rig.host
        .keep(1, Vec::new(), Vec::new(), Vec::new(), vec![3]);
    unload(&[]);
    assert_eq!(rig.host.plugins()[0].running, 0);
    assert_eq!(loaded(&rig), 2);
    rig.host
        .keep(1, Vec::new(), Vec::new(), Vec::new(), vec![4]);
    unload(&[]);
    assert_eq!(loaded(&rig), 0);
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
    // The orbit holds the plugin of the slot, so the next request builds no
    // second copy.
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
    // The orbit has 1 copy, and each of the 3 prepared slots has one.
    rig.host.wait_idle();
    let built = rig.host.prepared(NAME, None, false, SAMPLE_RATE, early);
    assert_eq!(built, Prepared::Ready(key));
    assert_eq!(rig.host.plugins()[0].running, 4);
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
    let keep = |copy: &WantedCopy| {
        rig.host
            .keep(1, vec![copy.clone()], vec![5], Vec::new(), Vec::new())
    };
    assert_eq!(keep(&other), [(slot, key)]);
    assert_eq!(running(), parked - 1);
    assert_eq!(keep(&other), [(slot, key)]);
    assert!(
        rig.host
            .keep(2, Vec::new(), vec![6], Vec::new(), Vec::new())
            .is_empty()
    );
    rig.host
        .keep(2, Vec::new(), Vec::new(), Vec::new(), Vec::new());
    assert!(keep(&wanted).is_empty());

    // The copy goes home at its drop and waits for its slot again: the next
    // take builds nothing. A take at another rate sets the same copy up at
    // the new rate.
    let ready = |rate| rig.host.prepared(NAME, None, false, rate, slot);
    drop(held);
    let home = ready(44_100);
    assert!(
        matches!(home, Prepared::Returning | Prepared::Ready(_)),
        "{home:?}"
    );
    until(|| (ready(44_100) == Prepared::Ready(key)).then_some(()));
    let held = provider(key, 44_100, slot).expect("the copy that came home");
    assert_eq!(running(), 4);
    drop(held);
    until(|| (ready(44_100) == Prepared::Ready(key)).then_some(()));
    assert!(
        provider(key, SAMPLE_RATE, slot).is_none(),
        "the retune runs"
    );
    let mut held = until(|| provider(key, SAMPLE_RATE, slot));
    assert_eq!(block(held.as_mut(), 0.5, 16), vec![0.5; 16]);
    assert_eq!(running(), 4);
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, slot), Some(0));
    drop(held);
    until(|| (ready(SAMPLE_RATE) == Prepared::Ready(key)).then_some(()));
    // The slot with no copy gives the latency of the last copy of its key
    // at the rate. The copy of a slot its chain does not want ends.
    assert_eq!(rig.host.latency(key, 44_100, slot), Some(0));
    assert_eq!(rig.host.latency(key, 22_050, slot), None);
    // A quiet copy parks at its drop, on the thread of the drop: its slot is
    // ready at once, also while the plugin thread builds. A value that waits
    // for a later frame goes at the drop. A copy that sounds at its drop
    // goes home in a job of the plugin thread, which plays out its tail.
    // That copy turns its plugin off and on slowly, so its job still runs at
    // the look.
    let mut held = provider(key, SAMPLE_RATE, slot).expect("the parked copy");
    silence(held.as_mut());
    let later = InsertParam {
        id: PARAM_GAIN,
        value: 0.25,
    };
    held.set_param(later, 1_000);
    let loud = InsertSlot::effect(7, 0);
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variable in the build below. No other thread reads a
    // variable now.
    unsafe { std::env::set_var(SLOW_ACTIVATE_ENV, "200") };
    assert!(provider(key, SAMPLE_RATE, loud).is_none());
    drop(held);
    assert_eq!(ready(SAMPLE_RATE), Prepared::Ready(key));
    let held = provider(key, SAMPLE_RATE, slot).expect("the copy that parked at its drop");
    drop(held);
    let mut held = until(|| provider(key, SAMPLE_RATE, loud));
    // SAFETY: as above. The copy read the variable at its build.
    unsafe { std::env::remove_var(SLOW_ACTIVATE_ENV) };
    assert_eq!(running(), 5);
    assert_eq!(block(held.as_mut(), 0.5, 16), vec![0.5; 16]);
    drop(held);
    let home = || rig.host.prepared(NAME, None, false, SAMPLE_RATE, loud);
    assert_eq!(home(), Prepared::Returning);
    rig.host.wait_idle();
    assert_eq!(home(), Prepared::Ready(key));
    // The copy of the other slot ends with its chain.
    rig.host
        .keep(3, Vec::new(), Vec::new(), Vec::new(), vec![7]);
    assert_eq!(running(), 4);
    // The door of the copies on their way home counts the copy caller 1
    // wants at another rate, and starts its retune. A copy ready at the
    // rate does not count, and nor does the build of a new copy. That build
    // turns its plugin on slowly, so it still runs at the look.
    assert_eq!(rig.host.returning(1, 44_100), 1, "the retune");
    until(|| (ready(44_100) == Prepared::Ready(key)).then_some(()));
    assert_eq!(rig.host.returning(1, 44_100), 0, "a copy at the rate");
    // A copy a caller keeps at rest does not count, and keeps its rate: its
    // score does not play.
    let rest = vec![(wanted.clone(), 1)];
    rig.host.keep(2, Vec::new(), Vec::new(), rest, Vec::new());
    assert_eq!(rig.host.returning(2, SAMPLE_RATE), 0, "a copy at rest");
    assert_eq!(ready(44_100), Prepared::Ready(key));
    rig.host
        .keep(2, Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let built = WantedCopy {
        slot: InsertSlot::effect(5, 3),
        ..other.clone()
    };
    let both = vec![wanted.clone(), built.clone()];
    assert!(
        rig.host
            .keep(1, both, vec![5], Vec::new(), Vec::new())
            .is_empty()
    );
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variable in the build below. No other thread reads a
    // variable now.
    unsafe { std::env::set_var(SLOW_ACTIVATE_ENV, "300") };
    rig.host.prepare(NAME, None, false, 44_100, built.slot);
    assert_eq!(rig.host.returning(1, 44_100), 0, "a build");
    // The retune of the copy of `wanted` to SAMPLE_RATE waits in the line
    // behind that build. keep() takes the build of the retune before the job
    // runs, so the job ends the copy.
    assert_eq!(
        rig.host.returning(1, SAMPLE_RATE),
        1,
        "the retune in the line"
    );
    assert!(keep(&built).is_empty());
    rig.host.wait_idle();
    // SAFETY: as above. The copy read the variable at its build.
    unsafe { std::env::remove_var(SLOW_ACTIVATE_ENV) };
    assert_eq!(running(), 4, "the copy of the retune ended");
    // At a new rate the slow copy turns off and on slowly too. The door
    // counts it until its retune ends.
    assert_eq!(rig.host.returning(1, SAMPLE_RATE), 1, "the slow retune");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(rig.host.returning(1, SAMPLE_RATE), 1, "the retune runs");
    rig.host.wait_idle();
    assert_eq!(rig.host.returning(1, SAMPLE_RATE), 0, "the retune ended");
    // The door counts nothing for the slot of the copy that ended in the
    // line, and a prepare there builds a new copy.
    assert!(keep(&wanted).is_empty());
    assert_eq!(rig.host.returning(1, SAMPLE_RATE), 0, "no retune is left");
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, slot);
    assert_ne!(ready(SAMPLE_RATE), Prepared::Unbuilt(key));
    until(|| (ready(SAMPLE_RATE) == Prepared::Ready(key)).then_some(()));
    assert!(keep(&other).is_empty());
    rig.host.wait_idle();
    rig.host
        .keep(1, Vec::new(), Vec::new(), Vec::new(), Vec::new());
    assert_eq!(prepared(NAME, false), Prepared::Unbuilt(key));
    assert_eq!(running(), 3);

    // The mode follows the chain: a build of a render runs offline, and a
    // live build in real time. The render provider builds a copy for a
    // render key with no parked copy, offline too, and a second call gives
    // a second copy. Each mark is read after the job of its build ended.
    let modes = rig.root.join("modes");
    let marks = || {
        let marks = std::fs::read_to_string(&modes).unwrap_or_default();
        let _ = std::fs::remove_file(&modes);
        marks
    };
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variable in the builds below. No other thread reads
    // a variable now.
    unsafe { std::env::set_var(MODE_MARK_ENV, &modes) };
    let render = InsertSlot::effect(20, 0);
    let parks = rig.host.prepare_render(key, SAMPLE_RATE, render, &[], true);
    assert_eq!(parks.expect("the render build").recv(), Ok(Ok(0)));
    assert_eq!(marks(), "setup offline\n");
    // A render copy at a new rate keeps its mode.
    let retuned = InsertSlot::effect(29, 0);
    for rate in [SAMPLE_RATE, 44_100] {
        let parks = rig.host.prepare_render(key, rate, retuned, &[], true);
        assert_eq!(parks.expect("the render build").recv(), Ok(Ok(0)));
    }
    assert_eq!(marks(), "setup offline\nsetup offline\n");
    rig.host
        .prepare(NAME, None, false, SAMPLE_RATE, InsertSlot::effect(21, 0));
    rig.host.wait_idle();
    assert_eq!(marks(), "setup realtime\n");
    let fresh = InsertSlot::effect(22, 0);
    let copies = running();
    let first = rig.host.insert_render(key, SAMPLE_RATE, fresh, None);
    assert!(first.is_some(), "the render copy");
    assert_eq!(marks(), "setup offline\n");
    let second = rig.host.insert_render(key, SAMPLE_RATE, fresh, None);
    assert!(second.is_some(), "a second render copy");
    assert_eq!(marks(), "setup offline\n");
    assert_eq!(running(), copies + 2);
    drop((first, second));
    // A plugin that refuses the offline mode renders in real time.
    let realtime = InsertSlot::effect(23, 0);
    // SAFETY: as above.
    unsafe { std::env::set_var(REALTIME_ONLY_ENV, "1") };
    let parks = rig
        .host
        .prepare_render(key, SAMPLE_RATE, realtime, &[], true);
    assert_eq!(parks.expect("the render build").recv(), Ok(Ok(0)));
    // SAFETY: as above. The copy read the variables at its build.
    unsafe {
        std::env::remove_var(REALTIME_ONLY_ENV);
        std::env::remove_var(MODE_MARK_ENV);
    }
    assert_eq!(marks(), "setup offline\nsetup realtime\n");
    let mut copy = rig
        .host
        .insert_render(key, SAMPLE_RATE, realtime, None)
        .expect("the copy in real time");
    assert_eq!(block(copy.as_mut(), 0.5, 16), vec![0.5; 16]);
    drop(copy);
    // A render queues no build for a slot with a build, or for a chain
    // that ended.
    let copies = running();
    let again = rig.host.prepare_render(key, SAMPLE_RATE, render, &[], true);
    assert_eq!(again.err(), Some(NotQueued::Exists));
    rig.host
        .keep(4, Vec::new(), Vec::new(), Vec::new(), vec![24]);
    let ended = InsertSlot::effect(24, 0);
    let again = rig.host.prepare_render(key, SAMPLE_RATE, ended, &[], true);
    assert_eq!(again.err(), Some(NotQueued::Ended));
    assert_eq!(running(), copies);

    // The render provider waits for a build until the stop of the render,
    // and its build then makes no copy.
    // SAFETY: as above. The plugin thread reads the variable in the build
    // below.
    unsafe { std::env::set_var(SLOW_ACTIVATE_ENV, "600") };
    let start = Instant::now();
    let stop = || start.elapsed() >= Duration::from_millis(100);
    let stopped = rig
        .host
        .insert_render(key, SAMPLE_RATE, InsertSlot::effect(25, 0), Some(&stop));
    assert!(stopped.is_none());
    assert!(start.elapsed() < Duration::from_millis(300));
    rig.host.wait_idle();
    // SAFETY: as above. The copy read the variable at its build.
    unsafe { std::env::remove_var(SLOW_ACTIVATE_ENV) };
    assert_eq!(running(), copies);

    // The park of a copy whose plugin asked for a restart runs before the
    // builds in the line: its stage comes back after the job in progress.
    // The copy is quiet at its drop, and its park still restarts it and
    // reads its new latency.
    let lane = InsertSlot::effect(26, 0);
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, lane);
    let mut copy = until(|| provider(key, SAMPLE_RATE, lane));
    let half = InsertParam {
        id: PARAM_DELAY,
        value: 0.5,
    };
    copy.set_param(half, 0);
    block(copy.as_mut(), 0.0, 128);
    silence(copy.as_mut());
    // SAFETY: as above. The plugin thread reads the variable in the builds
    // below.
    unsafe { std::env::set_var(SLOW_ACTIVATE_ENV, "1000") };
    let slow = [InsertSlot::effect(27, 0), InsertSlot::effect(28, 0)];
    for slot in slow {
        rig.host.prepare(NAME, None, false, SAMPLE_RATE, slot);
    }
    drop(copy);
    let look = |slot| rig.host.prepared(NAME, None, false, SAMPLE_RATE, slot);
    let second = until(|| (look(lane) == Prepared::Ready(key)).then(|| look(slow[1])));
    assert_eq!(second, Prepared::Pending, "the second slow build");
    assert_eq!(rig.host.latency(key, SAMPLE_RATE, lane), Some(2048));
    rig.host.wait_idle();
    // SAFETY: as above. The copies read the variable at their builds.
    unsafe { std::env::remove_var(SLOW_ACTIVATE_ENV) };
}

/// A read of the preset folders that starts during the load of a plugin
/// puts a prepare at the end of the line again. A wait for the plugin
/// thread follows it there.
#[test]
fn a_wait_ends_after_a_prepare_behind_a_preset_read() {
    let rig = Rig::new("prepare-wait");
    let slot = InsertSlot::effect(1, 0);
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread has no work. So no other thread reads the variable now.
    unsafe { std::env::set_var(rustel_vst3_fixture::SLOW_ENV, "300") };
    // The preset folder is new, so the prepare waits for a read. The load
    // takes 300 ms, so the read of the change starts before the load ends.
    rig.host.set_preset_folder(rig.root.join("presets"));
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, slot);
    rig.host.set_preset_folders(Vec::new());
    rig.host.wait_idle();
    // SAFETY: as above. The load read the variable before the wait ended.
    unsafe { std::env::remove_var(rustel_vst3_fixture::SLOW_ENV) };
    let Resolved::Ready(plugin, _) = rig.host.resolve(NAME, false) else {
        panic!("the fixture plugin did not load");
    };
    let key = InsertKey { plugin, preset: 0 };
    let prepared = rig.host.prepared(NAME, None, false, SAMPLE_RATE, slot);
    assert_eq!(prepared, Prepared::Ready(key));
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
    rig.host
        .keep(1, wanted.to_vec(), vec![1], Vec::new(), Vec::new());
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
    // Each copy goes home at its drop, and waits for the slot again.
    for ((name, instrument), key) in requests.into_iter().zip(keys) {
        until(|| (prepared(name, instrument) == Prepared::Ready(key)).then_some(()));
    }
    assert_eq!(running(), 2);

    // The chain drops the effect: its parked copy ends, and a prepare of
    // the effect builds nothing.
    for (name, instrument) in requests {
        rig.host.prepare(name, None, instrument, SAMPLE_RATE, slot);
    }
    assert_eq!(running(), 2);
    rig.host
        .keep(1, wanted[1..].to_vec(), vec![1], Vec::new(), Vec::new());
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
    rig.host
        .keep(1, vec![wanted.clone()], vec![1], Vec::new(), Vec::new());
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, slot);
    rig.host.wait_idle();
    let copy = || rig.host.prepared(NAME, None, false, SAMPLE_RATE, slot);
    assert_eq!(copy(), Prepared::Ready(key));
    let Resolved::Failed(error) = rig.host.resolve("broken", true) else {
        panic!("a text file loaded as a plugin");
    };
    rig.host
        .keep(1, vec![wanted], vec![1], Vec::new(), Vec::new());
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

    // A copy of the tone with a preset of another class does not start, with
    // one line. Then the tone does not start at a new load of its bundle. Its
    // copy gets no new try: the copy has no key, no load runs, no build
    // starts and no line comes.
    let folder = rig.root.join("presets").join(TONE_NAME);
    std::fs::create_dir_all(&folder).expect("preset folder");
    let other = rustel_vst3_fixture::preset(0.5, 0.0);
    std::fs::write(folder.join("Other.vstpreset"), other).expect("preset file");
    rig.host.set_preset_folders(Vec::new());
    rig.host.wait_idle();
    let tone_slot = InsertSlot::instrument(2);
    let tone = || {
        let preset = Some("Other");
        rig.host
            .prepared(TONE_NAME, preset, true, SAMPLE_RATE, tone_slot)
    };
    rig.host
        .prepare(TONE_NAME, Some("Other"), true, SAMPLE_RATE, tone_slot);
    rig.host.wait_idle();
    let reason = format!("{TONE_NAME} did not start: the preset is for a different plugin");
    assert_eq!(rig.host.take_errors(), [format!("vsti skipped: {reason}")]);
    assert_eq!(tone(), Prepared::Unavailable(reason));
    let tone_row = |rig: &Rig| {
        let rows = rig.host.plugins().into_iter();
        let mut rows = rows.filter(|row| row.name == TONE_NAME);
        rows.next().map(|row| row.status)
    };
    let tone_fault = |rig: &Rig| {
        let rows = rig.host.plugins().into_iter();
        let mut rows = rows.filter(|row| row.name == TONE_NAME);
        rows.next().and_then(|row| row.fault)
    };
    assert_eq!(
        tone_fault(&rig).as_deref(),
        Some("a copy did not start: the preset is for a different plugin")
    );
    rig.host.unload_unused(&[]);
    rig.host.wait_idle();
    assert_eq!(tone_row(&rig), Some(Status::Found), "the unload");
    // The failed build stays through the unload, and a row that is not
    // ready says no fault of a copy.
    assert_eq!(tone_fault(&rig), None);
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variable in the load below. No other thread reads a
    // variable now.
    unsafe { std::env::set_var(BROKEN_TONE_ENV, "1") };
    let Resolved::Failed(fault) = rig.host.resolve(TONE_NAME, true) else {
        panic!("the tone started");
    };
    // SAFETY: as above. The load read the variable.
    unsafe { std::env::remove_var(BROKEN_TONE_ENV) };
    assert_eq!(tone_row(&rig), Some(Status::Failed(fault.clone())));
    let copy = WantedCopy {
        slot: tone_slot,
        name: TONE_NAME.to_owned(),
        preset: Some("Other".into()),
    };
    assert_eq!(rig.host.retry(&[copy], SAMPLE_RATE), 0);
    assert!(rig.host.loads().is_empty());
    let reason = format!("{TONE_NAME} did not load: {fault}");
    assert_eq!(tone(), Prepared::Unavailable(reason));
    assert!(rig.host.take_errors().is_empty());

    // A new scan gives the bundle a new try. The tone keeps its fault: the
    // cache has it with the stamp of its bundle.
    rig.scan();
    assert_eq!(status(&rig), Status::Found);
    assert_eq!(tone_row(&rig), Some(Status::Failed(fault)));
}

fn walk(folder: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder)
        .expect("the folder is listed")
        .flatten()
    {
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
/// second caller changes no chain of the first. A copy an engine holds ends
/// when it comes home, for a chain that ended or a key its chain dropped. A
/// copy whose chain ends during its park ends at the end of the park. A take
/// of another slot during a park keeps the copy on its way home. A chain at
/// rest keeps its copies as a live chain does. The copies end a few at a
/// time, so a build does not wait for each end.
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
    rig.host
        .keep(1, Vec::new(), Vec::new(), Vec::new(), vec![8]);
    assert_eq!(running(), 1);
    rig.host
        .keep(1, Vec::new(), Vec::new(), Vec::new(), vec![9]);
    assert_eq!(running(), 0);
    assert_eq!(prepared(), Prepared::Unbuilt(key));

    // A chain that ends with its build in the line keeps no copy, also
    // when the build is done first. No later call builds a copy for the
    // chain: a build with this preset fails, and `Host::take_errors` gives
    // nothing. No chain number comes a second time.
    let (stays, ends) = (InsertSlot::effect(10, 0), InsertSlot::effect(11, 0));
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, stays);
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, ends);
    rig.host
        .keep(1, Vec::new(), Vec::new(), Vec::new(), vec![11]);
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
    rig.host.keep(
        1,
        vec![wanted(live, Some("Half"))],
        vec![12],
        Vec::new(),
        Vec::new(),
    );
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
    rig.host.keep(1, both, vec![12], Vec::new(), Vec::new());
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
    rig.host.keep(1, one, vec![12], Vec::new(), Vec::new());
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
    rig.host.keep(
        2,
        vec![wanted(other, None)],
        vec![13],
        Vec::new(),
        Vec::new(),
    );
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
    rig.host
        .keep(1, copies, vec![12, 15, 16], Vec::new(), Vec::new());
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, slow);
    // The end of a key the chain drops waits behind the slow build. A key
    // the chain wants again before that end keeps its copy.
    rig.host.keep(
        2,
        vec![wanted(other, Some("Half"))],
        vec![13],
        Vec::new(),
        Vec::new(),
    );
    rig.host.keep(
        2,
        vec![wanted(other, None)],
        vec![13],
        Vec::new(),
        Vec::new(),
    );
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
    rig.host
        .keep(1, Vec::new(), Vec::new(), Vec::new(), Vec::new());
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, live);
    assert_eq!(running(), 5);

    // A copy an engine holds plays on when its chain ends, and ends when it
    // comes home. So does a copy whose key its chain no longer wants.
    let provider = rig.host.provider(false);
    let ending = InsertSlot::effect(17, 0);
    let mut held = until(|| provider(key, SAMPLE_RATE, ending));
    let copies = running();
    rig.host
        .keep(1, Vec::new(), Vec::new(), Vec::new(), vec![17]);
    // The engine holds a copy of the chain that ended, and a name of its
    // plugin keeps the plugin.
    assert!(rig.host.engine_holds_chains(&[17]));
    assert!(!rig.host.engine_holds_chains(&[16, 18]));
    assert!(!rig.host.engine_holds_unkept(&["fixture".to_owned()]));
    assert_eq!(block(held.as_mut(), 1.0, 128), vec![1.0; 128]);
    drop(held);
    assert!(!rig.host.engine_holds_chains(&[17]));
    assert_eq!(running(), copies - 1);
    let dropped = InsertSlot::effect(18, 0);
    let held = until(|| provider(key, SAMPLE_RATE, dropped));
    let copies = running();
    rig.host.keep(
        1,
        vec![wanted(dropped, Some("Half"))],
        vec![18],
        Vec::new(),
        Vec::new(),
    );
    drop(held);
    assert_eq!(running(), copies - 1);

    // A copy that played a loud block turns its plugin off and on in its
    // park, 600 ms here. A take of another slot in that time keeps the copy
    // on its way home: its slot is not Unbuilt. Its chain ends in that time,
    // and the park ends the copy.
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variable in the build below. No other thread reads a
    // variable now.
    unsafe { std::env::set_var(SLOW_ACTIVATE_ENV, "300") };
    let parking = InsertSlot::effect(19, 0);
    let mut held = until(|| provider(key, SAMPLE_RATE, parking));
    // SAFETY: as above. The copy read the variable at its build.
    unsafe { std::env::remove_var(SLOW_ACTIVATE_ENV) };
    let next = InsertSlot::effect(20, 0);
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, next);
    let copies = running();
    assert_eq!(block(held.as_mut(), 1.0, 128), vec![1.0; 128]);
    drop(held);
    let _next = provider(key, SAMPLE_RATE, next).expect("the copy of the next slot");
    let home = rig.host.prepared(NAME, None, false, SAMPLE_RATE, parking);
    assert!(
        matches!(home, Prepared::Returning | Prepared::Ready(_)),
        "{home:?}"
    );
    std::thread::sleep(Duration::from_millis(100));
    rig.host
        .keep(1, Vec::new(), Vec::new(), Vec::new(), vec![19]);
    assert_eq!(running(), copies - 1);

    // A chain at rest keeps its parked copy, and a key it drops ends, as for
    // a named chain.
    let resting = InsertSlot::effect(21, 0);
    rig.host.prepare(NAME, None, false, SAMPLE_RATE, resting);
    let copies = running();
    assert_eq!(ready(resting), Prepared::Ready(key));
    let rest = vec![(wanted(resting, None), 1)];
    rig.host.keep(1, Vec::new(), Vec::new(), rest, Vec::new());
    assert_eq!(running(), copies);
    assert_eq!(ready(resting), Prepared::Ready(key));
    let rest = vec![(wanted(resting, Some("Half")), 1)];
    rig.host.keep(1, Vec::new(), Vec::new(), rest, Vec::new());
    assert_eq!(running(), copies - 1);
    assert_eq!(ready(resting), Prepared::Unbuilt(key));

    // keep() ends 4 copies in a job and puts the others in a job at the end
    // of the line, with the copies of the calls that come meanwhile. A set
    // change ends the copies of each scene it closes in a call of its own,
    // as the 3 calls here do. So a build sent right after them waits for 4
    // ends at most. The fixture marks each copy it makes and ends. These
    // copies turn off in 10 ms each, so most ends come after the send of
    // the build.
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variable in the builds below. No other thread reads
    // a variable now.
    unsafe { std::env::set_var(SLOW_ACTIVATE_ENV, "10") };
    let chains: Vec<u64> = (22..42).collect();
    for chain in &chains {
        let slot = InsertSlot::effect(*chain, 0);
        rig.host.prepare(NAME, None, false, SAMPLE_RATE, slot);
    }
    let copies = running();
    // SAFETY: as above. The copies read the variable at their builds.
    unsafe { std::env::remove_var(SLOW_ACTIVATE_ENV) };
    let marks = rig.root.join("marks");
    // SAFETY: as above. The plugin thread reads the variable at the ends and
    // at the build below.
    unsafe { std::env::set_var(THREAD_MARK_ENV, &marks) };
    for ended in [&chains[..12], &chains[12..16], &chains[16..]] {
        rig.host
            .keep(1, Vec::new(), Vec::new(), Vec::new(), ended.to_vec());
    }
    rig.host
        .prepare(NAME, None, false, SAMPLE_RATE, InsertSlot::effect(42, 0));
    // Each end before the build joined the line is in the file now, and
    // the file comes with the first end. A slow test thread finds more ends
    // here. The build waits for 4 more at most.
    let sent = std::fs::read_to_string(&marks).unwrap_or_default();
    let sent = sent.lines().filter(|line| line.starts_with("end ")).count();
    until(|| (running() == copies - 19).then_some(()));
    // A copy leaves the count as its end starts: the wait sees the last end
    // through.
    rig.host.wait_idle();
    // SAFETY: as above.
    unsafe { std::env::remove_var(THREAD_MARK_ENV) };
    let marks = std::fs::read_to_string(marks).expect("the mark file");
    let lines: Vec<&str> = marks.lines().collect();
    let ends = lines.iter().filter(|line| line.starts_with("end ")).count();
    assert_eq!(ends, 20, "{marks}");
    let made = lines.iter().position(|line| line.starts_with("make "));
    let made = made.expect("the build");
    let before = lines[..made].iter().filter(|line| line.starts_with("end "));
    assert!(before.count() <= sent + 4, "{marks}");

    // A parked render copy takes a place of the room of a render, and a
    // copy with no park ends in its job and takes none. keep() ends the
    // render chain and its copy, and the chain takes no new build.
    let room = rig.host.render_room();
    let render = InsertSlot::effect(50, 0);
    let parks = rig.host.prepare_render(key, SAMPLE_RATE, render, &[], true);
    assert_eq!(parks.expect("the render build").recv(), Ok(Ok(0)));
    assert_eq!(rig.host.render_room(), room - 1);
    let ends = InsertSlot::effect(51, 0);
    let ends = rig.host.prepare_render(key, SAMPLE_RATE, ends, &[], false);
    assert_eq!(ends.expect("the render build").recv(), Ok(Ok(0)));
    assert_eq!(rig.host.render_room(), room - 1);
    let copies = running();
    rig.host
        .keep(2, Vec::new(), Vec::new(), Vec::new(), vec![50]);
    assert_eq!(running(), copies - 1);
    assert_eq!(rig.host.render_room(), room);
    let again = rig.host.prepare_render(key, SAMPLE_RATE, render, &[], true);
    assert!(again.is_err(), "a chain that ended");
    assert_eq!(running(), copies - 1);
    // The failed build of a render stays to the end of its render, also
    // through a scan, and the failed build of a live chain goes.
    let Ok(Some((refused, _))) = rig.host.key(NAME, fails, false) else {
        panic!("the preset of the other plugin has no key");
    };
    let (render, live) = (InsertSlot::effect(52, 0), InsertSlot::effect(53, 0));
    let failed = |slot| rig.host.prepared(NAME, fails, false, SAMPLE_RATE, slot);
    let read = rig
        .host
        .prepare_render(refused, SAMPLE_RATE, render, &[], true);
    let Ok(Err(reason)) = read.expect("the render build").recv() else {
        panic!("the render build did not fail");
    };
    rig.host.prepare(NAME, fails, false, SAMPLE_RATE, live);
    rig.host.wait_idle();
    assert!(matches!(failed(live), Prepared::Unavailable(_)));
    rig.scan();
    assert_eq!(failed(render), Prepared::Unavailable(reason));
    assert_eq!(failed(live), Prepared::Unbuilt(refused));
    // The end of the render chain ends its failed build and its lines.
    rig.host
        .keep(2, Vec::new(), Vec::new(), Vec::new(), vec![52]);
    assert_eq!(failed(render), Prepared::Unbuilt(refused));
    assert!(rig.host.take_render_errors(&[52]).is_empty());
}

/// A preset of another plugin makes no plugin. The copies of a key that
/// did not start get one new try at a time. The first try runs at once. A
/// try that fails gives the key a wait: the time of the try, then twice the
/// wait before. A read of the preset folders that finds the file gone or
/// saved again ends the wait. A try that builds gives the other copies of
/// its key their tries at once. A render chain gets no try, and keeps its
/// failed build to the end of its render.
#[test]
fn a_copy_that_did_not_start_gets_one_try_at_a_time() {
    let rig = Rig::new("retry");
    let mut other = rustel_vst3_fixture::preset(0.5, 0.0);
    other[8] = b'0';
    rig.write_preset("Other Plugin.vstpreset", &other);
    // The effect refuses a state of 8 bytes.
    let refused_state = rustel_vst3_fixture::preset_file(&[0.5]);
    rig.write_preset("Refused.vstpreset", &refused_state);
    rig.plugin();
    let running = || {
        rig.host.wait_idle();
        rig.host.plugins()[0].running
    };

    // The fixture marks each plugin it makes, and the build of a preset of
    // another plugin makes none.
    let marks = rig.root.join("marks");
    // SAFETY: the tests of this binary run one at a time, and the plugin
    // thread reads the variable in the build below. No other thread reads a
    // variable now.
    unsafe { std::env::set_var(THREAD_MARK_ENV, &marks) };
    let other = Some("Other Plugin");
    rig.host
        .prepare(NAME, other, false, SAMPLE_RATE, InsertSlot::effect(1, 0));
    rig.host.wait_idle();
    // SAFETY: as above.
    unsafe { std::env::remove_var(THREAD_MARK_ENV) };
    let line = format!("vst skipped: {NAME} did not start: the preset is for a different plugin");
    assert_eq!(rig.host.take_errors(), [line]);
    let marks = std::fs::read_to_string(&marks).unwrap_or_default();
    assert!(
        !marks.lines().any(|line| line.starts_with("make ")),
        "{marks}"
    );

    // 3 copies of the refused preset do not start, and neither does a copy
    // of a render chain. Each start takes 300 ms or more.
    // SAFETY: as above. The plugin thread reads the variable in the builds
    // below.
    unsafe { std::env::set_var(SLOW_CLASS_ENV, "300") };
    let preset = Some("Refused");
    let slots = [2, 3, 4].map(|chain| InsertSlot::effect(chain, 0));
    for slot in slots {
        rig.host.prepare(NAME, preset, false, SAMPLE_RATE, slot);
    }
    let Ok(Some((key, _))) = rig.host.key(NAME, preset, false) else {
        panic!("the refused preset has no key");
    };
    let render = InsertSlot::effect(5, 0);
    let read = rig.host.prepare_render(key, SAMPLE_RATE, render, &[], true);
    let read = read.expect("the render build");
    rig.host.wait_idle();
    let reason = format!("{NAME} did not start: the plugin refused the preset");
    let refused = format!("vst skipped: {reason}");
    assert_eq!(rig.host.take_errors(), vec![refused.clone(); 3]);
    assert_eq!(read.recv(), Ok(Err(reason.clone())));
    let render_lines = rig.host.take_render_errors(&[render.chain]);
    assert_eq!(render_lines, std::slice::from_ref(&refused));

    // The first try runs at once, and a key has one try at a time. The
    // other copies of the key wait for the try, and the render chain gets
    // none.
    let copies = slots.map(|slot| WantedCopy {
        slot,
        name: NAME.to_owned(),
        preset: preset.map(str::to_owned),
    });
    let retry = || rig.host.retry(&copies, SAMPLE_RATE);
    let prepared = |slot| rig.host.prepared(NAME, preset, false, SAMPLE_RATE, slot);
    // Each try fails with one line.
    let tried = || {
        rig.host.wait_idle();
        assert_eq!(rig.host.take_errors(), std::slice::from_ref(&refused));
    };
    let start = Instant::now();
    assert_eq!(retry(), 1);
    for slot in slots {
        assert_eq!(prepared(slot), Prepared::Retrying);
    }
    assert_eq!(prepared(render), Prepared::Unavailable(reason.clone()));
    // The row of the plugin says the try.
    let row_fault = || rig.host.plugins()[0].fault.clone();
    assert_eq!(
        row_fault().as_deref(),
        Some("a copy did not start: the plugin refused the preset · a new try runs")
    );
    tried();
    let first = start.elapsed();
    // The wait is the time of the try.
    assert_eq!(retry(), 0);
    std::thread::sleep(first + Duration::from_millis(200));
    let start = Instant::now();
    assert_eq!(retry(), 1);
    tried();
    let second = start.elapsed();
    // The wait is twice the wait before now: 600 ms or more.
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(retry(), 0);

    // A read of the folders that finds the file gone ends the wait, and
    // takes the number of the preset: the copies have no key. The file
    // named back has its number again, and its next try fails with a first
    // wait again.
    let folder = rig.root.join("presets").join(NAME);
    let (file, moved) = (
        folder.join("Refused.vstpreset"),
        folder.join("Moved.vstpreset"),
    );
    let read_folders = || {
        rig.host.set_preset_folders(Vec::new());
        rig.host.wait_idle();
    };
    std::fs::rename(&file, &moved).expect("the move of the preset file");
    read_folders();
    std::thread::sleep(2 * first.max(second) + Duration::from_millis(200));
    assert_eq!(retry(), 0, "a copy with no key");
    let missing = rig.host.key(NAME, preset, false).err();
    assert!(
        missing
            .as_ref()
            .is_some_and(|reason| reason.contains("has no preset with the name")),
        "{missing:?}"
    );
    std::fs::rename(&moved, &file).expect("the move of the preset file");
    read_folders();
    let found = rig.host.key(NAME, preset, false);
    assert_eq!(found.map(|found| found.map(|(key, _)| key)), Ok(Some(key)));
    let start = Instant::now();
    assert_eq!(retry(), 1);
    tried();
    let third = start.elapsed();
    assert_eq!(retry(), 0);
    std::thread::sleep(third + Duration::from_millis(200));
    assert_eq!(retry(), 1, "a first wait");
    tried();
    assert_eq!(retry(), 0, "a wait of 600 ms or more");

    // A save of the file ends that wait at the next read: the try runs at
    // once. It builds, and the other copies of the key try right behind it.
    // No copy of a live chain reads as unbuilt or failed meanwhile.
    rig.write_preset("Refused.vstpreset", &rustel_vst3_fixture::preset(0.5, 0.0));
    read_folders();
    let copies_before = running();
    assert_eq!(retry(), 1);
    until(|| {
        let answers = slots.map(prepared);
        for answer in &answers[1..] {
            let tries = matches!(answer, Prepared::Retrying | Prepared::Ready(_));
            assert!(tries, "{answer:?}");
        }
        let ready = answers.iter().all(|answer| *answer == Prepared::Ready(key));
        ready.then_some(())
    });
    assert!(rig.host.take_errors().is_empty());
    assert_eq!(running(), copies_before + 3);
    // SAFETY: as above. The builds read the variable.
    unsafe { std::env::remove_var(SLOW_CLASS_ENV) };

    // The render chain keeps its failed build to the end of its render, and
    // the row says no fault for it. The row says the copy with the preset of
    // another plugin until its chain ends.
    assert_eq!(
        row_fault().as_deref(),
        Some("a copy did not start: the preset is for a different plugin")
    );
    rig.host
        .keep(1, Vec::new(), Vec::new(), Vec::new(), vec![1]);
    assert_eq!(row_fault(), None);
    assert!(
        rig.host
            .insert_render(key, SAMPLE_RATE, render, None)
            .is_none()
    );
    rig.host
        .keep(1, Vec::new(), Vec::new(), Vec::new(), vec![render.chain]);
    assert_eq!(prepared(render), Prepared::Unbuilt(key));
}
