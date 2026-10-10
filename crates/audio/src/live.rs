//! Fixed-capacity scalar consumer for the audio SPSC ring.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::{AudioBackend, DspDispatch, OnsetEvent, QueuedAudioEvent, Ring, ScalarBackend};

/// What a published cutover asked the consumer to do to what is sounding
/// under it - the newest generation's silence contract.
///
/// - [`TakeoverCut::None`] is an ordinary edit: the old rendition rings out
///   by contract, click-free.
/// - [`TakeoverCut::AtFlip`] is an immediate rewind: the restarted loop is
///   heard alone from the first consumer block, like a retriggered sample.
/// - [`TakeoverCut::AtTakeover`] is a quantised rewind: the old score plays
///   the countdown to its line, then the restarted loop takes over at the
///   takeover frame itself. Cutting at the flip here would silence the
///   countdown - up to a head-room of publication latency plus schedule
///   lead plus continuity margin before the line.
///
/// Where the consumer stops the outgoing generation, on the frame clock:
///
/// ```text
///                flip block            takeover frame
/// frames  -----------+----------------------+---------------------->
/// None               :  old onsets sound    | dropped, voices ring out
/// AtFlip             | old onsets dropped, voices fade
/// AtTakeover         :  old onsets sound    | dropped, voices fade
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TakeoverCut {
    #[default]
    None,
    AtFlip,
    AtTakeover,
}

impl TakeoverCut {
    /// Decodes the device's shared word: 0 none, 1 at-flip, 2 at-takeover.
    pub fn from_bits(bits: u64) -> Self {
        match bits {
            1 => Self::AtFlip,
            2 => Self::AtTakeover,
            _ => Self::None,
        }
    }

    /// Whether the outgoing rendition is silenced at all (the two rewind
    /// shapes) - as opposed to ringing out.
    pub fn is_cut(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Maximum simultaneously active/pending scalar voices in live playback.
pub const MAX_LIVE_VOICES: usize = 4096;

/// The line-arm word a WITHDRAWN arm leaves behind (bit 0 clear, so nothing
/// fires): distinct from the zero a published flip stores, so the consumer
/// can tell "the launch was cancelled or failed" from "the flip took over".
/// After a withdrawal the consumer lets go of a fired arm - the outgoing
/// rendition's later events pass again and the next launch's arm can fire.
pub const LINE_ARM_WITHDRAWN: u64 = 0b100;

/// The four handshake words the producer thread publishes and the callback
/// reads, named. All four are `AtomicU64`, so as positional arguments any two
/// could be swapped without a type error. The struct holds four borrowed
/// pointers and is built at each call, so it allocates nothing.
#[derive(Clone, Copy, Debug)]
pub struct LiveFlipAtomics<'a> {
    /// The newest published generation.
    pub generation: &'a AtomicU64,
    /// Where the newest generation's re-query cursor starts; published
    /// before the generation.
    pub takeover_frame: &'a AtomicU64,
    /// The newest cutover's [`TakeoverCut`], as [`TakeoverCut::from_bits`]
    /// reads it; published with the takeover frame.
    pub takeover_cut: &'a AtomicU64,
    /// A launch's pre-armed line: the frame in the top 62 bits, bit 1 the
    /// drop horizon, bit 0 the arm; [`LINE_ARM_WITHDRAWN`] once withdrawn.
    pub line_arm: &'a AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LiveBlockReport {
    pub accepted: usize,
    pub stale: usize,
    pub refused: usize,
    /// Accepted, but the target frame was already in the past: the producer
    /// fell behind the playhead. This is the producer-starvation signal.
    pub late: usize,
}

/// Callback-owned state. All dynamic storage is allocated by `new`, before the
/// object reaches a real-time thread.
pub struct LiveScalarBackend {
    scalar: ScalarBackend,
    held: Option<QueuedAudioEvent>,
    generation: u64,
    sample_rate: u32,
    /// Frames of the stop ramp already rendered; `None` while not stopped.
    /// A counter is required because hosts may deliver any block size.
    stop_fade_pos: Option<usize>,
    /// The last onset whose sidechain was armed at ring intake, so re-held
    /// events don't re-arm every block.
    armed_duck: Option<u64>,
    /// A restart's catch-up floor, `(generation, frame)`, latched at a cut
    /// flip (a rewind). The frame is the start of the first block that
    /// drains the restart's events. Onsets of that generation aimed before
    /// the floor are the restarted loop's first window, published after the
    /// instant it was anchored to. They are admitted at the floor, not in
    /// the past. A voice admitted in the past starts partway into its
    /// sample: the downbeat loses its attack, and a one-shot later than
    /// its own length never sounds. Later onsets of the generation aim past
    /// the floor and are untouched. An edit's flip clears the floor.
    restart_floor: Option<(u64, u64)>,
    /// A takeover cut's drop horizon: old-generation ring events with a
    /// target at or after this frame are refused - they are the ghost
    /// window a restart replaces. Latched at the flip alongside the cut
    /// and cleared with it (a later flip without a cut supersedes).
    takeover_cut_frame: Option<u64>,
    /// The pre-armed line cut's drop horizon, latched when the arm fires
    /// (the block that reaches the line). Ring events of the outgoing
    /// generation (or older) with a target at or after this frame are
    /// refused until the flip. This covers the late-flip window, where a
    /// slow evaluation would otherwise let the old score's first beat
    /// sound. Cleared at the flip, whose own horizons take over, or when
    /// the arm is withdrawn without one.
    line_arm_drop_from: Option<u64>,
    /// The pre-armed line cut fired (the block that reached its frame);
    /// later blocks must not re-fire it. Reset at every flip so a second
    /// launch's arm can fire fresh.
    line_arm_fired: bool,
    #[cfg(feature = "device-audio")]
    confirmation_stopped: bool,
    #[cfg(feature = "device-audio")]
    confirmation_takeover: u64,
}

impl LiveScalarBackend {
    /// Bounded, allocation-free admission for hand-played notes. This queue
    /// cannot sit behind a future score onset. Successive commands retain
    /// their order within the block, including release followed by re-press.
    pub fn admit_immediate(
        &mut self,
        ring: &Ring,
        start_frame: u64,
        frames: usize,
    ) -> LiveBlockReport {
        let mut report = LiveBlockReport::default();
        for offset in 0..frames.min(64) {
            let Some(event) = ring.pop() else { break };
            if self.scalar.try_note_prepared_with_confirmation(
                OnsetEvent::new(
                    start_frame.saturating_add(offset as u64),
                    event.freq_hz,
                    event.gain,
                    event.duration_secs,
                )
                .with_generation(self.generation)
                .with_controls(event.controls)
                .with_optional_sample(event.sample)
                .with_optional_wavetable(event.wavetable)
                .with_optional_synth(event.synth)
                .with_cut(event.cut),
                None,
                None,
            ) {
                report.accepted += 1;
            } else {
                report.refused += 1;
            }
        }
        report
    }

    pub fn max_polyphony(&self) -> usize {
        self.scalar.max_polyphony()
    }
    pub fn set_max_polyphony(&mut self, limit: usize) {
        self.scalar.set_max_polyphony(limit);
    }

    /// Set sample interpolation before the callback owns this backend.
    #[cfg(feature = "device-audio")]
    pub(crate) fn set_sample_resampling_mode(&mut self, mode: crate::SampleResamplingMode) {
        self.scalar.set_sample_resampling_mode(mode);
    }

    /// What this backend wrote while it was prepared, itself included: see
    /// [`ScalarBackend::prepared_bytes`].
    pub fn prepared_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.scalar.prepared_bytes()
    }

