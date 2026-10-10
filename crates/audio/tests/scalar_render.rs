use rustel_audio::{
    BUNDLED_BD_SAMPLE_ID, Envelope, FilterControls, LiveFlipAtomics, OnsetEvent,
    OscillatorControls, SampleControls, SampleHold, ScalarBackend, Waveform, render_pcm,
    write_pcm16_wav,
};

/// The plain `fmi`/`fmi2`.. chain expressed as matrix routes: operator `n`
/// into operator `n - 1`, and operator 1 into the carrier. Slot `k` is
/// operator `k + 1`; `None` leaves that operator - and the connection it would
/// have made - absent.
fn fm_chain(stages: &[Option<(f32, f32)>]) -> rustel_audio::FmControls {
    let mut operators = [None; rustel_audio::MAX_FM_OPERATORS];
    let mut routes = [None; rustel_audio::MAX_FM_ROUTES];
    for (slot, stage) in stages.iter().enumerate() {
        let Some((amount, harmonicity)) = *stage else {
            continue;
        };
        operators[slot] = Some(rustel_audio::FmOperator {
            harmonicity,
            waveform: rustel_audio::FmWave::Sine,
            env: None,
            env_exponential: true,
        });
        routes[slot] = Some(rustel_audio::FmRoute {
            source: slot as u8 + 1,
            target: slot as u8,
            amount,
            mod_slot: Some(slot as u8),
        });
    }
    rustel_audio::FmControls { operators, routes }
}

fn controls(waveform: Waveform, pan: Option<f32>) -> OscillatorControls {
    OscillatorControls {
        limit: None,
        live_controls: [0; 2],
        preview_epoch: 0,
        choke_only: false,
        piano: false,
        noise: 0.0,
        modulator_release_secs: 0.01,
        worklet_begin_secs: 0.0,
        lfo_end_secs: f32::INFINITY,
        filter_lfo_end_secs: f32::INFINITY,
        bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
        bus: None,
        busgain: 1.0,
        channels: None,
        waveform,
        envelope: Envelope {
            attack_secs: 0.001,
            decay_secs: 0.001,
            sustain: 1.0,
            release_secs: 0.01,
        },
        velocity: 1.0,
        postgain: 1.0,
        pan,
        filters: FilterControls::default(),
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
    }
}

#[test]
fn scalar_wav_is_streamed_with_a_correct_non_silent_body() {
    let path = std::env::temp_dir().join(format!(
        "rustel-scalar-{}-{}.wav",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let event = OnsetEvent::new(0, 440.0, 0.8, 0.08);
    let bytes = write_pcm16_wav(&path, &mut ScalarBackend::new(), 48_000, 4_800, &[event])
        .expect("write scalar WAV");
    assert_eq!(bytes, 4_800 * 2 * 2);
    let wav = std::fs::read(&path).expect("read scalar WAV");
    let _ = std::fs::remove_file(path);
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(&wav[36..40], b"data");
    assert_eq!(wav.len(), 44 + bytes);
    assert!(
        wav[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .any(|sample| *sample != [0, 0]),
        "audible scalar WAV was all zeroes"
    );
}

#[test]
fn waveform_and_equal_power_pan_are_load_bearing() {
    let square =
        OnsetEvent::new(0, 1_000.0, 1.0, 0.02).with_controls(controls(Waveform::Square, Some(1.0)));
    let pcm = render_pcm(&mut ScalarBackend::new(), 48_000, 2_000, &[square]).expect("render");
    assert!(
        pcm.as_chunks::<2>()
            .0
            .iter()
            .all(|frame| frame[0].abs() <= f32::EPSILON),
        "hard-right pan leaked into the left channel"
    );
    assert!(
        pcm.as_chunks::<2>()
            .0
            .iter()
            .any(|frame| frame[1].abs() > 0.25),
        "square shape or the required 0.3 oscillator gain was lost"
    );

    let centered =
        OnsetEvent::new(0, 1_000.0, 1.0, 0.02).with_controls(controls(Waveform::Sine, Some(0.5)));
    let unpanned =
        OnsetEvent::new(0, 1_000.0, 1.0, 0.02).with_controls(controls(Waveform::Sine, None));
    let centered =
        render_pcm(&mut ScalarBackend::new(), 48_000, 2_000, &[centered]).expect("centered render");
    let unpanned =
        render_pcm(&mut ScalarBackend::new(), 48_000, 2_000, &[unpanned]).expect("unpanned render");
    let centered_peak = centered.iter().copied().map(f32::abs).fold(0.0, f32::max);
    let unpanned_peak = unpanned.iter().copied().map(f32::abs).fold(0.0, f32::max);
    assert!(
        ((centered_peak / unpanned_peak) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6,
        "explicit center must use StereoPanner equal-power attenuation"
    );
}

#[test]
fn velocity_postgain_and_release_are_not_discarded() {
    let mut full = controls(Waveform::Sine, None);
    full.envelope = Envelope {
        attack_secs: 0.001,
        decay_secs: 0.001,
        sustain: 1.0,
        release_secs: 0.02,
    };
    let mut scaled = full;
    scaled.velocity = 0.5;
    scaled.postgain = 0.4;
    let full = render_pcm(
        &mut ScalarBackend::new(),
        48_000,
        2_000,
        &[OnsetEvent::new(0, 1_000.0, 1.0, 0.01).with_controls(full)],
    )
    .expect("full render");
    let scaled = render_pcm(
        &mut ScalarBackend::new(),
        48_000,
        2_000,
        &[OnsetEvent::new(0, 1_000.0, 1.0, 0.01).with_controls(scaled)],
    )
    .expect("scaled render");
    let full_peak = full.iter().copied().map(f32::abs).fold(0.0, f32::max);
    let scaled_peak = scaled.iter().copied().map(f32::abs).fold(0.0, f32::max);
    assert!((scaled_peak / full_peak - 0.2).abs() < 1e-6);
    let gate_frame = 480usize;
    assert!(
        full[gate_frame * 2..]
            .iter()
            .any(|sample| sample.abs() > 1e-4),
        "release must extend after the note gate"
    );
    assert!(
        full[1_440 * 2..].iter().all(|sample| sample.abs() < 1e-6),
        "voice must end after gate plus release"
    );
}

/// `postgain` is the post GainNode. strudel.cc builds it after the whole
/// chain. `distort(2)` is non-linear, so it shows whether postgain scales
/// the output or the drive.
#[test]
fn postgain_scales_the_distorted_output_rather_than_the_drive() {
    let mut full = controls(Waveform::Sawtooth, None);
    full.distort = Some(rustel_audio::DistortControls {
        amount: 2.0,
        postgain: 1.0,
        algorithm: 0,
    });
    let mut scaled = full;
    scaled.postgain = 0.5;
    let render = |controls| {
        render_pcm(
            &mut ScalarBackend::new(),
            48_000,
            4_000,
            &[OnsetEvent::new(0, 220.0, 1.0, 0.08).with_controls(controls)],
        )
        .expect("render")
    };
    let full = render(full);
    let scaled = render(scaled);
    assert!(
        full.iter().copied().map(f32::abs).fold(0.0, f32::max) > 0.1,
        "the fixture must actually be driving the waveshaper"
    );
    let worst = full
        .iter()
        .zip(&scaled)
        .map(|(full, scaled)| (full * 0.5 - scaled).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst < 1e-6,
        "postgain moved the drive rather than the output: off by {worst}"
    );
}

/// An FM modulator first runs in the quantum where its carrier starts, from
/// phase zero. Two identical FM notes are the same sound wherever they start.
#[test]
fn two_identical_fm_notes_sound_the_same_wherever_they_start() {
    let mut controls = controls(Waveform::Sine, None);
    controls.fm = Some(fm_chain(&[Some((4.0, 2.0))]));
    let render = |onset: u64| {
        render_pcm(
            &mut ScalarBackend::new(),
            48_000,
            onset as usize + 8_000,
            &[OnsetEvent::new(onset, 130.81, 1.0, 0.1).with_controls(controls)],
        )
        .expect("render")
    };
    let first = render(0);
    // A whole number of 128-frame render quanta later.
    let later = render(48_000);
    let worst = first
        .iter()
        .zip(&later[48_000 * 2..])
        .map(|(first, later)| (first - later).abs())
        .fold(0.0f32, f32::max);
    assert!(
        first.iter().copied().map(f32::abs).fold(0.0, f32::max) > 0.01,
        "the fixture must actually be sounding"
    );
    assert!(worst < 1e-9, "the two notes differ by {worst}");
}

/// Where in its render quantum a note starts is still audible, because Blink
/// writes an oscillator's first quantum from its sub-quantum offset while
/// reading the frequency param from the START of that quantum. So the carrier
/// hears the quantum's FIRST modulator samples until the quantum ends, and the
/// modulators are `offset` samples ahead from there on.
#[test]
fn an_fm_note_hears_its_modulators_from_the_start_of_its_quantum() {
    let mut controls = controls(Waveform::Sine, None);
    controls.fm = Some(fm_chain(&[Some((4.0, 2.0))]));
    let render = |onset: u64| {
        render_pcm(
            &mut ScalarBackend::new(),
            48_000,
            onset as usize + 8_000,
            &[OnsetEvent::new(onset, 130.81, 1.0, 0.1).with_controls(controls)],
        )
        .expect("render")
    };
    let aligned = render(0);
    let offset = 64u64;
    let late = render(offset);
    let skip = offset as usize * 2;
    // Up to the quantum boundary both notes read the same modulator samples,
    // counting from zero, so they are the same signal.
    let head = aligned[..skip]
        .iter()
        .zip(&late[skip..skip * 2])
        .map(|(aligned, late)| (aligned - late).abs())
        .fold(0.0f32, f32::max);
    assert!(head < 1e-9, "the first {offset} samples differ by {head}");
    // At the boundary the late note's modulators jump the offset they were
    // never charged for, and the two part company.
    let tail = aligned[skip..4_000 * 2]
        .iter()
        .zip(&late[skip * 2..])
        .map(|(aligned, late)| (aligned - late).abs())
        .fold(0.0f32, f32::max);
    assert!(
        tail > 0.01,
        "the modulators did not catch up at the quantum boundary (differ by {tail})"
    );
}

#[test]
fn zero_speed_sample_is_an_audio_no_op() {
    let event = OnsetEvent::new(0, 0.0, 1.0, 0.1).with_sample(SampleControls {
        sample: BUNDLED_BD_SAMPLE_ID,
        playback_rate: 1.0,
        begin: 0.0,
        end: 1.0,
        hold: SampleHold::Slice,
        muted: true,
        loop_secs: None,
        envelope_peak: 1.0,
        reversed: false,
        nudge_secs: 0.0,
        cut: None,
    });
    let pcm = render_pcm(&mut ScalarBackend::new(), 48_000, 4_800, &[event]).expect("render");
    assert!(
        pcm.iter().all(|sample| *sample == 0.0),
        "No voice is created when sample speed is zero"
    );
}

/// A looped sample is silent for the whole hap while its step is longer than
/// its loop, and sounds to the end of the hap at a step that fits.
#[test]
fn a_looped_sample_is_silent_while_its_step_is_longer_than_the_loop() {
    // The bundled bd is 12 000 frames at 48 kHz, so loopEnd at 0.25 ms is a
    // 12-frame loop.
    let render = |playback_rate: f32| {
        let event = OnsetEvent::new(0, 0.0, 1.0, 0.2).with_sample(SampleControls {
            sample: BUNDLED_BD_SAMPLE_ID,
            playback_rate,
            begin: 0.0,
            end: 1.0,
            hold: SampleHold::Hap,
            muted: false,
            loop_secs: Some((0.0, 0.000_25)),
            envelope_peak: 1.0,
            reversed: false,
            nudge_secs: 0.0,
            cut: None,
        });
        render_pcm(&mut ScalarBackend::new(), 48_000, 9_600, &[event]).expect("render")
    };
    let peak = |pcm: &[f32]| {
        pcm.iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
    };
    // An 8-frame step cycles through frames 0, 8 and 4, which clear 0.01.
    assert!(
        peak(&render(8.0)[6_000 * 2..]) > 0.01,
        "a step that fits the loop went silent before the hap ended"
    );
    assert_eq!(
        peak(&render(16.0)),
        0.0,
        "a step longer than the loop sounded"
    );
}

/// A looped sample detuned past the `f32` range has an infinite step; its
/// output stays finite.
#[test]
fn an_infinite_step_on_a_looped_sample_stays_finite() {
    let mut detuned = controls(Waveform::Sine, None);
    detuned.pitch_env = Some(rustel_audio::PitchEnvControls {
        adsr: rustel_audio::FilterEnvelope {
            attack_secs: 0.001,
            decay_secs: 0.001,
            sustain: 1.0,
            release_secs: 0.001,
            min_hz: 0.0,
            // Cents: `2^(200_000 / 1200)` overflows `f32`.
            max_hz: 200_000.0,
        },
        exponential: false,
    });
    let event = OnsetEvent::new(0, 0.0, 1.0, 0.2)
        .with_controls(detuned)
        .with_sample(SampleControls {
            sample: BUNDLED_BD_SAMPLE_ID,
            playback_rate: 1.0,
            begin: 0.0,
            end: 1.0,
            hold: SampleHold::Hap,
            muted: false,
            loop_secs: Some((0.0, 0.1)),
            envelope_peak: 1.0,
            reversed: false,
            nudge_secs: 0.0,
            cut: None,
        });
    let pcm = render_pcm(&mut ScalarBackend::new(), 48_000, 9_600, &[event]).expect("render");
    assert!(
        pcm.iter().all(|sample| sample.is_finite()),
        "an infinite loop step rendered a non-finite sample"
    );
}

#[test]
fn duck_sidechain_dips_the_target_orbit_exponentially() {
    // A steady tone in orbit 2 plus a near-silent trigger carrying
    // duckorbit(2): ramps the ORBIT's output gain
    // exponentially to clamp(1-sqrt(depth), 0.01, current) at t+onset and
    // back to 1 at t+onset+attack.
    let sr = 48_000u32;
    let tone =
        OnsetEvent::new(0, 220.0, 1.0, 1.0).with_controls(rustel_audio::OscillatorControls {
            limit: None,
            noise: 0.0,
            bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
            bus: None,
            busgain: 1.0,
            channels: None,
            waveform: rustel_audio::Waveform::Sine,
            envelope: rustel_audio::Envelope {
                attack_secs: 0.001,
                decay_secs: 0.001,
                sustain: 1.0,
                release_secs: 0.01,
            },
            orbit: 2,
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
            ..Default::default()
        });
    let duck_at = 24_000u64; // 0.5 s
    let mut ducker =
        OnsetEvent::new(1, 220.0, 0.0, 0.01).with_controls(rustel_audio::OscillatorControls {
            limit: None,
            noise: 0.0,
            bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
            bus: None,
            busgain: 1.0,
            channels: None,
            duck: Some(rustel_audio::DuckControls {
                targets: [
                    Some(rustel_audio::DuckTarget {
                        orbit: 2,
                        onset_secs: 0.0,
                        attack_secs: 0.1,
                        depth: 1.0,
                    }),
                    None,
                    None,
                    None,
                ],
            }),
            ..Default::default()
        });
    ducker.onset_frame = duck_at;
    let pcm = render_pcm(&mut ScalarBackend::new(), sr, 48_000, &[tone, ducker]).expect("render");

    let rms = |from: usize, to: usize| {
        let slice = &pcm[from * 2..to * 2];
        (slice
            .iter()
            .map(|s| f64::from(*s) * f64::from(*s))
            .sum::<f64>()
            / slice.len() as f64)
            .sqrt()
    };
    let before = rms(20_000, 23_000);
    // Right at the dip bottom (t..t+a few ms) the gain is ~0.01.
    let dipped = rms(24_050, 24_500);
    // Well after attack (0.1 s = 4_800 frames) the gain is back to 1.
    let recovered = rms(30_000, 34_000);
    assert!(
        dipped < before * 0.1,
        "duck must dip the orbit: before {before:.4}, dipped {dipped:.4}"
    );
    assert!(
        recovered > before * 0.8,
        "duck must recover to unity: before {before:.4}, recovered {recovered:.4}"
    );
}

#[test]
fn wavetable_unison_produces_stereo_width() {
    // Synthetic table: one saw frame, so voices at different phases produce
    // different instantaneous values and panspread MUST separate channels.
    let frame_len = 2048usize;
    let mut pcm = vec![0.0f32; frame_len];
    for (i, s) in pcm.iter_mut().enumerate() {
        *s = (i as f32 / frame_len as f32) * 2.0 - 1.0;
    }
    let mut backend = ScalarBackend::prepared(48_000, 8).expect("backend");
    let decoded = rustel_audio::decode_wav(&{
        // 16-bit mono WAV writer inline
        let mut bytes: Vec<u8> = Vec::new();
        let data_len = (pcm.len() * 2) as u32;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&96_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for s in &pcm {
            bytes.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes());
        }
        bytes
    })
    .expect("decode");
    backend
        .install_sample(rustel_audio::SampleId(9), Box::new(decoded))
        .expect("install");
    let mut event = OnsetEvent::new(0, 110.0, 1.0, 0.5);
    event.wavetable = Some(rustel_audio::WavetableControls {
        table: rustel_audio::SampleId(9),
        frame_len: 2048,
        voices: 2.0,
        lfo_shape: 0,
        phaserand: 1.0,
        freqspread: 0.0,
        panspread: 0.7,
        position: 0.0,
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
    let pcm_out = render_pcm(&mut backend, 48_000, 12_000, &[event]).expect("render");
    let l: Vec<f32> = pcm_out.iter().step_by(2).copied().collect();
    let r: Vec<f32> = pcm_out.iter().skip(1).step_by(2).copied().collect();
    let side: f64 = l
        .iter()
        .zip(&r)
        .map(|(a, b)| f64::from(a - b).powi(2))
        .sum();
    let mid: f64 = l
        .iter()
        .zip(&r)
        .map(|(a, b)| f64::from(a + b).powi(2))
        .sum();
    assert!(mid > 0.0, "wavetable must produce sound");
    assert!(
        side / mid > 0.05,
        "two unison voices at random phases with panspread 0.7 must be wide: side/mid {}",
        side / mid
    );
}

/// The wavetable begin gate: a voice writes nothing and does not advance its
/// phase until the first context quantum strictly after its onset.
#[test]
fn a_wavetable_voice_is_silent_until_the_quantum_after_its_onset() {
    let frame_len = 2048usize;
    let mut backend = ScalarBackend::prepared(48_000, 8).expect("backend");
    let decoded = rustel_audio::decode_wav(&{
        let pcm: Vec<f32> = (0..frame_len)
            .map(|i| (i as f32 / frame_len as f32) * 2.0 - 1.0)
            .collect();
        let mut bytes: Vec<u8> = Vec::new();
        let data_len = (pcm.len() * 2) as u32;
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&96_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for s in &pcm {
            bytes.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes());
        }
        bytes
    })
    .expect("decode");
    backend
        .install_sample(rustel_audio::SampleId(11), Box::new(decoded))
        .expect("install");
    // Onset at frame 0, so the gate holds for exactly the first quantum.
    let mut event = OnsetEvent::new(0, 220.0, 1.0, 0.3);
    event.wavetable = Some(rustel_audio::WavetableControls {
        table: rustel_audio::SampleId(11),
        frame_len: 2048,
        voices: 1.0,
        lfo_shape: 0,
        phaserand: 0.0,
        freqspread: 0.0,
        panspread: 0.0,
        position: 0.5,
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
    let pcm_out = render_pcm(&mut backend, 48_000, 12_000, &[event]).expect("render");
    assert!(
        pcm_out[..256].iter().all(|s| *s == 0.0),
        "the onset quantum must be silent - the wavetable started early"
    );
    let peak = pcm_out.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    assert!(
        peak > 1e-3,
        "wavetable must sound after the gate: peak {peak}"
    );
}

#[test]
fn every_duck_trigger_dips_in_an_offline_render() {
    // Offline renders intake all events up front; arming ducks at intake kept
    // only the last automation
    // generations, so a 30 s render pumped once near the end instead of on
    // every kick. Each trigger must dip, in order.
    let sr = 48_000u32;
    let tone =
        OnsetEvent::new(0, 220.0, 1.0, 2.0).with_controls(rustel_audio::OscillatorControls {
            limit: None,
            noise: 0.0,
            bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
            bus: None,
            busgain: 1.0,
            channels: None,
            envelope: rustel_audio::Envelope {
                attack_secs: 0.001,
                decay_secs: 0.001,
                sustain: 1.0,
                release_secs: 0.01,
            },
            orbit: 2,
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
            ..Default::default()
        });
    let mut events = vec![tone];
    for k in 0..3u64 {
        let mut kick =
            OnsetEvent::new(0, 220.0, 0.0, 0.01).with_controls(rustel_audio::OscillatorControls {
                limit: None,
                noise: 0.0,
                bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
                bus: None,
                busgain: 1.0,
                channels: None,
                duck: Some(rustel_audio::DuckControls {
                    targets: [
                        Some(rustel_audio::DuckTarget {
                            orbit: 2,
                            onset_secs: 0.0,
                            attack_secs: 0.1,
                            depth: 1.0,
                        }),
                        None,
                        None,
                        None,
                    ],
                }),
                ..Default::default()
            });
        kick.onset_frame = 24_000 + k * 24_000; // 0.5 s, 1.0 s, 1.5 s
        events.push(kick);
    }
    let pcm = render_pcm(&mut ScalarBackend::new(), sr, 96_000, &events).expect("render");
    let rms = |from: usize, to: usize| {
        let slice = &pcm[from * 2..to * 2];
        (slice
            .iter()
            .map(|s| f64::from(*s) * f64::from(*s))
            .sum::<f64>()
            / slice.len() as f64)
            .sqrt()
    };
    let steady = rms(20_000, 23_000);
    for k in 0..3usize {
        let onset = 24_000 + k * 24_000;
        let dipped = rms(onset + 50, onset + 400);
        let recovered = rms(onset + 8_000, onset + 12_000);
        assert!(
            dipped < steady * 0.1,
            "kick {k} must dip: steady {steady:.4}, dipped {dipped:.4}"
        );
        assert!(
            recovered > steady * 0.8,
            "kick {k} must recover: steady {steady:.4}, recovered {recovered:.4}"
        );
    }
}

/// Ducks sharing an arm frame arm in admission order, so the last-admitted
/// depth sets the orbit's dip floor, including after an earlier duck has left
/// the queue ahead of them.
#[test]
fn same_frame_ducks_leave_the_last_admitted_depth_in_charge() {
    let ducker = |onset_frame: u64, depth: f32, attack_secs: f32| {
        let mut event = OnsetEvent::new(1, 220.0, 0.0, 0.01).with_controls(OscillatorControls {
            duck: Some(rustel_audio::DuckControls {
                targets: [
                    Some(rustel_audio::DuckTarget {
                        orbit: 2,
                        onset_secs: 0.0,
                        attack_secs,
                        depth,
                    }),
                    None,
                    None,
                    None,
                ],
            }),
            ..Default::default()
        });
        event.onset_frame = onset_frame;
        event
    };
    let tone = OnsetEvent::new(0, 220.0, 1.0, 3.0).with_controls(OscillatorControls {
        envelope: Envelope {
            attack_secs: 0.001,
            decay_secs: 0.001,
            sustain: 1.0,
            release_secs: 0.01,
        },
        orbit: 2,
        ..Default::default()
    });
    // A two-second recovery keeps the dip bottom nearly flat, so a window
    // just after the shared onset reads the winning floor.
    let shared = 24_000u64;
    let cases = [
        (
            "three at one frame",
            [
                ducker(shared, 0.9, 2.0),
                ducker(shared, 0.5, 2.0),
                ducker(shared, 0.2, 2.0),
            ],
        ),
        (
            "two after an earlier duck",
            [
                ducker(12_000, 1.0, 0.1),
                ducker(shared, 0.5, 2.0),
                ducker(shared, 0.2, 2.0),
            ],
        ),
    ];
    let last_admitted = 1.0 - 0.2f64.sqrt();
    for (case, ducks) in cases {
        let mut events = vec![tone];
        events.extend(ducks);
        let pcm = render_pcm(&mut ScalarBackend::new(), 48_000, 124_000, &events).expect("render");
        let rms = |from: usize, to: usize| {
            let slice = &pcm[from * 2..to * 2];
            (slice
                .iter()
                .map(|s| f64::from(*s) * f64::from(*s))
                .sum::<f64>()
                / slice.len() as f64)
                .sqrt()
        };
        let steady = rms(20_000, 23_000);
        let ratio = rms(24_200, 26_200) / steady;
        assert!(
            (ratio - last_admitted).abs() < 0.06,
            "{case}: expected the last-admitted floor ~{last_admitted:.3}, got {ratio:.3}"
        );
        let recovered = rms(120_500, 123_000);
        assert!(
            recovered > steady * 0.8,
            "{case}: duck must recover: steady {steady:.4}, recovered {recovered:.4}"
        );
    }
}

#[test]
fn synth_sources_produce_their_characteristic_output() {
    // sbd: energetic pitched click that decays within `decay`.
    let mut sbd = OnsetEvent::new(0, 55.0, 1.0, 0.5);
    sbd.synth = Some(rustel_audio::SynthSource::Sbd {
        decay_secs: 0.2,
        pdecay_secs: 0.3,
        penv_semitones: 36.0,
        stop_secs: 0.21,
    });
    let pcm = render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[sbd]).expect("sbd");
    let rms = |pcm: &[f32], from: usize, to: usize| {
        let s = &pcm[from * 2..to * 2];
        (s.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>() / s.len() as f64).sqrt()
    };
    assert!(rms(&pcm, 0, 2_000) > 0.05, "sbd must thump");
    assert!(
        rms(&pcm, 12_000, 14_000) < rms(&pcm, 0, 2_000) * 0.05,
        "sbd must decay away"
    );

    // supersaw: sustained, stereo-wide.
    let mut saw = OnsetEvent::new(0, 110.0, 1.0, 0.4);
    saw.synth = Some(rustel_audio::SynthSource::Supersaw {
        voices: 5.0,
        freqspread: 0.6,
        panspread: 0.6,
    });
    let pcm = render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[saw]).expect("supersaw");
    let l: Vec<f32> = pcm.iter().step_by(2).copied().collect();
    let r: Vec<f32> = pcm.iter().skip(1).step_by(2).copied().collect();
    let side: f64 = l
        .iter()
        .zip(&r)
        .map(|(a, b)| f64::from(a - b).powi(2))
        .sum();
    let mid: f64 = l
        .iter()
        .zip(&r)
        .map(|(a, b)| f64::from(a + b).powi(2))
        .sum();
    assert!(
        mid > 0.0 && side / mid > 0.02,
        "supersaw must be wide: {}",
        side / mid
    );

    // pulse: audible tone; FM changes the waveform measurably.
    let mut pulse = OnsetEvent::new(0, 220.0, 1.0, 0.4);
    pulse.synth = Some(rustel_audio::SynthSource::Pulse {
        pulsewidth: 0.5,
        width_lfo: None,
    });
    let plain = render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[pulse]).expect("pulse");
    assert!(rms(&plain, 2_000, 10_000) > 0.01, "pulse must sound");
    let mut modulated = pulse;
    modulated.controls.fm = Some(fm_chain(&[Some((2.0, 2.04))]));
    let fm_pcm =
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[modulated]).expect("fm pulse");
    let diff: f64 = plain
        .iter()
        .zip(&fm_pcm)
        .map(|(a, b)| f64::from(a - b).powi(2))
        .sum();
    assert!(diff > 1.0, "fm must change the pulse output: {diff}");
}

#[test]
fn supersaw_source_modulation_keeps_using_dynamic_lane_values() {
    use rustel_audio::{LfoMod, ModTarget, SynthSource};

    let render = |target: Option<ModTarget>| {
        let mut event = OnsetEvent::new(0, 110.0, 0.8, 0.4);
        event.synth = Some(SynthSource::Supersaw {
            voices: 8.0,
            freqspread: 0.35,
            panspread: 0.7,
        });
        event.controls.lfos[0] = target.map(|target| LfoMod {
            fxi: None,
            target,
            frequency_hz: 0.0,
            phase0: 0.25,
            depth: 0.2,
            dcoffset: 0.0,
            skew: 0.5,
            curve: 1.0,
            shape: 4,
            min: 0.0,
            max: 0.2,
            param_base: 1.0,
            filter: None,
            id: None,
        });
        render_pcm(&mut ScalarBackend::new(), 48_000, 4_096, &[event]).expect("render")
    };

    let plain = render(None);
    for target in [ModTarget::SourceFreqspread, ModTarget::SourcePanspread] {
        assert_ne!(plain, render(Some(target)), "{target:?} changed nothing");
    }
}

#[test]
fn a_supersaw_spread_modulator_above_one_stays_finite() {
    let mut event = OnsetEvent::new(0, 110.0, 0.8, 0.4);
    event.synth = Some(rustel_audio::SynthSource::Supersaw {
        voices: 8.0,
        freqspread: 0.35,
        panspread: 0.7,
    });
    // The LFO holds 2, so the spread param sums to 2.7.
    event.controls.lfos[0] = Some(held_lfo(
        rustel_audio::ModTarget::SourcePanspread,
        0.7,
        1.0,
        2.0,
        1.0,
    ));
    let pcm = render_pcm(&mut ScalarBackend::new(), 48_000, 4_096, &[event]).expect("render");
    assert!(
        pcm.iter().all(|sample| sample.is_finite()),
        "a NaN reached the output"
    );
}

#[test]
fn supersaw_waits_for_the_begin_gate_and_holds_its_phases() {
    let render = |begin_secs: f32| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.worklet_begin_secs = begin_secs;
        let event = OnsetEvent::new(1_000, 110.0, 0.8, 0.5)
            .with_controls(c)
            .with_optional_synth(Some(rustel_audio::SynthSource::Supersaw {
                voices: 5.0,
                freqspread: 0.6,
                panspread: 0.6,
            }));
        render_pcm(&mut ScalarBackend::new(), 48_000, 4_096, &[event]).expect("render")
    };
    // A begin at frame 1000 opens the gate on quantum 1024.
    let gated = render((1_000.0 / 48_000.0_f64) as f32);
    assert!(
        gated[..1_024 * 2].iter().all(|s| *s == 0.0),
        "supersaw sounded before its begin gate opened"
    );
    // A begin of zero opens the gate on the onset frame. After the attack
    // and decay, the gated voice plays the same samples 24 frames later.
    let open = render(0.0);
    assert!(
        gated[1_224 * 2..] == open[1_200 * 2..(4_096 - 24) * 2],
        "supersaw phases advanced while the gate was closed"
    );
}

#[test]
fn sbd_replays_the_cached_noise_attack_for_each_hit() {
    let sr = 48_000;
    // Far enough apart that the first 510 ms voice has ended.
    let spacing = 28_800;
    let make_sbd = |onset_frame| {
        let mut event = OnsetEvent::new(onset_frame, 43.653_53, 1.0, 0.5);
        event.synth = Some(rustel_audio::SynthSource::Sbd {
            decay_secs: 0.5,
            pdecay_secs: 0.5,
            penv_semitones: 36.0,
            stop_secs: 0.51,
        });
        event
    };
    let pcm = render_pcm(
        &mut ScalarBackend::new(),
        sr,
        spacing * 2,
        &[make_sbd(0), make_sbd(spacing as u64)],
    )
    .expect("render repeated sbd hits");

    // Every SBD voice starts the session's cached brown-noise stream at offset
    // zero. The pitched body is deterministic too, so attacks at whole-frame
    // onsets must repeat inside one render.
    let attack_frames = (sr as f32 * 0.025) as usize;
    let max_delta = (0..attack_frames)
        .flat_map(|frame| {
            let first = frame * 2;
            let second = (spacing + frame) * 2;
            [
                (pcm[first] - pcm[second]).abs(),
                (pcm[first + 1] - pcm[second + 1]).abs(),
            ]
        })
        .fold(0.0_f32, f32::max);
    assert!(
        max_delta < 1e-5,
        "sbd attacks must replay the cached noise buffer; max delta was {max_delta}"
    );
}

#[test]
fn live_sbd_carries_the_waveshaper_prehistory_into_its_attack() {
    use std::sync::atomic::{AtomicBool, AtomicU64};

    const SAMPLE_RATE: u32 = 48_000;
    const GAIN: f32 = 0.8;
    const ONSET: usize = 9_600;
    const GRAPH_START: usize = ONSET - SAMPLE_RATE as usize / 10;
    const ATTACK_FRAMES: usize = 512;
    let synth = rustel_audio::SynthSource::Sbd {
        decay_secs: 0.5,
        pdecay_secs: 0.5,
        penv_semitones: 36.0,
        stop_secs: 0.51,
    };
    let ring = rustel_audio::Ring::new(4);
    assert!(ring.push(rustel_audio::AudioEvent {
        onset_id: 1,
        generation: 1,
        ui_visuals: 0,
        target_frame: ONSET as u64,
        onset_lead: 0.0,
        freq_hz: 43.653_53,
        gain: GAIN,
        duration_secs: 2.0,
        controls: OscillatorControls::default(),
        sample: None,
        wavetable: None,
        synth: Some(synth),
        cut: None,
    }));
    let generation = AtomicU64::new(1);
    let takeover = AtomicU64::new(0);
    let line_arm = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let mut backend = rustel_audio::LiveScalarBackend::new(SAMPLE_RATE, 4).expect("live backend");
    let frames = ONSET + ATTACK_FRAMES;
    let mut live = vec![0.0f32; frames * 2];
    let mut offset = 0;
    while offset < frames {
        let count = (frames - offset).min(128);
        backend.process_block_with(
            &mut live[offset * 2..(offset + count) * 2],
            count,
            offset as u64,
            &ring,
            LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &AtomicU64::new(0),
                line_arm: &line_arm,
            },
            &stopped,
        );
        offset += count;
    }
    let left: Vec<f32> = live.into_iter().step_by(2).collect();

    assert!(left[..GRAPH_START].iter().all(|&sample| sample == 0.0));
    let bias = left[GRAPH_START];
    assert!(bias < 0.0);
    assert_eq!((bias * 32_768.0).round() as i32, -1);
    assert!(
        left[GRAPH_START..ONSET]
            .iter()
            .all(|&sample| sample == bias)
    );

    let mut immediate = OnsetEvent::new(0, 43.653_53, GAIN, 2.0);
    immediate.synth = Some(synth);
    let attack = render_pcm(
        &mut ScalarBackend::new(),
        SAMPLE_RATE,
        ATTACK_FRAMES,
        &[immediate],
    )
    .expect("immediate SBD");
    let attack_left: Vec<f32> = attack.into_iter().step_by(2).collect();
    assert_eq!(&left[ONSET..], attack_left.as_slice());
}

