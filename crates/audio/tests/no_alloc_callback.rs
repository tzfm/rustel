//! Tripwire proving the ring/POD callback path does not allocate.
//!
//! Nothing crossing into the callback may allocate, free, or lock.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use rustel_audio::tripwire::{self, TripwireAlloc, Violations};
use rustel_audio::{
    AudioBackend, AudioEvent, BUNDLED_BD_SAMPLE_ID, DecodedSample, Envelope, FilterControls,
    FilterEnvelope, FilterStages, FmControls, FmOperator, FmRoute, FmWave, LiveFlipAtomics,
    LiveScalarBackend, MAX_FM_OPERATORS, MAX_FM_ROUTES, MAX_LIVE_VOICES, OnsetEvent,
    OscillatorControls, Ring, SampleControls, SampleHold, SampleId, ScalarBackend, StaticBiquad,
    Waveform, WavetableControls,
};

#[global_allocator]
static A: TripwireAlloc = TripwireAlloc;

const BLOCK_FRAMES: u64 = 128;
static TRIPWIRE_TEST: Mutex<()> = Mutex::new(());

fn serial_tripwire_test() -> MutexGuard<'static, ()> {
    TRIPWIRE_TEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct AudioState {
    frames: AtomicU64,
    held: Option<AudioEvent>,
    consumed: Vec<u64>,
}

fn callback(st: &mut AudioState, ring: &Ring, generation: u64, stopped: &AtomicBool) {
    tripwire::audio_scope(|| {
        if stopped.load(Ordering::Acquire) {
            st.frames.fetch_add(BLOCK_FRAMES, Ordering::Release);
            return;
        }
        let start = st.frames.load(Ordering::Relaxed);
        let end = start + BLOCK_FRAMES;
        while let Some(e) = st.held.take().or_else(|| ring.pop()) {
            if e.generation != generation {
                continue;
            }
            if e.target_frame >= end {
                st.held = Some(e);
                break;
            }
            // Prefill capacity so push cannot allocate inside the callback.
            st.consumed.push(e.onset_id);
        }
        st.frames.store(end, Ordering::Release);
    });
}

#[test]
fn ring_callback_path_has_no_allocation_or_free() {
    let _serial = serial_tripwire_test();
    assert!(!std::mem::needs_drop::<AudioEvent>());

    let ring = Ring::new(64);
    for i in 0..16u64 {
        assert!(ring.push(AudioEvent {
            onset_id: i,
            generation: 1,
            target_frame: i * 32,
            onset_lead: 0.0,
            freq_hz: 440.0,
            gain: 0.5,
            duration_secs: 0.01,
            ui_visuals: 0,
            controls: Default::default(),
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }));
    }

    let mut st = AudioState {
        frames: AtomicU64::new(0),
        held: None,
        consumed: Vec::with_capacity(32),
    };
    let stopped = AtomicBool::new(false);

    let before = Violations::capture();
    for _ in 0..8 {
        callback(&mut st, &ring, 1, &stopped);
    }
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "audio callback violated no-alloc contract: {delta:?}"
    );
    assert!(!st.consumed.is_empty(), "expected to consume POD onsets");
}

#[test]
fn negative_control_detects_allocation_inside_callback() {
    let _serial = serial_tripwire_test();
    let entries = tripwire::scope_entries();
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        #[allow(clippy::useless_vec)]
        let _leak = vec![1u8, 2, 3, 4];
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.allocs > 0,
        "negative control must observe allocation; got {delta:?}"
    );
    assert_eq!(
        tripwire::scope_entries() - entries,
        1,
        "the callback scope was not entered exactly once"
    );
}

#[test]
fn installed_allocator_passes_the_callback_arm_check() {
    let _serial = serial_tripwire_test();
    assert!(
        tripwire::allocator_is_armed(),
        "the allocation tripwire accepted a binary without TripwireAlloc"
    );
}

