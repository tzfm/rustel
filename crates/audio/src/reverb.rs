//! The `room` reverb: generated-impulse convolution, per orbit.
//!
//! - IR synthesis: stereo noise decaying `(1/1000)^(j/decayFrames)` over
//!   `1.5 × decayTime`, a linear fade-in over `fadeInTime`, then ONE lowpass
//!   biquad whose frequency ramps linearly from `lpFreqStart` (roomlp) to
//!   `lpFreqEnd` (roomdim) across `decayTime` and holds - Q is 0.0001 dB,
//!   the same coefficient formulas `crate::biquad` uses.
//! - One reverb per orbit, regenerated only when parameters change; the send
//!   taps post-pan (like the delay send) scaled by `room`, and the return
//!   joins the orbit's summing node - INSIDE the duck gain.
//! - The convolution follows the Web Audio ConvolverNode contract for a
//!   stereo input with a stereo IR: each channel convolves independently.
//!
//! The IR noise is a fixed splitmix64 stream, so renders are deterministic;
//! a random-seeded tail would be unreproducible even against itself. The
//! decay/lowpass ENVELOPE is what defines the sound.
//!
//! Real-time shape: everything here that allocates (IR synthesis, FFT
//! planning, partition spectra) runs at GENERATE time on a producer thread
//! (or inline for offline renders). [`OrbitReverb::process_block`] performs
//! only preplanned transforms into preallocated scratch.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::DspDispatch;
use crate::convolution_kernel::ConvolutionKernel;

/// The render quantum the partitioned convolution is built around - the
/// same 128 frames as WebAudio's graph quantum.
pub const REVERB_BLOCK: usize = 128;
const FFT_SIZE: usize = 256;

/// `roomsize`/`roomfade`/`roomlp`/`roomdim`, with the generate defaults
/// (2 / 0.1 / 15000 / 1000) already applied by the resolver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReverbParams {
    pub size_secs: f32,
    pub fade_secs: f32,
    pub lp_start_hz: f32,
    pub lp_end_hz: f32,
    /// Custom impulse response (`ir`/`irspeed`/`irbegin`); `None` = the
    /// generated IR.
    pub ir: Option<IrParams>,
}

/// A custom IR reference: the decoded sample slot plus the
/// `irspeed`/`irbegin` read transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IrParams {
    pub sample: crate::sample::SampleId,
    pub speed: f32,
    pub begin: f32,
}

/// Largest `roomsize` accepted: the partition count (and with it the
/// callback's multiply-accumulate cost) grows linearly with the tail.
pub const MAX_REVERB_SECONDS: f32 = 10.0;

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z ^= z >> 30;
    z = z.wrapping_mul(0xBF58476D1CE4E5B9);
    z ^= z >> 27;
    z = z.wrapping_mul(0x94D049BB133111EB);
    z ^= z >> 31;
    z
}