/// `fmi{k}` is the link from operator k to operator k-1. `fmi3` without
/// `fmi2` reaches nothing, so the note sounds as if `fmi3` were absent.
/// strudel.cc's `applyFM` skips the connection (`if (!amt) continue`).
#[test]
fn a_gap_in_the_fm_chain_severs_it_rather_than_being_stepped_over() {
    let base = |stages: &[Option<(f32, f32)>]| {
        let mut event = OnsetEvent::new(0, 220.0, 1.0, 0.4);
        event.controls.fm = Some(fm_chain(stages));
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };
    let carrier = Some((3.0, 1.0));

    let plain = base(&[carrier]);
    // fmi3 with no fmi2 - operator 3 present, the link below it missing.
    let far = base(&[carrier, None, Some((5.0, 2.0))]);
    // fmi2 present - a real chain, which MUST change the sound.
    let near = base(&[carrier, Some((5.0, 2.0))]);

    let distance = |a: &[f32], b: &[f32]| -> f64 {
        a.iter().zip(b).map(|(a, b)| f64::from(a - b).abs()).sum()
    };
    assert!(
        distance(&plain, &far) < 1e-6,
        "an operator whose link is missing must be inaudible, but it moved \
         the output by {}",
        distance(&plain, &far)
    );
    assert!(
        distance(&plain, &near) > 1.0,
        "a linked operator must be audible, but it changed nothing: {}",
        distance(&plain, &near)
    );
}

/// A duck trigger that arrives after its 10 ms hold point anchors the dip at
/// the current frame, as upstream's webAudioTimeout callback does
/// (t0 = max(t, now)). The orbit gain ramps from there and does not step.
#[test]
fn late_duck_trigger_ramps_from_now_instead_of_stepping() {
    use rustel_audio::AudioBackend;
    let sr = 48_000u32;
    let mut backend = ScalarBackend::new();
    backend.init(sr).expect("init");
    backend.reset();
    // Steady 220 Hz tone on orbit 2 from frame 0 (phase 0 at frame 24_000:
    // 220/48_000 * 24_000 = 110 whole cycles - deterministic envelope).
    let mut tone = OnsetEvent::new(0, 220.0, 1.0, 1.0).with_controls(OscillatorControls {
        limit: None,
        noise: 0.0,
        bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
        bus: None,
        busgain: 1.0,
        channels: None,
        orbit: 2,
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
        ..controls(Waveform::Sine, None)
    });
    tone.onset_frame = 0;
    backend.note(tone);
    let mut pcm = vec![0.0f32; 34_000 * 2];
    let mut cursor = 0usize;
    let render_to = |backend: &mut ScalarBackend, pcm: &mut [f32], to: usize, from: &mut usize| {
        while *from < to {
            let n = (to - *from).min(128);
            backend.process_block(&mut pcm[*from * 2..(*from + n) * 2], n);
            *from += n;
        }
    };
    render_to(&mut backend, &mut pcm, 24_000, &mut cursor);

    // The duck event arrives ONLY NOW (live intake), onset 50 frames ahead -
    // its hold point (onset − 441) is ~391 frames in the rendered past.
    let mut ducker = OnsetEvent::new(1, 220.0, 0.0, 0.01).with_controls(OscillatorControls {
        limit: None,
        noise: 0.0,
        bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
        bus: None,
        busgain: 1.0,
        channels: None,
        duck: Some(rustel_audio::DuckControls {
            targets: [
                Some(rustel_audio::DuckTarget {
                    orbit: 2,
                    onset_secs: 0.0,
                    attack_secs: 0.1,
                    depth: 1.0,
                }),
                None,
                None,
                None,
            ],
        }),
        ..controls(Waveform::Sine, None)
    });
    ducker.onset_frame = 24_050;
    backend.note(ducker);
    render_to(&mut backend, &mut pcm, 34_000, &mut cursor);

    let peak = |from: usize, to: usize| {
        pcm[from * 2..to * 2]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
    };
    // Continuity relative to the pre-dip level: the ramp starts from the
    // CURRENT value, so the first 30 frames still carry ~11% of it (gain x
    // |sin| maximum along the compressed ramp). The stepping bug landed the
    // gain at ~0.017 instantly: ~1.3% here.
    let before = peak(23_500, 24_000);
    let entering = peak(24_000, 24_030);
    assert!(
        entering > before * 0.06,
        "late duck must ramp from the current value, not step: \
         before {before:.4}, entering {entering:.4}"
    );
    let dipped = peak(24_100, 24_500);
    let recovered = peak(30_000, 34_000);
    assert!(
        dipped < before * 0.1,
        "late duck must still dip: before {before:.4}, dipped {dipped:.4}"
    );
    assert!(
        recovered > before * 0.8,
        "late duck must still recover: before {before:.4}, recovered {recovered:.4}"
    );
}

/// The live consumer arms the sidechain at ring intake, a schedule lead early.
/// The 10 ms pre-hold ramp renders in full and bottoms out at the onset, as
/// strudel.cc's webAudioTimeout contract requires.
#[test]
fn live_consumer_arms_the_duck_a_full_hold_ramp_early() {
    use std::sync::atomic::{AtomicBool, AtomicU64};
    let sr = 48_000u32;
    let ring = rustel_audio::Ring::new(16);
    let generation = AtomicU64::new(1);
    let takeover = AtomicU64::new(0);
    let line_arm = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let event = |onset_id: u64,
                 target_frame: u64,
                 freq: f32,
                 gain: f32,
                 dur: f32,
                 ctl: OscillatorControls| {
        rustel_audio::AudioEvent {
            onset_id,
            generation: 1,
            target_frame,
            onset_lead: 0.0,
            freq_hz: freq,
            gain,
            duration_secs: dur,
            ui_visuals: 0,
            controls: ctl,
            sample: None,
            synth: None,
            wavetable: None,
            cut: None,
        }
    };
    // Steady tone on orbit 2; a near-silent duck trigger at frame 24_000,
    // pushed a full lead ahead like the producer does.
    assert!(ring.push(event(
        0,
        0,
        220.0,
        1.0,
        1.0,
        OscillatorControls {
            limit: None,
            noise: 0.0,
            bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
            bus: None,
            busgain: 1.0,
            channels: None,
            orbit: 2,
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
            ..controls(Waveform::Sine, None)
        }
    )));
    assert!(ring.push(event(
        1,
        24_000,
        220.0,
        0.0,
        0.01,
        OscillatorControls {
            limit: None,
            noise: 0.0,
            bus_mods: [None; rustel_audio::MAX_VOICE_MODS],
            bus: None,
            busgain: 1.0,
            channels: None,
            duck: Some(rustel_audio::DuckControls {
                targets: [
                    Some(rustel_audio::DuckTarget {
                        orbit: 2,
                        onset_secs: 0.0,
                        attack_secs: 0.1,
                        depth: 1.0,
                    }),
                    None,
                    None,
                    None,
                ],
            }),
            ..controls(Waveform::Sine, None)
        }
    )));

    let mut backend = rustel_audio::LiveScalarBackend::new(sr, 16).expect("live backend");
    let total = 34_000usize;
    let mut pcm = vec![0.0f32; total * 2];
    let mut cursor = 0usize;
    while cursor < total {
        let n = (total - cursor).min(128);
        backend.process_block_with(
            &mut pcm[cursor * 2..(cursor + n) * 2],
            n,
            cursor as u64,
            &ring,
            LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &AtomicU64::new(0),
                line_arm: &line_arm,
            },
            &stopped,
        );
        cursor += n;
    }

    let peak = |from: usize, to: usize| {
        pcm[from * 2..to * 2]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
    };
    let before = peak(22_500, 23_400);
    // Mid-ramp, ~300 frames before the onset: the exponential is already
    // well below unity but far above the dip floor. Admission-time arming
    // kept this window at full level.
    let ramp_mid = peak(23_650, 23_780);
    assert!(
        ramp_mid < before * 0.7 && ramp_mid > before * 0.05,
        "the 10 ms pre-hold must ramp before the onset: \
         before {before:.4}, mid-ramp {ramp_mid:.4}"
    );
    // Dip floor lands AT the onset and recovery completes after the attack.
    let bottom = peak(24_010, 24_150);
    let recovered = peak(30_000, 34_000);
    assert!(
        bottom < before * 0.05,
        "dip must bottom at the onset: before {before:.4}, bottom {bottom:.4}"
    );
    assert!(
        recovered > before * 0.8,
        "duck must recover: before {before:.4}, recovered {recovered:.4}"
    );
}

#[test]
fn lfo_modulator_sweeps_the_filter_after_the_first_quantum() {
    use rustel_audio::{LfoMod, ModTarget};
    let base = |lfo: Option<LfoMod>| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.filters = FilterControls {
            lowpass: Some(rustel_audio::StaticBiquad {
                frequency_hz: 400.0,
                q: 1.0,
            }),
            ..FilterControls::default()
        };
        c.lfos[0] = lfo;
        // onset at frame 1000 so the worklet begin-gate (context-grid
        // quantum strictly after onset) is exercised off the origin. The
        // gate reads the note's begin in SECONDS, narrowed to f32 the way
        // the AudioParam narrows it, so the voice has to carry it.
        c.worklet_begin_secs = (1_000.0 / 48_000.0_f64) as f32;
        let event = OnsetEvent::new(1_000, 110.0, 0.8, 0.5).with_controls(c);
        let mut backend = ScalarBackend::new();
        render_pcm(&mut backend, 48_000, 48_000, &[event]).expect("render")
    };
    // deep 4Hz sweep: ±4×400Hz around the 400Hz base, tri shape
    let lfo = LfoMod {
        fxi: None,
        target: ModTarget::LowpassFreq,
        frequency_hz: 4.0,
        phase0: 0.0,
        depth: 1_600.0,
        dcoffset: -0.5,
        skew: 0.5,
        curve: 1.0,
        shape: 0,
        min: 20.0 - 400.0,
        max: 24_000.0 - 400.0,
        param_base: 400.0,
        filter: None,
        id: None,
    };
    let modulated = base(Some(lfo));
    let flat = base(None);
    // Identical through the onset quantum: modulators only engage at the
    // first 128-frame grid boundary strictly after frame 1000 => 1024.
    let gate_end = 1_024 * 2; // stereo interleaved
    assert_eq!(&modulated[..gate_end], &flat[..gate_end]);
    // and audibly different afterwards
    let diff: f64 = modulated[gate_end..]
        .iter()
        .zip(&flat[gate_end..])
        .map(|(a, b)| f64::from(a - b).abs())
        .sum();
    assert!(diff > 1.0, "LFO produced no modulation, |diff| = {diff}");
}