#[test]
fn reverb_installation_adopts_dispatch_without_callback_allocation() {
    let _serial = serial_tripwire_test();
    let mut backend =
        ScalarBackend::prepared_with_dispatch(48_000, 1, rustel_audio::DspDispatch::portable())
            .expect("portable backend");
    let params = rustel_audio::reverb::ReverbParams {
        ir: None,
        size_secs: 0.2,
        fade_secs: 0.01,
        lp_start_hz: 15_000.0,
        lp_end_hz: 1_000.0,
    };
    let first = Box::new(rustel_audio::reverb::OrbitReverb::generate(48_000, params));
    let second = Box::new(rustel_audio::reverb::OrbitReverb::generate(48_000, params));
    let before = Violations::capture();
    let (empty, replaced) = tripwire::audio_scope(|| {
        (
            backend.install_reverb(0, first),
            backend.install_reverb(0, second),
        )
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "reverb installation allocated or freed: {delta:?}"
    );
    assert!(empty.is_none());
    assert!(
        replaced
            .expect("installed reverb")
            .dispatch()
            .is_forced_portable()
    );
}

/// A gain as an orbit insert. Parameter 1 is the gain.
struct GainInsert {
    key: rustel_audio::InsertKey,
    gain: f32,
}

impl rustel_audio::OrbitInsert for GainInsert {
    fn key(&self) -> rustel_audio::InsertKey {
        self.key
    }

    fn set_param(&mut self, param: rustel_audio::InsertParam, _frames: u32) {
        if param.id == 1 {
            self.gain = param.value;
        }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for sample in left.iter_mut().chain(right) {
            *sample *= self.gain;
        }
    }
}

#[test]
fn an_orbit_insert_installs_and_processes_without_callback_allocation() {
    let _serial = serial_tripwire_test();
    let key = rustel_audio::InsertKey {
        plugin: 7,
        preset: 0,
    };
    let mut backend = ScalarBackend::prepared(48_000, 1).expect("scalar init");
    let mut insert = rustel_audio::InsertControls::new(key);
    assert!(insert.push(rustel_audio::InsertParam { id: 1, value: 0.0 }));
    let controls = OscillatorControls {
        effects: [Some(insert), None, None, None],
        ..OscillatorControls::default()
    };
    assert!(
        backend.try_note_prepared(OnsetEvent::new(0, 440.0, 0.5, 0.05).with_controls(controls))
    );
    let first = Box::new(GainInsert { key, gain: 1.0 });
    let second = Box::new(GainInsert { key, gain: 1.0 });
    let mut output = [1.0f32; 128 * 2];

    let before = Violations::capture();
    let (empty, replaced) = tripwire::audio_scope(|| {
        let empty = backend.install_insert(1, first);
        let replaced = backend.install_insert(1, second);
        backend.process_block(&mut output, 128);
        (empty, replaced)
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "the orbit insert allocated or freed in the callback: {delta:?}"
    );
    assert!(empty.is_none());
    assert!(replaced.is_some());
    // The note set the gain to zero, so the orbit is silent.
    assert!(output.iter().all(|sample| *sample == 0.0));
    assert_eq!(backend.missing_insert_events(), 0);
}

fn fx_reverb_test_controls(params: rustel_audio::reverb::ReverbParams) -> OscillatorControls {
    let mut controls = OscillatorControls {
        limit: None,
        envelope: Envelope {
            attack_secs: 0.0,
            decay_secs: 0.0,
            sustain: 1.0,
            release_secs: 0.0,
        },
        ..OscillatorControls::default()
    };
    controls.fx_stages[0] = Some(rustel_audio::FxStage {
        stretch: None,
        transient: None,
        gain: 1.0,
        filters: FilterControls::default(),
        vowel: None,
        coarse: None,
        crush: None,
        shape: None,
        distort: None,
        tremolo: None,
        compressor: None,
        pan_x: None,
        phaser: None,
        delay: None,
        dry: 0.0,
        room: Some(rustel_audio::ReverbControls {
            wet: 1.0,
            size_secs: params.size_secs,
            fade_secs: params.fade_secs,
            lp_start_hz: params.lp_start_hz,
            lp_end_hz: params.lp_end_hz,
            ir: params.ir,
        }),
    });
    controls
}

#[test]
fn degenerate_reverbs_retain_the_accounted_return_slot_minimum() {
    let source = DecodedSample::from_parts(48_000, 1, vec![1.0]).expect("one-frame IR");
    let minimum = 2
        * 4
        * (2 * rustel_audio::reverb::REVERB_BLOCK)
        * std::mem::size_of::<rustfft::num_complex::Complex<f32>>();
    for sample_rate in [0, 1, 48_000] {
        for size_secs in [0.0, f32::NAN] {
            let params = rustel_audio::reverb::ReverbParams {
                ir: None,
                size_secs,
                fade_secs: 0.0,
                lp_start_hz: 0.0,
                lp_end_hz: 0.0,
            };
            for reverb in [
                rustel_audio::reverb::OrbitReverb::generate(sample_rate, params),
                rustel_audio::reverb::OrbitReverb::generate_streaming(sample_rate, params),
                rustel_audio::reverb::OrbitReverb::generate_custom(sample_rate, params, &source),
            ] {
                assert!(reverb.approx_bytes() >= minimum);
            }
        }
    }
}

#[test]
fn fx_reverb_delivery_and_retirement_have_no_callback_allocation() {
    let _serial = serial_tripwire_test();
    let mut backend = ScalarBackend::prepared(48_000, 1).expect("prepared backend");
    backend.forbid_inline_reverb();
    let params = rustel_audio::reverb::ReverbParams {
        ir: None,
        size_secs: 0.001,
        fade_secs: 0.0,
        lp_start_hz: 0.0,
        lp_end_hz: 0.0,
    };
    let controls = fx_reverb_test_controls(params);
    let mut prepared = (0..33)
        .map(|_| {
            Box::new(rustel_audio::reverb::OrbitReverb::generate_streaming(
                48_000, params,
            ))
        })
        .collect::<Vec<_>>()
        .into_iter();
    let mut output = [0.0f32; 128 * 2];
    let mut heard = false;

    let before = Violations::capture();
    tripwire::audio_scope(|| {
        assert!(
            backend
                .install_fx_reverb(prepared.next().unwrap())
                .is_none()
        );
        assert!(
            backend.try_note_prepared(OnsetEvent::new(0, 440.0, 0.5, 1.0).with_controls(controls))
        );
        backend.process_block(&mut output, 128);
        heard |= output.iter().any(|sample| sample.abs() > 1e-6);

        // Delivery while one box is leased must leave room for its return,
        // not merely for the boxes currently in the free vector.
        for reverb in prepared.by_ref() {
            assert!(backend.install_fx_reverb(reverb).is_none());
        }
        backend.reset_at(0);
        assert!(
            backend
                .try_note_prepared(OnsetEvent::new(0, 440.0, 0.5, 0.001).with_controls(controls))
        );
        for _ in 0..8 {
            backend.process_block(&mut output, 128);
        }
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "FX reverb delivery or return allocated or freed: {delta:?}"
    );
    assert!(heard, "the prepared stage reverb was not heard");
    assert_eq!(backend.missing_reverb_events(), 0);
}

#[test]
fn fx_reverb_eviction_preserves_a_leased_room_without_callback_allocation() {
    let _serial = serial_tripwire_test();
    let mut backend = ScalarBackend::prepared(48_000, 1).expect("prepared backend");
    backend.forbid_inline_reverb();
    let params = |lp_end_hz| rustel_audio::reverb::ReverbParams {
        ir: None,
        size_secs: 6.0,
        fade_secs: 0.1,
        lp_start_hz: 15_000.0,
        lp_end_hz,
    };
    let room = |lp_end_hz| {
        Box::new(rustel_audio::reverb::OrbitReverb::generate_streaming(
            48_000,
            params(lp_end_hz),
        ))
    };
    let leased = room(1_000.0);
    let bytes = leased.approx_bytes();
    assert!(2 * bytes <= 64 * 1024 * 1024 && 3 * bytes > 64 * 1024 * 1024);
    let idle = room(2_000.0);
    let idle_pointer = std::ptr::from_ref(idle.as_ref());
    let incoming = room(3_000.0);
    let incoming_pointer = std::ptr::from_ref(incoming.as_ref());
    assert!(backend.install_fx_reverb(leased).is_none());
    assert!(backend.install_fx_reverb(idle).is_none());
    let controls = fx_reverb_test_controls(params(1_000.0));
    let mut output = [0.0; 256];
    let mut retired = None;

    let before = Violations::capture();
    let (refused, accepted) = tripwire::audio_scope(|| {
        assert!(
            backend.try_note_prepared(OnsetEvent::new(0, 440.0, 0.5, 1.0).with_controls(controls))
        );
        backend.process_block(&mut output, 128);
        let refused = backend
            .install_fx_reverb_evicting(incoming, 0, |_| {
                panic!("no return slot was reserved for an eviction")
            })
            .expect("no retirement slots");
        let refused_pointer = std::ptr::from_ref(refused.as_ref());
        let accepted = backend.install_fx_reverb_evicting(refused, 1, |room| {
            assert!(retired.is_none());
            retired = Some(room);
        });
        backend.reset_at(0);
        assert!(
            backend.try_note_prepared(OnsetEvent::new(0, 440.0, 0.5, 1.0).with_controls(controls))
        );
        backend.process_block(&mut output, 128);
        (refused_pointer, accepted)
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "FX reverb eviction allocated or freed: {delta:?}"
    );
    assert_eq!(refused, incoming_pointer);
    assert!(accepted.is_none());
    assert_eq!(
        std::ptr::from_ref(retired.as_ref().expect("idle room retired").as_ref()),
        idle_pointer
    );
    assert_eq!(
        backend.missing_reverb_events(),
        0,
        "the leased room returns to the pool"
    );
}

#[test]
fn an_oversized_fx_reverb_is_refused_without_eviction_or_callback_allocation() {
    let _serial = serial_tripwire_test();
    let mut backend = ScalarBackend::prepared(48_000, 1).expect("prepared backend");
    let params = rustel_audio::reverb::ReverbParams {
        ir: None,
        size_secs: 6.0,
        fade_secs: 0.1,
        lp_start_hz: 15_000.0,
        lp_end_hz: 1_000.0,
    };
    let idle = Box::new(rustel_audio::reverb::OrbitReverb::generate_streaming(
        48_000, params,
    ));
    assert!(backend.install_fx_reverb(idle).is_none());
    let oversized = Box::new(rustel_audio::reverb::OrbitReverb::generate_streaming(
        192_000, params,
    ));
    assert!(oversized.approx_bytes() > 64 * 1024 * 1024);
    let pointer = std::ptr::from_ref(oversized.as_ref());

    let before = Violations::capture();
    let refused = tripwire::audio_scope(|| {
        backend.install_fx_reverb_evicting(oversized, usize::MAX, |_| {
            panic!("an oversized room must not evict an admitted room")
        })
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "oversized FX reverb allocated or freed: {delta:?}"
    );
    assert_eq!(
        std::ptr::from_ref(refused.as_ref().expect("oversized room refused").as_ref()),
        pointer
    );
}

#[test]
fn inline_fx_reverb_returns_remain_prepared_after_reinitialization() {
    let _serial = serial_tripwire_test();
    let mut backend = ScalarBackend::prepared(48_000, 1).expect("prepared backend");
    let controls = fx_reverb_test_controls(rustel_audio::reverb::ReverbParams {
        ir: None,
        size_secs: 0.001,
        fade_secs: 0.0,
        lp_start_hz: 0.0,
        lp_end_hz: 0.0,
    });
    let mut output = [0.0f32; 128 * 2];
    // Offline generation may allocate, but its later return must not.
    backend.note(OnsetEvent::new(0, 440.0, 0.5, 1.0).with_controls(controls));
    backend.process_block(&mut output, 128);
    let before = Violations::capture();
    tripwire::audio_scope(|| backend.reset_at(0));
    assert!(Violations::capture().since(before).clean());

    backend.init(48_000).expect("reinitialize backend");
    backend.forbid_inline_reverb();
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        assert!(
            backend
                .try_note_prepared(OnsetEvent::new(0, 440.0, 0.5, 0.001).with_controls(controls))
        );
        for _ in 0..8 {
            backend.process_block(&mut output, 128);
        }
        backend.reset_at(0);
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "inline FX reverb return allocated or freed: {delta:?}"
    );
    assert_eq!(backend.missing_reverb_events(), 0);
}

#[test]
fn wavetable_unison_rendering_has_no_allocation_or_free() {
    let _serial = serial_tripwire_test();
    for voices in [1.0, 7.0, 8.0, 32.0] {
        let automatic = render_wavetable_unison(rustel_audio::DspDispatch::automatic(), voices);
        let portable = render_wavetable_unison(rustel_audio::DspDispatch::portable(), voices);
        assert_eq!(automatic.len(), portable.len());
        for (automatic, portable) in automatic.into_iter().zip(portable) {
            assert!(automatic.is_finite() && portable.is_finite());
            assert_eq!(automatic.to_bits(), portable.to_bits());
        }
    }
}

fn render_wavetable_unison(dispatch: rustel_audio::DspDispatch, voices: f32) -> Vec<f32> {
    const FRAME_LEN: usize = 64;
    const TABLE: SampleId = SampleId(41);

    let pcm = (0..FRAME_LEN * 2)
        .map(|index| (index as f32 * 0.17).sin())
        .collect();
    let decoded = DecodedSample::from_parts(48_000, 1, pcm).expect("wavetable sample");
    let mut backend =
        ScalarBackend::prepared_with_dispatch(48_000, 1, dispatch).expect("prepared backend");
    backend
        .install_sample(TABLE, Box::new(decoded))
        .expect("wavetable installation");

    let mut event = OnsetEvent::new(0, 220.0, 1.0, 1.0);
    event.wavetable = Some(WavetableControls {
        table: TABLE,
        frame_len: FRAME_LEN as u32,
        voices,
        lfo_shape: 0,
        phaserand: 1.0,
        freqspread: 0.35,
        panspread: 0.8,
        position: 0.4,
        pos_env_amount: 0.0,
        pos_attack: 0.0,
        pos_decay: 0.5,
        pos_sustain: 0.0,
        pos_release: 0.1,
        lfo_depth: 0.0,
        lfo_rate: 1.0,
        lfo_skew: 0.5,
        lfo_dc: 0.0,
        warp: 0.0,
        warp_mode: 0,
        warp_env_amount: 0.0,
        warp_attack: 0.0,
        warp_decay: 0.5,
        warp_sustain: 0.0,
        warp_release: 0.1,
        warp_lfo_depth: 0.0,
        warp_lfo_rate: 1.0,
        warp_lfo_skew: 0.5,
        warp_lfo_dc: 0.0,
        warp_lfo_shape: 0,
    });
    assert!(backend.try_note_prepared(event));

    let mut output = vec![0.0f32; 16_384 * 2];
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        // Cross quantum and vector-lane boundaries repeatedly, retaining all
        // rendered samples for the automatic/portable comparison.
        let mut remaining = output.as_mut_slice();
        for frames in [1, 127, 129, 255, 65, 511, 1024, 4096].into_iter().cycle() {
            if remaining.is_empty() {
                break;
            }
            let samples = (frames * 2).min(remaining.len());
            let (block, rest) = remaining.split_at_mut(samples);
            backend.process_block(block, samples / 2);
            remaining = rest;
        }
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "wavetable callback rendering allocated or freed: {delta:?}"
    );
    assert!(output.iter().any(|sample| sample.abs() > 1e-6));
    output
}

#[test]
fn tripwire_deltas_survive_counter_wrap() {
    assert_eq!(
        (Violations {
            allocs: 1,
            frees: 2,
        })
        .since(Violations {
            allocs: u64::MAX,
            frees: u64::MAX,
        }),
        Violations {
            allocs: 2,
            frees: 3,
        }
    );
}

#[test]
fn nested_callback_scope_restores_the_outer_tripwire() {
    let _serial = serial_tripwire_test();
    let entries = tripwire::scope_entries();
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        tripwire::audio_scope(|| {});
        // The old boolean teardown cleared the OUTER scope here, so this
        // allocation escaped the tripwire. Restoring the previous state is
        // load-bearing for nested callback adapters.
        let allocation = vec![0u8; 8];
        std::hint::black_box(allocation);
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.allocs > 0 && delta.frees > 0,
        "nested teardown disabled the outer scope: {delta:?}"
    );
    assert_eq!(tripwire::scope_entries() - entries, 2);
}

#[test]
fn prepared_scalar_backend_processes_without_callback_allocation() {
    let _serial = serial_tripwire_test();
    let mut backend = ScalarBackend::new();
    backend.init(48_000).expect("scalar init");
    // Submission and voice-capacity reservation happen off the callback.
    for frame in [0, 32, 64, 96] {
        backend.note(OnsetEvent::new(frame, 440.0, 0.5, 0.1));
    }
    let mut output = [0.0f32; 128 * 2];

    let before = Violations::capture();
    tripwire::audio_scope(|| backend.process_block(&mut output, 128));
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "prepared scalar callback allocated or freed: {delta:?}"
    );
    assert!(output.iter().any(|sample| sample.abs() > 1e-6));
}

#[test]
fn pooled_fx_stage_state_activates_and_retires_without_callback_allocation() {
    let _serial = serial_tripwire_test();
    let mut backend = ScalarBackend::prepared(48_000, 1).expect("scalar init");
    let mut controls = OscillatorControls::default();
    controls.fx_stages[0] = Some(rustel_audio::FxStage {
        stretch: None,
        transient: None,
        gain: 1.0,
        filters: FilterControls::default(),
        vowel: None,
        coarse: None,
        crush: None,
        shape: None,
        distort: None,
        tremolo: None,
        compressor: None,
        pan_x: None,
        phaser: None,
        delay: None,
        dry: 1.0,
        room: None,
    });
    assert!(
        backend.try_note_prepared(OnsetEvent::new(0, 440.0, 0.5, 0.001).with_controls(controls))
    );
    let mut output = [0.0f32; 128 * 2];

    let before = Violations::capture();
    tripwire::audio_scope(|| {
        for _ in 0..8 {
            backend.process_block(&mut output, 128);
        }
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "pooled FX-stage activation or retirement allocated or freed: {delta:?}"
    );
}

#[test]
fn pooled_fm_state_activates_and_retires_without_callback_allocation() {
    let _serial = serial_tripwire_test();
    let mut operators = [None; MAX_FM_OPERATORS];
    operators[0] = Some(FmOperator {
        harmonicity: 2.0,
        waveform: FmWave::Sine,
        env: None,
        env_exponential: true,
    });
    let mut routes = [None; MAX_FM_ROUTES];
    routes[0] = Some(FmRoute {
        source: 1,
        target: 0,
        amount: 1.0,
        mod_slot: Some(0),
    });
    let controls = OscillatorControls {
        limit: None,
        envelope: Envelope {
            attack_secs: 0.0,
            decay_secs: 0.0,
            sustain: 1.0,
            release_secs: 0.0,
        },
        fm: Some(FmControls { operators, routes }),
        ..OscillatorControls::default()
    };
    let mut backend = ScalarBackend::prepared(48_000, 1).expect("scalar init");
    assert!(
        backend.try_note_prepared(OnsetEvent::new(0, 440.0, 0.5, 0.001).with_controls(controls))
    );
    let mut output = [0.0f32; 128 * 2];

    let before = Violations::capture();
    tripwire::audio_scope(|| {
        // The first block both activates and retires this short voice. A box
        // drop here would be a callback free just as surely as allocating a
        // replacement state would be a callback allocation.
        backend.process_block(&mut output, 128);
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "pooled FM activation or retirement allocated or freed: {delta:?}"
    );
    assert!(output.iter().any(|sample| sample.abs() > 1e-6));
}

#[test]
fn bundled_sample_is_preloaded_and_processed_without_callback_allocation() {
    let _serial = serial_tripwire_test();
    let mut backend = ScalarBackend::prepared(48_000, 1).expect("preload sample bank");
    let sample = SampleControls {
        sample: BUNDLED_BD_SAMPLE_ID,
        playback_rate: 1.25,
        begin: 0.1,
        end: 0.85,
        hold: SampleHold::Slice,
        muted: false,
        loop_secs: None,
        envelope_peak: 1.0,
        reversed: false,
        nudge_secs: 0.0,
        // Cut-group registration runs on the audio thread; the tripwire
        // proves the registry never grows there.
        cut: Some(1.0),
    };
    assert!(
        backend.try_note_prepared(
            OnsetEvent::new(0, 0.0, 0.7, 0.5)
                .with_controls(OscillatorControls {
                    limit: None,
                    noise: 0.0,
                    bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
                    bus: None,
                    busgain: 1.0,
                    channels: None,
                    envelope: Envelope {
                        attack_secs: 0.001,
                        decay_secs: 0.001,
                        sustain: 1.0,
                        release_secs: 0.01,
                    },
                    pan: Some(0.3),
                    ..OscillatorControls::default()
                })
                .with_sample(sample)
        )
    );
    let mut expected = [0.0f32; 128 * 2];
    let before = Violations::capture();
    tripwire::audio_scope(|| backend.process_block(&mut expected, 128));
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "bundled-sample callback allocated or freed: {delta:?}"
    );
    assert!(expected.iter().any(|sample| sample.abs() > 1e-6));

    let ring = Ring::new(2);
    assert!(ring.push(AudioEvent {
        onset_id: 1,
        generation: 1,
        target_frame: 0,
        onset_lead: 0.0,
        freq_hz: 0.0,
        gain: 0.7,
        duration_secs: 0.5,
        ui_visuals: 0,
        controls: OscillatorControls {
            limit: None,
            noise: 0.0,
            bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
            bus: None,
            busgain: 1.0,
            channels: None,
            envelope: Envelope {
                attack_secs: 0.001,
                decay_secs: 0.001,
                sustain: 1.0,
                release_secs: 0.01,
            },
            pan: Some(0.3),
            ..OscillatorControls::default()
        },
        sample: Some(sample),
        wavetable: None,
        synth: None,
        cut: None,
    }));
    let mut live = LiveScalarBackend::new(48_000, 1).expect("preload live sample bank");
    let generation = AtomicU64::new(1);
    let takeover = AtomicU64::new(0);
    let line_arm = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let mut output = [0.0f32; 128 * 2];
    let before = Violations::capture();
    let report = tripwire::audio_scope(|| {
        live.process_block_with(
            &mut output,
            128,
            0,
            &ring,
            LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &AtomicU64::new(0),
                line_arm: &line_arm,
            },
            &stopped,
        )
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "live bundled-sample ring path allocated or freed: {delta:?}"
    );
    assert_eq!(report.accepted, 1);
    assert_eq!(output, expected, "live ring dropped a sample control");
}

#[test]
fn submitting_while_voices_are_active_preserves_callback_capacity() {
    let _serial = serial_tripwire_test();
    let mut backend = ScalarBackend::new();
    backend.init(48_000).expect("scalar init");

    // Fill the first allocation and activate all eight voices. A reservation
    // based only on `pending.len()` then grows capacity to sixteen while the
    // ninth new event actually needs the seventeenth slot. Counting active
    // plus pending voices before submission is therefore load-bearing.
    for _ in 0..8 {
        backend.note(OnsetEvent::new(0, 440.0, 0.1, 1.0));
    }
    let mut output = [0.0f32; 128 * 2];
    backend.process_block(&mut output, 128);

    for _ in 0..9 {
        backend.note(OnsetEvent::new(128, 660.0, 0.1, 1.0));
    }
    let before = Violations::capture();
    tripwire::audio_scope(|| backend.process_block(&mut output, 128));
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "activating submitted voices allocated or freed: {delta:?}"
    );
}

#[test]
fn live_ring_consumer_is_fixed_capacity_and_generation_filtered() {
    let _serial = serial_tripwire_test();
    let ring = Ring::new(16);
    // One stale event sits ahead of the live generation deliberately.
    assert!(ring.push(AudioEvent {
        onset_id: 1,
        generation: 1,
        target_frame: 0,
        onset_lead: 0.0,
        freq_hz: 220.0,
        gain: 0.5,
        duration_secs: 0.1,
        ui_visuals: 0,
        controls: Default::default(),
        sample: None,
        wavetable: None,
        synth: None,
        cut: None,
    }));
    for id in 2..=3 {
        assert!(ring.push(AudioEvent {
            onset_id: id,
            generation: 2,
            target_frame: 0,
            onset_lead: 0.0,
            freq_hz: 440.0,
            gain: 0.5,
            duration_secs: 0.1,
            ui_visuals: 0,
            controls: Default::default(),
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }));
    }
    let generation = AtomicU64::new(2);
    let takeover = AtomicU64::new(0);
    let line_arm = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let mut backend = LiveScalarBackend::new(48_000, 8).expect("live scalar");
    let mut output = [0.0f32; 128 * 2];

    let before = Violations::capture();
    let report = tripwire::audio_scope(|| {
        backend.process_block_with(
            &mut output,
            128,
            0,
            &ring,
            LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &AtomicU64::new(0),
                line_arm: &line_arm,
            },
            &stopped,
        )
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "live callback violated RT boundary: {delta:?}"
    );
    assert_eq!(report.accepted, 2);
    assert_eq!(report.stale, 1);
    assert_eq!(report.refused, 0);
    assert!(output.iter().any(|sample| sample.abs() > 1e-6));
}

#[test]
fn live_held_events_survive_future_breaks_and_generation_changes_without_allocating() {
    let _serial = serial_tripwire_test();
    for (queued_successor, replace_generation) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let ring = Ring::new(2);
        let event = AudioEvent {
            onset_id: 1,
            generation: 1,
            // Still at the admission horizon on the second block.
            target_frame: 96_256,
            onset_lead: 0.0,
            freq_hz: 440.0,
            gain: 0.5,
            duration_secs: 0.1,
            ui_visuals: 0,
            controls: Default::default(),
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        };
        assert!(ring.push(event));
        let generation = AtomicU64::new(1);
        let takeover = AtomicU64::new(0);
        let line_arm = AtomicU64::new(0);
        let stopped = AtomicBool::new(false);
        let mut backend = LiveScalarBackend::new(48_000, 2).expect("live scalar");
        let mut output = [0.0f32; 128 * 2];
        let before = Violations::capture();
        let first = tripwire::audio_scope(|| {
            backend.process_block_with(
                &mut output,
                128,
                0,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &AtomicU64::new(0),
                    line_arm: &line_arm,
                },
                &stopped,
            )
        });
        assert!(Violations::capture().since(before).clean());
        assert_eq!(
            (first.accepted, first.stale, first.refused, first.late),
            (0, 0, 0, 0)
        );
        assert!(ring.is_empty());
        assert!(output.iter().all(|sample| *sample == 0.0));
        if queued_successor {
            assert!(ring.push(AudioEvent {
                onset_id: 2,
                generation: if replace_generation { 2 } else { 1 },
                freq_hz: 660.0,
                ..event
            }));
        }

        let before = Violations::capture();
        let second = tripwire::audio_scope(|| {
            backend.process_block_with(
                &mut output,
                128,
                128,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &AtomicU64::new(0),
                    line_arm: &line_arm,
                },
                &stopped,
            )
        });
        assert!(Violations::capture().since(before).clean());
        assert_eq!(
            (second.accepted, second.stale, second.refused, second.late),
            (0, 0, 0, 0)
        );
        // Breaking on the retained future event must not pop its successor.
        assert_eq!(ring.len(), usize::from(queued_successor));
        assert!(output.iter().all(|sample| *sample == 0.0));

        if replace_generation {
            takeover.store(256, Ordering::Release);
            generation.store(2, Ordering::Release);
        }
        let before = Violations::capture();
        let third = tripwire::audio_scope(|| {
            backend.process_block_with(
                &mut output,
                128,
                256,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &AtomicU64::new(0),
                    line_arm: &line_arm,
                },
                &stopped,
            )
        });
        assert!(Violations::capture().since(before).clean());
        // An unchanged held event is admitted even without queued work. A
        // replaced one is stale, with or without a new-generation successor.
        let retained = usize::from(!replace_generation);
        assert_eq!(third.accepted, retained + usize::from(queued_successor));
        assert_eq!(third.stale, usize::from(replace_generation));
        assert_eq!((third.refused, third.late), (0, 0));
        assert_eq!(backend.generation(), if replace_generation { 2 } else { 1 });
        assert!(ring.is_empty());
        assert!(output.iter().all(|sample| *sample == 0.0));

        let before = Violations::capture();
        let fourth = tripwire::audio_scope(|| {
            backend.process_block_with(
                &mut output,
                128,
                384,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &AtomicU64::new(0),
                    line_arm: &line_arm,
                },
                &stopped,
            )
        });
        assert!(Violations::capture().since(before).clean());
        assert_eq!(
            (fourth.accepted, fourth.stale, fourth.refused, fourth.late),
            (0, 0, 0, 0)
        );
        assert!(ring.is_empty());
    }
}