/// Uniform noise in [-1, 1) on the deterministic stream.
fn noise_sample(state: &mut u64) -> f32 {
    ((splitmix64(state) >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}

/// One lowpass biquad in Direct Form 1, coefficients recomputed every
/// sample - how Web Audio runs a biquad whose frequency is under a linear
/// ramp, which the IR's gradual lowpass is.
struct RampedLowpass {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl RampedLowpass {
    fn new() -> Self {
        Self {
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn process(&mut self, x: f64, frequency_hz: f64, sample_rate: f64) -> f64 {
        // biquad.cc lowpass with Q in dB (0.0001), normalized frequency.
        let frequency = (frequency_hz / (sample_rate * 0.5)).clamp(0.0, 1.0);
        let (b0, b1, b2, a1, a2) = if frequency == 1.0 {
            (1.0, 0.0, 0.0, 0.0, 0.0)
        } else if frequency > 0.0 {
            let resonance = 10.0f64.powf(0.0001 / 20.0);
            let theta = std::f64::consts::PI * frequency;
            let alpha = theta.sin() / (2.0 * resonance);
            let cosine = theta.cos();
            let beta = (1.0 - cosine) * 0.5;
            let a0 = 1.0 + alpha;
            (
                beta / a0,
                2.0 * beta / a0,
                beta / a0,
                -2.0 * cosine / a0,
                (1.0 - alpha) / a0,
            )
        } else {
            (0.0, 0.0, 0.0, 0.0, 0.0)
        };
        let y = b0 * x + b1 * self.x1 + b2 * self.x2 - a1 * self.y1 - a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// One channel of the generated impulse response.
fn generate_channel(sample_rate: u32, params: ReverbParams, noise: &mut u64) -> Vec<f32> {
    let sr = f64::from(sample_rate);
    let decay_time = f64::from(params.size_secs.clamp(0.001, MAX_REVERB_SECONDS));
    let total_time = decay_time * 1.5;
    let decay_frames = (decay_time * sr).round().max(1.0);
    let total_frames = (total_time * sr).round() as usize;
    let fade_frames = (f64::from(params.fade_secs.max(0.0)) * sr).round() as usize;
    // 60 dB is a factor of 1000 in amplitude.
    let decay_base = (1.0f64 / 1000.0).powf(1.0 / decay_frames);

    let mut channel: Vec<f32> = Vec::with_capacity(total_frames);
    let mut envelope = 1.0f64;
    for _ in 0..total_frames {
        channel.push((f64::from(noise_sample(noise)) * envelope) as f32);
        envelope *= decay_base;
    }
    for (j, value) in channel.iter_mut().enumerate().take(fade_frames) {
        *value *= j as f32 / fade_frames as f32;
    }

    // applyGradualLowpass: skipped entirely when lpFreqStart is 0.
    if params.lp_start_hz != 0.0 {
        let lp_start = f64::from(params.lp_start_hz).min(sr / 2.0);
        let lp_end = f64::from(params.lp_end_hz).min(sr / 2.0);
        let mut filter = RampedLowpass::new();
        for (j, value) in channel.iter_mut().enumerate() {
            let t = j as f64 / sr;
            let frequency = if t >= decay_time {
                lp_end
            } else {
                lp_start + (lp_end - lp_start) * (t / decay_time)
            };
            *value = filter.process(f64::from(*value), frequency, sr) as f32;
        }
    }
    channel
}

/// Where the head stops and the tail begins, in samples of impulse response.
///
/// The head must stay at [`REVERB_BLOCK`] so a note can be heard through the
/// reverb within one 128-frame callback. Everything past this point is tail:
/// it is already delayed by more than a tail block, so a tail block's own
/// latency costs nothing, and partitioning it coarsely is free accuracy-wise.
const TAIL_START: usize = 2048;

/// Tail partition size. Cost per sample falls as this grows - a 2-second room
/// is 750 head-sized partitions but only 46 tail-sized ones - while the FFT
/// grows only as `log`. 2048 puts a 2-second room near a hundred complex
/// multiply-accumulates per sample instead of fifteen hundred.
const TAIL_BLOCK: usize = TAIL_START;
const TAIL_FFT: usize = 2 * TAIL_BLOCK;

/// The tail half of a non-uniform partitioned convolution.
///
/// Handles impulse-response offsets `[TAIL_START, end)` only. Because every
/// offset it owns is at least one tail block old, a hop can compute the NEXT
/// block of output from input it already has: partition `k` here is IR block
/// `k + 1` in tail-block units, and skipping block 0 is exactly what removes
/// the latency that would otherwise make a coarse partition unusable.
struct TailConvolver {
    kernel: ConvolutionKernel,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    partitions: Vec<Vec<Complex<f32>>>,
    history: Vec<Vec<Complex<f32>>>,
    head: usize,
    previous_block: Vec<f32>,
    stage: Vec<f32>,
    stage_fill: usize,
    work: Vec<Complex<f32>>,
    accumulator: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Output for the block now being played, produced one hop ahead.
    wet: Vec<f32>,
    wet_read: usize,
}

impl TailConvolver {
    fn approx_bytes(&self) -> usize {
        let complex = std::mem::size_of::<Complex<f32>>();
        let spectra: usize = self
            .partitions
            .iter()
            .chain(self.history.iter())
            .map(Vec::len)
            .sum();
        spectra * complex
            + (self.work.len() + self.accumulator.len() + self.scratch.len()) * complex
            + (self.previous_block.len() + self.stage.len()) * std::mem::size_of::<f32>()
    }

    /// `None` when the impulse response is too short to have a tail.
    fn new(ir: &[f32], planner: &mut FftPlanner<f32>, kernel: ConvolutionKernel) -> Option<Self> {
        let tail = ir.get(TAIL_START..)?;
        if tail.is_empty() {
            return None;
        }
        let fft = planner.plan_fft_forward(TAIL_FFT);
        let ifft = planner.plan_fft_inverse(TAIL_FFT);
        let scratch_len = fft
            .get_inplace_scratch_len()
            .max(ifft.get_inplace_scratch_len());
        let mut scratch = vec![Complex::default(); scratch_len];
        let mut partitions = Vec::with_capacity(tail.len().div_ceil(TAIL_BLOCK));
        for chunk in tail.chunks(TAIL_BLOCK) {
            let mut spectrum = vec![Complex::default(); TAIL_FFT];
            for (bin, sample) in spectrum.iter_mut().zip(chunk) {
                bin.re = *sample;
            }
            fft.process_with_scratch(&mut spectrum, &mut scratch);
            partitions.push(spectrum);
        }
        let history = vec![vec![Complex::default(); TAIL_FFT]; partitions.len()];
        Some(Self {
            kernel,
            fft,
            ifft,
            partitions,
            history,
            head: 0,
            previous_block: vec![0.0; TAIL_BLOCK],
            stage: vec![0.0; TAIL_BLOCK],
            stage_fill: 0,
            work: vec![Complex::default(); TAIL_FFT],
            accumulator: vec![Complex::default(); TAIL_FFT],
            scratch,
            wet: vec![0.0; TAIL_BLOCK],
            wet_read: 0,
        })
    }

    fn reset(&mut self) {
        for spectrum in &mut self.history {
            spectrum.fill(Complex::default());
        }
        self.previous_block.fill(0.0);
        self.stage_fill = 0;
        self.wet.fill(0.0);
        self.wet_read = 0;
        self.head = 0;
    }

    /// Take one input sample and give back this sample's tail contribution.
    #[inline]
    fn step(&mut self, sample: f32) -> f32 {
        let wet = self.wet[self.wet_read];
        self.wet_read += 1;
        self.stage[self.stage_fill] = sample;
        self.stage_fill += 1;
        if self.stage_fill == TAIL_BLOCK {
            self.run_hop();
            self.wet_read = 0;
        }
        wet
    }

    /// Advance the tail's staging without transforming anything.
    ///
    /// Called when the head has established that the whole response is
    /// multiplying silence. The tail must still consume its share of the
    /// sample clock, or it would fall out of step with the head the moment
    /// audio returns.
    fn skip_hop(&mut self) {
        for _ in 0..REVERB_BLOCK {
            self.stage[self.stage_fill] = 0.0;
            self.stage_fill += 1;
            if self.stage_fill == TAIL_BLOCK {
                self.previous_block.copy_from_slice(&self.stage);
                self.stage_fill = 0;
                self.wet.fill(0.0);
                self.wet_read = 0;
            }
        }
    }

    fn run_hop(&mut self) {
        for (slot, sample) in self.work.iter_mut().zip(self.previous_block.iter()) {
            *slot = Complex {
                re: *sample,
                im: 0.0,
            };
        }
        for (slot, sample) in self.work[TAIL_BLOCK..].iter_mut().zip(self.stage.iter()) {
            *slot = Complex {
                re: *sample,
                im: 0.0,
            };
        }
        self.previous_block.copy_from_slice(&self.stage);
        self.stage_fill = 0;

        self.fft
            .process_with_scratch(&mut self.work, &mut self.scratch);
        self.head = (self.head + 1) % self.history.len();
        self.history[self.head].copy_from_slice(&self.work);

        self.accumulator.fill(Complex::default());
        let count = self.history.len();
        for (lag, partition) in self.partitions.iter().enumerate() {
            let slot = (self.head + count - lag) % count;
            let spectrum = &self.history[slot];
            self.kernel
                .multiply_accumulate(&mut self.accumulator, spectrum, partition);
        }
        self.ifft
            .process_with_scratch(&mut self.accumulator, &mut self.scratch);
        let scale = 1.0 / TAIL_FFT as f32;
        for (slot, bin) in self.wet.iter_mut().zip(&self.accumulator[TAIL_BLOCK..]) {
            *slot = bin.re * scale;
        }
    }
}

/// Partitioned overlap-save convolution of one channel: fine partitions for
/// the head so it stays playable live, coarse ones for the tail so a long
/// room does not cost a partition every 128 samples.
struct MonoConvolver {
    kernel: ConvolutionKernel,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    /// IR block spectra, oldest lag first.
    partitions: Vec<Vec<Complex<f32>>>,
    /// Ring of recent input-block spectra; `head` is the newest.
    history: Vec<Vec<Complex<f32>>>,
    head: usize,
    previous_block: [f32; REVERB_BLOCK],
    /// Any input processed since the last reset (guards the reset cost).
    dirty: bool,
    work: Vec<Complex<f32>>,
    accumulator: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Input staging: a hop runs only on exactly REVERB_BLOCK gathered
    /// frames. Live hosts deliver callback buffers that are NOT multiples of
    /// 128 (pulse: 2062-frame blocks), so the caller's final sub-block is
    /// short; treating it as a full hop corrupted the overlap-save history
    /// and clicked at every callback boundary.
    stage: [f32; REVERB_BLOCK],
    stage_fill: usize,
    /// Wet output FIFO (ring). Aligned 128-frame feeds drain it exactly each
    /// call, with zero latency. The first short call underflows once;
    /// `bridge_underflow` then front-fills one hop of silence. After that the
    /// wet stream runs one hop late (at most 2.9 ms, inaudible for a reverb
    /// tail) and has no further gaps.
    wet: [f32; WET_CAP],
    wet_head: usize,
    wet_len: usize,
    /// Impulse-response offsets past `TAIL_START`, partitioned coarsely.
    tail: Option<TailConvolver>,
    /// How long the impulse response is, and how long the input has been
    /// silent. Once the second exceeds the first, every remaining
    /// contribution multiplies silence by the response, so the output is
    /// exactly zero and the transforms that would produce it are skipped.
    ir_frames: usize,
    silent_frames: usize,
    /// Zero-latency head, present only for a convolver fed one sample at a
    /// time.
    ///
    /// A uniformly-partitioned convolver cannot emit partition 0's
    /// contribution until the whole current block has arrived, so a per-sample
    /// feed comes out one hop late. With aligned 128-frame blocks the hop runs
    /// before the drain and there is no latency, which is how the orbit sends
    /// work. The head is therefore a mode: on an aligned convolver it would
    /// make the output one hop early.
    ///
    /// When present, the response is split. `direct` holds its first
    /// REVERB_BLOCK samples and is convolved in the time domain. The
    /// partitions are built from the response from REVERB_BLOCK onwards, so
    /// their one hop of delay is exactly the offset they represent:
    ///
    /// ```text
    /// response offset:  [0, 128)      [128, 2176)           [2176, end)
    ///                   `direct`      128-frame partitions  `tail`
    ///                   time domain   one hop of delay      2048-frame blocks
    /// ```
    direct: Option<Box<DirectHead>>,
}

/// The time-domain head of a streaming convolver.
struct DirectHead {
    ir: [f32; REVERB_BLOCK],
    history: [f32; REVERB_BLOCK],
    pos: usize,
}

impl DirectHead {
    #[inline]
    fn process(&mut self, sample: f32) -> f32 {
        self.history[self.pos] = sample;
        let mut sum = 0.0f32;
        for (k, tap) in self.ir.iter().enumerate() {
            let idx = (self.pos + REVERB_BLOCK - k) % REVERB_BLOCK;
            sum += tap * self.history[idx];
        }
        self.pos = (self.pos + 1) % REVERB_BLOCK;
        sum
    }
}

/// Below this counts as silence for the idle check. Not zero, because a
/// decaying voice approaches zero without arriving; -140 dBFS is far under
/// anything a 16-bit render can carry, so treating it as silence cannot
/// change an audible sample.
const SILENCE_EPSILON: f32 = 1e-7;

/// Wet FIFO bound: ≤ one hop of standing balance + one hop just produced.
const WET_CAP: usize = 2 * REVERB_BLOCK;

impl MonoConvolver {
    fn approx_bytes(&self) -> usize {
        let complex = std::mem::size_of::<Complex<f32>>();
        let spectra = self.partitions.len() + self.history.len();
        spectra * FFT_SIZE * complex
            + (self.work.len() + self.accumulator.len() + self.scratch.len()) * complex
            + self.tail.as_ref().map_or(0, TailConvolver::approx_bytes)
    }

    fn new(ir: &[f32], planner: &mut FftPlanner<f32>, kernel: ConvolutionKernel) -> Self {
        Self::build(ir, planner, false, kernel)
    }

    /// For a convolver that will be fed a sample at a time rather than in
    /// aligned 128-frame blocks. See [`MonoConvolver::direct`].
    fn new_streaming(ir: &[f32], planner: &mut FftPlanner<f32>, kernel: ConvolutionKernel) -> Self {
        Self::build(ir, planner, true, kernel)
    }

    fn build(
        ir: &[f32],
        planner: &mut FftPlanner<f32>,
        streaming: bool,
        kernel: ConvolutionKernel,
    ) -> Self {
        let (direct, ir) = if streaming {
            let mut taps = [0.0f32; REVERB_BLOCK];
            for (slot, sample) in taps.iter_mut().zip(ir) {
                *slot = *sample;
            }
            let rest = ir.get(REVERB_BLOCK..).unwrap_or(&[]);
            (
                Some(Box::new(DirectHead {
                    ir: taps,
                    history: [0.0; REVERB_BLOCK],
                    pos: 0,
                })),
                rest,
            )
        } else {
            (None, ir)
        };
        Self::build_partitioned(ir, planner, direct, kernel)
    }

    fn build_partitioned(
        ir: &[f32],
        planner: &mut FftPlanner<f32>,
        direct: Option<Box<DirectHead>>,
        kernel: ConvolutionKernel,
    ) -> Self {
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let ifft = planner.plan_fft_inverse(FFT_SIZE);
        let scratch_len = fft
            .get_inplace_scratch_len()
            .max(ifft.get_inplace_scratch_len());
        let mut scratch = vec![Complex::default(); scratch_len];
        let head = &ir[..ir.len().min(TAIL_START)];
        let count = head.len().div_ceil(REVERB_BLOCK).max(1);
        let mut partitions = Vec::with_capacity(count);
        for chunk in head.chunks(REVERB_BLOCK) {
            let mut spectrum = vec![Complex::default(); FFT_SIZE];
            for (bin, sample) in spectrum.iter_mut().zip(chunk) {
                bin.re = *sample;
            }
            fft.process_with_scratch(&mut spectrum, &mut scratch);
            partitions.push(spectrum);
        }
        if partitions.is_empty() {
            partitions.push(vec![Complex::default(); FFT_SIZE]);
        }
        let history = vec![vec![Complex::default(); FFT_SIZE]; partitions.len()];
        let tail = TailConvolver::new(ir, planner, kernel);
        Self {
            kernel,
            fft,
            ifft,
            partitions,
            history,
            head: 0,
            previous_block: [0.0; REVERB_BLOCK],
            dirty: false,
            work: vec![Complex::default(); FFT_SIZE],
            accumulator: vec![Complex::default(); FFT_SIZE],
            scratch,
            stage: [0.0; REVERB_BLOCK],
            stage_fill: 0,
            wet: [0.0; WET_CAP],
            wet_head: 0,
            wet_len: 0,
            tail,
            ir_frames: ir.len(),
            // Starts idle: nothing has been fed in yet, so nothing can ring.
            silent_frames: usize::MAX / 2,
            direct,
        }
    }

    fn reset(&mut self) {
        if !self.dirty {
            return;
        }
        for spectrum in &mut self.history {
            spectrum.fill(Complex::default());
        }
        self.previous_block.fill(0.0);
        if let Some(head) = self.direct.as_mut() {
            head.history.fill(0.0);
            head.pos = 0;
        }
        self.stage_fill = 0;
        self.wet_head = 0;
        self.wet_len = 0;
        self.dirty = false;
        self.silent_frames = usize::MAX / 2;
        if let Some(tail) = self.tail.as_mut() {
            tail.reset();
        }
    }

    /// Convolve one ≤128-frame block and ADD the wet result into `out`.
    /// Allocation-free. Hops run on the absolute input-sample grid (every
    /// 128 gathered frames), independent of how the caller chunks the
    /// stream, so misaligned host buffers cannot desync the overlap-save.
    fn process_block(&mut self, input: &[f32], out: &mut [f32]) {
        debug_assert!(input.len() <= REVERB_BLOCK && out.len() >= input.len());
        self.dirty = true;
        for &sample in input {
            if sample.abs() > SILENCE_EPSILON {
                self.silent_frames = 0;
            } else {
                self.silent_frames = self.silent_frames.saturating_add(1);
            }
            self.stage[self.stage_fill] = sample;
            self.stage_fill += 1;
            if self.stage_fill == REVERB_BLOCK {
                self.run_hop();
            }
        }
        if self.wet_len < input.len() {
            self.bridge_underflow();
        }
        for (slot, &sample) in out[..input.len()].iter_mut().zip(input) {
            let wet = self.wet[self.wet_head];
            self.wet_head = (self.wet_head + 1) % WET_CAP;
            self.wet_len -= 1;
            // The head is time-domain and lands on THIS sample; the
            // partitioned remainder is the response from REVERB_BLOCK on, so
            // its one hop of delay is exactly the offset it represents.
            *slot += wet
                + self
                    .direct
                    .as_mut()
                    .map_or(0.0, |head| head.process(sample));
        }
    }

    /// One overlap-save hop over the gathered stage; wet lands in the FIFO.
    fn run_hop(&mut self) {
        debug_assert!(self.wet_len + REVERB_BLOCK <= WET_CAP);
        // An orbit that has been quiet for longer than its own impulse
        // response cannot be ringing: every partition is multiplying silence,
        // and the sum is zero. Skipping is exact, not an approximation, and
        // avoids convolution work for idle orbits.
        //
        // The history stays correct while idle: each skipped hop would have
        // pushed the spectrum of silence, which is zero, and the ring is
        // already all zeros by the time this triggers. Resuming needs no
        // repair.
        // The margin is required. `silent_frames` counts silence ending at
        // the block just staged, but this hop emits output for the whole
        // block. Its first sample needs the response to end before the block
        // begins, not before the block ends. One more tail block covers the
        // coarse half, which computes one block ahead. Without the margin the
        // tail stops mid-decay: a 2047-sample response goes silent at 1920.
        if self.silent_frames >= self.ir_frames + TAIL_BLOCK + REVERB_BLOCK {
            self.previous_block.copy_from_slice(&self.stage);
            self.stage_fill = 0;
            for _ in 0..REVERB_BLOCK {
                self.wet[(self.wet_head + self.wet_len) % WET_CAP] = 0.0;
                self.wet_len += 1;
            }
            if let Some(tail) = self.tail.as_mut() {
                tail.skip_hop();
            }
            return;
        }
        // Overlap-save frame: previous block then this one.
        for (slot, sample) in self.work.iter_mut().zip(self.previous_block.iter()) {
            *slot = Complex {
                re: *sample,
                im: 0.0,
            };
        }
        for (slot, sample) in self.work[REVERB_BLOCK..].iter_mut().zip(self.stage.iter()) {
            *slot = Complex {
                re: *sample,
                im: 0.0,
            };
        }
        let staged = self.stage;
        self.previous_block.copy_from_slice(&self.stage);
        self.stage_fill = 0;

        self.fft
            .process_with_scratch(&mut self.work, &mut self.scratch);
        self.head = (self.head + 1) % self.history.len();
        self.history[self.head].copy_from_slice(&self.work);

        self.accumulator.fill(Complex::default());
        let count = self.partitions.len();
        for (lag, partition) in self.partitions.iter().enumerate() {
            let slot = (self.head + count - lag) % count;
            let spectrum = &self.history[slot];
            self.kernel
                .multiply_accumulate(&mut self.accumulator, spectrum, partition);
        }
        self.ifft
            .process_with_scratch(&mut self.accumulator, &mut self.scratch);
        // rustfft's inverse is unnormalized; the valid overlap-save half is
        // the SECOND 128 samples.
        let scale = 1.0 / FFT_SIZE as f32;
        for (bin, sample) in self.accumulator[REVERB_BLOCK..].iter().zip(staged.iter()) {
            // The tail advances on the hop clock, not the sample clock, so it
            // shares the head's staging and its wet FIFO. That is what keeps
            // the two halves aligned when a host delivers a short buffer and
            // the head bridges one silent hop to resynchronise.
            let tail = match self.tail.as_mut() {
                Some(tail) => tail.step(*sample),
                None => 0.0,
            };
            self.wet[(self.wet_head + self.wet_len) % WET_CAP] = bin.re * scale + tail;
            self.wet_len += 1;
        }
    }

    /// First short call on a misaligned stream: the hop for those frames
    /// hasn't gathered yet. Front-fill one hop of silence ONCE; from then on
    /// the FIFO balance stays at `128 − (total mod 128) ≥ 1`, so the wet
    /// stream never gaps again (it just runs one hop late).
    fn bridge_underflow(&mut self) {
        self.wet_head = (self.wet_head + WET_CAP - REVERB_BLOCK) % WET_CAP;
        for offset in 0..REVERB_BLOCK {
            self.wet[(self.wet_head + offset) % WET_CAP] = 0.0;
        }
        self.wet_len += REVERB_BLOCK;
    }
}

/// One orbit's reverb: parameters + a stereo pair of partitioned convolvers.
impl std::fmt::Debug for OrbitReverb {
    /// The convolvers are megabytes of spectra; only the params identify one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrbitReverb")
            .field("params", &self.params)
            .finish_non_exhaustive()
    }
}

pub struct OrbitReverb {
    params: ReverbParams,
    dispatch: DspDispatch,
    left: MonoConvolver,
    right: MonoConvolver,
}

impl OrbitReverb {
    /// Build from a CUSTOM impulse response (the `irspeed`/`irbegin` walk +
    /// the ConvolverNode normalization). NOT real-time safe.
    pub fn generate_custom(
        sample_rate: u32,
        params: ReverbParams,
        source: &crate::sample::DecodedSample,
    ) -> Self {
        Self::generate_custom_with_dispatch(sample_rate, params, source, DspDispatch::automatic())
    }

    /// Prepare a custom IR with the renderer's engine-kernel selection.
    /// This allocates and must run outside the audio callback.
    pub fn generate_custom_with_dispatch(
        sample_rate: u32,
        params: ReverbParams,
        source: &crate::sample::DecodedSample,
        dispatch: DspDispatch,
    ) -> Self {
        let max_frames = custom_ir_ceiling_frames(source.sample_rate());
        let (mut left_ir, mut right_ir, logical_frames) =
            custom_ir(sample_rate, params, source, max_frames);
        // Include the omitted zero tail in the RMS divisor to preserve the
        // wet level of the full-length IR.
        Self::normalize_and_plan_mode(
            sample_rate,
            params,
            &mut left_ir,
            &mut right_ir,
            logical_frames * 2,
            false,
            dispatch,
        )
    }

    fn normalize_and_plan(
        sample_rate: u32,
        params: ReverbParams,
        left_ir: &mut [f32],
        right_ir: &mut [f32],
        dispatch: DspDispatch,
    ) -> Self {
        let power_frames = left_ir.len() + right_ir.len();
        Self::normalize_and_plan_mode(
            sample_rate,
            params,
            left_ir,
            right_ir,
            power_frames,
            false,
            dispatch,
        )
    }

    /// `power_frames` is the sample count the RMS power is averaged over:
    /// both IRs' lengths, unless the caller dropped trailing zeros it still
    /// wants counted (a custom IR bounded by the tail ceiling).
    fn normalize_and_plan_mode(
        sample_rate: u32,
        params: ReverbParams,
        left_ir: &mut [f32],
        right_ir: &mut [f32],
        power_frames: usize,
        streaming: bool,
        dispatch: DspDispatch,
    ) -> Self {
        let scale = normalization_scale(sample_rate, left_ir, right_ir, power_frames);
        for sample in left_ir.iter_mut().chain(right_ir.iter_mut()) {
            *sample = (f64::from(*sample) * scale) as f32;
        }
        let mut planner = FftPlanner::new();
        let kernel = dispatch.convolution();
        Self {
            params,
            dispatch,
            left: if streaming {
                MonoConvolver::new_streaming(left_ir, &mut planner, kernel)
            } else {
                MonoConvolver::new(left_ir, &mut planner, kernel)
            },
            right: if streaming {
                MonoConvolver::new_streaming(right_ir, &mut planner, kernel)
            } else {
                MonoConvolver::new(right_ir, &mut planner, kernel)
            },
        }
    }

    /// Generate the IR and preplan the convolution for a caller that feeds
    /// one sample at a time instead of aligned 128-frame blocks: an `.FX()`
    /// stage, which runs inside the per-sample voice chain. The head is
    /// convolved in the time domain so the wet signal is not one hop late.
    /// See [`MonoConvolver::direct`]. Not real-time safe: call it from the
    /// producer thread or an offline render only.
    pub fn generate_streaming(sample_rate: u32, params: ReverbParams) -> Self {
        Self::generate_streaming_with_dispatch(sample_rate, params, DspDispatch::automatic())
    }

    /// Prepare a sample-fed IR with the renderer's engine-kernel selection.
    /// This allocates and must run outside the audio callback.
    pub fn generate_streaming_with_dispatch(
        sample_rate: u32,
        params: ReverbParams,
        dispatch: DspDispatch,
    ) -> Self {
        let mut noise = 0x5EEDC0DE_u64;
        let mut left_ir = generate_channel(sample_rate, params, &mut noise);
        let mut right_ir = generate_channel(sample_rate, params, &mut noise);
        let power_frames = left_ir.len() + right_ir.len();
        Self::normalize_and_plan_mode(
            sample_rate,
            params,
            &mut left_ir,
            &mut right_ir,
            power_frames,
            true,
            dispatch,
        )
    }

    pub fn generate(sample_rate: u32, params: ReverbParams) -> Self {
        Self::generate_with_dispatch(sample_rate, params, DspDispatch::automatic())
    }

    /// Prepare a block-fed IR with the renderer's engine-kernel selection.
    /// This allocates and must run outside the audio callback.
    pub fn generate_with_dispatch(
        sample_rate: u32,
        params: ReverbParams,
        dispatch: DspDispatch,
    ) -> Self {
        // Fixed stream; both channels draw from it in order, mirroring the
        // two-channel loop in generateReverb.
        let mut noise = 0x5EEDC0DE_u64;
        let mut left_ir = generate_channel(sample_rate, params, &mut noise);
        let mut right_ir = generate_channel(sample_rate, params, &mut noise);
        Self::normalize_and_plan(sample_rate, params, &mut left_ir, &mut right_ir, dispatch)
    }

    pub const fn dispatch(&self) -> DspDispatch {
        self.dispatch
    }

    /// Retarget only the exact engine-owned accumulation kernels. Existing
    /// spectra, FFT plans and convolution history stay valid and untouched.
    pub(crate) fn set_dispatch(&mut self, dispatch: DspDispatch) {
        let kernel = dispatch.convolution();
        for convolver in [&mut self.left, &mut self.right] {
            convolver.kernel = kernel;
            if let Some(tail) = convolver.tail.as_mut() {
                tail.kernel = kernel;
            }
        }
        self.dispatch = dispatch;
    }

    pub fn params(&self) -> ReverbParams {
        self.params
    }

    /// Roughly how much heap this instance holds, for pool accounting.
    ///
    /// The struct size of a convolver does not show its memory cost: the FFT
    /// of the impulse response and the matching input history dominate, and
    /// both grow with `roomsize`. Measured resident cost is 2.7 MiB at
    /// roomsize 0.5, 7.0 MiB at the default 2, and 24.8 MiB at 6. A pool
    /// bounded by instance count therefore does not bound memory.
    pub fn approx_bytes(&self) -> usize {
        self.left.approx_bytes() + self.right.approx_bytes()
    }

    /// Drop the tail (input history and overlap), keeping the planned IR.
    /// Cheap when the convolver never ran since the last reset.
    pub fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
    }

    /// Convolve one ≤128-frame block of the orbit's stereo send and ADD the
    /// wet result into the stereo block buffers. Allocation-free.
    pub fn process_block(
        &mut self,
        input_left: &[f32],
        input_right: &[f32],
        out_left: &mut [f32],
        out_right: &mut [f32],
    ) {
        self.left.process_block(input_left, out_left);
        self.right.process_block(input_right, out_right);
    }
}

/// ConvolverNode normalization: RMS level across both channels, with
/// `power_frames` counting samples from both channels. Apply a 0.000125 RMS
/// floor and a -58 dB calibration at the 44100 Hz reference rate.
fn normalization_scale(
    sample_rate: u32,
    left_ir: &[f32],
    right_ir: &[f32],
    power_frames: usize,
) -> f64 {
    let sum: f64 = left_ir
        .iter()
        .chain(right_ir.iter())
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum();
    let power = (sum / power_frames.max(1) as f64).sqrt().max(0.000125);
    (1.0 / power) * 10f64.powf(-58.0 / 20.0) * (44_100.0 / f64::from(sample_rate))
}

/// Maximum filled source frames for a resolver-admitted `roomsize`, whose
/// limit is [`MAX_REVERB_SECONDS`]. Frames beyond this are zero.
fn custom_ir_ceiling_frames(src_rate: u32) -> usize {
    (f64::from(MAX_REVERB_SECONDS) * f64::from(src_rate)).ceil() as usize
}

/// A custom IR's two channels at `sample_rate`, plus its full per-channel
/// length for normalization, including the omitted zero tail.
///
/// Deliberate quirks of the IR walk, after Strudel's `adjustLength`: output
/// starts with the source length and fills at most `src_rate * size_secs`
/// frames. Position walks `(offset + i * |speed|) % len`, is negated when
/// speed < 1, and indexes from the end when negative. Length, offset and
/// modulo use the whole source so bounding the output preserves the walk.
///
/// Retain only `ceiling_frames` plus one guard frame. For admitted room
/// sizes, omitted frames are zero and need no convolution partitions. The
/// guard frame lets resampling interpolate into that zero tail instead of
/// repeating the last filled frame.
fn custom_ir(
    sample_rate: u32,
    params: ReverbParams,
    source: &crate::sample::DecodedSample,
    ceiling_frames: usize,
) -> (Vec<f32>, Vec<f32>, usize) {
    let ir = params.ir.unwrap_or(IrParams {
        sample: crate::sample::SampleId(0),
        speed: 1.0,
        begin: 0.0,
    });
    let src_rate = source.sample_rate();
    let channels = source.channels().max(1);
    let src_frames = source.frames();
    let out_frames = src_frames.min(ceiling_frames.saturating_add(1));
    let pcm = source.pcm();
    let adjusted_channel = |channel: usize| -> Vec<f32> {
        let read = |frame: usize| -> f32 {
            let idx = frame * usize::from(channels) + channel.min(usize::from(channels) - 1);
            pcm.get(idx).copied().unwrap_or(0.0)
        };
        let len = src_frames as f64;
        let offset = (f64::from(ir.begin).clamp(0.0, 1.0) * len).floor();
        let fill = ((f64::from(src_rate) * f64::from(params.size_secs)) as usize)
            .min(src_frames)
            .min(out_frames);
        let mut out = vec![0.0f32; out_frames];
        for (i, slot) in out.iter_mut().enumerate().take(fill) {
            let mut position = (offset + i as f64 * f64::from(ir.speed.abs())) % len;
            if ir.speed < 1.0 {
                position = -position;
            }
            // Truncate toward zero; negative indexes from the end;
            // out of range reads 0.
            let idx = position.trunc();
            let resolved = if idx < 0.0 { len + idx } else { idx };
            *slot = if (0.0..len).contains(&resolved) {
                read(resolved as usize)
            } else {
                0.0
            };
        }
        out
    };
    let left_src = adjusted_channel(0);
    let right_src = adjusted_channel(1);
    // A decoded IR is normally already at the context rate when the walk
    // above runs, which is why it uses `src_rate` for its fill length. The
    // loader converts on the same terms, so this is a guard for a caller
    // that hands over something else, not a stage the sample path passes
    // through.
    let ratio = f64::from(src_rate) / f64::from(sample_rate);
    let resampled_len = |frames: usize| ((frames as f64) / ratio).floor() as usize;
    let resample = |data: &[f32]| -> Vec<f32> {
        if src_rate == sample_rate || data.is_empty() {
            return data.to_vec();
        }
        (0..resampled_len(data.len()))
            .map(|i| {
                let pos = i as f64 * ratio;
                let base = pos as usize;
                let t = (pos - base as f64) as f32;
                let a = data.get(base).copied().unwrap_or(0.0);
                let b = data.get(base + 1).copied().unwrap_or(a);
                a + (b - a) * t
            })
            .collect()
    };
    let logical_frames = if src_rate == sample_rate || src_frames == 0 {
        src_frames
    } else {
        resampled_len(src_frames)
    };
    (resample(&left_src), resample(&right_src), logical_frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_accumulation_dispatch_preserves_fft_plans_and_running_tails() {
        let params = ReverbParams {
            ir: None,
            size_secs: 0.2,
            fade_secs: 0.01,
            lp_start_hz: 8_000.0,
            lp_end_hz: 1_000.0,
        };
        let mut actual = OrbitReverb::generate(24_000, params);
        let mut expected =
            OrbitReverb::generate_with_dispatch(24_000, params, DspDispatch::portable());
        let fft = Arc::clone(&actual.left.fft);
        let tail_fft = Arc::clone(&actual.left.tail.as_ref().expect("long IR tail").fft);
        for block in 0..64 {
            if block == 23 {
                actual.set_dispatch(DspDispatch::portable());
                assert!(actual.dispatch().is_forced_portable());
                for convolver in [&actual.left, &actual.right] {
                    assert!(
                        convolver
                            .kernel
                            .same_implementation(ConvolutionKernel::PORTABLE)
                    );
                    assert!(
                        convolver
                            .tail
                            .as_ref()
                            .expect("long IR tail")
                            .kernel
                            .same_implementation(ConvolutionKernel::PORTABLE)
                    );
                }
                assert!(Arc::ptr_eq(&fft, &actual.left.fft));
                assert!(Arc::ptr_eq(
                    &tail_fft,
                    &actual.left.tail.as_ref().unwrap().fft
                ));
            }
            let input = std::array::from_fn::<_, REVERB_BLOCK, _>(|index| {
                if block < 16 {
                    ((index * 17 % 71) as f32 - 35.0) / 35.0
                } else {
                    0.0
                }
            });
            let (mut actual_left, mut actual_right) = ([0.0; REVERB_BLOCK], [0.0; REVERB_BLOCK]);
            let (mut expected_left, mut expected_right) =
                ([0.0; REVERB_BLOCK], [0.0; REVERB_BLOCK]);
            actual.process_block(&input, &input, &mut actual_left, &mut actual_right);
            expected.process_block(&input, &input, &mut expected_left, &mut expected_right);
            for (actual, expected) in actual_left
                .into_iter()
                .chain(actual_right)
                .zip(expected_left.into_iter().chain(expected_right))
            {
                assert!(actual.is_finite() && expected.is_finite());
                assert_eq!(actual.to_bits(), expected.to_bits(), "block {block}");
            }
        }
    }

    /// A callback buffer that is not a multiple of 128 frames (for example
    /// 2062) yields the aligned wet stream exactly, one hop (128 frames) late
    /// after one silent bridge at the first short call.
    #[test]
    fn misaligned_chunking_matches_the_aligned_wet_stream_one_hop_late() {
        let params = ReverbParams {
            ir: None,
            size_secs: 0.3,
            fade_secs: 0.02,
            lp_start_hz: 15_000.0,
            lp_end_hz: 1_000.0,
        };
        let mut aligned = OrbitReverb::generate(44_100, params);
        let mut chunked = OrbitReverb::generate(44_100, params);
        // Deterministic noise input, long enough for several "callbacks".
        let mut state = 0x1234_5678_u64;
        let total = 2062 * 8;
        let input: Vec<f32> = (0..total)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                ((state >> 33) as f32 / (1u64 << 31) as f32) - 1.0
            })
            .collect();

        let render = |reverb: &mut OrbitReverb, chunks: &dyn Fn(usize) -> usize| -> Vec<f32> {
            let mut out = vec![0.0f32; total];
            let mut at = 0;
            while at < total {
                let len = chunks(at).min(total - at);
                let (left, right) = (input[at..at + len].to_vec(), input[at..at + len].to_vec());
                let mut wet_l = vec![0.0f32; len];
                let mut wet_r = vec![0.0f32; len];
                reverb.process_block(&left, &right, &mut wet_l, &mut wet_r);
                out[at..at + len].copy_from_slice(&wet_l);
                at += len;
            }
            out
        };
        let a = render(&mut aligned, &|_| REVERB_BLOCK);
        // The live pattern: within each 2062-frame callback, 16×128 then 14.
        let b = render(&mut chunked, &|at| {
            let in_callback = at % 2062;
            (2062 - in_callback).min(REVERB_BLOCK)
        });

        // Before the first short call (frame 2048) the streams are identical.
        assert_eq!(a[..2048], b[..2048]);
        // One silent bridge hop while the true hop gathers…
        assert!(b[2048..2176].iter().all(|s| *s == 0.0));
        // …then the aligned stream continues exactly one hop late, forever.
        for i in 2176..total {
            assert_eq!(a[i - REVERB_BLOCK], b[i], "diverged at frame {i}");
        }
        // And the wet stream carries real (nonzero) signal after the bridge.
        assert!(b[2176..].iter().any(|s| s.abs() > 1e-6));
    }

    #[test]
    fn impulse_response_envelope_matches_the_generator_contract() {
        let params = ReverbParams {
            ir: None,
            size_secs: 0.5,
            fade_secs: 0.05,
            lp_start_hz: 0.0, // skip the lowpass so the envelope is exact
            lp_end_hz: 0.0,
        };
        let mut noise = 1u64;
        let channel = generate_channel(48_000, params, &mut noise);
        assert_eq!(channel.len(), (0.5 * 1.5 * 48_000.0) as usize);
        // Fade-in: first sample silent, ramp up.
        assert_eq!(channel[0], 0.0);
        // Decay: |envelope| at decayTime is 1/1000 of the start.
        let window = |at: usize| -> f32 {
            channel[at..at + 480]
                .iter()
                .fold(0.0f32, |peak, s| peak.max(s.abs()))
        };
        let early = window(2_400); // just past fade-in
        let at_decay = window(24_000 - 480);
        assert!(
            at_decay < early * 0.01,
            "decay must reach ~-60 dB at decayTime: early {early}, late {at_decay}"
        );
    }

    /// The roomsize ceiling bounds a custom IR. The bound drops only zero
    /// frames: the planned IR is an exact prefix of the unbounded walk at
    /// every `irbegin`/`irspeed` and context rate, and the wet level is equal.
    #[test]
    fn a_custom_ir_past_the_ceiling_plans_a_prefix_of_the_unbounded_walk() {
        let rate = 8_000;
        let max_frames = custom_ir_ceiling_frames(rate);
        // Three ceilings of PCM - 30 s at 8 kHz - is cheap to build here.
        let pcm: Vec<f32> = (0..max_frames * 3)
            .map(|i| (((i * 37) % 211) as f32 - 105.0) / 105.0)
            .collect();
        let over =
            crate::sample::DecodedSample::from_parts(rate, 1, pcm).expect("over-ceiling IR source");
        let params = |speed: f32, begin: f32| ReverbParams {
            ir: Some(IrParams {
                sample: crate::sample::SampleId(0),
                speed,
                begin,
            }),
            // The longest walk the resolver admits; fade/lowpass are
            // generated-path knobs and are ignored by the custom walk.
            size_secs: MAX_REVERB_SECONDS,
            fade_secs: 0.01,
            lp_start_hz: 0.0,
            lp_end_hz: 0.0,
        };
        for (speed, begin) in [
            (1.0, 0.0),
            (1.0, 0.5),
            (2.0, 0.25),
            (-1.0, 0.0),
            (-2.0, 0.75),
        ] {
            for context_rate in [rate, 48_000] {
                let what = format!("speed {speed}, begin {begin}, at {context_rate} Hz");
                let (left, right, logical) =
                    custom_ir(context_rate, params(speed, begin), &over, max_frames);
                let (full_left, full_right, full_logical) =
                    custom_ir(context_rate, params(speed, begin), &over, usize::MAX);
                assert_eq!(logical, full_logical, "{what}");
                assert_eq!(full_left.len(), full_logical, "{what}");
                for (bounded, full) in [(&left, &full_left), (&right, &full_right)] {
                    assert!(
                        bounded.len() * 2 < full.len(),
                        "the ceiling bounds the IR: {what}"
                    );
                    assert!(
                        bounded[..] == full[..bounded.len()],
                        "the bounded IR is a prefix of the unbounded walk: {what}"
                    );
                    assert!(
                        full[bounded.len()..].iter().all(|sample| *sample == 0.0),
                        "only frames that were always zero are dropped: {what}"
                    );
                }
                // Averaged over the unbounded length, the power - and so the
                // wet level - is exactly the unbounded IR's.
                assert_eq!(
                    normalization_scale(context_rate, &left, &right, logical * 2),
                    normalization_scale(
                        context_rate,
                        &full_left,
                        &full_right,
                        full_left.len() + full_right.len()
                    ),
                    "{what}"
                );
                assert!(
                    full_left.iter().any(|sample| *sample != 0.0),
                    "the walk reads audio: {what}"
                );
            }
        }
        // The planned convolver is the bounded IR, not the source.
        let planned = OrbitReverb::generate_custom(rate, params(1.0, 0.0), &over);
        let tail_partitions = (max_frames + 1 - TAIL_START).div_ceil(TAIL_BLOCK);
        for convolver in [&planned.left, &planned.right] {
            assert_eq!(convolver.ir_frames, max_frames + 1);
            assert_eq!(
                convolver
                    .tail
                    .as_ref()
                    .expect("a ceiling-length IR has a coarse tail")
                    .partitions
                    .len(),
                tail_partitions
            );
        }
    }

    /// The same equality for an impulse response past the 2048-sample head,
    /// so the coarse tail partitions run.
    #[test]
    fn a_long_impulse_response_still_matches_direct_convolution() {
        let ir: Vec<f32> = (0..9_000)
            .map(|i| (((i * 37) % 211) as f32 - 105.0) / 105.0 * (1.0 - i as f32 / 9_000.0))
            .collect();
        let mut planner = FftPlanner::new();
        let mut convolver = MonoConvolver::new(&ir, &mut planner, ConvolutionKernel::PORTABLE);

        let frames = 6_144;
        let input: Vec<f32> = (0..frames)
            .map(|i| (((i * 53) % 97) as f32 - 48.0) / 48.0)
            .collect();
        let mut partitioned = vec![0.0f32; frames];
        for block in 0..frames / REVERB_BLOCK {
            let range = block * REVERB_BLOCK..(block + 1) * REVERB_BLOCK;
            let mut out = [0.0f32; REVERB_BLOCK];
            convolver.process_block(&input[range.clone()], &mut out);
            partitioned[range].copy_from_slice(&out);
        }

        let mut worst = 0.0f64;
        for n in 0..frames {
            let mut direct = 0.0f64;
            for (k, tap) in ir.iter().enumerate() {
                if n >= k {
                    direct += f64::from(input[n - k]) * f64::from(*tap);
                }
            }
            worst = worst.max((f64::from(partitioned[n]) - direct).abs());
        }
        // Float32 FFT round-trip over thousands of taps; the signal itself
        // reaches into the tens, so this is parts-per-million.
        assert!(worst < 5e-2, "worst tail divergence {worst}");
    }

    /// An orbit that goes quiet must stop costing anything, and must come
    /// back intact. The skip is only sound if a resumed reverb is identical
    /// to one that never idled, so this compares the two directly.
    #[test]
    fn an_idle_reverb_resumes_exactly_as_if_it_had_never_idled() {
        let ir: Vec<f32> = (0..6_000)
            .map(|i| (((i * 37) % 211) as f32 - 105.0) / 105.0 * (1.0 - i as f32 / 6_000.0))
            .collect();
        let frames = 64 * REVERB_BLOCK;
        // A hit, a long silence well past the idle threshold, then another.
        let mut input = vec![0.0f32; frames];
        for (n, slot) in input.iter_mut().enumerate() {
            if n < REVERB_BLOCK || (n >= frames - 2 * REVERB_BLOCK && n < frames - REVERB_BLOCK) {
                *slot = (((n * 53) % 97) as f32 - 48.0) / 48.0;
            }
        }

        let mut planner = FftPlanner::new();
        let mut convolver = MonoConvolver::new(&ir, &mut planner, ConvolutionKernel::PORTABLE);
        let mut got = vec![0.0f32; frames];
        for block in 0..frames / REVERB_BLOCK {
            let range = block * REVERB_BLOCK..(block + 1) * REVERB_BLOCK;
            let mut out = [0.0f32; REVERB_BLOCK];
            convolver.process_block(&input[range.clone()], &mut out);
            got[range].copy_from_slice(&out);
        }

        let mut worst = 0.0f64;
        for n in 0..frames {
            let mut direct = 0.0f64;
            for (k, tap) in ir.iter().enumerate() {
                if n >= k {
                    direct += f64::from(input[n - k]) * f64::from(*tap);
                }
            }
            worst = worst.max((f64::from(got[n]) - direct).abs());
        }
        assert!(worst < 5e-2, "idling changed the output: worst {worst}");
        // And the second hit must actually be audible, or the test would pass
        // on a convolver that fell silent for good.
        let after = &got[frames - 2 * REVERB_BLOCK..];
        let peak = after.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
        assert!(peak > 0.01, "the reverb never woke up: peak {peak}");
    }

    /// The split point itself: an impulse response of exactly one head plus
    /// one sample has a one-sample tail, and an all-head one has none.
    #[test]
    fn the_head_tail_boundary_convolves_correctly() {
        for length in [TAIL_START - 1, TAIL_START, TAIL_START + 1, TAIL_START * 2] {
            let ir: Vec<f32> = (0..length)
                .map(|i| (((i * 17) % 71) as f32 - 35.0) / 35.0)
                .collect();
            let mut planner = FftPlanner::new();
            let mut convolver = MonoConvolver::new(&ir, &mut planner, ConvolutionKernel::PORTABLE);
            let frames = length + 2 * REVERB_BLOCK;
            let frames = frames.next_multiple_of(REVERB_BLOCK);
            let input: Vec<f32> = (0..frames)
                .map(|i| if i == 0 { 1.0 } else { 0.0 })
                .collect();
            let mut got = vec![0.0f32; frames];
            for block in 0..frames / REVERB_BLOCK {
                let range = block * REVERB_BLOCK..(block + 1) * REVERB_BLOCK;
                let mut out = [0.0f32; REVERB_BLOCK];
                convolver.process_block(&input[range.clone()], &mut out);
                got[range].copy_from_slice(&out);
            }
            // A unit impulse in must give the impulse response back.
            for (n, tap) in ir.iter().enumerate() {
                assert!(
                    (got[n] - tap).abs() < 1e-3,
                    "ir length {length}, sample {n}: got {} want {tap}",
                    got[n]
                );
            }
        }
    }

    #[test]
    fn partitioned_convolution_matches_direct_convolution() {
        // Small IR, arbitrary block content, three blocks: the partitioned
        // engine must equal a textbook direct convolution.
        let ir: Vec<f32> = (0..300)
            .map(|i| ((i * 37 % 100) as f32 - 50.0) / 50.0)
            .collect();
        let mut planner = FftPlanner::new();
        let mut convolver = MonoConvolver::new(&ir, &mut planner, ConvolutionKernel::PORTABLE);
        let input: Vec<f32> = (0..384)
            .map(|i| ((i * 53 % 97) as f32 - 48.0) / 48.0)
            .collect();

        let mut partitioned = vec![0.0f32; 384];
        for block in 0..3 {
            let range = block * 128..(block + 1) * 128;
            let mut out = [0.0f32; 128];
            convolver.process_block(&input[range.clone()], &mut out);
            partitioned[range].copy_from_slice(&out);
        }

        for n in 0..384 {
            let mut direct = 0.0f64;
            for (k, tap) in ir.iter().enumerate() {
                if n >= k {
                    direct += f64::from(input[n - k]) * f64::from(*tap);
                }
            }
            assert!(
                (partitioned[n] as f64 - direct).abs() < 1e-3,
                "sample {n}: partitioned {} vs direct {direct}",
                partitioned[n]
            );
        }
    }
}