/// A stopped ramp at phase 0. Its sample is `(dcoffset * depth).powf(curve)`.
fn held_lfo(
    target: rustel_audio::ModTarget,
    param_base: f32,
    dcoffset: f32,
    depth: f32,
    curve: f32,
) -> rustel_audio::LfoMod {
    rustel_audio::LfoMod {
        fxi: None,
        target,
        frequency_hz: 0.0,
        phase0: 0.0,
        depth,
        dcoffset,
        skew: 0.5,
        curve,
        shape: 2,
        min: -1e9,
        max: 1e9,
        param_base,
        filter: None,
        id: None,
    }
}

/// A negative value under a fractional curve is NaN.
fn nan_lfo(target: rustel_audio::ModTarget, param_base: f32) -> rustel_audio::LfoMod {
    held_lfo(target, param_base, -0.5, 1.0, 0.5)
}

#[test]
fn a_nan_lfo_sample_gives_the_target_its_audio_param_default() {
    use rustel_audio::{FilterStages, ModTarget};
    let render = |stages, lfos: [Option<rustel_audio::LfoMod>; 2]| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.filters = FilterControls {
            lowpass: Some(rustel_audio::StaticBiquad {
                frequency_hz: 800.0,
                q: 1.0,
            }),
            stages,
            ..FilterControls::default()
        };
        c.lfos[..2].copy_from_slice(&lfos);
        let event = OnsetEvent::new(0, 110.0, 0.5, 0.5).with_controls(c);
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };
    for (target, stages, base, default) in [
        (ModTarget::LowpassFreq, FilterStages::One, 800.0, 350.0),
        (ModTarget::LowpassFreq, FilterStages::Ladder, 800.0, 500.0),
        (ModTarget::Gain, FilterStages::One, 0.5, 1.0),
    ] {
        let nan = render(stages, [Some(nan_lfo(target, base)), None]);
        // This LFO holds `default - base` without a NaN.
        let held = held_lfo(target, base, 1.0, default - base, 1.0);
        assert!(
            nan.iter().all(|sample| sample.is_finite()),
            "{target:?} let a NaN reach the output"
        );
        assert!(
            nan == render(stages, [Some(held), None]),
            "{target:?} with {stages:?} did not take its default"
        );
    }
    // The NaN sum hides a second modulator on the filter frequency.
    let target = ModTarget::LowpassFreq;
    let nan = nan_lfo(target, 800.0);
    let second = held_lfo(target, 800.0, 1.0, 200.0, 1.0);
    let held = held_lfo(target, 800.0, 1.0, 350.0 - 800.0, 1.0);
    assert!(
        render(FilterStages::One, [Some(nan), Some(second)])
            == render(FilterStages::One, [Some(held), None]),
        "the second modulator moved the default"
    );
}

#[test]
fn a_nan_lfo_sample_is_found_in_f64() {
    use rustel_audio::{LfoMod, ModTarget};
    let render = |lfo| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.lfos[0] = Some(lfo);
        let event = OnsetEvent::new(0, 110.0, 0.5, 0.5).with_controls(c);
        render_pcm(&mut ScalarBackend::new(), 48_000, 4_096, &[event]).expect("render")
    };
    // 3000 steps of 4 Hz give a triangle value slightly under 0.5 in f64.
    // The f32 value is 0.5.
    let nan = render(LfoMod {
        frequency_hz: 4.0,
        shape: 0,
        ..nan_lfo(ModTarget::Gain, 0.5)
    });
    let held = render(held_lfo(ModTarget::Gain, 0.5, 1.0, 1.0 - 0.5, 1.0));
    // The LFO starts on quantum 128.
    let sample = (128 + 3_000) * 2;
    assert!(held[sample] != 0.0);
    assert_eq!(nan[sample], held[sample]);
}

#[test]
fn nan_feedback_modulators_on_one_orbit_keep_the_delay_bounded() {
    use rustel_audio::{DelayControls, ModTarget};
    let mut c = controls(Waveform::Sawtooth, None);
    c.delay = Some(DelayControls {
        wet: 1.0,
        time_secs: 0.01,
        feedback: 0.5,
    });
    c.lfos[0] = Some(nan_lfo(ModTarget::DelayFeedback, 0.5));
    // Each voice adds `1 - 0.5` to the feedback of the shared delay.
    let events = [110.0, 165.0, 220.0].map(|hz| OnsetEvent::new(0, hz, 0.5, 0.5).with_controls(c));
    let pcm = render_pcm(&mut ScalarBackend::new(), 48_000, 48_000, &events).expect("render");
    let peak = pcm.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    assert!(peak < 100.0, "the orbit feedback went above 1, peak {peak}");
}

#[test]
fn a_nan_tremolodepth_modulator_gives_the_gain_its_default() {
    use rustel_audio::{ModTarget, TremoloControls};
    let render = |tremolo: bool| {
        let mut c = controls(Waveform::Sawtooth, None);
        if tremolo {
            c.tremolo = Some(TremoloControls {
                frequency_hz: 4.0,
                depth: 0.5,
                skew: 1.0,
                shape: 0,
                phase_offset: 0.0,
                time_secs: 0.0,
            });
            c.lfos[0] = Some(nan_lfo(ModTarget::TremoloDepth, 0.5));
        }
        let event = OnsetEvent::new(0, 110.0, 0.8, 0.5).with_controls(c);
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };
    let pcm = render(true);
    assert!(
        pcm.iter().all(|sample| sample.is_finite()),
        "a NaN reached the output"
    );
    // The LFO starts on quantum 128.
    assert!(
        pcm[128 * 2..] == render(false)[128 * 2..],
        "the gain did not take its default"
    );
}

#[test]
fn tremolo_gates_the_onset_quantum_then_pumps() {
    use rustel_audio::TremoloControls;
    let mut c = controls(Waveform::Sawtooth, None);
    c.tremolo = Some(TremoloControls {
        frequency_hz: 8.0,
        depth: 1.0,
        skew: 1.0,
        shape: 0,
        phase_offset: 0.0,
        time_secs: 0.0,
    });
    let event = OnsetEvent::new(0, 110.0, 0.8, 0.5).with_controls(c);
    let mut backend = ScalarBackend::new();
    let pcm = render_pcm(&mut backend, 48_000, 24_000, &[event]).expect("render");
    // depth-1 tremolo holds gain 0 until the first 128-frame quantum
    // strictly after the onset (worklet begin gate) - silence first.
    assert!(
        pcm[..256].iter().all(|s| *s == 0.0),
        "onset quantum not gated"
    );
    let peak = pcm.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    assert!(peak > 1e-3, "tremolo silenced the whole note, peak {peak}");
}

/// 375 Hz at 48 kHz steps the phase by 2^-7, so the walk reaches exactly 1.0.
/// The default tremolo shape (tri, skew 1) must not divide by `1 - skew` = 0
/// there.
#[test]
fn default_tremolo_at_a_dyadic_rate_stays_finite() {
    use rustel_audio::TremoloControls;
    let mut c = controls(Waveform::Sawtooth, None);
    c.tremolo = Some(TremoloControls {
        frequency_hz: 375.0, // 48_000 / 128: the walk lands on phase 1.0 exactly
        depth: 0.8,
        skew: 1.0,
        shape: 0,
        phase_offset: 0.0,
        time_secs: 0.0,
    });
    let event = OnsetEvent::new(0, 110.0, 0.8, 0.5).with_controls(c);
    let mut backend = ScalarBackend::new();
    let pcm = render_pcm(&mut backend, 48_000, 24_000, &[event]).expect("render");
    assert!(
        pcm.iter().all(|sample| sample.is_finite()),
        "tremolo produced non-finite samples"
    );
    let peak = pcm.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    assert!(peak > 1e-3, "tremolo silenced the whole note, peak {peak}");
}

/// A negative depth drives the LFO's 1.5 exponent to NaN. The tremolo gain
/// is then 1.0, on the main chain and in an `.FX()` stage.
#[test]
fn negative_depth_tremolo_stays_finite_at_unity_gain() {
    use rustel_audio::TremoloControls;
    let negative = Some(TremoloControls {
        frequency_hz: 4.0,
        depth: -1.0,
        skew: 1.0,
        shape: 0,
        phase_offset: 0.0,
        time_secs: 0.0,
    });
    let render = |in_stage: bool, tremolo: Option<TremoloControls>| {
        let mut c = controls(Waveform::Sawtooth, None);
        if in_stage {
            c.fx_stages[0] = Some(rustel_audio::FxStage {
                stretch: None,
                transient: None,
                gain: 1.0,
                filters: FilterControls::default(),
                vowel: None,
                coarse: None,
                crush: None,
                shape: None,
                distort: None,
                tremolo,
                compressor: None,
                pan_x: None,
                phaser: None,
                delay: None,
                dry: 1.0,
                room: None,
            });
        } else {
            c.tremolo = tremolo;
        }
        let event = OnsetEvent::new(0, 110.0, 0.8, 0.5).with_controls(c);
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };
    for (site, in_stage) in [("main chain", false), ("FX stage", true)] {
        let pcm = render(in_stage, negative);
        let bad = pcm.iter().filter(|s| !s.is_finite()).count();
        assert_eq!(
            bad,
            0,
            "{site}: {bad} of {} samples are not finite",
            pcm.len()
        );
        // The LFO starts at frame 128 with a zero sample. Each later LFO
        // sample is NaN, so the voice matches a voice with no tremolo.
        let from = 129 * 2; // stereo interleaved
        assert!(
            pcm[from..] == render(in_stage, None)[from..],
            "{site}: the tremolo gain is not 1.0"
        );
    }
}

#[test]
fn phaser_changes_the_spectrum_over_time() {
    use rustel_audio::PhaserControls;
    let base = |phaser: Option<PhaserControls>| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.phaser = phaser;
        let event = OnsetEvent::new(0, 110.0, 0.8, 1.0).with_controls(c);
        let mut backend = ScalarBackend::new();
        render_pcm(&mut backend, 48_000, 48_000, &[event]).expect("render")
    };
    let with = base(Some(PhaserControls {
        rate_hz: 2.0,
        depth: 0.9,
        center_hz: 800.0,
        sweep_cents: 3_000.0,
        time_secs: 0.0,
    }));
    let flat = base(None);
    let diff: f64 = with
        .iter()
        .zip(&flat)
        .map(|(a, b)| f64::from(a - b).abs())
        .sum();
    assert!(diff > 1.0, "phaser had no effect, |diff| = {diff}");
}

/// An LFO is anchored to a clock, not to the note: `phase0 =
/// ffrac(time · frequency)`. So the same phaser on the same note sounds
/// different depending on WHEN the note happens.
///
/// A note at time 0 always seeds to 0 regardless of frequency, so both anchors
/// below are non-zero.
#[test]
fn a_phasers_lfo_starts_where_its_clock_says_not_at_zero() {
    use rustel_audio::PhaserControls;
    let at = |time_secs: f32| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.phaser = Some(PhaserControls {
            rate_hz: 2.0,
            depth: 0.9,
            center_hz: 800.0,
            sweep_cents: 3_000.0,
            time_secs,
        });
        let event = OnsetEvent::new(0, 110.0, 0.8, 1.0).with_controls(c);
        let mut backend = ScalarBackend::new();
        render_pcm(&mut backend, 48_000, 24_000, &[event]).expect("render")
    };
    // A quarter of a cycle apart at 2 Hz: phase0 0.5 against 0.75.
    let early = at(0.25);
    let late = at(0.375);
    let diff: f64 = early
        .iter()
        .zip(&late)
        .map(|(a, b)| f64::from(a - b).abs())
        .sum();
    assert!(
        diff > 1.0,
        "the phaser LFO ignored its clock anchor - both onsets gave the same \
         sweep, so phase0 is pinned to 0 instead of ffrac(time·rate); \
         |diff| = {diff}"
    );
}

#[test]
fn ladder_model_lowpasses_and_differs_from_the_biquad() {
    use rustel_audio::FilterStages;
    let render = |stages: FilterStages| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.filters = FilterControls {
            lowpass: Some(rustel_audio::StaticBiquad {
                frequency_hz: 400.0,
                q: 10.0,
            }),
            stages,
            ..FilterControls::default()
        };
        let event = OnsetEvent::new(0, 110.0, 0.8, 0.5).with_controls(c);
        let mut backend = ScalarBackend::new();
        render_pcm(&mut backend, 48_000, 24_000, &[event]).expect("render")
    };
    let ladder = render(FilterStages::Ladder);
    let biquad = render(FilterStages::One);
    let rms = |pcm: &[f32]| {
        (pcm.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt()
    };
    let (l, b) = (rms(&ladder), rms(&biquad));
    assert!(l > 1e-4, "ladder silenced the voice, rms {l}");
    assert!(
        (l - b).abs() / b > 0.05,
        "ladder output is indistinguishable from the biquad: {l} vs {b}"
    );
    // High-frequency content must drop: the ladder is a lowpass.
    let hf = |pcm: &[f32]| {
        pcm.windows(2)
            .map(|w| f64::from(w[1] - w[0]).abs())
            .sum::<f64>()
            / pcm.len() as f64
    };
    let mut open = controls(Waveform::Sawtooth, None);
    open.filters = FilterControls::default();
    let raw = {
        let event = OnsetEvent::new(0, 110.0, 0.8, 0.5).with_controls(open);
        let mut backend = ScalarBackend::new();
        render_pcm(&mut backend, 48_000, 24_000, &[event]).expect("render")
    };
    // q=10 resonance re-boosts mids, so the drop is moderate (~0.6x).
    assert!(
        hf(&ladder) < hf(&raw) * 0.75,
        "ladder did not attenuate highs: {} vs raw {}",
        hf(&ladder),
        hf(&raw)
    );
}

#[test]
fn a_crush_modulator_sweeps_the_bitcrusher_but_only_when_crush_is_set() {
    use rustel_audio::{LfoMod, ModTarget};
    // ±6 bits around a base of 8, slow enough that a quarter second lands
    // well away from the starting depth.
    let lfo = LfoMod {
        fxi: None,
        target: ModTarget::Crush,
        frequency_hz: 4.0,
        phase0: 0.0,
        depth: 6.0,
        dcoffset: -0.5,
        skew: 0.5,
        curve: 1.0,
        shape: 0,
        min: f32::NEG_INFINITY,
        max: f32::INFINITY,
        param_base: 8.0,
        filter: None,
        id: None,
    };
    let render = |crush: Option<f32>, lfo: Option<LfoMod>| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.crush = crush;
        c.lfos[0] = lfo;
        let event = OnsetEvent::new(0, 110.0, 0.8, 0.5).with_controls(c);
        let mut backend = ScalarBackend::new();
        render_pcm(&mut backend, 48_000, 24_000, &[event]).expect("render")
    };
    let sum_abs_diff = |a: &[f32], b: &[f32]| -> f64 {
        a.iter()
            .zip(b)
            .map(|(x, y)| f64::from(x - y).abs())
            .sum::<f64>()
    };

    // With the crusher in the chain the modulator moves it.
    let swept = render(Some(8.0), Some(lfo));
    let fixed = render(Some(8.0), None);
    assert!(
        sum_abs_diff(&swept, &fixed) > 1.0,
        "crush modulator did not move the bitcrusher"
    );

    // Without it there is no node to reach: strudel.cc only builds the crush
    // worklet `if (crush !== undefined)`, so the modulator must be inert
    // rather than conjuring a crusher that the pattern never asked for.
    let no_node = render(None, Some(lfo));
    let clean = render(None, None);
    assert_eq!(
        no_node, clean,
        "a crush modulator with no crush control must not add a bitcrusher"
    );
}

/// The distortion worklets read `parameters.x[0]` once per 128-frame render
/// quantum and hold it, so modulating them is block-rate, not
/// a-rate like the filter params. CoarseProcessor makes that observable: with
/// `coarse` held at C for a block, the sample-and-hold re-anchors at the block
/// start and its segment lengths inside that block are all C (or C's two
/// neighbouring integers when C is fractional). A per-sample sweep of the same
/// LFO would stretch and squeeze segments across the block instead.
#[test]
fn a_coarse_modulator_holds_one_value_for_each_render_quantum() {
    use rustel_audio::{LfoMod, ModTarget};
    let mut c = controls(Waveform::Sawtooth, None);
    c.coarse = Some(8.0);
    // 200 Hz is fast against a 2.67 ms quantum: half an LFO cycle per block,
    // so a per-sample sweep would visibly vary the divisor mid-block.
    c.lfos[0] = Some(LfoMod {
        fxi: None,
        target: ModTarget::Coarse,
        frequency_hz: 200.0,
        phase0: 0.0,
        depth: 6.0,
        dcoffset: -0.5,
        skew: 0.5,
        curve: 1.0,
        shape: 0,
        min: f32::NEG_INFINITY,
        max: f32::INFINITY,
        param_base: 8.0,
        filter: None,
        id: None,
    });
    let event = OnsetEvent::new(0, 110.0, 0.8, 1.0).with_controls(c);
    let mut backend = ScalarBackend::new();
    let pcm = render_pcm(&mut backend, 48_000, 24_000, &[event]).expect("render");

    // Skip the attack/decay ramp so the envelope is at a constant sustain and
    // a held sample stays bit-identical through the output gain.
    let mut checked = 0usize;
    for block in 2..(pcm.len() / 2 / 128) {
        let left: Vec<f32> = (0..128).map(|n| pcm[(block * 128 + n) * 2]).collect();
        let mut lengths = Vec::new();
        let mut run = 1usize;
        for n in 1..left.len() {
            if left[n] == left[n - 1] {
                run += 1;
            } else {
                lengths.push(run);
                run = 1;
            }
        }
        // The trailing run is truncated by the block edge, so drop it; a
        // single run means the divisor swallowed the whole block.
        if lengths.len() < 3 {
            continue;
        }
        let (min, max) = (
            *lengths.iter().min().unwrap(),
            *lengths.iter().max().unwrap(),
        );
        assert!(
            max - min <= 1,
            "block {block}: coarse changed inside the quantum - segment \
             lengths {min}..={max} ({lengths:?})"
        );
        checked += 1;
    }
    assert!(checked > 20, "only {checked} blocks were testable");
}

/// A source starts at a float time, between output samples. `crush` is a
/// quantiser, so a half-sample error can become a whole step. At cps 0.5625
/// a cycle spans 85333.33 frames, so two onsets in three need fractional
/// placement.
#[test]
fn a_sub_sample_onset_shifts_the_source_by_a_fraction_of_a_frame() {
    let render = |lead: f32| {
        let mut c = controls(Waveform::Sine, None);
        c.envelope = Envelope {
            attack_secs: 0.0,
            decay_secs: 0.0,
            sustain: 1.0,
            release_secs: 0.0,
        };
        let event = OnsetEvent::new(480, 100.0, 1.0, 0.2)
            .with_onset_lead(lead)
            .with_controls(c);
        let mut backend = ScalarBackend::new();
        render_pcm(&mut backend, 48_000, 4_800, &[event]).expect("render")
    };
    let none = render(0.0);
    let half = render(0.5);
    let full = render(1.0);

    // A zero lead is exactly today's whole-frame placement.
    assert_eq!(none, render(0.0));
    // Half a frame of lead is audible in the samples but must not move the
    // voice to a different frame: the onset frame itself is unchanged.
    assert_ne!(none, half, "a half-frame lead changed nothing");
    // A 100 Hz sine at 48 kHz advances 0.0075 rad per frame; half a frame of
    // lead puts every sample halfway to its neighbour, so lead 1.0 reproduces
    // the next frame of the un-led render.
    let at = |pcm: &[f32], frame: usize| pcm[frame * 2];
    for frame in 481..2_000 {
        let expected = at(&none, frame + 1);
        let got = at(&full, frame);
        assert!(
            (expected - got).abs() < 2e-4,
            "lead 1.0 should equal the next frame of lead 0.0 at {frame}: \
             {expected} vs {got}"
        );
    }
    // and half a frame lands between the two neighbours
    for frame in 600..1_500 {
        let (a, b) = (at(&none, frame), at(&none, frame + 1));
        let mid = at(&half, frame);
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        assert!(
            mid >= lo - 2e-4 && mid <= hi + 2e-4,
            "half-frame lead must land between neighbours at {frame}: \
             {mid} not in {lo}..={hi}"
        );
    }
}

#[test]
fn live_consumer_preserves_fractional_onset_lead() {
    use std::sync::atomic::{AtomicBool, AtomicU64};

    const SAMPLE_RATE: u32 = 48_000;
    const FRAMES: usize = 2_048;
    let mut oscillator = controls(Waveform::Sine, None);
    oscillator.envelope = Envelope {
        attack_secs: 0.0,
        decay_secs: 0.0,
        sustain: 1.0,
        release_secs: 0.0,
    };
    let onset = OnsetEvent::new(128, 100.0, 1.0, 0.02)
        .with_onset_lead(0.75)
        .with_generation(1)
        .with_controls(oscillator);
    let offline = render_pcm(&mut ScalarBackend::new(), SAMPLE_RATE, FRAMES, &[onset])
        .expect("offline render");

    let ring = rustel_audio::Ring::new(4);
    assert!(ring.push(rustel_audio::AudioEvent {
        onset_id: 1,
        generation: 1,
        ui_visuals: 0,
        target_frame: onset.onset_frame,
        onset_lead: onset.onset_lead,
        freq_hz: onset.freq_hz,
        gain: onset.gain,
        duration_secs: onset.duration_secs,
        controls: onset.controls,
        sample: onset.sample,
        wavetable: onset.wavetable,
        synth: onset.synth,
        cut: None,
    }));
    let generation = AtomicU64::new(1);
    let takeover = AtomicU64::new(0);
    let line_arm = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let mut backend = rustel_audio::LiveScalarBackend::new(SAMPLE_RATE, 4).expect("live backend");
    let mut live = vec![0.0f32; FRAMES * 2];
    let mut offset = 0;
    let mut accepted = 0;
    while offset < FRAMES {
        let frames = (FRAMES - offset).min(128);
        accepted += backend
            .process_block_with(
                &mut live[offset * 2..(offset + frames) * 2],
                frames,
                offset as u64,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &AtomicU64::new(0),
                    line_arm: &line_arm,
                },
                &stopped,
            )
            .accepted;
        offset += frames;
    }

    assert_eq!(accepted, 1);
    assert_eq!(live, offline);
    let rounded = render_pcm(
        &mut ScalarBackend::new(),
        SAMPLE_RATE,
        FRAMES,
        &[onset.with_onset_lead(0.0)],
    )
    .expect("whole-frame render");
    assert_ne!(live, rounded);
}