#[test]
fn live_ring_preserves_the_complete_oscillator_control_bundle() {
    let _serial = serial_tripwire_test();
    let ring = Ring::new(2);
    let controls = OscillatorControls {
        limit: None,
        live_controls: [0; 2],
        preview_epoch: 7,
        choke_only: false,
        piano: true,
        noise: 0.0,
        modulator_release_secs: 0.01,
        worklet_begin_secs: 0.0,
        lfo_end_secs: f32::INFINITY,
        filter_lfo_end_secs: f32::INFINITY,
        bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
        bus: None,
        busgain: 1.0,
        channels: None,
        waveform: Waveform::Square,
        envelope: Envelope {
            attack_secs: 0.001,
            decay_secs: 0.001,
            sustain: 1.0,
            release_secs: 0.01,
        },
        velocity: 0.5,
        postgain: 0.5,
        pan: Some(1.0),
        filters: FilterControls {
            lowpass: Some(StaticBiquad {
                frequency_hz: 800.0,
                q: 1.0,
            }),
            lowpass_envelope: Some(FilterEnvelope {
                attack_secs: 0.01,
                decay_secs: 0.02,
                sustain: 0.5,
                release_secs: 0.05,
                min_hz: 800.0,
                max_hz: 1_600.0,
            }),
            stages: FilterStages::Two,
            ..FilterControls::default()
        },
        distort: None,
        delay: None,
        duck: None,
        reverb: None,
        dry: None,
        stretch: None,
        fm: None,
        orbit: 1,
        insert_orbit: None,
        effects: [None; rustel_audio::EFFECT_CHAIN],
        instrument: None,
        lfos: [None; rustel_audio::MAX_VOICE_MODS],
        envs: [None; rustel_audio::MAX_VOICE_MODS],
        phaser: None,
        tremolo: None,
        vibrato: None,
        pitch_env: None,
        djf: None,
        compressor: None,
        partials: None,
        transient: None,
        fx_stages: [None; rustel_audio::MAX_FX_STAGES],
        vowel: None,
        coarse: None,
        crush: None,
        shape: None,
    };
    assert!(ring.push(AudioEvent {
        onset_id: 1,
        generation: 1,
        target_frame: 0,
        onset_lead: 0.0,
        freq_hz: 440.0,
        gain: 1.0,
        duration_secs: 0.1,
        ui_visuals: 0,
        controls,
        sample: None,
        wavetable: None,
        synth: None,
        cut: None,
    }));
    let generation = AtomicU64::new(1);
    let takeover = AtomicU64::new(0);
    let line_arm = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let mut backend = LiveScalarBackend::new(48_000, 1).expect("live scalar");
    let mut output = [0.0f32; 128 * 2];
    let before = Violations::capture();
    let report = tripwire::audio_scope(|| {
        backend.process_block_with(
            &mut output,
            128,
            0,
            &ring,
            LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &AtomicU64::new(0),
                line_arm: &line_arm,
            },
            &stopped,
        )
    });
    let delta = Violations::capture().since(before);
    assert!(delta.clean(), "control propagation allocated: {delta:?}");
    assert_eq!(report.accepted, 1);
    let mut reference = ScalarBackend::prepared(48_000, 1).expect("reference scalar");
    assert!(
        reference.try_note_prepared(OnsetEvent::new(0, 440.0, 1.0, 0.1).with_controls(controls))
    );
    let mut expected = [0.0f32; 128 * 2];
    reference.process_block(&mut expected, 128);
    assert!(output == expected, "live boundary dropped a scalar control");
    assert!(
        output
            .as_chunks::<2>()
            .0
            .iter()
            .all(|frame| frame[0].abs() <= f32::EPSILON)
    );
    assert!(
        output
            .as_chunks::<2>()
            .0
            .iter()
            .any(|frame| frame[1].abs() > 0.01)
    );
}

