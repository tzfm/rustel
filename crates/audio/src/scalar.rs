//! Scalar synthesis, sample playback and effects for offline and live audio.

use crate::DspDispatch;
use crate::backend::{
    AudioBackend, BusMod, CompressorControls, EnvMod, Envelope, FmControls, FxStage, LfoMod,
    MAX_VOICE_MODS, OnsetEvent, OscillatorControls, PartialsControls, PhaserControls,
    PitchEnvControls, ShapeControls, TremoloControls, VibratoControls, VowelControls, Waveform,
};
use crate::sample::{
    DecodedSample, SampleBank, SampleId, SampleResamplingMode, bundled_sample, stop_secs_for,
};
use crate::wavetable_kernel::WavetableKernel;
use std::sync::Arc;

use crate::biquad::FilterChain;
use crate::periodic_wave::{PeriodicWaveTables, TableSelection, prepared_tables};
#[cfg(feature = "device-audio")]
use crate::pressure::RealtimePressureObservation;
use crate::pressure::{REALTIME_POOL_COUNT, RealtimePool};

/// On x86_64, flush subnormal inputs and results to zero so quiet filter and
/// reverb tails avoid denormal arithmetic costs. These flags are per-thread;
/// set them at every `process_block` because init may run on another thread.
/// Miri has no inline assembly, so a Miri run keeps the default flags.
fn enable_denormal_flush() {
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    {
        const FTZ: u32 = 1 << 15;
        const DAZ: u32 = 1 << 6;
        let mut csr = 0u32;
        // SAFETY: `stmxcsr`/`ldmxcsr` only read and write this thread's MXCSR.
        unsafe {
            std::arch::asm!(
                "stmxcsr [{csr}]",
                csr = in(reg) &mut csr,
                options(nostack, preserves_flags)
            );
            let flushed = csr | FTZ | DAZ;
            if csr != flushed {
                csr = flushed;
                std::arch::asm!(
                    "ldmxcsr [{csr}]",
                    csr = in(reg) &csr,
                    options(nostack, preserves_flags)
                );
            }
        }
    }
}

#[cfg(test)]
mod preview_replacement_tests {
    use super::*;

    fn note(frame: u64, frequency: f32, epoch: u64) -> OnsetEvent {
        let mut event = OnsetEvent::new(frame, frequency, 0.1, 3.0);
        event.controls.preview_epoch = epoch;
        event
    }

    #[test]
    fn replacing_preview_fades_only_older_preview_voices_at_the_new_onset() {
        let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
        backend.note(note(0, 220.0, 0)); // the user's score
        backend.note(note(0, 330.0, 1));
        backend.note(note(0, 440.0, 1)); // a chord, not a choke group
        backend.note(note(512, 550.0, 2));
        backend.note(note(512, 660.0, 2));
        let mut out = [0.0; 256];
        for _ in 0..4 {
            backend.process_block(&mut out, 128);
        }
        assert_eq!(backend.voices.len(), 3);
        assert!(backend.voices.iter().all(|v| v.cut_fade_frame.is_none()));
        backend.process_block(&mut out, 128);
        assert_eq!(backend.voices.len(), 5);
        for voice in &backend.voices {
            assert_eq!(
                voice.cut_fade_frame,
                (voice.preview_epoch == 1).then_some(512)
            );
        }
        // Even a late outgoing onset cannot cut the new preview or revive the old one.
        backend.note(note(640, 770.0, 1));
        for _ in 0..6 {
            backend.process_block(&mut out, 128);
        }
        assert_eq!(backend.voices.len(), 3);
        assert!(backend.voices.iter().all(|v| v.preview_epoch != 1));
        assert!(backend.voices.iter().all(|v| v.cut_fade_frame.is_none()));
        assert!(out.iter().all(|v| v.is_finite()));
        assert!(out.iter().any(|v| v.abs() > 0.001));
    }
}

#[cfg(test)]
mod held_piano_tests {
    use super::*;

    fn held(frequency: f32, group: Option<f32>) -> OnsetEvent {
        let mut event = OnsetEvent::new(0, frequency, 0.1, f32::INFINITY).with_cut(group);
        event.controls.waveform = Waveform::Triangle;
        event.controls.envelope = Envelope {
            attack_secs: 0.005,
            decay_secs: 0.1,
            sustain: 0.7,
            release_secs: 0.02,
        };
        event.controls.lfo_end_secs = f32::INFINITY;
        event.controls.filter_lfo_end_secs = f32::INFINITY;
        event
    }

    fn release(frame: u64, group: f32) -> OnsetEvent {
        let mut event = OnsetEvent::new(frame, 440.0, 0.0, 0.01).with_cut(Some(group));
        event.controls.choke_only = true;
        event
    }

    fn advance(backend: &mut ScalarBackend, blocks: usize) {
        let mut out = [0.0; 256];
        for _ in 0..blocks {
            backend.process_block(&mut out, 128);
            assert!(out.iter().all(|sample| sample.is_finite()));
        }
        assert!(out.iter().any(|sample| sample.abs() > 0.0001));
    }

    #[test]
    fn held_piano_chord_survives_long_hold_and_releases_only_its_key() {
        let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
        backend.note(held(110.0, None)); // score
        backend.note(held(220.0, Some(f32::MAX))); // browser preview
        backend.note(held(330.0, Some(100.0))); // physical piano key one
        backend.note(held(440.0, Some(101.0))); // physical piano key two
        advance(&mut backend, 2000); // 5.3s: beyond every browser preview gate
        assert_eq!(backend.voices.len(), 4);
        assert!(
            backend
                .voices
                .iter()
                .all(|voice| voice.cut_fade_frame.is_none())
        );
        backend.note(release(backend.frame, 100.0));
        advance(&mut backend, 16);
        assert_eq!(backend.voices.len(), 3);
        assert!(!backend.voices.iter().any(|voice| voice.freq_hz == 330.0));
        assert!(backend.voices.iter().any(|voice| voice.freq_hz == 440.0));
        backend.note(release(backend.frame, 101.0));
        advance(&mut backend, 16);
        assert_eq!(backend.voices.len(), 2);
        assert!(
            backend
                .voices
                .iter()
                .all(|voice| voice.cut_fade_frame.is_none())
        );
    }

    #[test]
    fn held_piano_key_release_never_steals_a_voice_at_full_polyphony() {
        for limit in [2, 4] {
            let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
            backend.set_max_polyphony(limit);
            backend.note(held(110.0, None)); // oldest score voice
            if limit == 4 {
                backend.note(held(220.0, Some(f32::MAX))); // browser
                backend.note(held(330.0, Some(100.0))); // other piano key
            }
            backend.note(held(440.0, Some(101.0))); // key being released
            advance(&mut backend, 8);
            assert_eq!(backend.voices.len(), limit);
            backend.note(release(backend.frame, 101.0));
            advance(&mut backend, 1);
            assert_eq!(
                backend.voices.len(),
                limit,
                "release must not add a silent voice"
            );
            assert!(
                backend
                    .voices
                    .iter()
                    .all(|voice| voice.polyphony_fade_frame.is_none())
            );
            advance(&mut backend, 16);
            assert_eq!(backend.voices.len(), limit - 1);
            assert!(
                backend
                    .voices
                    .iter()
                    .all(|voice| voice.cut_fade_frame.is_none())
            );
            assert!(backend.voices.iter().any(|voice| voice.freq_hz == 110.0));
        }
    }

    #[test]
    fn held_piano_release_bypasses_full_prepared_voice_and_pending_capacity() {
        let mut backend = ScalarBackend::prepared(48_000, 2).unwrap();
        backend.set_max_polyphony(2);
        assert!(backend.try_note_prepared(held(110.0, None)));
        assert!(backend.try_note_prepared(held(440.0, Some(1.0))));
        advance(&mut backend, 1);
        assert_eq!(backend.voices.len(), 2);
        // Normal admission would refuse this because all prepared voice
        // capacity is occupied. A key-up is not a third voice.
        assert!(backend.try_note_prepared(release(backend.frame, 1.0)));
        advance(&mut backend, 16);
        assert_eq!(backend.voices.len(), 1);
        assert_eq!(backend.voices[0].freq_hz, 110.0);
        assert!(backend.voices[0].polyphony_fade_frame.is_none());

        backend.reset();
        for (freq, group) in [(110.0, None), (440.0, Some(1.0))] {
            let mut event = held(freq, group);
            event.onset_frame = 128;
            assert!(backend.try_note_prepared(event));
        }
        assert_eq!(backend.pending.len(), backend.pending.capacity());
        assert!(backend.try_note_prepared(release(129, 1.0)));
        advance(&mut backend, 18);
        assert_eq!(backend.voices.len(), 1);
        assert_eq!(backend.voices[0].freq_hz, 110.0);
    }

    #[test]
    fn held_piano_pending_notes_survive_score_retirement_and_rewind() {
        let mut backend = ScalarBackend::prepared(48_000, 4).unwrap();
        let mut event = held(440.0, Some(1.0));
        event.onset_frame = 1024;
        event.generation = 4;
        event.controls.piano = true;
        backend.note(event);
        backend.retire_pending_from(512, 0);
        assert_eq!(backend.pending.len(), 1);
        backend.begin_takeover_cut(1024, 4);
        advance(&mut backend, 16);
        assert_eq!(backend.voices.len(), 1);
        assert!(backend.voices[0].cut_fade_frame.is_none());
        backend.begin_takeover_cut(backend.frame, 5);
        advance(&mut backend, 16);
        assert_eq!(backend.voices.len(), 1);
        backend.note(release(backend.frame, 1.0));
        let mut out = [0.0; 256];
        for _ in 0..16 {
            backend.process_block(&mut out, 128);
        }
        assert!(backend.voices.is_empty());
        assert!(
            out.iter()
                .all(|sample| sample.is_finite() && *sample == 0.0)
        );
    }

    #[test]
    fn held_piano_release_cannot_exhaust_choke_groups() {
        let mut backend = ScalarBackend::prepared(48_000, 128).unwrap();
        // More simultaneous groups than the old device-lifetime registry.
        for group in 0..96 {
            backend.note(held(110.0 + group as f32, Some(group as f32)));
        }
        advance(&mut backend, 1);
        assert_eq!(backend.voices.len(), 96);
        backend.note(release(backend.frame, 95.0));
        advance(&mut backend, 16);
        assert_eq!(backend.voices.len(), 95);
        assert!(
            backend
                .voices
                .iter()
                .all(|voice| voice.cut_group != Some(95.0f32.to_bits()))
        );
        assert!(
            backend
                .voices
                .iter()
                .all(|voice| voice.cut_fade_frame.is_none())
        );
    }
}

#[cfg(test)]
mod ui_visual_mix_tests {
    use super::*;
    use crate::backend::{AudioBackend, OnsetEvent};

    fn render(first_frequency: f32) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let mut backend = ScalarBackend::prepared(48_000, 16).expect("backend");
        backend.set_ui_visual_capture_mask(0b11);
        backend.note(OnsetEvent::new(0, first_frequency, 0.2, 0.1).with_ui_visuals(0b01));
        backend.note(OnsetEvent::new(0, 440.0, 0.4, 0.1).with_ui_visuals(0b10));
        let mut output = vec![0.0; UI_VISUAL_MIX_FRAMES * 2];
        backend.process_block(&mut output, UI_VISUAL_MIX_FRAMES);
        (
            output,
            backend.ui_visual_mix(0, UI_VISUAL_MIX_FRAMES).to_vec(),
            backend.ui_visual_mix(1, UI_VISUAL_MIX_FRAMES).to_vec(),
        )
    }

    #[test]
    fn visual_audio_contains_only_voices_tagged_for_that_receiver() {
        let (output, first, second) = render(220.0);
        assert!(first.iter().any(|sample| sample.abs() > 1e-5));
        assert!(second.iter().any(|sample| sample.abs() > 1e-5));
        for ((mixed, first), second) in output.iter().zip(&first).zip(&second) {
            assert!((mixed - first - second).abs() < 1e-6);
        }

        let (_, _, second_after_sibling_change) = render(880.0);
        assert_eq!(second, second_after_sibling_change);
    }
}

#[cfg(test)]
mod feedback_line_tests {
    use super::{OrbitDelay, storable};

    /// A feedback line must not store a `NaN`. The write is `input +
    /// feedback * delayed`, so a stored non-finite sample never decays.
    #[test]
    fn a_feedback_line_forgets_a_bad_sample_instead_of_circulating_it_forever() {
        assert_eq!(storable(f32::NAN), 0.0);
        assert_eq!(storable(f32::INFINITY), 0.0);
        assert_eq!(storable(f32::NEG_INFINITY), 0.0);
        assert_eq!(storable(-0.75), -0.75, "an ordinary sample is untouched");

        // Run the line the way the mixer does, with one poisoned sample in
        // the middle of an otherwise silent input.
        let frames = 512;
        let mut line = vec![0.0f32; frames];
        let mut write = 0usize;
        let feedback = 0.98;
        let delay_frames = 200.0;
        for frame in 0..4_000 {
            let input = if frame == 10 { f32::NAN } else { 0.0 };
            let delayed = OrbitDelay::read(&line, write, delay_frames);
            line[write] = storable(input + feedback * delayed);
            write = (write + 1) % frames;
        }
        assert!(
            line.iter().all(|sample| sample.is_finite()),
            "the line is still carrying something that is not a number"
        );

        // And it can carry sound again afterwards, which is what "it never
        // plays again" was really about.
        for frame in 0..600 {
            let input = if frame < 64 { 0.5 } else { 0.0 };
            let delayed = OrbitDelay::read(&line, write, delay_frames);
            line[write] = storable(input + feedback * delayed);
            write = (write + 1) % frames;
        }
        assert!(
            line.iter().any(|sample| sample.abs() > 0.1),
            "the line stayed dead after the bad sample"
        );
    }

    #[test]
    fn an_orbit_delay_stays_energized_through_its_silent_gap_until_overwritten() {
        let mut delay = OrbitDelay {
            left: vec![0.0; 4],
            right: vec![0.0; 4],
            active: true,
            ..OrbitDelay::default()
        };
        delay.store(0.5, 0.0);
        assert_eq!(delay.energized, 1);

        for _ in 0..3 {
            delay.store(0.0, 0.0);
            assert_eq!(
                delay.energized, 1,
                "silence before the echo must not look like a drained line"
            );
        }
        delay.store(0.0, 0.0);
        assert_eq!(delay.energized, 0, "the last audible cell was overwritten");
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    #[test]
    fn external_fx_reverbs_adopt_the_receiving_backend_dispatch() {
        let mut backend = ScalarBackend::with_dispatch(DspDispatch::portable());
        let reverb = crate::reverb::OrbitReverb::generate_streaming(
            24_000,
            crate::reverb::ReverbParams {
                ir: None,
                size_secs: 0.2,
                fade_secs: 0.01,
                lp_start_hz: 8_000.0,
                lp_end_hz: 1_000.0,
            },
        );
        assert!(!reverb.dispatch().is_forced_portable());
        assert!(backend.install_fx_reverb(Box::new(reverb)).is_none());
        assert!(backend.fx_reverb_pool[0].dispatch().is_forced_portable());
    }
}

#[cfg(test)]
mod voice_layout_tests {
    use super::{
        FmState, FxStageStates, PreparedSupersaw, ScalarBackend, Voice, VoiceRenderControls,
    };
    use crate::backend::{
        AudioBackend, FilterControls, FmControls, FmOperator, FmRoute, FmWave, FxStage,
        MAX_FM_OPERATORS, MAX_FM_ROUTES, MAX_FX_STAGES, OnsetEvent, OscillatorControls,
        ShapeControls, StaticBiquad, VowelControls,
    };

    fn prepared_pre_distort_activity(controls: OscillatorControls) -> bool {
        let mut backend = ScalarBackend::prepared(48_000, 1).expect("backend");
        backend.note(OnsetEvent::new(0, 220.0, 0.5, 0.1).with_controls(controls));
        backend.process_block(&mut [0.0; 2], 1);
        backend.voices[0].has_pre_distort_fx
    }

    #[test]
    fn optional_state_stays_out_of_the_hot_voice_layout() {
        assert!(std::mem::size_of::<FxStageStates>() > 4 * 1024);
        assert!(std::mem::size_of::<FmState>() > 512);
        assert_eq!(
            std::mem::size_of::<Option<Box<FxStageStates>>>(),
            std::mem::size_of::<usize>()
        );
        assert_eq!(
            std::mem::size_of::<Option<Box<FmState>>>(),
            std::mem::size_of::<usize>()
        );
        assert!(
            std::mem::size_of::<Voice>() <= 5 * 1024,
            "the per-sample voice stride grew beyond 5 KiB"
        );
        assert!(
            std::mem::size_of::<VoiceRenderControls>()
                + std::mem::size_of::<[Option<FxStage>; MAX_FX_STAGES]>()
                < std::mem::size_of::<OscillatorControls>(),
            "activation-only or pooled FX controls leaked back into the hot voice stride"
        );
    }

    fn fm_controls() -> OscillatorControls {
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
        OscillatorControls {
            fm: Some(FmControls { operators, routes }),
            ..OscillatorControls::default()
        }
    }

    #[test]
    fn fm_state_is_leased_only_where_used_and_recycled() {
        let mut backend = ScalarBackend::prepared(48_000, 2).expect("backend");
        assert_eq!(backend.fm_state_pool.len(), 2);

        assert!(backend.try_note_prepared(OnsetEvent::new(0, 220.0, 0.5, 0.1)));
        assert!(
            backend.try_note_prepared(
                OnsetEvent::new(0, 330.0, 0.5, 0.1).with_controls(fm_controls())
            )
        );
        backend.process_block(&mut [0.0; 2], 1);

        assert_eq!(backend.voices.len(), 2);
        assert_eq!(
            backend
                .voices
                .iter()
                .filter(|voice| voice.fm_state.is_some())
                .count(),
            1
        );
        assert_eq!(backend.fm_state_pool.len(), 1);

        backend.reset_at(0);
        assert!(backend.voices.is_empty());
        assert_eq!(backend.fm_state_pool.len(), 2);

        for frequency in [440.0, 550.0] {
            assert!(backend.try_note_prepared(
                OnsetEvent::new(0, frequency, 0.5, 0.1).with_controls(fm_controls())
            ));
        }
        backend.process_block(&mut [0.0; 2], 1);

        assert_eq!(backend.voices.len(), 2);
        assert!(backend.voices.iter().all(|voice| voice.fm_state.is_some()));
        assert!(backend.fm_state_pool.is_empty());
    }

    #[test]
    fn supersaw_plan_preserves_the_original_static_lane_math() {
        for (voices, freqspread, panspread) in [
            (1.0_f32, 0.0_f32, 0.0_f32),
            (5.0, 0.6, 0.6),
            (8.0, 0.35, 0.8),
            (32.0, 1.25, -0.2),
            (64.0, 0.25, 1.2),
        ] {
            let plan = PreparedSupersaw::new(voices, freqspread, panspread);
            let voices = voices.max(1.0);
            let expected_count = (voices.ceil() as usize).clamp(1, super::MAX_UNISON);
            assert_eq!(plan.count, expected_count);

            let spread = freqspread.max(0.0);
            let (scale, center) = if voices < 2.0 {
                (0.0, 0.0)
            } else {
                (spread / (voices - 1.0), spread * 0.5)
            };
            for (lane, actual) in plan.detune_ratios.iter().take(expected_count).enumerate() {
                let detune = lane as f32 * scale - center;
                let expected = 2.0_f32.powf(detune / 12.0);
                assert_eq!(actual.to_bits(), expected.to_bits());
            }

            let panspread01 = panspread * 0.5 + 0.5;
            let expected = ((1.0 - panspread01).sqrt(), panspread01.sqrt());
            assert_eq!(plan.pan_gains.0.to_bits(), expected.0.to_bits());
            assert_eq!(plan.pan_gains.1.to_bits(), expected.1.to_bits());
        }
    }

    #[test]
    fn filter_activity_is_prepared_when_the_voice_activates() {
        let mut backend = ScalarBackend::prepared(48_000, 1).expect("backend");

        backend.note(OnsetEvent::new(0, 220.0, 0.5, 0.1));
        backend.process_block(&mut [0.0; 2], 1);
        assert!(!backend.voices[0].has_filters);

        backend.reset_at(0);
        let controls = OscillatorControls {
            filters: FilterControls {
                lowpass: Some(StaticBiquad {
                    frequency_hz: 800.0,
                    q: 1.0,
                }),
                ..FilterControls::default()
            },
            ..OscillatorControls::default()
        };
        backend.note(OnsetEvent::new(0, 220.0, 0.5, 0.1).with_controls(controls));
        backend.process_block(&mut [0.0; 2], 1);
        assert!(backend.voices[0].has_filters);
    }

    #[test]
    fn pre_distort_activity_is_prepared_when_the_voice_activates() {
        assert!(!prepared_pre_distort_activity(OscillatorControls::default()));
        assert!(prepared_pre_distort_activity(OscillatorControls {
            vowel: Some(VowelControls {
                freqs: [300.0; 5],
                gains: [1.0; 5],
                qs: [1.0; 5],
            }),
            ..OscillatorControls::default()
        }));
        assert!(prepared_pre_distort_activity(OscillatorControls {
            coarse: Some(2.0),
            ..OscillatorControls::default()
        }));
        assert!(prepared_pre_distort_activity(OscillatorControls {
            crush: Some(6.0),
            ..OscillatorControls::default()
        }));
        assert!(prepared_pre_distort_activity(OscillatorControls {
            shape: Some(ShapeControls {
                shape: 0.5,
                postgain: 1.0,
            }),
            ..OscillatorControls::default()
        }));
    }
}

/// One shared feedback delay per orbit: a stereo one-second line. The send
/// taps the chain after the panner, and each triggering voice resets its time
/// and feedback.
#[derive(Clone, Debug, Default)]
struct OrbitDelay {
    left: Vec<f32>,
    right: Vec<f32>,
    write: usize,
    time_frames: usize,
    feedback: f32,
    active: bool,
    /// Stereo cells whose stored energy can still produce an audible echo.
    /// Maintaining this as the ring is overwritten avoids scanning a full
    /// one-second line in the realtime callback.
    energized: usize,
}

/// What a feedback line is allowed to remember.
///
/// A delay line is the one place in the graph where a single bad sample is
/// permanent: the write is `input + feedback * delayed`, so once a `NaN`
/// lands in the buffer it reads back as `NaN`, and `NaN * feedback` is `NaN`
/// however far below one the feedback is clamped. The line never decays, the
/// orbit never sounds again, and taking the offending score back out changes
/// nothing - the state that is broken is not in the score.
///
/// So nothing but a real number is stored. Whatever produced it is a bug
/// worth fixing at the source; this is what keeps that bug an audible glitch
/// rather than the end of the set.
#[inline]
fn storable(sample: f32) -> f32 {
    if sample.is_finite() { sample } else { 0.0 }
}

impl OrbitDelay {
    const SILENCE_FLOOR: f32 = 0.000_001;

    /// DelayNode linearly interpolates an a-rate delay between adjacent
    /// samples. A node in a feedback cycle cannot read inside the current
    /// quantum, so the effective delay is at least 128 frames.
    #[inline]
    fn read(line: &[f32], write: usize, delay_frames: f32) -> f32 {
        let len = line.len();
        let delay_frames = delay_frames.clamp(128.0, (len - 1) as f32);
        let whole = delay_frames.floor() as usize;
        let fraction = delay_frames - whole as f32;
        let newer = (write + len - whole) % len;
        let older = (newer + len - 1) % len;
        line[older] * fraction + line[newer] * (1.0 - fraction)
    }

    #[inline]
    fn store(&mut self, left: f32, right: f32) {
        let left = storable(left);
        let right = storable(right);
        for (old, new) in [
            (self.left[self.write], left),
            (self.right[self.write], right),
        ] {
            self.energized = self
                .energized
                .saturating_sub(usize::from(old.abs() > Self::SILENCE_FLOOR))
                .saturating_add(usize::from(new.abs() > Self::SILENCE_FLOOR));
        }
        self.left[self.write] = left;
        self.right[self.write] = right;
        self.write = (self.write + 1) % self.left.len();
    }
}

/// One `.FX()` stage's inline feedback delay.
///
/// An `.FX()` stage carries a feedback delay of its own instead of the
/// orbit send, so this is per voice, not per orbit. Pooled and allocated at
/// init like the compressor:
/// a 1-second stereo line is 384 KiB at 48 kHz and must never be allocated in
/// the callback.
#[derive(Clone, Debug)]
struct FxDelayLine {
    left: Vec<f32>,
    right: Vec<f32>,
    write: usize,
}

impl FxDelayLine {
    fn new(sample_rate: u32) -> Self {
        // The line is fixed at a 1-second maximum, and `delaytime` is
        // clamped to that at resolve.
        let frames = sample_rate.max(1) as usize + 1;
        Self {
            left: vec![0.0; frames],
            right: vec![0.0; frames],
            write: 0,
        }
    }

    fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write = 0;
    }
}

/// How much memory `.FX()` stage reverbs may hold between them.
///
/// Bounded in BYTES, not instances, because a convolver's cost is set by its
/// roomsize and a count-based cap has no idea what it is holding: measured
/// resident cost is 2.7 MiB at roomsize 0.5, 7.0 MiB at the default 2, and
/// 24.8 MiB at 6, so sixteen of them is 43 MiB or 397 MiB depending entirely
/// on the pattern.
///
/// CPU is the lesser limit - one stereo reverb costs 18 us per 128-frame
/// block against a 2667 us budget, so about 37 could run inside a sane 25%
/// share - which is why this bound is the memory one.
///
/// A voice that finds none plays dry and is counted, exactly like an orbit
/// whose reverb has not arrived. Refusing is the same never-die trade as
/// the voice and queue ceilings; an unbounded convolver-per-note policy
/// would hand one pattern hundreds of MiB.
pub(crate) const MAX_FX_REVERB_BYTES: usize = 64 * 1024 * 1024;

// Each stereo reverb accounts for at least one partition, matching history,
// work and accumulator per channel, even for an empty IR. Scratch and tails
// only increase that minimum. Reserve return slots for every box the existing
// byte budget can admit, including boxes currently leased to voices.
const MIN_FX_REVERB_BYTES: usize = 2
    * 4
    * (2 * crate::reverb::REVERB_BLOCK)
    * std::mem::size_of::<rustfft::num_complex::Complex<f32>>();
const MAX_INSTALLED_FX_REVERBS: usize = MAX_FX_REVERB_BYTES / MIN_FX_REVERB_BYTES;

#[allow(clippy::vec_box)]
fn reserve_fx_reverb_returns(pool: &mut Vec<Box<crate::reverb::OrbitReverb>>, inline_count: usize) {
    let target = MAX_INSTALLED_FX_REVERBS
        .checked_add(inline_count)
        .expect("FX reverb return capacity overflow");
    if pool.capacity() < target {
        pool.reserve(target - pool.len());
    }
}

/// Orbit indices in common use; sends beyond this refuse loudly
/// at resolve time rather than aliasing into a wrong bus.
pub const MAX_ORBITS: usize = 16;
/// An insert with no input and no output for this long sleeps. The time is
/// long, so the late echo of a delay effect still sounds.
const INSERT_IDLE_SECONDS: u32 = 30;
/// An insert output below this level is silence: about -120 dB.
const INSERT_SILENCE: f32 = 1e-6;
/// Frames of per-orbit mix kept for routing: the largest block a host asks
/// for in one go.
pub const ORBIT_MIX_FRAMES: usize = 4096;
pub const MAX_UI_AUDIO_VISUALS: usize = 64;
pub const UI_VISUAL_MIX_FRAMES: usize = crate::reverb::REVERB_BLOCK;

/// Concurrent-voice ceiling. The per-frame mix is O(voices × frames), so an
/// unbounded list lets a pathological score (`chop(32).slow(0.001)` is
/// ~266k simultaneous voices) starve the audio callback - silence, the
/// worst live failure. An uncapped graph just dies under that; the
/// never-die contract prefers dropping the newest onset and counting it,
/// exactly like the live ring refusing when full.
pub const MAX_ACTIVE_VOICES: usize = 512;
/// One limiter for every voice that can sound at once. A voice that asks
/// for `.limit()` and does not get one plays unlimited, and the documented
/// advice, `all(x => x.limit(...))`, asks for one per voice in the set.
///
/// Each in-line limiter holds a few hundred bytes, so the whole pool is
/// small.
const LIMITER_POOL: usize = MAX_ACTIVE_VOICES;

/// Ceiling on events waiting for their onset frame. `process_block` scans
/// this list EVERY callback, so an unbounded queue turns one heavy score
/// into an audio thread that can no longer meet its deadline - frames stop
/// advancing while every other part looks healthy. Dropping the newest
/// admission keeps callback work bounded, matching the live ring's policy.
pub const MAX_PENDING_EVENTS: usize = 8192;

/// The subset of event controls still consulted after a voice is activated.
///
/// [`OscillatorControls`] is the fixed event-boundary record, so it also owns
/// one-shot setup data such as routing, sends, filter configuration and rare
/// `.FX()` stages. The callback turns those into prepared voice, orbit and
/// pooled stage state once. Retaining them afterward would inflate the stride
/// walked for every voice and every sample. Keep the exhaustive projection
/// below: a newly added event control must be consciously classified as
/// render-time or activation-only.
#[derive(Clone, Copy, Debug)]
struct VoiceRenderControls {
    noise: f32,
    waveform: Waveform,
    envelope: Envelope,
    worklet_begin_secs: f32,
    lfo_end_secs: f32,
    filter_lfo_end_secs: f32,
    modulator_release_secs: f32,
    postgain: f32,
    distort: Option<crate::distortion::DistortControls>,
    stretch: Option<f32>,
    fm: Option<FmControls>,
    lfos: [Option<LfoMod>; MAX_VOICE_MODS],
    bus_mods: [Option<BusMod>; MAX_VOICE_MODS],
    bus: Option<u8>,
    busgain: f32,
    envs: [Option<EnvMod>; MAX_VOICE_MODS],
    phaser: Option<PhaserControls>,
    tremolo: Option<TremoloControls>,
    vowel: Option<VowelControls>,
    coarse: Option<f32>,
    crush: Option<f32>,
    shape: Option<ShapeControls>,
    vibrato: Option<VibratoControls>,
    pitch_env: Option<PitchEnvControls>,
    compressor: Option<CompressorControls>,
    partials: Option<PartialsControls>,
}

impl From<OscillatorControls> for VoiceRenderControls {
    fn from(controls: OscillatorControls) -> Self {
        let OscillatorControls {
            live_controls: _,
            preview_epoch: _,
            choke_only: _,
            piano: _,
            limit: _,
            noise,
            waveform,
            envelope,
            worklet_begin_secs,
            lfo_end_secs,
            filter_lfo_end_secs,
            modulator_release_secs,
            velocity: _,
            postgain,
            pan: _,
            filters: _,
            distort,
            delay: _,
            duck: _,
            reverb: _,
            dry: _,
            stretch,
            fm,
            orbit: _,
            insert_orbit: _,
            effects: _,
            instrument: _,
            lfos,
            bus_mods,
            bus,
            busgain,
            channels: _,
            envs,
            phaser,
            tremolo,
            vowel,
            coarse,
            crush,
            shape,
            vibrato,
            pitch_env,
            djf: _,
            compressor,
            transient: _,
            fx_stages: _,
            partials,
        } = controls;
        Self {
            noise,
            waveform,
            envelope,
            worklet_begin_secs,
            lfo_end_secs,
            filter_lfo_end_secs,
            modulator_release_secs,
            postgain,
            distort,
            stretch,
            fm,
            lfos,
            bus_mods,
            bus,
            busgain,
            envs,
            phaser,
            tremolo,
            vowel,
            coarse,
            crush,
            shape,
            vibrato,
            pitch_env,
            compressor,
            partials,
        }
    }
}

/// Not `Clone`: a voice leases pooled state (a compressor, a delay line, a
/// reverb with its impulse response) that exists once and is returned when the
/// voice is dropped. Copying one would duplicate the lease.
#[derive(Debug)]
struct Voice {
    /// Only these directly bound controls can move on a sustained voice.
    live_gain: crate::live_control::Ramp,
    live_cutoff: crate::live_control::Ramp,
    /// Frame at which the connected graph begins producing output. This is
    /// normally the source onset. SBD is the exception: its graph begins one
    /// scheduler lead earlier because its asymmetric WaveShaper curve produces
    /// a tiny non-zero value before the oscillator starts.
    graph_start_frame: u64,
    start_frame: u64,
    generation: u64,
    /// The generation whose takeover kept this voice and plays its onset
    /// again. `None` once the voice stands in for that copy. See
    /// [`ScalarBackend::keep_started_onsets`].
    replayed_by: Option<u64>,
    /// The onset is one frame before the takeover frame, so the copy is on
    /// the next frame and nowhere else. Read only with `replayed_by` set.
    replayed_on_next_frame: bool,
    ui_visuals: u64,
    preview_epoch: u64,
    piano: bool,
    /// Sub-sample onset placement: how far the source has already advanced at
    /// `start_frame` (see OnsetEvent::onset_lead). Rounding this away costs up
    /// to half a sample per onset, which `crush` then magnifies into whole
    /// quantisation steps.
    onset_lead: f32,
    /// Precomputed `expm1(distort)` for the per-sample transfer.
    distort_shape: f32,
    /// Wet send into the orbit delay (0 = no send).
    delay_wet: f32,
    /// Wet send into the orbit reverb (0 = no send).
    reverb_wet: f32,
    /// The dry path feeds the orbit insert, not the orbit.
    through_insert: bool,
    insert_orbit: usize,
    dry_gain: f32,
    stretch: Option<Box<[crate::stretch::Stretch; 2]>>,
    orbit: usize,
    freq_hz: f32,
    duration_secs: f32,
    /// The hap's own duration, which for a sample is NOT `duration_secs`: a
    /// sample held for its slice sounds on past the note. Modulators end on
    /// the note, so this is the length they live for.
    hap_duration_secs: f32,
    controls: VoiceRenderControls,
    /// Whether any modulator contributes through [`ModAdds`]. Most voices do
    /// not, so they share one immutable zero frame instead of clearing the
    /// full accumulator for every rendered sample.
    has_param_modulators: bool,
    source: VoiceSource,
    amplitude_scale: f32,
    gate_value: f32,
    left_gain: f32,
    right_gain: f32,
    /// StereoPanner x in [-1, 1] for STEREO sources (2·pan − 1; 0 when the
    /// pan control is absent).
    pan_x: f32,
    filters: FilterChain,
    has_filters: bool,
    /// Whether vowel, coarse, crush, or shape has real work to perform.
    /// Ordinary voices skip that whole routine instead of checking four empty
    /// controls for every rendered channel and sample.
    has_pre_distort_fx: bool,
    /// Second-channel filter state, present only for stereo sources - the
    /// biquad chain processes each channel independently.
    filters_right: Option<FilterChain>,
    stop_secs: f32,
    /// The bank slot this voice reads was emptied under it - an uninstall
    /// landed - so it renders silence for the rest of the block and retires
    /// at the block's end. Set on the audio thread; the id may then go to
    /// another sound, and a voice still reading it would play that.
    source_gone: bool,
    /// Free-running LFO modulator phases,
    /// one per `controls.lfos` slot, advanced once per rendered frame.
    /// f64: see [`lfo_phase0`].
    mod_lfo_phases: [f64; crate::backend::MAX_VOICE_MODS],
    /// The detune ratio a sample plays this block at.
    ///
    /// A buffer source's `detune` and `playbackRate` are k-rate: the browser
    /// reads them once per 128-frame quantum, from the value the automation
    /// holds at the end of that quantum, and plays the whole block at the
    /// resulting rate. An oscillator's are a-rate and do move per sample,
    /// which is why only samples need this.
    sample_pitch_hold: Option<f32>,
    /// Phaser LFO phase. `None` until the first sample past the onset
    /// quantum: the LFO seeds on its first run past the begin gate, with
    /// the frequency it reads there. A modulated rate therefore changes the
    /// seed, not just the advance, so this cannot be computed when the
    /// voice is built.
    /// f64: see [`lfo_phase0`].
    phaser_phase: Option<f64>,
    /// Notch biquad state, [mono/left, right].
    phaser_notch: [NotchState; 2],
    /// Tremolo LFO phase, seeded on first use like [`Self::phaser_phase`].
    /// f64: see [`lfo_phase0`].
    tremolo_phase: Option<f64>,
    /// Modulation of other modulators, latched per quantum: both the LFO
    /// and envelope modulators read their params once per quantum.
    mod_param_hold: ModParamAdds,
    /// The transient shaper's followers, when the voice has one.
    transient: Option<TransientState>,
    /// The main stage's gain, held back until after the `.FX()` stages.
    fx_post_gain: f32,
    /// One cold configuration and mutable state bundle per `.FX()` stage. A
    /// stage is a complete effects pass, so it cannot share the main chain's
    /// filters - running two lowpasses through one biquad would be a different
    /// filter, not two. The box is leased from a pool prepared before
    /// playback, never allocated by the audio callback.
    fx_stage_state: Option<Box<FxStageStates>>,
    /// f64: see [`lfo_phase0`].
    vibrato_phase: f64,
    /// Mutable operator state exists only for an FM voice. The box is leased
    /// from a pool prepared before playback, so ordinary voices keep this
    /// cold state out of their per-sample stride without allocating in the
    /// audio callback.
    fm_state: Option<Box<FmState>>,
    /// The same sub-quantum offset, for the PITCH envelope's param.
    ///
    /// The pitch envelope's automation begins exactly at the note's start,
    /// and detune is read from the START of the render quantum, so a note
    /// beginning mid-quantum spends its first `128 - offset` samples
    /// undetuned rather than at the envelope's floor.
    ///
    /// Note 1 of a score lands on frame 0 and never sees this; every later
    /// note does. Proved by quantum alignment against Chromium at cps 0.5:
    /// `seg(3)`, `seg(5)` and `seg(6)` put every onset on a 128-frame
    /// boundary and score corr 1.000000, while `seg(4)` (187.5 quanta) and
    /// `seg(8)` (93.75) score 0.929 and 0.945.
    pitch_quantum_skip: u8,
    /// The pink source `noise` crossfades in.
    source_noise: NoiseGen,
    /// Vowel formant bank state: 5 bandpass biquads × [mono/left, right].
    vowel_filters: [[NotchState; 5]; 2],
    /// Static formant bandpass coefficients, precomputed at voice start.
    vowel_coefs: [NotchCoefs; 5],
    /// Coarse sample-and-hold last output, per channel.
    coarse_hold: [f32; 2],
    fx_mod_hold: FxParamHold,
    /// Gate value for the pitch envelope's linear shape.
    pitch_gate: f32,
    /// Event/sample choke group. Keeping it on the live voice means groups
    /// cannot exhaust a separate lifetime registry and strand held notes.
    cut_group: Option<u32>,
    id: u64,
    /// Absolute frame at which a later cut-group trigger started fading
    /// this voice (1 → 0 over 10 ms).
    cut_fade_frame: Option<u64>,
    /// Absolute frame at which the polyphony cap started fading this voice
    /// (1 → 0 over 250 ms). Set once and never cleared: a voice the cap has
    /// dropped cannot be chosen again.
    polyphony_fade_frame: Option<u64>,
    /// Per-voice compressor delay + detector, leased from the backend's
    /// pre-allocated pool (never allocated in the callback).
    compressor: Option<Box<CompressorState>>,
    /// `limit` - a brickwall in line on this voice, leased from its own
    /// pool for the same reason: the ring is kilobytes and the callback
    /// allocates nothing.
    limiter: Option<Box<crate::limiter::InlineLimiter>>,
}

/// State advanced only while a voice has an FM operator graph.
#[derive(Debug)]
struct FmState {
    /// Phase per operator, index 0 being the one that reaches the carrier.
    /// Double precision: an operator integrates for a whole note, and an f32
    /// phase drifts audibly over a few seconds where the carrier's own
    /// short-lived phase does not - most of all when an operator modulates
    /// another at its own frequency, where the drift beats against the pair.
    phases: [f64; crate::backend::MAX_FM_OPERATORS + 1],
    /// How far into its render quantum this note starts, until the
    /// modulators have caught that up. An oscillator's first quantum is
    /// written from its sub-quantum offset while its frequency param is read
    /// from the start of that quantum.
    quantum_skip: u8,
    /// Noise state per operator, used where `fmwave` names a noise kind.
    noise: [NoiseGen; crate::backend::MAX_FM_OPERATORS + 1],
}

impl Voice {
    /// Whether this voice TAKES from a bus, either as its source or as a
    /// modulation input. Used only to order receivers after senders.
    fn reads_a_bus(&self) -> bool {
        matches!(self.source, VoiceSource::Bus { .. })
            || self.controls.bus_mods.iter().any(Option::is_some)
    }
}

/// Move every heap-backed lease home before `Voice` is dropped; both ordinary
/// retirement and a stop/reset run inside the audio callback.
#[allow(clippy::too_many_arguments, clippy::vec_box)]
fn retire_voice_resources(
    voice: &mut Voice,
    pressure: &mut ScalarPressureState,
    fm_state_pool: &mut Vec<Box<FmState>>,
    compressor_pool: &mut Vec<Box<CompressorState>>,
    limiter_pool: &mut Vec<Box<crate::limiter::InlineLimiter>>,
    delay_pool: &mut Vec<FxDelayLine>,
    reverb_pool: &mut Vec<Box<crate::reverb::OrbitReverb>>,
    zzfx_pool: &mut Vec<Box<[f32]>>,
    stretch_pool: &mut Vec<Box<[crate::stretch::Stretch; 2]>>,
    fx_stage_state_pool: &mut Vec<Box<FxStageStates>>,
) {
    pressure.retire(voice);
    if let Some(state) = voice.fm_state.take() {
        fm_state_pool.push(state);
    }
    if let Some(state) = voice.compressor.take() {
        compressor_pool.push(state);
    }
    if let Some(limiter) = voice.limiter.take() {
        limiter_pool.push(limiter);
    }
    if let Some(mut stretch) = voice.stretch.take() {
        stretch[0].reset();
        stretch[1].reset();
        stretch_pool.push(stretch);
    }
    if let VoiceSource::ZzFx { ring, .. } = &mut voice.source
        && let Some(mut ring) = ring.take()
    {
        ring.fill(0.0);
        zzfx_pool.push(ring);
    }
    if let Some(mut stages) = voice.fx_stage_state.take() {
        for slot in stages.iter_mut() {
            if let Some(stage) = slot {
                if let Some(state) = stage.compressor.take() {
                    compressor_pool.push(state);
                }
                if let Some(line) = stage.delay.take() {
                    delay_pool.push(line);
                }
                if let Some(reverb) = stage.room.take() {
                    reverb_pool.push(reverb);
                }
                if let Some(mut stretch) = stage.stretch.take() {
                    stretch[0].reset();
                    stretch[1].reset();
                    stretch_pool.push(stretch);
                }
            }
            *slot = None;
        }
        fx_stage_state_pool.push(stages);
    }
}

/// Direct-form-I biquad state for the phaser notch (WebAudio BiquadFilter,
/// notch type: b0=1, b1=−2cosω0, b2=1, a=1±α, α=sinω0/(2Q), normalized).
#[derive(Clone, Copy, Debug, Default)]
struct NotchState {
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl NotchState {
    #[inline]
    fn process(&mut self, c: &NotchCoefs, x: f32) -> f32 {
        let y = c.b0 * x + c.b1 * self.x1 + c.b2 * self.x2 - c.a1 * self.y1 - c.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Bandpass coefficients (linear Q): alpha = sin(w0)/(2Q). Vowel formants
/// always land in the well-defined range, but keep the edges.
fn bandpass_coefs(frequency_hz: f32, q: f32, sample_rate: f32) -> NotchCoefs {
    let f = (frequency_hz / (sample_rate * 0.5)).clamp(0.0, 1.0);
    if f > 0.0 && f < 1.0 {
        if q <= 0.0 {
            return NotchCoefs {
                b0: 1.0,
                b1: 0.0,
                b2: 0.0,
                a1: 0.0,
                a2: 0.0,
            };
        }
        let w0 = std::f32::consts::PI * f;
        let alpha = w0.sin() / (2.0 * q);
        let cos_w0 = w0.cos();
        let a0 = 1.0 + alpha;
        return NotchCoefs {
            b0: alpha / a0,
            b1: 0.0,
            b2: -alpha / a0,
            a1: -2.0 * cos_w0 / a0,
            a2: (1.0 - alpha) / a0,
        };
    }
    NotchCoefs {
        b0: 0.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    }
}

/// The four stages `voice_fx_pre_distort` walks, passed on their own so a
/// `.FX()` stage - which has no `OscillatorControls` of its own - can run the
/// same code as the main chain.
#[derive(Clone, Copy)]
struct FxWorkletControls<'a> {
    vowel: Option<&'a crate::backend::VowelControls>,
    coarse: Option<f32>,
    crush: Option<f32>,
    shape: Option<crate::backend::ShapeControls>,
}

/// One `.FX()` stage: a complete effects chain of its own.
///
/// Stereo rather than per-channel, because `pan` sits in the middle of this
/// chain and needs both channels at once.
///
/// Modulators targeting this stage's `fxi` contribute through `adds`.
#[inline]
#[allow(clippy::too_many_arguments)]
fn apply_fx_stage(
    left: f32,
    right: f32,
    state: &mut FxStageState,
    t: f32,
    // The hap's end, which is where a filter's envelope is scheduled to.
    hap_duration_secs: f32,
    block_n: f32,
    sample_rate: f32,
    // The transient shaper's frame-based begin and end gate.
    transient_live: bool,
    // Tremolo and phaser share the gate with f32-rounded begin and end times.
    // See `lfo_quantum_open`.
    lfo_live: bool,
    // What modulators naming this stage's `fxi` contributed this sample.
    adds: &FxStageAdds,
) -> (f32, f32) {
    let stage = &state.controls;
    let stereo = state.filters_right.is_some();
    let mut left = left;
    // A mono source's right channel is a copy. Mirrored here as well as after
    // the filters because stretch and the transient shaper run BEFORE them and
    // are stateful per channel: feeding one of them a stale right channel
    // desynchronises its envelope from the left's.
    let mut right = if stereo { right } else { left };

    // Stretch and the transient shaper come FIRST, ahead of the gain stage
    if let (Some(vocoder), Some(factor)) = (state.stretch.as_mut(), stage.stretch) {
        left = vocoder[0].process(left, factor);
        if stereo {
            right = vocoder[1].process(right, factor);
        }
    }
    if let Some(transient) = state.transient.as_mut() {
        // Outside the gate, leave the shaper's state unchanged and feed
        // silence to the remaining stages.
        if transient_live {
            let (l, r) = transient.process(left, right);
            left = l;
            right = r;
        } else {
            left = 0.0;
            right = 0.0;
        }
    }

    let stage_gain = stage.gain * adds.gain_factor();
    left *= stage_gain;
    right *= stage_gain;

    let filter_adds = [adds.lowpass_hz, adds.highpass_hz, adds.bandpass_hz];
    let filter_q_adds = [adds.lowpass_q, adds.highpass_q, adds.band_q];
    left = state
        .filters
        .process_modulated(left, t, hap_duration_secs, filter_adds, filter_q_adds);
    if let Some(filters_right) = state.filters_right.as_mut() {
        right = filters_right.process_modulated(
            right,
            t,
            hap_duration_secs,
            filter_adds,
            filter_q_adds,
        );
    }

    let worklets = FxWorkletControls {
        vowel: stage.vowel.as_ref(),
        coarse: stage.coarse,
        crush: stage.crush,
        shape: stage.shape,
    };
    let vowel_coefs = state.vowel_coefs;
    left = voice_fx_pre_distort(
        left,
        0,
        worklets,
        &vowel_coefs,
        &mut state.vowel_filters,
        &mut state.coarse_hold,
        block_n,
        &FxParamHold::default(),
    );
    if stereo {
        right = voice_fx_pre_distort(
            right,
            1,
            worklets,
            &vowel_coefs,
            &mut state.vowel_filters,
            &mut state.coarse_hold,
            block_n,
            &FxParamHold::default(),
        );
    }

    if let Some(distort) = stage.distort {
        let shape = distort.shape();
        left = distort.apply(left, shape);
        if stereo {
            right = distort.apply(right, shape);
        }
    }
    // Everything above is per-channel and a mono source has no right chain, so
    // its right channel is a copy. Everything below can split the two: the
    // pan stage upmixes mono, and after it the channels differ. The copy must
    // happen here. A `(left, left)` copy at the end would discard the pan.
    if !stereo {
        right = left;
    }

    // Tremolo is an amplitude gain shared by both channels: one LFO into one
    // GainNode, not one per channel.
    if let Some(tr) = stage.tremolo {
        let base = tr.gain_floor();
        let gain = if lfo_live {
            let phase = *state
                .tremolo_phase
                .get_or_insert_with(|| lfo_phase0(tr.time_secs, tr.frequency_hz, tr.phase_offset));
            let raw = lfo_waveshape(tr.shape, phase, tr.skew) * tr.depth;
            let next = phase + f64::from(tr.frequency_hz) / f64::from(sample_rate);
            state.tremolo_phase = Some(if next > 1.0 { next - 1.0 } else { next });
            let gain = base + raw.powf(1.5).clamp(0.0, 1.0);
            // A NaN sum gives the gain its default.
            if gain.is_nan() { 1.0 } else { gain }
        } else {
            base
        };
        left *= gain;
        right *= gain;
    }

    if let (Some(compressor), Some(controls)) = (state.compressor.as_mut(), stage.compressor) {
        let (l, r) = compressor.process(left, right, &controls, sample_rate);
        left = l;
        right = r;
    }

    // StereoPanner has two laws, selected by its input's channel count. For
    // a mono input it is `L = in*cos(x), R = in*sin(x)` with
    // `x = (pan+1)*pi/4`, so a centred pan attenuates by 0.707. For a stereo
    // input it is the pass-through law that `stereo_pan` implements, which
    // is the identity at centre. The stereo law on a mono voice is too
    // loud, by sqrt(2) at centre.
    if let Some(pan_x) = stage.pan_x {
        let x = pan_x.clamp(-1.0, 1.0);
        if state.source_is_mono {
            let (lg, rg) = ScalarBackend::channel_gains(Some((x + 1.0) * 0.5));
            right = left * rg;
            left *= lg;
        } else {
            let (l, r) = stereo_pan(left, right, x);
            left = l;
            right = r;
        }
    }

    // Phaser: one notch whose centre is swept by a triangle LFO, the detune
    // clamped to +/- sweep CENTS. `getPhaser` builds its LFO with depth =
    // sweep*2 and passes no phase offset, anchored at the note's begin.
    if let Some(ph) = stage.phaser {
        let lfo_depth = (2.0 * ph.sweep_cents).max(0.0);
        let sweep = lfo_depth * 0.5;
        // Before the gate opens and after it closes the LFO is silent, so
        // the notch sits at its static centre.
        let detune = if lfo_live {
            let phase = *state
                .phaser_phase
                .get_or_insert_with(|| lfo_phase0(ph.time_secs, ph.rate_hz, 0.0));
            let raw = (lfo_waveshape(0, phase, 0.5) - 0.5) * lfo_depth;
            let next = phase + f64::from(ph.rate_hz) / f64::from(sample_rate);
            state.phaser_phase = Some(if next > 1.0 { next - 1.0 } else { next });
            js_clamp(raw, -sweep, sweep)
        } else {
            0.0
        };
        let frequency = (ph.center_hz + 282.0) * (detune / 1200.0).exp2();
        // phaserdepth names the notch's Q, not a wet/dry depth.
        let q = 2.0 - (ph.depth * 2.0).clamp(0.0, 1.9);
        let coefs = notch_coefs(frequency, q, sample_rate);
        left = state.phaser_filters[0].process(&coefs, left);
        right = state.phaser_filters[1].process(&coefs, right);
    }

    // Inline feedback delay, last in the stage. The dry
    // leg is summed with the wet one rather than replaced:
    // `out = signal·dry + delay(signal)·wet`.
    if let (Some(line), Some(delay)) = (state.delay.as_mut(), stage.delay) {
        let frames = delay.time_secs * sample_rate;
        let wet_l = OrbitDelay::read(&line.left, line.write, frames);
        let wet_r = OrbitDelay::read(&line.right, line.write, frames);
        // What goes INTO the line is the signal plus the delayed output fed
        // back, which is the DelayNode -> feedbackGain -> DelayNode cycle.
        line.left[line.write] = storable(left + wet_l * delay.feedback);
        line.right[line.write] = storable(right + wet_r * delay.feedback);
        line.write = (line.write + 1) % line.left.len();
        left = left * stage.dry + wet_l * delay.wet;
        right = right * stage.dry + wet_r * delay.wet;
    }

    // Inline convolution reverb, last in the stage.
    // Fed one frame at a time: the convolver already stages input to its own
    // 128-frame hop and drains a wet FIFO, which is what lets a live host hand
    // it buffers that are not multiples of 128. The cost is that the wet
    // stream runs one hop (<=2.9 ms) late, which for a reverb tail is not
    // audible.
    if let (Some(reverb), Some(room)) = (state.room.as_mut(), stage.room) {
        let mut wet = [0.0f32; 1];
        let mut wet_r = [0.0f32; 1];
        reverb.process_block(&[left], &[right], &mut wet, &mut wet_r);
        left = left * stage.dry + wet[0] * room.wet;
        right = right * stage.dry + wet_r[0] * room.wet;
    }

    (left, right)
}

/// Post-filter, pre-distort stages in the FX chain order:
/// vowel -> coarse -> crush -> shape. A modulator add only lands when the
/// matching control is set: an LFO on `crush` with no `crush` in the pattern
/// has no stage to reach and does nothing. `block_n` is the CONTEXT
/// block-local index (abs % 128) - the coarse hold pattern is block-local,
/// so it re-anchors every quantum.
#[allow(clippy::too_many_arguments)]
#[inline]
fn voice_fx_pre_distort(
    mut sample: f32,
    ch: usize,
    controls: FxWorkletControls,
    vowel_coefs: &[NotchCoefs; 5],
    vowel_filters: &mut [[NotchState; 5]; 2],
    coarse_hold: &mut [f32; 2],
    block_n: f32,
    fx_mod: &FxParamHold,
) -> f32 {
    if let Some(vw) = controls.vowel {
        let mut sum = 0.0;
        for k in 0..5 {
            sum += vowel_filters[ch][k].process(&vowel_coefs[k], sample) * vw.gains[k];
        }
        sample = sum * 8.0;
    }
    if let Some(coarse) = controls.coarse {
        let coarse = (coarse + fx_mod.coarse).max(1.0);
        if block_n % coarse < 1.0 {
            coarse_hold[ch] = sample;
        }
        sample = coarse_hold[ch];
    }
    if let Some(crush) = controls.crush {
        let crush = f64::from(crush + fx_mod.crush).max(1.0);
        let x = (crush - 1.0).exp2();
        // Round half-up (floor(x + 0.5)), which differs from f64::round on
        // negative halves.
        sample = ((f64::from(sample) * x + 0.5).floor() / x) as f32;
    }
    if let Some(sh) = controls.shape {
        let s0 = f64::from(sh.shape + fx_mod.shape);
        let s0 = if s0 < 1.0 { s0 } else { 1.0 - 4e-10 };
        let k = (2.0 * s0) / (1.0 - s0);
        let post = f64::from(sh.postgain + fx_mod.shape_vol).clamp(0.001, 1.0);
        let x = f64::from(sample);
        sample = ((1.0 + k) * x / (1.0 + k * x.abs()) * post) as f32;
    }
    sample
}

/// Per-`.FX()` stage's cold configuration and mutable render state.
#[derive(Debug)]
struct FxStageState {
    /// Immutable stage configuration is cold unless `.FX()` is active, so it
    /// lives with this already-pooled state instead of inflating every voice.
    controls: FxStage,
    filters: FilterChain,
    /// Present only for a stereo path, matching the main chain's split.
    filters_right: Option<FilterChain>,
    coarse_hold: [f32; 2],
    /// Each stage owns its state outright - none of it may be shared with
    /// the main chain or with another stage. Sharing it would make two
    /// stages that name the same effect behave as one.
    vowel_coefs: [NotchCoefs; 5],
    vowel_filters: [[NotchState; 5]; 2],
    /// Seeded on first use, exactly like the main chain's: the LFO reads its
    /// frequency at the first quantum past the begin gate.
    tremolo_phase: Option<f64>,
    phaser_phase: Option<f64>,
    phaser_filters: [NotchState; 2],
    compressor: Option<Box<CompressorState>>,
    /// Whether the SOURCE is mono, which is not the same as having no right
    /// filter chain: naming pan in a stage forces the stereo path, because a
    /// StereoPanner upmixes. StereoPanner has two laws and this picks between
    /// them - see the pan stage.
    source_is_mono: bool,
    /// One phase vocoder per channel, exactly like the main chain's.
    stretch: Option<Box<[crate::stretch::Stretch; 2]>>,
    transient: Option<TransientState>,
    delay: Option<FxDelayLine>,
    room: Option<Box<crate::reverb::OrbitReverb>>,
}

type FxStageStates = [Option<FxStageState>; crate::backend::MAX_FX_STAGES];

/// Transient shaper state, per voice.
///
/// Two one-pole followers per channel track the signal at different speeds;
/// their difference is how peaky it is right now. The makeup gain is averaged
/// across BOTH channels - `avgGain` is processor state, not per-channel - so
/// a transient in one channel pulls the other's level with it.
#[derive(Clone, Copy, Debug)]
struct TransientState {
    attack_env: [f32; 2],
    sustain_env: [f32; 2],
    avg_gain: f32,
    attack_coeff: f32,
    sustain_coeff: f32,
    gain_coeff: f32,
    attack_amt: f32,
    sustain_amt: f32,
    scaling: f32,
    mix: f32,
}

impl TransientState {
    /// `timeToCoeff(t) = 1 - exp(-1/(sr·t))`.
    fn new(controls: crate::backend::TransientControls, sample_rate: f32) -> Self {
        let coeff = |t: f32| 1.0 - (-1.0 / (sample_rate * t)).exp();
        // Only attack and sustain are passed; the rest are the
        // processor's own defaults, already clamped there.
        Self {
            attack_env: [0.0; 2],
            sustain_env: [0.0; 2],
            avg_gain: 1.0,
            attack_coeff: coeff(0.003),
            sustain_coeff: coeff(0.08),
            gain_coeff: coeff(0.2),
            attack_amt: controls.attack.clamp(-1.0, 1.0),
            sustain_amt: controls.sustain.clamp(-1.0, 1.0),
            scaling: 0.5 + 5.0 * 0.1_f32.clamp(0.0, 1.0),
            mix: 1.0,
        }
    }

    /// One channel. `avg_gain` is shaper state rather than per-channel, so
    /// it is threaded through instead of read from `self`: channel 1
    /// continues from where channel 0 left it.
    fn process_channel(&mut self, ch: usize, sample: f32, avg_gain: &mut f32) -> f32 {
        let x = sample.abs();
        let att = lerp(self.attack_env[ch], x, self.attack_coeff);
        let sus = lerp(self.sustain_env[ch], x, self.sustain_coeff);
        self.attack_env[ch] = att;
        self.sustain_env[ch] = sus;
        let peakiness = js_clamp(self.scaling * (att - sus) / (sus + 1e-6), -1.5, 1.5);
        let att_scale = peakiness.max(0.0);
        let sus_scale = (-peakiness).max(0.0);
        let attack_gain = db_to_lin(self.attack_amt * att_scale * 18.0);
        let sustain_gain = db_to_lin(self.sustain_amt * sus_scale * 36.0);
        let gain = js_clamp(attack_gain * sustain_gain, 0.0, 8.0);
        *avg_gain = lerp(*avg_gain, gain, self.gain_coeff);
        let makeup = if *avg_gain > 1e-3 {
            1.0 / *avg_gain
        } else {
            1.0
        };
        let wet = sample * gain * makeup;
        let y = lerp(sample, wet, self.mix);
        y / (1.0 + y.abs())
    }

    fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let mut avg_gain = self.avg_gain;
        let l = self.process_channel(0, left, &mut avg_gain);
        let r = self.process_channel(1, right, &mut avg_gain);
        self.avg_gain = avg_gain;
        (l, r)
    }

    /// A source that has not been upmixed yet reaches the shaper as ONE
    /// channel and runs exactly one follower - not the stereo pair with a
    /// duplicated signal.
    fn process_mono(&mut self, sample: f32) -> f32 {
        let mut avg_gain = self.avg_gain;
        let out = self.process_channel(0, sample, &mut avg_gain);
        self.avg_gain = avg_gain;
        out
    }
}

#[inline]
fn lerp(a: f32, b: f32, n: f32) -> f32 {
    n * (b - a) + a
}

#[inline]
fn db_to_lin(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

/// Per-orbit DJ filter: sticky once any
/// trigger on the orbit sets `djf`; value < 0.49 sweeps a 2-pole lowpass,
/// > 0.51 a highpass (input − lowpass), cutoff (v·11)^4 Hz, resonance 0.1.
#[derive(Clone, Copy, Debug, Default)]
struct DjfState {
    active: bool,
    value: f32,
    /// Summed `djf` modulation from the voices on this orbit, latched once
    /// per 128-frame quantum because DJFProcessor reads `parameters.value[0]`.
    modulation: f32,
    /// TwoPoleFilter state per channel: [s0, s1].
    s: [[f64; 2]; 2],
}

impl DjfState {
    #[inline]
    fn process(&mut self, ch: usize, input: f32, sample_rate: f64) -> f32 {
        let value = (self.value + self.modulation).clamp(0.0, 1.0);
        let (hipass, v) = if value > 0.51 {
            (true, (value - 0.5) * 2.0)
        } else if value < 0.49 {
            (false, value * 2.0)
        } else {
            return input;
        };
        let cutoff = f64::from(v * 11.0)
            .powi(4)
            .clamp(0.0, sample_rate * 0.5 - 1.0);
        let c = (2.0 * (cutoff * std::f64::consts::PI / sample_rate).sin()).clamp(0.0, 1.14);
        // resonance fixed at 0.1: r = 0.5^(8·0.1 + 1)
        let r = 0.5f64.powf(1.8);
        let mrc = 1.0 - r * c;
        let state = &mut self.s[ch];
        let x = f64::from(input);
        state[0] = mrc * state[0] - c * state[1] + c * x;
        state[1] = mrc * state[1] + c * state[0];
        if hipass {
            (x - state[1]) as f32
        } else {
            state[1] as f32
        }
    }
}

/// Per-voice dynamics compressor, matched sample-for-sample against
/// Chromium's DynamicsCompressorNode.
///
/// NOT the Giannoulis dB-domain design most readings of the spec arrive at
/// (a dB gain computer with branching attack/release smoothing). This one
/// runs an exponential knee (`k` solved by bisection), a per-sample
/// linear-domain detector, and a compressor gain that evolves per 32-frame
/// DIVISION - multiplicatively on release with a 4th-order adaptive-release
/// polynomial, exponentially toward an `asin`-pre-warped target on attack -
/// then post-warps the gain through `sin` and applies
/// `(1 / saturate(1, k))^0.6` makeup. The dB-domain design measured 2.88
/// over the first 480 frames (settling to ~0.95) on
/// `compressor("-20:20:10:.002:.02")`, and -2.78 dB with a threshold at 0.
///
/// The 6 ms pre-delay and its 1023-frame ceiling are Chromium-verified:
/// impulse onsets at 264/288/576/1023/1023 for 44.1/48/96/176.4/192 kHz -
/// the pre-delay is `preDelayTime · sampleRate` truncated, never rounded to
/// a quantum.
///
/// The compressor exists from GRAPH BUILD, not from the note's onset: it
/// processes silence from context time zero until the source starts, and
/// its detector COLD-STARTS at zero, so the early gain dips and recovers
/// along the adaptive release curve. `new` replays that silence
/// (division-stepped, with an early exit at the silence fixed point of
/// detector 1, gain 1), so a voice triggered mid-render starts from the
/// state the graph would have reached.
const COMPRESSOR_MAX_PREDELAY_FRAMES: usize = 1024;
/// The ring is exactly the max pre-delay; indices are masked by 1023.
const COMPRESSOR_RING_FRAMES: usize = COMPRESSOR_MAX_PREDELAY_FRAMES;

/// Release zone values 0 -> 1, and the 4th-order polynomial fitted through
/// them (y1: x == 0 … y4: x == 3). The constants are load-bearing; do not
/// refit.
const COMPRESSOR_RELEASE_ZONES: [f32; 4] = [0.09, 0.16, 0.42, 0.98];
const COMPRESSOR_POLY: [[f64; 4]; 5] = [
    [
        0.999_999_999_999_999_8,
        1.843_221_968_432_392_3e-16,
        -1.937_339_435_167_642_3e-16,
        8.824_516_011_816_245e-18,
    ],
    [
        -1.578_832_035_284_588_8,
        2.330_583_703_207_428_6,
        -0.914_119_420_484_042_9,
        0.162_367_752_561_203_2,
    ],
    [
        0.533_414_286_910_642_4,
        -1.272_736_789_213_631,
        0.925_885_604_220_751_2,
        -0.186_563_101_917_762_26,
    ],
    [
        0.087_834_631_382_072_34,
        -0.169_416_296_792_562_2,
        0.085_880_579_515_952_72,
        -0.004_298_914_105_462_83,
    ],
    [
        -0.042_416_883_008_123_074,
        0.111_569_382_798_760_2,
        -0.097_646_763_252_658_72,
        0.028_494_263_462_021_576,
    ],
];
/// Detector release time.
const COMPRESSOR_SAT_RELEASE_SECS: f32 = 0.0025;

#[derive(Clone, Copy, Debug)]
struct CompressorState {
    /// Interleaved stereo pre-delay ring, index-masked.
    ring: [[f32; 2]; COMPRESSOR_RING_FRAMES],
    read: usize,
    write: usize,
    delay_frames: usize,
    /// Absolute frame counter, in lock-step with the render's, so the
    /// 32-frame division boundaries stay aligned with the context's.
    frame: u64,
    detector_average: f32,
    compressor_gain: f32,
    db_max_attack_diff: f32,
    /// Latched at each division boundary.
    envelope_rate: f32,
    scaled_desired_gain: f32,
    /// (threshold, knee, ratio) the static curve below was built from;
    /// `UpdateStaticCurveParameters` recomputes only on change.
    cached_params: (f32, f32, f32),
    k: f32,
    linear_threshold: f32,
    knee_threshold: f32,
    db_knee_threshold: f32,
    db_yknee_threshold: f32,
    slope: f32,
    linear_post_gain: f32,
}

fn compressor_db_to_lin(db: f32) -> f32 {
    10.0f32.powf(0.05 * db)
}

fn compressor_lin_to_db(x: f32) -> f32 {
    // `log10f(0)` is -inf - deliberately let through and caught with
    // `finite_or` at each use site.
    20.0 * x.log10()
}

fn finite_or(x: f32, default: f32) -> f32 {
    if x.is_finite() { x } else { default }
}

impl CompressorState {
    /// A pool entry before any voice has leased it. The contents are
    /// irrelevant: every lease overwrites with [`Self::new`].
    fn cold() -> Self {
        Self {
            ring: [[0.0; 2]; COMPRESSOR_RING_FRAMES],
            read: 0,
            write: 0,
            delay_frames: 0,
            frame: 0,
            detector_average: 0.0,
            compressor_gain: 1.0,
            db_max_attack_diff: -1.0,
            envelope_rate: 1.0,
            scaled_desired_gain: 1.0,
            cached_params: (f32::NAN, f32::NAN, f32::NAN),
            k: 5.0,
            linear_threshold: 0.0,
            knee_threshold: 0.0,
            db_knee_threshold: 0.0,
            db_yknee_threshold: 0.0,
            slope: 1.0,
            linear_post_gain: 1.0,
        }
    }

    fn new(sample_rate: f32, c: &crate::backend::CompressorControls, onset_frame: u64) -> Self {
        let mut state = Self::cold();
        state.delay_frames = ((sample_rate * 0.006) as usize).min(COMPRESSOR_RING_FRAMES - 1);
        state.write = state.delay_frames;
        state.update_static_curve(c);
        state.preroll(onset_frame, c, sample_rate);
        state
    }

    /// `KneeCurve`: linear up to the threshold, then 1st-derivative matched
    /// and asymptotically approaching `linear_threshold + 1/k`.
    fn knee_curve(&self, x: f32, k: f32) -> f32 {
        if x < self.linear_threshold {
            return x;
        }
        self.linear_threshold + (1.0 - f64::from(-k * (x - self.linear_threshold)).exp() as f32) / k
    }

    /// `Saturate`: the knee curve below the knee threshold, constant ratio
    /// above it.
    fn saturate(&self, x: f32) -> f32 {
        if x < self.knee_threshold {
            return self.knee_curve(x, self.k);
        }
        let db_x = compressor_lin_to_db(x);
        let db_y = self.db_yknee_threshold + self.slope * (db_x - self.db_knee_threshold);
        compressor_db_to_lin(db_y)
    }

    /// `KAtSlope`: bisect for the knee sharpness whose slope in dB/dB at the
    /// knee's end matches `1/ratio`. Fifteen geometric-mean steps, verbatim.
    fn k_at_slope(&self, desired_slope: f32) -> f32 {
        let db_x = self.cached_params.0 + self.cached_params.1;
        let x = compressor_db_to_lin(db_x);
        let (mut x2, mut db_x2) = (1.0f32, 0.0f32);
        if x >= self.linear_threshold {
            x2 = x * 1.001;
            db_x2 = compressor_lin_to_db(x2);
        }
        let mut min_k = 0.1f32;
        let mut max_k = 10_000.0f32;
        let mut k = 5.0f32;
        let mut slope = 1.0f32;
        for _ in 0..15 {
            if x >= self.linear_threshold {
                let db_y = compressor_lin_to_db(self.knee_curve(x, k));
                let db_y2 = compressor_lin_to_db(self.knee_curve(x2, k));
                slope = (db_y2 - db_y) / (db_x2 - db_x);
            }
            if slope < desired_slope {
                max_k = k;
            } else {
                min_k = k;
            }
            k = (min_k * max_k).sqrt();
        }
        k
    }

    /// `UpdateStaticCurveParameters` + the makeup gain derived from it.
    fn update_static_curve(&mut self, c: &crate::backend::CompressorControls) {
        let params = (c.threshold_db, c.knee_db, c.ratio);
        if params == self.cached_params {
            return;
        }
        self.cached_params = params;
        self.linear_threshold = compressor_db_to_lin(c.threshold_db);
        self.slope = 1.0 / c.ratio;
        self.k = self.k_at_slope(self.slope);
        self.db_knee_threshold = c.threshold_db + c.knee_db;
        self.knee_threshold = compressor_db_to_lin(self.db_knee_threshold);
        self.db_yknee_threshold =
            compressor_lin_to_db(self.knee_curve(self.knee_threshold, self.k));
        // Makeup gain with the perceptual 0.6 power - empirical, keep it.
        self.linear_post_gain = (1.0 / self.saturate(1.0)).powf(0.6);
    }

    /// The per-division envelope update, run at every 32nd frame.
    fn division(&mut self, c: &crate::backend::CompressorControls, sample_rate: f32) {
        self.detector_average = finite_or(self.detector_average, 1.0);
        let desired_gain = self.detector_average;
        // Pre-warp so the `sin` warp in `process` lands on the desired gain.
        let scaled = desired_gain.asin() / std::f32::consts::FRAC_PI_2;
        self.scaled_desired_gain = scaled;
        let is_releasing = scaled > self.compressor_gain;
        let db_compression_diff = if scaled == 0.0 {
            if is_releasing { -1.0 } else { 1.0 }
        } else {
            compressor_lin_to_db(self.compressor_gain / scaled)
        };
        if is_releasing {
            self.db_max_attack_diff = -1.0;
            let diff = finite_or(db_compression_diff, -1.0);
            // Adaptive release - deeper compression releases faster. The
            // polynomial passes through the four release-zone fractions of
            // `release_frames` at x = 0..3, x spanning -12 -> 0 dB.
            let x = (diff.clamp(-12.0, 0.0) + 12.0) * 0.25;
            let release_frames = sample_rate * c.release_secs;
            let mut coeffs = [0.0f32; 5];
            for (slot, row) in coeffs.iter_mut().zip(COMPRESSOR_POLY) {
                let mut sum = 0.0f64;
                for (weight, zone) in row.iter().zip(COMPRESSOR_RELEASE_ZONES) {
                    sum += weight * f64::from(zone);
                }
                *slot = release_frames * sum as f32;
            }
            let x2 = x * x;
            let calc_release_frames = coeffs[0]
                + coeffs[1] * x
                + coeffs[2] * x2
                + coeffs[3] * x2 * x
                + coeffs[4] * x2 * x2;
            const DB_SPACING: f32 = 5.0;
            self.envelope_rate = compressor_db_to_lin(DB_SPACING / calc_release_frames);
        } else {
            let diff = finite_or(db_compression_diff, 1.0);
            // Attack keeps using the largest difference seen this attack.
            if self.db_max_attack_diff == -1.0 || self.db_max_attack_diff < diff {
                self.db_max_attack_diff = diff;
            }
            let eff_atten_diff = self.db_max_attack_diff.max(0.5);
            let x = 0.25 / eff_atten_diff;
            let attack_frames = c.attack_secs.max(0.001) * sample_rate;
            self.envelope_rate = 1.0 - x.powf(1.0 / attack_frames);
        }
    }

    /// Replay `frames` of the silence the compressor processed between graph
    /// build and the note's onset.
    ///
    /// Frame-by-frame in f32, not closed forms. The iteration stalls an ulp
    /// or two short of its limits once the per-frame step drops below f32
    /// resolution. `asin` is steep near 1, so the difference between the
    /// stall value and an exact 1.0 is audible through the makeup gain as a
    /// small transient on every prerolled onset. The stall also ends the
    /// loop: a division that changes nothing is a true fixed point of the
    /// iteration, and the rest of the wait is a counter update.
    fn preroll(&mut self, frames: u64, c: &crate::backend::CompressorControls, sample_rate: f32) {
        if frames == 0 {
            return;
        }
        // Silence keeps the detector's inputs constant: attenuation is 1, its
        // dB floor is the 2 dB minimum, so the release rate is fixed.
        let sat_release_frames = COMPRESSOR_SAT_RELEASE_SECS * sample_rate;
        let detector_rate = compressor_db_to_lin(2.0 / sat_release_frames) - 1.0;
        let mut done = 0u64;
        while done < frames {
            let before = (
                self.detector_average.to_bits(),
                self.compressor_gain.to_bits(),
                self.db_max_attack_diff.to_bits(),
            );
            self.division(c, sample_rate);
            let span = (frames - done).min(32);
            for _ in 0..span {
                // attenuation 1 > detector while the detector is below 1.
                if self.detector_average < 1.0 {
                    self.detector_average += (1.0 - self.detector_average) * detector_rate;
                }
                self.detector_average = finite_or(self.detector_average.min(1.0), 1.0);
                if self.envelope_rate < 1.0 {
                    self.compressor_gain +=
                        (self.scaled_desired_gain - self.compressor_gain) * self.envelope_rate;
                } else {
                    self.compressor_gain = (self.compressor_gain * self.envelope_rate).min(1.0);
                }
            }
            done += span;
            self.frame += span;
            let after = (
                self.detector_average.to_bits(),
                self.compressor_gain.to_bits(),
                self.db_max_attack_diff.to_bits(),
            );
            if span == 32 && after == before {
                self.frame += frames - done;
                break;
            }
        }
    }

    #[inline]
    fn process(
        &mut self,
        l: f32,
        r: f32,
        c: &crate::backend::CompressorControls,
        sample_rate: f32,
    ) -> (f32, f32) {
        // Params are read once per render quantum; the static curve and
        // envelope refresh at the division boundary.
        if self.frame & 31 == 0 {
            // Division locals are flushed to zero when subnormal; at
            // extreme thresholds the gain genuinely reaches that range.
            if self.detector_average.abs() < f32::MIN_POSITIVE {
                self.detector_average = 0.0;
            }
            if self.compressor_gain.abs() < f32::MIN_POSITIVE {
                self.compressor_gain = 0.0;
            }
            self.update_static_curve(c);
            self.division(c, sample_rate);
        }
        self.frame += 1;

        // The detector runs on the UNDELAYED input while the gain applies to
        // the delayed signal - the pre-delay is lookahead.
        let compressor_input = l.abs().max(r.abs());
        self.ring[self.write] = [l, r];

        let shaped_input = self.saturate(compressor_input);
        let attenuation = if compressor_input <= 1e-4 {
            1.0
        } else {
            shaped_input / compressor_input
        };
        let db_attenuation = (-compressor_lin_to_db(attenuation)).max(2.0);
        let sat_release_frames = COMPRESSOR_SAT_RELEASE_SECS * sample_rate;
        let sat_release_rate = compressor_db_to_lin(db_attenuation / sat_release_frames) - 1.0;
        let rate = if attenuation > self.detector_average {
            sat_release_rate
        } else {
            1.0
        };
        self.detector_average += (attenuation - self.detector_average) * rate;
        self.detector_average = finite_or(self.detector_average.min(1.0), 1.0);

        if self.envelope_rate < 1.0 {
            // Attack - pull the gain down toward the pre-warped target.
            self.compressor_gain +=
                (self.scaled_desired_gain - self.compressor_gain) * self.envelope_rate;
        } else {
            // Release - multiplicative climb back toward 1.
            self.compressor_gain = (self.compressor_gain * self.envelope_rate).min(1.0);
        }

        // Warp the gain to smooth the exponential transition points. In f64
        // on purpose: the reference computes exactly this one in double.
        let post_warp =
            (std::f64::consts::FRAC_PI_2 * f64::from(self.compressor_gain)).sin() as f32;
        let total_gain = self.linear_post_gain * post_warp;

        let delayed = self.ring[self.read];
        self.read = (self.read + 1) & (COMPRESSOR_RING_FRAMES - 1);
        self.write = (self.write + 1) & (COMPRESSOR_RING_FRAMES - 1);
        (delayed[0] * total_gain, delayed[1] * total_gain)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct NotchCoefs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

/// Notch coefficients: normalized frequency clamped to [0, 1]; at the
/// edges nothing gets notched (pass-through). Q stays > 0 here (the phaser
/// clamps depth so Q ∈ [0.1, 2]).
fn notch_coefs(frequency_hz: f32, q: f32, sample_rate: f32) -> NotchCoefs {
    let f = (frequency_hz / (sample_rate * 0.5)).clamp(0.0, 1.0);
    if f <= 0.0 || f >= 1.0 {
        // Frequency pinned to an edge: the z-transform is 1 (pass-through).
        return NotchCoefs {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        };
    }
    if q <= 0.0 {
        // The response is defined as zero when Q ≤ 0.
        return NotchCoefs {
            b0: 0.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        };
    }
    let w0 = std::f32::consts::PI * f;
    let alpha = w0.sin() / (2.0 * q);
    let cos_w0 = w0.cos();
    let a0 = 1.0 + alpha;
    NotchCoefs {
        b0: 1.0 / a0,
        b1: -2.0 * cos_w0 / a0,
        b2: 1.0 / a0,
        a1: -2.0 * cos_w0 / a0,
        a2: (1.0 - alpha) / a0,
    }
}

/// What a modulator aimed at one `.FX()` stage contributes.
///
/// Deliberately narrower than [`ModAdds`]: a stage is a fixed chain, so only
/// the parameters it actually builds can be a target, and carrying the whole
/// main-chain set per stage would multiply the per-voice state for buckets
/// nothing can reach.
#[derive(Clone, Copy, Default)]
struct FxStageAdds {
    lowpass_hz: f32,
    highpass_hz: f32,
    bandpass_hz: f32,
    lowpass_q: f32,
    highpass_q: f32,
    band_q: f32,
    gain: f32,
    gain_base: f32,
}

impl FxStageAdds {
    /// `gain` is relative to the value the modulator was built against, the
    /// same ratio the main chain uses.
    fn gain_factor(&self) -> f32 {
        if self.gain_base == 0.0 {
            1.0
        } else {
            (self.gain_base + self.gain) / self.gain_base
        }
    }
}

/// Per-frame modulator sums, one bucket per supported AudioParam target.
#[derive(Clone, Copy, Default)]
struct ModAdds {
    /// Modulators naming an `fxi` land here instead of the main buckets.
    fx_stage: [FxStageAdds; crate::backend::MAX_FX_STAGES],
    lowpass_hz: f32,
    highpass_hz: f32,
    bandpass_hz: f32,
    vowel_hz: f32,
    gain: f32,
    gain_base: f32,
    frequency_hz: f32,
    pan: f32,
    postgain: f32,
    postgain_base: f32,
    lowpass_q: f32,
    highpass_q: f32,
    band_q: f32,
    coarse: f32,
    crush: f32,
    shape: f32,
    shape_vol: f32,
    distort: f32,
    distort_vol: f32,
    delay_send: f32,
    room_send: f32,
    phaser_rate: f32,
    phaser_sweep: f32,
    phaser_center: f32,
    phaser_depth: f32,
    comp_threshold: f32,
    comp_ratio: f32,
    comp_knee: f32,
    comp_attack: f32,
    comp_release: f32,
    trem_rate: f32,
    trem_depth: f32,
    trem_skew: f32,
    trem_shape: f32,
    vib_rate: f32,
    vib_depth: f32,
    pulse_width: f32,
    dry: f32,
    /// A-rate modulation of the shared orbit DelayNode's `delayTime`.
    delay_time: f32,
    /// A-rate modulation of the GainNode on the shared delay's feedback edge.
    delay_feedback: f32,
    /// Per FM operator, slot 0 being the one that reaches the carrier. Both
    /// are a-rate - an oscillator frequency and an ordinary gain - so unlike
    /// the block-latched params they are NOT latched per quantum.
    fm_index: [f32; crate::backend::MAX_FM_OPERATORS + 1],
    fm_freq: [f32; crate::backend::MAX_FM_OPERATORS + 1],
    /// Modulation aimed at another MODULATOR rather than at the voice.
    mod_params: ModParamAdds,
    /// The wavetable / supersaw source's own a-rate params.
    position: f32,
    freqspread: f32,
    panspread: f32,
    warp: f32,
    djf: f32,
}

/// Modulation aimed at a filter's own LFO rather than at the filter.
#[derive(Clone, Copy, Debug, Default)]
struct FilterLfoAdds {
    depth: f32,
    dc: f32,
    skew: f32,
}

/// Modulation aimed at one of the pattern's own `lfo()` modulators, by id.
#[derive(Clone, Copy, Debug, Default)]
struct LfoParamAdds {
    rate: f32,
    depth: f32,
    skew: f32,
    curve: f32,
    dcoffset: f32,
}

/// What a modulator can move on the wavetable source: its a-rate params plus
/// its own position LFO, the latter already latched for the quantum.
#[derive(Clone, Copy, Debug, Default)]
struct WavetableAdds {
    position: f32,
    freqspread: f32,
    panspread: f32,
    lfo_rate: f32,
    lfo_depth: f32,
    lfo_skew: f32,
    warp: f32,
    warp_lfo_rate: f32,
    warp_lfo_depth: f32,
    warp_lfo_skew: f32,
}

/// Modulation aimed at the wavetable's own position LFO.
#[derive(Clone, Copy, Debug, Default)]
struct WtLfoAdds {
    rate: f32,
    depth: f32,
    skew: f32,
}

/// Modulation of the pulse oscillator's own width LFO. Both params are read
/// through `parameters.x[0]`, so these values are quantum-held.
#[derive(Clone, Copy, Debug, Default)]
struct PulseWidthLfoAdds {
    rate: f32,
    depth: f32,
}

/// The same for an `env()`.
#[derive(Clone, Copy, Debug, Default)]
struct EnvParamAdds {
    depth: f32,
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
}

/// Everything a modulator can aim at ANOTHER modulator. Latched per render
/// quantum, because LFO and envelope params are block-rate.
#[derive(Clone, Copy, Debug, Default)]
struct ModParamAdds {
    /// Indexed by `FilterLfoKind::index` - lowpass, highpass, bandpass.
    filter: [FilterLfoAdds; 3],
    /// Indexed by the target modulator's id.
    lfo: [LfoParamAdds; crate::backend::MAX_VOICE_MODS],
    env: [EnvParamAdds; crate::backend::MAX_VOICE_MODS],
    /// The wavetable's own position LFO, and its warp LFO.
    wt: WtLfoAdds,
    warp: WtLfoAdds,
    /// The pulse source's own width LFO.
    pulse_width: PulseWidthLfoAdds,
}

impl ModAdds {
    /// The compressor with this frame's modulation folded in. Each param
    /// clamps to its own range, so a modulator cannot drive the ratio below
    /// 1 or the knee negative however deep it swings.
    fn compressor(
        &self,
        base: crate::backend::CompressorControls,
    ) -> crate::backend::CompressorControls {
        if self.comp_threshold == 0.0
            && self.comp_ratio == 0.0
            && self.comp_knee == 0.0
            && self.comp_attack == 0.0
            && self.comp_release == 0.0
        {
            return base;
        }
        crate::backend::CompressorControls {
            threshold_db: (base.threshold_db + self.comp_threshold).clamp(-100.0, 0.0),
            ratio: (base.ratio + self.comp_ratio).clamp(1.0, 20.0),
            knee_db: (base.knee_db + self.comp_knee).clamp(0.0, 40.0),
            attack_secs: (base.attack_secs + self.comp_attack).clamp(0.0, 1.0),
            release_secs: (base.release_secs + self.comp_release).clamp(0.0, 1.0),
        }
    }
}

/// These params latch at the START of each 128-frame render quantum and
/// hold for the whole block. Modulation of them is therefore block-rate,
/// unlike the a-rate filter and gain params above; sampling them per-sample
/// would sweep more smoothly than the latched behavior - audibly wrong.
#[derive(Clone, Copy, Debug, Default)]
struct FxParamHold {
    coarse: f32,
    crush: f32,
    shape: f32,
    shape_vol: f32,
    /// The phaser's LFO params. `frequency` and `depth` are block-latched
    /// too, so a modulator on the rate moves in quantum steps - and the
    /// rate matters more than the rest, because phase integrates it and a
    /// per-sample rate would drift the phase.
    phaser_rate: f32,
    phaser_sweep: f32,
    trem_rate: f32,
    trem_skew: f32,
    trem_shape: f32,
}

impl ModAdds {
    /// The block-rate subset, snapshotted at a quantum start.
    fn fx_hold(&self) -> FxParamHold {
        FxParamHold {
            coarse: self.coarse,
            crush: self.crush,
            shape: self.shape,
            shape_vol: self.shape_vol,
            phaser_rate: self.phaser_rate,
            phaser_sweep: self.phaser_sweep,
            trem_rate: self.trem_rate,
            trem_skew: self.trem_skew,
            trem_shape: self.trem_shape,
        }
    }

    /// Route one modulator's contribution, either to the main chain or to the
    /// `.FX()` stage it named.
    ///
    /// A stage is a fixed chain rather than the full param surface, so a
    /// target it cannot build is dropped here rather than silently landing on
    /// the main chain's bucket of the same name - which would modulate a
    /// different filter than the score asked for.
    fn bucket_at(
        &mut self,
        fxi: Option<u8>,
        target: crate::backend::ModTarget,
        value: f32,
        base: f32,
    ) {
        use crate::backend::ModTarget;
        let Some(stage) = fxi else {
            return self.bucket(target, value, base);
        };
        let Some(adds) = self.fx_stage.get_mut(usize::from(stage)) else {
            return;
        };
        match target {
            ModTarget::LowpassFreq => adds.lowpass_hz += value,
            ModTarget::HighpassFreq => adds.highpass_hz += value,
            ModTarget::BandFreq => adds.bandpass_hz += value,
            ModTarget::LowpassQ => adds.lowpass_q += value,
            ModTarget::HighpassQ => adds.highpass_q += value,
            ModTarget::BandQ => adds.band_q += value,
            ModTarget::Gain => {
                adds.gain += value;
                adds.gain_base = base;
            }
            _ => {}
        }
    }

    fn bucket(&mut self, target: crate::backend::ModTarget, value: f32, base: f32) {
        use crate::backend::ModTarget;
        match target {
            ModTarget::LowpassFreq => self.lowpass_hz += value,
            ModTarget::HighpassFreq => self.highpass_hz += value,
            ModTarget::BandFreq => self.bandpass_hz += value,
            ModTarget::VowelFreq => self.vowel_hz += value,
            ModTarget::Gain => {
                self.gain += value;
                self.gain_base = base;
            }
            ModTarget::Frequency => self.frequency_hz += value,
            ModTarget::Pan => self.pan += value,
            ModTarget::Postgain => {
                self.postgain += value;
                self.postgain_base = base;
            }
            ModTarget::LowpassQ => self.lowpass_q += value,
            ModTarget::HighpassQ => self.highpass_q += value,
            ModTarget::BandQ => self.band_q += value,
            ModTarget::Coarse => self.coarse += value,
            ModTarget::Crush => self.crush += value,
            ModTarget::Shape => self.shape += value,
            ModTarget::ShapeVol => self.shape_vol += value,
            ModTarget::Distort => self.distort += value,
            ModTarget::DistortVol => self.distort_vol += value,
            ModTarget::DelaySend => self.delay_send += value,
            ModTarget::RoomSend => self.room_send += value,
            ModTarget::PhaserRate => self.phaser_rate += value,
            ModTarget::PhaserSweep => self.phaser_sweep += value,
            ModTarget::PhaserCenter => self.phaser_center += value,
            ModTarget::PhaserDepth => self.phaser_depth += value,
            ModTarget::CompressorThreshold => self.comp_threshold += value,
            ModTarget::CompressorRatio => self.comp_ratio += value,
            ModTarget::CompressorKnee => self.comp_knee += value,
            ModTarget::CompressorAttack => self.comp_attack += value,
            ModTarget::CompressorRelease => self.comp_release += value,
            ModTarget::TremoloRate => self.trem_rate += value,
            ModTarget::TremoloDepth => self.trem_depth += value,
            ModTarget::TremoloSkew => self.trem_skew += value,
            ModTarget::TremoloShape => self.trem_shape += value,
            ModTarget::VibratoRate => self.vib_rate += value,
            ModTarget::VibratoDepth => self.vib_depth += value,
            ModTarget::PulseWidth => self.pulse_width += value,
            ModTarget::PulseWidthLfoRate => self.mod_params.pulse_width.rate += value,
            ModTarget::PulseWidthLfoDepth => self.mod_params.pulse_width.depth += value,
            ModTarget::Dry => self.dry += value,
            ModTarget::Djf => self.djf += value,
            ModTarget::DelayTime => self.delay_time += value,
            ModTarget::DelayFeedback => self.delay_feedback += value,
            ModTarget::FmIndex(slot) => {
                if let Some(add) = self.fm_index.get_mut(usize::from(slot)) {
                    *add += value;
                }
            }
            ModTarget::FmFreq(slot) => {
                if let Some(add) = self.fm_freq.get_mut(usize::from(slot)) {
                    *add += value;
                }
            }
            ModTarget::WavetablePosition => self.position += value,
            ModTarget::SourceFreqspread => self.freqspread += value,
            ModTarget::SourcePanspread => self.panspread += value,
            ModTarget::WtLfoRate => self.mod_params.wt.rate += value,
            ModTarget::WtLfoDepth => self.mod_params.wt.depth += value,
            ModTarget::WtLfoSkew => self.mod_params.wt.skew += value,
            ModTarget::WavetableWarp => self.warp += value,
            ModTarget::WarpLfoRate => self.mod_params.warp.rate += value,
            ModTarget::WarpLfoDepth => self.mod_params.warp.depth += value,
            ModTarget::WarpLfoSkew => self.mod_params.warp.skew += value,
            ModTarget::FilterLfoDepth(kind) => self.mod_params.filter[kind.index()].depth += value,
            ModTarget::FilterLfoDc(kind) => self.mod_params.filter[kind.index()].dc += value,
            ModTarget::FilterLfoSkew(kind) => self.mod_params.filter[kind.index()].skew += value,
            ModTarget::LfoParam(id, param) => {
                if let Some(add) = self.mod_params.lfo.get_mut(usize::from(id)) {
                    use crate::backend::ModulatorParam;
                    match param {
                        ModulatorParam::Rate => add.rate += value,
                        ModulatorParam::Depth => add.depth += value,
                        ModulatorParam::Skew => add.skew += value,
                        ModulatorParam::Curve => add.curve += value,
                        ModulatorParam::Dcoffset => add.dcoffset += value,
                    }
                }
            }
            ModTarget::EnvParam(id, param) => {
                if let Some(add) = self.mod_params.env.get_mut(usize::from(id)) {
                    use crate::backend::EnvelopeParam;
                    match param {
                        EnvelopeParam::Depth => add.depth += value,
                        EnvelopeParam::Attack => add.attack += value,
                        EnvelopeParam::Decay => add.decay += value,
                        EnvelopeParam::Sustain => add.sustain += value,
                        EnvelopeParam::Release => add.release += value,
                    }
                }
            }
        }
    }

    /// True when this target aims at another MODULATOR rather than at the
    /// voice. Those have to be summed BEFORE the modulators they aim at are
    /// evaluated, the way a node feeding an AudioParam is processed before the
    /// node that owns it.
    ///
    /// One level deep. A modulator of a modulator of a modulator would need
    /// a real topological order, which two passes do not provide;
    /// three-deep chains are refused.
    fn targets_another_modulator(target: crate::backend::ModTarget) -> bool {
        use crate::backend::ModTarget;
        matches!(
            target,
            ModTarget::FilterLfoDepth(_)
                | ModTarget::FilterLfoDc(_)
                | ModTarget::FilterLfoSkew(_)
                | ModTarget::LfoParam(..)
                | ModTarget::EnvParam(..)
                | ModTarget::WtLfoRate
                | ModTarget::WtLfoDepth
                | ModTarget::WtLfoSkew
                | ModTarget::WarpLfoRate
                | ModTarget::WarpLfoDepth
                | ModTarget::WarpLfoSkew
                | ModTarget::PulseWidthLfoRate
                | ModTarget::PulseWidthLfoDepth
        )
    }

    /// Multiplier the modulated gain param applies relative to its base
    /// (the base is already baked into the voice amplitude).
    fn gain_factor(&self) -> f32 {
        if self.gain == 0.0 || self.gain_base <= 0.0 {
            1.0
        } else {
            ((self.gain_base + self.gain) / self.gain_base).max(0.0)
        }
    }

    /// Same contract for the post gain stage.
    fn postgain_factor(&self) -> f32 {
        if self.postgain == 0.0 || self.postgain_base <= 0.0 {
            1.0
        } else {
            ((self.postgain_base + self.postgain) / self.postgain_base).max(0.0)
        }
    }
}

/// Clamp that tolerates an inverted range. `f32::clamp` PANICS when
/// `min > max`, which a modulator reaches legitimately: `depth = depth ·
/// currentValue` goes negative on a negative param base (`lfo({c:'pan'})`
/// with pan < 0.5 gives base 2·pan−1 < 0), and the LFO's
/// `min = dcoffset·depth`, `max = dcoffset·depth + depth` then arrive
/// inverted. This form returns `max` in that case.
///
/// A NaN input gives a bound and does not propagate. The `lfo()` site tests
/// its sample for NaN before this clamp. See [`nan_param_default`].
#[inline]
fn js_clamp(value: f32, min: f32, max: f32) -> f32 {
    value.max(min).min(max)
}

/// The default value of the AudioParam a modulator target names.
///
/// An `lfo()` sample is NaN when a fractional `curve` meets a negative value.
/// The sum on the target param is then NaN. WebAudio replaces a NaN param
/// value with the param default. `source` selects the param where the source
/// nodes differ.
///
/// The caller adds `default - base` in place of the NaN sample. The param
/// lands on the default when no other signal feeds the param. A second
/// modulator still adds. So do FM on a synth, vibrato and a pitch envelope on
/// a sample, and the LFO or envelope of a source param. An orbit param gets
/// one such sum from each voice. The five vowel filters share one base, so
/// the first one alone lands on 350 Hz.
///
/// The function returns NaN for a filter frequency and for the tremolo gain.
/// The sum stays NaN, and the reader of the sum takes the default alone.
#[cold]
fn nan_param_default(target: crate::backend::ModTarget, source: &VoiceSource) -> f32 {
    use crate::backend::{EnvelopeParam, ModTarget, ModulatorParam};
    // The wavetable and the supersaw worklets have different spread defaults.
    let (freqspread, panspread) = if matches!(source, VoiceSource::Supersaw { .. }) {
        (0.2, 0.4)
    } else {
        (0.18, 0.7)
    };
    match target {
        // The filter stage takes 350 Hz, or the 500 Hz of the ladder worklet.
        // The tremolo takes 1.
        ModTarget::LowpassFreq
        | ModTarget::HighpassFreq
        | ModTarget::BandFreq
        | ModTarget::TremoloDepth => f32::NAN,
        // BiquadFilterNode `frequency`.
        ModTarget::VowelFreq | ModTarget::PhaserCenter => 350.0,
        // A sample has no `frequency` param. Its pitch modulator rides
        // `detune`.
        ModTarget::Frequency if matches!(source, VoiceSource::Sample { .. }) => 0.0,
        // The `frequency` of an OscillatorNode and of each synth worklet.
        ModTarget::Frequency | ModTarget::FmFreq(_) | ModTarget::VibratoRate => 440.0,
        // BiquadFilterNode `Q`.
        ModTarget::LowpassQ | ModTarget::HighpassQ | ModTarget::BandQ | ModTarget::PhaserDepth => {
            1.0
        }
        // GainNode `gain`.
        ModTarget::Gain
        | ModTarget::Postgain
        | ModTarget::Dry
        | ModTarget::DelaySend
        | ModTarget::RoomSend
        | ModTarget::DelayFeedback
        | ModTarget::VibratoDepth
        | ModTarget::FmIndex(_) => 1.0,
        // StereoPannerNode `pan` and DelayNode `delayTime`.
        ModTarget::Pan | ModTarget::DelayTime => 0.0,
        ModTarget::CompressorThreshold => -24.0,
        ModTarget::CompressorRatio => 12.0,
        ModTarget::CompressorKnee => 30.0,
        ModTarget::CompressorAttack => 0.003,
        ModTarget::CompressorRelease => 0.25,
        // The effect and source worklets.
        ModTarget::Coarse | ModTarget::ShapeVol | ModTarget::DistortVol | ModTarget::PulseWidth => {
            1.0
        }
        ModTarget::Crush
        | ModTarget::Shape
        | ModTarget::Distort
        | ModTarget::WavetablePosition
        | ModTarget::WavetableWarp => 0.0,
        ModTarget::Djf => 0.5,
        ModTarget::SourceFreqspread => freqspread,
        ModTarget::SourcePanspread => panspread,
        // The LFO worklet: `frequency` and `skew` 0.5, `depth` and `curve` 1,
        // `dcoffset` and `shape` 0.
        ModTarget::PhaserRate
        | ModTarget::TremoloRate
        | ModTarget::TremoloSkew
        | ModTarget::PulseWidthLfoRate
        | ModTarget::WtLfoRate
        | ModTarget::WtLfoSkew
        | ModTarget::WarpLfoRate
        | ModTarget::WarpLfoSkew
        | ModTarget::FilterLfoSkew(_)
        | ModTarget::LfoParam(_, ModulatorParam::Rate | ModulatorParam::Skew) => 0.5,
        ModTarget::PhaserSweep
        | ModTarget::PulseWidthLfoDepth
        | ModTarget::WtLfoDepth
        | ModTarget::WarpLfoDepth
        | ModTarget::FilterLfoDepth(_)
        | ModTarget::LfoParam(_, ModulatorParam::Depth | ModulatorParam::Curve) => 1.0,
        ModTarget::TremoloShape
        | ModTarget::FilterLfoDc(_)
        | ModTarget::LfoParam(_, ModulatorParam::Dcoffset) => 0.0,
        // The envelope worklet.
        ModTarget::EnvParam(_, EnvelopeParam::Depth) => 1.0,
        ModTarget::EnvParam(_, EnvelopeParam::Attack) => 0.005,
        ModTarget::EnvParam(_, EnvelopeParam::Decay) => 0.14,
        ModTarget::EnvParam(_, EnvelopeParam::Sustain) => 0.0,
        ModTarget::EnvParam(_, EnvelopeParam::Release) => 0.1,
    }
}

/// One-time LFO phase seeding: `frac(time · frequency + phaseoffset)`.
///
/// An LFO is anchored to a clock rather than to the note: it starts wherever
/// a free-running oscillator of that frequency would have been at `time`.
/// Two consequences that are easy to miss. The frequency is the one read on
/// the first run past the begin gate, so modulating the rate moves the
/// starting point too. And a note at `time` 0 seeds to 0 whatever the
/// frequency - which is why a mistake here hides in the first note and only
/// shows up in the second.
///
/// Computed in f64: `time` is a context timestamp, large enough that an f32
/// product loses the fraction that is the whole answer.
#[inline]
/// The accumulator has to hold f64 precision, not just the seed. An LFO
/// adds `frequency/sampleRate` once per sample for the whole life of the
/// voice, and in f32 that walk arrives at the wrap a sample early:
/// simulating `tremolo(6)` at 48 kHz, f64 crosses 1.0 after 8000 steps and
/// f32 after 8001. The wrap is a discontinuity in every shape but the sine,
/// so landing it on the wrong sample puts a spurious step in the gain -
/// measured as a 4.6x spike two samples wide, once per LFO cycle.
///
/// The wavetable phases are f64 for the same reason.
fn lfo_phase0(time_secs: f32, frequency_hz: f32, phase_offset: f32) -> f64 {
    (f64::from(time_secs) * f64::from(frequency_hz) + f64::from(phase_offset)).rem_euclid(1.0)
}

/// Unipolar LFO waveshapes on phase in [0, 1]; phase stays f64. The walks
/// wrap on a strict `>`, so a phase of exactly 1.0 is stored and must read as
/// the start of the next cycle.
///
/// Casting it down first is not a rounding nicety: an f64 phase just under 1.0
/// does not wrap, but rounds to EXACTLY 1.0 in f32, and `tri` at the default
/// tremolo skew of 1 then takes its `phase >= skew` branch and divides by
/// `1 - skew`. `Infinity - Infinity` is NaN, and 14 of them reached the output
/// of `s("sawtooth").tremolo(4).tremolodepth(0.8)`.
fn lfo_waveshape(shape: u8, phase: f64, skew: f32) -> f32 {
    lfo_waveshape_f64(shape, phase, skew) as f32
}

/// [`lfo_waveshape`] before the cast to f32.
fn lfo_waveshape_f64(shape: u8, phase: f64, skew: f32) -> f64 {
    let skew = f64::from(skew);
    match shape {
        // tri
        0 => {
            let x = 1.0 - skew;
            // The same guards the f32 twin below carries. Keeping phase in
            // f64 stopped it ROUNDING to 1.0, but the walk wraps on a strict
            // `>`, so a rate whose ratio to the sample rate is dyadic
            // (375 Hz at 48 kHz = 2^-7) sums to EXACTLY 1.0 and stores it.
            // At the default tremolo skew of 1 that made `1/x - phase/x`
            // into `inf - inf` = NaN for one sample per LFO cycle, smeared
            // through everything downstream. The ramp's value at phase 1 is
            // its limit: 0.
            if phase >= skew {
                if x <= 0.0 { 0.0 } else { 1.0 / x - phase / x }
            } else if skew <= 0.0 {
                0.0
            } else {
                phase / skew
            }
        }
        // sine
        1 => (std::f64::consts::TAU * phase).sin() * 0.5 + 0.5,
        // ramp
        2 => phase,
        // saw
        3 => 1.0 - phase,
        // square
        4 => {
            if phase >= skew {
                0.0
            } else {
                1.0
            }
        }
        _ => 0.0,
    }
}

/// Envelope curve warp: curvature −1..1 bends the segment phase.
fn env_mod_warp(phase: f32, curvature: f32) -> f32 {
    const STRENGTH: f32 = 8.0;
    if phase == 0.0 || phase == 1.0 {
        return phase;
    }
    if curvature > 0.0 {
        let exp = 1.0 + STRENGTH * curvature;
        1.0 - (1.0 - phase).powf(exp)
    } else {
        let exp = 1.0 - STRENGTH * curvature;
        phase.powf(exp)
    }
}

/// `env()` modulator value at `t` seconds after the trigger, before
/// depth/min/max. Elapsed time divides by each state's CUMULATIVE threshold
/// (attack, attack+decay, susTime, susTime+release), so later segments
/// enter mid-phase with a value jump - deliberate, part of the sound.
fn env_mod_value(env: &crate::backend::EnvMod, t: f32) -> f32 {
    let a = env.attack_secs;
    let d = env.decay_secs;
    let sus_time = env.sustain_secs;
    let r = env.release_secs;
    let seg = |start: f32, target: f32, time: f32, curve: f32| -> f32 {
        if time == 0.0 || start == target {
            target
        } else {
            let phase = (t / time).min(1.0);
            start + (target - start) * env_mod_warp(phase, curve)
        }
    };
    if t < a {
        seg(0.0, 1.0, a, env.a_curve)
    } else if t < a + d {
        seg(1.0, env.sustain, a + d, env.d_curve)
    } else if t < sus_time {
        env.sustain
    } else if t < sus_time + r {
        seg(env.sustain, 0.0, sus_time + r, env.r_curve)
    } else {
        0.0
    }
}

#[inline]
/// Whether the LFO begin gate is open for this quantum.
///
/// No output or phase advance occurs until the first quantum strictly past
/// the f32-rounded begin time. Comparing it with the f64 render clock retains
/// AudioParam timing semantics: rounding can decide whether the onset's own
/// quantum runs.
fn lfo_quantum_open(quantum_start: u64, sample_rate: f64, begin_secs: f32) -> bool {
    (quantum_start as f64) / sample_rate > f64::from(begin_secs)
}

/// The first render quantum the begin gate opens on, as a frame.
///
/// For the one source that wants its gate precomputed rather than tested per
/// sample. Same rule as `lfo_quantum_open`, so it lands on the same quantum.
fn first_open_quantum(begin_secs: f32, sample_rate: f64) -> u64 {
    let floor = (f64::from(begin_secs) * sample_rate / 128.0).max(0.0) as u64 * 128;
    if lfo_quantum_open(floor, sample_rate, begin_secs) {
        floor
    } else {
        floor + 128
    }
}

/// Whether this quantum begins before the f32-rounded LFO end time.
fn lfo_quantum_alive(quantum_start: u64, sample_rate: f64, end_secs: f32) -> bool {
    (quantum_start as f64) / sample_rate < f64::from(end_secs)
}

/// Whether this quantum starts before the note's hold plus release ends.
/// Uses the hap duration, not the sample slice length: a sample can keep
/// sounding after its modulators stop. `onset_lead` preserves the fractional
/// frame offset lost when the onset was rounded to `start_frame`.
fn note_end_alive(
    quantum_start: u64,
    start_frame: u64,
    onset_lead: f32,
    hap_duration_secs: f32,
    release_secs: f32,
    sample_rate: f64,
) -> bool {
    let life = f64::from(hap_duration_secs) + f64::from(release_secs);
    (quantum_start as f64) < start_frame as f64 - f64::from(onset_lead) + life * sample_rate
}

/// Where a nudged source starts, as whole frames of silence plus the
/// sub-sample lead left over.
///
/// The nudge and `onset_lead` are floats, so the start can fall between output
/// frames. `ceil` gives the first output frame at or after the start, which is
/// the contract of `onset_lead`. The residue is how far the source has
/// advanced by then. A negative nudge uses the same expression: the delay is
/// zero and the whole offset becomes lead.
///
/// A truncation gives a delay one frame short. `nudge(0.02)` is 959.9999785
/// frames at 48 kHz, because the f32 is just under 0.02. With no `onset_lead`,
/// the delay is 960 frames, not 959.
fn nudged_start(nudge_secs: f32, onset_lead: f32, sample_rate: f64) -> (u32, f64) {
    let start_offset = f64::from(nudge_secs) * sample_rate - f64::from(onset_lead);
    let delay = start_offset.ceil().max(0.0);
    (delay as u32, delay - start_offset)
}

/// Where `begin()` starts reading, in frames.
///
/// A grain offset resolves to a whole frame, so a buffer source at playback
/// rate 1 reads samples directly. A fractional offset makes every read
/// interpolate from the first sample, which acts as a low-pass filter.
///
/// A resampled buffer can have a fractional offset. A 44.1 kHz file converted
/// to 48 kHz has a frame count that can put `begin()` between two samples.
fn begin_frame(begin: f32, source_frames: f64, playback_rate: f64) -> f64 {
    let frames = f64::from(begin) * source_frames;
    // At rate 1 a buffer source snaps its grain offset to a whole frame so
    // playback stays sample-exact; at any other rate it keeps the fraction
    // and interpolates from it.
    if playback_rate == 1.0 {
        frames.round()
    } else {
        frames
    }
}

/// Stereo pan, STEREO input branch (the mono branch is
/// `channel_gains`): x ≤ 0 folds right into left, x > 0 folds left into
/// right, equal-power.
fn stereo_pan(l: f32, r: f32, x: f32) -> (f32, f32) {
    use std::f32::consts::FRAC_PI_2;
    if x <= 0.0 {
        let a = (x + 1.0) * FRAC_PI_2;
        (l + r * a.cos(), r * a.sin())
    } else {
        let a = x * FRAC_PI_2;
        (l * a.cos(), r + l * a.sin())
    }
}

/// One sample of an oscillator voice at `phase`, which then advances by the
/// frequency the voice plays, `(freq_hz + fm_offset_hz) * pitch_ratio`, and
/// wraps into one period.
///
/// While FM or the pitch ratio moves that frequency, each sample reads the
/// band-limited table for it. As in Firefox's OscillatorNode, the frequency
/// is not clamped past Nyquist (Chromium clamps it to ±Nyquist): the table
/// for a frequency past Nyquist has no partials, and a sine aliases. A steady
/// frequency reads `table_selection`, the table chosen at the note's start.
#[allow(clippy::too_many_arguments)]
#[inline]
fn oscillator_sample(
    tables: &PeriodicWaveTables,
    controls: &VoiceRenderControls,
    table_selection: TableSelection,
    phase: &mut f64,
    freq_hz: f32,
    fm_offset_hz: f32,
    pitch_ratio: f32,
    sr: f64,
) -> f32 {
    let frequency = (freq_hz + fm_offset_hz) * pitch_ratio;
    let wave = match &controls.partials {
        Some(partials) => partials_sample(partials, *phase, frequency.abs(), (sr * 0.5) as f32),
        None if fm_offset_hz != 0.0 || pitch_ratio != 1.0 => {
            tables.sample_for_frequency(controls.waveform, *phase, frequency)
        }
        None => tables.sample(controls.waveform, *phase, table_selection),
    };
    *phase += f64::from(frequency) / sr;
    *phase -= phase.floor();
    wave
}

/// A custom periodic wave at `phase`, summed exactly from its partials, with
/// every partial of `f0` at or past `nyquist` left out (band-limited
/// playback).
fn partials_sample(partials: &PartialsControls, phase: f64, f0: f32, nyquist: f32) -> f32 {
    let mut sum = 0.0f32;
    for k in 0..usize::from(partials.len) {
        let harmonic = (k + 1) as f32;
        if f0 * harmonic >= nyquist {
            break;
        }
        let angle = std::f32::consts::TAU * harmonic * (phase as f32);
        sum += partials.real[k] * angle.cos() + partials.imag[k] * angle.sin();
    }
    sum * partials.norm
}

/// Sidechain automation on one orbit's output gain: hold the current value
/// about 10 ms before the trigger, ramp exponentially to the ducked value at
/// t+onset, then ramp exponentially back to 1 at t+onset+attack. Piecewise
/// math over absolute frames avoids per-frame state stepping, so retriggers
/// capture the true current value.
#[derive(Clone, Copy, Debug)]
struct PendingDuck {
    arm_frame: u64,
    onset_frame: u64,
    controls: crate::backend::DuckControls,
}

#[derive(Clone, Copy, Debug, Default)]
struct DuckSegments {
    active: bool,
    capture_frame: u64,
    from: f32,
    ducked: f32,
    mid_frame: u64,
    end_frame: u64,
}

/// Two generations of automation: arming happens at event INTAKE (a
/// schedule lead ahead of the onset), so frames before the new capture
/// point must still render the PREVIOUS curve - exactly what the old
/// AudioParam timeline would have played until `cancelScheduledValues`
/// fires 10 ms before the trigger.
#[derive(Clone, Copy, Debug, Default)]
struct OrbitDuck {
    current: DuckSegments,
    previous: DuckSegments,
}

impl DuckSegments {
    fn value_at(&self, frame: u64) -> f32 {
        if !self.active {
            return 1.0;
        }
        if frame >= self.end_frame {
            return 1.0;
        }
        if frame <= self.capture_frame {
            return self.from;
        }
        // exponentialRampToValueAtTime: v0 * (v1/v0)^((t-t0)/(t1-t0)),
        // endpoints clamped positive by the ducked-value clamp.
        let (t0, v0, t1, v1) = if frame <= self.mid_frame {
            (self.capture_frame, self.from, self.mid_frame, self.ducked)
        } else {
            (self.mid_frame, self.ducked, self.end_frame, 1.0)
        };
        if t1 <= t0 {
            return v1;
        }
        let progress = (frame - t0) as f32 / (t1 - t0) as f32;
        v0 * (v1 / v0).powf(progress)
    }
}

impl OrbitDuck {
    fn value_at(&self, frame: u64) -> f32 {
        if self.current.active && frame <= self.current.capture_frame {
            // Before the new hold point the OLD curve is still in charge.
            return self.previous.value_at(frame);
        }
        self.current.value_at(frame)
    }

    /// `duck(t, onset, attack, depth)` - schedule a dip at `t` (frames).
    fn trigger(&mut self, t: u64, now: u64, sample_rate: u32, duck: crate::backend::DuckTarget) {
        // The dip is armed ~10 ms before t and holds the CURRENT value as
        // the first ramp's start point.
        // When the trigger arrives later than the hold point, still anchor
        // at NOW (cancel+hold at now, t0 = max(t, now)) so the curve stays
        // continuous. Anchoring in already-rendered
        // frames instead stepped the gain onto the middle of the ramp and
        // clicked on every live sidechain trigger.
        let capture = t
            .saturating_sub((0.01 * sample_rate as f64) as u64)
            .max(now);
        let from = self.value_at(capture).max(0.0001);
        let attack = duck.attack_secs.max(0.002);
        // clamp(1 - sqrt(depth), 0.01, current) with the inverted-range
        // rule: min(max(x, lo), hi), so hi (the current value) wins when
        // below lo.
        let ducked = (1.0 - duck.depth.max(0.0).sqrt()).max(0.01).min(from);
        let sr = sample_rate as f64;
        let t0 = t.max(capture);
        self.previous = self.current;
        self.current = DuckSegments {
            active: true,
            capture_frame: capture,
            from,
            ducked,
            mid_frame: t0 + (f64::from(duck.onset_secs.max(0.0)) * sr) as u64,
            end_frame: 0, // filled below
        };
        self.current.end_frame = self.current.mid_frame + (f64::from(attack) * sr) as u64;
    }
}

#[derive(Clone, Debug)]
// The wavetable variant carries 32 f64 phases, which makes it much larger
// than the rest. Boxing it would put a pointer chase in the per-sample voice
// loop to save memory we are not short of - one voice's phases are 256 bytes.
#[allow(clippy::large_enum_variant)]
enum VoiceSource {
    /// `s("in")` - one channel of the audio input, read from the ring the
    /// input callback fills.
    Input { channel: u8 },
    /// `s("bus")` - the voice's source is bus N's mix rather than a generator.
    /// Receivers are evaluated after the senders, so the
    /// sum for this sample is already complete when it is read.
    Bus { bus: u8 },
    /// ZzFX's generate loop, streamed (see `crate::zzfx`). The ring is the
    /// `zdelay` history - leased from the pool only when the voice needs one,
    /// and an exhausted pool plays the voice undelayed rather than not at all.
    ZzFx {
        voice: crate::zzfx::ZzfxVoice,
        ring: Option<Box<[f32]>>,
    },
    /// `s("bytebeat")`. `t` is an integer sample counter, not a phase:
    /// seeded at `begin * sampleRate` on the first block past the begin
    /// gate and incremented once per sample from there.
    ByteBeat {
        expression: u8,
        /// A compiled custom expression, taking precedence over `expression`.
        program: Option<crate::bytebeat::ByteBeatProgram>,
        t: f64,
        /// Floored `byteBeatStartTime`, or zero after its presence has already
        /// selected the initial counter. Keep the sample loop branch-free.
        start_offset: f64,
        /// First frame the source may emit on: the begin gate holds through
        /// the quantum containing the onset, so output starts at the next
        /// 128-frame boundary strictly after it - and `t` still begins at
        /// the onset frame, not at the gate.
        gate: u64,
    },
    Oscillator {
        table_selection: TableSelection,
        phase: f64,
    },
    Sample {
        sample: SampleId,
        position: f64,
        increment: f64,
        /// `speed < 0`: read at `(frames−1) − position` - linear interp on
        /// a reversed buffer equals interp of the original at L−1−p, so
        /// only the read index flips.
        reversed: bool,
        /// `nudge` - source frames still to wait before playback begins
        /// (the amplitude envelope is NOT delayed).
        delay_frames: u32,
        /// Soundfont zone loop region in SOURCE frames - the buffer keeps
        /// looping for the voice's whole life while the envelope gates it.
        loop_frames: Option<(f64, f64)>,
    },
    /// Wavetable oscillator, per-voice phases inline (no allocation at
    /// trigger time on the audio thread).
    Wavetable {
        controls: crate::backend::WavetableControls,
        /// f64 on purpose. f32 is close enough for every warp mode that
        /// reads the table smoothly, and not for the ones that quantise:
        /// BINARY bit-reverses a 128-step staircase, so a phase landing one
        /// step over sends the read somewhere entirely different.
        phases: [f64; MAX_UNISON],
        /// The position LFO's phase. `None` until first use, seeded then
        /// from `frac(onset · rate)` with the rate READ THERE - the same
        /// contract as every other LFO. Integrating rather than recomputing
        /// `frac(now · rate)` is what makes a MODULATED rate come out
        /// right: frequency is a velocity, not a position.
        lfo_phase: Option<f64>,
        /// The warp LFO's phase, on the same contract.
        warp_lfo_phase: Option<f64>,
    },
    /// Supersaw: polyblep saw stack with per-voice detune and alternating
    /// pan.
    Supersaw {
        plan: PreparedSupersaw,
        phases: [f32; MAX_UNISON],
    },
    /// Pulse: half-Tomisawa cosine pair with feedback and anti-hunting
    /// filters - including the deliberate per-128-frame internal decay
    /// quirk.
    Pulse {
        pulsewidth: f32,
        width_lfo: Option<crate::backend::PulseWidthLfoControls>,
        width_lfo_phase: Option<f64>,
        phi: f64,
        y0: f64,
        y1: f64,
        dphif: f64,
        envf: f64,
        env: f64,
    },
    /// `sbd`: triangle osc with an exponential pitch envelope into its sampled
    /// WaveShaper curve and amplitude envelope, plus a 25 ms brown-noise
    /// attack. It bypasses the standard ADSR by design - the drum's own
    /// envelope is the whole sound.
    Sbd {
        controls: SbdState,
        phase: f64,
        noise: NoiseGen,
    },
    /// The looping noise buffer, played back from index 0.
    Noise {
        kind: u8,
        density: f32,
        state: NoiseGen,
    },
}

/// Immutable supersaw work shared by every sample of one voice.
///
/// The source's lane count, static detune ratios, and static pan gains depend
/// only on onset controls. Preparing them once keeps transcendental work out
/// of the steady sample loop. A-rate modulation still takes the original
/// calculation path for the samples where it contributes.
#[derive(Clone, Debug)]
struct PreparedSupersaw {
    voices: f32,
    count: usize,
    freqspread: f32,
    panspread: f32,
    detune_ratios: [f32; MAX_UNISON],
    pan_gains: (f32, f32),
}

impl PreparedSupersaw {
    fn new(voices: f32, freqspread: f32, panspread: f32) -> Self {
        let voices = voices.max(1.0);
        let count = (voices.ceil() as usize).clamp(1, MAX_UNISON);
        let (scale, center) = supersaw_spread_geometry(voices, freqspread.max(0.0));
        let mut detune_ratios = [1.0; MAX_UNISON];
        for (lane, ratio) in detune_ratios.iter_mut().enumerate().take(count) {
            *ratio = supersaw_detune_ratio(lane, scale, center);
        }
        Self {
            voices,
            count,
            freqspread,
            panspread,
            detune_ratios,
            pan_gains: supersaw_pan_gains(panspread),
        }
    }

    #[inline]
    fn pan_gains(&self, addition: f32) -> (f32, f32) {
        if addition.to_bits() == 0 {
            self.pan_gains
        } else {
            // Outside -1..1 a gain is the square root of a negative value.
            supersaw_pan_gains((self.panspread + addition).clamp(-1.0, 1.0))
        }
    }
}

#[derive(Debug, Default)]
struct ScalarPressureState {
    active_orbit_voices: [u16; MAX_ORBITS],
    active_pool_leases: [u64; REALTIME_POOL_COUNT],
    semantic_polyphony_fades: u64,
    voice_ceiling_drops: u64,
    pending_ceiling_drops: u64,
    pool_misses: [u64; REALTIME_POOL_COUNT],
    orbit_reverb_misses: u64,
}

impl ScalarPressureState {
    fn pool_miss(&mut self, pool: RealtimePool) {
        self.pool_misses[pool.index()] = self.pool_misses[pool.index()].saturating_add(1);
    }

    fn activate(&mut self, voice: &Voice) {
        let orbit = voice.orbit.min(MAX_ORBITS - 1);
        self.active_orbit_voices[orbit] = self.active_orbit_voices[orbit].saturating_add(1);

        let mut lease = |pool: RealtimePool, active: bool| {
            if active {
                self.active_pool_leases[pool.index()] =
                    self.active_pool_leases[pool.index()].saturating_add(1);
            }
        };
        lease(RealtimePool::Compressor, voice.compressor.is_some());
        lease(RealtimePool::Stretch, voice.stretch.is_some());
        lease(
            RealtimePool::ZzfxDelay,
            matches!(voice.source, VoiceSource::ZzFx { ring: Some(_), .. }),
        );
        for stage in voice
            .fx_stage_state
            .iter()
            .flat_map(|stages| stages.iter().flatten())
        {
            lease(RealtimePool::Compressor, stage.compressor.is_some());
            lease(RealtimePool::FxDelay, stage.delay.is_some());
            lease(RealtimePool::Stretch, stage.stretch.is_some());
            lease(RealtimePool::FxReverb, stage.room.is_some());
        }

        if voice.controls.compressor.is_some() && voice.compressor.is_none() {
            self.pool_miss(RealtimePool::Compressor);
        }
        if voice.controls.stretch.is_some() && voice.stretch.is_none() {
            self.pool_miss(RealtimePool::Stretch);
        }
        if let Some(stages) = &voice.fx_stage_state {
            for state in stages.iter().flatten() {
                let controls = &state.controls;
                if controls.compressor.is_some() && state.compressor.is_none() {
                    self.pool_miss(RealtimePool::Compressor);
                }
                if controls.delay.is_some() && state.delay.is_none() {
                    self.pool_miss(RealtimePool::FxDelay);
                }
                if controls.stretch.is_some() && state.stretch.is_none() {
                    self.pool_miss(RealtimePool::Stretch);
                }
                if controls.room.is_some() && state.room.is_none() {
                    self.pool_miss(RealtimePool::FxReverb);
                }
            }
        }
    }

    fn retire(&mut self, voice: &Voice) {
        let orbit = voice.orbit.min(MAX_ORBITS - 1);
        self.active_orbit_voices[orbit] = self.active_orbit_voices[orbit].saturating_sub(1);

        let mut release = |pool: RealtimePool, active: bool| {
            if active {
                self.active_pool_leases[pool.index()] =
                    self.active_pool_leases[pool.index()].saturating_sub(1);
            }
        };
        release(RealtimePool::Compressor, voice.compressor.is_some());
        release(RealtimePool::Stretch, voice.stretch.is_some());
        release(
            RealtimePool::ZzfxDelay,
            matches!(voice.source, VoiceSource::ZzFx { ring: Some(_), .. }),
        );
        for stage in voice
            .fx_stage_state
            .iter()
            .flat_map(|stages| stages.iter().flatten())
        {
            release(RealtimePool::Compressor, stage.compressor.is_some());
            release(RealtimePool::FxDelay, stage.delay.is_some());
            release(RealtimePool::Stretch, stage.stretch.is_some());
            release(RealtimePool::FxReverb, stage.room.is_some());
        }
    }

    fn clear_active(&mut self) {
        self.active_orbit_voices = [0; MAX_ORBITS];
        self.active_pool_leases = [0; REALTIME_POOL_COUNT];
    }
}

/// How many of the oldest sounds are killed before a new one is admitted.
///
/// The kill loop re-reads its own condition as the pool shrinks, so it does
/// not clamp straight back to the cap - 130 active sounds lose two and 131
/// also lose two, converging over the onsets that follow rather than all at
/// once.
fn polyphony_kills(active: usize, max_polyphony: usize) -> usize {
    let mut remaining = active as i64;
    let mut killed = 0i64;
    while killed <= remaining - max_polyphony as i64 {
        remaining -= 1;
        killed += 1;
    }
    killed as usize
}

/// Advance a voice's FM modulators by `steps` samples, with no envelope and
/// no per-voice modulation.
///
/// The two things left out are the two that would make a trajectory belong
/// to one note rather than to the configuration, and neither has happened
/// yet at the point this is used: an operator's envelope is scheduled AT
/// the note's start, so the gain it rides still holds the 1 it was
/// constructed with, and a modulator aimed at an FM parameter has the same
/// begin gate and has written nothing.
///
/// Three passes, needing no topological order: read every operator's output
/// at this sample, sum what each target receives, and only then advance.
fn fm_advance(
    fm: crate::backend::FmControls,
    freq_hz: f32,
    phases: &mut [f64; crate::backend::MAX_FM_OPERATORS + 1],
    noise: &mut [NoiseGen; crate::backend::MAX_FM_OPERATORS + 1],
    steps: u32,
    tables: &PeriodicWaveTables,
    sample_rate: u32,
) {
    for _ in 0..steps {
        let mut signal = [0.0f32; crate::backend::MAX_FM_OPERATORS + 1];
        for (slot, operator) in fm.operators.iter().enumerate() {
            let Some(op) = operator else {
                continue;
            };
            let base = freq_hz * op.harmonicity;
            signal[slot + 1] = match op.waveform.oscillator() {
                Some(waveform) => tables.sample_for_frequency(waveform, phases[slot], base),
                None => {
                    let crate::backend::FmWave::Noise(kind) = op.waveform else {
                        unreachable!("only a noise shape has no oscillator")
                    };
                    noise[slot].next(kind, 0.0)
                }
            };
        }
        let mut incoming = [0.0f32; crate::backend::MAX_FM_OPERATORS + 1];
        for route in fm.routes.iter().flatten() {
            let Some(Some(op)) = fm.operators.get(usize::from(route.source) - 1) else {
                continue;
            };
            incoming[usize::from(route.target)] +=
                signal[usize::from(route.source)] * route.amount * freq_hz * op.harmonicity;
        }
        for (slot, operator) in fm.operators.iter().enumerate() {
            let Some(op) = operator else {
                continue;
            };
            if !op.waveform.accepts_modulation() {
                continue;
            }
            let base = freq_hz * op.harmonicity;
            let phase = &mut phases[slot];
            *phase += f64::from(base + incoming[slot + 1]) / f64::from(sample_rate);
            *phase -= phase.floor();
        }
    }
}

/// The noise source is a two-second LOOPED buffer, so a note restarts the
/// same samples from index 0 and only wraps if it outlives the buffer.
#[derive(Clone, Copy, Debug)]
struct NoiseGen {
    rng: u64,
    /// The pink filter's seven poles (`b0`..`b6`).
    pink: [f32; 7],
    brown_last: f32,
    /// Position in the two-second buffer; wrapping restarts the sequence.
    index: u32,
    len: u32,
    /// Sub-sample read offset, `OnsetEvent::onset_lead`.
    ///
    /// A noise note starts at a FLOAT time, so one landing between output
    /// samples reads the buffer at a fractional position and every returned
    /// sample is linearly interpolated. On full-bandwidth material that is
    /// not a nuance: adjacent noise samples are uncorrelated, so mixing
    /// them attenuates by `sqrt((1-f)^2 + f^2)` - 0 dB at a whole sample
    /// and -3.01 dB at half of one. Measured against Chromium on
    /// `s("white")` at eight forced offsets, that curve held to within
    /// 0.4%.
    ///
    /// 0 for the FM modulator path, which always begins on a frame
    /// boundary.
    frac: f32,
    /// The next buffer sample, already generated. The recurrences only run
    /// forwards, so interpolating needs one sample of lookahead.
    pending: Option<f32>,
}

impl NoiseGen {
    /// Buffer length is `2 * sampleRate`, and the seed is fixed so renders
    /// are reproducible.
    fn new(sample_rate: u32) -> Self {
        Self {
            rng: 0x5EED_C0DE_u64 | 1,
            pink: [0.0; 7],
            brown_last: 0.0,
            index: 0,
            len: sample_rate.saturating_mul(2).max(1),
            frac: 0.0,
            pending: None,
        }
    }

    /// A noise source started at the note's own time, which can land
    /// between output frames. See `frac`.
    fn at_onset(sample_rate: u32, onset_lead: f32) -> Self {
        Self {
            frac: if onset_lead.is_finite() {
                onset_lead.clamp(0.0, 1.0)
            } else {
                0.0
            },
            ..Self::new(sample_rate)
        }
    }

    /// One output sample, read from the buffer at `frac` past `index`.
    fn next(&mut self, kind: u8, density: f32) -> f32 {
        let current = match self.pending.take() {
            Some(sample) => sample,
            None => self.raw(kind, density),
        };
        let next = self.raw(kind, density);
        self.pending = Some(next);
        // `frac` is 0 for every frame-aligned source, which returns `current`
        // unchanged and leaves the sequence exactly as it was.
        current + (next - current) * self.frac
    }

    /// The buffer's own contents - the per-type noise recurrences, before
    /// any playback interpolation.
    fn raw(&mut self, kind: u8, density: f32) -> f32 {
        if self.index >= self.len {
            // Only the generator restarts; the read offset belongs to the
            // playback and survives the loop, as does the lookahead.
            *self = Self {
                len: self.len,
                frac: self.frac,
                pending: self.pending,
                ..Self::new(self.len / 2)
            };
        }
        self.index += 1;
        // crackle draws its probability FIRST and only reaches for a value on
        // a hit, so it is handled before the shared white draw below.
        if kind == 3 {
            let roll = (noise_sample_u64(&mut self.rng) + 1.0) * 0.5;
            return if roll < density * 0.01 {
                noise_sample_u64(&mut self.rng)
            } else {
                0.0
            };
        }
        let white = noise_sample_u64(&mut self.rng);
        match kind {
            // brown: a leaky integrator over white.
            2 => {
                self.brown_last = (self.brown_last + 0.02 * white) / 1.02;
                self.brown_last
            }
            // pink: Paul Kellett's filter bank, verbatim.
            1 => {
                let b = &mut self.pink;
                b[0] = 0.99886 * b[0] + white * 0.0555179;
                b[1] = 0.99332 * b[1] + white * 0.0750759;
                b[2] = 0.969 * b[2] + white * 0.153852;
                b[3] = 0.8665 * b[3] + white * 0.3104856;
                b[4] = 0.55 * b[4] + white * 0.5329522;
                b[5] = -0.7616 * b[5] - white * 0.016898;
                let out = (b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + white * 0.5362) * 0.11;
                b[6] = white * 0.115926;
                out
            }
            _ => white,
        }
    }
}

impl FmState {
    fn new(sample_rate: u32) -> Self {
        Self {
            phases: [0.0; crate::backend::MAX_FM_OPERATORS + 1],
            quantum_skip: 0,
            noise: [NoiseGen::new(sample_rate); crate::backend::MAX_FM_OPERATORS + 1],
        }
    }

    fn restart(&mut self, onset_frame: u64, sample_rate: u32) {
        self.phases.fill(0.0);
        self.quantum_skip = (onset_frame % 128) as u8;
        self.noise.fill(NoiseGen::new(sample_rate));
    }
}

#[derive(Clone, Copy, Debug)]
struct SbdState {
    decay_secs: f32,
    pdecay_secs: f32,
    penv_semitones: f32,
    stop_secs: f32,
}

/// SBD noise-gain automation: start at 1.2, ramp exponentially to 0.001 over
/// 25 ms, then hold that endpoint until the voice stops.
fn sbd_noise_gain(t: f32) -> f32 {
    const START: f32 = 1.2;
    const END: f32 = 0.001;
    const DECAY: f32 = 0.025;
    if t >= DECAY {
        END
    } else {
        START * (END / START).powf((t / DECAY).max(0.0))
    }
}

/// SBD's graph becomes active one scheduler lead before its source starts.
/// This lets the zero-input WaveShaper establish the required pre-onset state
/// while the oscillator and noise clocks remain parked. A scheduling tick may
/// construct the graph earlier; 100 ms is the stable lower bound.
const SBD_GRAPH_LEAD_SECS: f64 = 0.1;

fn sbd_graph_lead_frames(sample_rate: u32) -> u64 {
    (f64::from(sample_rate) * SBD_GRAPH_LEAD_SECS).round() as u64
}

/// How far, in seconds, a takeover can move an onset whose copy a kept voice
/// still stands in for. A kept SBD onset is at most one graph lead after the
/// flip. A tempo change of 9% that starts at the flip, as a clock steer
/// does, moves it this far. A save changes the tempo from the edit instant,
/// which is before the flip, so a smaller tempo edit moves the onset as far.
/// The reach is half the 20 ms between the hits of a fast roll, so the
/// nearest kept voice is the voice of the same hit.
const KEPT_ONSET_REACH_SECS: f64 = 0.01;

fn kept_onset_reach_frames(sample_rate: u32) -> u64 {
    (f64::from(sample_rate) * KEPT_ONSET_REACH_SECS).round() as u64
}

/// The sample-rate-length transfer curve used by SBD's WaveShaper.
fn sbd_saturation_curve(sample_rate: u32) -> Box<[f32]> {
    let curve_len = sample_rate.max(2) as usize;
    (0..curve_len)
        .map(|index| {
            let x = index as f64 * 2.0 / curve_len as f64 - 1.0;
            (x * 2.0).tanh() as f32
        })
        .collect()
}

/// Linearly sample SBD's precomputed WaveShaper curve instead of evaluating
/// `tanh(2x)` directly. The curve is prepared with the backend, outside the
/// audio callback.
fn sbd_saturation(input: f32, curve: &[f32]) -> f32 {
    let curve_len = curve.len();
    let last = curve_len - 1;
    let virtual_index = ((input + 1.0) * 0.5 * last as f32).clamp(0.0, last as f32);
    let index = virtual_index.floor() as usize;
    let current = curve[index];
    if index == last {
        current
    } else {
        let next = curve[index + 1];
        current + (next - current) * (virtual_index - index as f32)
    }
}

/// Polyblep-corrected saw.
pub(crate) fn saw_blep(phase: f32, mut dt: f32) -> f32 {
    let v = 2.0 * phase - 1.0;
    dt = dt.min(1.0 - dt);
    if dt <= 0.0 {
        return v;
    }
    let invdt = 1.0 / dt;
    let blep = if phase < dt {
        let p = phase * invdt;
        2.0 * p - p * p - 1.0
    } else if phase > 1.0 - dt {
        let p = (phase - 1.0) * invdt;
        p * p + 2.0 * p + 1.0
    } else {
        0.0
    };
    v - blep
}

/// Native capacity limit that keeps each `Voice` bounded while supporting
/// dense unison configurations.
pub const MAX_UNISON: usize = 32;

#[inline]
fn supersaw_spread_geometry(voices: f32, spread: f32) -> (f32, f32) {
    if voices < 2.0 {
        (0.0, 0.0)
    } else {
        (spread / (voices - 1.0), spread * 0.5)
    }
}

#[inline]
fn supersaw_detune_ratio(lane: usize, scale: f32, center: f32) -> f32 {
    let detune = lane as f32 * scale - center;
    2.0f32.powf(detune / 12.0)
}

#[inline]
fn supersaw_pan_gains(panspread: f32) -> (f32, f32) {
    let panspread01 = panspread * 0.5 + 0.5;
    ((1.0 - panspread01).sqrt(), panspread01.sqrt())
}

/// Default polyphony cap. Sounds beyond it are faded out oldest-first; see
/// [`ScalarBackend::cull_for_polyphony`].
pub const MAX_POLYPHONY: usize = 128;

/// Largest supported musical voice limit. The separate hard voice ceiling
/// retains room for the quarter-second fades of voices retired by this limit.
pub const MAX_CONFIGURABLE_POLYPHONY: usize = 256;

struct PolyphonyLimit(usize);

impl Default for PolyphonyLimit {
    fn default() -> Self {
        Self(MAX_POLYPHONY)
    }
}

/// The fade a culled sound gets.
const POLYPHONY_FADE_SECS: f32 = 0.25;
/// Duration of the linear choke fade from full gain to silence, in seconds.
const CUT_FADE_SECS: f32 = 0.01;

/// LFO waveshapes, unipolar 0..1.
fn lfo_shape(shape: u8, phase: f32, skew: f32) -> f32 {
    match shape {
        0 => {
            // tri with skew
            let x = 1.0 - skew;
            if phase >= skew {
                if x <= 0.0 { 0.0 } else { 1.0 / x - phase / x }
            } else if skew <= 0.0 {
                0.0
            } else {
                phase / skew
            }
        }
        1 => ((std::f32::consts::TAU * phase).sin()) * 0.5 + 0.5,
        2 => phase,
        3 => 1.0 - phase,
        4 => {
            if phase >= skew {
                0.0
            } else {
                1.0
            }
        }
        _ => phase,
    }
}

/// White noise on a per-voice splitmix64 stream - seeded, like the unison
/// phases, so renders are reproducible.
fn noise_sample_u64(state: &mut u64) -> f32 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z ^= z >> 30;
    z = z.wrapping_mul(0xBF58476D1CE4E5B9);
    z ^= z >> 27;
    z = z.wrapping_mul(0x94D049BB133111EB);
    z ^= z >> 31;
    ((z >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}

/// Deterministic per-voice initial phase for `phaserand`: a hash of onset
/// id and voice index keeps renders reproducible.
fn seeded_phase(onset_id: u64, voice: usize) -> f32 {
    // Full splitmix64 finalizer: the single-round mix used first left
    // consecutive voice indices nearly collinear in the top bits, which
    // collapsed the unison field to mono (probed: phases 0.997, 0.995, …).
    let mut z = (onset_id ^ (voice as u64).wrapping_mul(0x9E3779B97F4A7C15))
        .wrapping_add(0x9E3779B97F4A7C15u64.wrapping_mul(voice as u64 + 1));
    z ^= z >> 30;
    z = z.wrapping_mul(0xBF58476D1CE4E5B9);
    z ^= z >> 27;
    z = z.wrapping_mul(0x94D049BB133111EB);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32
}

/// Stable admission order without compacting the large onset descriptions.
///
/// A live callback keeps roughly two seconds of future events here. Removing
/// the events due in one block with `Vec::retain` copied every later
/// [`OnsetEvent`] down over the holes. Those controls are large, and the copy
/// became a material part of callback time on dense scores. Events stay in
/// stable slots while a compact index vector preserves admission order. A
/// retain pass may move eight-byte indices, never the event payloads, and
/// removed slots form an allocation-free free list for later intake.
#[derive(Default)]
struct PendingEvents {
    slots: Vec<PendingEventSlot>,
    order: Vec<usize>,
    free: Option<usize>,
}

struct PendingEventSlot {
    event: Option<OnsetEvent>,
    confirmation: Option<crate::confirmation::ConfirmationOnset>,
    expected_sample_identity: Option<u64>,
    next_free: Option<usize>,
}

impl PendingEvents {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            order: Vec::with_capacity(capacity),
            ..Self::default()
        }
    }

    fn len(&self) -> usize {
        self.order.len()
    }

    fn capacity(&self) -> usize {
        self.slots.capacity().min(self.order.capacity())
    }

    fn push(&mut self, event: OnsetEvent) {
        self.push_with_confirmation(event, None, None);
    }

    fn push_with_confirmation(
        &mut self,
        event: OnsetEvent,
        confirmation: Option<crate::confirmation::ConfirmationOnset>,
        expected_sample_identity: Option<u64>,
    ) {
        let index = match self.free {
            Some(index) => {
                self.free = self.slots[index].next_free;
                self.slots[index] = PendingEventSlot {
                    event: Some(event),
                    confirmation,
                    expected_sample_identity,
                    next_free: None,
                };
                index
            }
            None => {
                let index = self.slots.len();
                self.slots.push(PendingEventSlot {
                    event: Some(event),
                    confirmation,
                    expected_sample_identity,
                    next_free: None,
                });
                index
            }
        };
        self.order.push(index);
    }

    /// Visit in insertion order and recycle rejected slots without moving any
    /// surviving event. Reused slots are always appended to `order`, so
    /// simultaneous onsets retain trigger order even after many removals.
    #[cfg(test)]
    fn retain(&mut self, mut keep: impl FnMut(&OnsetEvent) -> bool) {
        self.retain_with_confirmation(|event, _, _| keep(event));
    }

    fn retain_with_confirmation(
        &mut self,
        mut keep: impl FnMut(
            &OnsetEvent,
            Option<crate::confirmation::ConfirmationOnset>,
            Option<u64>,
        ) -> bool,
    ) {
        let mut write = 0;
        for read in 0..self.order.len() {
            let index = self.order[read];
            let keep_event = keep(
                self.slots[index]
                    .event
                    .as_ref()
                    .expect("pending order may only contain occupied slots"),
                self.slots[index].confirmation,
                self.slots[index].expected_sample_identity,
            );
            if keep_event {
                self.order[write] = index;
                write += 1;
            } else {
                self.slots[index].event = None;
                self.slots[index].confirmation = None;
                self.slots[index].expected_sample_identity = None;
                self.slots[index].next_free = self.free;
                self.free = Some(index);
            }
        }
        self.order.truncate(write);
    }

    fn clear(&mut self) {
        self.slots.clear();
        self.order.clear();
        self.free = None;
    }
}

/// The orbit faders, at unity by default. A backend built from `Default`
/// must not open silent.
#[derive(Clone, Copy, Debug)]
pub struct OrbitGains(pub [f32; MAX_ORBITS]);

impl Default for OrbitGains {
    fn default() -> Self {
        Self([1.0; MAX_ORBITS])
    }
}

/// The scalar renderer: voices, modulators, effects and orbit mixing,
/// computed one sample at a time.
#[derive(Default)]
pub struct ScalarBackend {
    max_polyphony: PolyphonyLimit,
    sample_rate: u32,
    sample_resampling_mode: SampleResamplingMode,
    frame: u64,
    dispatch: DspDispatch,
    pending: PendingEvents,
    #[cfg(feature = "device-audio")]
    pub(crate) confirmations: Option<crate::confirmation::ConfirmationConsumer>,
    voices: Vec<Voice>,
    preview_epoch: u64,
    /// Recent exact targets also correct prefetched onsets. The producer's
    /// ordinary requery replaces the horizon; sounding voices own their ramps.
    live_control_targets: Vec<crate::live_control::LiveControlUpdate>,
    tables: Option<Arc<PeriodicWaveTables>>,
    /// SBD's WaveShaper curve, allocated when the backend is prepared and only
    /// read by the audio callback.
    sbd_saturation_curve: Option<Box<[f32]>>,
    bank: Option<SampleBank>,
    /// Events dropped because their sample id had nothing installed yet -
    /// a "loading took too long" skip, never a fatal error.
    missing_sample_events: u64,
    /// One shared delay line per orbit, allocated at init (never in the
    /// callback); `active` orbits are the only ones mixed.
    orbit_delays: Vec<OrbitDelay>,
    orbit_ducks: Vec<OrbitDuck>,
    /// Duck triggers awaiting their ARM time (the 10 ms pre-hold point).
    /// Intake pushes here - for an offline render that is ALL events up
    /// front, and arming immediately would leave only the last automation
    /// generations alive. Each render block arms exactly the triggers whose
    /// hold point it contains, so order and timing match the live path.
    pending_ducks: Vec<PendingDuck>,
    orbit_reverbs: Vec<Option<Box<crate::reverb::OrbitReverb>>>,
    /// The outside effects and the instrument of each orbit. The notes that
    /// ask for an effect feed `insert_blocks`, and the output of the chain
    /// joins the orbit.
    orbit_inserts: Vec<Option<Box<dyn crate::insert::OrbitInsert>>>,
    /// Live inserts wait for a matching onset before replacing the active copy.
    prepared_inserts: Vec<Option<Box<dyn crate::insert::OrbitInsert>>>,
    /// Displaced copies wait in this bounded list for return to the producer.
    retired_inserts: Vec<Box<dyn crate::insert::OrbitInsert>>,
    insert_blocks: Vec<[[f32; crate::reverb::REVERB_BLOCK]; 2]>,
    /// Output orbit of each physical insert bus, selected by its latest onset.
    insert_outputs: [usize; MAX_ORBITS],
    /// Frames each insert had no input and no output. A long idle insert
    /// sleeps until a note asks for the effect again.
    insert_idle_frames: Vec<u32>,
    /// The effects of the orbit that run, one bit for each stage of the
    /// chain: the stages that served the last note with an effect.
    effect_stages: [u8; MAX_ORBITS],
    /// The instrument of the orbit goes through the effects of the orbit:
    /// the last instrument note asked for an effect too.
    instrument_to_effect: [bool; MAX_ORBITS],
    /// Offline only: builds a missing insert at activation, as the inline
    /// reverb does.
    insert_provider: Option<Arc<crate::insert::InsertProvider>>,
    /// Notes that asked for an insert their orbit did not hold.
    missing_insert_events: u64,
    /// Per-orbit stereo accumulation for one ≤128-frame sub-block: the
    /// pre-duck orbit signal (voices + delay + reverb returns).
    orbit_blocks: Vec<[[f32; crate::reverb::REVERB_BLOCK]; 2]>,
    /// Highest absolute sample each orbit put into the mix since the peaks
    /// were last taken: the orbit meters.
    orbit_peaks: [f32; MAX_ORBITS],
    /// Each orbit's fader, linear, applied after its duck and before the
    /// sum: the mixer's strips. Unity until the mixer says otherwise.
    orbit_gains: OrbitGains,
    /// Which output pair each orbit goes to: 0 is the main pair the mix
    /// is written to, higher pairs are kept apart in `orbit_mix`.
    orbit_pairs: [u8; MAX_ORBITS],
    /// Per-orbit stereo for the orbits not on the main pair, interleaved,
    /// `MAX_ORBITS` blocks of `ORBIT_MIX_FRAMES * 2`. Only written while
    /// some orbit is routed away.
    orbit_mix: Vec<f32>,
    /// Per-visual dry stereo sums for the current live render quantum.
    /// These are post-voice-DSP and pre-orbit: shared delay, reverb, ducking,
    /// DJF and orbit gain cannot be assigned to one receiver without mixing
    /// sibling voices back together. Only enabled bits are cleared and written.
    ui_visual_mix: Vec<[f32; UI_VISUAL_MIX_FRAMES * 2]>,
    ui_visual_capture_mask: u64,
    ui_visual_capture_generation_floor: u64,
    /// Per-orbit stereo reverb SEND accumulation (post-pan, scaled by wet).
    reverb_sends: Vec<[[f32; crate::reverb::REVERB_BLOCK]; 2]>,
    /// Offline renders may synthesise a missing orbit reverb at activation
    /// time; the LIVE path must never (producer installs instead).
    allow_inline_reverb: bool,
    /// Onsets whose orbit reverb was not installed yet: the voice plays DRY
    /// this time, like a still-loading skip.
    missing_reverb_events: u64,
    /// Onsets dropped at the concurrent-voice ceiling.
    voice_ceiling_drops: u64,
    /// Onsets dropped because the pending queue was already at its ceiling.
    pending_ceiling_drops: u64,
    /// Per-orbit DJ filter - created by the first `djf` trigger and sticky
    /// for the orbit's lifetime.
    orbit_djf: Vec<DjfState>,
    /// Where each ORBIT's output lands, keyed by the FIRST voice that ever
    /// triggers into it: the destination is wired once, at orbit creation,
    /// so the first voice's `channels` - or the plain stereo default when
    /// it has none - routes the whole orbit for its entire life, sends
    /// included, and every later voice's `channels` is INERT. `None` is an
    /// orbit nothing has triggered into yet.
    orbit_channels: Vec<Option<[u8; 2]>>,
    next_voice_id: u64,
    /// Independent, fixed release capacity: computer piano has 15 keys.
    /// Key-ups must still be admitted when the voice/pending pools are full.
    pending_chokes: [Option<(u64, u32)>; 16],
    /// Mutable FM operator state, leased only by voices that use FM. The pool
    /// keeps every activation allocation-free without making ordinary voices
    /// stride over phases and noise generators they never read.
    #[allow(clippy::vec_box)]
    fm_state_pool: Vec<Box<FmState>>,
    /// Pre-allocated compressor states (10 KB rings): leased to voices at
    /// activation, returned at retire. Empty pool → the voice plays
    /// uncompressed rather than allocating on the audio thread.
    compressor_pool: Vec<Box<CompressorState>>,
    /// Per-voice brickwalls for `.limit()`, leased at activation and returned
    /// at retire, one per voice the engine can sound at once so that a voice
    /// asking for a ceiling always gets one.
    ///
    /// Boxed for the reason `fm_state_pool` is boxed, not for the reason
    /// clippy's threshold assumes: the state is small - a four-frame ring
    /// rather than a kilobyte one - but it lives in `Voice` as
    /// `Option<Box<_>>`, and `Voice` is the struct the render loop strides
    /// over every frame for every voice. Keeping the rings off that stride is
    /// worth a pointer, which is why the lint is answered rather than obeyed.
    #[allow(clippy::vec_box)]
    limiter_pool: Vec<Box<crate::limiter::InlineLimiter>>,
    /// Leased by `.FX()` stages that name a delay. Allocated at init: a
    /// 1-second stereo line is 384 KiB at 48 kHz.
    fx_delay_pool: Vec<FxDelayLine>,
    /// `zdelay` history rings, two seconds each, leased by ZzFX voices that
    /// name a delay and returned when the voice ends. Allocated at init
    /// like every other pool; a `zdelay` past two seconds is clamped by the
    /// ring's own wrap.
    zzfx_ring_pool: Vec<Box<[f32]>>,
    /// Audio buses, as `.bus(n)` fills them and `.bmod({b: n})` reads them.
    ///
    /// Two copies, because a bus is read by voices in the same sample it is
    /// written by others and the voice list has no meaningful order. Receivers
    /// read what the previous sample produced, so the result does not depend
    /// on which voice happens to come first. One sample is 20 us at 48 kHz -
    /// far below anything audible, and the alternative is either an ordering
    /// that changes with the pattern or a second pass over every voice.
    bus_now: [[f32; 2]; crate::backend::MAX_BUSES],
    /// The audio input, when a device has one open.
    input: Option<std::sync::Arc<crate::input::InputRing>>,
    /// Where the input voices read next, in the ring's own frames: the read
    /// position for output frame zero of the next block. The ring places it
    /// and can move it. It holds a fraction when the ring runs at another
    /// rate than the output.
    input_cursor: Option<f64>,
    /// Reverbs available to `.FX()` stages, each already carrying a generated
    /// impulse response. A stage's reverb is per VOICE and its IR depends on
    /// that hap's own roomsize, so unlike the orbit reverbs these cannot be a
    /// fixed set: they are generated OFF the audio thread and installed here.
    /// Generating one costs 2.7 ms at roomsize 0.5 and 31 ms at 6, against
    /// the 2.7 ms a 128-frame callback has to spend.
    fx_reverb_pool: Vec<Box<crate::reverb::OrbitReverb>>,
    /// Params a stage asked for and did not get, for the producer to
    /// synthesise and install. Bounded: a pattern sweeping `roomsize` would
    /// otherwise queue one per hap forever.
    fx_reverb_wanted: Vec<crate::reverb::ReverbParams>,
    /// Bytes admitted through `install_fx_reverb`, including leased boxes.
    fx_reverb_bytes: usize,
    /// Offline inline generation retains its separate allocation policy.
    /// Each created box reserves an extra return slot before being leased.
    fx_reverb_inline_count: usize,
    /// Phase vocoders leased to voices / `.FX()` stages that name `stretch`.
    /// Built at init with shared FFT plans: activation must never run
    /// `FftPlanner` or allocate the analysis buffers on the audio thread.
    ///
    /// The `Box` is required. `Voice::stretch` is an
    /// `Option<Box<[Stretch; 2]>>`, so a boxed pool element moves into the
    /// voice as a pointer. Clippy's `Vec<[Stretch; 2]>` would return an
    /// array by value that needs `Box::new` to store: one heap allocation
    /// per activation on the audio thread, which this pool exists to avoid.
    #[allow(clippy::vec_box)]
    stretch_pool: Vec<Box<[crate::stretch::Stretch; 2]>>,
    /// Empty containers for the rare per-voice `.FX()` state. Keeping the
    /// 6.7 KiB array inline made every ordinary voice carry and stride over
    /// it. Containers are allocated on the producer side and leased here.
    #[allow(clippy::vec_box)]
    fx_stage_state_pool: Vec<Box<FxStageStates>>,
    /// A rewind's cut (a from-zero reload), armed by
    /// [`Self::begin_takeover_cut`] and expired once a block starts past its
    /// frame. The choke ramp makes the silence click-free.
    ///
    /// The pair is (cut frame, OUTGOING generation). The frame is where the
    /// outgoing rendition falls silent: the flip block for an immediate
    /// rewind (its ghost window - onsets scheduled between the flip and the
    /// takeover, a restarted loop's own first beat - must not sound, and the
    /// consumer drops those from the ring), the takeover frame for a
    /// quantised one (the countdown plays to its line), or the line itself
    /// when a pre-armed line cut fires before any flip. An edit's flip that
    /// lands before the frame (a control requery inside a quantised
    /// rewind's head-room) leaves the arm in place: the generation guard
    /// keeps the pre-fade off every generation but the one it names, and a
    /// countdown onset that activates after the edit's flip still has to
    /// stop at the line. That generation's onsets before the frame are kept
    /// through the edit's flip too, pending and in the ring, whatever the
    /// edit's own takeover (see [`Self::retire_pending_keeping_countdown`]).
    ///
    /// This is NOT the audition verb (`cut_sounding`), which silences
    /// immediately: the takeover cut is click-free over the choke ramp,
    /// and nothing of the new generation is touched. Voices constructed
    /// from pending events of the outgoing generation are pre-faded at
    /// activation by the same contract.
    pub(crate) armed_takeover_cut: Option<(u64, u64)>,
    /// Set while voices can stand in for events of the newest generation:
    /// (that generation, the block start its flip landed on, the last frame
    /// such an event can aim at). See [`Self::keep_started_onsets`].
    kept_onsets: Option<(u64, u64, u64)>,
    pressure: ScalarPressureState,
}

impl ScalarBackend {
    /// Select how this backend reads fractional positions in sample voices.
    /// The mode is sampled once per render block and does not allocate.
    pub fn set_sample_resampling_mode(&mut self, mode: SampleResamplingMode) {
        self.sample_resampling_mode = mode;
    }

    pub fn sample_resampling_mode(&self) -> SampleResamplingMode {
        self.sample_resampling_mode
    }

    /// Musical voice limit, excluding voices already in their retirement fade.
    pub fn max_polyphony(&self) -> usize {
        self.max_polyphony.0
    }

    /// Update this renderer's limit without allocating or resetting playback.
    /// Lowering it fades the oldest excess voices over a quarter second.
    pub fn set_max_polyphony(&mut self, limit: usize) {
        let limit = limit.clamp(1, MAX_CONFIGURABLE_POLYPHONY);
        if limit == self.max_polyphony.0 {
            return;
        }
        self.max_polyphony.0 = limit;
        let active = self
            .voices
            .iter()
            .filter(|voice| voice.polyphony_fade_frame.is_none())
            .count();
        let faded = Self::fade_oldest_voices(
            &mut self.voices,
            active.saturating_sub(limit),
            self.frame,
            self.sample_rate,
        );
        self.pressure.semantic_polyphony_fades = self
            .pressure
            .semantic_polyphony_fades
            .saturating_add(faded as u64);
    }

    /// Change a proven continuous binding without retriggering any voice.
    pub fn set_live_control(&mut self, update: crate::live_control::LiveControlUpdate) {
        if update.binding == 0 || !update.value.is_finite() {
            return;
        }
        for voice in &mut self.voices {
            voice.live_gain.retarget(update, self.sample_rate);
            voice.live_cutoff.retarget(update, self.sample_rate);
        }
        if let Some(target) = self
            .live_control_targets
            .iter_mut()
            .find(|target| target.binding == update.binding)
        {
            *target = update;
        } else if self.live_control_targets.len() < self.live_control_targets.capacity() {
            self.live_control_targets.push(update);
        } else if !self.live_control_targets.is_empty() {
            // Bounded history: even an exceptionally large score cannot make
            // a mouse update allocate inside the callback. Existing voices
            // already received the update above; new queries read the cell.
            self.live_control_targets.rotate_left(1);
            *self.live_control_targets.last_mut().unwrap() = update;
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a renderer with an immutable engine-kernel selection.
    pub fn with_dispatch(dispatch: DspDispatch) -> Self {
        Self {
            dispatch,
            ..Self::default()
        }
    }

    pub const fn dispatch(&self) -> DspDispatch {
        self.dispatch
    }

    /// Heap this backend wrote while it was prepared: the pools voices and
    /// `.FX()` stages lease from, filled up front so the callback never
    /// allocates, the saturation curve, and its own wave tables at a rate
    /// that does not share them. Room reserved and never written - voice
    /// and pending slots, delay lines, ZzFX rings, the orbit mix - is left
    /// out: zeroed or untouched, it takes memory only as voices reach it.
    /// Worked out on the thread that prepares the backend, before the
    /// callback owns it.
    pub fn prepared_bytes(&self) -> usize {
        use std::mem::size_of;
        let stretch: usize = self
            .stretch_pool
            .iter()
            .map(|pair| {
                size_of::<[crate::stretch::Stretch; 2]>()
                    + pair
                        .iter()
                        .map(crate::stretch::Stretch::heap_bytes)
                        .sum::<usize>()
            })
            .sum();
        let tables = self
            .tables
            .as_ref()
            .filter(|_| self.sample_rate != crate::periodic_wave::SHARED_TABLES_RATE)
            .map_or(0, |tables| tables.bytes());
        let curve = self
            .sbd_saturation_curve
            .as_deref()
            .map_or(0, std::mem::size_of_val);
        stretch
            + self.fx_stage_state_pool.len() * size_of::<FxStageStates>()
            + self.compressor_pool.len() * size_of::<CompressorState>()
            + self.fm_state_pool.len() * size_of::<FmState>()
            + self.limiter_pool.len() * size_of::<crate::limiter::InlineLimiter>()
            + curve
            + tables
    }

    fn ensure_fx_stage_state_pool(&mut self, capacity: usize) {
        let leased = self
            .voices
            .iter()
            .filter(|voice| voice.fx_stage_state.is_some())
            .count();
        let target = capacity.min(MAX_ACTIVE_VOICES);
        let available = self.fx_stage_state_pool.len().saturating_add(leased);
        self.fx_stage_state_pool
            .extend((available..target).map(|_| Box::new(std::array::from_fn(|_| None))));
    }

    fn ensure_fm_state_pool(&mut self, capacity: usize) {
        let leased = self
            .voices
            .iter()
            .filter(|voice| voice.fm_state.is_some())
            .count();
        let target = capacity.min(MAX_ACTIVE_VOICES);
        let available = self.fm_state_pool.len().saturating_add(leased);
        let sample_rate = self.sample_rate;
        self.fm_state_pool
            .extend((available..target).map(|_| Box::new(FmState::new(sample_rate))));
    }

    /// The orbit meters: each orbit's highest absolute sample since the
    /// last take, reset by taking.
    /// Give input voices a ring to read; the device that opens the input
    /// fills it.
    pub fn set_input(&mut self, ring: Option<std::sync::Arc<crate::input::InputRing>>) {
        self.input = ring;
        self.input_cursor = None;
    }

    pub fn take_orbit_peaks(&mut self) -> [f32; MAX_ORBITS] {
        std::mem::replace(&mut self.orbit_peaks, [0.0; MAX_ORBITS])
    }

    /// Keyboard voices have their own lifetime and cannot prolong a score stop.
    #[cfg(feature = "device-audio")]
    pub(crate) fn score_sources_active(&self) -> bool {
        self.voices.iter().any(|voice| !voice.piano)
            || self.pending.order.iter().any(|index| {
                self.pending.slots[*index]
                    .event
                    .as_ref()
                    .is_some_and(|event| !event.controls.piano)
            })
            || self
                .orbit_delays
                .iter()
                .any(|delay| delay.active && delay.energized != 0)
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn pressure_observation(&self) -> RealtimePressureObservation {
        RealtimePressureObservation {
            max_polyphony: self.max_polyphony() as u64,
            active_voices: u64::try_from(self.voices.len()).unwrap_or(u64::MAX),
            pending_events: u64::try_from(self.pending.len()).unwrap_or(u64::MAX),
            active_orbits: u64::try_from(
                self.pressure
                    .active_orbit_voices
                    .iter()
                    .filter(|voices| **voices != 0)
                    .count(),
            )
            .unwrap_or(u64::MAX),
            active_pool_leases: self.pressure.active_pool_leases,
            active_orbit_delays: u64::try_from(
                self.orbit_delays
                    .iter()
                    // An allocated line can be silent between an onset and
                    // its first echo. Stored energy, rather than the sticky
                    // routing flag, says whether Stop still owes an echo.
                    .filter(|delay| delay.active && delay.energized != 0)
                    .count(),
            )
            .unwrap_or(u64::MAX),
            active_orbit_reverbs: u64::try_from(
                self.orbit_reverbs
                    .iter()
                    .filter(|reverb| reverb.is_some())
                    .count(),
            )
            .unwrap_or(u64::MAX),
            active_dj_filters: u64::try_from(
                self.orbit_djf.iter().filter(|filter| filter.active).count(),
            )
            .unwrap_or(u64::MAX),
            semantic_polyphony_fades: self.pressure.semantic_polyphony_fades,
            voice_ceiling_drops: self.pressure.voice_ceiling_drops,
            pending_ceiling_drops: self.pressure.pending_ceiling_drops,
            pool_misses: self.pressure.pool_misses,
            orbit_reverb_misses: self.pressure.orbit_reverb_misses,
        }
    }

    /// Route each orbit to an output pair; pair 0 is the main mix.
    pub fn set_orbit_pairs(&mut self, pairs: [u8; MAX_ORBITS]) {
        self.orbit_pairs = pairs;
    }

    pub fn orbit_pairs(&self) -> [u8; MAX_ORBITS] {
        self.orbit_pairs
    }

    /// Whether any orbit is kept apart from the main pair.
    pub fn routing_active(&self) -> bool {
        self.orbit_pairs.iter().any(|pair| *pair != 0)
    }

    /// The stereo this orbit produced in the last block, interleaved, when
    /// it is routed away from the main pair; empty otherwise.
    pub fn orbit_mix(&self, orbit: usize, frames: usize) -> &[f32] {
        if orbit >= MAX_ORBITS || self.orbit_pairs[orbit] == 0 || self.orbit_mix.is_empty() {
            return &[];
        }
        let from = orbit * ORBIT_MIX_FRAMES * 2;
        &self.orbit_mix[from..from + frames.min(ORBIT_MIX_FRAMES) * 2]
    }

    pub fn set_ui_visual_capture_mask(&mut self, mask: u64) {
        self.ui_visual_capture_mask = mask;
    }

    pub fn set_ui_visual_capture_generation_floor(&mut self, generation: u64) {
        self.ui_visual_capture_generation_floor = generation;
    }

    /// Receiver-isolated post-voice, pre-orbit stereo audio.
    ///
    /// Shared orbit delay, reverb, ducking, DJF and orbit gain are excluded:
    /// attributing those mixed buses to one receiver would reintroduce sibling
    /// leakage.
    pub fn ui_visual_mix(&self, slot: usize, frames: usize) -> &[f32] {
        let Some(block) = self.ui_visual_mix.get(slot) else {
            return &[];
        };
        &block[..frames.min(UI_VISUAL_MIX_FRAMES) * 2]
    }

    /// Preallocate the complete voice/pending budget off the real-time thread.
    /// Install (or replace) a decoded sample in the bank. Offline surface:
    /// the live path delivers through the device's SPSC sample channel and
    /// installs between blocks.
    /// Sidechain scheduling, shared by both intake paths: queue the trigger
    /// for its ARM time (10 ms before the onset), never arm immediately.
    fn arm_duck(&mut self, event: &OnsetEvent) {
        self.arm_duck_at(event.onset_frame, event.controls.duck);
    }

    /// Arm a sidechain dip the moment its event is first seen - at SCHEDULE
    /// time, while the 10 ms pre-hold point is comfortably in the future.
    /// The duck applies even when the event itself is muted (speed 0) or
    /// its voice is later refused.
    pub fn arm_duck_at(&mut self, onset_frame: u64, duck: Option<crate::backend::DuckControls>) {
        if let Some(duck) = duck {
            if duck.is_empty() {
                return;
            }
            if self.pending_ducks.len() == self.pending_ducks.capacity() {
                // Full queue: dropping one duck dip beats allocating on the
                // audio thread. Capacity matches the voice budget.
                return;
            }
            let hold = (0.01 * f64::from(self.sample_rate.max(1))) as u64;
            self.pending_ducks.push(PendingDuck {
                arm_frame: onset_frame.saturating_sub(hold),
                onset_frame,
                controls: duck,
            });
        }
    }

    pub fn install_sample(
        &mut self,
        id: SampleId,
        sample: Box<DecodedSample>,
    ) -> Result<Option<Box<DecodedSample>>, Box<DecodedSample>> {
        // Callable before `init`: preloading an offline render's samples
        // must survive the later init, which only tops up the bundled slot.
        self.bank
            .get_or_insert_with(SampleBank::empty)
            .install(id, sample)
    }

    /// Empty a live slot so unused preview PCM can be reclaimed. The
    /// bundled `bd` is left alone.
    pub fn clear_sample(&mut self, id: SampleId) -> Option<Box<DecodedSample>> {
        self.bank.as_mut()?.clear(id)
    }

    /// The mixer's orbit faders, linear; 1 is unity. Read once a block by
    /// the callback from the device's shared state.
    pub fn set_orbit_gains(&mut self, gains: &[f32; MAX_ORBITS]) {
        self.orbit_gains = OrbitGains(*gains);
    }

    /// Events skipped because their sample was not installed at onset time.
    pub fn missing_sample_events(&self) -> u64 {
        self.missing_sample_events
    }

    /// Install (or replace) one orbit's reverb, returning the displaced one
    /// for the CALLER to free. Live path: the producer generates and ships
    /// through the device's channel; offline renders generate inline.
    pub fn install_reverb(
        &mut self,
        orbit: usize,
        mut reverb: Box<crate::reverb::OrbitReverb>,
    ) -> Option<Box<crate::reverb::OrbitReverb>> {
        if self.orbit_reverbs.is_empty() {
            self.orbit_reverbs = (0..MAX_ORBITS).map(|_| None).collect();
        }
        reverb.set_dispatch(self.dispatch);
        self.orbit_reverbs[orbit.min(MAX_ORBITS - 1)].replace(reverb)
    }

    /// The reverb an orbit currently has, for producer-side param checks.
    pub fn orbit_reverb_params(&self, orbit: usize) -> Option<crate::reverb::ReverbParams> {
        self.orbit_reverbs
            .get(orbit)?
            .as_ref()
            .map(|reverb| reverb.params())
    }

    /// Install or replace the insert of one slot: an effect of an orbit has
    /// [`crate::insert::effect_slot`], and the instrument has
    /// [`crate::insert::instrument_slot`]. The caller frees the displaced
    /// box outside the callback.
    pub fn install_insert(
        &mut self,
        slot: usize,
        insert: Box<dyn crate::insert::OrbitInsert>,
    ) -> Option<Box<dyn crate::insert::OrbitInsert>> {
        let slots = crate::insert::INSERT_SLOTS;
        if self.orbit_inserts.is_empty() {
            self.orbit_inserts = (0..slots).map(|_| None).collect();
        }
        self.orbit_inserts[slot.min(slots - 1)].replace(insert)
    }

    /// Stage a live insert until an accepted onset asks for it. The caller
    /// returns any displaced preparation to the producer.
    pub(crate) fn prepare_insert(
        &mut self,
        slot: usize,
        insert: Box<dyn crate::insert::OrbitInsert>,
    ) -> Option<Box<dyn crate::insert::OrbitInsert>> {
        let Some(prepared) = self.prepared_inserts.get_mut(slot) else {
            return Some(insert);
        };
        prepared.replace(insert)
    }

    /// Take a displaced live insert for off-thread reclamation.
    pub(crate) fn take_retired_insert(&mut self) -> Option<Box<dyn crate::insert::OrbitInsert>> {
        self.retired_inserts.pop()
    }

    /// The rate the backend renders at.
    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The insert a slot holds now.
    pub fn orbit_insert_key(&self, slot: usize) -> Option<crate::insert::InsertKey> {
        self.orbit_inserts
            .get(slot)?
            .as_ref()
            .map(|insert| insert.key())
    }

    /// Offline renders build a missing insert at activation with this
    /// provider. The live path ignores the provider and waits for an install.
    pub fn set_insert_provider(&mut self, provider: Arc<crate::insert::InsertProvider>) {
        self.insert_provider = Some(provider);
    }

    /// Notes that asked for an insert their orbit did not hold.
    pub fn missing_insert_events(&self) -> u64 {
        self.missing_insert_events
    }

    /// Live-path policy switch: forbid IR synthesis at activation time.
    pub fn forbid_inline_reverb(&mut self) {
        self.allow_inline_reverb = false;
    }

    /// Hand a stage reverb, generated off the audio thread, to the pool.
    /// Returns the box when the byte budget refuses it so the CALLER can free
    /// off the audio thread - dropping here would free IR partitions under the
    /// callback deadline.
    pub fn install_fx_reverb(
        &mut self,
        mut reverb: Box<crate::reverb::OrbitReverb>,
    ) -> Option<Box<crate::reverb::OrbitReverb>> {
        let bytes = reverb.approx_bytes();
        if bytes <= MAX_FX_REVERB_BYTES.saturating_sub(self.fx_reverb_bytes) {
            // Init reserves these slots before callback delivery. Pre-init
            // installation is also supported and prepares the same capacity.
            reserve_fx_reverb_returns(&mut self.fx_reverb_pool, self.fx_reverb_inline_count);
            reverb.set_dispatch(self.dispatch);
            self.fx_reverb_bytes += bytes;
            self.fx_reverb_pool.push(reverb);
            None
        } else {
            Some(reverb)
        }
    }

    /// Live installs may retire idle stage reverbs to make room for new
    /// parameters. Leased reverbs stay with their voices until retirement.
    /// The caller returns displaced boxes to the producer for destruction.
    pub fn install_fx_reverb_evicting(
        &mut self,
        reverb: Box<crate::reverb::OrbitReverb>,
        max_retire: usize,
        mut retire: impl FnMut(Box<crate::reverb::OrbitReverb>),
    ) -> Option<Box<crate::reverb::OrbitReverb>> {
        let bytes = reverb.approx_bytes();
        if bytes > MAX_FX_REVERB_BYTES {
            return Some(reverb);
        }
        let mut retired = 0;
        while bytes > MAX_FX_REVERB_BYTES.saturating_sub(self.fx_reverb_bytes)
            && retired < max_retire
            && !self.fx_reverb_pool.is_empty()
        {
            // Pointer movement only. Vec::remove would shift the whole pool
            // under the callback deadline; swap_remove does no allocation.
            let old = self.fx_reverb_pool.swap_remove(0);
            self.fx_reverb_bytes -= old.approx_bytes();
            retire(old);
            retired += 1;
        }
        self.install_fx_reverb(reverb)
    }

    pub(crate) fn fx_reverb_resident_bytes(&self) -> usize {
        self.fx_reverb_bytes
    }

    /// Reverb params `.FX()` stages asked for and did not get. Drained by the
    /// producer, which generates them and installs the result.
    pub fn take_wanted_fx_reverbs(&mut self) -> Vec<crate::reverb::ReverbParams> {
        std::mem::take(&mut self.fx_reverb_wanted)
    }

    /// Onsets that played dry because their orbit reverb was not ready.
    pub fn missing_reverb_events(&self) -> u64 {
        self.missing_reverb_events
    }

    /// One stereo frame of the wavetable oscillator.
    #[allow(clippy::too_many_arguments)]
    fn wavetable_sample(
        bank: &SampleBank,
        controls: &crate::backend::WavetableControls,
        phases: &mut [f64; MAX_UNISON],
        lfo_phase: &mut Option<f64>,
        warp_lfo_phase: &mut Option<f64>,
        onset_secs: f64,
        freq_hz: f32,
        t: f32,
        duration_secs: f32,
        sample_rate: f32,
        adds: WavetableAdds,
        wavetable_kernel: Option<WavetableKernel>,
    ) -> (f32, f32) {
        let Some(decoded) = bank.get(controls.table) else {
            return (0.0, 0.0);
        };
        let frame_len = (controls.frame_len as usize).max(1);
        let num_frames = (decoded.frames() / frame_len).max(1);
        let pcm = decoded.pcm();

        // Position = linear ADSR from the base (min) to base+amount, plus
        // the absolute-time-locked LFO, clamped 0..1 in the processor.
        let mut position = controls.position;
        if controls.pos_env_amount != 0.0 {
            let env = crate::backend::Envelope {
                attack_secs: controls.pos_attack,
                decay_secs: controls.pos_decay,
                sustain: controls.pos_sustain,
                release_secs: controls.pos_release,
            };
            let gate = Self::gate_value(duration_secs, env);
            position += controls.pos_env_amount * Self::envelope(t, duration_secs, env, gate);
        }
        if controls.lfo_depth != 0.0 {
            // The LFO's own params are block-latched; `position` is a-rate.
            let rate = controls.lfo_rate + adds.lfo_rate;
            let depth = controls.lfo_depth + adds.lfo_depth;
            let skew = controls.lfo_skew + adds.lfo_skew;
            let phase =
                *lfo_phase.get_or_insert_with(|| (onset_secs * f64::from(rate)).rem_euclid(1.0));
            let raw = (lfo_shape(controls.lfo_shape, phase as f32, skew) + controls.lfo_dc) * depth;
            // `min`/`max` were computed from the depth the LFO was BUILT
            // with, and nothing modulates them - so a modulated depth
            // widens `raw` while the bounds stay where they were.
            let lo = controls.lfo_dc * controls.lfo_depth;
            position += raw.clamp(
                lo.min(lo + controls.lfo_depth),
                lo.max(lo + controls.lfo_depth),
            );
            let next = phase + f64::from(rate) / f64::from(sample_rate);
            *lfo_phase = Some(if next > 1.0 { next - 1.0 } else { next });
        }
        position += adds.position;
        let position = position.clamp(0.0, 1.0);

        // Warp rides an envelope + LFO pair identical to position's, and
        // the result clamps 0..1 before use.
        let mode = crate::warp::WarpMode::from_index(i32::from(controls.warp_mode));
        let mut warp = controls.warp;
        if controls.warp_env_amount != 0.0 {
            let env = crate::backend::Envelope {
                attack_secs: controls.warp_attack,
                decay_secs: controls.warp_decay,
                sustain: controls.warp_sustain,
                release_secs: controls.warp_release,
            };
            let gate = Self::gate_value(duration_secs, env);
            warp += controls.warp_env_amount * Self::envelope(t, duration_secs, env, gate);
        }
        if controls.warp_lfo_depth != 0.0 {
            let rate = controls.warp_lfo_rate + adds.warp_lfo_rate;
            let depth = controls.warp_lfo_depth + adds.warp_lfo_depth;
            let skew = controls.warp_lfo_skew + adds.warp_lfo_skew;
            let phase = *warp_lfo_phase
                .get_or_insert_with(|| (onset_secs * f64::from(rate)).rem_euclid(1.0));
            let raw = (lfo_shape(controls.warp_lfo_shape, phase as f32, skew)
                + controls.warp_lfo_dc)
                * depth;
            let lo = controls.warp_lfo_dc * controls.warp_lfo_depth;
            warp += raw.clamp(
                lo.min(lo + controls.warp_lfo_depth),
                lo.max(lo + controls.warp_lfo_depth),
            );
            let next = phase + f64::from(rate) / f64::from(sample_rate);
            *warp_lfo_phase = Some(if next > 1.0 { next - 1.0 } else { next });
        }
        let warp = (warp + adds.warp).clamp(0.0, 1.0);

        let idx = position * (num_frames - 1) as f32;
        let frame_index = (idx as usize).min(num_frames - 1);
        let interp = idx - frame_index as f32;
        let frame_b = (frame_index + 1).min(num_frames - 1);

        let sample_frame = |frame: usize, phase: f32| -> f32 {
            let base = frame * frame_len;
            let pos = phase * frame_len as f32;
            // Derive the fraction before wrapping: warped phase can equal 1.
            let mut i = pos as usize;
            let frac = pos - i as f32;
            if i >= frame_len {
                i = 0;
            }
            let a = pcm.get(base + i).copied().unwrap_or(0.0);
            let mut i1 = i + 1;
            if i1 >= frame_len {
                i1 = 0;
            }
            let b = pcm.get(base + i1).copied().unwrap_or(0.0);
            a + (b - a) * frac
        };

        let voices_raw = controls.voices.max(1.0);
        let voices = (voices_raw.ceil() as usize).clamp(1, MAX_UNISON);
        let panspread = if voices_raw > 1.0 {
            (controls.panspread + adds.panspread).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let gain1 = (0.5 - 0.5 * panspread).sqrt();
        let gain2 = (0.5 + 0.5 * panspread).sqrt();
        let normalizer = 1.0 / (voices as f32).sqrt();
        let spread = (controls.freqspread + adds.freqspread).max(0.0);
        // getDetuner(unison, detune): raw-float division, `< 2` on the raw.
        let (scale, center) = if voices_raw < 2.0 {
            (0.0, 0.0)
        } else {
            (spread / (voices_raw - 1.0), spread * 0.5)
        };
        let mut left = 0.0f32;
        let mut right = 0.0f32;
        if let Some(kernel) = wavetable_kernel
            && let Some(sample) = Self::accelerated_wavetable_sample(
                kernel,
                pcm,
                frame_len,
                frame_index,
                frame_b,
                interp,
                phases,
                voices,
                gain1,
                gain2,
                normalizer,
                scale,
                center,
                freq_hz,
                sample_rate,
                warp,
                mode,
            )
        {
            return sample;
        }
        // `voices` is clamped to MAX_UNISON above, which is `phases.len()`, so
        // the take never shortens the loop.
        for (n, phase) in phases.iter_mut().enumerate().take(voices) {
            let (gain_l, gain_r) = if n & 1 == 1 {
                (gain2, gain1)
            } else {
                (gain1, gain2)
            };
            let detune = f64::from(n as f32 * scale - center);
            let voice_freq = f64::from(freq_hz) * 2.0f64.powf(detune / 12.0);
            let d_phase = voice_freq / f64::from(sample_rate);
            let raw_phase = *phase;
            let ph = crate::warp::warp_phase(raw_phase as f32, warp, mode);
            let s0 = sample_frame(frame_index, ph);
            let s1 = sample_frame(frame_b, ph);
            let mut value = s0 + (s1 - s0) * interp;
            // FLIP is not a phase transform: it inverts the SAMPLE over the
            // first `warp` of the cycle, and it tests the raw phase.
            if mode.flips_sample(raw_phase as f32, warp) {
                value = -value;
            }
            left += value * gain_l * normalizer;
            right += value * gain_r * normalizer;
            // The RAW phase is what advances. Warping is a read transform on
            // the way into the table, not a change to the oscillator's rate -
            // feeding the warped value back here would make the pitch depend
            // on the warp mode.
            *phase = (raw_phase + d_phase).fract();
        }
        (left, right)
    }

    /// Keep architecture-specific table work out of the already large source
    /// dispatcher. This boundary also leaves the original scalar loop compact
    /// and directly available for unsupported tables and CPUs.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn accelerated_wavetable_sample(
        kernel: WavetableKernel,
        pcm: &[f32],
        frame_len: usize,
        frame_index: usize,
        frame_b: usize,
        interp: f32,
        phases: &mut [f64; MAX_UNISON],
        voices: usize,
        gain1: f32,
        gain2: f32,
        normalizer: f32,
        scale: f32,
        center: f32,
        freq_hz: f32,
        sample_rate: f32,
        warp: f32,
        mode: crate::warp::WarpMode,
    ) -> Option<(f32, f32)> {
        if voices < 8 {
            return None;
        }

        let mut warped_phases = [0.0f32; MAX_UNISON];
        let mut phase_steps = [0.0f64; MAX_UNISON];
        let mut flips = [false; MAX_UNISON];
        for n in 0..voices {
            let detune = f64::from(n as f32 * scale - center);
            let voice_freq = f64::from(freq_hz) * 2.0f64.powf(detune / 12.0);
            phase_steps[n] = voice_freq / f64::from(sample_rate);
            let raw_phase = phases[n];
            warped_phases[n] = crate::warp::warp_phase(raw_phase as f32, warp, mode);
            flips[n] = mode.flips_sample(raw_phase as f32, warp);
        }

        let mut values = [0.0f32; MAX_UNISON];
        if !kernel.try_interpolate(
            pcm,
            frame_len,
            frame_index,
            frame_b,
            interp,
            &warped_phases,
            &mut values,
            voices,
        ) {
            return None;
        }

        let mut left = 0.0f32;
        let mut right = 0.0f32;
        for n in 0..voices {
            let (gain_l, gain_r) = if n & 1 == 1 {
                (gain2, gain1)
            } else {
                (gain1, gain2)
            };
            let mut value = values[n];
            if flips[n] {
                value = -value;
            }
            left += value * gain_l * normalizer;
            right += value * gain_r * normalizer;
            phases[n] = (phases[n] + phase_steps[n]).fract();
        }
        Some((left, right))
    }

    pub fn prepared(sample_rate: u32, voice_capacity: usize) -> Result<Self, String> {
        Self::prepared_with_dispatch(sample_rate, voice_capacity, DspDispatch::automatic())
    }

    /// Preallocate the renderer without changing its selected kernels on init.
    pub fn prepared_with_dispatch(
        sample_rate: u32,
        voice_capacity: usize,
        dispatch: DspDispatch,
    ) -> Result<Self, String> {
        let mut backend = Self {
            dispatch,
            pending: PendingEvents::with_capacity(voice_capacity),
            voices: Vec::with_capacity(voice_capacity),
            ..Self::default()
        };
        backend.init(sample_rate)?;
        backend.ensure_fx_stage_state_pool(voice_capacity);
        Ok(backend)
    }

    /// Submit only if the preallocated real-time capacity is sufficient.
    /// Returns false rather than allocating inside the callback.
    pub fn try_note_prepared(&mut self, event: OnsetEvent) -> bool {
        self.try_note_prepared_with_confirmation(event, None, None)
    }

    fn queue_choke(&mut self, event: OnsetEvent) -> bool {
        let Some(group) = event.cut else {
            return true;
        };
        let group = group.to_bits();
        if let Some(slot) = self
            .pending_chokes
            .iter_mut()
            .flatten()
            .find(|(_, key)| *key == group)
        {
            slot.0 = slot.0.max(event.onset_frame);
            return true;
        }
        if let Some(slot) = self.pending_chokes.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some((event.onset_frame, group));
            return true;
        }
        false
    }

    pub(crate) fn try_note_prepared_with_confirmation(
        &mut self,
        event: OnsetEvent,
        confirmation: Option<crate::confirmation::ConfirmationOnset>,
        expected_sample_identity: Option<u64>,
    ) -> bool {
        if event.controls.choke_only {
            return self.queue_choke(event);
        }
        // The LIVE consumer arms the sidechain at ring intake (arm_duck_at),
        // the first time it SEES the event - a schedule lead before the
        // onset. Admission happens only in the onset's own block, far too
        // late to start the 10 ms pre-hold ramp.
        let required = self
            .voices
            .len()
            .saturating_add(self.pending.len())
            .saturating_add(1);
        if self.pending.len() == self.pending.capacity() {
            self.pressure.pending_ceiling_drops =
                self.pressure.pending_ceiling_drops.saturating_add(1);
            #[cfg(feature = "device-audio")]
            if let (Some(consumer), Some(tag)) = (&mut self.confirmations, confirmation) {
                consumer.admission(tag, event.onset_frame, event.generation, false);
            }
            return false;
        }
        if required > self.voices.capacity() {
            self.pressure.voice_ceiling_drops = self.pressure.voice_ceiling_drops.saturating_add(1);
            #[cfg(feature = "device-audio")]
            if let (Some(consumer), Some(tag)) = (&mut self.confirmations, confirmation) {
                consumer.admission(tag, event.onset_frame, event.generation, false);
            }
            return false;
        }
        #[cfg(feature = "device-audio")]
        if let (Some(consumer), Some(tag)) = (&mut self.confirmations, confirmation) {
            consumer.admission(tag, event.onset_frame, event.generation, true);
        }
        self.pending
            .push_with_confirmation(event, confirmation, expected_sample_identity);
        true
    }

    /// Clear scheduled/active voices while retaining capacity and align the
    /// backend with an absolute live sample clock.
    pub fn reset_at(&mut self, frame: u64) {
        self.pending.clear();
        self.pending_chokes.fill(None);
        self.preview_epoch = 0;
        self.live_control_targets.clear();
        while let Some(mut voice) = self.voices.pop() {
            retire_voice_resources(
                &mut voice,
                &mut self.pressure,
                &mut self.fm_state_pool,
                &mut self.compressor_pool,
                &mut self.limiter_pool,
                &mut self.fx_delay_pool,
                &mut self.fx_reverb_pool,
                &mut self.zzfx_ring_pool,
                &mut self.stretch_pool,
                &mut self.fx_stage_state_pool,
            );
        }
        self.pressure.clear_active();
        self.kept_onsets = None;
        // Automation does not survive a clock reset: the graph builds new
        // gain curves and never replays an old AudioParam timeline.
        for duck in &mut self.orbit_ducks {
            *duck = OrbitDuck::default();
        }
        self.pending_ducks.clear();
        for reverb in self.orbit_reverbs.iter_mut().flatten() {
            reverb.reset();
        }
        for insert in self.orbit_inserts.iter_mut().flatten() {
            insert.reset();
        }
        self.insert_outputs = std::array::from_fn(|orbit| orbit);
        for bus in &mut self.orbit_delays {
            // Skip inactive lines: after the stop ramp reset_at runs on every
            // stopped block, and re-zeroing 16 one-second stereo lines would
            // write 6 MB per block.
            if bus.active {
                bus.left.fill(0.0);
                bus.right.fill(0.0);
                bus.active = false;
                bus.energized = 0;
            }
        }
        self.frame = frame;
    }

    /// Drop everything scheduled but not yet sounding; voices already
    /// sounding keep ringing through their envelopes. Live-update
    /// semantics: voices are independent, and a new evaluation never cuts
    /// one - a full reset would audibly click on every edit.
    ///
    /// The stop ramp calls this for each of its blocks, so nothing new
    /// starts under the ramp. The call also sets the clock to `frame`.
    pub fn retire_pending_at(&mut self, frame: u64) {
        self.pending.clear();
        self.frame = frame;
    }

    /// Silence everything sounding, over the choke ramp.
    ///
    /// A reload deliberately lets voices ring: an edit that cut them would
    /// click on every save. Auditioning is the other case - the snippet
    /// being replaced must stop, not play under the one asked for - so it
    /// has its own verb rather than a flag on the reload. The ramp is the
    /// choke group's, which is what a cut already sounds like here.
    pub fn cut_sounding_voices(&mut self, at_frame: u64) {
        self.cut_instrument_notes(at_frame);
        for voice in &mut self.voices {
            if voice.cut_fade_frame.is_none() {
                voice.cut_fade_frame = Some(at_frame);
            }
            let stop_at = (at_frame.saturating_sub(voice.start_frame)) as f32
                / self.sample_rate as f32
                + CUT_FADE_SECS;
            voice.stop_secs = voice.stop_secs.min(stop_at);
        }
    }

    /// An instrument holds its own notes. A cut of the engine voices at
    /// `at_frame` ends them at the same frame.
    fn cut_instrument_notes(&mut self, at_frame: u64) {
        let frames = at_frame.saturating_sub(self.frame).min(u64::from(u32::MAX)) as u32;
        let instruments = crate::insert::instrument_slot(0);
        for instrument in self.orbit_inserts.iter_mut().skip(instruments).flatten() {
            instrument.cut_notes(frames);
        }
    }

    /// A choke is also used as an internal note-off. It never needs a
    /// voice slot and must remain possible when the polyphony cap is full.
    fn choke_previous(voices: &mut [Voice], event: &OnsetEvent, frame: u64, sample_rate: u32) {
        let Some(cut) = event
            .cut
            .or_else(|| event.sample.and_then(|sample| sample.cut))
        else {
            return;
        };
        let nudge = event
            .sample
            .map(|sample| (f64::from(sample.nudge_secs) * f64::from(sample_rate)) as u64)
            .unwrap_or(0);
        let cut_at = event.onset_frame.saturating_add(nudge).max(frame);
        let key = cut.to_bits();
        // Voices retain trigger order, with no lifetime group registry limit.
        if let Some(previous) = voices
            .iter_mut()
            .filter(|voice| voice.cut_group == Some(key))
            .max_by_key(|voice| voice.id)
        {
            let fade = previous
                .cut_fade_frame
                .map_or(cut_at, |running| running.min(cut_at));
            previous.cut_fade_frame = Some(fade);
            let fade_end =
                fade.saturating_sub(previous.start_frame) as f32 / sample_rate as f32 + 0.011;
            previous.stop_secs = previous.stop_secs.min(fade_end);
        }
    }

    /// Takeover cut - the from-zero reload's (a rewind's) signature.
    ///
    /// Every voice of the OUTGOING rendition falls silent at `cut_frame`
    /// over the same choke ramp a cut group uses - click-free, so the
    /// restarted loop is heard alone, like a retriggered sample. An
    /// ordinary reload never calls this: its voices ring out by contract.
    ///
    /// `cut_frame` is the flip (the consumer's current block start) for an
    /// immediate restart: onsets the outgoing generation had already
    /// scheduled into the ghost window up to its takeover are part of what
    /// a restart replaces, so the consumer drops them from its ring and
    /// this arm only has to catch what is sounding or still activates.
    /// A QUANTISED rewind passes the takeover frame instead - the line the
    /// launch waited for - so the old score plays its countdown and every
    /// countdown activation pre-fades to stop at the line.
    ///
    /// The voices loop here is deliberately UNGUARDED by generation: it is
    /// only ever armed at a flip, where every sounding voice is by
    /// construction outgoing (the new generation has not admitted
    /// anything), or - for the quantised arm - before the line, where the
    /// new generation's first events target at/after the takeover and
    /// cannot have activated. Tails from generations older than the
    /// immediate outgoing one are choked too; a restart silences the past
    /// whole. The ACTIVATION pre-fade (at construction) stays
    /// generation-guarded: activations after the arm must prove they
    /// belong to the outgoing rendition before it stops them.
    ///
    /// Live input windows (`s("in")`) are cut like any other voice, on
    /// purpose. The input is a score event with a window, and a restarted
    /// loop opens its own window again from the top: the old one fading over
    /// the ramp while the new one opens hands the monitored signal over
    /// without a gap, where leaving it out of the cut would sound the input
    /// twice, at double level, until the old window's end.
    pub fn begin_takeover_cut(&mut self, cut_frame: u64, outgoing_generation: u64) {
        // A cut frame already behind the clock (a quantised flip published
        // late, or a line arm whose block has passed it) still fades from
        // NOW: the ramp's gain is `1 - (frame - fade) / ramp`, so a fade
        // pinned in the past is already at zero - a step to silence, the
        // click the choke ramp exists to avoid. The arm keeps the requested
        // frame so a late-activating outgoing onset is pre-faded to it.
        let fade_from = cut_frame.max(self.frame);
        self.cut_instrument_notes(fade_from);
        for voice in self.voices.iter_mut().filter(|voice| !voice.piano) {
            voice.cut_fade_frame = Some(match voice.cut_fade_frame {
                Some(fade) => fade.min(fade_from),
                None => fade_from,
            });
            let stop_at = (fade_from.saturating_sub(voice.start_frame)) as f32
                / self.sample_rate as f32
                + CUT_FADE_SECS;
            voice.stop_secs = voice.stop_secs.min(stop_at);
        }
        self.armed_takeover_cut = Some((cut_frame, outgoing_generation));
    }

    /// Reload takeover: the new generation re-queries from `takeover_frame`,
    /// so it replaces pending onsets at or after that frame. Earlier pending
    /// onsets cover the horizon the old generation already scheduled and
    /// must still sound, or every save cuts the music for one continuity
    /// margin. Like [`Self::retire_pending_at`], the clock syncs to the
    /// consumer's block cursor. The sync does nothing in steady playback. A
    /// consumer that begins mid-stream needs it.
    pub fn retire_pending_from(&mut self, takeover_frame: u64, at_frame: u64) {
        self.retire_pending_by(at_frame, |_| takeover_frame);
    }

    /// [`Self::retire_pending_from`] for an edit's flip to the generation
    /// `incoming`. The outgoing voices ring out, and those whose onset the
    /// incoming generation plays again stand in for its copies: see
    /// [`Self::keep_started_onsets`].
    pub(crate) fn hand_over_from(&mut self, incoming: u64, takeover_frame: u64, at_frame: u64) {
        self.retire_pending_by(at_frame, |_| takeover_frame);
        self.keep_started_onsets(incoming, at_frame, |_| takeover_frame);
    }

    /// [`Self::hand_over_from`] for an edit's flip that lands while a
    /// takeover cut is still ahead of it: a control requery inside a
    /// quantised rewind's head-room. The requery's takeover belongs to the
    /// score it re-queries, the restarted one, which has nothing before the
    /// line. Retired from there, the countdown's own onsets between the
    /// requery's takeover and the line were lost and nothing replaced them:
    /// moving a slider in the last bar muted the rest of the countdown. So
    /// the generation the arm names as outgoing is retired from the cut
    /// frame, its line, and every other generation from the takeover.
    pub(crate) fn retire_pending_keeping_countdown(
        &mut self,
        incoming: u64,
        takeover_frame: u64,
        (cut_frame, outgoing): (u64, u64),
        at_frame: u64,
    ) {
        let horizon = |generation| {
            if generation == outgoing {
                cut_frame
            } else {
                takeover_frame
            }
        };
        self.retire_pending_by(at_frame, horizon);
        self.keep_started_onsets(incoming, at_frame, horizon);
    }

    /// Retire every pending onset at or after the frame `horizon` names for
    /// its generation, reporting each as superseded, and sync the clock to
    /// `at_frame`. A borrowed closure over a fixed pool: nothing allocates.
    ///
    /// A voice is never retired here, also not an SBD voice whose graph runs
    /// ahead of its source: see [`Self::keep_started_onsets`].
    fn retire_pending_by(&mut self, at_frame: u64, horizon: impl Fn(u64) -> u64) {
        #[cfg(feature = "device-audio")]
        let confirmations = &mut self.confirmations;
        self.pending
            .retain_with_confirmation(|event, confirmation, _| {
                let keep = event.controls.piano || event.onset_frame < horizon(event.generation);
                #[cfg(feature = "device-audio")]
                if !keep && let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                    consumer.superseded(tag);
                }
                #[cfg(not(feature = "device-audio"))]
                let _ = confirmation;
                keep
            });
        self.frame = at_frame;
    }

    /// Mark the voices that stand in for events of `incoming`, the
    /// generation an edit's flip installs on the block at `at_frame`.
    ///
    /// The incoming generation plays every onset from its takeover frame
    /// again. A voice of an older generation with its onset on that frame or
    /// after it has started, so the takeover keeps it and the onset sounds.
    /// The caller leaves out the incoming copy, or the onset sounds twice.
    /// Such a voice is one of two kinds:
    ///
    /// - An SBD voice. Its graph starts one lead before its onset.
    /// - Any voice, when the flip lands after its takeover frame. A control
    ///   requery can publish that late.
    ///
    /// ```text
    ///              flip      takeover         onset
    /// old voice  ---|-- graph runs, source parked --x==== kept
    /// new event     |           |                 x       left out
    /// ```
    ///
    /// The two generations can round one hit to either side of the takeover
    /// frame. An SBD voice with its onset one frame before that frame is
    /// marked too, for the copy on the takeover frame only.
    ///
    /// `horizon` names the takeover frame for a voice's generation, as for
    /// [`Self::retire_pending_by`]. Frame zero names no takeover: the
    /// incoming generation starts a new lifetime and plays nothing again.
    fn keep_started_onsets(&mut self, incoming: u64, at_frame: u64, horizon: impl Fn(u64) -> u64) {
        let mut last_onset = None;
        for voice in self
            .voices
            .iter_mut()
            .filter(|voice| !voice.piano && voice.generation < incoming)
        {
            let takeover_frame = horizon(voice.generation);
            let one_frame_before = takeover_frame.checked_sub(1) == Some(voice.start_frame)
                && voice.graph_start_frame < voice.start_frame;
            let kept =
                one_frame_before || (takeover_frame != 0 && voice.start_frame >= takeover_frame);
            voice.replayed_by = kept.then_some(incoming);
            voice.replayed_on_next_frame = one_frame_before;
            if kept {
                last_onset = last_onset.max(Some(voice.start_frame));
            }
        }
        let reach = kept_onset_reach_frames(self.sample_rate);
        self.kept_onsets =
            last_onset.map(|frame| (incoming, at_frame, frame.saturating_add(reach)));
    }

    /// Whether a kept voice stands in for the event of `generation` aimed
    /// at `onset_frame`. It is the nearest voice that
    /// [`Self::keep_started_onsets`] marked for that generation, at most
    /// [`KEPT_ONSET_REACH_SECS`] away. The voice then stands in for no
    /// other event, so an event with no voice of its own near it sounds.
    ///
    /// `early_graph` says that the event is an SBD hit, and an event pairs
    /// only with a voice of its kind. Any other voice started before the
    /// block the flip landed on. It pairs only with an event aimed before
    /// that block, so an event that is on time always sounds.
    ///
    /// A voice one frame before the takeover frame pairs only with the
    /// event on that frame, so it never takes the next hit of a roll.
    ///
    /// A voice that a cut silences from its onset stands in for nothing. For
    /// a voice one frame before the takeover frame, that is a cut on the
    /// takeover frame or before it.
    /// Runs on the audio callback: a scan of the voice pool, no allocation.
    pub(crate) fn kept_onset_stands_for(
        &mut self,
        onset_frame: u64,
        generation: u64,
        early_graph: bool,
    ) -> bool {
        let Some((incoming, landed_at, last_frame)) = self.kept_onsets else {
            return false;
        };
        if generation != incoming
            || onset_frame > last_frame
            || (!early_graph && onset_frame >= landed_at)
        {
            return false;
        }
        let reach = kept_onset_reach_frames(self.sample_rate);
        let nearest = self
            .voices
            .iter_mut()
            .filter(|voice| {
                let (copy_frame, reach) = if voice.replayed_on_next_frame {
                    (voice.start_frame.saturating_add(1), 0)
                } else {
                    (voice.start_frame, reach)
                };
                voice.replayed_by == Some(generation)
                    && (voice.graph_start_frame < voice.start_frame) == early_graph
                    && voice.cut_fade_frame.is_none_or(|fade| fade > copy_frame)
                    && copy_frame.abs_diff(onset_frame) <= reach
            })
            .min_by_key(|voice| voice.start_frame.abs_diff(onset_frame));
        match nearest {
            Some(voice) => {
                voice.replayed_by = None;
                true
            }
            None => false,
        }
    }

    /// Whether an older generation started the SBD graph of the onset at
    /// `onset_frame`, at most one frame away. This is the rule for an event
    /// of a generation that a newer flip replaced. Two flips can land before
    /// the device reads the events of the first one, and then no voice is
    /// marked for that generation: see [`Self::keep_started_onsets`]. The
    /// event is still a copy of the onset that the voice plays.
    ///
    /// A voice that a cut silences from its onset stands in for nothing.
    /// Runs on the audio callback: a scan of the voice pool, no allocation.
    pub(crate) fn started_graph_plays(&self, onset_frame: u64, generation: u64) -> bool {
        self.voices.iter().any(|voice| {
            !voice.piano
                && voice.graph_start_frame < voice.start_frame
                && voice.generation < generation
                && voice
                    .cut_fade_frame
                    .is_none_or(|fade| fade > voice.start_frame)
                && voice.start_frame.abs_diff(onset_frame) <= 1
        })
    }

    /// Kill the oldest sounds when the polyphony cap is exceeded, run
    /// before every new sound is admitted.
    ///
    /// The kill condition is re-read as the pool shrinks (see
    /// [`polyphony_kills`]), so it settles at the configured limit. A
    /// killed sound leaves the pool at once and cannot be chosen again,
    /// though it is still audible through its quarter-second fade - which
    /// is why the count here ignores voices already fading.
    ///
    /// Without a cap, dense music grows steadily louder as voices pile up:
    /// a generated 916-onset score measured +3.899 dB overall.
    fn cull_for_polyphony(
        voices: &mut [Voice],
        onset_frame: u64,
        sample_rate: u32,
        max_polyphony: usize,
    ) -> usize {
        let active = voices
            .iter()
            .filter(|voice| voice.polyphony_fade_frame.is_none())
            .count();
        Self::fade_oldest_voices(
            voices,
            polyphony_kills(active, max_polyphony),
            onset_frame,
            sample_rate,
        )
    }

    fn fade_oldest_voices(
        voices: &mut [Voice],
        count: usize,
        onset_frame: u64,
        sample_rate: u32,
    ) -> usize {
        let mut faded = 0;
        for _ in 0..count {
            // `voices` is pushed in trigger order and `retain_mut` keeps it, so
            // the first still-counted voice is the oldest.
            let Some(oldest) = voices
                .iter_mut()
                .find(|voice| voice.polyphony_fade_frame.is_none())
            else {
                break;
            };
            oldest.polyphony_fade_frame = Some(onset_frame);
            let stop_at = (onset_frame.saturating_sub(oldest.start_frame)) as f32
                / sample_rate as f32
                + POLYPHONY_FADE_SECS;
            oldest.stop_secs = oldest.stop_secs.min(stop_at);
            faded += 1;
        }
        faded
    }

    /// `channels` - which OUTPUT channel each source channel is wired to.
    ///
    /// The control counts outputs from 1 (interface convention), and source
    /// channel `i` is wired to destination `channels[i]`. So a source
    /// channel with no entry is DROPPED, an output nobody names stays
    /// SILENT, two entries naming one output SUM there, and the order
    /// matters - `"2:1"` swaps the pair. The index wraps on the render's
    /// channel count, which is why `channels(3)` is `channels(1)` here and
    /// `"3:4"` is `"1:2"`.
    ///
    /// Without the routing, `channels(2)` plays from both outputs and is
    /// twice as loud in mono: +6.012 dB against Chromium, which leaves its
    /// left output silent.
    ///
    /// Applied at the ORBIT mix, deliberately: the orbit bus itself is
    /// connected to the named outputs, so every voice in the orbit - and
    /// its reverb and delay sends - travels together. Which list wins is
    /// decided at orbit creation; see `orbit_channels`.
    #[inline]
    fn route_channels(channels: Option<[u8; 2]>, left: f32, right: f32) -> (f32, f32) {
        let Some(destinations) = channels else {
            return (left, right);
        };
        let mut out = [0.0f32; 2];
        for (source, destination) in [left, right].into_iter().zip(destinations) {
            if destination == 0 {
                continue;
            }
            let slot = usize::from(destination - 1) % out.len();
            out[slot] += source;
        }
        (out[0], out[1])
    }

    fn gate_value(duration: f32, envelope: Envelope) -> f32 {
        if duration < envelope.attack_secs {
            duration / envelope.attack_secs
        } else if duration < envelope.attack_secs + envelope.decay_secs {
            1.0 + (envelope.sustain - 1.0)
                * ((duration - envelope.attack_secs) / envelope.decay_secs)
        } else {
            envelope.sustain
        }
    }

    fn envelope(t: f32, duration: f32, envelope: Envelope, gate_value: f32) -> f32 {
        if duration < 0.0 || t < 0.0 {
            return 0.0;
        }
        let attack = envelope.attack_secs;
        let decay = envelope.decay_secs;
        let sustain = envelope.sustain;
        let release = envelope.release_secs;
        if t >= duration {
            if release <= 0.0 || t >= duration + release {
                return 0.0;
            }
            return gate_value * (1.0 - (t - duration) / release);
        }
        if t < attack {
            return t / attack;
        }
        if t < attack + decay {
            let u = (t - attack) / decay;
            return 1.0 + (sustain - 1.0) * u;
        }
        sustain
    }

    /// Render one sample of the complete FM operator matrix.
    ///
    /// Read every operator at its current phase, sum routes into each target,
    /// then advance phases. This order gives every route the same phase
    /// snapshot and keeps floating-point accumulation consistent.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn render_fm_offset(
        fm: FmControls,
        freq_hz: f32,
        t: f32,
        duration_secs: f32,
        quantum_skip: u8,
        phases: &mut [f64; crate::backend::MAX_FM_OPERATORS + 1],
        noise: &mut [NoiseGen; crate::backend::MAX_FM_OPERATORS + 1],
        mod_adds: &ModAdds,
        tables: &PeriodicWaveTables,
        sample_rate: f64,
    ) -> f32 {
        // The depth automation is read from the start of the onset quantum.
        // Before that automation begins, depth remains at unity.
        let param_t = t - f32::from(quantum_skip) / sample_rate as f32;
        let depth_env = |envelope: Option<Envelope>, exponential: bool| match envelope {
            None => 1.0,
            _ if param_t < 0.0 => 1.0,
            Some(envelope) if exponential => crate::biquad::FilterStage::frequency_at(
                crate::backend::FilterEnvelope {
                    attack_secs: envelope.attack_secs,
                    decay_secs: envelope.decay_secs,
                    sustain: f64::from(envelope.sustain),
                    release_secs: envelope.release_secs,
                    min_hz: 0.0,
                    max_hz: 1.0,
                },
                param_t,
                duration_secs,
            ),
            Some(envelope) => Self::envelope(
                param_t,
                duration_secs,
                envelope,
                Self::gate_value(duration_secs, envelope),
            ),
        };

        let mut signal = [0.0f32; crate::backend::MAX_FM_OPERATORS + 1];
        for (slot, operator) in fm.operators.iter().enumerate() {
            let Some(op) = operator else {
                continue;
            };
            let base = freq_hz * op.harmonicity;
            let shape = match op.waveform.oscillator() {
                Some(waveform) => tables.sample_for_frequency(waveform, phases[slot], base),
                None => {
                    let crate::backend::FmWave::Noise(kind) = op.waveform else {
                        unreachable!("only a noise shape has no oscillator")
                    };
                    noise[slot].next(kind, 0.0)
                }
            };
            signal[slot + 1] = shape * depth_env(op.env, op.env_exponential);
        }

        // The route depth uses the source's static nominal frequency. Live
        // frequency modulation moves that oscillator but not the gain of the
        // route carrying it.
        let mut incoming = [0.0f32; crate::backend::MAX_FM_OPERATORS + 1];
        for route in fm.routes.iter().flatten() {
            let Some(Some(op)) = fm.operators.get(usize::from(route.source) - 1) else {
                continue;
            };
            let amount = route.amount
                + route
                    .mod_slot
                    .and_then(|slot| mod_adds.fm_index.get(usize::from(slot)).copied())
                    .unwrap_or(0.0);
            let base = freq_hz * op.harmonicity;
            incoming[usize::from(route.target)] +=
                signal[usize::from(route.source)] * amount * base;
        }

        for (slot, operator) in fm.operators.iter().enumerate() {
            let Some(op) = operator else {
                continue;
            };
            if !op.waveform.accepts_modulation() {
                continue;
            }
            let base = freq_hz * op.harmonicity;
            let osc_hz =
                base + mod_adds.fm_freq.get(slot).copied().unwrap_or(0.0) + incoming[slot + 1];
            let phase = &mut phases[slot];
            *phase += f64::from(osc_hz) / sample_rate;
            // A large excursion can cross more than one period in a sample.
            *phase -= phase.floor();
        }

        incoming[0]
    }

    fn channel_gains(pan: Option<f32>) -> (f32, f32) {
        let Some(pan) = pan else {
            return (1.0, 1.0);
        };
        let angle = f64::from(pan.clamp(0.0, 1.0)) * std::f64::consts::FRAC_PI_2;
        (angle.cos() as f32, angle.sin() as f32)
    }
}

impl AudioBackend for ScalarBackend {
    fn init(&mut self, sample_rate: u32) -> Result<(), String> {
        if sample_rate == 0 {
            return Err("sample_rate must be > 0".into());
        }
        self.sample_rate = sample_rate;
        reserve_fx_reverb_returns(&mut self.fx_reverb_pool, self.fx_reverb_inline_count);
        self.tables = Some(prepared_tables(sample_rate));
        self.sbd_saturation_curve = Some(sbd_saturation_curve(sample_rate));
        match self.bank.as_mut() {
            None => self.bank = Some(SampleBank::with_bundled_bd()?),
            Some(bank) => {
                // Re-init (or preloaded-before-init): keep installed samples,
                // just guarantee the bundled slot.
                if bank.get(crate::sample::BUNDLED_BD_SAMPLE_ID).is_none() {
                    let _ = bank.install(
                        crate::sample::BUNDLED_BD_SAMPLE_ID,
                        Box::new(bundled_sample(crate::sample::BundledSample::Bd)?),
                    );
                }
            }
        }
        // Fixed 1-second stereo line per orbit.
        self.orbit_ducks = vec![OrbitDuck::default(); MAX_ORBITS];
        self.orbit_djf = vec![DjfState::default(); MAX_ORBITS];
        self.orbit_channels = vec![None; MAX_ORBITS];
        self.limiter_pool = (0..LIMITER_POOL)
            .map(|_| {
                Box::new(crate::limiter::InlineLimiter::in_line(
                    self.sample_rate,
                    crate::limiter::DEFAULT_THRESHOLD_DB,
                    crate::limiter::Character::default(),
                ))
            })
            .collect();
        self.compressor_pool = (0..16 * (1 + crate::backend::MAX_FX_STAGES))
            .map(|_| Box::new(CompressorState::cold()))
            .collect();
        // Smaller than the compressor pool because each line is far larger:
        // 384 KiB at 48 kHz against 10 KiB. Sixteen covers sixteen voices
        // delaying in one stage, or five using all three.
        self.fx_delay_pool = (0..16).map(|_| FxDelayLine::new(sample_rate)).collect();
        self.zzfx_ring_pool = (0..8)
            .map(|_| vec![0.0f32; (sample_rate as usize) * 2].into_boxed_slice())
            .collect();
        // Shared FFT plans, buffers allocated once: leasing from this pool is
        // pointer work. Building Stretch::new inside process_block was the
        // live-path xrun for `.stretch()`.
        {
            let mut planner = rustfft::FftPlanner::new();
            let prototype = crate::stretch::Stretch::new(&mut planner);
            self.stretch_pool = (0..16 * (1 + crate::backend::MAX_FX_STAGES))
                .map(|_| Box::new([prototype.clone(), prototype.clone()]))
                .collect();
        }
        if self.fx_reverb_wanted.capacity() < 8 {
            self.fx_reverb_wanted = Vec::with_capacity(8);
        }
        if self.pending_ducks.capacity() == 0 {
            self.pending_ducks = Vec::with_capacity(4096);
        }
        if self.orbit_reverbs.is_empty() {
            self.orbit_reverbs = (0..MAX_ORBITS).map(|_| None).collect();
        }
        if self.orbit_inserts.is_empty() {
            self.orbit_inserts = (0..crate::insert::INSERT_SLOTS).map(|_| None).collect();
        }
        if self.prepared_inserts.is_empty() {
            self.prepared_inserts = (0..crate::insert::INSERT_SLOTS).map(|_| None).collect();
            self.retired_inserts = Vec::with_capacity(crate::insert::INSERT_SLOTS);
        }
        self.orbit_blocks = vec![[[0.0; crate::reverb::REVERB_BLOCK]; 2]; MAX_ORBITS];
        self.insert_blocks = vec![[[0.0; crate::reverb::REVERB_BLOCK]; 2]; MAX_ORBITS];
        self.insert_idle_frames = vec![0; crate::insert::INSERT_SLOTS];
        self.effect_stages = [0; MAX_ORBITS];
        self.instrument_to_effect = [false; MAX_ORBITS];
        self.reverb_sends = vec![[[0.0; crate::reverb::REVERB_BLOCK]; 2]; MAX_ORBITS];
        self.orbit_mix = vec![0.0; MAX_ORBITS * ORBIT_MIX_FRAMES * 2];
        self.ui_visual_mix = vec![[0.0; UI_VISUAL_MIX_FRAMES * 2]; MAX_UI_AUDIO_VISUALS];
        self.ui_visual_capture_mask = 0;
        self.ui_visual_capture_generation_floor = 0;
        self.orbit_peaks = [0.0; MAX_ORBITS];
        self.live_control_targets = Vec::with_capacity(MAX_ACTIVE_VOICES * 2);
        self.allow_inline_reverb = true;
        self.orbit_delays = (0..MAX_ORBITS)
            .map(|_| OrbitDelay {
                left: vec![0.0; sample_rate as usize],
                right: vec![0.0; sample_rate as usize],
                ..OrbitDelay::default()
            })
            .collect();
        self.reset();
        self.ensure_fm_state_pool(self.voices.capacity());
        Ok(())
    }

    fn reset(&mut self) {
        self.reset_at(0);
    }

    fn note(&mut self, event: OnsetEvent) {
        if event.controls.choke_only {
            let _ = self.queue_choke(event);
            return;
        }
        // Muted (speed 0) events STAY: the sidechain is still armed
        // and updates the orbit chain before the sampler bails; only the
        // source is skipped (activation handles that).
        if self.pending.len() >= MAX_PENDING_EVENTS {
            self.pending_ceiling_drops += 1;
            self.pressure.pending_ceiling_drops =
                self.pressure.pending_ceiling_drops.saturating_add(1);
            return;
        }
        self.arm_duck(&event);
        // A real-time consumer activates pending events by moving them into
        // `voices`. Reserve that final size HERE, on the producer thread, so
        // the later push cannot allocate in the audio callback.
        let required = self
            .voices
            .len()
            .saturating_add(self.pending.len())
            .saturating_add(1);
        if self.voices.capacity() < required {
            self.voices.reserve(required - self.voices.len());
        }
        self.ensure_fx_stage_state_pool(self.voices.capacity());
        self.ensure_fm_state_pool(self.voices.capacity());
        self.pending.push(event);
    }

    fn process_block(&mut self, out: &mut [f32], frames: usize) {
        enable_denormal_flush();
        assert!(
            out.len() >= frames * 2,
            "stereo interleaved buffer too small"
        );
        // A takeover cut expires once its block has passed the cut frame:
        // the block that contains it is the last one whose activation pass
        // can see an outgoing-generation onset, and a later flip re-arms or
        // clears the field. Checked per block - the arm's whole life is the
        // few blocks the cut needs.
        if let Some((cut_frame, _)) = self.armed_takeover_cut
            && self.frame > cut_frame
        {
            self.armed_takeover_cut = None;
        }
        let visual_frames = frames.min(UI_VISUAL_MIX_FRAMES) * 2;
        let mut visual_slots = self.ui_visual_capture_mask;
        while visual_slots != 0 {
            let slot = visual_slots.trailing_zeros() as usize;
            if let Some(block) = self.ui_visual_mix.get_mut(slot) {
                block[..visual_frames].fill(0.0);
            }
            visual_slots &= visual_slots - 1;
        }
        let routing = self.routing_active();
        if routing {
            let span = frames.min(ORBIT_MIX_FRAMES) * 2;
            for orbit in 0..MAX_ORBITS {
                if self.orbit_pairs[orbit] != 0 {
                    let from = orbit * ORBIT_MIX_FRAMES * 2;
                    self.orbit_mix[from..from + span].fill(0.0);
                }
            }
        }
        let sr = self.sample_rate.max(1) as f64;
        let tables = self
            .tables
            .as_ref()
            .expect("ScalarBackend::init must prepare oscillator tables");
        let sbd_saturation_curve = self
            .sbd_saturation_curve
            .as_deref()
            .expect("ScalarBackend::init must prepare the SBD WaveShaper curve");
        let bank = self
            .bank
            .as_ref()
            .expect("ScalarBackend::init must preload bundled samples");
        let missing_sample_events = &mut self.missing_sample_events;
        let start = self.frame;
        let end = start + frames as u64;
        let sample_rate = self.sample_rate;
        let ducks = &mut self.orbit_ducks;
        let pending_ducks = &mut self.pending_ducks;
        let reverbs = &mut self.orbit_reverbs;
        let inserts = &mut self.orbit_inserts;
        let prepared_inserts = &mut self.prepared_inserts;
        let retired_inserts = &mut self.retired_inserts;
        let insert_blocks = &mut self.insert_blocks;
        let insert_outputs = &mut self.insert_outputs;
        let insert_idle_frames = &mut self.insert_idle_frames;
        let effect_stages = &mut self.effect_stages;
        let instrument_to_effect = &mut self.instrument_to_effect;
        let insert_provider = self.insert_provider.as_deref();
        let missing_insert_events = &mut self.missing_insert_events;
        let allow_inline_reverb = self.allow_inline_reverb;
        let missing_reverb_events = &mut self.missing_reverb_events;
        let voice_ceiling_drops = &mut self.voice_ceiling_drops;
        let pressure = &mut self.pressure;
        let dispatch = self.dispatch;
        let supersaw_kernel = dispatch.supersaw();
        let wavetable_kernel = dispatch.wavetable();
        let voice_count = self.voices.len();
        let mut activated = 0usize;
        let live_control_targets = &self.live_control_targets;

        // Activate due graphs. Most voices activate at source onset. SBD gets
        // one scheduler lead so its connected WaveShaper can establish the
        // non-zero state it produces from zero input (see `sbd_saturation`).
        #[cfg(feature = "device-audio")]
        let confirmations = &mut self.confirmations;
        self.pending.retain_with_confirmation(|e, confirmation, expected_sample_identity| {
            #[cfg(not(feature = "device-audio"))]
            let _ = (confirmation, expected_sample_identity);
            #[cfg(feature = "device-audio")]
            let mut confirmation_sample_matches = true;
            let graph_start_frame =
                if matches!(e.synth, Some(crate::backend::SynthSource::Sbd { .. })) {
                    e.onset_frame
                        .saturating_sub(sbd_graph_lead_frames(sample_rate))
                } else {
                    e.onset_frame
                };
            if graph_start_frame < end {
                #[cfg(feature = "device-audio")]
                let missing_reverbs_before = *missing_reverb_events;
                if voice_count + activated >= MAX_ACTIVE_VOICES {
                    // Ceiling: drop this onset rather than let the mix cost
                    // grow without bound. Retiring voices free slots, so a
                    // dense-but-sane score never reaches this.
                    *voice_ceiling_drops += 1;
                    pressure.voice_ceiling_drops = pressure.voice_ceiling_drops.saturating_add(1);
                    #[cfg(feature = "device-audio")]
                    if let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                        consumer.activation(tag, crate::confirmation::ActivationOutcome::VoiceRefused);
                    }
                    return false;
                }
                activated += 1;
                let mut event = *e;
                if event.controls.live_controls != [0; 2] {
                    for target in live_control_targets {
                        if event.controls.live_controls[0] == target.binding {
                            event.gain = target.value;
                        }
                        if event.controls.live_controls[1] == target.binding
                            && let Some(filter) = event.controls.filters.lowpass.as_mut()
                        {
                            filter.frequency_hz = target.value;
                        }
                    }
                }
                let mut live_gain = crate::live_control::Ramp::new(event.controls.live_controls[0], event.gain);
                let live_cutoff = crate::live_control::Ramp::new(
                    event.controls.live_controls[1],
                    event.controls.filters.lowpass.map_or(0.0, |filter| filter.frequency_hz),
                );
                // Keep the source's unit-gain amplitude even when the slider
                // begins at zero, so a sustained silent note can fade in.
                // Dynamic gain is applied at the same point in the chain.
                if live_gain.binding != 0 {
                    event.gain = 1.0;
                } else {
                    live_gain = crate::live_control::Ramp::new(0, 1.0);
                }
                let e = &event;
                let (left_gain, right_gain) = Self::channel_gains(e.controls.pan);
                let mut filters = FilterChain::new(e.controls.filters, self.sample_rate);
                filters.set_block_phase(start);
                let has_filters = filters.is_active();
                // Built-in oscillators stop 10 ms after the
                // amplitude release. Chromium can then render a downstream
                // biquad tail until the graph teardown crosses render-quantum
                // boundaries, so retain two 128-frame quanta of zero-input
                // filter state. The unfiltered path still ends at release.
                let (source, duration_secs, amplitude_scale, natural_stop_secs) =
                    if let Some(synth) = e.synth {
                        match synth {
                            crate::backend::SynthSource::Supersaw {
                                voices,
                                freqspread,
                                panspread,
                            } => {
                                let plan = PreparedSupersaw::new(voices, freqspread, panspread);
                                let mut phases = [0.0f32; MAX_UNISON];
                                for (n, phase) in phases.iter_mut().enumerate().take(plan.count) {
                                    // Seeded, like the wavetable phases, so
                                    // renders are reproducible.
                                    *phase = seeded_phase(e.onset_frame, n);
                                }
                                let amplitude_scale =
                                    e.gain * e.controls.velocity * 0.3 / plan.voices.sqrt();
                                (
                                    VoiceSource::Supersaw { plan, phases },
                                    e.duration_secs,
                                    // env peak 0.3 × 1/√voices.
                                    amplitude_scale,
                                    None,
                                )
                            }
                            crate::backend::SynthSource::Pulse {
                                pulsewidth,
                                width_lfo,
                            } => (
                                VoiceSource::Pulse {
                                    pulsewidth,
                                    width_lfo,
                                    width_lfo_phase: None,
                                    // The pulse pair starts half a turn
                                    // out (phi = -PI), not at zero.
                                    phi: -std::f64::consts::PI,
                                    y0: 0.0,
                                    y1: 0.0,
                                    dphif: 0.0,
                                    envf: 0.0,
                                    env: 1.0,
                                },
                                e.duration_secs,
                                // The 0.15 factor is inside the algorithm.
                                // Unlike the table oscillators, pulse runs
                                // its envelope to 1 rather than 0.3, which
                                // is the whole of the level difference.
                                e.gain * e.controls.velocity,
                                None,
                            ),
                            crate::backend::SynthSource::ByteBeat {
                                expression,
                                program,
                                start_offset,
                            } => (
                                VoiceSource::ByteBeat {
                                    expression,
                                    program,
                                    start_offset: start_offset.unwrap_or(0.0),
                                    // byteBeatStartTime, when present,
                                    // resets `t`; otherwise the worklet seeds
                                    // it with `params.begin[0] * sampleRate`,
                                    // which is the note's begin as the
                                    // AudioParam holds it - f32, and NOT
                                    // rounded to a frame. An onset that falls
                                    // between two samples seeds a fraction,
                                    // and the counter carries that fraction
                                    // through every byte it evaluates.
                                    t: if start_offset.is_some() {
                                        0.0
                                    } else {
                                        f64::from(e.controls.worklet_begin_secs)
                                            * f64::from(self.sample_rate)
                                    },
                                    gate: first_open_quantum(
                                        e.controls.worklet_begin_secs,
                                        self.sample_rate as f64,
                                    ),
                                },
                                e.duration_secs,
                                // The bytebeat byte clamp is the only
                                // shaping; an ordinary synth ADSR rides on
                                // top.
                                e.gain * e.controls.velocity,
                                None,
                            ),
                            crate::backend::SynthSource::ZzFx { params } => {
                                let mut params = params;
                                // The random draw is seeded from the onset
                                // like every other stochastic source, so
                                // zrand(0) stays bit-comparable.
                                params.random_draw = f64::from(seeded_phase(e.onset_frame, 0));
                                let voice = crate::zzfx::ZzfxVoice::new(
                                    &params,
                                    f64::from(self.sample_rate),
                                );
                                let natural =
                                    (voice.frames() as f32 / self.sample_rate as f32).max(0.0);
                                let ring = (params.delay > 0.0)
                                    .then(|| self.zzfx_ring_pool.pop())
                                    .flatten();
                                if params.delay > 0.0 && ring.is_none() {
                                    pressure.pool_miss(RealtimePool::ZzfxDelay);
                                }
                                (
                                    VoiceSource::ZzFx { voice, ring },
                                    // The buffer runs to its own end whatever
                                    // the hap lasts: hold the (flat) envelope
                                    // for the whole of it.
                                    natural,
                                    // volume 0.25 is baked into the params;
                                    // the chain gain rides on top, no 0.3.
                                    e.gain * e.controls.velocity,
                                    Some(natural),
                                )
                            }
                            crate::backend::SynthSource::Bus { bus } => (
                                VoiceSource::Bus { bus },
                                e.duration_secs,
                                // `gainNode(0)` under an ADSR peaking at 1 -
                                // no 0.3 scale, unlike the synth sources.
                                e.gain * e.controls.velocity,
                                None,
                            ),
                            crate::backend::SynthSource::Input { channel } => (
                                VoiceSource::Input { channel },
                                e.duration_secs,
                                // A live signal at its own level, under the
                                // ordinary envelope. postgain is NOT folded
                                // in here: like every other source it rides
                                // the chain tap after the stages, and folding
                                // it would apply it twice (and move the
                                // level the FX stages see).
                                e.gain * e.controls.velocity,
                                None,
                            ),
                            crate::backend::SynthSource::Noise { kind, density } => (
                                VoiceSource::Noise {
                                    kind,
                                    density,
                                    state: NoiseGen::at_onset(self.sample_rate, e.onset_lead),
                                },
                                e.duration_secs,
                                // gainNode(0.3) then an ADSR peaking at 1,
                                // the ordinary synth shape.
                                e.gain * e.controls.velocity * 0.3,
                                None,
                            ),
                            crate::backend::SynthSource::Sbd {
                                decay_secs,
                                pdecay_secs,
                                penv_semitones,
                                stop_secs,
                            } => (
                                VoiceSource::Sbd {
                                    controls: SbdState {
                                        decay_secs,
                                        pdecay_secs,
                                        penv_semitones,
                                        stop_secs,
                                    },
                                    phase: 0.0,
                                    // Every SBD voice replays the session's
                                    // cached brown-noise buffer from the
                                    // beginning. `NoiseGen::at_onset` restarts
                                    // the prepared stream at each hit.
                                    noise: NoiseGen::at_onset(self.sample_rate, e.onset_lead),
                                },
                                // SBD runs its own envelopes; the standard
                                // ADSR must stay at unity until the stop.
                                stop_secs,
                                e.gain * e.controls.velocity,
                                Some(stop_secs),
                            ),
                        }
                    } else if let Some(wavetable) = e.wavetable {
                        #[cfg(feature = "device-audio")]
                        if let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                            confirmation_sample_matches = bank.get(wavetable.table)
                                .is_some_and(|decoded| expected_sample_identity == Some(decoded.identity()));
                            if !confirmation_sample_matches {
                                consumer.activation(tag, crate::confirmation::ActivationOutcome::MissingSample);
                            }
                        }
                        // The wavetable synth follows the OSCILLATOR gain
                        // path (ADSR to 0.3) and the hap gate; phases seed
                        // deterministically when phaserand is set.
                        let mut phases = [0.0f64; MAX_UNISON];
                        if wavetable.phaserand > 0.0 {
                            for (n, phase) in phases
                                .iter_mut()
                                .enumerate()
                                .take((wavetable.voices.max(1.0).ceil() as usize).min(MAX_UNISON))
                            {
                                *phase = f64::from(
                                    seeded_phase(e.onset_frame, n)
                                        * wavetable.phaserand.clamp(0.0, 1.0),
                                );
                            }
                        }
                        (
                            VoiceSource::Wavetable {
                                controls: wavetable,
                                phases,
                                lfo_phase: None,
                                warp_lfo_phase: None,
                            },
                            e.duration_secs,
                            e.gain * e.controls.velocity * 0.3,
                            None,
                        )
                    } else if let Some(sample) = e.sample {
                        let Some(decoded) = bank.get(sample.sample) else {
                            // Not installed yet: skip only this onset, as the
                            // reference does when a sound misses its start
                            // time. Later triggers find it loaded.
                            *missing_sample_events += 1;
                            #[cfg(feature = "device-audio")]
                            if let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                                consumer.activation(tag, crate::confirmation::ActivationOutcome::MissingSample);
                            }
                            return false;
                        };
                        // A previous payload in the same bank slot must not
                        // certify this candidate's required body. Keep the
                        // existing playback policy even when receipt evidence
                        // is missing or mismatched.
                        #[cfg(feature = "device-audio")]
                        if let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                            confirmation_sample_matches = expected_sample_identity == Some(decoded.identity());
                            if !confirmation_sample_matches {
                                consumer.activation(tag, crate::confirmation::ActivationOutcome::MissingSample);
                            }
                        }
                        let source_frames = decoded.frames() as f64;
                        let playback_rate = f64::from(sample.playback_rate);
                        let begin = begin_frame(sample.begin, source_frames, playback_rate);
                        let (duration_secs, natural_stop_secs) =
                            sample.duration_and_natural_stop(decoded, e.duration_secs);
                        let loop_frames = sample.loop_secs.map(|(start, end)| {
                            (
                                f64::from(start) * f64::from(decoded.sample_rate()),
                                f64::from(end) * f64::from(decoded.sample_rate()),
                            )
                        });
                        let increment = playback_rate * f64::from(decoded.sample_rate()) / sr;
                        let (delay, lead) = nudged_start(sample.nudge_secs, e.onset_lead, sr);
                        (
                            VoiceSource::Sample {
                                sample: sample.sample,
                                position: begin + lead * increment,
                                increment,
                                reversed: sample.reversed,
                                delay_frames: delay,
                                loop_frames,
                            },
                            duration_secs,
                            e.gain * e.controls.velocity * sample.envelope_peak,
                            natural_stop_secs,
                        )
                    } else {
                        (
                            VoiceSource::Oscillator {
                                table_selection: tables.selection(e.freq_hz),
                                phase: f64::from(e.onset_lead * e.freq_hz) / sr,
                            },
                            e.duration_secs,
                            e.gain * e.controls.velocity * 0.3,
                            None,
                        )
                    };
                let mut source_stop_secs = duration_secs + e.controls.envelope.release_secs;
                if let Some(natural_stop_secs) = natural_stop_secs {
                    source_stop_secs = source_stop_secs.min(natural_stop_secs);
                }
                let stop_secs = stop_secs_for(
                    source_stop_secs,
                    has_filters || e.controls.vowel.is_some(),
                    self.sample_rate as f32,
                );
                let reverb_wet = match e.controls.reverb {
                    Some(reverb) if reverb.wet > 0.0 => {
                        let orbit = (e.controls.orbit as usize).min(MAX_ORBITS - 1);
                        // A custom IR whose sample is not decoded yet
                        // falls back to the generated IR for THIS trigger
                        // (never-die: no awaiting a fetch mid-render). The
                        // LIVE path's producer installs generated-IR reverbs
                        // only, so live strips the ir instead of playing dry.
                        let ir = if allow_inline_reverb {
                            reverb.ir.filter(|ir| bank.get(ir.sample).is_some())
                        } else {
                            None
                        };
                        let wanted = crate::reverb::ReverbParams {
                            size_secs: reverb.size_secs,
                            fade_secs: reverb.fade_secs,
                            lp_start_hz: reverb.lp_start_hz,
                            lp_end_hz: reverb.lp_end_hz,
                            ir,
                        };
                        let installed = reverbs.get(orbit).and_then(|slot| slot.as_ref());
                        if installed.is_some_and(|existing| {
                            !allow_inline_reverb || existing.params() == wanted
                        }) {
                            reverb.wet
                        } else if allow_inline_reverb {
                            // Offline: synthesise at activation, on the
                            // producer side of the pipeline.
                            reverbs[orbit] = Some(Box::new(match ir {
                                Some(params_ir) => match bank.get(params_ir.sample) {
                                    Some(decoded) => crate::reverb::OrbitReverb::generate_custom_with_dispatch(
                                        sample_rate,
                                        wanted,
                                        decoded,
                                        dispatch,
                                    ),
                                    None => crate::reverb::OrbitReverb::generate_with_dispatch(
                                        sample_rate,
                                        crate::reverb::ReverbParams { ir: None, ..wanted },
                                        dispatch,
                                    ),
                                },
                                None => crate::reverb::OrbitReverb::generate_with_dispatch(sample_rate, wanted, dispatch),
                            }));
                            reverb.wet
                        } else {
                            // Live and not installed yet: this onset plays
                            // dry; the producer's install lands for later
                            // triggers. Never fatal.
                            *missing_reverb_events += 1;
                            pressure.orbit_reverb_misses =
                                pressure.orbit_reverb_misses.saturating_add(1);
                            0.0
                        }
                    }
                    _ => 0.0,
                };
                // Gives the insert of a slot what the note asks for. False
                // means the slot does not hold the insert, and the note
                // plays with no insert.
                let mut serve = |wanted: &crate::insert::InsertControls, slot: usize| {
                    let Some(held) = inserts.get_mut(slot) else {
                        return false;
                    };
                    if held.as_ref().is_none_or(|held| held.key() != wanted.key)
                        && prepared_inserts[slot]
                            .as_ref()
                            .is_some_and(|prepared| prepared.key() == wanted.key)
                        && retired_inserts.len() < retired_inserts.capacity()
                    {
                        if let Some(retired) = held.take() {
                            retired_inserts.push(retired);
                        }
                        *held = prepared_inserts[slot].take();
                    }
                    // Offline: build at activation, as the inline reverb
                    // does. Live waits for the producer's install.
                    if allow_inline_reverb
                        && held.as_ref().is_none_or(|held| held.key() != wanted.key)
                        && let Some(built) =
                            insert_provider.and_then(|build| build(wanted.key, sample_rate, slot))
                    {
                        *held = Some(built);
                    }
                    let Some(held) = held.as_mut().filter(|held| held.key() == wanted.key) else {
                        *missing_insert_events += 1;
                        return false;
                    };
                    // The frames from the block start to the note.
                    let frames = e.onset_frame.saturating_sub(start);
                    let frames = frames.min(u64::from(u32::MAX)) as u32;
                    held.restore_params(frames);
                    for param in wanted.params() {
                        held.set_param(*param, frames);
                    }
                    if let Some((beats, tempo)) = wanted.clock() {
                        held.sync(beats, tempo, frames);
                    }
                    if let Some(note) = wanted.note() {
                        held.note(note, frames);
                    }
                    insert_idle_frames[slot] = 0;
                    true
                };
                let insert_orbit = e.controls.insert_orbit();
                // The stages of the chain that hold the effect the note
                // asks for. The note goes through these and no other.
                let mut stages = 0u8;
                for (stage, wanted) in e.controls.effects.iter().enumerate() {
                    if let Some(wanted) = wanted
                        && serve(wanted, crate::insert::effect_slot(insert_orbit, stage))
                    {
                        stages |= 1 << stage;
                    }
                }
                let through_insert = stages != 0;
                if through_insert {
                    effect_stages[insert_orbit] = stages;
                    insert_outputs[insert_orbit] = usize::from(e.controls.orbit).min(MAX_ORBITS - 1);
                }
                if let Some(wanted) = e.controls.instrument
                    && serve(&wanted, crate::insert::instrument_slot(insert_orbit))
                {
                    instrument_to_effect[insert_orbit] = through_insert;
                    insert_outputs[insert_orbit] = usize::from(e.controls.orbit).min(MAX_ORBITS - 1);
                }
                let muted = e.sample.is_some_and(|sample| sample.muted);
                let (delay_wet, orbit) = match e.controls.delay {
                    Some(delay) => {
                        let orbit = (e.controls.orbit as usize).min(MAX_ORBITS - 1);
                        let bus = &mut self.orbit_delays[orbit];
                        // Round, never truncate. The resolved time is f32,
                        // so 0.08 arrives as 3839.9999 frames and 0.15 as
                        // 7200.0003. A truncation leaves the first one
                        // sample short. Chromium is exactly nominal for
                        // both (impulse probe).
                        bus.time_frames = ((f64::from(delay.time_secs.clamp(0.0, 1.0))
                            * f64::from(self.sample_rate))
                        .round() as usize)
                            .clamp(1, self.sample_rate as usize - 1);
                        bus.feedback = delay.feedback.clamp(0.0, 0.98);
                        bus.active = true;
                        (delay.wet.abs(), orbit)
                    }
                    // The voice's orbit routes its output mix (duck stage)
                    // as well as the delay send, so it is always the real
                    // orbit, also when the voice has no delay.
                    None => (0.0, (e.controls.orbit as usize).min(MAX_ORBITS - 1)),
                };
                if muted {
                    // Chain side effects applied (duck armed at intake,
                    // delay params and reverb above); the source itself is
                    // the speed-zero no-op.
                    let _ = (source, duration_secs, amplitude_scale, natural_stop_secs);
                    let _ = (delay_wet, orbit);
                    #[cfg(feature = "device-audio")]
                    if confirmation_sample_matches && let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                        consumer.activation(tag, crate::confirmation::ActivationOutcome::Muted);
                    }
                    return false;
                }
                let source_stereo = match &source {
                    VoiceSource::Wavetable { .. }
                    | VoiceSource::Supersaw { .. }
                    | VoiceSource::ByteBeat { .. }
                    // A bus mixes in stereo, so its taps are stereo even
                    // when everything feeding it was mono.
                    | VoiceSource::Bus { .. }
                    | VoiceSource::Pulse { .. } => true,
                    VoiceSource::Sample { sample, .. } => bank
                        .get(*sample)
                        .is_some_and(|decoded| decoded.channels() == 2),
                    VoiceSource::Oscillator { .. }
                    | VoiceSource::Sbd { .. }
                    | VoiceSource::Input { .. }
                    // A ZzFX note is ONE channel by construction.
                    | VoiceSource::ZzFx { .. }
                    | VoiceSource::Noise { .. } => false,
                };
                let stereo_source = source_stereo
                // A StereoPanner UPMIXES: a mono voice that reaches one comes
                // out stereo, and everything after it is two channels. The
                // mono chain carries a single sample and applies pan only at
                // the very end from `left_gain`/`right_gain`, so a per-stage
                // pan on a mono voice would be computed and then thrown away.
                // Naming pan in any stage therefore makes the voice stereo
                // from the start.
                //
                // A stage REVERB needs it for the same reason: its impulse
                // response is two decorrelated channels, so it turns one
                // channel into two that differ. Left mono, the right half is
                // computed and then dropped by the mono chain, which both
                // silences the decorrelation and leaves the wrong channel's
                // wet signal in place.
                || e.controls.fx_stages.iter().flatten().any(|stage| {
                    stage.pan_x.is_some() || stage.room.is_some()
                });
                let mut voice_controls = e.controls;
                if matches!(source, VoiceSource::Sbd { .. }) {
                    // sbd runs its own amplitude envelope; the standard
                    // ADSR is never wired for it.
                    voice_controls.envelope = crate::backend::Envelope {
                        attack_secs: 0.0,
                        decay_secs: 0.0,
                        sustain: 1.0,
                        release_secs: 0.01,
                    };
                }
                if let Some(djf) = e.controls.djf {
                    // Create-once; each trigger just sets the value.
                    let state = &mut self.orbit_djf[orbit];
                    state.active = true;
                    state.value = djf;
                }
                Self::choke_previous(&mut self.voices, e, start, sample_rate);
                // Stages that wanted a reverb and found none play dry; counted
                // through the same channel as an orbit reverb that was not
                // ready, so one number covers both.
                let mut missed_stage_reverb = 0u64;
                // The first score voice into an orbit keys its routing for good;
                // `channels` on any later voice does nothing. Ordering
                // matters and is the trigger order.
                if !e.controls.piano && self.orbit_channels[orbit].is_none() {
                    self.orbit_channels[orbit] = Some(e.controls.channels.unwrap_or([1, 2]));
                }
                let faded =
                    Self::cull_for_polyphony(&mut self.voices, e.onset_frame, self.sample_rate, self.max_polyphony.0);
                pressure.semantic_polyphony_fades = pressure
                    .semantic_polyphony_fades
                    .saturating_add(u64::try_from(faded).unwrap_or(u64::MAX));
                // Lease before the Voice literal so the FX-stage block can
                // borrow the same pool without overlapping the main-chain pop.
                let stretch = e.controls.stretch.and_then(|_| self.stretch_pool.pop());
                let fx_stage_state = if voice_controls.fx_stages.iter().all(Option::is_none) {
                    None
                } else {
                    let mut stages = self
                        .fx_stage_state_pool
                        .pop()
                        .expect("prepared FX-stage state pool exhausted");
                    let sample_rate = self.sample_rate;
                    let onset_frame = e.onset_frame;
                    let pool = &mut self.compressor_pool;
                    let delay_pool = &mut self.fx_delay_pool;
                    let reverb_pool = &mut self.fx_reverb_pool;
                    let reverb_inline_count = &mut self.fx_reverb_inline_count;
                    let reverb_wanted = &mut self.fx_reverb_wanted;
                    let stretch_pool = &mut self.stretch_pool;
                    let missed = &mut missed_stage_reverb;

                    for (slot, stage) in stages.iter_mut().zip(voice_controls.fx_stages) {
                        *slot = stage.map(|stage| FxStageState {
                            controls: stage,
                            filters: {
                                let mut chain = FilterChain::new(stage.filters, sample_rate);
                                chain.set_block_phase(start);
                                chain
                            },
                            filters_right: stereo_source.then(|| {
                                let mut chain = FilterChain::new(stage.filters, sample_rate);
                                chain.set_block_phase(start);
                                chain
                            }),
                            coarse_hold: [0.0; 2],
                            vowel_coefs: match stage.vowel {
                                Some(vw) => std::array::from_fn(|k| {
                                    bandpass_coefs(vw.freqs[k], vw.qs[k], sample_rate as f32)
                                }),
                                None => [NotchCoefs::default(); 5],
                            },
                            vowel_filters: [[NotchState::default(); 5]; 2],
                            tremolo_phase: None,
                            phaser_phase: None,
                            phaser_filters: [NotchState::default(); 2],
                            // Drawn from the same pool as the main chain's.
                            // An exhausted pool means the stage plays
                            // uncompressed rather than not at all.
                            compressor: stage.compressor.and_then(|c| {
                                pool.pop().map(|mut state| {
                                    *state =
                                        CompressorState::new(sample_rate as f32, &c, onset_frame);
                                    state
                                })
                            }),
                            source_is_mono: !source_stereo,
                            stretch: stage.stretch.and_then(|_| stretch_pool.pop()),
                            transient: stage
                                .transient
                                .map(|c| TransientState::new(c, sample_rate as f32)),
                            // An exhausted pool means the stage plays dry
                            // rather than not at all.
                            delay: stage.delay.and_then(|_| {
                                delay_pool.pop().map(|mut line| {
                                    line.reset();
                                    line
                                })
                            }),
                            room: stage.room.and_then(|room| {
                                let params = crate::reverb::ReverbParams {
                                    ir: room.ir,
                                    size_secs: room.size_secs,
                                    fade_secs: room.fade_secs,
                                    lp_start_hz: room.lp_start_hz,
                                    lp_end_hz: room.lp_end_hz,
                                };
                                match reverb_pool.iter().position(|r| r.params() == params) {
                                    Some(index) => {
                                        let mut reverb = reverb_pool.swap_remove(index);
                                        reverb.reset();
                                        Some(reverb)
                                    }
                                    // Offline: synthesise at activation,
                                    // exactly as the orbit reverbs above do.
                                    None if allow_inline_reverb => {
                                        // Streaming: a stage is fed a sample
                                        // at a time, so its head is convolved
                                        // in the time domain.
                                        let reverb = Box::new(
                                            crate::reverb::OrbitReverb::generate_streaming_with_dispatch(
                                                sample_rate,
                                                crate::reverb::ReverbParams { ir: None, ..params },
                                                dispatch,
                                            ),
                                        );
                                        *reverb_inline_count = reverb_inline_count
                                            .checked_add(1)
                                            .expect("FX reverb inline count overflow");
                                        reserve_fx_reverb_returns(
                                            reverb_pool,
                                            *reverb_inline_count,
                                        );
                                        Some(reverb)
                                    }
                                    None => {
                                        // Live: play dry and ask for one. The
                                        // next hap with these params gets it.
                                        if reverb_wanted.len() < 8
                                            && !reverb_wanted.contains(&params)
                                        {
                                            reverb_wanted.push(params);
                                        }
                                        *missed += 1;
                                        None
                                    }
                                }
                            }),
                        });
                    }
                    Some(stages)
                };
                let fm_state = voice_controls.fm.map(|_| {
                    let mut state = self
                        .fm_state_pool
                        .pop()
                        .expect("prepared FM state pool exhausted");
                    state.restart(e.onset_frame, self.sample_rate);
                    state
                });
                self.next_voice_id += 1;
                let mut voice = Voice {
                    live_gain,
                    live_cutoff,
                    graph_start_frame,
                    start_frame: e.onset_frame,
                    generation: e.generation,
                    replayed_by: None,
                    replayed_on_next_frame: false,
                    ui_visuals: e.ui_visuals,
                    preview_epoch: e.controls.preview_epoch,
                    piano: e.controls.piano,
                    onset_lead: e.onset_lead,
                    distort_shape: e.controls.distort.map(|d| d.shape()).unwrap_or(0.0),
                    delay_wet,
                    reverb_wet,
                    through_insert,
                    insert_orbit,
                    dry_gain: e.controls.dry.unwrap_or(1.0),
                    // Leased from the pool built at init. An exhausted pool
                    // means the voice plays without the vocoder rather than
                    // allocating FFT state under the callback deadline.
                    stretch,
                    orbit,
                    freq_hz: e.freq_hz,
                    duration_secs,
                    hap_duration_secs: e.duration_secs,
                    controls: VoiceRenderControls::from(voice_controls),
                    has_param_modulators: voice_controls.lfos.iter().any(Option::is_some)
                        || voice_controls.envs.iter().any(Option::is_some)
                        || voice_controls.bus_mods.iter().any(Option::is_some),
                    source,
                    amplitude_scale,
                    // The main stage's own gain belongs AFTER the `.FX()`
                    // stages - it lives inside the last FX entry, which is
                    // the hap's own params. Folding it into
                    // `amplitude_scale` up front would hand every stage a
                    // signal 0.8x too quiet, which linear effects forgive
                    // and crush, shape and distort do not.
                    fx_post_gain: e.gain * e.controls.velocity,
                    gate_value: Self::gate_value(duration_secs, voice_controls.envelope),
                    left_gain,
                    right_gain,
                    pan_x: e.controls.pan.map(|pan| 2.0 * pan - 1.0).unwrap_or(0.0),
                    filters_right: stereo_source.then(|| {
                        let mut chain = FilterChain::new(voice_controls.filters, self.sample_rate);
                        chain.set_block_phase(start);
                        chain
                    }),
                    filters,
                    has_filters,
                    has_pre_distort_fx: voice_controls.vowel.is_some()
                        || voice_controls.coarse.is_some()
                        || voice_controls.crush.is_some()
                        || voice_controls.shape.is_some(),
                    stop_secs,
                    mod_lfo_phases: {
                        let mut phases = [0.0f64; crate::backend::MAX_VOICE_MODS];
                        for (slot, phase) in voice_controls.lfos.iter().zip(phases.iter_mut()) {
                            if let Some(lfo) = slot {
                                *phase = f64::from(lfo.phase0);
                            }
                        }
                        phases
                    },
                    sample_pitch_hold: None,
                    phaser_phase: None,
                    phaser_notch: [NotchState::default(); 2],
                    tremolo_phase: None,
                    mod_param_hold: ModParamAdds::default(),
                    transient: e
                        .controls
                        .transient
                        .map(|c| TransientState::new(c, self.sample_rate as f32)),
                    fx_stage_state,
                    vibrato_phase: 0.0,
                    // The noise-mix pink source starts at the note's own
                    // time, so it reads the buffer between samples; the FM
                    // modulators below start frame-aligned and do not.
                    source_noise: NoiseGen::at_onset(self.sample_rate, e.onset_lead),
                    // A modulator feeds ONLY the carrier's frequency param,
                    // and a node feeding a param alone is not pulled until
                    // the owning node evaluates it - which an oscillator
                    // skips entirely while it is still before its start
                    // time. So a modulator first runs in the quantum where
                    // its carrier starts, from phase zero, and two
                    // identical FM notes a second apart are bit-identical
                    // because of it.
                    fm_state,
                    pitch_quantum_skip: {
                        // The onset's offset within its 128-frame quantum:
                        // the pitch envelope runs that many frames late
                        // until the next boundary (see the apply site). A
                        // voice activated after that boundary has nothing
                        // to shift; live admits events already past their
                        // onset, so this is reachable.
                        let boundary = (e.onset_frame / 128 + 1) * 128;
                        if start >= boundary {
                            0
                        } else {
                            (e.onset_frame % 128) as u8
                        }
                    },
                    cut_group: e.cut.or_else(|| e.sample.and_then(|sample| sample.cut)).map(f32::to_bits),
                    id: self.next_voice_id,
                    cut_fade_frame: None,
                    polyphony_fade_frame: None,
                    compressor: if let Some(c) = e.controls.compressor {
                        self.compressor_pool.pop().map(|mut state| {
                            *state =
                                CompressorState::new(self.sample_rate as f32, &c, e.onset_frame);
                            state
                        })
                    } else {
                        None
                    },
                    limiter: if let Some(settings) = e.controls.limit {
                        // In place: the struct is twenty-five kilobytes
                        // of ring, and assigning a fresh one over it would
                        // build that on the callback's stack and copy it,
                        // once per note.
                        self.limiter_pool.pop().map(|mut limiter| {
                            limiter.reconfigure(
                                self.sample_rate,
                                settings.threshold_db,
                                settings.character,
                            );
                            limiter
                        })
                    } else {
                        None
                    },
                    pitch_gate: match voice_controls.pitch_env {
                        Some(pe) => Self::gate_value(
                            duration_secs,
                            crate::backend::Envelope {
                                attack_secs: pe.adsr.attack_secs,
                                decay_secs: pe.adsr.decay_secs,
                                sustain: pe.adsr.sustain as f32,
                                release_secs: pe.adsr.release_secs,
                            },
                        ),
                        None => 0.0,
                    },
                    vowel_filters: [[NotchState::default(); 5]; 2],
                    vowel_coefs: match voice_controls.vowel {
                        Some(vw) => std::array::from_fn(|k| {
                            bandpass_coefs(vw.freqs[k], vw.qs[k], self.sample_rate as f32)
                        }),
                        None => [NotchCoefs::default(); 5],
                    },
                    coarse_hold: [0.0; 2],
                    fx_mod_hold: FxParamHold::default(),
                    source_gone: false,
                };
                // A takeover cut's horizon: a pending onset of the OUTGOING
                // generation that activates here still pre-fades - the cut
                // caught voices already sounding, and this catches one the
                // ghost window is still owed. New-generation onsets fail the
                // generation guard and start untouched.
                if let Some((cut_frame, outgoing)) = self.armed_takeover_cut
                    && voice.generation == outgoing
                    && !voice.piano
                {
                    let stop_at = (cut_frame.saturating_sub(voice.start_frame)) as f32
                        / self.sample_rate as f32
                        + CUT_FADE_SECS;
                    voice.stop_secs = voice.stop_secs.min(stop_at);
                    voice.cut_fade_frame = Some(cut_frame);
                }
                // Change ownership at the new preview's actual onset, after
                // its source has resolved. Notes within one preview remain
                // polyphonic, and untagged score voices keep their tails.
                if voice.preview_epoch > self.preview_epoch {
                    self.preview_epoch = voice.preview_epoch;
                    for old in &mut self.voices {
                        if old.preview_epoch != 0 && old.preview_epoch < voice.preview_epoch {
                            let frame = old.cut_fade_frame
                                .unwrap_or(e.onset_frame).min(e.onset_frame);
                            old.cut_fade_frame = Some(frame);
                            let stop_at = frame.saturating_sub(old.start_frame) as f32
                                / sample_rate as f32 + CUT_FADE_SECS;
                            old.stop_secs = old.stop_secs.min(stop_at);
                        }
                    }
                } else if voice.preview_epoch != 0 && voice.preview_epoch < self.preview_epoch {
                    // A late outgoing event must not revive a retired preview.
                    voice.cut_fade_frame = Some(e.onset_frame);
                    voice.stop_secs = voice.stop_secs.min(CUT_FADE_SECS);
                }
                pressure.activate(&voice);
                self.voices.push(voice);
                *missing_reverb_events += missed_stage_reverb;
                #[cfg(feature = "device-audio")]
                if confirmation_sample_matches && let (Some(consumer), Some(tag)) = (&mut *confirmations, confirmation) {
                    consumer.activation(tag, if *missing_reverb_events > missing_reverbs_before {
                        crate::confirmation::ActivationOutcome::DryFallback
                    } else {
                        crate::confirmation::ActivationOutcome::Activated
                    });
                }
                false
            } else {
                true
            }
        });

        for slot in &mut self.pending_chokes {
            let Some((at, group)) = *slot else {
                continue;
            };
            if at >= end {
                continue;
            }
            if let Some(voice) = self
                .voices
                .iter_mut()
                .filter(|voice| voice.cut_group == Some(group) && voice.start_frame <= at)
                .max_by_key(|voice| voice.id)
            {
                let fade = voice
                    .cut_fade_frame
                    .map_or(at.max(start), |running| running.min(at.max(start)));
                voice.cut_fade_frame = Some(fade);
                let end =
                    fade.saturating_sub(voice.start_frame) as f32 / sample_rate as f32 + 0.011;
                voice.stop_secs = voice.stop_secs.min(end);
            }
            *slot = None;
        }

        // A bus receiver hears the CURRENT quantum's mix, not the previous
        // one (graph topological order). Reproduce that by evaluating every
        // pure sender before any receiver: a voice writes its send after
        // computing its own sample, so once the receivers are last in the
        // vec they read a complete `bus_now`. The sort is stable and voices
        // are found by `id`, never by index.
        //
        // A voice that both sends and receives sorts with the receivers and
        // so sees whatever landed before it - that is a graph cycle, which
        // has no same-quantum answer anyway.
        if self.voices.iter().any(Voice::reads_a_bus) {
            // Stable partition by rotation rather than `sort_by_key`, which
            // allocates a merge buffer above ~20 elements - not allowed on the
            // audio callback. Already-ordered voices cost one no-op rotate
            // each, which is the steady state.
            let mut senders = 0;
            for at in 0..self.voices.len() {
                if !self.voices[at].reads_a_bus() {
                    self.voices[senders..=at].rotate_right(1);
                    senders += 1;
                }
            }
        }

        // Orbit modulation is uncommon, but its sums used to be cleared for
        // every frame and updated with three zeroes for every ordinary voice.
        // The arrays start at zero for each block, so a block with no voice
        // capable of producing ModAdds can leave them untouched.
        let any_param_modulators = self.voices.iter().any(|voice| voice.has_param_modulators);

        let mut orbit_inputs = [[0.0f32; 2]; MAX_ORBITS];
        let zero_mod_adds = ModAdds::default();
        // `djf` belongs to the ORBIT but is modulated from a VOICE, so the
        // voices' contributions are summed here and handed to the orbit's
        // filter. DJFProcessor reads `parameters.value[0]`, so the sum is
        // latched once per 128-frame quantum.
        let mut djf_mods = [0.0f32; MAX_ORBITS];
        // DelayNode.delayTime is a-rate rather than latched, so its sum is
        // rebuilt every frame instead of every quantum.
        let mut delay_time_mods = [0.0f32; MAX_ORBITS];
        // The GainNode on the delay's feedback edge is shared in the same
        // way, and its `gain` AudioParam is a-rate too.
        let mut delay_feedback_mods = [0.0f32; MAX_ORBITS];
        // Per-orbit block pipeline: voices, delay and reverb returns
        // accumulate into ≤128-frame per-orbit buffers (the reverb is a
        // block convolution on WebAudio's 128-frame quantum), then the duck
        // gain scales each orbit's whole signal into the output - exactly
        // where the per-orbit output gain sits.
        let blocks = &mut self.orbit_blocks;
        let visual_capture_mask = self.ui_visual_capture_mask;
        let visual_capture_generation_floor = self.ui_visual_capture_generation_floor;
        let visual_blocks = &mut self.ui_visual_mix;
        let bus_now = &mut self.bus_now;
        // Input voices read the ring behind its writer; the cursor is placed
        // once per block and runs with the frames - at the ratio of the
        // ring's rate to the output's, when the input could not be opened at
        // the output's rate.
        let input = self.input.clone();
        let input_base = input.as_ref().map(|ring| {
            let ring_rate = ring.sample_rate();
            let ratio = if ring_rate == 0 || self.sample_rate == 0 {
                1.0
            } else {
                f64::from(ring_rate) / f64::from(self.sample_rate)
            };
            // What this block will read, in the ring's own frames: the
            // ring has no idea how big an output callback is, and that is
            // the whole of whether a cursor is safe to keep.
            let wanted = (frames as f64 * ratio).ceil() as u64;
            let placed = ring.reader_frame(
                self.input_cursor.map(|cursor| cursor.floor() as u64),
                wanted,
            );
            // The fraction survives while the cursor was not moved.
            let base = match self.input_cursor {
                Some(cursor) if cursor.floor() as u64 == placed => cursor,
                _ => placed as f64,
            };
            self.input_cursor = Some(base + frames as f64 * ratio);
            (base, ratio)
        });
        let sample_rate = self.sample_rate;
        let sample_resampling_mode = self.sample_resampling_mode;
        let djfs = &mut self.orbit_djf;
        let sends = &mut self.reverb_sends;
        let mut sub_start = 0usize;
        while sub_start < frames {
            let sub_len = (frames - sub_start).min(crate::reverb::REVERB_BLOCK);
            // The keyboard shares the output and master processing, but is
            // not a score orbit. Score faders, ducking, DJF and routing must
            // neither silence it nor redirect it to another output pair.
            let mut piano_mix = [[0.0f32; crate::reverb::REVERB_BLOCK]; 2];
            for buffer in blocks.iter_mut() {
                buffer[0][..sub_len].fill(0.0);
                buffer[1][..sub_len].fill(0.0);
            }
            let mut any_reverb_send = false;
            for buffer in sends.iter_mut() {
                buffer[0][..sub_len].fill(0.0);
                buffer[1][..sub_len].fill(0.0);
            }
            for (buffer, stages) in insert_blocks.iter_mut().zip(effect_stages.iter()) {
                if *stages != 0 {
                    buffer[0][..sub_len].fill(0.0);
                    buffer[1][..sub_len].fill(0.0);
                }
            }
            // Arm every duck whose 10 ms hold point this sub-block reaches,
            // in arm order and, within one arm frame, in admission order:
            // the last trigger armed owns the orbit's dip.
            let arm_through = start + (sub_start + sub_len) as u64;
            loop {
                let mut next: Option<usize> = None;
                for (index, pending) in pending_ducks.iter().enumerate() {
                    if pending.arm_frame < arm_through
                        && next.is_none_or(|best| pending.arm_frame < pending_ducks[best].arm_frame)
                    {
                        next = Some(index);
                    }
                }
                let Some(index) = next else { break };
                // An order-preserving remove keeps the queue in admission
                // order; it shifts within capacity and never allocates.
                let pending = pending_ducks.remove(index);
                for target in pending.controls.targets.iter().flatten() {
                    let orbit = usize::from(target.orbit);
                    if orbit < MAX_ORBITS {
                        ducks[orbit].trigger(
                            pending.onset_frame,
                            start + sub_start as u64,
                            sample_rate,
                            *target,
                        );
                    }
                }
            }
            for i in 0..sub_len {
                let abs = start + (sub_start + i) as u64;
                orbit_inputs.fill([0.0; 2]);
                if any_param_modulators {
                    if abs & 127 == 0 {
                        djf_mods = [0.0; MAX_ORBITS];
                    }
                    delay_time_mods.fill(0.0);
                    delay_feedback_mods.fill(0.0);
                }
                for v in &mut self.voices {
                    if abs < v.graph_start_frame {
                        continue;
                    }
                    let sbd_pre_onset =
                        abs < v.start_frame && matches!(v.source, VoiceSource::Sbd { .. });
                    if abs < v.start_frame && !sbd_pre_onset {
                        continue;
                    }
                    if !sbd_pre_onset && v.fm_state.is_some() && abs & 127 == 0 {
                        let state = v.fm_state.as_deref_mut().expect("FM state must be present");
                        if state.quantum_skip > 0 {
                            let fm = v.controls.fm.expect("FM state requires FM controls");
                            fm_advance(
                                fm,
                                v.freq_hz,
                                &mut state.phases,
                                &mut state.noise,
                                u32::from(state.quantum_skip),
                                tables,
                                sample_rate,
                            );
                            state.quantum_skip = 0;
                        }
                    }
                    if !sbd_pre_onset && v.pitch_quantum_skip > 0 && abs & 127 == 0 {
                        v.pitch_quantum_skip = 0;
                    }
                    // The oscillator, noise source, and their automation do
                    // not advance before `start_frame`. Only the connected
                    // WaveShaper processes its zero input. Downstream nodes do
                    // process that value, so use onset-time controls while the
                    // source clock remains parked.
                    let t = if sbd_pre_onset {
                        0.0
                    } else {
                        ((abs - v.start_frame) as f32 + v.onset_lead) / sr as f32
                    };
                    if t >= v.stop_secs {
                        continue;
                    }
                    let env = Self::envelope(t, v.duration_secs, v.controls.envelope, v.gate_value);
                    // lfo()/env() modulators: each rides its param
                    // additively. LFO phases free-run from the
                    // onset-aligned phase0.
                    // Modulators only run on 128-frame context quanta
                    // starting strictly after the onset (the begin gate), so
                    // each note opens unmodulated for up to one quantum, and
                    // the envelope clock reads quantum starts.
                    let quantum_start = abs & !127;
                    // Compare against the fractional onset, not its rounded
                    // frame. An onset at frame 127.6 rounds to 128 but must
                    // still open the transient gate at quantum 128.
                    let transient_open =
                        (quantum_start as f64) > v.start_frame as f64 - f64::from(v.onset_lead);
                    // Filter LFOs stop contributing at the hap's hold end.
                    // Other modulators can run until release ends. Neither end
                    // is based on the sample slice's potentially longer duration.
                    let filter_lfo_alive =
                        lfo_quantum_alive(quantum_start, sr, v.controls.filter_lfo_end_secs);
                    // LFOs and the pulse, wavetable and supersaw oscillators
                    // share the f32-rounded begin gate. The transient shaper
                    // uses the fractional-frame comparison above instead.
                    let source_open =
                        lfo_quantum_open(quantum_start, sr, v.controls.worklet_begin_secs);
                    let lfo_live = source_open
                        && lfo_quantum_alive(quantum_start, sr, v.controls.lfo_end_secs);
                    let mut mod_adds = if v.has_param_modulators && lfo_live {
                        Some(ModAdds::default())
                    } else {
                        None
                    };
                    let note_alive = note_end_alive(
                        quantum_start,
                        v.start_frame,
                        v.onset_lead,
                        v.hap_duration_secs,
                        v.controls.modulator_release_secs,
                        sr,
                    );
                    // Parameter and bus modulators stop at hold plus release;
                    // their target parameters then keep their base values.
                    if let Some(mod_adds) = mod_adds.as_mut() {
                        // The f32-rounded gate can open in the quantum that
                        // contains the onset, before `start_frame`. Keep the
                        // subtraction signed and clamp envelope time to zero.
                        let t_env = (((quantum_start as f64 - v.start_frame as f64) as f32
                            + v.onset_lead)
                            / sr as f32)
                            .max(0.0);
                        // Two passes. A modulator aimed at a FILTER's own LFO
                        // has to be summed before that LFO is evaluated -
                        // WebAudio gets the ordering for free by processing a
                        // node that feeds an AudioParam ahead of the node
                        // owning it, and here it has to be arranged. Each
                        // modulator appears in exactly one pass, so phases
                        // still advance once per sample.
                        // A bus modulator's signal is another pattern's audio.
                        // `connectBusModulator` sums the bus into a
                        // `ConstantSourceNode` offset, scales by depth/0.3,
                        // clamps to the param's range, and adds to the param.
                        // A stereo node feeding an AudioParam is down-mixed,
                        // so the two channels average.
                        for slot in v.controls.bus_mods.iter().flatten() {
                            let bus = bus_now[usize::from(slot.bus)];
                            let signal = (bus[0] + bus[1]) * 0.5 + slot.dc;
                            let value = (signal * slot.depth).clamp(slot.min, slot.max);
                            mod_adds.bucket_at(slot.fxi, slot.target, value, slot.param_base);
                        }
                        for first_pass in [true, false] {
                            for (slot, phase) in
                                v.controls.lfos.iter().zip(v.mod_lfo_phases.iter_mut())
                            {
                                let Some(lfo) = slot else { continue };
                                if ModAdds::targets_another_modulator(lfo.target) != first_pass {
                                    continue;
                                }
                                // Dead past the hold end; see `filter_lfo_alive`.
                                if lfo.filter.is_some() && !filter_lfo_alive {
                                    continue;
                                }
                                // Every one of these is block-rate, so
                                // modulation of them is latched per
                                // quantum.
                                let mut depth = lfo.depth;
                                let mut dcoffset = lfo.dcoffset;
                                let mut skew = lfo.skew;
                                let mut curve = lfo.curve;
                                let mut rate = lfo.frequency_hz;
                                if let Some(kind) = lfo.filter {
                                    let hold = v.mod_param_hold.filter[kind.index()];
                                    depth += hold.depth;
                                    dcoffset += hold.dc;
                                    skew += hold.skew;
                                }
                                if let Some(hold) = lfo
                                    .id
                                    .and_then(|id| v.mod_param_hold.lfo.get(usize::from(id)))
                                {
                                    depth += hold.depth;
                                    dcoffset += hold.dcoffset;
                                    skew += hold.skew;
                                    curve += hold.curve;
                                    rate += hold.rate;
                                }
                                let wave = lfo_waveshape_f64(lfo.shape, *phase, skew);
                                let raw = (wave as f32 + dcoffset) * depth;
                                let shaped = raw.powf(curve);
                                // The reference computes the sample in f64.
                                // A small negative value rounds to zero in
                                // f32 and hides the NaN.
                                let nan = shaped.is_nan()
                                    || (raw == 0.0 && {
                                        let exact = (wave + f64::from(dcoffset)) * f64::from(depth);
                                        exact < 0.0 && exact.powf(f64::from(curve)).is_nan()
                                    });
                                // A NaN sample makes the summed param NaN.
                                // The param then takes its default value.
                                let value = if nan {
                                    nan_param_default(lfo.target, &v.source) - lfo.param_base
                                } else {
                                    js_clamp(shaped, lfo.min, lfo.max)
                                };
                                mod_adds.bucket_at(lfo.fxi, lfo.target, value, lfo.param_base);
                                *phase += f64::from(rate) / sr;
                                if *phase > 1.0 {
                                    *phase -= 1.0;
                                }
                            }
                            for env_mod in v.controls.envs.iter().flatten() {
                                if ModAdds::targets_another_modulator(env_mod.target) != first_pass
                                {
                                    continue;
                                }
                                // begin==0 never satisfies the envelope's
                                // begin-changed trigger check, so a note
                                // starting at context time zero stays idle.
                                // The browser's envelope processor writes
                                // nothing for a block whose start is at or
                                // before the trigger, and triggers on the
                                // first block after it.
                                let value = if v.start_frame == 0 || t_env <= 0.0 {
                                    0.0
                                } else {
                                    let hold = env_mod
                                        .id
                                        .and_then(|id| v.mod_param_hold.env.get(usize::from(id)))
                                        .copied()
                                        .unwrap_or_default();
                                    // The envelope clamps sustain to
                                    // 0..1 and keeps every time non-negative,
                                    // so a modulator cannot drive them out of
                                    // range however deep it swings.
                                    let shaped = crate::backend::EnvMod {
                                        attack_secs: (env_mod.attack_secs + hold.attack).max(0.0),
                                        decay_secs: (env_mod.decay_secs + hold.decay).max(0.0),
                                        sustain: (env_mod.sustain + hold.sustain).clamp(0.0, 1.0),
                                        release_secs: (env_mod.release_secs + hold.release)
                                            .max(0.0),
                                        ..*env_mod
                                    };
                                    env_mod_value(&shaped, t_env) * (env_mod.depth + hold.depth)
                                };
                                mod_adds.bucket_at(
                                    env_mod.fxi,
                                    env_mod.target,
                                    js_clamp(value, env_mod.min, env_mod.max),
                                    env_mod.param_base,
                                );
                            }
                            if first_pass && abs & 127 == 0 {
                                v.mod_param_hold = mod_adds.mod_params;
                            }
                        }
                    }
                    // Latch the block-rate params once per render quantum,
                    // then hold for the block.
                    if abs & 127 == 0 {
                        v.fx_mod_hold = mod_adds
                            .as_ref()
                            .map_or_else(FxParamHold::default, ModAdds::fx_hold);
                    }
                    let mod_adds = mod_adds.as_ref().unwrap_or(&zero_mod_adds);
                    let fm_offset_hz = match v.controls.fm {
                        None => 0.0f32,
                        Some(fm) => {
                            let state =
                                v.fm_state.as_deref_mut().expect("FM state must be present");
                            Self::render_fm_offset(
                                fm,
                                v.freq_hz,
                                t,
                                v.duration_secs,
                                state.quantum_skip,
                                &mut state.phases,
                                &mut state.noise,
                                mod_adds,
                                tables,
                                sr,
                            )
                        }
                    };
                    let fm_offset_hz = fm_offset_hz + mod_adds.frequency_hz;
                    if v.live_cutoff.binding != 0 {
                        let cutoff = v.live_cutoff.next();
                        v.filters.set_lowpass_frequency(cutoff);
                        if let Some(right) = &mut v.filters_right {
                            right.set_lowpass_frequency(cutoff);
                        }
                    }
                    let filter_adds = [
                        mod_adds.lowpass_hz,
                        mod_adds.highpass_hz,
                        mod_adds.bandpass_hz,
                    ];
                    let filter_q_adds = [mod_adds.lowpass_q, mod_adds.highpass_q, mod_adds.band_q];
                    let live_gain = if v.live_gain.binding != 0 {
                        v.live_gain.next()
                    } else {
                        1.0
                    };
                    let mut env = env * mod_adds.gain_factor();
                    if let Some(fade) = v.cut_fade_frame
                        && abs >= fade
                    {
                        // setValueAtTime(1, t) + linearRampToValueAtTime(0, t+0.01)
                        env *= (1.0 - (abs - fade) as f32 / (0.01 * sr as f32)).max(0.0);
                    }
                    if let Some(fade) = v.polyphony_fade_frame
                        && abs >= fade
                    {
                        // A linear ramp to 0 over 0.25 s on the voice's own
                        // gain, paired with the stop at t + 0.25.
                        env *= (1.0 - (abs - fade) as f32 / (POLYPHONY_FADE_SECS * sr as f32))
                            .max(0.0);
                    }
                    // The `post` gain sits AFTER the whole chain and is
                    // tapped for every send. Folding it into the source
                    // instead would move the level a non-linear stage sees:
                    // `.distort(2).postgain(.5)` would drive the waveshaper
                    // at half the amplitude and come out 1.7 dB loud, with
                    // a different shape, rather than simply half as loud.
                    let post_gain = v.controls.postgain * mod_adds.postgain_factor();
                    // Pan modulators ride the StereoPanner's pan param
                    // (base 2·pan−1, clamped to the param's [-1, 1] range).
                    let pan_param = if mod_adds.pan != 0.0 {
                        (v.pan_x + mod_adds.pan).clamp(-1.0, 1.0)
                    } else {
                        v.pan_x
                    };
                    // vib/penv ride the source's DETUNE param (cents), which
                    // multiplies the computed frequency by 2^(cents/1200) after
                    // the additive frequency-param mods. sbd runs its own pitch
                    // envelope, so the general detune path skips it.
                    let pitch_ratio = {
                        let mut cents = 0.0f32;
                        if let Some(vb) = v.controls.vibrato {
                            // Phase INTEGRATES the rate. sin(2*pi*f*t) agrees
                            // only while f is constant; once a modulator moves
                            // it, treating the rate as a position rather than
                            // a speed bends the whole waveform.
                            let rate = vb.freq_hz + mod_adds.vib_rate;
                            let cents_depth = vb.cents + mod_adds.vib_depth;
                            cents += (std::f64::consts::TAU * v.vibrato_phase).sin() as f32
                                * cents_depth;
                            v.vibrato_phase += f64::from(rate) / sr;
                            if v.vibrato_phase > 1.0 {
                                v.vibrato_phase -= 1.0;
                            }
                        }
                        // In a source's first render quantum the browser reads
                        // its per-frame detune automation from the quantum
                        // start while writing output from the onset, so the
                        // envelope arrives `offset` frames late there and
                        // frames before the automation event read zero.
                        let t_pitch = t - f32::from(v.pitch_quantum_skip) / sr as f32;
                        if let Some(pe) = v.controls.pitch_env.filter(|_| t_pitch >= 0.0) {
                            cents += if pe.exponential {
                                crate::biquad::FilterStage::frequency_at(
                                    pe.adsr,
                                    t_pitch,
                                    v.duration_secs,
                                )
                            } else {
                                let shape = Self::envelope(
                                    t_pitch,
                                    v.duration_secs,
                                    crate::backend::Envelope {
                                        attack_secs: pe.adsr.attack_secs,
                                        decay_secs: pe.adsr.decay_secs,
                                        sustain: pe.adsr.sustain as f32,
                                        release_secs: pe.adsr.release_secs,
                                    },
                                    v.pitch_gate,
                                );
                                (pe.adsr.min_hz
                                    + f64::from(shape) * (pe.adsr.max_hz - pe.adsr.min_hz))
                                    as f32
                            };
                        }
                        if cents == 0.0 || matches!(v.source, VoiceSource::Sbd { .. }) {
                            1.0
                        } else {
                            (cents / 1200.0).exp2()
                        }
                    };
                    // What a SAMPLE plays at this block. `detune` on a buffer
                    // source is k-rate: one value per 128-frame quantum, read
                    // from where the automation ENDS in it, held for the whole
                    // block. Sampling it per frame glides where the browser
                    // steps, which leaves the level right and the waveform
                    // wrong. An oscillator's detune is a-rate and uses
                    // `pitch_ratio` above unchanged.
                    let sample_pitch = if matches!(v.source, VoiceSource::Sample { .. }) {
                        if abs & 127 == 0 || v.sample_pitch_hold.is_none() {
                            // A k-rate param is sampled at the FIRST frame of
                            // the block and held for all of it. Measured both
                            // ways against Chromium: reading the block's end
                            // instead scores 0.18 where this scores 1.000000.
                            let ahead_frames = -((abs & 127) as i32);
                            let block_start = t + ahead_frames as f32 / sr as f32;
                            // A block that began BEFORE this voice did reads
                            // the param where no automation has run yet, which
                            // for detune is zero. The browser plays that whole
                            // block at the undetuned rate, however late in it
                            // the source starts.
                            let mut cents = 0.0f32;
                            if let Some(pe) = v.controls.pitch_env.filter(|_| block_start >= 0.0) {
                                let shifted = block_start;
                                if shifted >= 0.0 {
                                    cents += if pe.exponential {
                                        crate::biquad::FilterStage::frequency_at(
                                            pe.adsr,
                                            shifted,
                                            v.duration_secs,
                                        )
                                    } else {
                                        let shape = Self::envelope(
                                            shifted,
                                            v.duration_secs,
                                            crate::backend::Envelope {
                                                attack_secs: pe.adsr.attack_secs,
                                                decay_secs: pe.adsr.decay_secs,
                                                sustain: pe.adsr.sustain as f32,
                                                release_secs: pe.adsr.release_secs,
                                            },
                                            v.pitch_gate,
                                        );
                                        (pe.adsr.min_hz
                                            + f64::from(shape) * (pe.adsr.max_hz - pe.adsr.min_hz))
                                            as f32
                                    };
                                }
                            }
                            // A modulator aimed at the source's pitch lands
                            // on `detune` here, in cents, because a buffer
                            // source has no frequency param of its own. It is
                            // the same k-rate param the envelope and the
                            // vibrato ride, so it is read at the block start
                            // with them.
                            cents += mod_adds.frequency_hz;
                            if let Some(vb) = v.controls.vibrato.filter(|_| block_start >= 0.0) {
                                // The vibrato oscillator runs on its own clock,
                                // so its value at the block end is read from the
                                // phase this block will reach.
                                let ahead = f64::from(ahead_frames) * f64::from(vb.freq_hz) / sr;
                                cents += (std::f64::consts::TAU * (v.vibrato_phase + ahead)).sin()
                                    as f32
                                    * vb.cents;
                            }
                            v.sample_pitch_hold = Some(if cents == 0.0 {
                                1.0
                            } else {
                                (cents / 1200.0).exp2()
                            });
                        }
                        v.sample_pitch_hold.unwrap_or(1.0)
                    } else {
                        1.0
                    };
                    // Tremolo: gain = max(1−depth, 0) + the LFO's
                    // clamp(pow(shape·depth, 1.5), 0, 1); the LFO obeys the
                    // same onset-quantum gate, so a full-depth tremolo opens
                    // silent for up to one quantum by design.
                    let trem_gain = match v.controls.tremolo {
                        None => 1.0f32,
                        Some(tr) => {
                            // The chain is a gain node the LFO ADDS to:
                            // gain = max(1-depth, 0), lfo -> gain.gain.
                            // `tremolodepth` names that
                            // node's gain, so a modulator moves the floor and
                            // leaves the LFO's own depth where it was.
                            let base = tr.gain_floor() + mod_adds.trem_depth;
                            if lfo_live {
                                let hold = &v.fx_mod_hold;
                                let skew = tr.skew + hold.trem_skew;
                                let shape = if hold.trem_shape == 0.0 {
                                    tr.shape
                                } else {
                                    ((f32::from(tr.shape) + hold.trem_shape) as i32).rem_euclid(5)
                                        as u8
                                };
                                // The seed reads the frequency param, so a
                                // modulated rate shifts where the LFO starts.
                                let freq = tr.frequency_hz + hold.trem_rate;
                                let phase = *v.tremolo_phase.get_or_insert_with(|| {
                                    lfo_phase0(tr.time_secs, freq, tr.phase_offset)
                                });
                                let raw = lfo_waveshape(shape, phase, skew) * tr.depth;
                                let shaped = raw.powf(1.5).clamp(0.0, 1.0);
                                let next = phase + f64::from(freq) / sr;
                                v.tremolo_phase = Some(if next > 1.0 { next - 1.0 } else { next });
                                // A NaN sum gives the gain its default.
                                let gain = base + shaped;
                                if gain.is_nan() { 1.0 } else { gain }
                            } else {
                                base
                            }
                        }
                    };
                    // Phaser: notch at (center+282)·2^(detune/1200), detune from
                    // a default tri LFO clamped to ±sweep cents. Before the
                    // gate opens and after it closes the LFO is silent, so the
                    // notch sits at its static center.
                    let phaser_coefs = match v.controls.phaser {
                        None => None,
                        Some(ph) => {
                            // The phaser's LFO is built with depth =
                            // sweep*2, and phasersweep names that depth
                            // param, so the modulator adds to the doubled
                            // value rather than to the sweep it was derived
                            // from.
                            let lfo_depth =
                                (2.0 * ph.sweep_cents + v.fx_mod_hold.phaser_sweep).max(0.0);
                            let sweep = lfo_depth * 0.5;
                            let rate = ph.rate_hz + v.fx_mod_hold.phaser_rate;
                            let detune = if lfo_live {
                                // getPhaser passes no phaseoffset, and its
                                // `time` is the note's begin.
                                let phase = *v
                                    .phaser_phase
                                    .get_or_insert_with(|| lfo_phase0(ph.time_secs, rate, 0.0));
                                let raw = (lfo_waveshape(0, phase, 0.5) - 0.5) * lfo_depth;
                                let next = phase + f64::from(rate) / sr;
                                v.phaser_phase = Some(if next > 1.0 { next - 1.0 } else { next });
                                js_clamp(raw, -sweep, sweep)
                            } else {
                                0.0
                            };
                            let center = ph.center_hz + mod_adds.phaser_center;
                            let frequency = (center + 282.0) * (detune / 1200.0).exp2();
                            // phaserdepth names the notch's Q, not the depth
                            // control: the modulator lands after the
                            // depth-to-Q conversion, not before it.
                            let q = 2.0 - (ph.depth * 2.0).clamp(0.0, 1.9) + mod_adds.phaser_depth;
                            Some(notch_coefs(frequency, q, sr as f32))
                        }
                    };
                    // All five vowel filters are one `vowel` target, so the
                    // SAME a-rate signal lands on every frequency param.
                    // Keep the precomputed static coefficients when there
                    // is no signal to add.
                    let vowel_coefs = match (v.controls.vowel, mod_adds.vowel_hz) {
                        (Some(vw), add) if add != 0.0 => std::array::from_fn(|k| {
                            bandpass_coefs(vw.freqs[k] + add, vw.qs[k], sr as f32)
                        }),
                        _ => v.vowel_coefs,
                    };
                    let (wave_l, wave_r) = match &mut v.source {
                        VoiceSource::Oscillator {
                            table_selection,
                            phase,
                        } => {
                            let wave = oscillator_sample(
                                tables,
                                &v.controls,
                                *table_selection,
                                phase,
                                v.freq_hz,
                                fm_offset_hz,
                                pitch_ratio,
                                sr,
                            );
                            // `noise` crossfades PINK noise with the
                            // oscillator before the envelope. `wetfade`
                            // holds at one below a half and falls linearly
                            // above it, and the dry leg reads the amount
                            // while the wet leg reads its complement - so a
                            // half leaves both legs open.
                            let wave = if v.controls.noise > 0.0 {
                                let wetfade =
                                    |d: f32| if d < 0.5 { 1.0 } else { 1.0 - (d - 0.5) / 0.5 };
                                let pink = v.source_noise.next(1, 0.0);
                                wave * wetfade(v.controls.noise)
                                    + pink * wetfade(1.0 - v.controls.noise)
                            } else {
                                wave
                            };
                            (wave, wave)
                        }
                        // The supersaw has the same begin gate as the pulse
                        // oscillator: no output and no phase advance until
                        // the gate opens.
                        VoiceSource::Supersaw { .. } if !source_open => (0.0, 0.0),
                        VoiceSource::Supersaw { plan, phases } => {
                            // `detune` and `spread` are params the supersaw
                            // shares with the wavetable, so a modulator on
                            // either moves both.
                            let (mut gain_l, mut gain_r) = plan.pan_gains(mod_adds.panspread);
                            let freq = (v.freq_hz + fm_offset_hz) * pitch_ratio;
                            let static_spread = mod_adds.freqspread.to_bits() == 0;
                            if static_spread
                                && let Some(kernel) = supersaw_kernel
                                && kernel.supports(plan.count)
                            {
                                // SAFETY: the kernel was selected only after
                                // its CPU prerequisites were detected, and
                                // both arrays have MAX_UNISON entries while
                                // count is clamped to that bound.
                                unsafe {
                                    kernel.render(
                                        phases,
                                        &plan.detune_ratios,
                                        plan.count,
                                        freq,
                                        sr as f32,
                                        (gain_l, gain_r),
                                    )
                                }
                            } else {
                                let (scale, center) = if static_spread {
                                    (0.0, 0.0)
                                } else {
                                    supersaw_spread_geometry(
                                        plan.voices,
                                        (plan.freqspread + mod_adds.freqspread).max(0.0),
                                    )
                                };
                                let mut left = 0.0f32;
                                let mut right = 0.0f32;
                                // `count` is clamped to MAX_UNISON when the
                                // plan is built, so the take never shortens
                                // the loop.
                                for (n, phase) in phases.iter_mut().enumerate().take(plan.count) {
                                    let ratio = if static_spread {
                                        plan.detune_ratios[n]
                                    } else {
                                        supersaw_detune_ratio(n, scale, center)
                                    };
                                    let voice_freq = freq * ratio;
                                    // Euclidean remainder, not `fract` - dt
                                    // must stay non-negative.
                                    let dt = (voice_freq / sr as f32).rem_euclid(1.0);
                                    let value = saw_blep(*phase, dt);
                                    left += value * gain_l;
                                    right += value * gain_r;
                                    let mut next = *phase + dt;
                                    if next >= 1.0 {
                                        next -= 1.0;
                                    }
                                    *phase = next;
                                    std::mem::swap(&mut gain_l, &mut gain_r);
                                }
                                (left, right)
                            }
                        }
                        VoiceSource::Pulse {
                            pulsewidth,
                            width_lfo,
                            width_lfo_phase,
                            phi,
                            y0,
                            y1,
                            dphif,
                            envf,
                            env: pulse_env,
                        } => {
                            use std::f64::consts::PI;
                            // The begin gate: the source emits nothing, and
                            // advances nothing, until the first context
                            // quantum past the note's begin - the begin the
                            // AudioParam holds, narrowed to f32, which is not
                            // always the quantum after the onset FRAME.
                            if !source_open {
                                (0.0, 0.0)
                            } else {
                                // The per-block decay resets each 128
                                // frames of the CONTEXT grid.
                                if abs.is_multiple_of(128) {
                                    *pulse_env = 1.0;
                                }
                                // The pulse synth's own LFO is a-rate at
                                // its output, but its frequency and depth
                                // params are read once per quantum. Its
                                // clamp bounds were baked from the original
                                // sweep, not from a later modulated depth.
                                let width_lfo_add = match width_lfo {
                                    None => 0.0,
                                    Some(lfo) => {
                                        let hold = v.mod_param_hold.pulse_width;
                                        let rate = lfo.frequency_hz + hold.rate;
                                        let depth = lfo.depth + hold.depth;
                                        let phase = *width_lfo_phase.get_or_insert_with(|| {
                                            lfo_phase0(lfo.time_secs, rate, 0.0)
                                        });
                                        let raw = (lfo_waveshape(0, phase, 0.5) - 0.5) * depth;
                                        let add = js_clamp(raw, -0.5 * lfo.depth, 0.5 * lfo.depth);
                                        let next = phase + f64::from(rate) / sr;
                                        *width_lfo_phase =
                                            Some(if next > 1.0 { next - 1.0 } else { next });
                                        add
                                    }
                                };
                                let width = *pulsewidth + mod_adds.pulse_width + width_lfo_add;
                                let pw = (1.0 - f64::from(width.clamp(-0.99, 0.99))) * PI;
                                let freq = f64::from((v.freq_hz + fm_offset_hz) * pitch_ratio);
                                let dphi = freq * (2.0 * PI) / sr;
                                *dphif += 0.1 * (dphi - *dphif);
                                *pulse_env *= 0.9998;
                                *envf += 0.1 * (*pulse_env - *envf);
                                let mut b = 2.3 * (1.0 - 0.0001 * freq);
                                if b < 0.0 {
                                    b = 0.0;
                                }
                                *phi += *dphif;
                                if *phi >= PI {
                                    *phi -= 2.0 * PI;
                                }
                                let out0 = (*phi + b * *y0).cos();
                                *y0 = 0.5 * (out0 + *y0);
                                let out1 = (*phi + b * *y1 + pw).cos();
                                *y1 = 0.5 * (out1 + *y1);
                                let value = (0.15 * (out0 - out1) * *envf) as f32;
                                (value, value)
                            }
                        }
                        VoiceSource::ByteBeat {
                            expression,
                            program,
                            t,
                            start_offset,
                            gate,
                        } => {
                            if abs < *gate {
                                // Gated: the source has not run yet, so it
                                // emits nothing and advances nothing.
                                (0.0, 0.0)
                            } else {
                                // local_t scales the integer counter by
                                // 256/sampleRate and the voice's frequency,
                                // the expression's low byte becomes a
                                // bipolar signal, and the output is clamped
                                // before anything downstream sees it.
                                let scale = 256.0 / sr;
                                let local_t = scale * f64::from(v.freq_hz + fm_offset_hz) * *t
                                    + *start_offset;
                                // A compiled `byteBeatExpression` replaces the
                                // built-in table; `n` still picks a built-in
                                // when the score gave no expression of its own.
                                let value = match program {
                                    Some(program) => program.eval(local_t),
                                    None => crate::backend::bytebeat_sample(*expression, local_t),
                                };
                                let byte = f64::from(crate::backend::bytebeat_byte(value));
                                let signal = (byte / 127.5 - 1.0) * 0.2;
                                let out = signal.clamp(-0.4, 0.4) as f32;
                                *t += 1.0;
                                (out, out)
                            }
                        }
                        VoiceSource::Bus { bus } => {
                            let tap = bus_now[usize::from(*bus)];
                            (tap[0], tap[1])
                        }
                        VoiceSource::ZzFx { voice, ring } => {
                            let sample = voice
                                .step(ring.as_deref_mut().unwrap_or(&mut []))
                                .unwrap_or(0.0);
                            (sample, sample)
                        }
                        VoiceSource::Input { channel } => {
                            let sample = match (&input, input_base) {
                                (Some(ring), Some((base, ratio))) => ring.sample_at(
                                    base + (sub_start + i) as f64 * ratio,
                                    usize::from(*channel),
                                ),
                                _ => 0.0,
                            };
                            (sample, sample)
                        }
                        VoiceSource::Noise {
                            kind,
                            density,
                            state,
                        } => {
                            // One stream feeding both channels: the source is
                            // mono until the panner, like every other synth.
                            let sample = state.next(*kind, *density);
                            (sample, sample)
                        }
                        VoiceSource::Sbd {
                            controls: sbd,
                            phase,
                            noise,
                        } => {
                            if sbd_pre_onset {
                                // The scheduled oscillator contributes zero
                                // before source start, while the connected
                                // WaveShaper remains active. Its sample-rate-
                                // length curve is one element short of symmetry,
                                // so interpolating at zero yields the small
                                // negative DC value required by the downstream
                                // signal path.
                                let value = sbd_saturation(0.0, sbd_saturation_curve);
                                (value, value)
                            } else {
                                // Pitch env: detune from penv semitones (penv×100
                                // cents) exponentially to 0.001 cents over pdecay,
                                // then held.
                                let start_cents = f64::from(sbd.penv_semitones) * 100.0;
                                let cents = if start_cents.abs() < f64::EPSILON {
                                    0.0
                                } else if t >= sbd.pdecay_secs {
                                    0.001
                                } else {
                                    start_cents
                                        * (0.001 / start_cents.abs())
                                            .powf(f64::from(t / sbd.pdecay_secs))
                                };
                                let freq = f64::from(v.freq_hz) * 2.0f64.powf(cents / 1200.0);
                                // Use a band-limited PeriodicWave table selected
                                // from the swept pitch, not the base note,
                                // because SBD crosses three octaves.
                                let tri = tables.sample(
                                    crate::backend::Waveform::Triangle,
                                    *phase,
                                    tables.selection(freq as f32),
                                );
                                *phase += freq / sr;
                                if *phase >= 1.0 {
                                    *phase -= 1.0;
                                }
                                // WaveShaper saturation, then the drum's own envelope:
                                // hold 1 until 20 ms, exponential to 0.001 over decay.
                                let saturated = sbd_saturation(tri, sbd_saturation_curve);
                                let attackhold = 0.02f32;
                                let body_env = if t <= attackhold {
                                    1.0
                                } else {
                                    0.001f32.powf(((t - attackhold) / sbd.decay_secs).min(1.0))
                                };
                                // Brown noise buffer: gain 1.2 exponentially to
                                // 0.001 over 25 ms, then hold the endpoint.
                                let brown = noise.next(2, 0.0);
                                let noise_env = sbd_noise_gain(t);
                                let mut value = saturated * body_env + brown * noise_env;
                                // 10 ms linear fade landing exactly at the stop.
                                let remaining = sbd.stop_secs - t;
                                if remaining < 0.01 {
                                    value *= (remaining / 0.01).max(0.0);
                                }
                                (value, value)
                            }
                        }
                        VoiceSource::Sample {
                            sample,
                            position,
                            increment,
                            reversed,
                            delay_frames,
                            loop_frames,
                        } => {
                            if bank.get(*sample).is_none() {
                                // The slot was emptied under the voice - an
                                // uninstall landed. Silence for the rest of the
                                // block, and the voice ends with it: the id may
                                // go to another sound, and a voice still reading
                                // the slot would play that one under this name.
                                v.source_gone = true;
                                (0.0, 0.0)
                            } else if *delay_frames > 0 {
                                *delay_frames -= 1;
                                (0.0, 0.0)
                            } else if let Some((loop_start, loop_end)) = *loop_frames
                                && loop_end > loop_start
                                && bank.get(*sample).is_some_and(|decoded| {
                                    *increment * f64::from(sample_pitch)
                                        > loop_end.min(decoded.frames() as f64) - loop_start
                                })
                            {
                                // While the step is longer than the loop, its
                                // end clamped to the buffer, the voice is
                                // silent and its cursor holds, as Chromium's
                                // buffer source does.
                                (0.0, 0.0)
                            } else {
                                let wave = bank
                                    .get(*sample)
                                    .and_then(|decoded| {
                                        if *reversed {
                                            let last = (decoded.frames().max(1) - 1) as f64;
                                            let read = last - *position;
                                            if read < 0.0 {
                                                return None;
                                            }
                                            decoded.stereo_at_reversed_with_mode(
                                                read,
                                                sample_resampling_mode,
                                            )
                                        } else {
                                            decoded.stereo_at_with_mode(
                                                *position,
                                                sample_resampling_mode,
                                            )
                                        }
                                    })
                                    .unwrap_or((0.0, 0.0));
                                *position += *increment * f64::from(sample_pitch);
                                if let Some((loop_start, loop_end)) = loop_frames
                                    && *position >= *loop_end
                                    && *loop_end > *loop_start
                                {
                                    *position -= *loop_end - *loop_start;
                                }
                                wave
                            }
                        }
                        // The wavetable has the same begin gate as the pulse
                        // oscillator and the LFOs. Until the first context
                        // quantum strictly after its onset, it writes
                        // nothing and does not advance its phase.
                        VoiceSource::Wavetable { .. } if !source_open => (0.0, 0.0),
                        VoiceSource::Wavetable { controls, .. }
                            if bank.get(controls.table).is_none() =>
                        {
                            // The table's slot was emptied under the voice.
                            v.source_gone = true;
                            (0.0, 0.0)
                        }
                        VoiceSource::Wavetable {
                            controls,
                            phases,
                            lfo_phase,
                            warp_lfo_phase,
                        } => Self::wavetable_sample(
                            bank,
                            controls,
                            phases,
                            lfo_phase,
                            warp_lfo_phase,
                            v.start_frame as f64 / sr,
                            v.freq_hz * pitch_ratio,
                            t,
                            v.duration_secs,
                            sr as f32,
                            WavetableAdds {
                                position: mod_adds.position,
                                freqspread: mod_adds.freqspread,
                                panspread: mod_adds.panspread,
                                lfo_rate: v.mod_param_hold.wt.rate,
                                lfo_depth: v.mod_param_hold.wt.depth,
                                lfo_skew: v.mod_param_hold.wt.skew,
                                warp: mod_adds.warp,
                                warp_lfo_rate: v.mod_param_hold.warp.rate,
                                warp_lfo_depth: v.mod_param_hold.warp.depth,
                                warp_lfo_skew: v.mod_param_hold.warp.skew,
                            },
                            wavetable_kernel,
                        ),
                    };
                    // `.FX(...)` stages run BEFORE the voice's own params:
                    // `FX = [...FX, value]` appends the hap's controls last,
                    // so each stage is a complete effects pass the signal has
                    // already been through by the time the main chain sees it.
                    let block_n = (abs % 128) as f32;
                    // The source's amplitude envelope applies before the
                    // stages: the envelope gain sits between the source and
                    // the FX loop. The order matters. Crush, shape and
                    // distort are non-linear, so the raw oscillator would
                    // quantise and clip at the wrong level.
                    let has_stages = v.fx_stage_state.is_some();
                    let stretched = v.stretch.is_some() && v.controls.stretch.is_some();
                    // The vocoder and the transient shaper take the same
                    // split as the stages: both are fed the source BEFORE
                    // the main gain, the vocoder's delay leads a gain that
                    // moves, and a nonlinear shaper hears the level it is
                    // given.
                    let split_gain =
                        (has_stages || stretched || v.transient.is_some()) && v.fx_post_gain != 0.0;
                    // With no stages the split is invisible, so keep the
                    // single multiply the rest of the engine expects.
                    let scale = if split_gain {
                        v.amplitude_scale / v.fx_post_gain * env
                    } else {
                        v.amplitude_scale * env * live_gain
                    };
                    let (mut wave_l, mut wave_r) = (wave_l * scale, wave_r * scale);
                    if let Some(states) = &mut v.fx_stage_state {
                        for (index, state) in states.iter_mut().enumerate() {
                            let Some(state) = state else {
                                continue;
                            };
                            let stage_adds =
                                mod_adds.fx_stage.get(index).copied().unwrap_or_default();
                            (wave_l, wave_r) = apply_fx_stage(
                                wave_l,
                                wave_r,
                                state,
                                t,
                                v.hap_duration_secs,
                                block_n,
                                sr as f32,
                                transient_open && note_alive,
                                lfo_live,
                                &stage_adds,
                            );
                        }
                    }
                    // Signal order: earlier .FX() stages -> vocoder ->
                    // transient shaper -> main gain -> filters -> ... ->
                    // sends. The vocoder comes first, as in each stage, so
                    // everything downstream, the sends included, hears the
                    // shifted signal.
                    if let (Some(vocoder), Some(factor)) = (&mut v.stretch, v.controls.stretch) {
                        wave_l = vocoder[0].process(wave_l, factor);
                        // A mono chain carries one channel; the right is its
                        // copy.
                        wave_r = if v.filters_right.is_some() {
                            vocoder[1].process(wave_r, factor)
                        } else {
                            wave_l
                        };
                    }
                    // The shaper is nonlinear, so moving it across the gain
                    // or filters changes the sound.
                    if v.transient.is_some() {
                        if transient_open && note_alive {
                            if v.filters_right.is_some() {
                                let (l, r) = v
                                    .transient
                                    .as_mut()
                                    .map_or((wave_l, wave_r), |s| s.process(wave_l, wave_r));
                                wave_l = l;
                                wave_r = r;
                            } else {
                                // A mono chain reaches the shaper as ONE
                                // channel, and one follower runs.
                                let mono = v
                                    .transient
                                    .as_mut()
                                    .map_or(wave_l, |s| s.process_mono(wave_l));
                                wave_l = mono;
                                wave_r = mono;
                            }
                        } else {
                            // The shaper feeds silence into the downstream chain
                            // before it opens and after hold plus release.
                            wave_l = 0.0;
                            wave_r = 0.0;
                        }
                    }
                    if split_gain {
                        wave_l *= v.fx_post_gain * live_gain;
                        wave_r *= v.fx_post_gain * live_gain;
                    }
                    if let Some(filters_right) = &mut v.filters_right {
                        // STEREO chain: each channel filters and shapes
                        // independently, then the stereo pan law mixes.
                        let (mut left, mut right) = if v.has_filters {
                            (
                                v.filters.process_modulated(
                                    wave_l,
                                    t,
                                    v.hap_duration_secs,
                                    filter_adds,
                                    filter_q_adds,
                                ),
                                filters_right.process_modulated(
                                    wave_r,
                                    t,
                                    v.hap_duration_secs,
                                    filter_adds,
                                    filter_q_adds,
                                ),
                            )
                        } else {
                            (wave_l, wave_r)
                        };
                        if v.has_pre_distort_fx {
                            let worklets = FxWorkletControls {
                                vowel: v.controls.vowel.as_ref(),
                                coarse: v.controls.coarse,
                                crush: v.controls.crush,
                                shape: v.controls.shape,
                            };
                            let block_n = (abs % 128) as f32;
                            left = voice_fx_pre_distort(
                                left,
                                0,
                                worklets,
                                &vowel_coefs,
                                &mut v.vowel_filters,
                                &mut v.coarse_hold,
                                block_n,
                                &v.fx_mod_hold,
                            );
                            right = voice_fx_pre_distort(
                                right,
                                1,
                                worklets,
                                &vowel_coefs,
                                &mut v.vowel_filters,
                                &mut v.coarse_hold,
                                block_n,
                                &v.fx_mod_hold,
                            );
                        }
                        if let Some(distort) = v.controls.distort {
                            // a-rate: DistortProcessor reads per sample.
                            let shape = if mod_adds.distort == 0.0 {
                                v.distort_shape
                            } else {
                                distort.shape_with(mod_adds.distort)
                            };
                            let (l, r) = (distort.apply(left, shape), distort.apply(right, shape));
                            let vol = mod_adds.distort_vol;
                            let (l, r) = if vol == 0.0 {
                                (l, r)
                            } else {
                                let scale = distort.postgain_scale(vol);
                                (l * scale, r * scale)
                            };
                            left = l;
                            right = r;
                        }
                        left *= trem_gain;
                        right *= trem_gain;
                        let (left, right) = match (&mut v.compressor, v.controls.compressor) {
                            (Some(state), Some(c)) => {
                                state.process(left, right, &mod_adds.compressor(c), sr as f32)
                            }
                            _ => (left, right),
                        };
                        let (left, right) = stereo_pan(left, right, pan_param);
                        // Phaser sits AFTER the panner,
                        // one notch state per channel.
                        let (mut left, mut right) = if let Some(coefs) = &phaser_coefs {
                            (
                                v.phaser_notch[0].process(coefs, left),
                                v.phaser_notch[1].process(coefs, right),
                            )
                        } else {
                            (left, right)
                        };
                        left *= post_gain;
                        right *= post_gain;
                        // In line after the voice's own gain, so the delay
                        // and reverb sends and the dry path all carry what
                        // the limiter held - the same place the safety
                        // limiter sits on the master, one voice down.
                        if let Some(limiter) = v.limiter.as_mut() {
                            let (l, r) = limiter.process_frame(left, right);
                            left = l;
                            right = r;
                        }
                        let delay_wet = (v.delay_wet + mod_adds.delay_send).max(0.0);
                        if delay_wet > 0.0 && v.delay_wet > 0.0 {
                            orbit_inputs[v.orbit][0] += left * delay_wet;
                            orbit_inputs[v.orbit][1] += right * delay_wet;
                        }
                        let reverb_wet = (v.reverb_wet + mod_adds.room_send).max(0.0);
                        if reverb_wet > 0.0 && v.reverb_wet > 0.0 {
                            sends[v.orbit][0][i] += left * reverb_wet;
                            sends[v.orbit][1][i] += right * reverb_wet;
                            any_reverb_send = true;
                        }
                        // `effectSend(post, busNode, busgain)` taps the voice
                        // BEFORE its dry gain, so a `.dry(0)` sender is still
                        // heard by whatever modulates off its bus.
                        if let Some(bus) = v.controls.bus {
                            let send = v.controls.busgain;
                            let slot = &mut bus_now[usize::from(bus)];
                            slot[0] += left * send;
                            slot[1] += right * send;
                        }
                        let dry = v.dry_gain + mod_adds.dry;
                        let (left, right) = (left * dry, right * dry);
                        let mix = if v.piano {
                            &mut piano_mix
                        } else if v.through_insert {
                            &mut insert_blocks[v.insert_orbit]
                        } else {
                            &mut blocks[v.orbit]
                        };
                        mix[0][i] += left;
                        mix[1][i] += right;
                        let mut slots = if v.generation >= visual_capture_generation_floor {
                            v.ui_visuals & visual_capture_mask
                        } else {
                            0
                        };
                        while slots != 0 {
                            let slot = slots.trailing_zeros() as usize;
                            let at = (sub_start + i) * 2;
                            if let Some(block) = visual_blocks.get_mut(slot)
                                && at + 1 < block.len()
                            {
                                block[at] += left;
                                block[at + 1] += right;
                            }
                            slots &= slots - 1;
                        }
                    } else {
                        let mut sample = if v.has_filters {
                            v.filters.process_modulated(
                                wave_l,
                                t,
                                v.hap_duration_secs,
                                filter_adds,
                                filter_q_adds,
                            )
                        } else {
                            wave_l
                        };
                        // Mono chain: the modulated pan param re-derives the
                        // equal-power channel gains per frame.
                        let (lg, rg) = if mod_adds.pan != 0.0 {
                            Self::channel_gains(Some((pan_param + 1.0) * 0.5))
                        } else {
                            (v.left_gain, v.right_gain)
                        };
                        if v.has_pre_distort_fx {
                            sample = voice_fx_pre_distort(
                                sample,
                                0,
                                FxWorkletControls {
                                    vowel: v.controls.vowel.as_ref(),
                                    coarse: v.controls.coarse,
                                    crush: v.controls.crush,
                                    shape: v.controls.shape,
                                },
                                &vowel_coefs,
                                &mut v.vowel_filters,
                                &mut v.coarse_hold,
                                (abs % 128) as f32,
                                &v.fx_mod_hold,
                            );
                        }
                        // FX-loop distortion runs after the filter chain
                        // and before the panner, one sample at a time with
                        // no oversampling.
                        if let Some(distort) = v.controls.distort {
                            let shape = if mod_adds.distort == 0.0 {
                                v.distort_shape
                            } else {
                                distort.shape_with(mod_adds.distort)
                            };
                            sample = distort.apply(sample, shape);
                            if mod_adds.distort_vol != 0.0 {
                                sample *= distort.postgain_scale(mod_adds.distort_vol);
                            }
                        }
                        sample *= trem_gain;
                        if let (Some(state), Some(c)) = (&mut v.compressor, v.controls.compressor) {
                            let c = mod_adds.compressor(c);
                            sample = state.process(sample, sample, &c, sr as f32).0;
                        }
                        // Phaser belongs post-pan, but the mono path's pan
                        // is a static per-voice gain pair, which commutes
                        // with the (linear) notch - one state on the mono
                        // signal.
                        if let Some(coefs) = &phaser_coefs {
                            sample = v.phaser_notch[0].process(coefs, sample);
                        }
                        sample *= post_gain;
                        // The same insert as the stereo branch. A mono voice
                        // is the ordinary case for `s("sine")`, so limiting
                        // only the stereo path would make `.limit()` do
                        // nothing on most of what people reach for it with.
                        // Both channels are the one sample here, so the
                        // frame is fed twice and its left comes back.
                        if let Some(limiter) = v.limiter.as_mut() {
                            sample = limiter.process_frame(sample, sample).0;
                        }
                        // The delay send taps after the chain's panner and
                        // post-gain, so echoes keep the voice's stereo image.
                        let delay_wet = (v.delay_wet + mod_adds.delay_send).max(0.0);
                        if delay_wet > 0.0 && v.delay_wet > 0.0 {
                            orbit_inputs[v.orbit][0] += sample * lg * delay_wet;
                            orbit_inputs[v.orbit][1] += sample * rg * delay_wet;
                        }
                        let reverb_wet = (v.reverb_wet + mod_adds.room_send).max(0.0);
                        if reverb_wet > 0.0 && v.reverb_wet > 0.0 {
                            sends[v.orbit][0][i] += sample * lg * reverb_wet;
                            sends[v.orbit][1][i] += sample * rg * reverb_wet;
                            any_reverb_send = true;
                        }
                        // The same send as the stereo branch above. A mono
                        // voice is the ordinary case for `s("sine")`, so
                        // sending only on the stereo path left every bus empty
                        // while every part looked wired.
                        if let Some(bus) = v.controls.bus {
                            let send = v.controls.busgain;
                            let slot = &mut bus_now[usize::from(bus)];
                            slot[0] += sample * lg * send;
                            slot[1] += sample * rg * send;
                        }
                        let dry = v.dry_gain + mod_adds.dry;
                        let (left, right) = (sample * lg * dry, sample * rg * dry);
                        let mix = if v.piano {
                            &mut piano_mix
                        } else if v.through_insert {
                            &mut insert_blocks[v.insert_orbit]
                        } else {
                            &mut blocks[v.orbit]
                        };
                        mix[0][i] += left;
                        mix[1][i] += right;
                        let mut slots = if v.generation >= visual_capture_generation_floor {
                            v.ui_visuals & visual_capture_mask
                        } else {
                            0
                        };
                        while slots != 0 {
                            let slot = slots.trailing_zeros() as usize;
                            let at = (sub_start + i) * 2;
                            if let Some(block) = visual_blocks.get_mut(slot)
                                && at + 1 < block.len()
                            {
                                block[at] += left;
                                block[at + 1] += right;
                            }
                            slots &= slots - 1;
                        }
                    }
                    if v.has_param_modulators && lfo_live {
                        if abs & 127 == 0 {
                            djf_mods[v.orbit] += mod_adds.djf;
                        }
                        delay_time_mods[v.orbit] += mod_adds.delay_time;
                        delay_feedback_mods[v.orbit] += mod_adds.delay_feedback;
                    }
                }
                // Every voice has now had its say about this sample, so what
                // the senders built becomes what the receivers read next.
                // Doing it here rather than per voice is what keeps a bus
                // independent of the order voices happen to sit in.
                for bus in bus_now.iter_mut() {
                    *bus = [0.0; 2];
                }
                if abs & 127 == 0 {
                    for (orbit, state) in djfs.iter_mut().enumerate() {
                        state.modulation = djf_mods[orbit];
                    }
                }
                for (orbit, bus) in self.orbit_delays.iter_mut().enumerate() {
                    if !bus.active {
                        continue;
                    }
                    // WebAudio cycles carry an implicit render-quantum (128
                    // frames) of extra latency on the FEEDBACK edge, so the n-th
                    // echo lands at n·time + (n−1)·128 frames. The line stores
                    // the delay INPUT stream: out(t) = in(t−D), and the feedback
                    // term re-injects out delayed by one quantum
                    // (in(t−D−128)·feedback).
                    const FEEDBACK_QUANTUM: usize = 128;
                    let delay_frames =
                        bus.time_frames as f32 + delay_time_mods[orbit] * self.sample_rate as f32;
                    let out_left = OrbitDelay::read(&bus.left, bus.write, delay_frames);
                    let out_right = OrbitDelay::read(&bus.right, bus.write, delay_frames);
                    blocks[orbit][0][i] += out_left;
                    blocks[orbit][1][i] += out_right;

                    let feedback_left = OrbitDelay::read(
                        &bus.left,
                        bus.write,
                        delay_frames + FEEDBACK_QUANTUM as f32,
                    );
                    let feedback_right = OrbitDelay::read(
                        &bus.right,
                        bus.write,
                        delay_frames + FEEDBACK_QUANTUM as f32,
                    );
                    // Each voice adds its modulator sum. A feedback above 1
                    // makes the delay line grow without limit.
                    let feedback = (bus.feedback + delay_feedback_mods[orbit]).clamp(-1.0, 1.0);
                    bus.store(
                        orbit_inputs[orbit][0] + feedback * feedback_left,
                        orbit_inputs[orbit][1] + feedback * feedback_right,
                    );
                }
            }
            // Reverb returns join the orbit BEFORE the duck gain. A
            // convolver keeps running on silent input so tails ring out; it
            // goes idle only once installed-and-unused from the start of a
            // block window.
            let _ = any_reverb_send;
            for (orbit, slot) in reverbs.iter_mut().enumerate() {
                if let Some(reverb) = slot {
                    let (send, block) = (&sends[orbit], &mut blocks[orbit]);
                    let (left, right) = block.split_at_mut(1);
                    reverb.process_block(
                        &send[0][..sub_len],
                        &send[1][..sub_len],
                        &mut left[0][..sub_len],
                        &mut right[0][..sub_len],
                    );
                }
            }
            // A sample of an insert that is not finite becomes silence, so
            // one bad insert cannot poison what comes after. True means the
            // output was silent.
            let settle = |left: &mut [f32], right: &mut [f32]| {
                let mut silent = true;
                for sample in left.iter_mut().chain(right.iter_mut()) {
                    if !sample.is_finite() {
                        *sample = 0.0;
                    }
                    silent &= sample.abs() < INSERT_SILENCE;
                }
                silent
            };
            let add_insert_output =
                |mix: &mut [[f32; crate::reverb::REVERB_BLOCK]; 2], left: &[f32], right: &[f32]| {
                    for (channel, wet) in [(0, left), (1, right)] {
                        for (out, wet) in mix[channel][..sub_len].iter_mut().zip(wet) {
                            *out += *wet;
                        }
                    }
                };
            let insert_idle_limit = sample_rate.saturating_mul(INSERT_IDLE_SECONDS);
            let (effects, instruments) = inserts.split_at_mut(crate::insert::instrument_slot(0));
            // An instrument makes sound from its notes. The output joins the
            // effects of the orbit when the note asked for an effect, and
            // the orbit when not. An instrument with no sound and no note
            // for `INSERT_IDLE_SECONDS` sleeps until the next note.
            for (bus, slot) in instruments.iter_mut().enumerate() {
                let Some(instrument) = slot else { continue };
                let idle = &mut insert_idle_frames[crate::insert::instrument_slot(bus)];
                if *idle >= insert_idle_limit && !instrument.busy() {
                    continue;
                }
                let mut sound = [[0.0f32; crate::reverb::REVERB_BLOCK]; 2];
                let (left, right) = sound.split_at_mut(1);
                let (left, right) = (&mut left[0][..sub_len], &mut right[0][..sub_len]);
                instrument.process(left, right);
                let silent = settle(left, right);
                let mix = if instrument_to_effect[bus] && effect_stages[bus] != 0 {
                    &mut insert_blocks[bus]
                } else {
                    &mut blocks[insert_outputs[bus]]
                };
                add_insert_output(mix, left, right);
                *idle = if silent {
                    idle.saturating_add(sub_len as u32)
                } else {
                    0
                };
            }
            // The effects take the notes that ask for an effect, each stage
            // after the stage before, and the output of the chain joins the
            // orbit before the duck gain. An effect runs on silent input so
            // the tail rings out, then sleeps after `INSERT_IDLE_SECONDS`
            // with no input and no output.
            for (bus, stages) in effect_stages.iter().enumerate() {
                if *stages == 0 {
                    continue;
                }
                let (left, right) = insert_blocks[bus].split_at_mut(1);
                let (left, right) = (&mut left[0][..sub_len], &mut right[0][..sub_len]);
                for stage in 0..crate::insert::EFFECT_CHAIN {
                    let slot = crate::insert::effect_slot(bus, stage);
                    let Some(effect) = effects[slot]
                        .as_mut()
                        .filter(|_| stages & (1 << stage) != 0)
                    else {
                        continue;
                    };
                    let fed = left.iter().chain(right.iter()).any(|sample| *sample != 0.0);
                    let idle = &mut insert_idle_frames[slot];
                    if !fed && *idle >= insert_idle_limit && !effect.busy() {
                        continue;
                    }
                    effect.process(left, right);
                    let silent = settle(left, right);
                    *idle = if fed || !silent {
                        0
                    } else {
                        idle.saturating_add(sub_len as u32)
                    };
                }
                add_insert_output(&mut blocks[insert_outputs[bus]], left, right);
            }
            let orbit_pairs = self.orbit_pairs;
            let orbit_gains = self.orbit_gains.0;
            let orbit_peaks = &mut self.orbit_peaks;
            let orbit_mix = &mut self.orbit_mix;
            for i in 0..sub_len {
                let abs = start + (sub_start + i) as u64;
                let frame = sub_start + i;
                let mut l = piano_mix[0][i];
                let mut r = piano_mix[1][i];
                for (orbit, block) in blocks.iter().enumerate() {
                    let (mut bl, mut br) = (block[0][i], block[1][i]);
                    let djf = &mut djfs[orbit];
                    if djf.active {
                        // The filter keeps processing zeros, so its state
                        // rings out - run it even on silent frames.
                        bl = djf.process(0, bl, sr);
                        br = djf.process(1, br, sr);
                    }
                    if bl == 0.0 && br == 0.0 {
                        continue;
                    }
                    // The whole orbit - dry and sends alike - lands on the
                    // outputs its FIRST voice keyed; see `orbit_channels`.
                    let (bl, br) = Self::route_channels(self.orbit_channels[orbit], bl, br);
                    let gain = ducks[orbit].value_at(abs) * orbit_gains[orbit];
                    let (ol, or) = (bl * gain, br * gain);
                    orbit_peaks[orbit] = orbit_peaks[orbit].max(ol.abs().max(or.abs()));
                    // An orbit routed to another pair is kept apart for the
                    // device to place; the main pair sums the rest.
                    if routing && orbit_pairs[orbit] != 0 {
                        if frame < ORBIT_MIX_FRAMES {
                            let at = (orbit * ORBIT_MIX_FRAMES + frame) * 2;
                            orbit_mix[at] = ol;
                            orbit_mix[at + 1] = or;
                        }
                        continue;
                    }
                    l += ol;
                    r += or;
                }
                out[frame * 2] = l;
                out[frame * 2 + 1] = r;
            }
            sub_start += sub_len;
        }

        // Drop finished voices, returning leased state.
        let fm_state_pool = &mut self.fm_state_pool;
        let pool = &mut self.compressor_pool;
        let limiters = &mut self.limiter_pool;
        let delay_pool = &mut self.fx_delay_pool;
        let reverb_pool = &mut self.fx_reverb_pool;
        let zzfx_pool = &mut self.zzfx_ring_pool;
        let stretch_pool = &mut self.stretch_pool;
        let fx_stage_state_pool = &mut self.fx_stage_state_pool;
        let pressure = &mut self.pressure;
        self.voices.retain_mut(|v| {
            let age = (end.saturating_sub(v.start_frame)) as f32 / sr as f32;
            if age < v.stop_secs && !v.source_gone {
                true
            } else {
                retire_voice_resources(
                    v,
                    pressure,
                    fm_state_pool,
                    pool,
                    limiters,
                    delay_pool,
                    reverb_pool,
                    zzfx_pool,
                    stretch_pool,
                    fx_stage_state_pool,
                );
                false
            }
        });

        self.frame = end;
    }

    fn name(&self) -> &'static str {
        "scalar"
    }
}

#[cfg(test)]
mod fm_carrier_tests {
    //! A frequency-modulated oscillator reads, at each sample, the band-limited
    //! table for the frequency it has then, and follows that frequency past
    //! Nyquist unclamped, as Firefox does; Chromium clamps the frequency there.

    use super::{VoiceRenderControls, oscillator_sample};
    use crate::Waveform;
    use crate::backend::{
        FmControls, FmOperator, FmRoute, FmWave, MAX_FM_OPERATORS, MAX_FM_ROUTES, OnsetEvent,
        OscillatorControls,
    };
    use crate::periodic_wave::prepared_tables;

    const RATE: usize = 48_000;

    /// `fm(index)`: one sine operator at the carrier's own frequency.
    fn simple_fm(index: f32) -> FmControls {
        let mut operators = [None; MAX_FM_OPERATORS];
        operators[0] = Some(FmOperator {
            harmonicity: 1.0,
            waveform: FmWave::Sine,
            env: None,
            env_exponential: false,
        });
        let mut routes = [None; MAX_FM_ROUTES];
        routes[0] = Some(FmRoute {
            source: 1,
            target: 0,
            amount: index,
            mod_slot: None,
        });
        FmControls { operators, routes }
    }

    /// One second of the left channel of a sawtooth at `freq_hz`, from a quarter
    /// of a second into a note held for two: the envelope is steady throughout.
    fn held_sawtooth(freq_hz: f32, fm: Option<FmControls>) -> Vec<f32> {
        let controls = OscillatorControls {
            waveform: Waveform::Sawtooth,
            fm,
            ..OscillatorControls::default()
        };
        let event = OnsetEvent::new(0, freq_hz, 0.5, 2.0).with_controls(controls);
        let mut backend = super::ScalarBackend::new();
        let pcm = crate::render::render_pcm(&mut backend, RATE as u32, 2 * RATE, &[event])
            .expect("render");
        pcm.as_chunks::<2>()
            .0
            .iter()
            .map(|frame| frame[0])
            .skip(RATE / 4)
            .take(RATE)
            .collect()
    }

    /// The share of a one-second `signal`'s energy on the harmonics of `f0`
    /// (whole hertz, so each harmonic falls on a bin), DC aside.
    fn harmonic_share(signal: &[f32], f0: usize) -> f64 {
        let mut spectrum: Vec<rustfft::num_complex::Complex<f64>> = signal
            .iter()
            .map(|&sample| rustfft::num_complex::Complex::new(f64::from(sample), 0.0))
            .collect();
        rustfft::FftPlanner::new()
            .plan_fft_forward(spectrum.len())
            .process(&mut spectrum);
        let power = |bin: usize| spectrum[bin].norm_sqr();
        let total: f64 = (1..=RATE / 2).map(power).sum();
        let harmonic: f64 = (f0..=RATE / 2).step_by(f0).map(power).sum();
        harmonic / total
    }

    /// FM with the modulator at the carrier's frequency puts every partial on a
    /// harmonic of it. What lands between them is aliasing: partials a table
    /// chosen for the unmodulated note carries past Nyquist once the frequency
    /// swings up. At 293 Hz and an index of 40 the frequency swings to 12 kHz
    /// and back through zero, inside the band throughout.
    #[test]
    fn a_modulated_sawtooth_keeps_its_partials_below_nyquist() {
        let plain = harmonic_share(&held_sawtooth(293.0, None), 293);
        assert!(plain > 0.999, "the unmodulated sawtooth aliases: {plain}");
        let modulated = harmonic_share(&held_sawtooth(293.0, Some(simple_fm(40.0))), 293);
        assert!(
            modulated > 0.99,
            "{:.1}% of the modulated sawtooth's energy is aliasing",
            100.0 * (1.0 - modulated)
        );
    }

    /// Where the modulated frequency passes Nyquist, the band-limited table for
    /// it has no partials left: the carrier is silent for that part of each
    /// modulator cycle instead of folding back, and sounds for the rest of it,
    /// whichever way the frequency swings.
    #[test]
    fn a_carrier_swept_past_nyquist_is_silent_there_and_only_there() {
        let f0 = 830.61f64;
        let index = 100.0;
        let played = held_sawtooth(f0 as f32, Some(simple_fm(index as f32)));
        let peak = played
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 0.05, "the carrier is silent throughout: {peak}");
        let silent =
            played.iter().filter(|sample| sample.abs() < 1e-6).count() as f64 / played.len() as f64;
        // The share of a modulator cycle spent at or past Nyquist.
        let steps = 100_000;
        let past = (0..steps)
            .filter(|step| {
                let angle = std::f64::consts::TAU * f64::from(*step) / f64::from(steps);
                (f0 * (1.0 + index * angle.sin())).abs() >= RATE as f64 / 2.0
            })
            .count() as f64
            / f64::from(steps);
        assert!(
            (silent - past).abs() <= 0.01,
            "the carrier is past Nyquist {:.1}% of the time and silent {:.1}% of it",
            100.0 * past,
            100.0 * silent
        );
    }

    /// With the modulator at the carrier's frequency, the carrier's phase gains
    /// exactly one cycle per modulator cycle, so every partial stays on a
    /// harmonic, including while the frequency is past Nyquist. Chromium's clamp
    /// there drops part of that cycle and moves every partial off the series.
    #[test]
    fn a_carrier_swept_past_nyquist_keeps_its_harmonic_series() {
        // g#5 and c6, to the nearest hertz, at fm(32).
        for f0 in [831usize, 1047] {
            let played = held_sawtooth(f0 as f32, Some(simple_fm(32.0)));
            let share = harmonic_share(&played, f0);
            assert!(
                share > 0.8,
                "only {:.1}% of the {f0} Hz carrier's energy is on its harmonics",
                100.0 * share
            );
        }
    }

    /// A sine carrier past Nyquist plays the sine of its unclamped phase, which
    /// aliases, as Firefox's directly computed sine does. Chromium clamps the
    /// frequency to Nyquist, where its band-limited sine table is silent.
    #[test]
    fn a_sine_carrier_past_nyquist_follows_its_unclamped_phase() {
        let tables = prepared_tables(RATE as u32);
        let controls = VoiceRenderControls::from(OscillatorControls {
            waveform: Waveform::Sine,
            ..OscillatorControls::default()
        });
        let freq = 440.0f32;
        for offset in [30_000.0f32, -30_000.0] {
            let played_hz = f64::from(freq + offset);
            let mut phase = 0.0f64;
            for frame in 0..RATE {
                let played = oscillator_sample(
                    &tables,
                    &controls,
                    tables.selection(freq),
                    &mut phase,
                    freq,
                    offset,
                    1.0,
                    RATE as f64,
                );
                let cycles = (played_hz * frame as f64 / RATE as f64).fract();
                let expected = (std::f64::consts::TAU * cycles).sin() as f32;
                assert!(
                    (played - expected).abs() < 1e-6,
                    "{played_hz} Hz, frame {frame}: {played} against {expected}"
                );
            }
        }
    }
}

#[cfg(test)]
mod steady_oscillator_tests {
    //! An oscillator whose frequency holds still reads the table chosen at its
    //! start and steps its phase by one period's wrap, bit for bit. Only a moving
    //! frequency picks its table at each sample.

    use super::{VoiceRenderControls, oscillator_sample, partials_sample};
    use crate::Waveform;
    use crate::backend::{MAX_PARTIALS, OscillatorControls, PartialsControls};
    use crate::periodic_wave::prepared_tables;

    const RATE: f64 = 48_000.0;

    /// Eight partials with both cosine and sine terms, so a phase that drifts
    /// shows in either.
    fn custom_wave() -> PartialsControls {
        let mut real = [0.0; MAX_PARTIALS];
        let mut imag = [0.0; MAX_PARTIALS];
        for k in 0..8 {
            real[k] = 1.0 / (k + 1) as f32;
            imag[k] = 0.5 / (k + 1) as f32;
        }
        PartialsControls {
            real,
            imag,
            len: 8,
            norm: 0.5,
        }
    }

    /// Against the steady oscillator written out: the table picked once from the
    /// start frequency, and the phase stepped by `freq / sr` and brought back by
    /// one period either way, all a frequency below the sample rate needs. The
    /// frequencies run from low to past Nyquist, and one runs backwards.
    #[test]
    fn a_steady_oscillator_reads_its_start_table_and_wraps_by_one_period() {
        let tables = prepared_tables(RATE as u32);
        let nyquist = (RATE * 0.5) as f32;
        let voices = [
            (Waveform::Sine, None),
            (Waveform::Square, None),
            (Waveform::Sawtooth, None),
            (Waveform::Triangle, None),
            (Waveform::Sine, Some(custom_wave())),
        ];
        for (waveform, partials) in voices {
            let controls = VoiceRenderControls::from(OscillatorControls {
                waveform,
                partials,
                ..OscillatorControls::default()
            });
            let voice = if partials.is_some() {
                "custom wave".to_owned()
            } else {
                format!("{waveform:?}")
            };
            for freq in [27.5f32, 440.0, 3_520.0, 18_000.0, 30_000.0, -440.0] {
                let selection = tables.selection(freq);
                let mut phase = 0.0f64;
                let mut expected_phase = 0.0f64;
                for frame in 0..RATE as usize {
                    let played = oscillator_sample(
                        &tables, &controls, selection, &mut phase, freq, 0.0, 1.0, RATE,
                    );
                    let expected = match &partials {
                        Some(partials) => {
                            partials_sample(partials, expected_phase, freq.abs(), nyquist)
                        }
                        None => tables.sample(waveform, expected_phase, selection),
                    };
                    expected_phase += f64::from(freq) / RATE;
                    if expected_phase >= 1.0 {
                        expected_phase -= 1.0;
                    }
                    if expected_phase < 0.0 {
                        expected_phase += 1.0;
                    }
                    assert_eq!(
                        played.to_bits(),
                        expected.to_bits(),
                        "{voice} at {freq} Hz, frame {frame}: sample"
                    );
                    assert_eq!(
                        phase.to_bits(),
                        expected_phase.to_bits(),
                        "{voice} at {freq} Hz, frame {frame}: phase"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod lfo_waveshape_tests {
    /// `tremolo` defaults to skew 1, and `tri` divides by `1 - skew` on its
    /// `phase >= skew` branch. The phase reaches exactly 1.0 two ways: the
    /// walks wrap on a strict `>`, so a dyadic step (375 Hz at 48 kHz adds
    /// 2^-7) sums to 1.0 and stores it, and f32 rounds everything above
    /// `1 - 2^-25` up to 1.0. Either way the branch divided by zero and
    /// `Infinity - Infinity` put NaN into the output.
    #[test]
    fn a_phase_just_under_one_never_divides_by_a_zero_skew() {
        for phase in [
            1.0,
            1.0 - f64::EPSILON,
            0.999_999_999_9,
            0.999_999_999_999_999_9,
            1.0 - 1e-12,
            1.0 - 1e-15,
        ] {
            let value = super::lfo_waveshape(0, phase, 1.0);
            assert!(
                value.is_finite(),
                "tri({phase}, skew 1) produced {value}, which reaches the output"
            );
        }

        // Nothing else about the shape may move: skew 1 makes it a plain ramp.
        for phase in [0.0_f64, 0.25, 0.5, 0.75] {
            let value = super::lfo_waveshape(0, phase, 1.0);
            assert!(
                (f64::from(value) - phase).abs() < 1e-6,
                "tri({phase}, skew 1) should be the phase itself, got {value}"
            );
        }

        // And the other shapes stay finite wherever the phase can land.
        for shape in 0..5u8 {
            for phase in [0.0_f64, 0.5, 1.0 - f64::EPSILON, 1.0] {
                for skew in [0.0_f32, 0.5, 1.0] {
                    let value = super::lfo_waveshape(shape, phase, skew);
                    assert!(
                        value.is_finite() || skew == 0.0,
                        "shape {shape} at phase {phase} skew {skew} gave {value}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod pending_event_tests {
    use super::PendingEvents;
    use crate::backend::OnsetEvent;

    fn event(onset_frame: u64) -> OnsetEvent {
        OnsetEvent::new(onset_frame, onset_frame as f32, 1.0, 0.1)
    }

    #[test]
    fn reused_pending_slots_stay_in_admission_order() {
        let mut pending = PendingEvents::with_capacity(4);
        pending.push(event(10));
        pending.push(event(20));
        pending.push(event(30));

        pending.retain(|event| event.onset_frame != 20);
        assert_eq!(pending.len(), 2);
        let capacity = pending.capacity();

        // This reuses the physical slot formerly occupied by frame 20. Its
        // logical position must still be the tail, after the older frame 30.
        pending.push(event(40));
        let mut seen = Vec::new();
        pending.retain(|event| {
            seen.push(event.onset_frame);
            true
        });
        assert_eq!(seen, [10, 30, 40]);
        assert_eq!(pending.capacity(), capacity);

        pending.retain(|event| event.onset_frame >= 35);
        pending.push(event(50));
        let mut seen = Vec::new();
        pending.retain(|event| {
            seen.push(event.onset_frame);
            true
        });
        assert_eq!(seen, [40, 50]);
        assert_eq!(pending.capacity(), capacity);

        pending.clear();
        assert_eq!(pending.len(), 0);
        assert_eq!(pending.capacity(), capacity);
    }

    #[test]
    fn pending_slots_match_vec_order_through_repeated_reuse() {
        const CAPACITY: usize = 32;
        let mut pending = PendingEvents::with_capacity(CAPACITY);
        let mut expected = Vec::with_capacity(CAPACITY);
        let mut next_frame = 0u64;
        let capacity = pending.capacity();

        for round in 0..1_000u64 {
            let additions = ((round * 7 + 3) % 6) as usize;
            for _ in 0..additions.min(CAPACITY - expected.len()) {
                let next = event(next_frame);
                pending.push(next);
                expected.push(next);
                next_frame += 1;
            }

            let divisor = round % 7 + 2;
            pending.retain(|event| (event.onset_frame + round) % divisor != 0);
            expected.retain(|event| (event.onset_frame + round) % divisor != 0);

            let mut observed = Vec::with_capacity(expected.len());
            pending.retain(|event| {
                observed.push(*event);
                true
            });
            assert_eq!(
                observed, expected,
                "admission order changed in round {round}"
            );
            assert_eq!(pending.len(), expected.len());
            assert_eq!(pending.capacity(), capacity);
        }
    }
}

#[cfg(test)]
mod polyphony_tests {
    use super::{MAX_CONFIGURABLE_POLYPHONY, MAX_POLYPHONY, ScalarBackend, polyphony_kills};
    use crate::{AudioBackend, OnsetEvent};

    /// The kill loop re-reads its own condition as the pool shrinks, so it
    /// converges on the cap over successive onsets rather than clamping to
    /// it.
    #[test]
    fn the_polyphony_cap_kills_the_oldest_and_converges_rather_than_clamping() {
        assert_eq!(MAX_POLYPHONY, 128, "DEFAULT_MAX_POLYPHONY");

        // Under the cap nothing is touched - this is the ordinary case, and
        // a cap that fired here would thin out music it must leave alone.
        for active in [0, 1, 64, 127] {
            assert_eq!(
                polyphony_kills(active, MAX_POLYPHONY),
                0,
                "{active} sounds is under the cap"
            );
        }

        // At and just over it, one goes.
        assert_eq!(polyphony_kills(128, MAX_POLYPHONY), 1);
        assert_eq!(polyphony_kills(129, MAX_POLYPHONY), 1);

        // Further over, it does NOT clamp back in one pass: `i` advances while
        // the size falls, so they meet in the middle.
        assert_eq!(
            polyphony_kills(130, MAX_POLYPHONY),
            2,
            "130 loses two, leaving 128"
        );
        assert_eq!(
            polyphony_kills(131, MAX_POLYPHONY),
            2,
            "131 loses two, leaving 129"
        );
        // 140 and the index meet in the middle at 133, so seven go.
        assert_eq!(polyphony_kills(140, MAX_POLYPHONY), 7);

        // Whatever the excess, it always leaves at least the cap standing.
        for active in [128, 150, 200, 512] {
            let left = active - polyphony_kills(active, MAX_POLYPHONY);
            assert!(
                left >= MAX_POLYPHONY - 1,
                "{active} active left only {left} sounding"
            );
            assert!(left < active, "{active} active must lose something");
        }
    }
    fn populated(limit: usize) -> ScalarBackend {
        let mut backend = ScalarBackend::prepared(8_000, 256).expect("backend");
        backend.set_max_polyphony(limit);
        for _ in 0..160 {
            backend.note(OnsetEvent::new(0, 220.0, 0.001, 2.0));
        }
        backend.process_block(&mut [0.0; 256], 128);
        backend
    }

    #[test]
    fn raising_polyphony_keeps_more_than_128_voices_sounding() {
        let mut default = populated(MAX_POLYPHONY);
        let mut raised = populated(MAX_CONFIGURABLE_POLYPHONY);
        assert_eq!(default.pressure.semantic_polyphony_fades, 32);
        assert_eq!(raised.pressure.semantic_polyphony_fades, 0);
        let mut default_output = [0.0; 256];
        let mut raised_output = [0.0; 256];
        for _ in 0..20 {
            default.process_block(&mut default_output, 128);
            raised.process_block(&mut raised_output, 128);
        }
        assert_eq!(default.voices.len(), 128);
        assert_eq!(raised.voices.len(), 160);
        let energy = |samples: &[f32]| samples.iter().map(|sample| sample * sample).sum::<f32>();
        assert!(energy(&raised_output) > energy(&default_output) * 1.4);
    }

    #[test]
    fn lowering_polyphony_fades_oldest_voices_without_cutting_or_restarting() {
        let mut backend = populated(MAX_CONFIGURABLE_POLYPHONY);
        let frame = backend.frame;
        backend.set_max_polyphony(64);
        assert_eq!(backend.frame, frame);
        assert_eq!(
            backend.voices.len(),
            160,
            "retiring voices must keep ringing"
        );
        assert_eq!(backend.pressure.semantic_polyphony_fades, 96);
        assert!(
            backend.voices[..96]
                .iter()
                .all(|voice| voice.polyphony_fade_frame == Some(frame))
        );
        assert!(
            backend.voices[96..]
                .iter()
                .all(|voice| voice.polyphony_fade_frame.is_none())
        );
        backend.set_max_polyphony(64);
        assert_eq!(
            backend.pressure.semantic_polyphony_fades, 96,
            "reapplying the setting must not fade more voices"
        );
        for _ in 0..20 {
            backend.process_block(&mut [0.0; 256], 128);
        }
        assert_eq!(backend.voices.len(), 64);
    }

    #[test]
    fn polyphony_is_bounded_per_backend_and_survives_reset_and_init() {
        let mut first = ScalarBackend::new();
        let second = ScalarBackend::new();
        assert_eq!(first.max_polyphony(), MAX_POLYPHONY);
        first.set_max_polyphony(0);
        assert_eq!(first.max_polyphony(), 1);
        first.set_max_polyphony(usize::MAX);
        assert_eq!(first.max_polyphony(), MAX_CONFIGURABLE_POLYPHONY);
        assert_eq!(second.max_polyphony(), MAX_POLYPHONY);
        first.init(8_000).expect("backend");
        first.reset();
        assert_eq!(first.max_polyphony(), MAX_CONFIGURABLE_POLYPHONY);
    }
}

#[cfg(test)]
mod channel_route_tests {
    /// Every expectation here was read off Chromium's own left/right rms for
    /// `s("bd").bank("tr909")`, whose channels are 0.092521 and 0.092374.
    #[test]
    fn channels_wires_each_source_channel_to_the_output_it_names() {
        let route = |channels| super::ScalarBackend::route_channels(channels, 1.0, 2.0);

        // Absent is plain stereo.
        assert_eq!(route(None), (1.0, 2.0));
        // channels("1:2") names the outputs it already had.
        assert_eq!(route(Some([1, 2])), (1.0, 2.0));
        // channels("2:1") swaps the pair -- the order is meaningful.
        assert_eq!(route(Some([2, 1])), (2.0, 1.0));
        // A single channel routes the source's FIRST channel and DROPS its
        // second; the output nobody named stays silent. This is the case that
        // was playing twice as loud in mono.
        assert_eq!(route(Some([1, 0])), (1.0, 0.0));
        assert_eq!(route(Some([2, 0])), (0.0, 1.0));
        // Two entries naming one output sum there.
        assert_eq!(route(Some([1, 1])), (3.0, 0.0));
        assert_eq!(route(Some([2, 2])), (0.0, 3.0));
        // And the destination wraps on the render's channel count, so
        // channels(3) is channels(1) and channels(4) is channels(2).
        assert_eq!(route(Some([3, 0])), (1.0, 0.0));
        assert_eq!(route(Some([4, 0])), (0.0, 1.0));
        assert_eq!(route(Some([3, 4])), (1.0, 2.0));
    }
}

#[cfg(test)]
mod ring_out_tests {
    /// Filters outlive the source that fed them; a voice that stops dead at
    /// its envelope loses the tail.
    #[test]
    fn a_ringing_voice_outlives_its_source_and_a_dry_one_does_not() {
        let sr = 48_000.0;
        let allowance = 0.01 + 256.0 / sr;
        let bare = super::stop_secs_for(0.6, false, sr);
        assert!(
            (bare - 0.6).abs() < 1e-6,
            "nothing ringing means nothing to wait for, got {bare}"
        );

        // vowel is the case this exists for: it lives outside FilterChain, so
        // it used to be missed even though its x8 makeup makes it the loudest
        // ring of the lot.
        let ringing = super::stop_secs_for(0.6, true, sr);
        assert!(
            (ringing - (0.6 + allowance)).abs() < 1e-6,
            "a ringing voice must outlive its source, got {ringing}"
        );
        assert!(ringing > bare);

        // The allowance rides on whatever stopped the source, so a slice that
        // ended early because the buffer ran out still gets it. A chopped
        // sample through a vowel is the case: its slices are short, and
        // truncating each ring cost corr 0.982.
        let sliced = super::stop_secs_for(0.05, true, sr);
        assert!(
            (sliced - (0.05 + allowance)).abs() < 1e-6,
            "a short slice rings just as long, got {sliced}"
        );
    }
}

#[cfg(test)]
mod lfo_quantum_gate_tests {
    /// The gate compares an f64 against an f32 begin. In `s("ht*3").tremolo(4)`
    /// at 48 kHz, five of the six onset quanta tie and are skipped. The sixth
    /// begin rounds down in f32, so its onset quantum runs.
    #[test]
    fn the_onset_quantum_runs_only_when_f32_rounds_the_begin_down() {
        let sr = 48_000.0;
        let opens_on_its_own_onset = |k: u64| {
            let begin = (2.0 * k as f64 / 3.0) as f32;
            super::lfo_quantum_open(k * 32_000, sr, begin)
        };
        for k in [0, 1, 2, 3, 4] {
            assert!(
                !opens_on_its_own_onset(k),
                "note {k}'s begin rounds up, so its onset quantum ties and is skipped"
            );
        }
        assert!(
            opens_on_its_own_onset(5),
            "note 5's begin rounds down, so its onset quantum is already past it"
        );
    }

    /// Whatever the tie does, the quantum after it always runs, and one
    /// before the note never does.
    #[test]
    fn the_gate_is_shut_before_the_note_and_open_after_it() {
        let sr = 48_000.0;
        let begin = (2.0_f64 / 3.0) as f32;
        assert!(!super::lfo_quantum_open(31_872, sr, begin));
        assert!(super::lfo_quantum_open(32_128, sr, begin));
    }

    /// The end is read the same way, and a sample ringing past it is not
    /// modulated.
    #[test]
    fn the_gate_shuts_at_the_f32_end() {
        let sr = 48_000.0;
        // A note at 0 lasting an eighth of a second, with the default 10 ms
        // release: 6480 frames.
        let end = 0.135_f32;
        assert!(super::lfo_quantum_alive(6_400, sr, end));
        assert!(!super::lfo_quantum_alive(6_528, sr, end));
    }
}

#[cfg(test)]
mod note_end_tests {
    /// A tremolo or phaser LFO ends at the note's hold end PLUS its release,
    /// and the note here is an eighth of a second at 48 kHz with the default
    /// 10 ms release: 6480 frames in all.
    #[test]
    fn a_tremolo_lfo_outlives_the_note_by_its_release_and_no_further() {
        let sr = 48_000.0;
        let alive = |q| super::note_end_alive(q, 0, 0.0, 0.125, 0.01, sr);

        assert!(alive(0), "it runs from the note's own start");
        assert!(
            alive(6_400),
            "the last quantum before the release end is live"
        );
        assert!(!alive(6_528), "the first quantum past it is not");

        // This is the bug it exists for: a sample held for its own slice goes
        // on sounding long after the LFO has stopped, and every one of those
        // quanta is unmodulated.
        for q in (6_528..24_000).step_by(128) {
            assert!(!alive(q), "the ringing tail at {q} must be unmodulated");
        }
    }

    /// A long release carries the LFO with it, which is what makes the same
    /// score with `.release(2)` match sample for sample where the default
    /// release does not.
    #[test]
    fn a_long_release_keeps_the_lfo_running_over_the_whole_tail() {
        let sr = 48_000.0;
        let alive = |q| super::note_end_alive(q, 0, 0.0, 0.125, 2.0, sr);
        for q in (0..100_000).step_by(128) {
            assert!(alive(q), "the quantum at {q} is inside the release");
        }
        assert!(!alive(102_144), "past two seconds it is over");
    }

    /// A later note carries its own end, and a sub-sample onset shifts that
    /// end by the same fraction it shifts the start.
    #[test]
    fn each_note_ends_its_own_lfo() {
        let sr = 48_000.0;
        assert!(super::note_end_alive(48_000, 48_000, 0.0, 0.125, 0.01, sr));
        assert!(!super::note_end_alive(54_528, 48_000, 0.0, 0.125, 0.01, sr));
        assert!(!super::note_end_alive(6_480, 0, 0.5, 0.125, 0.01, sr));
    }
}

#[cfg(test)]
mod lfo_phase_tests {
    /// An LFO adds `frequency/sampleRate` once per sample for the whole life
    /// of the voice, so the accumulator's precision decides where the wrap
    /// lands - and the wrap is a discontinuity in every shape but the sine.
    #[test]
    fn an_lfo_wraps_on_the_exact_cycle_however_long_the_note_runs() {
        let sr = 48_000.0_f64;
        let freq = 6.0_f32; // tremolo(6): exactly 8000 samples per cycle

        let mut phase = super::lfo_phase0(0.0, freq, 0.0);
        assert_eq!(phase, 0.0, "a note at cycle 0 starts the LFO at phase 0");

        let mut wraps = Vec::new();
        for n in 0..48_000u32 {
            let next = phase + f64::from(freq) / sr;
            phase = if next > 1.0 {
                wraps.push(n + 1);
                next - 1.0
            } else {
                next
            };
        }

        // Six cycles per second, and not one sample of drift by the last of
        // them. In f32 this walks off by a sample per cycle: simulated, f64
        // crosses 1.0 after 8000 steps and f32 after 8001, which showed up
        // against Chromium as a 4.6x gain spike two samples wide, once per
        // cycle, on `s("sawtooth").tremolo(6)`.
        assert_eq!(
            wraps,
            vec![8_000, 16_000, 24_000, 32_000, 40_000, 48_000],
            "the LFO drifted off its cycle"
        );
    }
}

#[cfg(test)]
mod nudge_tests {
    /// `nudge` shifts the SOURCE, not the envelope, by a float number of
    /// seconds.
    #[test]
    fn a_nudge_starts_on_the_frame_at_or_after_it_never_the_one_before() {
        let sr = 48_000.0;

        // The values a person actually types all land a hair UNDER a whole
        // frame, so truncation dropped one every time.
        for (nudge, expected) in [(0.005_f32, 240_u32), (0.01, 480), (0.02, 960), (0.03, 1440)] {
            let (delay, lead) = super::nudged_start(nudge, 0.0, sr);
            assert_eq!(
                delay, expected,
                "nudge({nudge}) should wait {expected} frames"
            );
            assert!(lead < 0.001, "nudge({nudge}) left a lead of {lead}");
        }

        // And one that lands just OVER, which must not round back down.
        let (delay, lead) = super::nudged_start(0.025, 0.0, sr);
        assert_eq!(delay, 1_201);
        assert!(
            lead > 0.99,
            "0.025 starts just past frame 1200, lead was {lead}"
        );

        // No nudge must be bit-identical to before the fix: the lead is the
        // onset's own, and nothing waits.
        for onset_lead in [0.0_f32, 0.25, 0.5, 0.999] {
            let (delay, lead) = super::nudged_start(0.0, onset_lead, sr);
            assert_eq!(delay, 0, "an unnudged source must not wait");
            assert!(
                (lead - f64::from(onset_lead)).abs() < 1e-9,
                "expected the onset's own lead {onset_lead}, got {lead}"
            );
        }

        // A negative nudge cannot play before the voice exists, so it floors at
        // zero delay and turns the whole offset into lead.
        let (delay, lead) = super::nudged_start(-0.01, 0.0, sr);
        assert_eq!(delay, 0);
        assert!(
            (lead - 480.0).abs() < 0.01,
            "expected ~480 frames of lead, got {lead}"
        );

        // The lead is always a real sub-sample residue except where the delay
        // floored, which is the only case that can exceed one frame.
        for nudge in [0.0_f32, 0.001, 0.0123, 0.02, 0.5] {
            for onset_lead in [0.0_f32, 0.3, 0.7] {
                let (_, lead) = super::nudged_start(nudge, onset_lead, sr);
                assert!(
                    (0.0..=1.0).contains(&lead),
                    "nudge {nudge}/{onset_lead}: lead {lead}"
                );
            }
        }
    }
}

#[cfg(test)]
mod noise_playback_tests {
    #[test]
    fn sbd_noise_gain_holds_the_exponential_ramp_endpoint() {
        let midpoint = super::sbd_noise_gain(0.0125);
        assert!((super::sbd_noise_gain(0.0) - 1.2).abs() < 1e-7);
        assert!((midpoint - (1.2_f32 * 0.001).sqrt()).abs() < 1e-6);
        assert!((super::sbd_noise_gain(0.025) - 0.001).abs() < 1e-7);
        assert!((super::sbd_noise_gain(0.25) - 0.001).abs() < 1e-7);
    }

    /// The noise buffer is played, not generated inline: playback starts at
    /// a float time and therefore reads between samples.
    #[test]
    fn a_noise_buffer_read_between_samples_loses_the_level_blink_loses() {
        // A noise note starts at the note's own float time, so the buffer
        // is read at a fractional position and linearly interpolated.
        // Adjacent white samples are independent, so that mixing attenuates
        // by sqrt((1-f)^2 + f^2) - the curve held to within 0.4% against
        // Chromium at eight forced offsets, so allow 2%.
        fn rms_of(frac: f32) -> f32 {
            let mut source = super::NoiseGen::at_onset(48_000, frac);
            let n = 40_000;
            let power: f32 = (0..n).map(|_| source.next(0, 0.0)).map(|s| s * s).sum();
            (power / n as f32).sqrt()
        }

        let aligned = rms_of(0.0);
        assert!(
            aligned > 0.5,
            "white noise should sit near 1/sqrt(3), got {aligned}"
        );
        for frac in [0.125_f32, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875] {
            let expected = ((1.0 - frac).powi(2) + frac.powi(2)).sqrt();
            let ratio = rms_of(frac) / aligned;
            assert!(
                (ratio - expected).abs() < 0.02,
                "offset {frac}: expected {expected:.4} of full level, got {ratio:.4}"
            );
        }

        // Half a sample is the worst case, and it is a full -3.01 dB.
        assert!((rms_of(0.5) / aligned - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02);

        // A frame-aligned source must be untouched - same sequence, not just
        // the same level, or every unshifted render moves.
        let mut plain = super::NoiseGen::new(48_000);
        let mut onset = super::NoiseGen::at_onset(48_000, 0.0);
        for i in 0..1_000 {
            assert_eq!(plain.next(0, 0.0), onset.next(0, 0.0), "diverged at {i}");
        }
    }
}

#[cfg(test)]
mod source_gone_tests {
    use crate::sample::{DecodedSample, SampleControls, SampleHold, SampleId};
    use crate::{AudioBackend, OnsetEvent, ScalarBackend};

    const RATE: u32 = 48_000;
    const BLOCK: usize = 128;

    fn sample_event(id: SampleId) -> OnsetEvent {
        let mut event = OnsetEvent::new(0, 440.0, 1.0, 4.0);
        event.sample = Some(SampleControls {
            sample: id,
            playback_rate: 1.0,
            begin: 0.0,
            end: 1.0,
            hold: SampleHold::Hap,
            muted: false,
            loop_secs: None,
            envelope_peak: 1.0,
            reversed: false,
            nudge_secs: 0.0,
            cut: None,
        });
        event
    }

    fn render(backend: &mut ScalarBackend) -> Vec<f32> {
        let mut output = vec![0.0; BLOCK * 2];
        backend.process_block(&mut output, BLOCK);
        output
    }

    /// An uninstall lands under a sounding voice: the voice goes silent for
    /// the rest of the block and is gone at its end, so the id can go to
    /// another sound without this voice playing that sound under its name.
    #[test]
    fn a_voice_whose_slot_was_cleared_retires_at_the_block_end() {
        let id = SampleId(9);
        let mut backend = ScalarBackend::prepared(RATE, 4).expect("backend");
        let pcm = DecodedSample::from_parts(RATE, 1, vec![0.5; RATE as usize * 4]).expect("pcm");
        assert!(backend.install_sample(id, Box::new(pcm)).is_ok());
        assert!(backend.try_note_prepared(sample_event(id)));

        let sounding = render(&mut backend);
        assert!(
            sounding.iter().any(|sample| sample.abs() > 1e-3),
            "the voice reads its slot"
        );
        assert_eq!(backend.voices.len(), 1);

        assert!(backend.clear_sample(id).is_some(), "the slot held the PCM");
        let cleared = render(&mut backend);
        assert!(
            cleared.iter().all(|sample| *sample == 0.0),
            "silence once the slot is empty"
        );
        assert!(
            backend.voices.is_empty(),
            "the voice retired with the block, well before its 4 s duration"
        );

        // The same id installed again is a different sound: nothing from the
        // old voice reads it.
        let other = DecodedSample::from_parts(RATE, 1, vec![-0.25; RATE as usize]).expect("pcm");
        assert!(backend.install_sample(id, Box::new(other)).is_ok());
        let quiet = render(&mut backend);
        assert!(
            quiet.iter().all(|sample| *sample == 0.0),
            "no voice survived to play the newcomer"
        );
    }

    /// The mixer's orbit fader sits after the duck and before the sum:
    /// half the fader, half the orbit, and the orbit's own peak reads
    /// post-fader. A backend built from nothing opens at unity.
    #[test]
    fn an_orbit_fader_scales_the_orbit_and_its_meter() {
        assert_eq!(super::OrbitGains::default().0, [1.0; super::MAX_ORBITS]);
        let id = SampleId(9);
        let loud = |gains: [f32; super::MAX_ORBITS]| {
            let mut backend = ScalarBackend::prepared(RATE, 4).expect("backend");
            let pcm = DecodedSample::from_parts(RATE, 1, vec![0.5; RATE as usize]).expect("pcm");
            assert!(backend.install_sample(id, Box::new(pcm)).is_ok());
            backend.set_orbit_gains(&gains);
            assert!(backend.try_note_prepared(sample_event(id)));
            let out = render(&mut backend);
            let peak = out
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            let orbit_peak = backend
                .take_orbit_peaks()
                .iter()
                .fold(0.0f32, |peak, level| peak.max(*level));
            (peak, orbit_peak)
        };
        let (full, full_orbit) = loud([1.0; super::MAX_ORBITS]);
        let (half, half_orbit) = loud([0.5; super::MAX_ORBITS]);
        assert!(full > 0.1, "the voice sounds: {full}");
        assert!(
            (half - full * 0.5).abs() < 1e-3,
            "half the fader, half the orbit: {half} vs {full}"
        );
        assert!(
            (half_orbit - full_orbit * 0.5).abs() < 1e-3,
            "the meter is post-fader"
        );
    }

    /// A voice whose slot was never filled is dropped at onset, as before;
    /// this is the missing-sample path, not a retirement.
    #[test]
    fn a_voice_with_no_slot_never_starts() {
        let mut backend = ScalarBackend::prepared(RATE, 4).expect("backend");
        let started = backend.try_note_prepared(sample_event(SampleId(9)));
        let _ = render(&mut backend);
        assert!(backend.voices.is_empty(), "started: {started}");
        assert!(render(&mut backend).iter().all(|sample| *sample == 0.0));
    }
}

#[cfg(test)]
mod sample_resampling_mode_tests {
    use crate::sample::{
        DecodedSample, SampleControls, SampleHold, SampleId, SampleResamplingMode,
    };
    use crate::{AudioBackend, OnsetEvent, ScalarBackend};

    #[test]
    fn selected_mode_changes_the_sample_voice_render_path() {
        const OUTPUT_RATE: u32 = 48_000;
        const SOURCE_RATE: u32 = 24_000;
        const FRAMES: usize = 128;
        let id = SampleId(9);

        let render = |mode: Option<SampleResamplingMode>| {
            let mut backend = ScalarBackend::prepared(OUTPUT_RATE, 1).expect("backend");
            if let Some(mode) = mode {
                backend.set_sample_resampling_mode(mode);
                assert_eq!(backend.sample_resampling_mode(), mode);
            }
            let pcm = (0..SOURCE_RATE / 10)
                .map(|frame| if frame % 2 == 0 { 0.0 } else { 0.2 })
                .collect();
            let decoded = DecodedSample::from_parts(SOURCE_RATE, 1, pcm).expect("sample");
            assert!(backend.install_sample(id, Box::new(decoded)).is_ok());
            let mut event = OnsetEvent::new(0, 440.0, 1.0, 0.1);
            event.sample = Some(SampleControls {
                sample: id,
                playback_rate: 1.0,
                begin: 0.0,
                end: 1.0,
                hold: SampleHold::Hap,
                muted: false,
                loop_secs: None,
                envelope_peak: 1.0,
                reversed: false,
                nudge_secs: 0.0,
                cut: None,
            });
            assert!(backend.try_note_prepared(event));
            let mut output = [0.0; FRAMES * 2];
            backend.process_block(&mut output, FRAMES);
            output
        };

        assert_eq!(
            ScalarBackend::default().sample_resampling_mode(),
            SampleResamplingMode::Linear
        );
        let linear = render(Some(SampleResamplingMode::Linear));
        let raw = render(Some(SampleResamplingMode::Raw));
        assert_eq!(render(None), linear, "the default retains the old sound");
        assert!(
            linear
                .iter()
                .zip(raw.iter())
                .any(|(a, b)| (a - b).abs() > 1e-4),
            "fractional source positions must produce different audio"
        );
        assert!(linear.iter().any(|sample| sample.abs() > 1e-4));
        assert!(raw.iter().any(|sample| sample.abs() > 1e-4));
    }
}

#[cfg(test)]
mod sample_lifetime_tests {
    use crate::{
        AudioBackend, AudioEvent, DecodedSample, OnsetEvent, SampleControls, SampleHold, SampleId,
        ScalarBackend, StaticBiquad, SynthSource, WavetableControls,
    };

    const RATE: u32 = 48_000;
    const BLOCK: usize = 128;
    const ID: SampleId = SampleId(9);

    fn sample_event() -> OnsetEvent {
        let mut event = OnsetEvent::new(1_024, 440.0, 1.0, 0.1).with_onset_lead(0.75);
        event.sample = Some(SampleControls {
            sample: ID,
            playback_rate: 1.0,
            begin: 0.0,
            end: 1.0,
            hold: SampleHold::Slice,
            muted: false,
            loop_secs: None,
            envelope_peak: 1.0,
            reversed: false,
            nudge_secs: 0.0,
            cut: None,
        });
        event
    }

    fn queued(event: OnsetEvent) -> AudioEvent {
        AudioEvent {
            onset_id: 1,
            generation: event.generation,
            ui_visuals: event.ui_visuals,
            target_frame: event.onset_frame,
            onset_lead: event.onset_lead,
            freq_hz: event.freq_hz,
            gain: event.gain,
            duration_secs: event.duration_secs,
            controls: event.controls,
            sample: event.sample,
            wavetable: event.wavetable,
            synth: event.synth,
            cut: event.cut,
        }
    }

    fn wavetable() -> WavetableControls {
        WavetableControls {
            table: ID,
            frame_len: 64,
            voices: 1.0,
            lfo_shape: 0,
            phaserand: 0.0,
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
        }
    }

    /// Render through the last complete block before the retention deadline.
    /// The voice must sound past `audible_after_secs`, then retire before its
    /// bank slot can be reclaimed.
    fn assert_rendered_lifetime(
        event: OnsetEvent,
        decoded: DecodedSample,
        audible_after_secs: f32,
    ) {
        let (id, end_frame) = queued(event).sample_end_frame(&decoded, RATE).unwrap();
        assert_eq!(id, ID);
        let audible_after = event.onset_frame + (audible_after_secs * RATE as f32).ceil() as u64;
        let mut backend = ScalarBackend::prepared(RATE, 4).expect("backend");
        assert!(backend.install_sample(id, Box::new(decoded)).is_ok());
        assert!(backend.try_note_prepared(event));
        let mut frame = 0;
        let mut sounded_late = false;
        let mut output = [0.0; BLOCK * 2];
        while frame + BLOCK as u64 <= end_frame {
            backend.process_block(&mut output, BLOCK);
            if frame >= audible_after && output.iter().any(|sample| sample.abs() > 1e-5) {
                sounded_late = true;
            }
            frame += BLOCK as u64;
        }
        assert!(sounded_late, "the fixture must exercise its long tail");
        assert!(
            backend.voices.is_empty(),
            "the voice must retire before sample retention expires at {end_frame}"
        );
    }

    #[test]
    fn sample_lifetime_covers_a_slow_reverse_slice_beyond_the_hap() {
        let decoded = DecodedSample::from_parts(44_100, 1, vec![0.5; 44_100]).unwrap();
        let mut event = sample_event();
        let sample = event.sample.as_mut().unwrap();
        sample.playback_rate = 0.25;
        sample.begin = 0.25;
        sample.end = 0.875;
        sample.reversed = true;
        sample.nudge_secs = 0.3;
        event.controls.envelope.release_secs = 0.05;
        let (_, end_frame) = queued(event).sample_end_frame(&decoded, RATE).unwrap();
        let lifetime = (end_frame - event.onset_frame) as f64 / f64::from(RATE);
        // A 5/8 slice of a one-second source at quarter speed gates for 2.5 s,
        // regardless of its 0.1 s source hap, then takes its release and ring.
        assert!((2.55..2.58).contains(&lifetime), "{lifetime}");
        assert_rendered_lifetime(event, decoded, 2.4);
    }

    #[test]
    fn sample_lifetime_keeps_a_nudged_natural_end_and_its_filter_tail() {
        let decoded = DecodedSample::from_parts(RATE, 1, vec![0.5; 9_600]).unwrap();
        let mut event = sample_event();
        event.duration_secs = 2.0;
        let sample = event.sample.as_mut().unwrap();
        sample.hold = SampleHold::Hap;
        sample.begin = 0.5;
        sample.playback_rate = 0.5;
        sample.nudge_secs = 0.3;
        event.controls.filters.lowpass = Some(StaticBiquad {
            frequency_hz: 800.0,
            q: 2.0,
        });
        let (_, end_frame) = queued(event).sample_end_frame(&decoded, RATE).unwrap();
        let lifetime = (end_frame - event.onset_frame) as f64 / f64::from(RATE);
        // The remaining 0.1 s takes 0.2 s at half speed, beginning at 0.3 s.
        assert!((0.515..0.52).contains(&lifetime), "{lifetime}");
        assert_rendered_lifetime(event, decoded, 0.5);
    }

    #[test]
    fn sample_lifetime_keeps_looped_zones_and_wavetables_through_release() {
        let decoded = DecodedSample::from_parts(RATE, 1, vec![0.5; 960]).unwrap();
        for use_wavetable in [false, true] {
            let mut event = sample_event();
            event.duration_secs = 0.4;
            event.controls.envelope.release_secs = 0.15;
            let sample = event.sample.as_mut().unwrap();
            sample.hold = SampleHold::Hap;
            sample.loop_secs = Some((0.001, 0.019));
            sample.nudge_secs = 0.05;
            if use_wavetable {
                event.sample = None;
                event.wavetable = Some(wavetable());
            }
            let (_, end_frame) = queued(event).sample_end_frame(&decoded, RATE).unwrap();
            let lifetime = (end_frame - event.onset_frame) as f64 / f64::from(RATE);
            assert!((0.565..0.57).contains(&lifetime), "{lifetime}");
            assert_rendered_lifetime(event, decoded.clone(), 0.45);
        }
    }

    #[test]
    fn sample_lifetime_saturates_uncertain_ends_and_follows_source_precedence() {
        let decoded = DecodedSample::from_parts(RATE, 1, vec![0.5; 64]).unwrap();
        let mut event = queued(sample_event());
        event.wavetable = Some(wavetable());
        event.sample.as_mut().unwrap().sample = SampleId(10);
        assert_eq!(event.sample_end_frame(&decoded, RATE).unwrap().0, ID);
        for duration in [f32::NAN, f32::INFINITY, f32::MAX] {
            event.duration_secs = duration;
            assert_eq!(event.sample_end_frame(&decoded, RATE), Some((ID, u64::MAX)));
        }
        event.duration_secs = 1.0;
        assert_eq!(event.sample_end_frame(&decoded, 0), Some((ID, u64::MAX)));
        event.target_frame = u64::MAX - 1;
        assert_eq!(event.sample_end_frame(&decoded, RATE), Some((ID, u64::MAX)));
        event.synth = Some(SynthSource::Input { channel: 0 });
        assert_eq!(event.sample_end_frame(&decoded, RATE), None);
        event.synth = None;
        event.wavetable = None;
        event.sample = None;
        assert_eq!(event.sample_end_frame(&decoded, RATE), None);
    }

    #[test]
    fn sample_lifetime_rounds_past_the_renderers_long_duration_clock() {
        let decoded = DecodedSample::from_parts(RATE, 1, vec![0.5; 64]).unwrap();
        let mut event = queued(sample_event());
        event.wavetable = Some(wavetable());
        // At this duration, f32 frame spacing is much larger than 128 frames.
        event.duration_secs = 1e10;
        let (_, end_frame) = event.sample_end_frame(&decoded, RATE).unwrap();
        let age = (end_frame - event.target_frame - BLOCK as u64) as f32 / RATE as f32;
        assert!(age >= event.duration_secs);
        assert!(end_frame < u64::MAX);
    }
}

#[cfg(test)]
mod sbd_signal_path_tests {
    use crate::{AudioBackend, OnsetEvent, ScalarBackend, SynthSource};

    fn event(gain: f32) -> OnsetEvent {
        let mut event = OnsetEvent::new(0, 43.653_53, gain, 2.0);
        event.synth = Some(SynthSource::Sbd {
            decay_secs: 0.5,
            pdecay_secs: 0.5,
            penv_semitones: 36.0,
            stop_secs: 0.51,
        });
        event
    }

    fn render(sample_rate: u32, gain: f32, frames: usize) -> Vec<f32> {
        let mut backend = ScalarBackend::prepared(sample_rate, 1).expect("SBD backend");
        assert!(backend.try_note_prepared(event(gain)));
        let mut output = vec![0.0; frames * 2];
        backend.process_block(&mut output, frames);
        output.into_iter().step_by(2).collect()
    }

    /// A takeover keeps an SBD voice whose graph already runs. The voice
    /// stands in for one copy of its onset from the newer generation, also
    /// when a changed tempo moved the copy.
    #[test]
    fn a_takeover_keeps_a_started_sbd_graph_and_names_its_onset() {
        const ONSET: u64 = 9_600;
        let reach = super::kept_onset_reach_frames(48_000);
        let mut hit = event(1.0);
        hit.onset_frame = ONSET;
        hit.generation = 1;
        let mut backend = ScalarBackend::prepared(48_000, 2).expect("backend");
        assert!(backend.try_note_prepared(hit));
        assert!(!backend.kept_onset_stands_for(ONSET, 2, true), "pending");
        // 150 ms: the graph is running, 50 ms before its source starts.
        let mut output = vec![0.0; 7_200 * 2];
        backend.process_block(&mut output, 7_200);
        backend.hand_over_from(2, 8_000, 7_200);
        assert_eq!(backend.voices.len(), 1, "a started graph is kept");
        assert!(!backend.kept_onset_stands_for(ONSET, 1, true), "its own");
        assert!(
            !backend.kept_onset_stands_for(ONSET, 2, false),
            "no SBD hit"
        );
        assert!(!backend.kept_onset_stands_for(ONSET + reach + 1, 2, true));
        assert!(backend.kept_onset_stands_for(ONSET + reach, 2, true));
        assert!(!backend.kept_onset_stands_for(ONSET, 2, true), "one event");
        // The next generation plays the onset again.
        backend.hand_over_from(3, 8_200, 7_200);
        assert!(backend.kept_onset_stands_for(ONSET - reach, 3, true));
    }

    /// The takeover gives an onset before its frame to the outgoing
    /// generation alone. The incoming generation has no copy of it, so the
    /// voice must not take the next hit of a roll.
    #[test]
    fn a_started_sbd_graph_before_the_takeover_stands_in_for_nothing() {
        const ONSET: u64 = 9_600;
        let mut hit = event(1.0);
        hit.onset_frame = ONSET;
        hit.generation = 1;
        let mut backend = ScalarBackend::prepared(48_000, 2).expect("backend");
        assert!(backend.try_note_prepared(hit));
        let mut output = vec![0.0; 7_200 * 2];
        backend.process_block(&mut output, 7_200);
        backend.hand_over_from(2, ONSET + 2, 7_200);
        assert!(!backend.kept_onset_stands_for(ONSET, 2, true));
        assert!(!backend.kept_onset_stands_for(ONSET + 1, 2, true));
        assert!(!backend.kept_onset_stands_for(ONSET + 200, 2, true));
        // Frame zero names no takeover.
        backend.hand_over_from(3, 0, 7_200);
        assert!(!backend.kept_onset_stands_for(ONSET, 3, true));
    }

    /// Only an SBD graph starts before its onset. A voice of another kind
    /// that started one frame before the takeover frame is not marked.
    #[test]
    fn a_started_voice_of_another_kind_one_frame_before_the_takeover_is_not_marked() {
        const ONSET: u64 = 9_600;
        let mut hit = OnsetEvent::new(ONSET, 220.0, 0.5, 1.0);
        hit.generation = 1;
        let mut backend = ScalarBackend::prepared(48_000, 2).expect("backend");
        assert!(backend.try_note_prepared(hit));
        let mut output = vec![0.0; 9_700 * 2];
        backend.process_block(&mut output, 9_700);
        backend.hand_over_from(2, ONSET + 1, 9_700);
        assert_eq!(backend.voices.len(), 1, "the voice has started");
        assert!(!backend.kept_onset_stands_for(ONSET + 1, 2, false));
    }

    /// A roll of hits 25 ms apart, moved 300 frames by the takeover. Each
    /// started graph takes the copy of its own hit. The copy of a hit that
    /// was still pending finds no voice and sounds.
    #[test]
    fn each_started_sbd_graph_of_a_roll_stands_in_for_its_own_hit() {
        const HITS: [u64; 4] = [9_600, 10_800, 12_000, 13_200];
        let mut backend = ScalarBackend::prepared(48_000, 4).expect("backend");
        for onset in HITS {
            let mut hit = event(1.0);
            hit.onset_frame = onset;
            hit.generation = 1;
            assert!(backend.try_note_prepared(hit));
        }
        let mut output = vec![0.0; 7_300 * 2];
        backend.process_block(&mut output, 7_300);
        backend.hand_over_from(2, 7_400, 7_300);
        assert_eq!(backend.voices.len(), 3, "the last graph has not started");
        assert_eq!(backend.pending.len(), 0);
        for (index, onset) in HITS.into_iter().enumerate() {
            let stands_in = backend.kept_onset_stands_for(onset - 300, 2, true);
            assert_eq!(stands_in, index < 3, "hit {index}");
            let waiting = backend
                .voices
                .iter()
                .filter(|voice| voice.replayed_by.is_some())
                .map(|voice| voice.start_frame);
            assert!(waiting.eq(HITS[(index + 1).min(3)..3].iter().copied()));
        }
    }

    #[test]
    fn zero_input_waveshaper_interpolates_the_precomputed_curve() {
        // The curve has `sampleRate` entries (not sampleRate + 1), so input
        // zero falls between two unequal entries and does not map to zero.
        for (sample_rate, expected) in [
            (44_100, -0.000_045_351_473_f32),
            (48_000, -0.000_041_666_666_f32),
        ] {
            let curve = super::sbd_saturation_curve(sample_rate);
            let actual = super::sbd_saturation(0.0, &curve);
            assert!(
                (actual - expected).abs() < 2e-10,
                "{sample_rate} Hz: expected {expected}, got {actual}"
            );
            // At the score's default 0.8 gain this becomes exactly the one
            // negative 16-bit step measured in the live device capture.
            assert_eq!((actual * 0.8 * 32_768.0).round() as i32, -1);
        }
    }

    #[test]
    fn tonal_body_keeps_the_pinned_signal_path() {
        const SAMPLE_RATE: u32 = 48_000;
        const CHECKPOINTS: [usize; 11] = [0, 1, 31, 95, 127, 128, 511, 1_199, 2_400, 9_600, 22_000];
        // Independently captured oscillator -> WaveShaper -> gain -> mix
        // checkpoints. Noise was omitted at the source, so it can be removed
        // from the generated signal below.
        const EXPECTED_BODY: [f32; 11] = [
            -0.000_041_666_666,
            0.058_261_58,
            0.943_600_9,
            -0.836_568_83,
            -0.824_552,
            -0.808_099_2,
            -0.932_327_7,
            -0.390_854_63,
            0.532_205_4,
            0.042_446_423,
            -0.002_196_277_7,
        ];

        let output = render(SAMPLE_RATE, 1.0, CHECKPOINTS[CHECKPOINTS.len() - 1] + 1);
        let mut noise = super::NoiseGen::at_onset(SAMPLE_RATE, 0.0);
        let mut checkpoint = 0;
        for (frame, sample) in output.into_iter().enumerate() {
            let t = frame as f32 / SAMPLE_RATE as f32;
            let tonal = sample - noise.next(2, 0.0) * super::sbd_noise_gain(t);
            if frame == CHECKPOINTS[checkpoint] {
                let expected = EXPECTED_BODY[checkpoint];
                assert!(
                    (tonal - expected).abs() < 6e-5,
                    "frame {frame}: expected {expected}, got {tonal}"
                );
                checkpoint += 1;
                if checkpoint == CHECKPOINTS.len() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod begin_frame_tests {
    /// A resampled buffer lands `begin` between samples; the read must
    /// resolve to a whole frame. Frame counts below are the real ones: the
    /// bundled 48 kHz bd is
    /// 12000 frames, and tr909's open hat is 34094 frames at 44.1 kHz, which
    /// becomes 37109.116 at 48 k.
    #[test]
    fn a_fractional_begin_resolves_to_a_whole_frame() {
        // 48 kHz source: already exact, and must not move.
        assert_eq!(super::begin_frame(0.2, 12_000.0, 1.0), 2_400.0);

        // Resampled source: every one of these was fractional, and the
        // measured corr tracked how far from a whole frame it sat.
        for begin in [0.05_f32, 0.1, 0.2, 0.25, 0.4, 0.5] {
            let landed = super::begin_frame(begin, 37_109.116, 1.0);
            assert_eq!(
                landed,
                landed.round(),
                "begin({begin}) must land on a whole frame, got {landed}"
            );
            let exact = f64::from(begin) * 37_109.116;
            assert!(
                (landed - exact).abs() <= 0.5,
                "begin({begin}) moved {} frames, further than rounding",
                (landed - exact).abs()
            );
        }
    }
}

#[cfg(test)]
mod compressor_tests {
    use crate::backend::CompressorControls;

    /// An impulse through `process` must come out exactly `delay_frames`
    /// later. This catches an off-by-one in the ring read or write index.
    #[test]
    fn an_impulse_emerges_exactly_predelay_frames_later() {
        for (rate, expected) in [
            (48_000.0_f32, 288_usize),
            (44_100.0, 264),
            (192_000.0, 1023),
        ] {
            let controls = CompressorControls {
                threshold_db: -20.0,
                ratio: 10.0,
                knee_db: 20.0,
                attack_secs: 0.002,
                release_secs: 0.02,
            };
            let mut state = super::CompressorState::new(rate, &controls, 0);
            let mut first = None;
            for frame in 0..(expected + 64) {
                let input = if frame == 0 { 1.0 } else { 0.0 };
                let (left, _) = state.process(input, input, &controls, rate);
                if first.is_none() && left.abs() > 1e-9 {
                    first = Some(frame);
                }
            }
            assert_eq!(
                first,
                Some(expected),
                "at {rate} Hz the impulse should surface at frame {expected}"
            );
        }
    }

    /// The compressor pre-delay is in frames (`preDelayTime * sampleRate`,
    /// truncated). It does not round up to a render quantum: the correct
    /// delay here is 288 frames, not 384.
    #[test]
    fn predelay_is_frames_not_whole_quanta() {
        for (rate, expected) in [
            (48_000.0_f32, 288_usize),
            (44_100.0, 264),
            (96_000.0, 576),
            // The pre-delay clamps at 1023 frames, so these two share a
            // ceiling instead of growing: 0.006·176400 = 1058 and
            // 0.006·192000 = 1152 both land on 1023.
            (176_400.0, 1023),
            (192_000.0, 1023),
        ] {
            let controls = CompressorControls {
                threshold_db: -20.0,
                ratio: 10.0,
                knee_db: 20.0,
                attack_secs: 0.002,
                release_secs: 0.02,
            };
            let state = super::CompressorState::new(rate, &controls, 0);
            assert_eq!(
                state.delay_frames, expected,
                "pre-delay at {rate} Hz should be {expected} frames (0.006 s)"
            );
            assert!(
                !state.delay_frames.is_multiple_of(128),
                "0.006 s is not a whole number of quanta at {rate} Hz; \
                 landing on one means the rounding came back"
            );
        }
    }
}

#[cfg(test)]
mod compressor_preroll_tests {
    use crate::backend::CompressorControls;

    /// `new` replays the silence between graph build and the note's onset.
    /// The replay must be bit-identical to processing that silence,
    /// including the f32 stall an ulp short of the limits.
    #[test]
    fn a_prerolled_compressor_matches_one_that_lived_through_the_silence() {
        let rate = 48_000.0_f32;
        let controls = CompressorControls {
            threshold_db: -100.0, // the deep threshold is the sensitive case
            ratio: 10.0,
            knee_db: 10.0,
            attack_secs: 0.002,
            release_secs: 0.02,
        };
        for silence in [32u64, 800, 24_000, 100_000] {
            let mut lived = super::CompressorState::new(rate, &controls, 0);
            for _ in 0..silence {
                lived.process(0.0, 0.0, &controls, rate);
            }
            let mut prerolled = super::CompressorState::new(rate, &controls, silence);
            // Feed both a burst and compare every output sample.
            for frame in 0..2_000u64 {
                let x = if frame % 700 < 350 { 0.9 } else { 0.001 };
                let a = lived.process(x, x, &controls, rate);
                let b = prerolled.process(x, x, &controls, rate);
                assert_eq!(
                    a, b,
                    "after {silence} frames of silence, outputs split at frame {frame}"
                );
            }
        }
    }
}

#[cfg(test)]
mod modulator_tests {
    use super::{env_mod_value, env_mod_warp, lfo_waveshape};
    use crate::backend::{EnvMod, ModTarget};

    fn env(a: f32, d: f32, s: f32, r: f32, sus_time: f32) -> EnvMod {
        EnvMod {
            fxi: None,
            target: ModTarget::LowpassFreq,
            attack_secs: a,
            decay_secs: d,
            sustain: s,
            release_secs: r,
            a_curve: 0.0,
            d_curve: 0.0,
            r_curve: 0.0,
            depth: 1.0,
            min: -1e9,
            max: 1e9,
            sustain_secs: sus_time,
            param_base: 500.0,
            id: None,
        }
    }

    #[test]
    fn waveshapes_match_worklets() {
        // tri peaks at skew, hits 0 at both ends
        assert_eq!(lfo_waveshape(0, 0.0, 0.5), 0.0);
        assert_eq!(lfo_waveshape(0, 0.5, 0.5), 1.0);
        assert!((lfo_waveshape(0, 1.0, 0.5)).abs() < 1e-6);
        // skewed tri: peak moves to skew
        assert_eq!(lfo_waveshape(0, 0.25, 0.25), 1.0);
        // sine is 0.5 at phase 0, 1 at quarter
        assert!((lfo_waveshape(1, 0.0, 0.5) - 0.5).abs() < 1e-6);
        assert!((lfo_waveshape(1, 0.25, 0.5) - 1.0).abs() < 1e-6);
        // ramp/saw are complements
        assert_eq!(lfo_waveshape(2, 0.3, 0.5), 0.3);
        assert_eq!(lfo_waveshape(3, 0.3, 0.5), 0.7);
        // square: 1 below skew, 0 at/above
        assert_eq!(lfo_waveshape(4, 0.49, 0.5), 1.0);
        assert_eq!(lfo_waveshape(4, 0.5, 0.5), 0.0);
    }

    #[test]
    fn env_warp_is_identity_at_zero_curvature() {
        for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
            assert!((env_mod_warp(p, 0.0) - p).abs() < 1e-6);
        }
        // positive curvature = snappier (above identity mid-segment)
        assert!(env_mod_warp(0.5, 0.5) > 0.5);
        assert!(env_mod_warp(0.5, -0.5) < 0.5);
    }

    #[test]
    fn env_value_uses_cumulative_time_divisors() {
        // a=1 d=1 s=0.5, susTime=10: the envelope divides elapsed by the
        // CUMULATIVE threshold, so decay enters mid-phase with a jump.
        let e = env(1.0, 1.0, 0.5, 0.1, 10.0);
        assert!((env_mod_value(&e, 0.5) - 0.5).abs() < 1e-6); // attack midpoint
        // just after entering decay: phase = 1.0/(1+1) = 0.5 of the way
        // from 1 toward 0.5 already
        let just_in_decay = env_mod_value(&e, 1.0001);
        assert!((just_in_decay - 0.75).abs() < 1e-3);
        // decay endpoint lands on sustain
        assert!((env_mod_value(&e, 1.9999) - 0.5).abs() < 1e-3);
        // sustain holds to susTime
        assert!((env_mod_value(&e, 5.0) - 0.5).abs() < 1e-6);
        // past susTime+release: idle 0
        assert_eq!(env_mod_value(&e, 10.2), 0.0);
    }

    #[test]
    fn notch_kills_center_and_passes_extremes() {
        use super::{NotchState, notch_coefs};
        let sr = 48_000.0;
        let c = notch_coefs(1_000.0, 1.0, sr);
        let mut state = NotchState::default();
        // Drive a 1 kHz sine through the notch: steady-state output ~0.
        let mut tail = 0.0f32;
        for n in 0..48_000 {
            let x = (std::f32::consts::TAU * 1_000.0 * n as f32 / sr).sin();
            let y = state.process(&c, x);
            if n > 40_000 {
                tail = tail.max(y.abs());
            }
        }
        assert!(tail < 0.01, "notch leaked {tail} at center");
        // A 100 Hz sine passes nearly unscathed.
        let mut state = NotchState::default();
        let mut tail = 0.0f32;
        for n in 0..48_000 {
            let x = (std::f32::consts::TAU * 100.0 * n as f32 / sr).sin();
            let y = state.process(&c, x);
            if n > 40_000 {
                tail = tail.max(y.abs());
            }
        }
        assert!(tail > 0.9, "notch attenuated far-field to {tail}");
        // Frequency at the edges is a pass-through.
        let edge = notch_coefs(0.0, 1.0, sr);
        assert_eq!(edge.b0, 1.0);
        assert_eq!(edge.a1, 0.0);
    }

    #[test]
    fn env_zero_attack_snaps_to_peak_then_decays() {
        let e = env(0.0, 1.0, 0.0, 0.1, 10.0);
        // t inside decay, cumulative divisor is a+d = 1
        assert!((env_mod_value(&e, 0.5) - 0.5).abs() < 1e-3);
        assert!(env_mod_value(&e, 0.9999) < 1e-3);
    }
}

#[cfg(test)]
mod wavetable_boundary_tests {
    use super::*;

    #[test]
    fn wavetable_warp_reads_stay_within_the_table_at_cycle_boundaries() {
        // A steep first edge exposes extrapolation immediately. Exercise the
        // real portable oscillator, with two frames and frame crossfading.
        let pcm: Vec<f32> = (0..4096)
            .map(|i| if i % 2 == 0 { -0.75 } else { 0.75 } * if i < 2048 { 1.0 } else { 0.5 })
            .collect();
        let mut bank = SampleBank::empty();
        bank.install(
            crate::SampleId(9),
            Box::new(crate::DecodedSample::from_parts(48000, 1, pcm.clone()).unwrap()),
        )
        .unwrap();
        let mut controls = crate::WavetableControls {
            table: crate::SampleId(9),
            frame_len: 2048,
            voices: 1.0,
            lfo_shape: 0,
            phaserand: 0.0,
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
        };
        for mode in 0..=21 {
            controls.warp_mode = mode;
            let mode = crate::warp::WarpMode::from_index(i32::from(mode));
            for amount in [0.0, 0.25, 0.5, 0.75, 1.0] {
                controls.warp = amount;
                for position in [0.0, 0.37, 1.0] {
                    controls.position = position;
                    for step in 0..=2048 {
                        let phase = step as f32 / 2048.0;
                        let warped = crate::warp::warp_phase(phase, amount, mode);
                        // Independent circular interpolation: wrap the entire
                        // position before selecting either sample or fraction.
                        let pos = (f64::from(warped) * 2048.0).rem_euclid(2048.0);
                        let index = pos.floor() as usize;
                        let fraction = (pos - pos.floor()) as f32;
                        let mut expected =
                            pcm[index] * (1.0 - fraction) + pcm[(index + 1) % 2048] * fraction;
                        expected *= 1.0 - position * 0.5;
                        if mode.flips_sample(phase, amount) {
                            expected = -expected;
                        }
                        expected *= 0.5_f32.sqrt();
                        let mut phases = [f64::from(phase); MAX_UNISON];
                        let (left, right) = ScalarBackend::wavetable_sample(
                            &bank,
                            &controls,
                            &mut phases,
                            &mut None,
                            &mut None,
                            0.0,
                            440.0,
                            0.0,
                            1.0,
                            48000.0,
                            WavetableAdds::default(),
                            None,
                        );
                        assert!(
                            (left - expected).abs() < 1e-5 && (right - expected).abs() < 1e-5,
                            "mode={mode:?} amount={amount} position={position} phase={phase}: {left} expected {expected}"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod insert_routing_tests {
    use super::*;
    use crate::insert::{InsertControls, InsertKey, InsertNote, InsertParam, OrbitInsert};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StatefulInsert {
        key: InsertKey,
        memory: [f32; 2],
        held: u32,
        resets: Arc<AtomicUsize>,
    }

    impl OrbitInsert for StatefulInsert {
        fn key(&self) -> InsertKey {
            self.key
        }

        fn set_param(&mut self, _param: InsertParam, _frames: u32) {}

        fn note(&mut self, note: InsertNote, frames: u32) {
            assert_eq!(frames, 0);
            self.held = note.frames;
        }

        fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
            for (left, right) in left.iter_mut().zip(right) {
                let instrument = if self.held > 0 { 0.125 } else { 0.0 };
                self.held = self.held.saturating_sub(1);
                for (channel, sample) in [left, right].into_iter().enumerate() {
                    self.memory[channel] = *sample + instrument + self.memory[channel] * 0.5;
                    *sample = self.memory[channel];
                }
            }
        }

        fn reset(&mut self) {
            self.resets.fetch_add(1, Ordering::Relaxed);
            self.memory = [0.0; 2];
            self.held = 0;
        }
    }

    #[test]
    fn an_orbit_move_keeps_the_insert_state_and_old_native_delay() {
        for instrument in [false, true] {
            let key = InsertKey {
                plugin: 7,
                preset: 0,
            };
            let builds = Arc::new(AtomicUsize::new(0));
            let resets = Arc::new(AtomicUsize::new(0));
            let make_backend = |moved: bool| {
                let mut backend = ScalarBackend::prepared(48_000, 16).unwrap();
                backend.orbit_pairs[2] = u8::from(moved);
                let builds = Arc::clone(&builds);
                let resets = Arc::clone(&resets);
                backend.set_insert_provider(Arc::new(move |wanted, _, _| {
                    assert_eq!(wanted, key);
                    builds.fetch_add(1, Ordering::Relaxed);
                    Some(Box::new(StatefulInsert {
                        key,
                        memory: [0.0; 2],
                        held: 0,
                        resets: Arc::clone(&resets),
                    }))
                }));
                for (frame, orbit) in [(0, 1), (639, 2), (1024, 1), (1536, 2)] {
                    let mut event = OnsetEvent::new(frame, 440.0, 0.25, 0.04);
                    event.controls.orbit = if moved { orbit } else { 1 };
                    event.controls.insert_orbit = Some(1);
                    let mut insert = InsertControls::new(key);
                    if instrument {
                        if frame == 0 {
                            insert = insert.with_note(InsertNote {
                                pitch: 69.0,
                                velocity: 0.5,
                                frames: 768,
                            });
                        }
                        event.controls.instrument = Some(insert);
                    } else {
                        event.controls.effects[0] = Some(insert);
                    }
                    if frame == 0 && !instrument {
                        event.controls.delay = Some(crate::backend::DelayControls {
                            wet: 0.3,
                            time_secs: 256.0 / 48_000.0,
                            feedback: 0.5,
                        });
                    } else if frame != 0 {
                        event.gain = 0.0;
                    }
                    backend.note(event);
                }
                backend
            };
            let mut baseline = make_backend(false);
            let mut moved = make_backend(true);
            let slot = if instrument {
                crate::insert::instrument_slot(1)
            } else {
                crate::insert::effect_slot(1, 0)
            };
            let mut identity = None;
            let mut expected = [0.0; 256];
            let mut actual = [0.0; 256];
            for block in 0..16 {
                baseline.process_block(&mut expected, 128);
                moved.process_block(&mut actual, 128);
                for (actual, routed) in actual.iter_mut().zip(moved.orbit_mix(2, 128)) {
                    *actual += routed;
                }
                let pointer =
                    moved.orbit_inserts[slot].as_deref().unwrap() as *const dyn OrbitInsert;
                let pointer = pointer as *const ();
                assert_eq!(*identity.get_or_insert(pointer), pointer);
                for (expected, actual) in expected.iter().zip(actual) {
                    assert!((expected - actual).abs() < 1e-6, "block {block}");
                }
                assert_eq!(moved.orbit_delays[1].left, baseline.orbit_delays[1].left);
                assert_eq!(moved.orbit_delays[1].right, baseline.orbit_delays[1].right);
                assert!(!moved.orbit_delays[2].active);
                if block == 3 {
                    assert_eq!(moved.insert_outputs[1], 1);
                    assert!(
                        moved.orbit_blocks[2]
                            .iter()
                            .flatten()
                            .all(|value| *value == 0.0)
                    );
                }
                if block == 4 {
                    assert_eq!(moved.insert_outputs[1], 2);
                    // Route selection follows block activation: this onset
                    // at 639 moves the old sound from frame 512, 127 early.
                    assert!(moved.orbit_mix(2, 128)[0].abs() > 0.01);
                    assert!(
                        moved.orbit_blocks[2]
                            .iter()
                            .flatten()
                            .any(|value| value.abs() > 0.01)
                    );
                    if !instrument {
                        assert!(
                            moved.orbit_blocks[1]
                                .iter()
                                .flatten()
                                .any(|value| value.abs() > 0.001)
                        );
                        let old = moved
                            .voices
                            .iter()
                            .find(|voice| voice.start_frame == 0)
                            .unwrap();
                        assert_eq!((old.orbit, old.insert_orbit), (1, 1));
                    }
                }
                if block == 8 {
                    assert_eq!(moved.insert_outputs[1], 1);
                }
            }
            assert_eq!(builds.load(Ordering::Relaxed), 2);
            assert_eq!(resets.load(Ordering::Relaxed), 0);
            assert_eq!(moved.missing_insert_events(), 0);
        }

        // SBD connects its graph 100 ms before its source onset. The insert
        // follows the same graph while the oscillator phase remains parked.
        let mut backend = ScalarBackend::prepared(48_000, 1).unwrap();
        let key = InsertKey {
            plugin: 7,
            preset: 0,
        };
        backend.set_insert_provider(Arc::new(move |_, _, _| {
            Some(Box::new(StatefulInsert {
                key,
                memory: [0.0; 2],
                held: 0,
                resets: Arc::new(AtomicUsize::new(0)),
            }))
        }));
        let mut event = OnsetEvent::new(6336, 43.653_53, 0.25, 1.0);
        event.synth = Some(crate::backend::SynthSource::Sbd {
            decay_secs: 0.5,
            pdecay_secs: 0.5,
            penv_semitones: 36.0,
            stop_secs: 0.51,
        });
        event.controls.orbit = 2;
        event.controls.insert_orbit = Some(1);
        event.controls.effects[0] = Some(InsertControls::new(key));
        backend.note(event);
        let mut output = [0.0; 256];
        for _ in 0..12 {
            backend.process_block(&mut output, 128);
        }
        assert_eq!(backend.insert_outputs[1], 1);
        backend.process_block(&mut output, 128);
        assert_eq!(backend.insert_outputs[1], 2);
        let voice = &backend.voices[0];
        assert_eq!(voice.graph_start_frame, 1536);
        assert_eq!(voice.start_frame - voice.graph_start_frame, 4800);
        assert!(matches!(&voice.source, VoiceSource::Sbd { phase, .. } if *phase == 0.0));
    }
}