/// Modulators reach distortion and the two send gains only when that node
/// is in the chain. Distortion is a-rate. Coarse, crush and shape latch at
/// the quantum start.
#[test]
fn distortion_and_send_modulators_move_only_the_node_they_name() {
    use rustel_audio::{DistortControls, LfoMod, ModTarget};
    let lfo = |target: ModTarget, base: f32| LfoMod {
        fxi: None,
        target,
        frequency_hz: 4.0,
        phase0: 0.0,
        depth: base * 0.5,
        dcoffset: -0.5,
        skew: 0.5,
        curve: 1.0,
        shape: 0,
        min: f32::NEG_INFINITY,
        max: f32::INFINITY,
        param_base: base,
        filter: None,
        id: None,
    };
    let render = |with_node: bool, m: Option<LfoMod>| {
        let mut c = controls(Waveform::Sawtooth, None);
        if with_node {
            c.distort = Some(DistortControls {
                amount: 2.0,
                postgain: 0.6,
                algorithm: 0,
            });
        }
        c.lfos[0] = m;
        let event = OnsetEvent::new(0, 110.0, 0.8, 0.5).with_controls(c);
        let mut backend = ScalarBackend::new();
        render_pcm(&mut backend, 48_000, 24_000, &[event]).expect("render")
    };

    for (target, base) in [(ModTarget::Distort, 2.0), (ModTarget::DistortVol, 0.6)] {
        let modulated = render(true, Some(lfo(target, base)));
        let plain = render(true, None);
        assert_ne!(modulated, plain, "{target:?} did not move the distortion");
        // With no distortion in the chain there is no node to reach.
        assert_eq!(
            render(false, Some(lfo(target, base))),
            render(false, None),
            "{target:?} conjured a distortion the pattern never asked for"
        );
    }
}

/// `vowel` names all five formant frequency params. Its a-rate signal must
/// move the bank after the onset quantum and must not create a vowel stage
/// where the pattern has none.
#[test]
fn a_vowel_modulator_moves_the_existing_formant_bank() {
    use rustel_audio::{LfoMod, ModTarget, VowelControls};
    let lfo = LfoMod {
        fxi: None,
        target: ModTarget::VowelFreq,
        frequency_hz: 3.0,
        phase0: 0.0,
        depth: 1_000.0,
        dcoffset: -0.5,
        skew: 0.5,
        curve: 1.0,
        shape: 0,
        min: 20.0 - 660.0,
        max: 24_000.0 - 660.0,
        param_base: 660.0,
        filter: None,
        id: None,
    };
    let render = |with_bank: bool, modulated: bool| {
        let mut c = controls(Waveform::Sawtooth, None);
        if with_bank {
            c.vowel = Some(VowelControls {
                freqs: [660.0, 1_120.0, 2_750.0, 3_000.0, 3_350.0],
                gains: [1.0, 0.5012, 0.0708, 0.0631, 0.0126],
                qs: [80.0, 90.0, 120.0, 130.0, 140.0],
            });
        }
        if modulated {
            c.lfos[0] = Some(lfo);
        }
        let event = OnsetEvent::new(0, 110.0, 0.8, 0.4).with_controls(c);
        render_pcm(&mut ScalarBackend::new(), 48_000, 20_000, &[event]).expect("render")
    };

    let plain = render(true, false);
    let modulated = render(true, true);
    assert_eq!(&plain[..256], &modulated[..256]);
    let diff: f64 = plain[256..]
        .iter()
        .zip(&modulated[256..])
        .map(|(a, b)| f64::from(a - b).abs())
        .sum();
    assert!(
        diff > 1.0,
        "vowel frequency modulation changed nothing: {diff}"
    );
    assert_eq!(
        render(false, true),
        render(false, false),
        "vowel modulation conjured a formant bank"
    );
}

/// The pulse synth's own width LFO is a separate worklet. Its signal moves
/// the a-rate pulsewidth param, while modulators of its rate/depth are held at
/// the quantum boundary where LFOProcessor reads those params.
#[test]
fn pulse_width_lfo_rate_and_depth_modulators_move_that_lfo_only_when_built() {
    use rustel_audio::{LfoMod, ModTarget, PulseWidthLfoControls, SynthSource};
    let width_lfo = PulseWidthLfoControls {
        frequency_hz: 2.0,
        depth: 0.4,
        time_secs: 0.0,
    };
    let render = |with_width_lfo: bool, modulator: Option<LfoMod>| {
        let mut event = OnsetEvent::new(0, 110.0, 0.8, 0.4);
        event.synth = Some(SynthSource::Pulse {
            pulsewidth: 0.5,
            width_lfo: with_width_lfo.then_some(width_lfo),
        });
        event.controls.lfos[0] = modulator;
        render_pcm(&mut ScalarBackend::new(), 48_000, 20_000, &[event]).expect("render")
    };
    let lfo = |target: ModTarget, base: f32, depth: f32| LfoMod {
        fxi: None,
        target,
        frequency_hz: 3.0,
        phase0: 0.0,
        depth,
        dcoffset: -0.5,
        skew: 0.5,
        curve: 1.0,
        shape: 0,
        min: -0.5 * depth,
        max: 0.5 * depth,
        param_base: base,
        filter: None,
        id: None,
    };

    let unmodulated = render(true, None);
    assert_ne!(
        unmodulated,
        render(false, None),
        "the pulse-width LFO itself changed nothing"
    );
    for (target, base, depth) in [
        (ModTarget::PulseWidthLfoRate, 2.0, 1.0),
        (ModTarget::PulseWidthLfoDepth, 0.4, 0.2),
    ] {
        let modulator = lfo(target, base, depth);
        let modulated = render(true, Some(modulator));
        assert_eq!(&unmodulated[..256], &modulated[..256]);
        let diff: f64 = unmodulated[256..]
            .iter()
            .zip(&modulated[256..])
            .map(|(a, b)| f64::from(a - b).abs())
            .sum();
        assert!(diff > 1.0, "{target:?} modulation changed nothing: {diff}");
        assert_eq!(
            render(false, Some(modulator)),
            render(false, None),
            "{target:?} conjured a pulse-width LFO"
        );
    }
}

/// The feedback delay belongs to the orbit, but a `delaytime` modulator
/// belongs to one voice. Its a-rate contribution must reach the shared line
/// without moving the voice's direct path.
#[test]
fn a_delay_time_modulator_moves_the_orbit_echo() {
    use rustel_audio::{DelayControls, LfoMod, ModTarget};
    let render = |modulated: bool| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.delay = Some(DelayControls {
            wet: 0.8,
            time_secs: 0.06,
            feedback: 0.6,
        });
        if modulated {
            c.lfos[0] = Some(LfoMod {
                fxi: None,
                target: ModTarget::DelayTime,
                frequency_hz: 3.0,
                phase0: 0.0,
                depth: 0.04,
                dcoffset: -0.5,
                skew: 0.5,
                curve: 1.0,
                shape: 0,
                min: -0.02,
                max: 0.02,
                param_base: 0.06,
                filter: None,
                id: None,
            });
        }
        let event = OnsetEvent::new(0, 110.0, 0.8, 0.4).with_controls(c);
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };

    let plain = render(false);
    let modulated = render(true);
    // The modulator cannot affect the direct path before the first possible
    // echo, but a moving read head must change the wet signal afterwards.
    assert_eq!(&plain[..256], &modulated[..256]);
    let diff: f64 = plain
        .iter()
        .zip(&modulated)
        .map(|(a, b)| f64::from(a - b).abs())
        .sum();
    assert!(diff > 1.0, "delayTime modulation changed nothing: {diff}");
}

/// A `delayfeedback` modulator rides the a-rate GainNode inside the shared
/// orbit loop. The dry path and first echo do not depend on that gain, while
/// later echoes must follow it.
#[test]
fn a_delay_feedback_modulator_moves_later_orbit_echoes() {
    use rustel_audio::{DelayControls, LfoMod, ModTarget};
    let render = |modulated: bool| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.delay = Some(DelayControls {
            wet: 0.8,
            time_secs: 0.03,
            feedback: 0.5,
        });
        if modulated {
            c.lfos[0] = Some(LfoMod {
                fxi: None,
                target: ModTarget::DelayFeedback,
                frequency_hz: 4.0,
                phase0: 0.0,
                depth: 0.4,
                dcoffset: -0.5,
                skew: 0.5,
                curve: 1.0,
                shape: 0,
                min: -0.2,
                max: 0.2,
                param_base: 0.5,
                filter: None,
                id: None,
            });
        }
        let event = OnsetEvent::new(0, 110.0, 0.8, 0.3).with_controls(c);
        render_pcm(&mut ScalarBackend::new(), 48_000, 16_000, &[event]).expect("render")
    };

    let plain = render(false);
    let modulated = render(true);
    // The first delayed read has not traversed the feedback gain yet.
    assert_eq!(&plain[..3_000], &modulated[..3_000]);
    let diff: f64 = plain[3_000..]
        .iter()
        .zip(&modulated[3_000..])
        .map(|(a, b)| f64::from(a - b).abs())
        .sum();
    assert!(
        diff > 1.0,
        "delay feedback modulation changed nothing: {diff}"
    );
}

/// A `bus` send is inaudible on its own - `dry(0)` removes the direct leg, so
/// the only path to the output is a receiver. Covers all three halves of the
/// feature: the send, `busgain` scaling it, and `s("bus")` tapping it.
#[test]
fn a_bus_send_is_only_audible_through_a_receiver() {
    let send = |busgain: f32| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.bus = Some(1);
        c.busgain = busgain;
        c.dry = Some(0.0);
        OnsetEvent::new(0, 220.0, 0.8, 0.5).with_controls(c)
    };
    let tap = || {
        let c = controls(Waveform::Sine, None);
        OnsetEvent::new(0, 220.0, 1.0, 0.5)
            .with_controls(c)
            .with_optional_synth(Some(rustel_audio::SynthSource::Bus { bus: 1 }))
    };
    let render = |events: &[OnsetEvent]| {
        let mut backend = ScalarBackend::new();
        render_pcm(&mut backend, 48_000, 24_000, events).expect("render")
    };
    let energy = |pcm: &[f32]| pcm.iter().map(|s| f64::from(*s).abs()).sum::<f64>();

    // The send alone reaches nothing.
    assert_eq!(
        energy(&render(&[send(1.0)])),
        0.0,
        "a dry(0) bus send leaked to the output with no receiver"
    );
    // The receiver alone has nothing to read.
    assert_eq!(
        energy(&render(&[tap()])),
        0.0,
        "s(\"bus\") produced sound with nothing feeding the bus"
    );
    // Together they are audible, and `busgain` scales what arrives.
    let full = energy(&render(&[send(1.0), tap()]));
    let quarter = energy(&render(&[send(0.25), tap()]));
    assert!(full > 1.0, "bus receiver stayed silent, energy = {full}");
    let ratio = quarter / full;
    assert!(
        (ratio - 0.25).abs() < 0.01,
        "busgain(0.25) should quarter the send, got {ratio}"
    );
}

/// Upstream's bus is a GainNode, so WebAudio's topological order hands the
/// receiver the CURRENT block's mix. Ordering the receiver first in the event
/// list must not delay it by a sample.
#[test]
fn a_bus_receiver_reads_the_same_sample_its_senders_wrote() {
    let events = |receiver_first: bool| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.bus = Some(0);
        c.dry = Some(0.0);
        let send = OnsetEvent::new(0, 220.0, 0.8, 0.5).with_controls(c);
        let tap = OnsetEvent::new(0, 220.0, 1.0, 0.5)
            .with_controls(controls(Waveform::Sine, None))
            .with_optional_synth(Some(rustel_audio::SynthSource::Bus { bus: 0 }));
        if receiver_first {
            vec![tap, send]
        } else {
            vec![send, tap]
        }
    };
    let render = |receiver_first: bool| {
        let mut backend = ScalarBackend::new();
        render_pcm(&mut backend, 48_000, 8_000, &events(receiver_first)).expect("render")
    };
    assert_eq!(
        render(true),
        render(false),
        "bus output depended on the order voices arrived in"
    );
}

/// `bmod` reads the bus as a modulation source rather than as audio, and is
/// subject to the same ordering rule.
#[test]
fn bmod_modulates_from_a_bus_and_is_silent_without_one() {
    use rustel_audio::{BusMod, ModTarget};
    let render = |with_send: bool| {
        let mut c = controls(Waveform::Sine, None);
        c.bus = Some(2);
        c.dry = Some(0.0);
        let send = OnsetEvent::new(0, 4.0, 1.0, 0.5).with_controls(c);

        let mut target = controls(Waveform::Sawtooth, None);
        target.bus_mods[0] = Some(BusMod {
            fxi: None,
            bus: 2,
            target: ModTarget::Gain,
            depth: 1.0,
            dc: 0.0,
            min: -1.0,
            max: 1.0,
            param_base: 0.8,
        });
        let voice = OnsetEvent::new(0, 220.0, 0.8, 0.5).with_controls(target);

        let events = if with_send {
            vec![send, voice]
        } else {
            vec![voice]
        };
        let mut backend = ScalarBackend::new();
        render_pcm(&mut backend, 48_000, 24_000, &events).expect("render")
    };
    let modulated = render(true);
    let flat = render(false);
    let diff: f64 = modulated
        .iter()
        .zip(&flat)
        .map(|(a, b)| f64::from(a - b).abs())
        .sum();
    assert!(diff > 1.0, "bmod produced no modulation, |diff| = {diff}");
}

/// `fmwave` picks the modulator's own shape. Each shape has to reach the
/// carrier as a genuinely different spectrum - a modulator whose waveform is
/// ignored renders identically to the sine default, which is the failure this
/// catches.
#[test]
fn fmwave_gives_each_modulator_shape_its_own_spectrum() {
    use rustel_audio::FmWave;
    let render = |waveform: FmWave| {
        let mut event = OnsetEvent::new(0, 220.0, 1.0, 0.4);
        let mut fm = fm_chain(&[Some((4.0, 2.01))]);
        fm.operators[0].as_mut().expect("operator 1").waveform = waveform;
        event.controls.fm = Some(fm);
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };
    let sine = render(FmWave::Sine);
    assert!(
        sine.iter().any(|s| s.abs() > 1e-6),
        "the sine modulator rendered silence"
    );
    for shape in [
        FmWave::Triangle,
        FmWave::Square,
        FmWave::Sawtooth,
        FmWave::Noise(2),
    ] {
        let other = render(shape);
        let diff: f64 = other
            .iter()
            .zip(&sine)
            .map(|(a, b)| f64::from(a - b).abs())
            .sum();
        assert!(
            diff > 1.0,
            "{shape:?} modulated identically to sine, |diff| = {diff}"
        );
    }
}

/// A noise operator is a looping buffer on strudel.cc, with no `frequency`
/// AudioParam - so nothing can modulate INTO one. An operator above it in the
/// chain must therefore make no difference to what is heard.
#[test]
fn nothing_modulates_into_a_noise_shaped_operator() {
    use rustel_audio::FmWave;
    let render = |above: bool| {
        let stages: &[Option<(f32, f32)>] = if above {
            &[Some((2.0, 1.5)), Some((8.0, 3.0))]
        } else {
            &[Some((2.0, 1.5))]
        };
        let mut fm = fm_chain(stages);
        // Operator 1 is the noise one; the operator above aims at it.
        fm.operators[0].as_mut().expect("operator 1").waveform = FmWave::Noise(2);
        let mut event = OnsetEvent::new(0, 220.0, 1.0, 0.4);
        event.controls.fm = Some(fm);
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };
    let bare = render(false);
    let driven = render(true);
    assert_eq!(
        bare, driven,
        "an operator above a noise modulator changed the output"
    );
}

/// `fmi{i}{j}` connects operator i to operator j directly. `fmi20` sends
/// operator 2 past operator 1 to the carrier, and one operator can feed
/// several targets.
#[test]
fn the_fm_matrix_routes_past_the_chain_and_fans_out() {
    let render = |routes: &[(u8, u8, f32)]| {
        let mut operators = [None; rustel_audio::MAX_FM_OPERATORS];
        for (harmonicity, slot) in [(2.01f32, 0usize), (3.0, 1)] {
            operators[slot] = Some(rustel_audio::FmOperator {
                harmonicity,
                waveform: rustel_audio::FmWave::Sine,
                env: None,
                env_exponential: true,
            });
        }
        let mut wired = [None; rustel_audio::MAX_FM_ROUTES];
        for (slot, (source, target, amount)) in routes.iter().enumerate() {
            wired[slot] = Some(rustel_audio::FmRoute {
                source: *source,
                target: *target,
                amount: *amount,
                mod_slot: None,
            });
        }
        let mut event = OnsetEvent::new(0, 220.0, 1.0, 0.4);
        event.controls.fm = Some(rustel_audio::FmControls {
            operators,
            routes: wired,
        });
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };
    let distance = |a: &[f32], b: &[f32]| -> f64 {
        a.iter().zip(b).map(|(a, b)| f64::from(a - b).abs()).sum()
    };

    let plain = render(&[(1, 0, 2.0)]);
    // Operator 2 reaching the carrier directly is not the same connection as
    // operator 2 reaching operator 1.
    let past = render(&[(1, 0, 2.0), (2, 0, 2.5)]);
    let through = render(&[(1, 0, 2.0), (2, 1, 2.5)]);
    assert!(
        distance(&past, &plain) > 1.0,
        "fmi20 made no difference to the carrier"
    );
    assert!(
        distance(&past, &through) > 1.0,
        "routing operator 2 to the carrier sounded the same as routing it to operator 1"
    );

    // Fanning one operator into both targets differs from either alone.
    let fanned = render(&[(1, 0, 2.0), (2, 0, 2.5), (2, 1, 2.5)]);
    assert!(
        distance(&fanned, &past) > 1.0 && distance(&fanned, &through) > 1.0,
        "a fanned-out operator matched one of its single connections"
    );
}

/// A route whose source operator was never built contributes nothing, which is
/// how a gap in the chain stays severed once routing is a matrix.
#[test]
fn a_route_from_an_absent_operator_is_inaudible() {
    let render = |with_orphan: bool| {
        let mut operators = [None; rustel_audio::MAX_FM_OPERATORS];
        operators[0] = Some(rustel_audio::FmOperator {
            harmonicity: 2.01,
            waveform: rustel_audio::FmWave::Sine,
            env: None,
            env_exponential: true,
        });
        let mut routes = [None; rustel_audio::MAX_FM_ROUTES];
        routes[0] = Some(rustel_audio::FmRoute {
            source: 1,
            target: 0,
            amount: 2.0,
            mod_slot: None,
        });
        if with_orphan {
            // Operator 5 is named by a route but was never built.
            routes[1] = Some(rustel_audio::FmRoute {
                source: 5,
                target: 0,
                amount: 9.0,
                mod_slot: None,
            });
        }
        let mut event = OnsetEvent::new(0, 220.0, 1.0, 0.4);
        event.controls.fm = Some(rustel_audio::FmControls { operators, routes });
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };
    assert_eq!(
        render(false),
        render(true),
        "a route from an operator that does not exist was audible"
    );
}

/// No FM voice remembers the notes before it, however deep its chain.
///
/// `mod()` starting a modulator with a bare `osc.start()` reads like a
/// modulator running since the graph was built, and a note beginning late
/// finding it mid-cycle. It is not: the modulator is connected only into the
/// carrier's frequency param, nothing pulls it until the carrier evaluates
/// that param, and the carrier does not evaluate anything while it is still
/// before its start time. What plays earlier leaves nothing behind.
#[test]
fn no_fm_voice_remembers_the_notes_before_it() {
    let render = |stages: &[Option<(f32, f32)>], with_earlier_note: bool| {
        let fm = fm_chain(stages);
        let mut late = OnsetEvent::new(96_000, 130.81, 1.0, 0.4);
        late.controls.fm = Some(fm);
        let mut events = Vec::new();
        if with_earlier_note {
            let mut first = OnsetEvent::new(0, 130.81, 1.0, 0.4);
            first.controls.fm = Some(fm);
            events.push(first);
        }
        events.push(late);
        let pcm = render_pcm(&mut ScalarBackend::new(), 48_000, 105_600, &events).expect("render");
        // Only the late note, which both renders share.
        pcm[96_000 * 2..].to_vec()
    };

    for chain in [
        &[Some((2.0, 1.41))][..],
        &[Some((2.0, 1.41)), Some((3.0, 2.7))][..],
        &[Some((2.0, 1.41)), Some((3.0, 2.7)), Some((1.8, 0.5))][..],
    ] {
        assert_eq!(
            render(chain, false),
            render(chain, true),
            "an FM voice {} deep depended on an earlier note",
            chain.len()
        );
    }
}

/// `s("bytebeat")` picks one of fifteen built-in integer expressions with `n`.
/// Each has to be audibly its own: an index that silently collapsed onto its
/// neighbour would still render, just as the wrong sound.
#[test]
fn every_bytebeat_expression_is_distinct_and_audible() {
    let render = |expression: u8| {
        let event = OnsetEvent::new(0, 220.0, 1.0, 0.5).with_optional_synth(Some(
            rustel_audio::SynthSource::ByteBeat {
                expression,
                program: None,
                start_offset: None,
            },
        ));
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };
    let rendered: Vec<Vec<f32>> = (0..rustel_audio::BYTEBEAT_EXPRESSIONS)
        .map(render)
        .collect();
    for (index, pcm) in rendered.iter().enumerate() {
        assert!(
            pcm.iter().any(|s| s.abs() > 1e-6),
            "bytebeat expression {index} rendered silence"
        );
    }
    for a in 0..rendered.len() {
        for b in (a + 1)..rendered.len() {
            assert_ne!(
                rendered[a], rendered[b],
                "bytebeat expressions {a} and {b} produced identical audio"
            );
        }
    }
}

/// The worklet's begin gate holds through the quantum that contains the onset.
/// Output starts at the next 128-frame boundary, and `t` starts at the onset.
#[test]
fn bytebeat_waits_for_the_quantum_after_its_onset() {
    let event = OnsetEvent::new(0, 220.0, 1.0, 0.5).with_optional_synth(Some(
        rustel_audio::SynthSource::ByteBeat {
            program: None,
            expression: 2,
            start_offset: None,
        },
    ));
    let pcm = render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render");
    // Stereo interleaved: the first 128 frames are the gated quantum.
    assert!(
        pcm[..128 * 2].iter().all(|s| *s == 0.0),
        "bytebeat emitted inside the quantum containing its onset"
    );
    assert!(
        pcm[128 * 2..256 * 2].iter().any(|s| s.abs() > 1e-6),
        "bytebeat stayed silent past its gate"
    );
}