#[test]
fn replacing_preview_voices_has_no_callback_allocation_or_free() {
    let _serial = serial_tripwire_test();
    let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
    for (frame, epoch) in [(0, 0), (0, 1), (0, 1), (512, 2), (512, 2)] {
        let mut event = OnsetEvent::new(frame, 220.0, 0.1, 3.0);
        event.controls.preview_epoch = epoch;
        backend.note(event);
    }
    let mut out = [0.0; 256];
    let before = Violations::capture();
    for _ in 0..12 {
        tripwire::audio_scope(|| backend.process_block(&mut out, 128));
    }
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "preview replacement violated no-alloc contract: {delta:?}"
    );
    assert!(out.iter().any(|sample| sample.abs() > 0.001));
}

#[test]
fn live_pending_slots_reuse_retired_event_storage_without_allocating() {
    let _serial = serial_tripwire_test();
    let ring = Ring::new(16);
    let controls = OscillatorControls {
        limit: None,
        envelope: Envelope {
            attack_secs: 0.0,
            decay_secs: 0.0,
            sustain: 1.0,
            release_secs: 0.0,
        },
        ..OscillatorControls::default()
    };
    for id in 0..8u64 {
        assert!(ring.push(AudioEvent {
            onset_id: id,
            generation: 1,
            target_frame: id * BLOCK_FRAMES,
            onset_lead: 0.0,
            freq_hz: 220.0 + id as f32,
            gain: 0.5,
            duration_secs: 0.000_1,
            ui_visuals: 0,
            controls,
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }));
    }

    let generation = AtomicU64::new(1);
    let takeover = AtomicU64::new(0);
    let line_arm = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let mut backend = LiveScalarBackend::new(48_000, 8).expect("live scalar");
    let mut output = [0.0f32; BLOCK_FRAMES as usize * 2];

    let before = Violations::capture();
    let first = tripwire::audio_scope(|| {
        backend.process_block_with(
            &mut output,
            BLOCK_FRAMES as usize,
            0,
            &ring,
            LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &AtomicU64::new(0),
                line_arm: &line_arm,
            },
            &stopped,
        )
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "initial pending admission allocated: {delta:?}"
    );
    assert_eq!(first.accepted, 8);
    assert_eq!(first.refused, 0);

    // Frame zero's event left a hole in the pending store. Capacity is full
    // if that slot cannot be reused, so admitting this event exercises the
    // free-list path rather than spare Vec capacity.
    assert!(ring.push(AudioEvent {
        onset_id: 8,
        generation: 1,
        target_frame: 8 * BLOCK_FRAMES,
        onset_lead: 0.0,
        freq_hz: 228.0,
        gain: 0.5,
        duration_secs: 0.000_1,
        ui_visuals: 0,
        controls,
        sample: None,
        wavetable: None,
        synth: None,
        cut: None,
    }));
    let before = Violations::capture();
    let second = tripwire::audio_scope(|| {
        backend.process_block_with(
            &mut output,
            BLOCK_FRAMES as usize,
            BLOCK_FRAMES,
            &ring,
            LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &AtomicU64::new(0),
                line_arm: &line_arm,
            },
            &stopped,
        )
    });
    let delta = Violations::capture().since(before);
    assert!(delta.clean(), "reusing a pending slot allocated: {delta:?}");
    assert_eq!(second.accepted, 1);
    assert_eq!(second.refused, 0);
}