    pub fn set_live_control(&mut self, update: crate::live_control::LiveControlUpdate) {
        self.scalar.set_live_control(update);
    }
    /// Preparation only: the caller reclaims displaced PCM before this backend
    /// is moved into an audio callback.
    #[cfg(feature = "device-audio")]
    pub(crate) fn install_prepared_sample(
        &mut self,
        id: crate::sample::SampleId,
        sample: Box<crate::sample::DecodedSample>,
    ) -> Result<Option<Box<crate::sample::DecodedSample>>, Box<crate::sample::DecodedSample>> {
        self.scalar.install_sample(id, sample)
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn set_confirmations(&mut self, channel: crate::confirmation::ConfirmationChannel) {
        self.scalar.confirmations = Some(crate::confirmation::ConfirmationConsumer::new(channel));
    }

    /// Called only after this sub-block has reached the host's output slice.
    #[cfg(feature = "device-audio")]
    pub(crate) fn confirmation_copied(&mut self, start: u64, end: u64) {
        if let Some(consumer) = &mut self.scalar.confirmations {
            consumer.copied(start, end, self.generation, self.confirmation_stopped);
        }
    }

    /// Give input voices the ring the device's input callback fills.
    pub fn set_input(&mut self, ring: Option<std::sync::Arc<crate::input::InputRing>>) {
        self.scalar.set_input(ring);
    }

    /// The orbit meters since the last take.
    pub fn take_orbit_peaks(&mut self) -> [f32; crate::scalar::MAX_ORBITS] {
        self.scalar.take_orbit_peaks()
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn score_sources_active(&self) -> bool {
        self.scalar.score_sources_active()
    }

    /// Silence everything sounding, over the choke ramp. Asked for when
    /// one audition replaces another; a reload never does it.
    pub fn cut_sounding_voices(&mut self, at_frame: u64) {
        self.scalar.cut_sounding_voices(at_frame);
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn pressure_observation(&self) -> crate::pressure::RealtimePressureObservation {
        self.scalar.pressure_observation()
    }

    /// Route each orbit to an output pair; pair 0 is the main mix.
    pub fn set_orbit_pairs(&mut self, pairs: [u8; crate::scalar::MAX_ORBITS]) {
        if self.scalar.orbit_pairs() != pairs {
            self.scalar.set_orbit_pairs(pairs);
        }
    }

    /// The mixer's orbit faders, linear; unity until the mixer says.
    pub fn set_orbit_gains(&mut self, gains: &[f32; crate::scalar::MAX_ORBITS]) {
        self.scalar.set_orbit_gains(gains);
    }

    pub fn routing_active(&self) -> bool {
        self.scalar.routing_active()
    }

    pub fn orbit_mix(&self, orbit: usize, frames: usize) -> &[f32] {
        self.scalar.orbit_mix(orbit, frames)
    }

    /// Stage prepared inserts between audio blocks and return displaced
    /// copies to the producer. A full return ring defers new installs.
    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    pub(crate) fn drain_insert_installs(&mut self, channel: &crate::assets::SampleChannel) {
        loop {
            if channel.insert_returns.len() == channel.insert_returns.capacity() {
                return;
            }
            let Some(retired) = self.scalar.take_retired_insert() else {
                break;
            };
            let returned = crate::assets::ReturnedInsert(Box::into_raw(retired));
            // This callback is the sole producer and a slot was free.
            if channel.insert_returns.push(returned).is_err() {
                channel
                    .leaked
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let pending = channel.insert_installs.len();
        for _ in 0..pending {
            if channel.insert_returns.len() == channel.insert_returns.capacity() {
                break;
            }
            let Some(install) = channel.insert_installs.pop() else {
                break;
            };
            // SAFETY: producer-boxed pointer handed over via the SPSC ring.
            let insert = unsafe { Box::from_raw(install.insert) };
            // An insert prepared before the output changed its rate goes
            // back to the producer. The producer ships a new one.
            let displaced = if install.sample_rate == self.scalar.sample_rate() {
                self.scalar
                    .prepare_insert(usize::from(install.slot), insert)
            } else {
                Some(insert)
            };
            if let Some(displaced) = displaced {
                let returned = crate::assets::ReturnedInsert(Box::into_raw(displaced));
                // Only this callback fills the ring, and a slot was free.
                if channel.insert_returns.push(returned).is_err() {
                    channel
                        .leaked
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }

    /// Install prepared reverbs between audio blocks. Return displaced boxes
    /// to the producer for destruction. Defer FX installs when their return
    /// ring has no space for a refusal.
    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    pub(crate) fn drain_reverb_installs(&mut self, channel: &crate::assets::SampleChannel) {
        for install in channel.reverb_installs.drain_available() {
            // SAFETY: producer-boxed pointer handed over via the SPSC ring.
            let reverb = unsafe { Box::from_raw(install.reverb) };
            if let Some(displaced) = self
                .scalar
                .install_reverb(usize::from(install.orbit), reverb)
                && channel
                    .reverb_returns
                    .push(crate::assets::ReturnedReverb(Box::into_raw(displaced)))
                    .is_err()
            {
                channel
                    .leaked
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let pending = channel.fx_reverb_installs.len();
        for _ in 0..pending {
            let return_slots = channel
                .fx_reverb_returns
                .capacity()
                .saturating_sub(channel.fx_reverb_returns.len());
            // Reserve a refusal slot before taking ownership. Only this
            // callback fills return slots, so the free count cannot shrink
            // before the push.
            if return_slots == 0 {
                break;
            }
            let Some(install) = channel.fx_reverb_installs.pop() else {
                break;
            };
            // SAFETY: producer-boxed pointer handed over via the SPSC ring.
            let reverb = unsafe { Box::from_raw(install.reverb) };
            let retire_limit = return_slots - 1;
            if let Some(rejected) =
                self.scalar
                    .install_fx_reverb_evicting(reverb, retire_limit, |retired| {
                        let returned = crate::assets::ReturnedFxReverb(Box::into_raw(retired));
                        // retire_limit reserved this slot before the install.
                        if channel.fx_reverb_returns.push(returned).is_err() {
                            channel
                                .leaked
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    })
            {
                channel
                    .fx_reverb_refusals
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if channel
                    .fx_reverb_returns
                    .push(crate::assets::ReturnedFxReverb(Box::into_raw(rejected)))
                    .is_err()
                {
                    channel
                        .leaked
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        channel.fx_reverb_resident_bytes.store(
            self.scalar.fx_reverb_resident_bytes(),
            std::sync::atomic::Ordering::Release,
        );
    }

    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    pub(crate) fn drain_sample_installs(&mut self, channel: &crate::assets::SampleChannel) {
        for install in channel.installs.drain_available() {
            // A null pointer is an uninstall: the producer asked this slot
            // to be emptied so unused preview PCM can be reclaimed. The
            // displaced box, if any, still returns through the same ring.
            if install.sample.is_null() {
                if let Some(displaced) = self.scalar.clear_sample(install.id)
                    && channel
                        .returns
                        .push(crate::assets::ReturnedSample(Box::into_raw(displaced)))
                        .is_err()
                {
                    channel
                        .leaked
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                continue;
            }
            // SAFETY: the producer boxed this pointer and handed it over via
            // the SPSC ring; this side is its unique owner until installed.
            let sample = unsafe { Box::from_raw(install.sample) };
            match self.scalar.install_sample(install.id, sample) {
                Ok(Some(displaced)) => {
                    if channel
                        .returns
                        .push(crate::assets::ReturnedSample(Box::into_raw(displaced)))
                        .is_err()
                    {
                        channel
                            .leaked
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                Ok(None) => {}
                Err(rejected) => {
                    // Out-of-range id: hand it back rather than freeing here.
                    if channel
                        .returns
                        .push(crate::assets::ReturnedSample(Box::into_raw(rejected)))
                        .is_err()
                    {
                        channel
                            .leaked
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
    }

    pub fn new(sample_rate: u32, voice_capacity: usize) -> Result<Self, String> {
        Self::with_dispatch(sample_rate, voice_capacity, DspDispatch::automatic())
    }

    /// Prepare live DSP with the same kernel choice as its producer assets.
    pub fn with_dispatch(
        sample_rate: u32,
        voice_capacity: usize,
        dispatch: DspDispatch,
    ) -> Result<Self, String> {
        if voice_capacity == 0 || voice_capacity > MAX_LIVE_VOICES {
            return Err(format!(
                "live voice capacity must be within 1..={MAX_LIVE_VOICES}, got {voice_capacity}"
            ));
        }
        let mut scalar =
            ScalarBackend::prepared_with_dispatch(sample_rate, voice_capacity, dispatch)?;
        // IR synthesis must never run inside the audio callback; the device
        // producer generates and ships through the reverb install ring.
        scalar.forbid_inline_reverb();
        Ok(Self {
            scalar,
            held: None,
            generation: 0,
            sample_rate,
            stop_fade_pos: None,
            armed_duck: None,
            takeover_cut_frame: None,
            restart_floor: None,
            line_arm_drop_from: None,
            line_arm_fired: false,
            #[cfg(feature = "device-audio")]
            confirmation_stopped: false,
            #[cfg(feature = "device-audio")]
            confirmation_takeover: 0,
        })
    }

    pub const fn dispatch(&self) -> DspDispatch {
        self.scalar.dispatch()
    }

    pub fn set_ui_visual_capture_mask(&mut self, mask: u64) {
        self.scalar.set_ui_visual_capture_mask(mask);
    }

    pub fn set_ui_visual_capture_generation_floor(&mut self, generation: u64) {
        self.scalar
            .set_ui_visual_capture_generation_floor(generation);
    }

    pub fn ui_visual_mix(&self, slot: usize, frames: usize) -> &[f32] {
        self.scalar.ui_visual_mix(slot, frames)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Length of the stop ramp in frames: 10 ms.
    fn stop_ramp_frames(&self) -> usize {
        (self.sample_rate / 100).max(1) as usize
    }

    /// True from the block which ends the stop ramp and clears the voices,
    /// until the first block after the flag clears.
    ///
    /// A host waits for this before closing its stream. A stream closed
    /// earlier cuts the ramp and leaves a step in the output.
    pub fn stop_ramp_complete(&self) -> bool {
        self.stop_fade_pos
            .is_some_and(|rendered| rendered >= self.stop_ramp_frames())
    }

    /// Consume one stereo block at an absolute sample-clock position.
    ///
    /// Besides the event `ring`, it reads the producer's side of the
    /// handshake: the four words in [`LiveFlipAtomics`] (the newest
    /// generation, its takeover frame, its [`TakeoverCut`] and a launch's
    /// pre-armed line) and `stopped`, under which the block fades whatever
    /// sounds over 10 ms and admits nothing. They are passed as references
    /// rather than owned by this struct: the audio callback must read the
    /// live values, and a copy taken at construction would pin a generation
    /// that has since been replaced. The generation word is read at the
    /// block start and again before every ring event, so a flip published
    /// while the block is assembled is adopted before the remaining events
    /// are judged. It is read once more after the drain: a flip that lands
    /// after the last event has been judged still retires the onsets it
    /// replaces before DSP runs, and its restart floor lands on the next
    /// block, whose drain carries the replacement's events.
    ///
    /// The stop ramp carries across blocks shorter than 10 ms and runs to
    /// its end also when `stopped` clears inside the ramp.
    /// [`Self::stop_ramp_complete`] tells a host when to close its stream.
    pub fn process_block_with(
        &mut self,
        output: &mut [f32],
        frames: usize,
        start_frame: u64,
        ring: &Ring,
        flip: LiveFlipAtomics<'_>,
        stopped: &AtomicBool,
    ) -> LiveBlockReport {
        self.process_block_with_assets(output, frames, start_frame, ring, flip, stopped, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_block_with_assets(
        &mut self,
        output: &mut [f32],
        frames: usize,
        start_frame: u64,
        ring: &Ring,
        flip: LiveFlipAtomics<'_>,
        stopped: &AtomicBool,
        assets: Option<&crate::assets::SampleChannel>,
    ) -> LiveBlockReport {
        let LiveFlipAtomics {
            generation: active_generation,
            takeover_frame,
            takeover_cut,
            line_arm,
        } = flip;
        let Some(samples) = frames.checked_mul(2) else {
            output.fill(0.0);
            return LiveBlockReport::default();
        };
        if output.len() < samples {
            output.fill(0.0);
            return LiveBlockReport::default();
        }
        output[..samples].fill(0.0);
        let is_stopped = stopped.load(Ordering::Acquire);
        #[cfg(feature = "device-audio")]
        {
            self.confirmation_stopped = is_stopped;
            if let Some(consumer) = &mut self.scalar.confirmations {
                consumer.begin_block(start_frame, start_frame.saturating_add(frames as u64));
            }
        }
        // A stop runs its whole ramp, also when the flag clears inside the
        // ramp. The voices then end on zero and never return at full level.
        let fade_total = self.stop_ramp_frames();
        let ramping = self
            .stop_fade_pos
            .is_some_and(|rendered| rendered < fade_total);
        if is_stopped || ramping {
            self.held = None;
            // Ramp running voices to exactly zero over 10 ms, carrying the
            // fade across as many host blocks as necessary.
            let fade_pos = self.stop_fade_pos.get_or_insert(0);
            if *fade_pos < fade_total {
                // A stop admits nothing: an onset still pending must not
                // start a voice under the ramp.
                self.scalar.retire_pending_at(start_frame);
                self.scalar.process_block(&mut output[..samples], frames);
                for frame in 0..frames {
                    let position = *fade_pos + frame;
                    let gain = if position + 1 >= fade_total {
                        0.0
                    } else {
                        1.0 - (position + 1) as f32 / fade_total as f32
                    };
                    output[frame * 2] *= gain;
                    output[frame * 2 + 1] *= gain;
                }
                *fade_pos += frames;
            }
            // The voices live until the ramp ends. A reset after the first
            // block would end a 10 ms ramp at the block edge with a step.
            // After the ramp, reset_at runs on every stopped block. The
            // call is cheap once the voices are gone (orbit lines clear only
            // while still active) and keeps the clock aligned for a host
            // which resumes without tearing the device down.
            if *fade_pos >= fade_total {
                self.scalar
                    .reset_at(start_frame.saturating_add(frames as u64));
            }
            return LiveBlockReport::default();
        }
        self.stop_fade_pos = None;

        // Reload takeover: the producer publishes (before the generation
        // flip) the frame where the NEW generation's re-query cursor starts.
        // Old-generation events BEFORE that frame cover the already-scheduled
        // horizon and still sound - dropping them cuts the music by one
        // continuity margin on every save. Events at or after it are replaced
        // by the new generation. A cut takeover (a rewind) additionally
        // silences the old rendition - from the FLIP for an immediate
        // restart, from the takeover frame for a quantised one (its
        // countdown plays) - so the restarted loop is heard alone.
        let generation = active_generation.load(Ordering::Acquire);
        // The pre-armed line cut: a launch names a frame and asks for the
        // outgoing rendition to be silenced at it, before any replacement
        // exists. The first block that reaches the frame fires the cut:
        // voices fade over the choke ramp, pending onsets retire, and (with
        // the drop bit set) outgoing ring events from the line on are
        // refused until the flip. A late evaluation then cannot let the old
        // score play through the line and sound its first beat twice. A
        // flip that lands before the line supersedes the arm (cleared
        // below), because its own cut intent carries the handoff.
        let arm_word = line_arm.load(Ordering::Acquire);
        let line_frame = arm_word >> 2;
        // Release a fired arm when its launch is withdrawn (the evaluation
        // failed or was cancelled after the line, so no flip is coming).
        // The outgoing rendition's later events pass again, and the next
        // launch's arm can fire. A latch left set would block the line cut
        // of the next quantised rewind until some later flip. A published
        // flip stores zero, not this marker, and resets the latch on its
        // own path below.
        if arm_word == LINE_ARM_WITHDRAWN && self.line_arm_fired {
            self.line_arm_fired = false;
            self.line_arm_drop_from = None;
        }
        if arm_word & 0b01 != 0
            && !self.line_arm_fired
            && start_frame.wrapping_add(frames as u64) > line_frame
        {
            self.line_arm_fired = true;
            if arm_word & 0b10 != 0 {
                self.line_arm_drop_from = Some(line_frame);
            }
            self.scalar.retire_pending_from(line_frame, start_frame);
            // The outgoing generation is the one this consumer still plays.
            // No flip has been seen yet, so it is `self.generation`. The
            // published word minus one names the generation before the
            // outgoing one. With that value, a countdown onset that
            // activates in this block fails the scalar's generation guard
            // and rings through the line without a fade.
            self.scalar.begin_takeover_cut(line_frame, self.generation);
        }
        if generation != self.generation {
            let (takeover, cut_intent) = published_cutover(takeover_frame, takeover_cut);
            self.adopt_flip(generation, takeover, cut_intent, start_frame, start_frame);
        }

        let end_frame = start_frame.saturating_add(frames as u64);
        let mut report = LiveBlockReport::default();
        for mut event in self
            .held
            .take()
            .into_iter()
            .chain(ring.drain_queued_available())
        {
            // Reload can occur while this block is being assembled. Consult
            // the atomic for every event, so old events queued ahead of a new
            // generation cannot block or enter the new backend.
            let latest_generation = active_generation.load(Ordering::Acquire);
            if latest_generation != self.generation {
                // A flip seen only mid-drain is adopted exactly as at the
                // block start: this block's remaining events are the new
                // generation's to judge.
                let (takeover, cut_intent) = published_cutover(takeover_frame, takeover_cut);
                self.adopt_flip(
                    latest_generation,
                    takeover,
                    cut_intent,
                    start_frame,
                    start_frame,
                );
            }
            // The pre-armed line's drop horizon, latched when the arm
            // fires. Until the flip, outgoing events from the line on are
            // the ghost window that the restart replaces. Without this
            // horizon, a slow evaluation lets the old score's first beat
            // sound in the late-flip window. Events before the line still
            // cover the already-scheduled horizon and pass.
            // Before the flip, the outgoing generation is this consumer's
            // generation, so the test is "outgoing or older". A "not mine"
            // test would drop nothing. Newer events are the restart itself
            // and pass: the producer publishes their generation before it
            // pushes them, and the per-event check above adopts it.
            if let Some(line_horizon) = self.line_arm_drop_from
                && !event.controls.piano
                && event.generation <= self.generation
                && event.target_frame >= line_horizon
            {
                report.stale += 1;
                #[cfg(feature = "device-audio")]
                if let (Some(consumer), Some(tag)) =
                    (&mut self.scalar.confirmations, event.confirmation)
                {
                    consumer.superseded(tag);
                }
                continue;
            }
            // The latched cut's drop horizon: onsets the outgoing
            // generation scheduled from the cut point on are part of what
            // the restart replaces, so they must not sound under the new
            // loop. AtFlip cuts from its flip block; AtTakeover keeps the
            // countdown's own onsets until the line. Events BEFORE the
            // horizon still cover the already-scheduled horizon and pass.
            if let Some(cut_horizon) = self.takeover_cut_frame
                && !event.controls.piano
                && event.generation != self.generation
                && event.target_frame >= cut_horizon
            {
                report.stale += 1;
                #[cfg(feature = "device-audio")]
                if let (Some(consumer), Some(tag)) =
                    (&mut self.scalar.confirmations, event.confirmation)
                {
                    consumer.superseded(tag);
                }
                continue;
            }
            if !event.controls.piano
                && event.generation != self.generation
                && event.target_frame >= self.replaced_from(event.generation, takeover_frame)
            {
                report.stale += 1;
                #[cfg(feature = "device-audio")]
                if let (Some(consumer), Some(tag)) =
                    (&mut self.scalar.confirmations, event.confirmation)
                {
                    consumer.superseded(tag);
                }
                continue;
            }
            // A voice the takeover kept already plays this onset: an SBD
            // graph that started early, or an onset the outgoing generation
            // started before a late flip landed. This copy would sound it a
            // second time. A generation that a newer flip replaced can have
            // no marked voice. Its SBD copy is also left out when an older
            // graph has started within one frame of it.
            let early_graph = matches!(event.synth, Some(crate::backend::SynthSource::Sbd { .. }));
            if !event.controls.piano
                && (self.scalar.kept_onset_stands_for(
                    event.target_frame,
                    event.generation,
                    early_graph,
                ) || (early_graph
                    && event.generation != self.generation
                    && self
                        .scalar
                        .started_graph_plays(event.target_frame, event.generation)))
            {
                report.stale += 1;
                #[cfg(feature = "device-audio")]
                if let (Some(consumer), Some(tag)) =
                    (&mut self.scalar.confirmations, event.confirmation)
                {
                    consumer.superseded(tag);
                }
                continue;
            }
            // A restart's first window lands whole: onsets the restarted
            // generation aimed behind its flip start AT the flip.
            if let Some((generation, floor)) = self.restart_floor
                && !event.controls.piano
                && event.generation == generation
                && event.target_frame < floor
            {
                event.event.target_frame = floor;
            }
            // Arm the sidechain at FIRST SIGHT of the event, at schedule
            // time: the duck's 10 ms pre-hold must still be in the future so
            // the dip ramps in full. Arming only at admission (the onset's
            // own block) compressed the ramp about tenfold into a thump on
            // every duck trigger.
            if self.armed_duck != Some(event.onset_id) {
                self.scalar
                    .arm_duck_at(event.target_frame, event.controls.duck);
                self.armed_duck = Some(event.onset_id);
            }
            // Admit ahead of the block. Scalar pending activates onsets only
            // when due, and early admission lets the sidechain's 10 ms
            // pre-hold (and any future automation) arm at its true time.
            // With a single-block window, a duck arms at most one host
            // buffer before the beat, and a small-buffer host compresses
            // the ramp into a thump.
            // 2 s covers every device's lead plus the producer horizon.
            let admit_horizon = end_frame.saturating_add(u64::from(self.sample_rate) * 2);
            if event.target_frame >= admit_horizon {
                self.held = Some(event);
                break;
            }
            if event.target_frame < start_frame {
                report.late += 1;
            }
            if self.scalar.try_note_prepared_with_confirmation(
                OnsetEvent::new(
                    event.target_frame,
                    event.freq_hz,
                    event.gain,
                    event.duration_secs,
                )
                .with_onset_lead(event.onset_lead)
                .with_generation(event.generation)
                .with_ui_visuals(event.ui_visuals)
                .with_controls(event.controls)
                .with_optional_sample(event.sample)
                .with_optional_wavetable(event.wavetable)
                .with_optional_synth(event.synth)
                .with_cut(event.cut),
                event.confirmation,
                event.expected_sample_identity,
            ) {
                report.accepted += 1;
            } else {
                report.refused += 1;
            }
        }

        // Close the window after the last ring read and before DSP. A reload
        // racing there must retire the replaced (post-takeover) onsets at
        // this block boundary; earlier onsets keep playing per the takeover
        // contract above. A published cut cuts at this flip and drops the
        // old generation's ghost window exactly as the first flip site does.
        let latest_generation = active_generation.load(Ordering::Acquire);
        if latest_generation != self.generation {
            // The replacement's events are drained from the NEXT block on,
            // so that block's start is where a restart's first window
            // lands.
            let (takeover, cut_intent) = published_cutover(takeover_frame, takeover_cut);
            self.adopt_flip(
                latest_generation,
                takeover,
                cut_intent,
                start_frame,
                end_frame,
            );
        }
        #[cfg(feature = "device-audio")]
        if let Some(consumer) = &mut self.scalar.confirmations {
            consumer.selected(self.generation, self.confirmation_takeover);
        }
        // The producer publishes inserts before their events. Take the
        // inserts after the event ring, so each onset finds its insert.
        if let Some(assets) = assets {
            self.drain_insert_installs(assets);
        }
        self.scalar.process_block(output, frames);
        report
    }

    /// Adopt a published generation flip. A block can observe a flip at
    /// three places (its start, mid-drain, its end). All three call this
    /// one body, so they stay identical. Runs on the audio callback: no
    /// allocation, no lock.
    ///
    /// `start_frame` is the block being assembled. `restart_floor_from` is
    /// where a restart's first window lands: this block's start when the
    /// flip is seen before the drain finishes, the next block's start when
    /// it is seen after (the replacement's events are drained from there).
    fn adopt_flip(
        &mut self,
        generation: u64,
        takeover: u64,
        cut_intent: TakeoverCut,
        start_frame: u64,
        restart_floor_from: u64,
    ) {
        // The rendition going out is the one this consumer was playing.
        // Not `generation - 1`: a slider requery superseded by a launch
        // before it published skips a generation (G audible, G+2
        // published), and a pre-fade guarded on G+1 caught nothing.
        let outgoing = self.generation;
        self.generation = generation;
        // A published flip supersedes the pre-armed line: from here the
        // flip's own cut intent and takeover frame carry the handoff. A
        // latch left standing (as the mid-drain site once did) kept the
        // NEXT launch's arm from ever firing.
        self.line_arm_drop_from = None;
        self.line_arm_fired = false;
        #[cfg(feature = "device-audio")]
        {
            self.confirmation_takeover = takeover;
        }
        // The cut intent selects where the outgoing rendition falls silent.
        // AtFlip (immediate rewind): the cut lands at the flip and takes
        // the whole outgoing rendition. Pending retires from the block,
        // voices fade at this frame, the scalar arms against outgoing
        // constructions, and the ring drops every old event from this
        // frame on. Otherwise the old rendition's next beat, already
        // scheduled into the lead between the flip and the takeover,
        // sounds after the cut: the restarted loop's first beat sounds
        // twice. AtTakeover (quantised rewind): the old score plays its
        // countdown to the line, so pending keeps the takeover horizon and
        // the scalar arms at the takeover frame instead. None (an edit):
        // the old voices ring out without a click.
        match cut_intent {
            TakeoverCut::AtFlip => {
                self.scalar.retire_pending_from(start_frame, start_frame);
                self.scalar.begin_takeover_cut(start_frame, outgoing);
                self.takeover_cut_frame = Some(start_frame);
            }
            TakeoverCut::AtTakeover => {
                self.scalar.retire_pending_from(takeover, start_frame);
                // Arming with the future frame fades the voices sounding now
                // at the line and pre-fades countdown activations to stop
                // there; the ordinary takeover horizon already drops the old
                // ring events at/after the line.
                self.scalar.begin_takeover_cut(takeover, outgoing);
                self.takeover_cut_frame = None;
            }
            TakeoverCut::None => {
                // An edit's flip (a control requery) can land inside a
                // quantised rewind's head-room, before its line. The
                // rewind's arm names its own outgoing generation and
                // expires once its frame is behind: cleared here, a
                // countdown onset activating after this flip rang past the
                // line under the restarted loop. And the countdown is not
                // the edit's to replace: the requery re-queries the
                // restarted score, which has nothing before the line, so
                // the outgoing generation keeps its onsets up to the line
                // (the ring keeps them too, see `replaced_from`). Only an
                // arm at or behind the block has nothing left to do.
                match self.scalar.armed_takeover_cut {
                    Some(line) if line.0 > start_frame => {
                        self.scalar.retire_pending_keeping_countdown(
                            generation,
                            takeover,
                            line,
                            start_frame,
                        );
                    }
                    _ => {
                        self.scalar.armed_takeover_cut = None;
                        self.scalar
                            .hand_over_from(generation, takeover, start_frame);
                    }
                }
                self.takeover_cut_frame = None;
            }
        }
        self.restart_floor = cut_intent
            .is_cut()
            .then_some((generation, restart_floor_from));
    }

    /// Where the ring stops admitting an older generation's events: the
    /// published takeover, where the newest generation's re-query starts,
    /// except for the generation a takeover cut still armed names as
    /// outgoing, whose events stop at the cut frame. That generation is a
    /// quantised rewind's countdown: an edit's flip inside the head-room
    /// moved the takeover word to the requery's own takeover, often before
    /// the line, and the countdown's events between the two were refused
    /// with nothing to replace them. At the cut frame the same rule refuses
    /// the countdown's ghost past the line whatever the requery's takeover.
    /// Read per event, like the takeover word; nothing allocates.
    fn replaced_from(&self, generation: u64, takeover_frame: &AtomicU64) -> u64 {
        match self.scalar.armed_takeover_cut {
            Some((cut_frame, outgoing)) if generation == outgoing => cut_frame,
            _ => takeover_frame.load(Ordering::Acquire),
        }
    }
}

/// The takeover frame and cut intent published with a flip, read in the
/// producer's order (the generation has already been read by the caller).
fn published_cutover(takeover_frame: &AtomicU64, takeover_cut: &AtomicU64) -> (u64, TakeoverCut) {
    let takeover = takeover_frame.load(Ordering::Acquire);
    let cut_intent = TakeoverCut::from_bits(takeover_cut.load(Ordering::Acquire));
    (takeover, cut_intent)
}

#[cfg(all(test, feature = "device-audio"))]
mod sample_lifetime_tests {
    use super::*;

    #[test]
    fn sample_lifetime_frontier_covers_a_delayed_restart_after_an_edit() {
        const RATE: u32 = 48_000;
        const BLOCK: usize = 128;
        const START: u64 = 8_192;
        let decoded = crate::DecodedSample::from_parts(RATE, 1, vec![0.5; 9_600]).unwrap();
        // A flip observed after the drain rebases its first notes to the
        // following block. Otherwise they start in the current block.
        for after_drain in [false, true] {
            let mut live = LiveScalarBackend::new(RATE, 4).expect("live backend");
            live.generation = 1;
            live.scalar.reset_at(START);
            assert!(
                live.scalar
                    .install_sample(crate::SampleId(9), Box::new(decoded.clone()))
                    .is_ok()
            );
            let event = crate::AudioEvent {
                onset_id: 1,
                generation: 2,
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 1.0,
                duration_secs: 0.1,
                ui_visuals: 0,
                controls: Default::default(),
                sample: Some(crate::SampleControls {
                    sample: crate::SampleId(9),
                    playback_rate: 1.0,
                    begin: 0.0,
                    end: 1.0,
                    hold: crate::SampleHold::Slice,
                    muted: false,
                    loop_secs: None,
                    envelope_peak: 1.0,
                    reversed: false,
                    nudge_secs: 0.0,
                    cut: None,
                }),
                synth: None,
                wavetable: None,
                cut: None,
            };
            let generation = AtomicU64::new(2);
            let takeover = AtomicU64::new(0);
            let cut = AtomicU64::new(1);
            let line = AtomicU64::new(0);
            let stopped = AtomicBool::new(false);
            let flip = LiveFlipAtomics {
                generation: &generation,
                takeover_frame: &takeover,
                takeover_cut: &cut,
                line_arm: &line,
            };
            let ring = Ring::new(4);
            assert!(ring.push(event));
            let (_, original_deadline) = event.sample_end_frame(&decoded, RATE).unwrap();
            // The producer reads the frontier after a successful push:
            // either the next block's start or the announced in-flight end.
            let frontier = START + if after_drain { BLOCK as u64 } else { 0 };
            let retained = crate::AudioEvent {
                target_frame: event.target_frame.max(frontier),
                ..event
            };
            let (_, retained_deadline) = retained.sample_end_frame(&decoded, RATE).unwrap();
            let mut output = [0.0; BLOCK * 2];
            let mut frame = START;
            if after_drain {
                // Publication landed after this block's ring snapshot;
                // its queued replacement is first drained next block.
                live.adopt_flip(2, 0, TakeoverCut::AtFlip, frame, frontier);
                live.scalar.process_block(&mut output, BLOCK);
                frame += BLOCK as u64;
            }
            assert_eq!(
                live.process_block_with(&mut output, BLOCK, frame, &ring, flip, &stopped)
                    .accepted,
                1
            );
            frame += BLOCK as u64;
            // Removing the sound with an ordinary edit clears the restart
            // floor while the rebased note keeps ringing.
            takeover.store(frame, Ordering::Release);
            cut.store(0, Ordering::Release);
            generation.store(3, Ordering::Release);
            let mut sounded_after_original_deadline = false;
            while frame + BLOCK as u64 <= retained_deadline {
                live.process_block_with(&mut output, BLOCK, frame, &ring, flip, &stopped);
                if frame > original_deadline && output.iter().any(|sample| sample.abs() > 1e-5) {
                    sounded_after_original_deadline = true;
                }
                frame += BLOCK as u64;
            }
            assert_eq!(live.restart_floor, None);
            assert!(sounded_after_original_deadline);
            assert!(
                !live.scalar.score_sources_active(),
                "the rebased voice must retire before its sample retention expires"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    mod fx_reverb {
        use super::*;
        use crate::assets::{FxReverbInstall, ReturnedFxReverb, SampleChannel};
        use crate::reverb::{OrbitReverb, ReverbParams};
        use std::sync::atomic::Ordering;

        fn room(size_secs: f32) -> Box<OrbitReverb> {
            Box::new(OrbitReverb::generate_streaming(
                48_000,
                ReverbParams {
                    size_secs,
                    fade_secs: 0.01,
                    lp_start_hz: 15_000.0,
                    lp_end_hz: 1_000.0,
                    ir: None,
                },
            ))
        }

        fn enqueue(channel: &SampleChannel, reverb: Box<OrbitReverb>) -> *mut OrbitReverb {
            let reverb = Box::into_raw(reverb);
            assert!(
                channel
                    .fx_reverb_installs
                    .push(FxReverbInstall { reverb })
                    .is_ok()
            );
            reverb
        }

        fn fill_returns(channel: &SampleChannel, count: usize) {
            for _ in 0..count {
                assert!(
                    channel
                        .fx_reverb_returns
                        .push(ReturnedFxReverb(Box::into_raw(room(0.001))))
                        .is_ok()
                );
            }
        }

        #[test]
        fn a_full_return_ring_defers_install_ownership_until_reclaimed() {
            let mut live = LiveScalarBackend::new(48_000, 8).expect("live backend");
            let channel = SampleChannel::new();
            fill_returns(&channel, channel.fx_reverb_returns.capacity());
            let pending = room(0.001);
            let bytes = pending.approx_bytes();
            enqueue(&channel, pending);

            live.drain_reverb_installs(&channel);
            assert_eq!(channel.fx_reverb_installs.len(), 1);
            assert_eq!(live.scalar.fx_reverb_resident_bytes(), 0);
            assert_eq!(channel.fx_reverb_refusals.load(Ordering::Acquire), 0);
            assert_eq!(channel.leaked.load(Ordering::Acquire), 0);

            channel.reclaim();
            live.drain_reverb_installs(&channel);
            assert_eq!(channel.fx_reverb_installs.len(), 0);
            assert_eq!(
                channel.fx_reverb_resident_bytes.load(Ordering::Acquire),
                bytes
            );
            assert_eq!(channel.leaked.load(Ordering::Acquire), 0);
        }

        #[test]
        fn a_refusal_that_fills_the_return_ring_defers_the_next_install() {
            let mut live = LiveScalarBackend::new(48_000, 8).expect("live backend");
            let channel = SampleChannel::new();
            let first = room(6.0);
            let bytes = first.approx_bytes();
            assert!(2 * bytes <= crate::scalar::MAX_FX_REVERB_BYTES);
            assert!(3 * bytes > crate::scalar::MAX_FX_REVERB_BYTES);
            assert!(live.scalar.install_fx_reverb(first).is_none());
            assert!(live.scalar.install_fx_reverb(room(6.0)).is_none());
            fill_returns(&channel, channel.fx_reverb_returns.capacity() - 1);
            let refused = enqueue(&channel, room(6.0));
            enqueue(&channel, room(6.0));

            live.drain_reverb_installs(&channel);
            assert_eq!(channel.fx_reverb_installs.len(), 1);
            assert_eq!(
                channel.fx_reverb_returns.len(),
                channel.fx_reverb_returns.capacity()
            );
            assert_eq!(channel.fx_reverb_refusals.load(Ordering::Acquire), 1);
            assert_eq!(channel.leaked.load(Ordering::Acquire), 0);
            assert_eq!(
                channel.fx_reverb_resident_bytes.load(Ordering::Acquire),
                2 * bytes
            );

            let mut returned = Vec::new();
            channel.reclaim_with_fx(|pointer| returned.push(pointer));
            assert!(returned.contains(&refused));
            live.drain_reverb_installs(&channel);
            assert_eq!(channel.fx_reverb_installs.len(), 0);
            assert_eq!(channel.fx_reverb_returns.len(), 1);
            assert_eq!(channel.fx_reverb_refusals.load(Ordering::Acquire), 1);
            assert_eq!(channel.leaked.load(Ordering::Acquire), 0);
            assert_eq!(
                channel.fx_reverb_resident_bytes.load(Ordering::Acquire),
                2 * bytes
            );
        }
    }

    use super::*;

    #[test]
    fn an_insert_published_after_callback_entry_reaches_its_first_onset() {
        use crate::assets::{InsertInstall, SampleChannel};
        use crate::insert::{InsertControls, InsertKey, InsertParam, OrbitInsert};
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        struct Probe(InsertKey, Arc<AtomicUsize>);
        impl OrbitInsert for Probe {
            fn key(&self) -> InsertKey {
                self.0
            }
            fn set_param(&mut self, _: InsertParam, _: u32) {}
            fn process(&mut self, _: &mut [f32], _: &mut [f32]) {
                self.1.fetch_add(1, Ordering::Relaxed);
            }
        }

        let mut live = LiveScalarBackend::new(48_000, 8).expect("live backend");
        let channel = SampleChannel::new();
        let ring = Ring::new(8);
        let words = Words::new();
        let key = InsertKey {
            plugin: 1,
            preset: 0,
        };
        let processed = Arc::new(AtomicUsize::new(0));
        live.drain_insert_installs(&channel);
        // The producer publishes between callback entry and the event drain.
        let insert = Box::new(Probe(key, Arc::clone(&processed))) as Box<dyn OrbitInsert>;
        assert!(
            channel
                .insert_installs
                .push(InsertInstall {
                    slot: 0,
                    sample_rate: 48_000,
                    insert: Box::into_raw(insert),
                })
                .is_ok()
        );
        let mut event = note(1, 1, 0);
        event.controls.orbit = 0;
        event.controls.effects[0] = Some(InsertControls::new(key));
        assert!(ring.push(event));
        let report = live.process_block_with_assets(
            &mut [0.0; BLOCK * 2],
            BLOCK,
            0,
            &ring,
            words.flip(),
            &words.stopped,
            Some(&channel),
        );
        assert_eq!(report.accepted, 1);
        assert_eq!(live.scalar.missing_insert_events(), 0);
        assert!(processed.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn a_replacement_insert_waits_for_its_first_onset() {
        use crate::assets::{InsertInstall, ReturnedInsert, SampleChannel};
        use crate::insert::{InsertControls, InsertKey, InsertParam, OrbitInsert};
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        struct Probe(InsertKey, Arc<AtomicUsize>, Arc<AtomicUsize>);
        impl Drop for Probe {
            fn drop(&mut self) {
                self.2.fetch_add(1, Ordering::Relaxed);
            }
        }
        impl OrbitInsert for Probe {
            fn key(&self) -> InsertKey {
                self.0
            }
            fn set_param(&mut self, _: InsertParam, _: u32) {}
            fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
                if left.iter().chain(right.iter()).any(|sample| *sample != 0.0) {
                    self.1.fetch_add(left.len(), Ordering::Relaxed);
                }
            }
        }

        for canceled in [false, true] {
            let mut live = LiveScalarBackend::new(48_000, 8).expect("live backend");
            let channel = SampleChannel::new();
            let ring = Ring::new(8);
            let words = Words::new();
            let old = InsertKey {
                plugin: 1,
                preset: 0,
            };
            let new = InsertKey { plugin: 2, ..old };
            let old_frames = Arc::new(AtomicUsize::new(0));
            let new_frames = Arc::new(AtomicUsize::new(0));
            let old_drops = Arc::new(AtomicUsize::new(0));
            let new_drops = Arc::new(AtomicUsize::new(0));
            let unused_drops = Arc::new(AtomicUsize::new(0));
            let slot = crate::insert::effect_slot(1, 0);
            live.scalar.install_insert(
                slot,
                Box::new(Probe(old, Arc::clone(&old_frames), Arc::clone(&old_drops))),
            );
            let mut event = note(1, 1, 0);
            event.controls.orbit = 1;
            event.controls.effects[0] = Some(InsertControls::new(old));
            assert!(ring.push(event));

            for block in 0..=4 {
                let onset = (3 * BLOCK) as u64;
                if block == 1 {
                    let insert =
                        Box::new(Probe(new, Arc::clone(&new_frames), Arc::clone(&new_drops)))
                            as Box<dyn OrbitInsert>;
                    assert!(
                        channel
                            .insert_installs
                            .push(InsertInstall {
                                slot: slot as u8,
                                sample_rate: 48_000,
                                insert: Box::into_raw(insert),
                            })
                            .is_ok()
                    );
                    words.publish(2, onset, TakeoverCut::None);
                    let mut event = note(2, 2, onset);
                    event.controls.orbit = 1;
                    event.controls.effects[0] = Some(InsertControls::new(new));
                    assert!(ring.push(event));
                } else if block == 2 && canceled {
                    words.publish(3, onset, TakeoverCut::None);
                    let mut event = note(3, 3, onset);
                    event.controls.orbit = 1;
                    event.controls.effects[0] = Some(InsertControls::new(old));
                    assert!(ring.push(event));
                } else if block == 4 && !canceled {
                    for _ in 0..channel.insert_returns.capacity() {
                        let insert = Box::new(Probe(
                            new,
                            Arc::clone(&new_frames),
                            Arc::clone(&unused_drops),
                        )) as Box<dyn OrbitInsert>;
                        assert!(
                            channel
                                .insert_returns
                                .push(ReturnedInsert(Box::into_raw(insert)))
                                .is_ok()
                        );
                    }
                    let insert = Box::new(Probe(
                        InsertKey { plugin: 3, ..new },
                        Arc::clone(&new_frames),
                        Arc::clone(&unused_drops),
                    )) as Box<dyn OrbitInsert>;
                    assert!(
                        channel
                            .insert_installs
                            .push(InsertInstall {
                                slot: slot as u8,
                                sample_rate: 48_000,
                                insert: Box::into_raw(insert),
                            })
                            .is_ok()
                    );
                }
                let before = old_frames.load(Ordering::Relaxed);
                let mut output = [0.0; BLOCK * 2];
                live.process_block_with_assets(
                    &mut output,
                    BLOCK,
                    (block * BLOCK) as u64,
                    &ring,
                    words.flip(),
                    &words.stopped,
                    Some(&channel),
                );
                assert!(output.iter().any(|sample| *sample != 0.0));
                if block < 3 || canceled {
                    assert!(
                        old_frames.load(Ordering::Relaxed) > before,
                        "the old insert processes until an accepted replacement onset"
                    );
                    assert_eq!(new_frames.load(Ordering::Relaxed), 0);
                } else {
                    assert_eq!(old_frames.load(Ordering::Relaxed), before);
                    assert!(new_frames.load(Ordering::Relaxed) > 0);
                }
                assert_eq!(old_drops.load(Ordering::Relaxed), 0);
                assert_eq!(new_drops.load(Ordering::Relaxed), 0);
                assert_eq!(unused_drops.load(Ordering::Relaxed), 0);
            }
            assert_eq!(live.scalar.missing_insert_events(), 0);
            assert_eq!(channel.insert_installs.len(), usize::from(!canceled));
            channel.reclaim();
            assert_eq!(old_drops.load(Ordering::Relaxed), 0);
            live.drain_insert_installs(&channel);
            assert_eq!(channel.insert_installs.len(), 0);
            assert_eq!(old_drops.load(Ordering::Relaxed), 0);
            channel.reclaim();
            assert_eq!(old_drops.load(Ordering::Relaxed), usize::from(!canceled));
            if !canceled {
                assert_eq!(new_drops.load(Ordering::Relaxed), 0);
                assert_eq!(
                    unused_drops.load(Ordering::Relaxed),
                    channel.insert_returns.capacity()
                );
            }
            assert_eq!(channel.leaked.load(Ordering::Relaxed), 0);
            drop(live);
            let unused = if canceled {
                0
            } else {
                channel.insert_returns.capacity() + 1
            };
            drop(channel);
            assert_eq!(old_drops.load(Ordering::Relaxed), 1);
            assert_eq!(new_drops.load(Ordering::Relaxed), 1);
            assert_eq!(unused_drops.load(Ordering::Relaxed), unused);
        }
    }

    /// The one flip body every site calls: it names the generation it
    /// replaces as the outgoing one (across a skipped generation too),
    /// clears the line arm's latch, arms a rewind's cut, keeps a rewind's
    /// arm across an edit's flip while its line is still ahead, and lands a
    /// restart's floor where the caller says its first window is drained.
    #[test]
    fn adopt_flip_hands_over_in_one_place() {
        let mut live = LiveScalarBackend::new(48_000, 8).expect("live backend");
        live.generation = 1;
        live.line_arm_fired = true;
        live.line_arm_drop_from = Some(900);

        // 1 -> 3: generation 2 never published.
        live.adopt_flip(3, 10_000, TakeoverCut::AtTakeover, 1_024, 1_024);
        assert_eq!(live.generation, 3);
        assert!(!live.line_arm_fired, "a flip lets the fired arm go");
        assert_eq!(live.line_arm_drop_from, None);
        assert_eq!(
            live.scalar.armed_takeover_cut,
            Some((10_000, 1)),
            "the arm names the generation actually playing, not new - 1"
        );
        assert_eq!(
            live.takeover_cut_frame, None,
            "the countdown keeps its ring events"
        );
        assert_eq!(live.restart_floor, Some((3, 1_024)));

        // An edit's flip inside the head-room keeps the line cut.
        live.adopt_flip(4, 12_000, TakeoverCut::None, 2_048, 2_048);
        assert_eq!(live.generation, 4);
        assert_eq!(live.scalar.armed_takeover_cut, Some((10_000, 1)));
        assert_eq!(live.restart_floor, None, "an edit is not a restart");

        // At the line the arm has nothing left to do.
        live.adopt_flip(5, 14_000, TakeoverCut::None, 10_000, 10_000);
        assert_eq!(live.scalar.armed_takeover_cut, None);

        // An immediate rewind seen after the drain: the cut is at this
        // block, the floor at the next one, where its events are drained.
        live.adopt_flip(6, 16_000, TakeoverCut::AtFlip, 16_000, 16_128);
        assert_eq!(live.scalar.armed_takeover_cut, Some((16_000, 5)));
        assert_eq!(live.takeover_cut_frame, Some(16_000));
        assert_eq!(live.restart_floor, Some((6, 16_128)));
    }

    const BLOCK: usize = 128;

    /// The producer's side of the handshake, playing generation 1.
    struct Words {
        generation: AtomicU64,
        takeover: AtomicU64,
        cut: AtomicU64,
        line: AtomicU64,
        stopped: AtomicBool,
    }

    impl Words {
        fn new() -> Self {
            Self {
                generation: AtomicU64::new(1),
                takeover: AtomicU64::new(0),
                cut: AtomicU64::new(0),
                line: AtomicU64::new(0),
                stopped: AtomicBool::new(false),
            }
        }

        fn publish(&self, generation: u64, takeover: u64, cut: TakeoverCut) {
            self.takeover.store(takeover, Ordering::Release);
            self.cut.store(cut as u64, Ordering::Release);
            self.generation.store(generation, Ordering::Release);
        }

        fn flip(&self) -> LiveFlipAtomics<'_> {
            LiveFlipAtomics {
                generation: &self.generation,
                takeover_frame: &self.takeover,
                takeover_cut: &self.cut,
                line_arm: &self.line,
            }
        }
    }

    fn note(onset_id: u64, generation: u64, target_frame: u64) -> crate::AudioEvent {
        crate::AudioEvent {
            onset_id,
            generation,
            target_frame,
            onset_lead: 0.0,
            freq_hz: 220.0,
            gain: 0.5,
            duration_secs: 0.05,
            ui_visuals: 0,
            controls: Default::default(),
            sample: None,
            synth: None,
            wavetable: None,
            cut: None,
        }
    }

    fn sbd(onset_id: u64, generation: u64, target_frame: u64) -> crate::AudioEvent {
        crate::AudioEvent {
            freq_hz: 55.0,
            duration_secs: 0.3,
            synth: Some(crate::SynthSource::Sbd {
                decay_secs: 0.2,
                pdecay_secs: 0.3,
                penv_semitones: 36.0,
                stop_secs: 0.21,
            }),
            ..note(onset_id, generation, target_frame)
        }
    }

    /// Render `blocks` blocks from frame zero. `before_block` runs ahead of
    /// each one, as the producer does between two callbacks.
    fn render(
        blocks: usize,
        ring: &Ring,
        words: &Words,
        before_block: impl FnMut(usize),
    ) -> (Vec<f32>, LiveBlockReport) {
        render_with_voices(8, blocks, ring, words, before_block)
    }

    /// [`render`] on a backend with room for `voices` voices.
    fn render_with_voices(
        voices: usize,
        blocks: usize,
        ring: &Ring,
        words: &Words,
        mut before_block: impl FnMut(usize),
    ) -> (Vec<f32>, LiveBlockReport) {
        let mut live = LiveScalarBackend::with_dispatch(48_000, voices, DspDispatch::portable())
            .expect("live backend");
        live.generation = 1;
        let mut pcm = vec![0.0; blocks * BLOCK * 2];
        let mut total = LiveBlockReport::default();
        for (index, output) in pcm.chunks_mut(BLOCK * 2).enumerate() {
            before_block(index);
            let start = (index * BLOCK) as u64;
            let report =
                live.process_block_with(output, BLOCK, start, ring, words.flip(), &words.stopped);
            total.accepted += report.accepted;
            total.stale += report.stale;
            total.refused += report.refused;
            total.late += report.late;
        }
        (pcm, total)
    }

    /// A control requery can publish after its takeover frame. The outgoing
    /// generation has then started the onsets between the two frames, and
    /// the incoming copies of them arrive late.
    #[test]
    fn a_flip_after_its_takeover_frame_does_not_sound_an_onset_twice() {
        let once = {
            let (ring, words) = (Ring::new(8), Words::new());
            assert!(ring.push(note(1, 1, 300)));
            assert!(ring.push(note(2, 1, 1_000)));
            render(24, &ring, &words, |_| {}).0
        };
        let (ring, words) = (Ring::new(8), Words::new());
        assert!(ring.push(note(1, 1, 300)));
        assert!(ring.push(note(2, 1, 1_000)));
        let (handed_over, report) = render(24, &ring, &words, |block| {
            // The flip lands on frame 384, one block after its takeover.
            if block == 3 {
                words.publish(2, 256, TakeoverCut::None);
                assert!(ring.push(note(3, 2, 300)));
                assert!(ring.push(note(4, 2, 1_000)));
            }
        });
        assert_eq!((report.stale, report.late), (1, 0));
        assert_eq!(report.accepted, 3);
        assert!(handed_over == once, "each onset sounds once");
    }

    /// An outgoing voice stands in for one incoming event aimed before the
    /// block the flip lands on. Each other incoming event sounds.
    #[test]
    fn a_late_flip_leaves_out_one_incoming_event_for_each_outgoing_voice() {
        let (ring, words) = (Ring::new(8), Words::new());
        assert!(ring.push(note(1, 1, 300)));
        let (_, report) = render(8, &ring, &words, |block| {
            if block == 3 {
                words.publish(2, 256, TakeoverCut::None);
                // The second event at 300 has no outgoing voice of its own.
                // The event at 390 is on time.
                for (id, frame) in [(2, 300), (3, 300), (4, 390)] {
                    assert!(ring.push(note(id, 2, frame)));
                }
            }
        });
        assert_eq!((report.stale, report.late), (1, 1));
        assert_eq!(report.accepted, 3);

        // The outgoing generation played nothing after the takeover frame.
        let (ring, words) = (Ring::new(8), Words::new());
        let (_, report) = render(8, &ring, &words, |block| {
            if block == 3 {
                words.publish(2, 256, TakeoverCut::None);
                assert!(ring.push(note(1, 2, 300)));
            }
        });
        assert_eq!((report.stale, report.late, report.accepted), (0, 1, 1));
    }

    /// A slider that moves sends a requery again and again. Each one lands
    /// almost two blocks after its takeover frame, over a pattern with an
    /// onset each 2 ms. The producer keeps eight blocks scheduled.
    #[test]
    fn late_flips_one_after_another_sound_each_onset_once() {
        const STEP: u64 = 96;
        const AHEAD: u64 = 8 * BLOCK as u64;
        let play = |requeries: bool| {
            let (ring, words) = (Ring::new(64), Words::new());
            let mut generation = 1;
            let mut scheduled_to = 0;
            let mut id = 0;
            render_with_voices(64, 110, &ring, &words, |block| {
                let start = (block * BLOCK) as u64;
                if requeries && block % 7 == 3 {
                    generation += 1;
                    scheduled_to = start - 2 * BLOCK as u64 + 5;
                    words.publish(generation, scheduled_to, TakeoverCut::None);
                }
                let mut frame = scheduled_to.div_ceil(STEP) * STEP;
                while frame < start + AHEAD {
                    id += 1;
                    assert!(ring.push(note(id, generation, frame)));
                    frame += STEP;
                }
                scheduled_to = start + AHEAD;
            })
        };
        let (once, report) = play(false);
        assert_eq!((report.stale, report.late, report.refused), (0, 0, 0));
        let (handed_over, report) = play(true);
        assert_eq!((report.late, report.refused), (0, 0));
        assert!(handed_over == once, "each onset sounds once");
        assert!(report.stale > 0, "test premise: copies arrive late");
    }

    /// A flip that names no takeover frame starts a new lifetime. Its
    /// events are not copies of the voices that still ring.
    #[test]
    fn a_flip_without_a_takeover_frame_leaves_out_no_event() {
        let (ring, words) = (Ring::new(8), Words::new());
        assert!(ring.push(note(1, 1, 300)));
        let (_, report) = render(8, &ring, &words, |block| {
            if block == 3 {
                words.publish(2, 0, TakeoverCut::None);
                assert!(ring.push(note(2, 2, 300)));
            }
        });
        assert_eq!((report.stale, report.late, report.accepted), (0, 1, 2));
    }

    /// A takeover that changes the tempo moves the onset. The SBD graph
    /// that has started still plays it, so the moved copy is left out.
    #[test]
    fn a_takeover_leaves_out_the_moved_copy_of_a_started_sbd_graph() {
        let once = {
            let (ring, words) = (Ring::new(8), Words::new());
            assert!(ring.push(sbd(1, 1, 4_800)));
            assert!(ring.push(sbd(2, 1, 11_000)));
            render(192, &ring, &words, |_| {}).0
        };
        let (ring, words) = (Ring::new(8), Words::new());
        assert!(ring.push(sbd(1, 1, 4_800)));
        let (handed_over, report) = render(192, &ring, &words, |block| {
            if block == 1 {
                words.publish(2, 128 + 3_840, TakeoverCut::None);
                // The copy is 8 ms early. The next hit has no voice yet.
                assert!(ring.push(sbd(2, 2, 4_800 - 384)));
                assert!(ring.push(sbd(3, 2, 11_000)));
            }
        });
        assert_eq!((report.stale, report.late, report.accepted), (1, 0, 2));
        assert!(handed_over == once, "each hit sounds once");
    }

    /// Two flips can land in one block. The device then reads the copy of
    /// the first generation under the second one, and no voice is marked
    /// for it. The started graph still plays that onset.
    #[test]
    fn a_replaced_generation_does_not_sound_a_started_sbd_graph_again() {
        let once = {
            let (ring, words) = (Ring::new(8), Words::new());
            assert!(ring.push(sbd(1, 1, 4_800)));
            render(96, &ring, &words, |_| {}).0
        };
        // The device adopts generation 2 before generation 3, or it does not
        // read generation 2 at all.
        for adopts_both in [true, false] {
            let (ring, words) = (Ring::new(8), Words::new());
            assert!(ring.push(sbd(1, 1, 4_800)));
            let (handed_over, report) = render(96, &ring, &words, |block| {
                if block == 1 && adopts_both {
                    words.publish(2, 384, TakeoverCut::None);
                }
                if block == 2 {
                    words.publish(2, 384, TakeoverCut::None);
                    assert!(ring.push(sbd(2, 2, 4_800)));
                    // The hit is before this takeover: no third copy comes.
                    words.publish(3, 4_800 + 960, TakeoverCut::None);
                }
            });
            assert_eq!((report.stale, report.accepted), (1, 1));
            assert!(handed_over == once, "the hit sounds once");
        }
    }

    /// An edit removes the hit and adds one 25 ms later. The started graph
    /// still sounds, and the new hit is too far away to be its copy.
    #[test]
    fn a_started_sbd_graph_does_not_take_a_hit_out_of_its_reach() {
        let (ring, words) = (Ring::new(8), Words::new());
        assert!(ring.push(sbd(1, 1, 4_800)));
        let (_, report) = render(8, &ring, &words, |block| {
            if block == 1 {
                words.publish(2, 128 + 3_840, TakeoverCut::None);
                assert!(ring.push(sbd(2, 2, 6_000)));
            }
        });
        assert_eq!((report.stale, report.accepted), (0, 2));
    }

    /// An SBD graph one frame before the takeover frame has started. The copy
    /// on the takeover frame is left out. A copy one frame later is another
    /// hit and sounds.
    #[test]
    fn a_takeover_leaves_out_the_copy_one_frame_after_a_started_sbd_graph() {
        const TAKEOVER: u64 = 4_800;
        let once = {
            let (ring, words) = (Ring::new(8), Words::new());
            assert!(ring.push(sbd(1, 1, TAKEOVER - 1)));
            render(96, &ring, &words, |_| {}).0
        };
        for (copy_frame, stale, accepted) in [(TAKEOVER, 1, 1), (TAKEOVER + 1, 0, 2)] {
            let (ring, words) = (Ring::new(8), Words::new());
            assert!(ring.push(sbd(1, 1, TAKEOVER - 1)));
            let (handed_over, report) = render(96, &ring, &words, |block| {
                if block == 1 {
                    words.publish(2, TAKEOVER, TakeoverCut::None);
                    assert!(ring.push(sbd(2, 2, copy_frame)));
                }
            });
            assert_eq!((report.stale, report.accepted), (stale, accepted));
            assert_eq!(handed_over == once, stale == 1, "copy at {copy_frame}");
        }
    }

    /// The outgoing graph has its hit one frame before a rewind's line, and
    /// the rewind cuts it on the line. A control requery inside the head-room
    /// must not let it stand in for the first hit of the restarted loop.
    #[test]
    fn a_cut_sbd_graph_one_frame_before_the_line_takes_no_hit() {
        const LINE: u64 = 4_800;
        let (ring, words) = (Ring::new(8), Words::new());
        assert!(ring.push(sbd(1, 1, LINE - 1)));
        let (_, report) = render(8, &ring, &words, |block| {
            if block == 1 {
                words.publish(2, LINE, TakeoverCut::AtTakeover);
            }
            if block == 2 {
                words.publish(3, 1_024, TakeoverCut::None);
                assert!(ring.push(sbd(2, 3, LINE)));
            }
        });
        assert_eq!((report.stale, report.accepted), (0, 2));
    }

    /// A rewind cuts the outgoing voices on its line. An outgoing SBD graph
    /// that has started for a hit on the line is cut where its source
    /// starts, so it cannot stand in for the first hit of the restarted loop.
    #[test]
    fn a_rewind_sounds_its_own_sbd_hit_on_the_line() {
        const LINE: u64 = 4_800;
        let (ring, words) = (Ring::new(8), Words::new());
        assert!(ring.push(sbd(1, 1, LINE)));
        let (pcm, report) = render(96, &ring, &words, |block| {
            // The outgoing graph has run since frame 0.
            if block == 1 {
                words.publish(2, LINE, TakeoverCut::AtTakeover);
                assert!(ring.push(sbd(2, 2, LINE)));
            }
        });
        assert_eq!((report.stale, report.accepted), (0, 2));
        // 20 ms after the line the cut's 10 ms ramp has ended.
        let after_ramp = (LINE as usize + 960) * 2;
        assert!(pcm[after_ramp..].iter().any(|sample| sample.abs() > 1e-2));

        // A control requery lands before the line. The graph of the
        // restarted loop takes its copy, and the cut graph takes no event.
        let (ring, words) = (Ring::new(8), Words::new());
        assert!(ring.push(sbd(1, 1, LINE)));
        let (_, report) = render(8, &ring, &words, |block| {
            if block == 1 {
                words.publish(2, LINE, TakeoverCut::AtTakeover);
                assert!(ring.push(sbd(2, 2, LINE)));
            }
            if block == 2 {
                words.publish(3, 1_024, TakeoverCut::None);
                assert!(ring.push(sbd(3, 3, LINE)));
                assert!(ring.push(sbd(4, 3, LINE + 300)));
            }
        });
        assert_eq!((report.stale, report.accepted), (1, 3));
    }
}