/// The worklet distinguishes an omitted `byteBeatStartTime` from an explicit
/// zero: omission seeds `t` from the onset, while presence resets it to zero.
/// Collapsing the two makes a late bytebeat begin in the wrong part of its
/// expression even though both values appear to carry an offset of zero.
#[test]
fn bytebeat_start_time_presence_resets_the_counter() {
    let render = |start_offset| {
        // The counter seeds from the note's begin in SECONDS, the way the
        // worklet's `params.begin[0] * sampleRate` does, so the voice has to
        // carry it: a default of zero would make both renders start at the
        // same place and the comparison below vacuous.
        let controls = rustel_audio::OscillatorControls {
            limit: None,
            worklet_begin_secs: 47_999.0 / 48_000.0,
            ..rustel_audio::OscillatorControls::default()
        };
        let event = OnsetEvent::new(47_999, 220.0, 1.0, 0.5)
            .with_controls(controls)
            .with_optional_synth(Some(rustel_audio::SynthSource::ByteBeat {
                program: None,
                expression: 2,
                start_offset,
            }));
        render_pcm(&mut ScalarBackend::new(), 48_000, 72_000, &[event]).expect("render")
    };

    let absent = render(None);
    let explicit_zero = render(Some(0.0));
    assert_ne!(
        &absent[48_000 * 2..48_128 * 2],
        &explicit_zero[48_000 * 2..48_128 * 2],
        "an explicit zero did not reset the bytebeat counter"
    );
}

/// `fxi` aims a modulator at one `.FX()` stage instead of the main chain.
/// Two stages with different cutoffs make the routing observable: modulating
/// stage 0 and modulating stage 1 must both change the sound, and differently
/// from each other. Landing on the main chain's parameter of the same name
/// would modulate a filter the score never named.
#[test]
fn fxi_aims_a_modulator_at_one_stage_and_not_another() {
    use rustel_audio::{LfoMod, ModTarget};
    let render = |fxi: Option<u8>| {
        let mut c = controls(Waveform::Sawtooth, None);
        let stage = |cutoff: f32| rustel_audio::FxStage {
            stretch: None,
            transient: None,
            gain: 1.0,
            filters: FilterControls {
                lowpass: Some(rustel_audio::StaticBiquad {
                    frequency_hz: cutoff,
                    q: 1.0,
                }),
                ..FilterControls::default()
            },
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
        };
        c.fx_stages[0] = Some(stage(700.0));
        c.fx_stages[1] = Some(stage(2400.0));
        c.lfos[0] = Some(LfoMod {
            fxi,
            target: ModTarget::LowpassFreq,
            frequency_hz: 3.0,
            phase0: 0.0,
            depth: 900.0,
            dcoffset: -0.5,
            skew: 0.5,
            curve: 1.0,
            shape: 0,
            min: -20_000.0,
            max: 20_000.0,
            param_base: 700.0,
            filter: None,
            id: None,
        });
        let event = OnsetEvent::new(0, 110.0, 0.8, 0.5).with_controls(c);
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };
    let energy = |a: &[f32], b: &[f32]| -> f64 {
        a.iter().zip(b).map(|(a, b)| f64::from(a - b).abs()).sum()
    };

    // Unmodulated: no LFO reaches any stage.
    let plain = render(None);
    let first = render(Some(0));
    let second = render(Some(1));

    assert!(
        energy(&first, &plain) > 1.0,
        "fxi 0 did not reach the first stage"
    );
    assert!(
        energy(&second, &plain) > 1.0,
        "fxi 1 did not reach the second stage"
    );
    assert!(
        energy(&first, &second) > 1.0,
        "fxi 0 and fxi 1 modulated the same filter"
    );
}

/// `noise` crossfades pink noise with the oscillator before the envelope.
/// Ignoring it rendered the bare oscillator, which is audible but wrong; and
/// zero must skip the mix entirely rather than crossfade at zero.
#[test]
fn noise_crossfades_pink_into_the_oscillator() {
    let render = |noise: f32| {
        let mut c = controls(Waveform::Sawtooth, None);
        c.noise = noise;
        let event = OnsetEvent::new(0, 220.0, 0.8, 0.5).with_controls(c);
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };
    let dry = render(0.0);
    assert_eq!(dry, render(0.0), "the dry path is not deterministic");
    for amount in [0.25f32, 0.5, 1.0] {
        let mixed = render(amount);
        let diff: f64 = mixed
            .iter()
            .zip(&dry)
            .map(|(a, b)| f64::from(a - b).abs())
            .sum();
        assert!(diff > 1.0, "noise({amount}) did not reach the oscillator");
    }
    // Fully wet is quieter than the oscillator alone: `wetfade` closes the dry
    // leg at one while the noise leg stays open.
    let rms = |pcm: &[f32]| -> f64 {
        (pcm.iter()
            .map(|s| f64::from(*s) * f64::from(*s))
            .sum::<f64>()
            / pcm.len() as f64)
            .sqrt()
    };
    assert!(
        rms(&render(1.0)) < rms(&dry),
        "fully wet noise was not quieter than the bare oscillator"
    );
}

/// The pitch envelope must be held off for exactly the opening PARTIAL
/// quantum and no longer.
///
/// `getPitchEnvelope` schedules its first value at the note's start, so
/// before then `detune` still holds the 0 it was built with, and Blink reads
/// that param from the START of the render quantum. A note beginning
/// mid-quantum is therefore undetuned for its first `128 - offset` samples.
///
/// Asserted by rendering the same voice twice, once with the envelope and
/// once without: inside the partial quantum the two must be IDENTICAL, and
/// after the boundary they must diverge. A note that starts on a boundary
/// has no partial quantum and must differ immediately.
#[test]
fn the_pitch_envelope_is_held_off_only_for_the_opening_partial_quantum() {
    fn render_at(onset: u64, with_env: bool) -> Vec<f32> {
        let mut c = controls(Waveform::Sine, None);
        if with_env {
            c.pitch_env = Some(rustel_audio::PitchEnvControls {
                adsr: rustel_audio::FilterEnvelope {
                    attack_secs: 0.2,
                    decay_secs: 0.001,
                    sustain: 1.0,
                    release_secs: 0.001,
                    min_hz: -1200.0,
                    max_hz: 0.0,
                },
                exponential: false,
            });
        }
        let event = OnsetEvent::new(onset, 261.63, 1.0, 0.5).with_controls(c);
        render_pcm(&mut ScalarBackend::new(), 48_000, 1_024, &[event]).expect("render")
    }

    let offset = 64_u64;
    let with = render_at(offset, true);
    let without = render_at(offset, false);
    let frame = |pcm: &[f32], i: usize| pcm[i * 2];

    // Inside the partial quantum the automation has not begun: identical.
    for i in (offset as usize)..128 {
        assert!(
            (frame(&with, i) - frame(&without, i)).abs() < 1e-9,
            "frame {i} is inside the opening partial quantum and must be \
             undetuned, but the envelope moved it"
        );
    }

    // After the boundary the envelope is live, so the two must part.
    let parted = (128..1_024).any(|i| (frame(&with, i) - frame(&without, i)).abs() > 1e-6);
    assert!(
        parted,
        "the envelope never took effect after the quantum boundary - the \
         hold is suppressing the whole note"
    );

    // A note ON a boundary has no partial quantum to hold.
    let aligned_with = render_at(128, true);
    let aligned_without = render_at(128, false);
    let aligned_parted =
        (128..1_024).any(|i| (frame(&aligned_with, i) - frame(&aligned_without, i)).abs() > 1e-6);
    assert!(
        aligned_parted,
        "an onset on a 128-frame boundary must not be held off at all"
    );
}

/// The first voice into an orbit sets the orbit's routing, sends included.
/// The `channels` of every later voice is inert.
#[test]
fn the_first_voice_into_an_orbit_keys_its_channels_for_good() {
    let voice = |onset: u64, channels: Option<[u8; 2]>| {
        let mut event = OnsetEvent::new(onset, 220.0, 1.0, 0.05);
        event.controls.channels = channels;
        event
    };
    let channel_rms = |events: &[OnsetEvent]| {
        let pcm = render_pcm(&mut ScalarBackend::new(), 48_000, 9_600, events).expect("render");
        let mut power = [0.0f64; 2];
        for frame in pcm.as_chunks::<2>().0.iter() {
            power[0] += f64::from(frame[0]) * f64::from(frame[0]);
            power[1] += f64::from(frame[1]) * f64::from(frame[1]);
        }
        [
            (power[0] / (pcm.len() / 2) as f64).sqrt(),
            (power[1] / (pcm.len() / 2) as f64).sqrt(),
        ]
    };

    // The first voice names the left output only: the whole orbit follows,
    // including the second voice, whose own lack of channels changes nothing.
    let keyed = channel_rms(&[voice(0, Some([1, 0])), voice(2_400, None)]);
    assert!(keyed[0] > 1e-3, "the keyed output must carry both voices");
    assert_eq!(
        keyed[1], 0.0,
        "an output the orbit never named stays silent"
    );

    // Reversed order: the first voice has no channels, so the orbit is plain
    // stereo and the SECOND voice's channels(1) is inert.
    let inert = channel_rms(&[voice(0, None), voice(2_400, Some([1, 0]))]);
    assert!(
        inert[1] > 1e-3,
        "a later voice's channels must not re-route the orbit: right was silenced"
    );
    let plain = channel_rms(&[voice(0, None), voice(2_400, None)]);
    assert_eq!(inert, plain, "the late channels must change nothing at all");
}

/// The transient shaper runs on the source, before the main gain, so the
/// hap's gain scales the output linearly.
#[test]
fn the_hap_gain_scales_a_transient_voice_linearly() {
    let render = |gain: f32| {
        let mut event = OnsetEvent::new(0, 220.0, gain, 0.3);
        event.controls.transient = Some(rustel_audio::TransientControls {
            attack: 0.8,
            sustain: -0.3,
        });
        render_pcm(&mut ScalarBackend::new(), 48_000, 24_000, &[event]).expect("render")
    };
    let half = render(0.4);
    let full = render(0.8);
    let mut worst = 0.0f32;
    for (a, b) in half.iter().zip(full.iter()) {
        // Everything after the shaper is linear here, so exactly 2x.
        worst = worst.max((b - a * 2.0).abs());
    }
    assert!(
        worst < 1e-5,
        "gain must ride AFTER the shaper; doubling it bent the signal by {worst}"
    );
}

/// A pitch envelope starts `offset` frames late in the voice's first render
/// quantum, matching the browser: a source that begins mid-quantum writes
/// output from its onset but reads its per-frame detune automation from the
/// quantum start, so the first `offset` frames play the base pitch and the
/// envelope catches up at the next 128-frame boundary.
#[test]
fn a_pitch_envelope_runs_offset_frames_late_in_the_first_quantum() {
    let render = |with_env: bool| {
        let mut event = OnsetEvent::new(32, 36.7, 0.8, 0.5);
        if with_env {
            event.controls.pitch_env = Some(rustel_audio::PitchEnvControls {
                adsr: rustel_audio::FilterEnvelope {
                    attack_secs: 0.0,
                    decay_secs: 0.12,
                    sustain: 0.0,
                    release_secs: 0.0,
                    min_hz: 0.0,
                    max_hz: 3600.0,
                },
                exponential: false,
            });
        }
        render_pcm(&mut ScalarBackend::new(), 48_000, 512, &[event]).expect("render")
    };
    let plain = render(false);
    let enveloped = render(true);
    let frame = |pcm: &Vec<f32>, f: usize| pcm[2 * f];
    let same_until_64 = (32..64).all(|f| (frame(&plain, f) - frame(&enveloped, f)).abs() < 1e-6);
    assert!(
        same_until_64,
        "the first 32 frames after an offset-32 onset must play the base pitch"
    );
    let differs_before_boundary =
        (64..128).any(|f| (frame(&plain, f) - frame(&enveloped, f)).abs() > 1e-4);
    assert!(
        differs_before_boundary,
        "the envelope must be audible before the quantum boundary, 32 frames late, not muted until it"
    );
}

#[test]
fn an_event_level_cut_chokes_the_previous_voice_in_the_group() {
    // Two long tones, half a second apart, in one event-level choke group
    // with no sample controls. The first must be gone 10 ms after the second
    // starts.
    let sr = 48_000u32;
    let first = OnsetEvent::new(0, 220.0, 1.0, 2.0).with_cut(Some(f32::MAX));
    let second = OnsetEvent::new(u64::from(sr) / 2, 493.9, 1.0, 2.0).with_cut(Some(f32::MAX));

    let choked =
        render_pcm(&mut ScalarBackend::new(), sr, sr as usize, &[first, second]).expect("render");
    let alone = render_pcm(&mut ScalarBackend::new(), sr, sr as usize, &[second])
        .expect("render second alone");

    // Well past the choke fade, the mix is EXACTLY the second voice alone.
    let from = (sr as usize / 2 + sr as usize / 10) * 2;
    let differs = choked[from..]
        .iter()
        .zip(&alone[from..])
        .map(|(mixed, solo)| (mixed - solo).abs())
        .fold(0.0f32, f32::max);
    assert!(
        differs < 1e-6,
        "the first voice should be silent after the choke (max residue {differs})"
    );
    // And before the second onset the first tone was audibly there.
    let head = &choked[..sr as usize / 2 * 2];
    assert!(
        head.iter().any(|sample| sample.abs() > 0.01),
        "the first preview sounds until it is choked"
    );
}

/// A rewind's takeover cut: the outgoing rendition falls silent at the flip
/// over the 10 ms choke ramp, and its ghost-window events never sound. A
/// flip without the cut (an ordinary edit) leaves the old voices ringing.
#[test]
fn a_takeover_cut_silences_the_old_generation_at_the_flip() {
    use rustel_audio::TakeoverCut;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    const SR: u32 = 48_000;
    // `ghost` puts an old-generation event INSIDE the ghost window -
    // scheduled before the flip, targeting after it (the horizon contract
    // keeps it in the ring). It is the doubled first beat when it sounds.
    let render = |intent: TakeoverCut, flip: usize, takeover_frame: u64, ghost: bool| {
        let ring = rustel_audio::Ring::new(8);
        // Old generation (1): the pad, scheduled long before the flip.
        assert!(ring.push(rustel_audio::AudioEvent {
            onset_id: 1,
            generation: 1,
            ui_visuals: 0,
            target_frame: 0,
            onset_lead: 0.0,
            freq_hz: 110.0,
            gain: 0.8,
            duration_secs: 4.0,
            controls: {
                let mut c = controls(Waveform::Sawtooth, None);
                c.envelope = rustel_audio::Envelope {
                    attack_secs: 0.001,
                    decay_secs: 0.001,
                    sustain: 1.0,
                    release_secs: 4.0,
                };
                c
            },
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }));
        // The old generation's ghost: scheduled into the ring long before
        // the flip, targeting a beat after it - exactly what a restarted
        // loop plays again as its own first beat.
        if ghost {
            assert!(ring.push(rustel_audio::AudioEvent {
                onset_id: 3,
                generation: 1,
                ui_visuals: 0,
                target_frame: takeover_frame + 4_800, // 100 ms past the takeover
                onset_lead: 0.0,
                freq_hz: 110.0,
                gain: 0.8,
                duration_secs: 0.5,
                controls: {
                    let mut c = controls(Waveform::Sawtooth, None);
                    c.envelope = rustel_audio::Envelope {
                        attack_secs: 0.001,
                        decay_secs: 0.001,
                        sustain: 1.0,
                        release_secs: 0.5,
                    };
                    c
                },
                sample: None,
                wavetable: None,
                synth: None,
                cut: None,
            }));
        }

        let generation = AtomicU64::new(1);
        let takeover = AtomicU64::new(0);
        let line_arm = AtomicU64::new(0);
        let cut_flag = AtomicU64::new(0);
        let stopped = AtomicBool::new(false);
        let mut backend = rustel_audio::LiveScalarBackend::new(SR, 8).expect("live backend");

        // Long enough to cover the new loop's downbeat 250 ms past the
        // takeover, for every variant this helper renders.
        let total = takeover_frame as usize + 16_000;
        let mut pcm = vec![0.0f32; total * 2];
        let mut cursor = 0usize;
        while cursor < total {
            let n = (total - cursor).min(128);
            // Mid-stream flip: the producer stores the takeover (and cut)
            // before the generation, so the consumer sees them together.
            if cursor == flip {
                // The producer's order: takeover and cut, then the generation
                // flip, then the replacement batch. Pending never holds a
                // new-generation onset at flip time. The new loop's first
                // event lands 250 ms past the takeover frame, which is the
                // restart's natural latency that the ghost window spans.
                takeover.store(takeover_frame, std::sync::atomic::Ordering::Release);
                cut_flag.store(intent as u64, std::sync::atomic::Ordering::Release);
                generation.store(2, std::sync::atomic::Ordering::Release);
                assert!(ring.push(rustel_audio::AudioEvent {
                    onset_id: 2,
                    generation: 2,
                    ui_visuals: 0,
                    target_frame: takeover_frame + 12_000,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.8,
                    duration_secs: 0.5,
                    controls: controls(Waveform::Sine, None),
                    sample: None,
                    wavetable: None,
                    synth: None,
                    cut: None,
                }));
            }
            backend.process_block_with(
                &mut pcm[cursor * 2..(cursor + n) * 2],
                n,
                cursor as u64,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &cut_flag,
                    line_arm: &line_arm,
                },
                &stopped,
            );
            cursor += n;
        }
        pcm
    };

    let peak = |pcm: &[f32], from: usize, to: usize| {
        pcm[from * 2..to * 2]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
    };

    // The flip sits on a block boundary near the saw's peak, so a step mute
    // would be audible. The takeover is the same frame here: this run pins
    // the AtFlip drop horizon.
    const FLIP: usize = 9_344;
    const RAMP: usize = 480; // 10 ms
    let cut_pcm = render(TakeoverCut::AtFlip, FLIP, FLIP as u64, true);
    let edit_pcm = render(TakeoverCut::None, FLIP, FLIP as u64, true);

    // Identical up to the flip, both channels: nothing the producer
    // published yet can change what is heard.
    assert_eq!(
        cut_pcm[..FLIP * 2],
        edit_pcm[..FLIP * 2],
        "pre-flip audio differs"
    );

    let loud = peak(&cut_pcm, FLIP - 1_280, FLIP - 128);
    assert!(
        loud > 0.1,
        "the pad must be audible before the flip: {loud}"
    );

    // AT the flip the pad begins to fade; past the ramp only silence
    // remains until the ghost's target - it never did sound.
    let past_ghost = peak(&cut_pcm, FLIP + RAMP + 128, GHOST_AT);
    assert!(
        past_ghost < loud * 0.1,
        "between the flip and the ghost target only the fading pad may sound: {past_ghost} vs {loud}"
    );

    // The GHOST never sounds: at its target (+100 ms) the cut run is
    // silent where the edit run carries the old beat again.
    const GHOST_AT: usize = FLIP + 4_800; // the ghost's target
    let ghost_in_cut = peak(&cut_pcm, GHOST_AT, GHOST_AT + 1_280);
    let ghost_in_edit = peak(&edit_pcm, GHOST_AT, GHOST_AT + 1_280);
    assert!(
        ghost_in_edit > loud * 0.5,
        "without the cut the ghost beat sounds (the old behaviour): {ghost_in_edit}"
    );
    assert!(
        ghost_in_cut < ghost_in_edit * 0.1,
        "the ghost beat must never sound under a rewind: {ghost_in_cut} vs {ghost_in_edit}"
    );

    // The new loop's downbeat (250 ms after the flip) sounds in the cut
    // run, untouched by the cut.
    const NEW_AT: usize = FLIP + 12_000;
    let new_alone = peak(&cut_pcm, NEW_AT + 128, NEW_AT + 1_280);
    assert!(
        new_alone > 0.05,
        "the new loop must sound after the flip: {new_alone}"
    );
    // And at its full level (0.8 gain, 0.24 peak), well past any choke
    // ramp: a new voice caught by the cut would stop 10 ms in.
    let new_full = peak(&cut_pcm, NEW_AT + RAMP + 128, NEW_AT + 3_600);
    assert!(
        new_full > 0.2,
        "the new loop's downbeat is untouched by the cut: {new_full}"
    );

    // WITHOUT the cut the pad rings on under the new loop - the old
    // artefact this contract removes.
    let rings_on = peak(&edit_pcm, NEW_AT + 128, NEW_AT + 1_280);
    assert!(
        rings_on > new_alone,
        "an edit keeps its ringing-out contract: {rings_on} vs {new_alone}"
    );

    // WITH the cut the pad is gone: past the 10 ms choke ramp the mix is
    // the new generation alone. The two runs carry the identical
    // new-generation onset, so their difference IS the old pad's tail.
    let removed = |from: usize, to: usize| {
        cut_pcm[from * 2..to * 2]
            .iter()
            .zip(&edit_pcm[from * 2..to * 2])
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()))
    };
    // The fade is the choke ramp from unity at the flip: linear over 10 ms,
    // silent past it - not a step mute, not a shorter ramp.
    assert_ramp_from(&cut_pcm, &edit_pcm, FLIP, "the flip's takeover cut");
    let gone = removed(NEW_AT, NEW_AT + 1_152);
    assert!(
        gone > rings_on * 0.5,
        "the pad is fully removed past the ramp: {gone} vs {rings_on}"
    );
}