#[test]
fn live_voice_overflow_refuses_without_allocating_or_truncating_silently() {
    let _serial = serial_tripwire_test();
    let ring = Ring::new(4);
    for id in 0..2 {
        assert!(ring.push(AudioEvent {
            onset_id: id,
            generation: 1,
            target_frame: 0,
            onset_lead: 0.0,
            freq_hz: 440.0,
            gain: 0.5,
            duration_secs: 1.0,
            ui_visuals: 0,
            controls: Default::default(),
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }));
    }
    let generation = AtomicU64::new(1);
    let takeover = AtomicU64::new(0);
    let line_arm = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let mut backend = LiveScalarBackend::new(48_000, 1).expect("live scalar");
    let mut output = [0.0f32; 128 * 2];

    let before = Violations::capture();
    let report = tripwire::audio_scope(|| {
        backend.process_block_with(
            &mut output,
            128,
            0,
            &ring,
            LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &AtomicU64::new(0),
                line_arm: &line_arm,
            },
            &stopped,
        )
    });
    let delta = Violations::capture().since(before);
    assert!(delta.clean(), "overflow path allocated: {delta:?}");
    assert_eq!(report.accepted, 1);
    assert_eq!(report.refused, 1, "overflow was silently truncated");
}

#[test]
fn live_voice_capacity_is_bounded_before_allocation() {
    let _serial = serial_tripwire_test();
    assert!(LiveScalarBackend::new(48_000, MAX_LIVE_VOICES).is_ok());
    assert!(LiveScalarBackend::new(48_000, 0).is_err());
    assert!(LiveScalarBackend::new(48_000, MAX_LIVE_VOICES + 1).is_err());
}

