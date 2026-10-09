//! The stop ramp of the live backend.
//!
//! A stop ramps the running voices to zero over 10 ms. Most host blocks are
//! shorter than the ramp, so the ramp must carry across blocks.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rustel_audio::{
    AudioEvent, Envelope, LiveFlipAtomics, LiveScalarBackend, OscillatorControls, Ring,
};

struct Host {
    live: LiveScalarBackend,
    ring: Ring,
    generation: AtomicU64,
    takeover_frame: AtomicU64,
    takeover_cut: AtomicU64,
    line_arm: AtomicU64,
    stopped: AtomicBool,
    sample_rate: u32,
    block_frames: usize,
    frame: u64,
    next_onset: u64,
}

impl Host {
    fn new(sample_rate: u32, block_frames: usize) -> Self {
        Self {
            live: LiveScalarBackend::new(sample_rate, 8).expect("live backend"),
            ring: Ring::new(16),
            generation: AtomicU64::new(1),
            takeover_frame: AtomicU64::new(0),
            takeover_cut: AtomicU64::new(0),
            line_arm: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            sample_rate,
            block_frames,
            frame: 0,
            next_onset: 1,
        }
    }

    /// Frames of the 10 ms ramp at this sample rate.
    fn ramp_frames(&self) -> usize {
        (self.sample_rate / 100) as usize
    }

    /// Queue a 1 kHz tone of 4 s which holds one level after 2 ms.
    fn tone_at(&mut self, target_frame: u64) {
        assert!(self.ring.push(AudioEvent {
            onset_id: self.next_onset,
            generation: 1,
            ui_visuals: 0,
            target_frame,
            onset_lead: 0.0,
            freq_hz: 1000.0,
            gain: 0.5,
            duration_secs: 4.0,
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
        self.next_onset += 1;
    }

    /// Render one block. Returns the larger of the two channels per frame.
    fn block(&mut self) -> Vec<f32> {
        let mut output = vec![0.0_f32; self.block_frames * 2];
        self.live.process_block_with(
            &mut output,
            self.block_frames,
            self.frame,
            &self.ring,
            LiveFlipAtomics {
                generation: &self.generation,
                takeover_frame: &self.takeover_frame,
                takeover_cut: &self.takeover_cut,
                line_arm: &self.line_arm,
            },
            &self.stopped,
        );
        self.frame += self.block_frames as u64;
        assert!(output.iter().all(|sample| sample.is_finite()));
        let (frames, _) = output.as_chunks::<2>();
        frames
            .iter()
            .map(|[left, right]| left.abs().max(right.abs()))
            .collect()
    }

    /// Render blocks until at least `frames` frames are out.
    fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut out = Vec::new();
        while out.len() < frames {
            out.extend(self.block());
        }
        out
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    fn resume(&self) {
        self.stopped.store(false, Ordering::Release);
    }
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().copied().fold(0.0, f32::max)
}

/// A host with a tone at its level, and the level.
fn playing(sample_rate: u32, block_frames: usize) -> (Host, f32) {
    let mut host = Host::new(sample_rate, block_frames);
    host.tone_at(0);
    host.render(sample_rate as usize / 10);
    let level = peak(&host.render(sample_rate as usize / 100));
    assert!(level > 0.01, "the tone does not sound: {level}");
    (host, level)
}

#[test]
fn a_stop_ramps_over_ten_milliseconds_at_every_block_size() {
    for (sample_rate, block_frames) in [
        (48_000, 128),
        (48_000, 32),
        (48_000, 1024),
        (44_100, 128),
        (96_000, 128),
    ] {
        let case = format!("{sample_rate} Hz, blocks of {block_frames}");
        let (mut host, level) = playing(sample_rate, block_frames);
        let ramp = host.ramp_frames();
        assert!(!host.live.stop_ramp_complete(), "{case}");

        host.stop();
        let stopped = host.render(ramp * 2);
        assert!(host.live.stop_ramp_complete(), "{case}");

        // The last ramp frame is zero and the output stays silent.
        assert_eq!(peak(&stopped[ramp - 1..]), 0.0, "{case}");

        // Each quarter of the ramp still sounds, below the level the ramp
        // allows at the start of the quarter.
        for (index, quarter) in stopped[..ramp].chunks(ramp / 4).take(4).enumerate() {
            let allowed = level * (1.0 - index as f32 / 4.0);
            let heard = peak(quarter);
            assert!(heard > 0.0, "{case}: quarter {index} of the ramp is silent");
            assert!(
                heard <= allowed * 1.001,
                "{case}: quarter {index} peaks at {heard}, above {allowed}"
            );
        }
    }
}

#[test]
fn the_voices_are_gone_after_a_stop_and_the_clock_is_aligned() {
    let (mut host, _) = playing(48_000, 128);
    host.stop();
    host.render(1024);

    // The 4 s tone does not return.
    host.resume();
    assert_eq!(peak(&host.render(1024)), 0.0);

    // A new tone due now sounds in its first block.
    host.tone_at(host.frame);
    assert!(peak(&host.block()) > 0.01);
}

#[test]
fn a_stop_shorter_than_the_ramp_still_ramps_to_the_end() {
    let (mut host, level) = playing(48_000, 128);
    let ramp = host.ramp_frames();
    host.stop();
    let first = host.block();
    assert!(!host.live.stop_ramp_complete());

    // The flag clears after one block. The ramp goes on from the same
    // position, reaches zero, and the tone does not return.
    host.resume();
    let rest = host.render(1024);
    let left = ramp - first.len();
    assert!(peak(&rest[..left]) > 0.0);
    assert!(rest[0] <= level * (1.0 - first.len() as f32 / ramp as f32));
    assert_eq!(peak(&rest[left - 1..]), 0.0);
}

#[test]
fn an_onset_pending_at_a_stop_does_not_start_under_the_ramp() {
    let mut host = Host::new(48_000, 128);
    // The backend admits the tone one block before the tone is due.
    host.tone_at(200);
    assert_eq!(peak(&host.block()), 0.0);

    // With no voice to fade, the ramp still runs to its end.
    host.stop();
    assert_eq!(peak(&host.render(1024)), 0.0);
    assert!(host.live.stop_ramp_complete());
    host.resume();
    assert_eq!(peak(&host.render(1024)), 0.0);
}