/// In a reload, the takeover is the flip plus the continuity margin (a 0.25 s
/// ghost window). The cut must remove the pad and a ghost onset in the window.
/// The new loop's downbeat on the takeover frame must sound alone.
#[test]
fn a_takeover_cut_spans_the_flip_to_takeover_gap() {
    use rustel_audio::TakeoverCut;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    const SR: u32 = 48_000;
    // ~0.2 s, on a block and near the saw's peak (not its zero crossing).
    const FLIP: usize = 9_344;
    const TAKEOVER: usize = FLIP + 12_000; // + 0.25 s: the real margin
    let render = |intent: TakeoverCut| {
        let ring = rustel_audio::Ring::new(8);
        // The outgoing pad, sounding long before the flip.
        assert!(ring.push(rustel_audio::AudioEvent {
            onset_id: 1,
            generation: 1,
            ui_visuals: 0,
            target_frame: 0,
            onset_lead: 0.0,
            freq_hz: 110.0,
            gain: 0.8,
            duration_secs: 4.0,
            controls: {
                let mut c = controls(Waveform::Sawtooth, None);
                c.envelope = rustel_audio::Envelope {
                    attack_secs: 0.001,
                    decay_secs: 0.001,
                    sustain: 1.0,
                    release_secs: 4.0,
                };
                c
            },
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }));
        // The ghost: an old-generation onset the old score had already
        // scheduled into the ghost window - the doubled first beat when
        // it sounds.
        assert!(ring.push(rustel_audio::AudioEvent {
            onset_id: 3,
            generation: 1,
            ui_visuals: 0,
            target_frame: (FLIP + 4_800) as u64, // mid-gap
            onset_lead: 0.0,
            freq_hz: 110.0,
            gain: 0.8,
            duration_secs: 0.5,
            controls: {
                let mut c = controls(Waveform::Sawtooth, None);
                c.envelope = rustel_audio::Envelope {
                    attack_secs: 0.001,
                    decay_secs: 0.001,
                    sustain: 1.0,
                    release_secs: 0.5,
                };
                c
            },
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }));

        let generation = AtomicU64::new(1);
        let takeover = AtomicU64::new(0);
        let line_arm = AtomicU64::new(0);
        let cut_flag = AtomicU64::new(0);
        let stopped = AtomicBool::new(false);
        let mut backend = rustel_audio::LiveScalarBackend::new(SR, 8).expect("live backend");
        let total = TAKEOVER + 12_000;
        let mut pcm = vec![0.0f32; total * 2];
        let mut cursor = 0usize;
        while cursor < total {
            let n = (total - cursor).min(128);
            if cursor == FLIP {
                // The producer's order: takeover and cut BEFORE the
                // generation flip. The new loop's downbeat lands ON the
                // takeover frame.
                takeover.store(TAKEOVER as u64, std::sync::atomic::Ordering::Release);
                cut_flag.store(intent as u64, std::sync::atomic::Ordering::Release);
                generation.store(2, std::sync::atomic::Ordering::Release);
                assert!(ring.push(rustel_audio::AudioEvent {
                    onset_id: 2,
                    generation: 2,
                    ui_visuals: 0,
                    target_frame: TAKEOVER as u64,
                    onset_lead: 0.0,
                    freq_hz: 220.0,
                    gain: 0.8,
                    duration_secs: 0.5,
                    controls: controls(Waveform::Sine, None),
                    sample: None,
                    wavetable: None,
                    synth: None,
                    cut: None,
                }));
            }
            backend.process_block_with(
                &mut pcm[cursor * 2..(cursor + n) * 2],
                n,
                cursor as u64,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &cut_flag,
                    line_arm: &line_arm,
                },
                &stopped,
            );
            cursor += n;
        }
        pcm
    };
    let peak = |pcm: &[f32], from: usize, to: usize| {
        pcm[from * 2..to * 2]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
    };

    let cut_pcm = render(TakeoverCut::AtFlip);
    let edit_pcm = render(TakeoverCut::None);

    let loud = peak(&cut_pcm, FLIP - 1_280, FLIP - 128);
    assert!(loud > 0.1, "the pad must sound before the flip: {loud}");
    // The pad fades over the choke ramp from the flip, not the takeover.
    assert_ramp_from(&cut_pcm, &edit_pcm, FLIP, "the gap's takeover cut");

    // Mid-gap (the ghost's target) the cut run carries no old onset - the
    // flip horizon dropped it - while the edit run replays the old beat.
    const GHOST_AT: usize = FLIP + 4_800;
    let ghost_in_cut = peak(&cut_pcm, GHOST_AT, GHOST_AT + 1_280);
    let ghost_in_edit = peak(&edit_pcm, GHOST_AT, GHOST_AT + 1_280);
    assert!(
        ghost_in_edit > loud * 0.5,
        "without the cut the ghost sounds mid-gap: {ghost_in_edit}"
    );
    assert!(
        ghost_in_cut < ghost_in_edit * 0.1,
        "the ghost must never sound under an immediate rewind: {ghost_in_cut} vs {ghost_in_edit}"
    );

    // At the takeover the cut run's pad is long gone (ramp ended 10 ms
    // after the flip) and the new downbeat sounds alone.
    let new_alone = peak(&cut_pcm, TAKEOVER + 128, TAKEOVER + 1_280);
    assert!(
        new_alone > 0.05,
        "the new loop's downbeat lands on the takeover: {new_alone}"
    );
    let new_full = takeover_peak(&cut_pcm, TAKEOVER + 480 + 128, TAKEOVER + 4_800);
    assert!(
        new_full > 0.2,
        "the new loop's downbeat keeps its full level past any ramp: {new_full}"
    );
    let pad_in_edit = peak(&edit_pcm, TAKEOVER + 128, TAKEOVER + 1_280);
    assert!(
        pad_in_edit > new_alone,
        "the edit run's pad rings across the gap to the takeover: {pad_in_edit} vs {new_alone}"
    );
    let removed = |from: usize, to: usize| {
        cut_pcm[from * 2..to * 2]
            .iter()
            .zip(&edit_pcm[from * 2..to * 2])
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()))
    };
    let gone = removed(TAKEOVER, TAKEOVER + 1_152);
    assert!(
        gone > pad_in_edit * 0.5,
        "the pad is removed across the whole gap: {gone} vs {pad_in_edit}"
    );
}

/// A quantised rewind cuts at its line, not at the flip. The old score
/// plays its countdown to the line, and the restarted loop takes over there.
#[test]
fn a_quantised_takeover_cut_lands_at_the_line_not_the_flip() {
    use rustel_audio::TakeoverCut;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    const SR: u32 = 48_000;
    const FLIP: usize = 9_600; // 0.2 s
    // + 0.5 s: the countdown. Off the saw's zero crossing (FLIP + 24_000
    // is exactly 77 periods), near its peak, so a step mute at the line
    // removes the whole level at once.
    const TAKEOVER: usize = FLIP + 24_000 + 175;
    const RAMP: usize = 480; // 10 ms
    // `downbeat` pushes the restarted loop's first onset on the line;
    // `horizon` pushes two more countdown events after the flip, one past
    // the line (the takeover horizon refuses it) and one before it (it
    // plays, and stops at the line). Returns the pcm and the stale count.
    let render = |intent: TakeoverCut, downbeat: bool, horizon: bool| {
        let ring = rustel_audio::Ring::new(8);
        // The outgoing pad - the countdown's drone.
        assert!(ring.push(rustel_audio::AudioEvent {
            onset_id: 1,
            generation: 1,
            ui_visuals: 0,
            target_frame: 0,
            onset_lead: 0.0,
            freq_hz: 110.0,
            gain: 0.8,
            duration_secs: 4.0,
            controls: {
                let mut c = controls(Waveform::Sawtooth, None);
                c.envelope = rustel_audio::Envelope {
                    attack_secs: 0.001,
                    decay_secs: 0.001,
                    sustain: 1.0,
                    release_secs: 4.0,
                };
                c
            },
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }));
        // A countdown beat INSIDE the window (mid-countdown): for a
        // quantised rewind this is the old score playing its last bar and
        // it must SOUND - it is not a ghost, it is the countdown.
        assert!(ring.push(rustel_audio::AudioEvent {
            onset_id: 3,
            generation: 1,
            ui_visuals: 0,
            target_frame: (FLIP + 4_800) as u64,
            onset_lead: 0.0,
            freq_hz: 110.0,
            gain: 0.8,
            duration_secs: 0.5,
            controls: {
                let mut c = controls(Waveform::Sawtooth, None);
                c.envelope = rustel_audio::Envelope {
                    attack_secs: 0.001,
                    decay_secs: 0.001,
                    sustain: 1.0,
                    release_secs: 0.5,
                };
                c
            },
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }));

        let generation = AtomicU64::new(1);
        let takeover = AtomicU64::new(0);
        let line_arm = AtomicU64::new(0);
        let cut_flag = AtomicU64::new(0);
        let stopped = AtomicBool::new(false);
        let mut backend = rustel_audio::LiveScalarBackend::new(SR, 8).expect("live backend");
        let total = TAKEOVER + 12_000;
        let mut pcm = vec![0.0f32; total * 2];
        let mut stale = 0usize;
        let mut cursor = 0usize;
        while cursor < total {
            let n = (total - cursor).min(128);
            if cursor == FLIP {
                takeover.store(TAKEOVER as u64, std::sync::atomic::Ordering::Release);
                cut_flag.store(intent as u64, std::sync::atomic::Ordering::Release);
                generation.store(2, std::sync::atomic::Ordering::Release);
                if downbeat {
                    assert!(ring.push(rustel_audio::AudioEvent {
                        onset_id: 2,
                        generation: 2,
                        ui_visuals: 0,
                        target_frame: TAKEOVER as u64,
                        onset_lead: 0.0,
                        freq_hz: 220.0,
                        gain: 0.8,
                        duration_secs: 0.5,
                        controls: controls(Waveform::Sine, None),
                        sample: None,
                        wavetable: None,
                        synth: None,
                        cut: None,
                    }));
                }
                if horizon {
                    for (onset_id, target_frame) in [(4, TAKEOVER + 100), (5, TAKEOVER - 2_000)] {
                        let mut event = takeover_pad(onset_id, 1, target_frame as u64);
                        event.freq_hz = 330.0;
                        event.duration_secs = 0.5;
                        assert!(ring.push(event));
                    }
                }
            }
            let report = backend.process_block_with(
                &mut pcm[cursor * 2..(cursor + n) * 2],
                n,
                cursor as u64,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &cut_flag,
                    line_arm: &line_arm,
                },
                &stopped,
            );
            stale += report.stale;
            cursor += n;
        }
        (pcm, stale)
    };
    let peak = |pcm: &[f32], from: usize, to: usize| {
        pcm[from * 2..to * 2]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()))
    };

    let (quantised_pcm, quantised_stale) = render(TakeoverCut::AtTakeover, true, false);
    let (edit_pcm, _) = render(TakeoverCut::None, true, false);
    assert_eq!(quantised_stale, 0);

    // Mid-countdown the old score still plays. The pad rings on exactly as
    // in an edit, and the countdown beat sounds.
    const COUNT_AT: usize = FLIP + 4_800;
    let beat_in_edit = peak(&edit_pcm, COUNT_AT, COUNT_AT + 1_280);
    let beat_in_quantised = peak(&quantised_pcm, COUNT_AT, COUNT_AT + 1_280);
    assert!(
        beat_in_edit > 0.1,
        "the countdown beat sounds in the edit run: {beat_in_edit}"
    );
    assert!(
        beat_in_quantised > beat_in_edit * 0.9,
        "the countdown must play under a quantised rewind: {beat_in_quantised} vs {beat_in_edit}"
    );

    // Just BEFORE the line the pad is still sounding - the cut has not
    // landed. (A flip-cut would already have faded it 490 ms ago.)
    let before_line = peak(&quantised_pcm, TAKEOVER - 1_280, TAKEOVER - 128);
    assert!(
        before_line > 0.1,
        "the countdown's pad plays right up to the line: {before_line}"
    );

    // At the line the pad begins its fade. The reference is the edit run,
    // whose pad rings on: both runs are identical up to the line and share
    // the new loop's downbeat after it.
    let removed = |from: usize, to: usize| {
        quantised_pcm[from * 2..to * 2]
            .iter()
            .zip(&edit_pcm[from * 2..to * 2])
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()))
    };
    let identical = removed(TAKEOVER - 1_280, TAKEOVER - 128);
    assert!(
        identical < 0.001,
        "up to the line the countdown plays exactly as an edit's: {identical}"
    );
    let pad_ref = peak(&edit_pcm, TAKEOVER + RAMP + 128, TAKEOVER + 6_400);
    assert!(
        pad_ref > 0.1,
        "the edit run's pad rings across the line (the contract being contrasted): {pad_ref}"
    );
    // The fade itself, measured without the downbeat (which both runs
    // share from the line): the choke ramp from unity at the line, linear
    // over 10 ms, and silence past it.
    let (quantised_bare, _) = render(TakeoverCut::AtTakeover, false, false);
    let (edit_bare, _) = render(TakeoverCut::None, false, false);
    assert_ramp_from(
        &quantised_bare,
        &edit_bare,
        TAKEOVER,
        "the line's takeover cut",
    );
    let pad_gone = removed(TAKEOVER + RAMP + 128, TAKEOVER + 6_400);
    assert!(
        pad_gone > pad_ref * 0.5,
        "the pad is gone just past the line: {pad_gone} vs {pad_ref}"
    );
    let new_alone = peak(&quantised_pcm, TAKEOVER + 128, TAKEOVER + 1_280);
    assert!(
        new_alone > 0.05,
        "the new loop starts exactly on the line: {new_alone}"
    );
    // At its full level well past the choke ramp: the arm is still standing
    // when the downbeat activates on the line, and only the generation
    // guard keeps the new voice from being stopped 10 ms in.
    let new_full = peak(&quantised_pcm, TAKEOVER + RAMP + 128, TAKEOVER + 4_800);
    assert!(
        new_full > 0.2,
        "the new loop's downbeat is untouched by the line cut: {new_full}"
    );

    // Old ring events scheduled after the flip: one past the line is
    // refused by the ordinary takeover horizon, one before it plays the
    // countdown and stops at the line with everything else.
    let (with_events, events_stale) = render(TakeoverCut::AtTakeover, true, true);
    assert_eq!(
        events_stale, 1,
        "the old event past the line is refused, the one before it is not"
    );
    let events = pcm_minus(&with_events, &quantised_pcm);
    let before = takeover_peak(&events, TAKEOVER - 2_000 + 128, TAKEOVER - 128);
    assert!(
        before > 0.1,
        "the countdown event before the line plays: {before}"
    );
    let after = takeover_peak(&events, TAKEOVER + RAMP + 128, TAKEOVER + 6_400);
    assert!(
        after < 1e-6,
        "nothing of the old events sounds past the line's ramp: {after}"
    );
}

/// A pad event of the given generation, sounding from `target_frame`.
fn takeover_pad(onset_id: u64, generation: u64, target_frame: u64) -> rustel_audio::AudioEvent {
    rustel_audio::AudioEvent {
        onset_id,
        generation,
        ui_visuals: 0,
        target_frame,
        onset_lead: 0.0,
        freq_hz: 110.0,
        gain: 0.8,
        duration_secs: 4.0,
        controls: {
            let mut c = controls(Waveform::Sawtooth, None);
            c.envelope = rustel_audio::Envelope {
                attack_secs: 0.001,
                decay_secs: 0.001,
                sustain: 1.0,
                release_secs: 4.0,
            };
            c
        },
        sample: None,
        wavetable: None,
        synth: None,
        cut: None,
    }
}

/// Peak absolute sample over `[from, to)` frames of an interleaved buffer.
fn takeover_peak(pcm: &[f32], from: usize, to: usize) -> f32 {
    pcm[from * 2..to * 2]
        .iter()
        .fold(0.0f32, |m, s| m.max(s.abs()))
}

/// Largest per-sample difference between two renders over `[from, to)`.
fn takeover_removed(a: &[f32], b: &[f32], from: usize, to: usize) -> f32 {
    a[from * 2..to * 2]
        .iter()
        .zip(&b[from * 2..to * 2])
        .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()))
}

/// A pre-armed line cut with no replacement published: the consumer still
/// silences the outgoing rendition at the line, and a late flip still
/// installs the restarted loop.
#[test]
fn a_pre_armed_line_cut_fires_at_the_line_before_any_flip() {
    use rustel_audio::TakeoverCut;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    const SR: u32 = 48_000;
    // Block-aligned (128 frames) so the flip below lands on a cursor.
    const LINE: usize = 24_064; // ~0.5 s
    const GHOST: usize = LINE + 2_400; // the old score's beat past the line
    const LATE_FLIP: usize = LINE + 9_600; // the evaluation lands 0.2 s late
    const RAMP: usize = 480; // 10 ms
    let render = |armed: bool| {
        let ring = rustel_audio::Ring::new(8);
        assert!(ring.push(takeover_pad(1, 1, 0)));
        // The countdown's last beat, before the line: it must sound.
        assert!(ring.push(rustel_audio::AudioEvent {
            onset_id: 3,
            generation: 1,
            ui_visuals: 0,
            target_frame: (LINE - 4_800) as u64,
            onset_lead: 0.0,
            freq_hz: 220.0,
            gain: 0.8,
            duration_secs: 0.05,
            controls: controls(Waveform::Sine, None),
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }));
        let generation = AtomicU64::new(1);
        let takeover = AtomicU64::new(0);
        let line_arm = AtomicU64::new(if armed {
            ((LINE as u64) << 2) | 0b11
        } else {
            0
        });
        let cut_flag = AtomicU64::new(0);
        let stopped = AtomicBool::new(false);
        let mut backend = rustel_audio::LiveScalarBackend::new(SR, 8).expect("live backend");
        let total = LATE_FLIP + 12_000;
        let mut pcm = vec![0.0f32; total * 2];
        let mut cursor = 0usize;
        while cursor < total {
            let n = (total - cursor).min(128);
            if cursor == LINE + 128 {
                // The old score's beat AFTER the line - the ghost a restart
                // replaces - scheduled by its producer once the arm has
                // fired, so only the arm's drop horizon can refuse it.
                assert!(ring.push(rustel_audio::AudioEvent {
                    onset_id: 4,
                    generation: 1,
                    ui_visuals: 0,
                    target_frame: GHOST as u64,
                    onset_lead: 0.0,
                    freq_hz: 330.0,
                    gain: 0.8,
                    duration_secs: 0.5,
                    controls: controls(Waveform::Sine, None),
                    sample: None,
                    wavetable: None,
                    synth: None,
                    cut: None,
                }));
            }
            if armed && cursor == LATE_FLIP {
                // The late evaluation publishes the line EXACTLY, even though
                // it is behind the clock, and the restarted loop's downbeat
                // aimed at the line activates past due at the flip.
                takeover.store(LINE as u64, Ordering::Release);
                cut_flag.store(TakeoverCut::AtTakeover as u64, Ordering::Release);
                // The device clears the arm as it publishes (`set_generation`):
                // the flip's own intent carries the handoff from here.
                line_arm.store(0, Ordering::Release);
                generation.store(2, Ordering::Release);
                assert!(ring.push(rustel_audio::AudioEvent {
                    onset_id: 2,
                    generation: 2,
                    ui_visuals: 0,
                    target_frame: LINE as u64,
                    onset_lead: 0.0,
                    freq_hz: 440.0,
                    gain: 0.8,
                    duration_secs: 1.0,
                    controls: controls(Waveform::Sine, None),
                    sample: None,
                    wavetable: None,
                    synth: None,
                    cut: None,
                }));
            }
            backend.process_block_with(
                &mut pcm[cursor * 2..(cursor + n) * 2],
                n,
                cursor as u64,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &cut_flag,
                    line_arm: &line_arm,
                },
                &stopped,
            );
            cursor += n;
        }
        pcm
    };

    let armed_pcm = render(true);
    let plain_pcm = render(false);

    // Up to the line the two renders are the same music: the countdown
    // plays whole, its last beat included.
    let identical = takeover_removed(&armed_pcm, &plain_pcm, 0, LINE - 128);
    assert!(
        identical < 0.001,
        "the arm changes nothing before the line: {identical}"
    );
    let count_beat = takeover_peak(&armed_pcm, LINE - 4_800, LINE - 4_800 + 1_280);
    assert!(
        count_beat > 0.1,
        "the countdown's last beat sounds under the arm: {count_beat}"
    );

    // AT the line the pad fades from unity - not a step - and is gone past
    // the ramp, with no replacement published at all.
    let pad_ref = takeover_peak(&plain_pcm, LINE + RAMP + 128, GHOST - 128);
    assert!(pad_ref > 0.1, "the un-armed pad rings on: {pad_ref}");
    assert_ramp_from(&armed_pcm, &plain_pcm, LINE, "the pre-armed line cut");
    let after_ramp = takeover_peak(&armed_pcm, LINE + RAMP + 128, GHOST - 128);
    assert!(
        after_ramp < 0.01,
        "the outgoing rendition is silent past the line's ramp: {after_ramp}"
    );

    // The ghost beat after the line is dropped by the arm's horizon: the
    // un-armed run sounds it, the armed run does not.
    let ghost_ref = takeover_peak(&plain_pcm, GHOST + 128, GHOST + 1_280);
    assert!(
        ghost_ref > 0.1,
        "the ghost sounds when nothing is armed: {ghost_ref}"
    );
    let ghost_armed = takeover_peak(&armed_pcm, GHOST + 128, GHOST + 1_280);
    assert!(
        ghost_armed < 0.01,
        "the old score's beat past the line never sounds under the arm: {ghost_armed}"
    );

    // The late flip lands: the restarted loop's downbeat, aimed at the
    // line, activates past due at the flip and is heard.
    let restart = takeover_peak(&armed_pcm, LATE_FLIP + 128, LATE_FLIP + 4_800);
    assert!(
        restart > 0.1,
        "the late flip still installs the restarted loop: {restart}"
    );
}

/// A countdown onset that activates in the block that reaches the line
/// belongs to the outgoing generation and must fade at the line.
#[test]
fn an_onset_activating_in_the_arm_block_is_cut_at_the_line() {
    use std::sync::atomic::{AtomicBool, AtomicU64};

    const SR: u32 = 48_000;
    const LINE: usize = 24_000;
    const RAMP: usize = 480;
    let render = |armed: bool| {
        let ring = rustel_audio::Ring::new(8);
        // The consumer runs 128-frame blocks from 0, so the block that
        // reaches the line is [23_936, 24_064): the arm fires at its start
        // and this onset activates inside it, after the arm.
        assert!(ring.push(rustel_audio::AudioEvent {
            onset_id: 5,
            generation: 1,
            ui_visuals: 0,
            target_frame: (LINE - 64) as u64,
            onset_lead: 0.0,
            freq_hz: 330.0,
            gain: 0.8,
            duration_secs: 1.0,
            controls: controls(Waveform::Sine, None),
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }));
        let generation = AtomicU64::new(1);
        let takeover = AtomicU64::new(0);
        let line_arm = AtomicU64::new(if armed {
            ((LINE as u64) << 2) | 0b11
        } else {
            0
        });
        let cut_flag = AtomicU64::new(0);
        let stopped = AtomicBool::new(false);
        let mut backend = rustel_audio::LiveScalarBackend::new(SR, 8).expect("live backend");
        let total = LINE + 24_000;
        let mut pcm = vec![0.0f32; total * 2];
        let mut cursor = 0usize;
        while cursor < total {
            let n = (total - cursor).min(128);
            backend.process_block_with(
                &mut pcm[cursor * 2..(cursor + n) * 2],
                n,
                cursor as u64,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &cut_flag,
                    line_arm: &line_arm,
                },
                &stopped,
            );
            cursor += n;
        }
        pcm
    };

    let armed_pcm = render(true);
    let plain_pcm = render(false);
    let rings = takeover_peak(&plain_pcm, LINE + RAMP + 128, LINE + 9_600);
    assert!(
        rings > 0.1,
        "un-armed, the onset rings on past the line: {rings}"
    );
    let sounds = takeover_peak(&armed_pcm, LINE - 64, LINE);
    assert!(
        sounds > 0.0,
        "the onset does start before the line: {sounds}"
    );
    let cut = takeover_peak(&armed_pcm, LINE + RAMP + 128, LINE + 9_600);
    assert!(
        cut < 0.01,
        "an outgoing onset from the arm's own block is faded at the line: {cut}"
    );
}