#[test]
fn malformed_live_block_sizes_fail_silent_without_panicking_or_allocating() {
    let _serial = serial_tripwire_test();
    let ring = Ring::new(1);
    let generation = AtomicU64::new(1);
    let takeover = AtomicU64::new(0);
    let line_arm = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let mut backend = LiveScalarBackend::new(48_000, 1).expect("live scalar");
    let mut output = [];

    let before = Violations::capture();
    let overflow = tripwire::audio_scope(|| {
        backend.process_block_with(
            &mut output,
            usize::MAX,
            0,
            &ring,
            LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &AtomicU64::new(0),
                line_arm: &line_arm,
            },
            &stopped,
        )
    });
    let short = tripwire::audio_scope(|| {
        backend.process_block_with(
            &mut output,
            1,
            0,
            &ring,
            LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &AtomicU64::new(0),
                line_arm: &line_arm,
            },
            &stopped,
        )
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "malformed-block refusal allocated: {delta:?}"
    );
    assert_eq!(overflow, Default::default());
    assert_eq!(short, Default::default());
}

/// Bus receivers are ordered after their senders once per block, on the audio
/// callback. That reordering must not allocate - `sort_by_key` would, above
/// ~20 elements, so the partition is done by in-place rotation instead. Enough
/// voices here to be past that threshold.
#[test]
fn ordering_bus_receivers_on_the_callback_does_not_allocate() {
    let _serial = serial_tripwire_test();
    let mut backend = ScalarBackend::new();
    backend.init(48_000).expect("scalar init");
    let voice = |bus: Option<u8>, receives: bool| {
        let mut controls = OscillatorControls {
            limit: None,
            noise: 0.0,
            bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
            bus,
            busgain: 1.0,
            channels: None,
            waveform: Waveform::Sawtooth,
            envelope: Envelope {
                attack_secs: 0.001,
                decay_secs: 0.001,
                sustain: 1.0,
                release_secs: 0.01,
            },
            ..OscillatorControls::default()
        };
        if receives {
            controls.bus_mods[0] = Some(rustel_audio::BusMod {
                fxi: None,
                bus: 0,
                target: rustel_audio::ModTarget::Gain,
                depth: 1.0,
                dc: 0.0,
                min: -1.0,
                max: 1.0,
                param_base: 0.7,
            });
        }
        OnsetEvent::new(0, 220.0, 0.7, 0.5).with_controls(controls)
    };
    // Interleaved so the partition actually has work to do, and 40 voices so
    // an allocating sort would be past its insertion-sort fast path.
    for n in 0..40 {
        let event = if n % 2 == 0 {
            voice(Some(0), false)
        } else {
            voice(None, true)
        };
        // The producer path, which reserves the voice capacity the callback
        // then fills - `try_note_prepared` would refuse past the default.
        let _ = n;
        backend.note(event);
    }

    let mut out = [0.0f32; 128 * 2];
    let before = Violations::capture();
    tripwire::audio_scope(|| backend.process_block(&mut out, 128));
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "ordering bus receivers allocated or freed: {delta:?}"
    );
    assert!(out.iter().any(|sample| sample.abs() > 1e-6));
}