/// A quantised flip published after its takeover frame fades the outgoing
/// rendition from the flip over the choke ramp, not as a step to silence.
#[test]
fn a_late_quantised_flip_fades_from_the_flip_not_a_step() {
    use rustel_audio::TakeoverCut;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    const SR: u32 = 48_000;
    const FLIP: usize = 24_064; // block-aligned so the flip lands on a cursor
    const PAST_LINE: usize = FLIP - 4_800; // the line the flip missed
    const RAMP: usize = 480;
    let render = |intent: TakeoverCut| {
        let ring = rustel_audio::Ring::new(8);
        assert!(ring.push(takeover_pad(1, 1, 0)));
        let generation = AtomicU64::new(1);
        let takeover = AtomicU64::new(0);
        let line_arm = AtomicU64::new(0);
        let cut_flag = AtomicU64::new(0);
        let stopped = AtomicBool::new(false);
        let mut backend = rustel_audio::LiveScalarBackend::new(SR, 8).expect("live backend");
        let total = FLIP + 12_000;
        let mut pcm = vec![0.0f32; total * 2];
        let mut cursor = 0usize;
        while cursor < total {
            let n = (total - cursor).min(128);
            if cursor == FLIP {
                takeover.store(PAST_LINE as u64, Ordering::Release);
                cut_flag.store(intent as u64, Ordering::Release);
                generation.store(2, Ordering::Release);
            }
            backend.process_block_with(
                &mut pcm[cursor * 2..(cursor + n) * 2],
                n,
                cursor as u64,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &cut_flag,
                    line_arm: &line_arm,
                },
                &stopped,
            );
            cursor += n;
        }
        pcm
    };

    let late_pcm = render(TakeoverCut::AtTakeover);
    let edit_pcm = render(TakeoverCut::None);
    let pad_ref = takeover_peak(&edit_pcm, FLIP + RAMP + 128, FLIP + 6_400);
    assert!(
        pad_ref > 0.1,
        "the edit run's pad rings across the flip: {pad_ref}"
    );
    let before = takeover_removed(&late_pcm, &edit_pcm, FLIP - 1_280, FLIP);
    assert!(before < 0.001, "nothing changes before the flip: {before}");
    // The ramp starts at unity from the flip, not at zero from the past
    // line.
    assert_ramp_from(&late_pcm, &edit_pcm, FLIP, "a late quantised flip");
    let gone = takeover_peak(&late_pcm, FLIP + RAMP + 128, FLIP + 6_400);
    assert!(
        gone < 0.01,
        "and the outgoing pad is gone past the ramp: {gone}"
    );
}

/// A launch whose evaluation fails or is cancelled AFTER its line has cut
/// the room is withdrawn without a flip. The consumer must let the fired
/// arm go: the outgoing rendition's later events pass again (the room
/// comes back rather than staying dead until some later edit), and the
/// NEXT launch's arm fires at its own line. A latch reset only by a flip
/// left the second rewind's countdown playing through its line.
#[test]
fn a_withdrawn_arm_lets_the_room_resume_and_the_next_arm_fire() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    const SR: u32 = 48_000;
    // The stores below land on 128-frame block cursors.
    const LINE: usize = 24_064;
    const WITHDRAWN_AT: usize = LINE + 2_432; // the failed launch is withdrawn
    const PUSH_AT: usize = LINE + 4_096; // the producer schedules the old score on
    const RESUME: usize = LINE + 4_736; // the old score's next event
    const LINE2: usize = LINE + 14_400; // the second launch's line
    const RAMP: usize = 480;
    let render = |withdraw_then_rearm: bool| {
        let ring = rustel_audio::Ring::new(8);
        assert!(ring.push(takeover_pad(1, 1, 0)));
        let generation = AtomicU64::new(1);
        let takeover = AtomicU64::new(0);
        let line_arm = AtomicU64::new(((LINE as u64) << 2) | 0b11);
        let cut_flag = AtomicU64::new(0);
        let stopped = AtomicBool::new(false);
        let mut backend = rustel_audio::LiveScalarBackend::new(SR, 8).expect("live backend");
        let total = LINE2 + 9_600;
        let mut pcm = vec![0.0f32; total * 2];
        let mut cursor = 0usize;
        while cursor < total {
            let n = (total - cursor).min(128);
            if withdraw_then_rearm && cursor == WITHDRAWN_AT {
                line_arm.store(rustel_audio::LINE_ARM_WITHDRAWN, Ordering::Release);
            }
            if cursor == PUSH_AT {
                // The outgoing generation is still the room: its producer
                // keeps scheduling it past the line. (What it had scheduled
                // BEFORE the line was retired by the arm, as it should be.)
                assert!(ring.push(takeover_pad(2, 1, RESUME as u64)));
            }
            if withdraw_then_rearm && cursor == RESUME {
                // The second quantised rewind arms its own line.
                line_arm.store(((LINE2 as u64) << 2) | 0b11, Ordering::Release);
            }
            backend.process_block_with(
                &mut pcm[cursor * 2..(cursor + n) * 2],
                n,
                cursor as u64,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &cut_flag,
                    line_arm: &line_arm,
                },
                &stopped,
            );
            cursor += n;
        }
        pcm
    };

    let pcm = render(true);
    let still_armed = render(false);

    // The first line cut fired: the first pad is gone past its ramp.
    let cut_once = takeover_peak(&pcm, LINE + RAMP + 128, WITHDRAWN_AT);
    assert!(
        cut_once < 0.01,
        "the first arm cut the room at its line: {cut_once}"
    );
    // Withdrawn: the old score's next event passes and the room resumes.
    let resumed = takeover_peak(&pcm, RESUME + 128, LINE2 - 128);
    assert!(
        resumed > 0.1,
        "after the withdrawal the outgoing rendition resumes: {resumed}"
    );
    let held = takeover_peak(&still_armed, RESUME + 128, LINE2 - 128);
    assert!(
        held < 0.01,
        "with the arm standing, its drop horizon still refuses the event: {held}"
    );
    // The second arm fires at its own line.
    let second_cut = takeover_peak(&pcm, LINE2 + RAMP + 128, LINE2 + 9_600);
    assert!(
        second_cut < 0.01,
        "the next launch's arm fires at its line: {second_cut}"
    );
}

/// A generation can be skipped on its way to the device: a slider requery
/// armed G+1, a launch superseded it before it published, and the device
/// flips G straight to G+2. The countdown onsets still belong to G. The
/// activation pre-fade was keyed to `new - 1` (G+1), so an outgoing onset
/// activating between the flip and the line rang on under the restart.
#[test]
fn a_quantised_cut_after_a_skipped_generation_still_fades_the_countdown() {
    use rustel_audio::TakeoverCut;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    const SR: u32 = 48_000;
    const FLIP: usize = 9_600;
    const LATE_ONSET: usize = FLIP + 4_800; // activates after the flip
    const TAKEOVER: usize = FLIP + 24_000; // the line
    const RAMP: usize = 480;
    let render = |intent: TakeoverCut| {
        let ring = rustel_audio::Ring::new(8);
        assert!(ring.push(takeover_pad(1, 1, LATE_ONSET as u64)));
        let generation = AtomicU64::new(1);
        let takeover = AtomicU64::new(0);
        let line_arm = AtomicU64::new(0);
        let cut_flag = AtomicU64::new(0);
        let stopped = AtomicBool::new(false);
        let mut backend = rustel_audio::LiveScalarBackend::new(SR, 8).expect("live backend");
        let total = TAKEOVER + 12_000;
        let mut pcm = vec![0.0f32; total * 2];
        let mut cursor = 0usize;
        while cursor < total {
            let n = (total - cursor).min(128);
            if cursor == FLIP {
                takeover.store(TAKEOVER as u64, Ordering::Release);
                cut_flag.store(intent as u64, Ordering::Release);
                // 1 -> 3: generation 2 never published.
                generation.store(3, Ordering::Release);
            }
            backend.process_block_with(
                &mut pcm[cursor * 2..(cursor + n) * 2],
                n,
                cursor as u64,
                &ring,
                LiveFlipAtomics {
                    generation: &generation,
                    takeover_frame: &takeover,
                    takeover_cut: &cut_flag,
                    line_arm: &line_arm,
                },
                &stopped,
            );
            cursor += n;
        }
        pcm
    };

    let cut_pcm = render(TakeoverCut::AtTakeover);
    let edit_pcm = render(TakeoverCut::None);
    let sounds = takeover_peak(&cut_pcm, LATE_ONSET + 128, TAKEOVER - 128);
    assert!(
        sounds > 0.1,
        "the countdown onset plays up to the line: {sounds}"
    );
    let rings = takeover_peak(&edit_pcm, TAKEOVER + RAMP + 128, TAKEOVER + 9_600);
    assert!(rings > 0.1, "under an edit it rings past the line: {rings}");
    let gone = takeover_peak(&cut_pcm, TAKEOVER + RAMP + 128, TAKEOVER + 9_600);
    assert!(
        gone < 0.01,
        "the outgoing onset is faded at the line even across a skipped generation: {gone}"
    );
}

/// Render an immediate rewind's flip at `FLIP` with the restarted loop's
/// downbeat aimed at `downbeat` (possibly already behind the flip), over an
/// old pad; returns the interleaved buffer.
fn render_restart(downbeat: u64, duration_secs: f32) -> Vec<f32> {
    use rustel_audio::TakeoverCut;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    const SR: u32 = 48_000;
    const FLIP: usize = 24_064;
    let ring = rustel_audio::Ring::new(8);
    assert!(ring.push(takeover_pad(1, 1, 0)));
    let generation = AtomicU64::new(1);
    let takeover = AtomicU64::new(0);
    let line_arm = AtomicU64::new(0);
    let cut_flag = AtomicU64::new(0);
    let stopped = AtomicBool::new(false);
    let mut backend = rustel_audio::LiveScalarBackend::new(SR, 8).expect("live backend");
    let total = FLIP + 12_000;
    let mut pcm = vec![0.0f32; total * 2];
    let mut cursor = 0usize;
    while cursor < total {
        let n = (total - cursor).min(128);
        if cursor == FLIP {
            takeover.store(downbeat, Ordering::Release);
            cut_flag.store(TakeoverCut::AtFlip as u64, Ordering::Release);
            generation.store(2, Ordering::Release);
            assert!(ring.push(rustel_audio::AudioEvent {
                onset_id: 2,
                generation: 2,
                ui_visuals: 0,
                target_frame: downbeat,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 0.8,
                duration_secs,
                controls: {
                    let mut c = controls(Waveform::Sine, None);
                    c.envelope = rustel_audio::Envelope {
                        attack_secs: 0.001,
                        decay_secs: 0.001,
                        sustain: 1.0,
                        release_secs: 0.001,
                    };
                    c
                },
                sample: None,
                wavetable: None,
                synth: None,
                cut: None,
            }));
        }
        backend.process_block_with(
            &mut pcm[cursor * 2..(cursor + n) * 2],
            n,
            cursor as u64,
            &ring,
            LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &cut_flag,
                line_arm: &line_arm,
            },
            &stopped,
        );
        cursor += n;
    }
    pcm
}

/// A restart's downbeat reaches the consumer behind the flip. The consumer
/// admits it at the flip, so the voice renders whole, attack included.
#[test]
fn a_restarts_past_due_downbeat_renders_whole_from_the_flip() {
    const FLIP: usize = 24_064;
    let on_time = render_restart(FLIP as u64, 0.25);
    let past_due = render_restart((FLIP - 480) as u64, 0.25);
    let differs = takeover_removed(&on_time, &past_due, FLIP, FLIP + 12_000);
    assert!(
        differs < 1e-6,
        "a downbeat 10 ms behind the flip renders exactly as one aimed at it: {differs}"
    );
    let attack = takeover_peak(&past_due, FLIP, FLIP + 480);
    assert!(
        attack > 0.1,
        "the downbeat sounds from its attack: {attack}"
    );

    // A 5 ms one-shot aimed 20 ms behind the flip still sounds.
    let short = render_restart((FLIP - 960) as u64, 0.005);
    let heard = takeover_peak(&short, FLIP, FLIP + 480);
    let pad_only = render_restart((FLIP - 960) as u64, 0.0);
    let pad_level = takeover_peak(&pad_only, FLIP, FLIP + 480);
    assert!(
        heard > pad_level + 0.1,
        "a short one-shot later than its own length is not lost: {heard} vs pad fade {pad_level}"
    );
}

/// The live consumer's handshake, driven offline by a test: the ring and
/// the four atomics the producer thread shares with the callback.
struct LiveRig {
    ring: rustel_audio::Ring,
    generation: std::sync::atomic::AtomicU64,
    takeover: std::sync::atomic::AtomicU64,
    cut: std::sync::atomic::AtomicU64,
    line_arm: std::sync::atomic::AtomicU64,
    stopped: std::sync::atomic::AtomicBool,
}