/// The safety limiter runs inside the device callback, so it must not
/// allocate, free or lock there. Its rings are fixed arrays and its only
/// allocation is the `Box` the stream takes at setup, before the scope.
#[test]
fn the_limiter_does_not_allocate_in_the_callback() {
    let _serial = serial_tripwire_test();
    let mut limiter = Box::new(rustel_audio::Limiter::new(
        48_000,
        rustel_audio::DEFAULT_THRESHOLD_DB,
        rustel_audio::LimiterCharacter::Transparent,
    ));
    // A block that makes it work: past the ceiling, and back under it.
    let mut block: Vec<f32> = (0..BLOCK_FRAMES * 2)
        .map(|index| if index % 97 < 8 { 6.0 } else { 0.2 })
        .collect();

    let before = Violations::capture();
    tripwire::audio_scope(|| {
        for _ in 0..64 {
            limiter.process_stereo(&mut block);
            let _ = limiter.take_reduction();
        }
    });
    let delta = Violations::capture().since(before);
    assert!(
        delta.clean(),
        "the limiter allocated on the audio thread: {delta:?}"
    );
}

/// The producer's side of the live handshake, for the rewind paths below.
struct Handshake {
    ring: Ring,
    generation: AtomicU64,
    takeover: AtomicU64,
    cut: AtomicU64,
    line_arm: AtomicU64,
    stopped: AtomicBool,
}

impl Handshake {
    fn flip(&self, generation: u64, takeover: u64, cut: rustel_audio::TakeoverCut) {
        self.takeover.store(takeover, Ordering::Release);
        self.cut.store(cut as u64, Ordering::Release);
        self.line_arm.store(0, Ordering::Release);
        self.generation.store(generation, Ordering::Release);
    }

    fn push(&self, onset_id: u64, generation: u64, target_frame: u64, duration_secs: f32) {
        assert!(self.ring.push(AudioEvent {
            onset_id,
            generation,
            target_frame,
            onset_lead: 0.0,
            freq_hz: 220.0,
            gain: 0.5,
            duration_secs,
            ui_visuals: 0,
            controls: OscillatorControls {
                envelope: Envelope {
                    attack_secs: 0.001,
                    decay_secs: 0.001,
                    sustain: 1.0,
                    release_secs: 0.01,
                },
                ..OscillatorControls::default()
            },
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }));
    }

    /// One 128-frame block at `start`, inside the callback scope, asserted
    /// free of allocation, free and lock.
    fn block(&self, live: &mut LiveScalarBackend, start: u64) -> rustel_audio::LiveBlockReport {
        let mut output = [0.0f32; 128 * 2];
        let before = Violations::capture();
        let report = tripwire::audio_scope(|| {
            live.process_block_with(
                &mut output,
                128,
                start,
                &self.ring,
                LiveFlipAtomics {
                    generation: &self.generation,
                    takeover_frame: &self.takeover,
                    takeover_cut: &self.cut,
                    line_arm: &self.line_arm,
                },
                &self.stopped,
            )
        });
        let delta = Violations::capture().since(before);
        assert!(
            delta.clean(),
            "the rewind block at {start} allocated or freed: {delta:?}"
        );
        report
    }
}

/// Every branch a rewind adds to the callback runs under the tripwire: the
/// pre-armed line cut firing (pending retired, voices faded, a late
/// outgoing onset pre-faded at activation), its drop horizon refusing a
/// ghost, a withdrawn arm let go, an AtTakeover flip across a skipped
/// generation, an AtFlip flip's own drop horizon and a restart's past-due
/// downbeat retimed to its floor, an edit's flip that keeps a rewind's arm
/// and its countdown (pending and ring) for onsets activating after it, and
/// one at the line that lets the arm go. Frame-driven, so the same on every
/// platform.
#[test]
fn takeover_cut_and_line_arm_paths_do_not_allocate() {
    use rustel_audio::{LINE_ARM_WITHDRAWN, TakeoverCut};

    let _serial = serial_tripwire_test();
    let hs = Handshake {
        ring: Ring::new(16),
        generation: AtomicU64::new(1),
        takeover: AtomicU64::new(0),
        cut: AtomicU64::new(0),
        line_arm: AtomicU64::new(0),
        stopped: AtomicBool::new(false),
    };
    let mut live = LiveScalarBackend::new(48_000, 8).expect("live scalar");

    // A: the line arm fires at 256 with its drop bit.
    const LINE: u64 = 256;
    hs.push(1, 1, 0, 4.0);
    assert_eq!(hs.block(&mut live, 0).accepted, 1);
    hs.line_arm.store((LINE << 2) | 0b11, Ordering::Release);
    assert_eq!(hs.block(&mut live, 128), Default::default());
    hs.push(2, 1, LINE + 4_800, 0.5); // the ghost past the line
    hs.push(3, 1, LINE - 56, 0.5); // late, before the line: pre-faded
    let fired = hs.block(&mut live, 256);
    assert_eq!(
        (fired.accepted, fired.stale, fired.late),
        (1, 1, 1),
        "the arm fires, refuses the ghost and pre-fades the late onset"
    );
    hs.push(4, 1, 384 + 4_800, 0.5);
    let after = hs.block(&mut live, 384);
    assert_eq!(
        (after.accepted, after.stale),
        (0, 1),
        "the drop horizon stands until a flip or a withdrawal"
    );

    // B: withdrawn, then a quantised rewind flips 1 -> 3 at its line.
    hs.line_arm.store(LINE_ARM_WITHDRAWN, Ordering::Release);
    hs.push(5, 1, 512 + 9_600, 0.5); // passes again, retired by the flip
    let withdrawn = hs.block(&mut live, 512);
    assert_eq!((withdrawn.accepted, withdrawn.stale), (1, 0));
    const LINE2: u64 = 1_024;
    hs.flip(3, LINE2, TakeoverCut::AtTakeover);
    hs.push(6, 3, LINE2, 0.5); // the restart's downbeat
    hs.push(7, 1, LINE2 + 1_000, 0.5); // the old score past the line
    hs.push(8, 1, LINE2 - 200, 0.5); // a countdown hit activating after the flip
    let mut totals = (0, 0);
    for start in (640..=LINE2 + 512).step_by(128) {
        let report = hs.block(&mut live, start);
        totals.0 += report.accepted;
        totals.1 += report.stale;
    }
    assert_eq!(totals, (2, 1));
    assert_eq!(live.generation(), 3);

    // C: an immediate rewind flips at 1664, its takeover past the ghost so
    // only the cut's own drop horizon refuses it; its downbeat is past due.
    const FLIP: u64 = 1_664;
    hs.flip(4, FLIP + 9_600, TakeoverCut::AtFlip);
    hs.push(9, 3, FLIP + 4_800, 0.5);
    hs.push(10, 4, FLIP - 480, 0.5);
    let flipped = hs.block(&mut live, FLIP);
    assert_eq!(
        (flipped.accepted, flipped.stale, flipped.late),
        (1, 1, 0),
        "the ghost is refused and the downbeat lands at the floor, not late"
    );
    assert_eq!(live.generation(), 4);

    // D: a quantised rewind arms its line at 4096 and an edit's flip lands
    // before it, its takeover ahead of the countdown's last hits: the one
    // already admitted is kept to the line and activates after the edit,
    // the one still in the ring is admitted, and the ghost past the line is
    // refused.
    const LINE3: u64 = 4_096;
    hs.push(11, 4, LINE3 - 300, 0.5);
    assert_eq!(hs.block(&mut live, 1_792).accepted, 1);
    hs.flip(5, LINE3, TakeoverCut::AtTakeover);
    hs.block(&mut live, 1_920);
    hs.flip(6, LINE3 - 1_024, TakeoverCut::None);
    hs.push(12, 4, LINE3 - 200, 0.5);
    hs.push(13, 4, LINE3 + 200, 0.5);
    let edited = hs.block(&mut live, 2_048);
    assert_eq!(
        (edited.accepted, edited.stale),
        (1, 1),
        "the countdown keeps its line across the edit's flip; its ghost does not"
    );
    for start in (2_176..=LINE3 + 512).step_by(128) {
        hs.block(&mut live, start);
    }
    assert_eq!(live.generation(), 6);

    // E: an edit's flip in the very block of a rewind's line lets the arm
    // go.
    const LINE4: u64 = 5_120;
    hs.flip(7, LINE4, TakeoverCut::AtTakeover);
    hs.block(&mut live, 4_736);
    hs.flip(8, LINE4 + 4_800, TakeoverCut::None);
    hs.block(&mut live, LINE4);
    assert_eq!(live.generation(), 8);
}

#[test]
fn piano_release_at_full_capacity_has_no_callback_allocation_or_voice_steal() {
    let _serial = serial_tripwire_test();
    let mut backend = ScalarBackend::prepared(48_000, 2).unwrap();
    backend.set_max_polyphony(2);
    assert!(backend.try_note_prepared(OnsetEvent::new(0, 220.0, 0.1, f32::INFINITY)));
    let mut note = OnsetEvent::new(0, 440.0, 0.1, f32::INFINITY).with_cut(Some(1.0));
    note.controls.piano = true;
    assert!(backend.try_note_prepared(note));
    let mut out = [0.0; 256];
    backend.process_block(&mut out, 128);
    let mut release = OnsetEvent::new(128, 440.0, 0.0, 0.01).with_cut(Some(1.0));
    release.controls.choke_only = true;
    release.controls.piano = true;
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        assert!(backend.try_note_prepared(release));
        for _ in 0..16 {
            backend.process_block(&mut out, 128);
        }
    });
    let delta = Violations::capture().since(before);
    assert!(delta.clean(), "piano release allocated or freed: {delta:?}");
    let mut expected_backend = ScalarBackend::prepared(48_000, 1).unwrap();
    assert!(expected_backend.try_note_prepared(OnsetEvent::new(0, 220.0, 0.1, f32::INFINITY)));
    let mut expected = [0.0; 256];
    for _ in 0..17 {
        expected_backend.process_block(&mut expected, 128);
    }
    assert_eq!(
        out, expected,
        "key-up must leave the score exactly unchanged"
    );
    assert!(out.iter().all(|sample| sample.is_finite()));
    assert!(out.iter().any(|sample| sample.abs() > 0.0001));
}

#[test]
fn immediate_piano_chords_and_releases_are_allocation_free() {
    let _serial = serial_tripwire_test();
    let immediate = Ring::new(64);
    let score = Ring::new(2);
    let mut backend = LiveScalarBackend::new(48_000, 32).unwrap();
    let generation = AtomicU64::new(1);
    let takeover_frame = AtomicU64::new(0);
    let takeover_cut = AtomicU64::new(0);
    let line_arm = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let atomics = LiveFlipAtomics {
        generation: &generation,
        takeover_frame: &takeover_frame,
        takeover_cut: &takeover_cut,
        line_arm: &line_arm,
    };
    let event = |slot, off| AudioEvent {
        onset_id: u64::MAX,
        generation: 0,
        target_frame: u64::MAX,
        onset_lead: 0.0,
        freq_hz: 220.0 + f32::from(slot) * 20.0,
        gain: 0.02,
        duration_secs: f32::INFINITY,
        ui_visuals: 0,
        controls: OscillatorControls {
            piano: true,
            choke_only: off,
            ..Default::default()
        },
        sample: None,
        wavetable: None,
        synth: None,
        cut: Some(f32::from(slot) + 1.0),
    };
    for key in 0u8..15 {
        assert!(immediate.push(event(key, false)));
    }
    let mut output = [0.0; 256];
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        assert_eq!(backend.admit_immediate(&immediate, 0, 128).accepted, 15);
        backend.process_block_with(&mut output, 128, 0, &score, atomics, &stopped);
    });
    assert!(Violations::capture().since(before).clean());
    assert!(output.iter().any(|sample| sample.abs() > 0.0001));
    for key in 0u8..15 {
        assert!(immediate.push(event(key, true)));
    }
    let before = Violations::capture();
    tripwire::audio_scope(|| {
        assert_eq!(backend.admit_immediate(&immediate, 128, 128).accepted, 15);
        for block in 1..32 {
            backend.process_block_with(&mut output, 128, block * 128, &score, atomics, &stopped);
        }
    });
    assert!(Violations::capture().since(before).clean());
    assert!(
        output
            .iter()
            .all(|sample| sample.is_finite() && sample.abs() < 0.000001)
    );
}