impl LiveRig {
    fn new() -> Self {
        Self {
            ring: rustel_audio::Ring::new(16),
            generation: std::sync::atomic::AtomicU64::new(1),
            takeover: std::sync::atomic::AtomicU64::new(0),
            cut: std::sync::atomic::AtomicU64::new(0),
            line_arm: std::sync::atomic::AtomicU64::new(0),
            stopped: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Publish a flip in the producer's order: takeover and cut before the
    /// generation.
    fn flip(&self, generation: u64, takeover: u64, cut: rustel_audio::TakeoverCut) {
        use std::sync::atomic::Ordering;
        self.takeover.store(takeover, Ordering::Release);
        self.cut.store(cut as u64, Ordering::Release);
        self.line_arm.store(0, Ordering::Release);
        self.generation.store(generation, Ordering::Release);
    }
}

/// Render `total` frames through a fresh live consumer in 128-frame blocks
/// from frame 0. `at_block(cursor, rig)` runs before each block - the
/// producer's side - and each block's report is handed to `on_report`.
fn render_live(
    total: usize,
    at_block: impl FnMut(usize, &LiveRig),
    on_report: impl FnMut(usize, rustel_audio::LiveBlockReport),
) -> Vec<f32> {
    render_live_with(total, |_| {}, at_block, on_report)
}

/// [`render_live`] with `prepare` run on the fresh consumer first (an
/// input ring, say).
fn render_live_with(
    total: usize,
    prepare: impl FnOnce(&mut rustel_audio::LiveScalarBackend),
    mut at_block: impl FnMut(usize, &LiveRig),
    mut on_report: impl FnMut(usize, rustel_audio::LiveBlockReport),
) -> Vec<f32> {
    let rig = LiveRig::new();
    let mut backend = rustel_audio::LiveScalarBackend::new(48_000, 8).expect("live backend");
    prepare(&mut backend);
    let mut pcm = vec![0.0f32; total * 2];
    let mut cursor = 0usize;
    while cursor < total {
        let n = (total - cursor).min(128);
        at_block(cursor, &rig);
        let report = backend.process_block_with(
            &mut pcm[cursor * 2..(cursor + n) * 2],
            n,
            cursor as u64,
            &rig.ring,
            LiveFlipAtomics {
                generation: &rig.generation,
                takeover_frame: &rig.takeover,
                takeover_cut: &rig.cut,
                line_arm: &rig.line_arm,
            },
            &rig.stopped,
        );
        on_report(cursor, report);
        cursor += n;
    }
    pcm
}

/// A long 440 Hz sine of the given generation and choke group, from
/// `target_frame`: loud everywhere within any 32-frame window, so a ramp's
/// gain can be read off it.
fn choke_tone(
    onset_id: u64,
    generation: u64,
    target_frame: u64,
    cut: Option<f32>,
) -> rustel_audio::AudioEvent {
    rustel_audio::AudioEvent {
        onset_id,
        generation,
        ui_visuals: 0,
        target_frame,
        onset_lead: 0.0,
        freq_hz: 440.0,
        gain: 0.8,
        duration_secs: 2.0,
        controls: controls(Waveform::Sine, None),
        sample: None,
        wavetable: None,
        synth: None,
        cut,
    }
}

/// Per-sample difference `a - b` over the whole interleaved buffer.
fn pcm_minus(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x - y).collect()
}

/// Assert `faded` is `reference` under a linear 10 ms ramp starting at
/// `from`: the gain removed around `from + k` is `k / 480`, the voice is
/// silent past the ramp, and nothing before `from` changed.
fn assert_ramp_from(faded: &[f32], reference: &[f32], from: usize, what: &str) {
    let before = takeover_removed(faded, reference, from - 1_280, from);
    assert!(
        before < 1e-6,
        "{what}: nothing changes before the fade: {before}"
    );
    for k in [16usize, 48, 120, 240, 360, 440] {
        let level = takeover_peak(reference, from + k - 16, from + k + 16);
        let removed = takeover_removed(faded, reference, from + k - 16, from + k + 16);
        let ratio = removed / level;
        let expected = k as f32 / 480.0;
        assert!(
            (ratio - expected).abs() < 0.15,
            "{what}: {k} frames into the fade {ratio:.3} of the level is gone, a ramp removes {expected:.3}"
        );
    }
    let tail = takeover_peak(faded, from + 480 + 64, from + 4_800);
    assert!(tail < 1e-4, "{what}: silent past the ramp: {tail}");
}

/// A trigger that arrives after its own onset still chokes the previous
/// voice of its group, over the ramp from the block it arrives in.
#[test]
fn a_late_trigger_chokes_its_group_mate_from_the_block_not_the_past() {
    const ARRIVES: usize = 24_064; // block-aligned
    const LATE_ONSET: u64 = (ARRIVES - 960) as u64; // 20 ms behind
    const TOTAL: usize = ARRIVES + 9_600;
    let render = |mate: bool, trigger: bool| {
        render_live(
            TOTAL,
            |cursor, rig| {
                if cursor == 0 && mate {
                    assert!(rig.ring.push(choke_tone(1, 1, 0, Some(1.0))));
                }
                if cursor == ARRIVES && trigger {
                    assert!(rig.ring.push(choke_tone(2, 1, LATE_ONSET, Some(1.0))));
                }
            },
            |_, _| {},
        )
    };
    let both = render(true, true);
    let trigger_alone = render(false, true);
    let mate_alone = render(true, false);
    // Voices sum: what the group-mate contributes under the choke is the
    // mix minus the trigger alone.
    let mate_choked = pcm_minus(&both, &trigger_alone);
    assert!(
        takeover_peak(&mate_alone, ARRIVES - 480, ARRIVES) > 0.1,
        "the group-mate sounds up to the late trigger"
    );
    assert_ramp_from(&mate_choked, &mate_alone, ARRIVES, "late choke");
}

/// A rewind's cut-group downbeat lands while the takeover cut fades the
/// outgoing group-mate. The choke must leave that ramp unchanged.
#[test]
fn a_rewinds_cut_group_downbeat_keeps_the_running_takeover_ramp() {
    use rustel_audio::TakeoverCut;

    const FLIP: usize = 24_064; // block-aligned
    const DOWNBEAT: u64 = (FLIP + 100) as u64; // inside the takeover's ramp
    const TOTAL: usize = FLIP + 9_600;
    let render = |mate: bool, downbeat_cut: Option<f32>| {
        render_live(
            TOTAL,
            |cursor, rig| {
                if cursor == 0 && mate {
                    assert!(rig.ring.push(choke_tone(1, 1, 0, Some(1.0))));
                }
                if cursor == FLIP {
                    rig.flip(2, FLIP as u64, TakeoverCut::AtFlip);
                    let mut downbeat = choke_tone(2, 2, DOWNBEAT, downbeat_cut);
                    downbeat.freq_hz = 660.0;
                    assert!(rig.ring.push(downbeat));
                }
            },
            |_, _| {},
        )
    };
    let downbeat_alone = render(false, Some(1.0));
    let in_group = pcm_minus(&render(true, Some(1.0)), &downbeat_alone);
    let out_of_group = pcm_minus(&render(true, None), &downbeat_alone);
    // The group-mate unfaded: the same flip published as an edit, which
    // rings out.
    let edit_reference = render_live(
        TOTAL,
        |cursor, rig| {
            if cursor == 0 {
                assert!(rig.ring.push(choke_tone(1, 1, 0, Some(1.0))));
            }
            if cursor == FLIP {
                rig.flip(2, FLIP as u64, TakeoverCut::None);
            }
        },
        |_, _| {},
    );
    assert_ramp_from(&out_of_group, &edit_reference, FLIP, "takeover cut alone");
    let differs = takeover_removed(&in_group, &out_of_group, 0, TOTAL);
    assert!(
        differs < 1e-6,
        "a downbeat in the group changes nothing about the takeover's ramp: {differs}"
    );
    assert_ramp_from(
        &in_group,
        &edit_reference,
        FLIP,
        "takeover cut with a choke",
    );
}

/// An edit's flip between a quantised rewind's flip and its line keeps the
/// rewind's armed cut. A countdown onset that activates after the edit's
/// flip must still pre-fade at the line.
#[test]
fn an_edit_flip_inside_the_head_room_keeps_the_rewinds_line_cut() {
    use rustel_audio::TakeoverCut;

    // Flips land on 128-frame block cursors.
    const FLIP: usize = 9_600; // the rewind publishes, AtTakeover
    const LINE: usize = FLIP + 24_000;
    const REQUERY: usize = LINE - 2_368; // a slider requery publishes, None
    // The requery's takeover is its publication plus a continuity margin:
    // past the countdown hit, so the requery keeps it pending.
    const REQUERY_TAKEOVER: u64 = (REQUERY + 4_800) as u64;
    const COUNTDOWN_AT: u64 = (LINE - 1_440) as u64; // activates after the requery
    const OWN_AT: u64 = (LINE - 1_000) as u64;
    const RAMP: usize = 480;
    const TOTAL: usize = LINE + 9_600;
    // `edit` publishes the rewind as an edit (no cut at all): the ringing
    // reference. `own` adds an event of the rewind's own generation just
    // before the line - not outgoing, never pre-faded.
    let render = |rewind: TakeoverCut, requery: bool, own: bool| {
        render_live(
            TOTAL,
            |cursor, rig| {
                if cursor == 0 {
                    // The countdown's last hit: scheduled long before, with
                    // a tail that crosses the line.
                    let mut hit = choke_tone(1, 1, COUNTDOWN_AT, None);
                    hit.duration_secs = 0.5;
                    assert!(rig.ring.push(hit));
                }
                if cursor == FLIP {
                    rig.flip(2, LINE as u64, rewind);
                    if own {
                        let mut event = choke_tone(2, 2, OWN_AT, None);
                        event.freq_hz = 660.0;
                        event.duration_secs = 0.5;
                        assert!(rig.ring.push(event));
                    }
                }
                if requery && cursor == REQUERY {
                    rig.flip(3, REQUERY_TAKEOVER, TakeoverCut::None);
                }
            },
            |_, _| {},
        )
    };

    let edit = render(TakeoverCut::None, true, false);
    let rings = takeover_peak(&edit, LINE + RAMP + 128, LINE + 6_400);
    assert!(
        rings > 0.1,
        "under an edit the countdown hit rings past the line: {rings}"
    );
    let no_requery = render(TakeoverCut::AtTakeover, false, false);
    let cut_alone = takeover_peak(&no_requery, LINE + RAMP + 128, LINE + 6_400);
    assert!(
        cut_alone < 1e-4,
        "the rewind alone fades it at the line: {cut_alone}"
    );

    let requeried = render(TakeoverCut::AtTakeover, true, false);
    let sounds = takeover_peak(&requeried, COUNTDOWN_AT as usize + 128, LINE - 128);
    assert!(
        sounds > 0.1,
        "the countdown hit plays up to the line: {sounds}"
    );
    let past_line = takeover_peak(&requeried, LINE + RAMP + 128, LINE + 6_400);
    assert!(
        past_line < rings * 0.01,
        "a requery's flip inside the head-room keeps the line cut: {past_line} vs {rings}"
    );
    let identical = takeover_removed(&requeried, &no_requery, 0, TOTAL);
    assert!(
        identical < 1e-6,
        "the requery's flip changes nothing about the rewind's cut: {identical}"
    );

    // The rewind's own generation is not outgoing: the kept arm leaves it
    // alone.
    let with_own = render(TakeoverCut::AtTakeover, true, true);
    let own_alone = pcm_minus(&with_own, &requeried);
    let own_rings = takeover_peak(&own_alone, LINE + RAMP + 128, LINE + 6_400);
    assert!(
        own_rings > 0.1,
        "an event of the rewind's own generation is not pre-faded: {own_rings}"
    );
}

/// The same requery, but published early enough that its takeover (its
/// publication plus a continuity margin) lands BEFORE the countdown's last
/// hit. The requery re-queries the restarted score, which has nothing
/// before the line, so an edit's ordinary horizon applied to the countdown
/// retired that hit from pending, or refused it from the ring: moving a
/// slider in the last bar muted the rest of the countdown. The rewind's
/// outgoing generation keeps its own horizon, the line, while the arm is
/// ahead: the hit sounds up to the line and is silent past the ramp,
/// whether it was already admitted or still in the ring, exactly as with no
/// requery. Every other generation keeps the edit's horizon: the rewind's
/// own event past the requery's takeover is replaced by it, and a ghost of
/// the countdown past the line is still refused.
#[test]
fn an_edit_flip_before_the_countdown_hit_keeps_it_to_the_line() {
    use rustel_audio::TakeoverCut;

    // Flips land on 128-frame block cursors.
    const FLIP: usize = 9_600; // the rewind publishes, AtTakeover
    const LINE: usize = FLIP + 24_000;
    const REQUERY: usize = LINE - 2_368; // a slider requery publishes, None
    const REQUERY_TAKEOVER: u64 = (REQUERY + 480) as u64;
    const COUNTDOWN_AT: u64 = (LINE - 1_440) as u64;
    const OWN_AT: u64 = (LINE - 1_000) as u64;
    const GHOST_AT: u64 = (LINE + 960) as u64;
    const RAMP: usize = 480;
    const TOTAL: usize = LINE + 9_600;
    const { assert!(REQUERY_TAKEOVER < COUNTDOWN_AT && COUNTDOWN_AT < LINE as u64) };

    let hit = || {
        let mut hit = choke_tone(1, 1, COUNTDOWN_AT, None);
        hit.duration_secs = 0.5;
        hit
    };
    let own = || {
        let mut event = choke_tone(2, 2, OWN_AT, None);
        event.freq_hz = 660.0;
        event.duration_secs = 0.5;
        event
    };
    let ghost = || {
        let mut event = choke_tone(3, 1, GHOST_AT, None);
        event.freq_hz = 330.0;
        event
    };
    // `via_ring` leaves the countdown hit (and the rewind's own event) in
    // the ring until the requery's block, where they are drained under its
    // flip; otherwise both are admitted long before. `with_own` adds the
    // rewind's own event; `with_ghost` a countdown event past the line,
    // still in the ring at the requery.
    let render = |requery: bool, via_ring: bool, with_own: bool, with_ghost: bool| {
        let mut stale = 0usize;
        let pcm = render_live(
            TOTAL,
            |cursor, rig| {
                if cursor == 0 && !via_ring {
                    assert!(rig.ring.push(hit()));
                }
                if cursor == FLIP {
                    rig.flip(2, LINE as u64, TakeoverCut::AtTakeover);
                    if with_own && !via_ring {
                        assert!(rig.ring.push(own()));
                    }
                }
                if cursor == REQUERY {
                    if via_ring {
                        assert!(rig.ring.push(hit()));
                        if with_own {
                            assert!(rig.ring.push(own()));
                        }
                    }
                    if with_ghost {
                        assert!(rig.ring.push(ghost()));
                    }
                    if requery {
                        rig.flip(3, REQUERY_TAKEOVER, TakeoverCut::None);
                    }
                }
            },
            |_, report| stale += report.stale,
        );
        (pcm, stale)
    };

    // The hit's ringing level: the same countdown with no rewind at all.
    let unrewound = render_live(
        TOTAL,
        |cursor, rig| {
            if cursor == 0 {
                assert!(rig.ring.push(hit()));
            }
        },
        |_, _| {},
    );
    let rings = takeover_peak(&unrewound, LINE + RAMP + 128, LINE + 6_400);
    assert!(
        rings > 0.1,
        "left alone the hit rings past the line: {rings}"
    );

    for via_ring in [false, true] {
        let path = if via_ring {
            "from the ring"
        } else {
            "admitted"
        };
        let (no_requery, _) = render(false, via_ring, false, false);
        let (requeried, stale) = render(true, via_ring, false, false);
        assert_eq!(stale, 0, "{path}: nothing of the countdown is refused");
        let sounds = takeover_peak(&requeried, COUNTDOWN_AT as usize + 128, LINE - 128);
        assert!(
            sounds > 0.1,
            "{path}: the countdown hit plays up to the line under the requery: {sounds}"
        );
        let past_line = takeover_peak(&requeried, LINE + RAMP + 128, LINE + 6_400);
        assert!(
            past_line < rings * 0.01,
            "{path}: the hit is silent past the line's ramp: {past_line} vs {rings}"
        );
        let identical = takeover_removed(&requeried, &no_requery, 0, TOTAL);
        assert!(
            identical < 1e-6,
            "{path}: the requery's flip changes nothing about the countdown: {identical}"
        );

        // The rewind's own generation is the requery's to replace from its
        // takeover, not the countdown's line.
        let (with_own, _) = render(true, via_ring, true, false);
        let own_left = takeover_removed(&with_own, &requeried, 0, TOTAL);
        assert!(
            own_left < 1e-6,
            "{path}: the rewind's own event past the requery's takeover is replaced: {own_left}"
        );
    }

    // A countdown event past the line is the restart's ghost: refused.
    let (with_ghost, ghost_stale) = render(true, false, false, true);
    let (requeried, _) = render(true, false, false, false);
    assert_eq!(ghost_stale, 1, "the ghost past the line is refused");
    let ghost_left = takeover_removed(&with_ghost, &requeried, 0, TOTAL);
    assert!(ghost_left < 1e-6, "and never sounds: {ghost_left}");
}

/// A line between two block boundaries fades from the line itself, not
/// from the start of the block that reaches it: the fire runs at the block
/// start, but the ramp belongs to the frame the launch named.
#[test]
fn a_line_inside_a_block_fades_from_the_line() {
    const LINE: u64 = 24_050; // inside [23_936, 24_064)
    const TOTAL: usize = 24_064 + 9_600;
    let render = |armed: bool| {
        render_live(
            TOTAL,
            |cursor, rig| {
                if cursor == 0 {
                    assert!(rig.ring.push(takeover_pad(1, 1, 0)));
                    if armed {
                        rig.line_arm
                            .store((LINE << 2) | 0b11, std::sync::atomic::Ordering::Release);
                    }
                }
            },
            |_, _| {},
        )
    };
    assert_ramp_from(
        &render(true),
        &render(false),
        LINE as usize,
        "a mid-block line",
    );
}

/// An arm whose frame is already behind the consumer (a launch armed late)
/// fires in the next block and fades from there, over the ramp: the
/// outgoing rendition does not step to silence for a line it has passed.
#[test]
fn an_arm_behind_the_cursor_fades_from_the_next_block() {
    const ARMED_AT: usize = 24_064; // block-aligned
    const LINE: u64 = (ARMED_AT - 1_000) as u64;
    const TOTAL: usize = ARMED_AT + 9_600;
    let render = |armed: bool| {
        render_live(
            TOTAL,
            |cursor, rig| {
                if cursor == 0 {
                    assert!(rig.ring.push(takeover_pad(1, 1, 0)));
                }
                if armed && cursor == ARMED_AT {
                    rig.line_arm
                        .store((LINE << 2) | 0b11, std::sync::atomic::Ordering::Release);
                }
            },
            |_, _| {},
        )
    };
    assert_ramp_from(
        &render(true),
        &render(false),
        ARMED_AT,
        "an arm behind the cursor",
    );
}

/// The arm fires once. After it, the outgoing generation may still schedule
/// on (a launch whose flip has not landed): with the drop bit clear its
/// events are neither refused nor cut by a re-fire - they sound whole. The
/// same event under the drop bit is refused as the ghost window.
#[test]
fn a_fired_arm_fires_once_and_its_drop_bit_decides_the_ring() {
    use std::sync::atomic::Ordering;

    const LINE: u64 = 24_064;
    const LATER: u64 = LINE + 9_600;
    const TOTAL: usize = LATER as usize + 12_000;
    let render = |word: u64, later: bool| {
        let mut stale = 0usize;
        let pcm = render_live(
            TOTAL,
            |cursor, rig| {
                if cursor == 0 {
                    assert!(rig.ring.push(takeover_pad(1, 1, 0)));
                    rig.line_arm.store(word, Ordering::Release);
                }
                if later && cursor == LINE as usize + 1_024 {
                    let mut event = takeover_pad(2, 1, LATER);
                    event.freq_hz = 330.0;
                    event.duration_secs = 0.2;
                    assert!(rig.ring.push(event));
                }
            },
            |_, report| stale += report.stale,
        );
        (pcm, stale)
    };

    let no_drop = (LINE << 2) | 0b01;
    let (with_later, stale) = render(no_drop, true);
    let (without_later, _) = render(no_drop, false);
    assert_eq!(stale, 0, "with the drop bit clear nothing is refused");
    let later = pcm_minus(&with_later, &without_later);
    let whole = takeover_peak(&later, LATER as usize + 4_800, LATER as usize + 9_000);
    assert!(
        whole > 0.1,
        "the later event sounds on for its whole length, not cut by a re-fire: {whole}"
    );
    // The line cut itself still fired: the pad is gone past its ramp.
    let pad = takeover_peak(&without_later, LINE as usize + 544, LATER as usize);
    assert!(pad < 1e-4, "the arm fired at its line: {pad}");

    let (dropped, dropped_stale) = render((LINE << 2) | 0b11, true);
    assert_eq!(
        dropped_stale, 1,
        "with the drop bit the later event is refused"
    );
    let silent = takeover_peak(&dropped, LINE as usize + 544, TOTAL);
    assert!(silent < 1e-4, "and never sounds: {silent}");
}

/// A live input voice (`s("in")`) over a rewind: the monitored signal is a
/// constant level, so the mix's level at every frame is the sum of the
/// input voices' gains. `restart_at` places the restarted loop's own
/// `s("in")` window (None: the new loop opens none at the flip).
fn render_input_rewind(cut: rustel_audio::TakeoverCut, restart_at: Option<u64>) -> Vec<f32> {
    use rustel_audio::input::InputRing;

    const FLIP: usize = 24_064;
    let ring = std::sync::Arc::new(InputRing::new());
    ring.set_channels(1);
    ring.set_sample_rate(48_000);
    // A second of a constant input, written ahead: every read is 0.5.
    ring.write(&[0.5f32; 96_000], 1);
    let window = |onset_id, generation, target_frame| rustel_audio::AudioEvent {
        onset_id,
        generation,
        ui_visuals: 0,
        target_frame,
        onset_lead: 0.0,
        freq_hz: 0.0,
        gain: 0.8,
        duration_secs: 2.0,
        controls: controls(Waveform::Sine, None),
        sample: None,
        wavetable: None,
        synth: Some(rustel_audio::SynthSource::Input { channel: 0 }),
        cut: None,
    };
    render_live_with(
        FLIP + 9_600,
        |backend| backend.set_input(Some(std::sync::Arc::clone(&ring))),
        |cursor, rig| {
            if cursor == 0 {
                assert!(rig.ring.push(window(1, 1, 0)));
            }
            if cursor == FLIP {
                rig.flip(2, FLIP as u64, cut);
                if let Some(at) = restart_at {
                    assert!(rig.ring.push(window(2, 2, at)));
                }
            }
        },
        |_, _| {},
    )
}

/// A rewind fades the old `s("in")` window over the choke ramp while the new
/// one opens. The monitored level has no gap and no doubling.
#[test]
fn a_rewind_hands_the_live_input_over_without_a_gap_or_a_double() {
    use rustel_audio::TakeoverCut;

    const FLIP: usize = 24_064;
    let level = |pcm: &[f32], at: usize| pcm[at * 2].abs();
    let rewind = render_input_rewind(TakeoverCut::AtFlip, Some(FLIP as u64));
    let before = level(&rewind, FLIP - 100);
    assert!(before > 0.1, "the input sounds before the rewind: {before}");
    for frame in FLIP - 480..FLIP + 960 {
        let ratio = level(&rewind, frame) / before;
        assert!(
            ratio >= 0.9,
            "the monitored input dips to {ratio:.3} of its level at frame {frame}"
        );
    }
    for frame in [FLIP + 544, FLIP + 4_800, FLIP + 9_000] {
        let ratio = level(&rewind, frame) / before;
        assert!(
            (ratio - 1.0).abs() < 1e-3,
            "past the ramp the input is one window again, not two: {ratio:.3} at {frame}"
        );
    }
    let edit = render_input_rewind(TakeoverCut::None, Some(FLIP as u64));
    let doubled = level(&edit, FLIP + 4_800) / level(&edit, FLIP - 100);
    assert!(
        (doubled - 2.0).abs() < 1e-3,
        "an uncut old window rings on beside the new one: {doubled:.3}"
    );
}

/// A live input voice takes its postgain exactly once, from the chain tap
/// that every source shares. `.postgain(0.5)` on `s("in")` halves the level.
#[test]
fn input_postgain_scales_the_monitored_signal_once() {
    use rustel_audio::input::InputRing;

    let level = |postgain: f32| -> f32 {
        let ring = std::sync::Arc::new(InputRing::new());
        ring.set_channels(1);
        ring.set_sample_rate(48_000);
        // A constant input, written ahead: every read is 0.5.
        ring.write(&[0.5f32; 96_000], 1);
        let pcm = render_live_with(
            9_600,
            |backend| backend.set_input(Some(std::sync::Arc::clone(&ring))),
            |cursor, rig| {
                if cursor == 0 {
                    let mut c = controls(Waveform::Sine, None);
                    c.postgain = postgain;
                    assert!(rig.ring.push(rustel_audio::AudioEvent {
                        onset_id: 1,
                        generation: 1,
                        ui_visuals: 0,
                        target_frame: 0,
                        onset_lead: 0.0,
                        freq_hz: 0.0,
                        gain: 0.8,
                        duration_secs: 2.0,
                        controls: c,
                        sample: None,
                        wavetable: None,
                        synth: Some(rustel_audio::SynthSource::Input { channel: 0 }),
                        cut: None,
                    }));
                }
            },
            |_, _| {},
        );
        // Steady state, long past the attack.
        pcm[4_800 * 2].abs()
    };
    let full = level(1.0);
    let half = level(0.5);
    assert!(full > 1e-3, "the fixture must monitor a live level: {full}");
    let ratio = half / full;
    assert!(
        (ratio - 0.5).abs() < 1e-3,
        "postgain 0.5 must halve the voice once, not twice: ratio {ratio:.3}"
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
fn an_orbit_insert_processes_the_notes_that_ask_for_the_effect() {
    use rustel_audio::{InsertControls, InsertKey, InsertParam, InsertProvider, OrbitInsert};
    use std::sync::Arc;

    let key = InsertKey {
        plugin: 7,
        preset: 0,
    };
    // One effect for each gain, in chain order.
    let note = |orbit: u8, gains: &[f32]| {
        let mut controls = controls(Waveform::Sine, None);
        controls.orbit = orbit;
        for (effect, value) in controls.effects.iter_mut().zip(gains) {
            let mut insert = InsertControls::new(key);
            assert!(insert.push(InsertParam {
                id: 1,
                value: *value
            }));
            *effect = Some(insert);
        }
        OnsetEvent::new(0, 440.0, 0.5, 0.05).with_controls(controls)
    };
    let provider: Arc<InsertProvider> = Arc::new(move |wanted, _sample_rate, _orbit| {
        (wanted == key).then(|| Box::new(GainInsert { key, gain: 1.0 }) as Box<dyn OrbitInsert>)
    });
    let render = |events: &[OnsetEvent], provider: Option<&Arc<InsertProvider>>| {
        let mut backend = ScalarBackend::new();
        if let Some(provider) = provider {
            backend.set_insert_provider(Arc::clone(provider));
        }
        let pcm = render_pcm(&mut backend, 48_000, 4_800, events).expect("render");
        (pcm, backend.missing_insert_events())
    };

    let (dry, _) = render(&[note(1, &[])], None);
    assert!(dry.iter().any(|sample| sample.abs() > 0.01));
    let (halved, missed) = render(&[note(1, &[0.5])], Some(&provider));
    assert_eq!(missed, 0);
    for (frame, (wet, dry)) in halved.iter().zip(&dry).enumerate() {
        assert_eq!(*wet, dry * 0.5, "sample {frame}");
    }

    // A second effect takes the output of the first.
    let (chained, missed) = render(&[note(1, &[0.5, 0.5])], Some(&provider));
    assert_eq!(missed, 0);
    for (frame, (wet, dry)) in chained.iter().zip(&dry).enumerate() {
        assert_eq!(*wet, dry * 0.25, "sample {frame}");
    }

    // Only the note that asks goes through the insert. A note on the same
    // orbit with no request, and a note on orbit 2, keep the level.
    let (muted_one, _) = render(&[note(1, &[0.0]), note(1, &[])], Some(&provider));
    assert_eq!(muted_one, dry);
    let (muted_one, _) = render(&[note(1, &[0.0]), note(2, &[])], Some(&provider));
    let (only_two, _) = render(&[note(2, &[])], None);
    assert_eq!(muted_one, only_two);

    // With no provider the note plays dry, and the miss is counted.
    let (unserved, missed) = render(&[note(1, &[0.5])], None);
    assert_eq!(unserved, dry);
    assert_eq!(missed, 1);
}

/// An instrument insert with a long silent start: one click 31 seconds into
/// its note. `busy` is true while the note holds.
struct LateClick {
    key: rustel_audio::InsertKey,
    busy: bool,
    frames_left: u32,
    click_at: u32,
}

impl rustel_audio::OrbitInsert for LateClick {
    fn key(&self) -> rustel_audio::InsertKey {
        self.key
    }

    fn set_param(&mut self, _param: rustel_audio::InsertParam, _frames: u32) {}

    fn note(&mut self, note: rustel_audio::InsertNote, _frames: u32) {
        self.frames_left = note.frames;
    }

    fn busy(&self) -> bool {
        self.busy && self.frames_left > 0
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (left, right) in left.iter_mut().zip(right) {
            if self.frames_left > 0 {
                self.frames_left -= 1;
                let click = self.frames_left == self.click_at;
                (*left, *right) = if click { (1.0, 1.0) } else { (0.0, 0.0) };
            }
        }
    }
}

/// An instrument that holds a note stays awake through 30 seconds of
/// silence. An instrument with no note to hold sleeps, and the engine voice
/// of an instrument note is silent.
#[test]
fn an_instrument_insert_that_holds_a_note_stays_awake() {
    use rustel_audio::{InsertControls, InsertKey, InsertNote, InsertProvider, OrbitInsert};
    use std::sync::Arc;

    const RATE: u32 = 8_000;
    let key = InsertKey {
        plugin: 9,
        preset: 0,
    };
    let frames = 32 * RATE;
    let note = InsertNote {
        pitch: 60.0,
        velocity: 1.0,
        frames,
    };
    let mut controls = controls(Waveform::Sine, None);
    controls.instrument = Some(InsertControls::new(key).with_note(note));
    let event = OnsetEvent::new(0, 440.0, 0.0, 32.0).with_controls(controls);
    let render = |busy: bool| {
        let provider: Arc<InsertProvider> = Arc::new(move |key, _rate, _slot| {
            Some(Box::new(LateClick {
                key,
                busy,
                frames_left: 0,
                click_at: RATE,
            }) as Box<dyn OrbitInsert>)
        });
        let mut backend = ScalarBackend::new();
        backend.set_insert_provider(provider);
        let pcm = render_pcm(&mut backend, RATE, frames as usize, &[event]).expect("render");
        pcm.iter().filter(|sample| sample.abs() > 0.1).count()
    };
    // The click is one stereo frame, 31 seconds into the note.
    assert_eq!(render(true), 2);
    assert_eq!(render(false), 0);
}
