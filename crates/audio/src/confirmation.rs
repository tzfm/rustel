//! Finite producer-window receipts emitted after copying rendered audio.
//!
//! These records describe a host-buffer copy, not presentation at a speaker.
//! Source text and replay settings remain on the producer side.

/// At most this many windows may be offered but not yet collected.
pub const MAX_CONFIRMATION_WINDOWS: usize = 4;
/// Converted onsets use consecutive ordinals in this fixed range.
pub const MAX_CONFIRMATION_ORDINALS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmationKey {
    pub epoch: u64,
    pub token: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmationOnset {
    pub key: ConfirmationKey,
    pub ordinal: u32,
}

/// Counts are disjoint and their sum must equal `intended`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowOffer {
    pub key: ConfirmationKey,
    pub generation: u64,
    pub takeover_frame: u64,
    pub start_frame: u64,
    pub end_frame: u64,
    pub intended: u32,
    pub converted: u32,
    pub skipped_loading: u32,
    pub refused: u32,
    pub external: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowOutcome {
    Confirmed,
    AdmissionRefused,
    MissingSample,
    VoiceRefused,
    Superseded,
    Cancelled,
    DuplicateOrdinal,
    InvalidOffer,
    CopyMissed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowTerminal {
    pub key: ConfirmationKey,
    pub generation: u64,
    pub takeover_frame: u64,
    pub outcome: WindowOutcome,
    /// The host-copy interval that completed or rejected this window.
    pub copied_start_frame: u64,
    pub copied_end_frame: u64,
}

/// A busy producer role is not proof that the terminal queue is empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalPoll {
    Terminal(WindowTerminal),
    Empty,
    Busy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationOutcome {
    Activated,
    Muted,
    DryFallback,
    /// No installed body, or not the body selected by the producer.
    MissingSample,
    VoiceRefused,
}

#[cfg(any(feature = "device-audio", test))]
pub use realtime::ConfirmationChannel;
#[cfg(feature = "device-audio")]
pub(crate) use realtime::{ConfirmationConsumer, EpochAdvanceError};

#[cfg(any(feature = "device-audio", test))]
mod realtime {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

    use super::*;
    use crate::assets::AssetRing;

    const FREE: u8 = 0;
    const OFFERED: u8 = 1;
    const TERMINAL: u8 = 2;

    struct Credit {
        state: AtomicU8,
        epoch: AtomicU64,
        token: AtomicU64,
    }

    impl Credit {
        fn new() -> Self {
            Self {
                state: AtomicU8::new(FREE),
                epoch: AtomicU64::new(0),
                token: AtomicU64::new(0),
            }
        }

        fn matches(&self, key: ConfirmationKey, state: u8) -> bool {
            self.state.load(Ordering::Acquire) == state
                && self.epoch.load(Ordering::Relaxed) == key.epoch
                && self.token.load(Ordering::Relaxed) == key.token
        }
    }

    #[derive(Clone, Copy)]
    struct OfferedWindow {
        slot: usize,
        offer: WindowOffer,
    }

    #[derive(Clone, Copy)]
    struct EmittedTerminal {
        slot: usize,
        terminal: WindowTerminal,
    }

    struct Shared {
        epoch: AtomicU64,
        last_token: AtomicU64,
        /// Public producer handles may be cloned. Refuse overlapping calls
        /// rather than allowing two owners into an SPSC role.
        producer_busy: AtomicBool,
        credits: [Credit; MAX_CONFIRMATION_WINDOWS],
        offers: AssetRing<OfferedWindow>,
        terminals: AssetRing<EmittedTerminal>,
    }

    struct ProducerCall<'a>(&'a AtomicBool);

    impl Drop for ProducerCall<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }

    impl Shared {
        fn producer_call(&self) -> Option<ProducerCall<'_>> {
            self.producer_busy
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .ok()
                .map(|_| ProducerCall(&self.producer_busy))
        }
    }

    /// Producer endpoint for a fixed number of callback-confirmed windows.
    /// A credit remains occupied until its terminal is collected.
    #[derive(Clone)]
    pub struct ConfirmationChannel {
        shared: Arc<Shared>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum EpochAdvanceError {
        Busy,
        Exhausted,
        Closed,
    }

    impl ConfirmationChannel {
        pub(crate) fn new() -> Self {
            Self {
                shared: Arc::new(Shared {
                    epoch: AtomicU64::new(1),
                    last_token: AtomicU64::new(0),
                    producer_busy: AtomicBool::new(false),
                    credits: std::array::from_fn(|_| Credit::new()),
                    offers: AssetRing::new(),
                    terminals: AssetRing::new(),
                }),
            }
        }

        /// Zero means the device's final output owner has joined and this
        /// channel is permanently closed. Live epochs start at one.
        pub fn epoch(&self) -> u64 {
            self.shared.epoch.load(Ordering::Acquire)
        }

        pub fn same_channel(&self, other: &Self) -> bool {
            Arc::ptr_eq(&self.shared, &other.shared)
        }

        /// Offer metadata before publishing its generation or tagged onsets.
        /// Refusal does not consume the token or evict any previous window.
        pub fn publish(&self, offer: WindowOffer) -> bool {
            let Some(_call) = self.shared.producer_call() else {
                return false;
            };
            let count = offer
                .converted
                .checked_add(offer.skipped_loading)
                .and_then(|sum| sum.checked_add(offer.refused))
                .and_then(|sum| sum.checked_add(offer.external));
            if offer.key.epoch == 0
                || offer.key.epoch != self.epoch()
                || offer.key.token <= self.shared.last_token.load(Ordering::Relaxed)
                || offer.converted as usize > MAX_CONFIRMATION_ORDINALS
                || count != Some(offer.intended)
                || offer.start_frame >= offer.end_frame
                || offer.takeover_frame > offer.start_frame
                || (offer.refused != 0 && offer.refused == offer.intended)
            {
                return false;
            }
            let Some(slot) = self
                .shared
                .credits
                .iter()
                .position(|credit| credit.state.load(Ordering::Acquire) == FREE)
            else {
                return false;
            };
            let credit = &self.shared.credits[slot];
            credit.epoch.store(offer.key.epoch, Ordering::Relaxed);
            credit.token.store(offer.key.token, Ordering::Relaxed);
            credit.state.store(OFFERED, Ordering::Release);
            if self
                .shared
                .offers
                .push(OfferedWindow { slot, offer })
                .is_err()
            {
                credit.state.store(FREE, Ordering::Release);
                return false;
            }
            self.shared
                .last_token
                .store(offer.key.token, Ordering::Relaxed);
            true
        }

        /// Consumers retiring stale source snapshots must distinguish Busy
        /// from Empty and observe a stable epoch across their complete drain.
        pub fn try_pop_terminal(&self) -> TerminalPoll {
            let Some(_call) = self.shared.producer_call() else {
                return TerminalPoll::Busy;
            };
            let Some(emitted) = self.shared.terminals.pop() else {
                return TerminalPoll::Empty;
            };
            let credit = &self.shared.credits[emitted.slot];
            if credit.matches(emitted.terminal.key, TERMINAL) {
                credit.state.store(FREE, Ordering::Release);
            }
            TerminalPoll::Terminal(emitted.terminal)
        }

        /// Convenience for callers that do not infer retirement from None.
        pub fn pop_terminal(&self) -> Option<WindowTerminal> {
            match self.try_pop_terminal() {
                TerminalPoll::Terminal(terminal) => Some(terminal),
                TerminalPoll::Empty | TerminalPoll::Busy => None,
            }
        }

        /// Final device teardown only: no replacement consumer can ever use
        /// this channel. Closure cannot be lost to a paused producer, and
        /// emitted terminals remain readable. No SPSC queue is touched here.
        #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
        pub(crate) fn retire_after_join(&self) {
            self.shared.epoch.store(0, Ordering::Release);
        }

        /// Recycle only, after the previous callback owner has joined.
        /// Already emitted terminals keep their credits and remain readable;
        /// unfinished old windows are retired by the producer on epoch change.
        pub(crate) fn advance_epoch_after_join(&self) -> Result<u64, EpochAdvanceError> {
            let _call = self.shared.producer_call().ok_or(EpochAdvanceError::Busy)?;
            let epoch = self.epoch();
            if epoch == 0 {
                return Err(EpochAdvanceError::Closed);
            }
            let next = epoch.checked_add(1).ok_or(EpochAdvanceError::Exhausted)?;
            for _ in 0..MAX_CONFIRMATION_WINDOWS {
                if self.shared.offers.pop().is_none() {
                    break;
                }
            }
            for credit in &self.shared.credits {
                if credit.state.load(Ordering::Acquire) == OFFERED {
                    credit.state.store(FREE, Ordering::Release);
                }
            }
            self.shared.epoch.store(next, Ordering::Release);
            Ok(next)
        }
    }

    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    enum OrdinalState {
        #[default]
        Unseen,
        Admitted,
        Activated,
        Copied,
        Failed,
    }

    #[derive(Clone, Copy, Default)]
    struct Ordinal {
        state: OrdinalState,
        target_frame: u64,
    }

    const ACTIVATED_WORD_BITS: usize = u64::BITS as usize;
    const ACTIVATED_WORDS: usize = MAX_CONFIRMATION_ORDINALS.div_ceil(ACTIVATED_WORD_BITS);

    /// Ascending offsets of set bits, without visiting inactive ordinals.
    fn activated_offsets(mut bits: u64) -> impl Iterator<Item = usize> {
        std::iter::from_fn(move || {
            if bits == 0 {
                return None;
            }
            let offset = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            Some(offset)
        })
    }

    struct Window {
        offer: Option<WindowOffer>,
        failure: Option<WindowOutcome>,
        terminal: Option<WindowTerminal>,
        selected_this_block: bool,
        ordinals: Box<[Ordinal]>,
        /// Exactly the ordinals in Activated state, including future starts.
        activated: [u64; ACTIVATED_WORDS],
        copied: u32,
    }

    impl Window {
        fn new() -> Self {
            Self {
                offer: None,
                failure: None,
                terminal: None,
                selected_this_block: false,
                ordinals: vec![Ordinal::default(); MAX_CONFIRMATION_ORDINALS].into_boxed_slice(),
                activated: [0; ACTIVATED_WORDS],
                copied: 0,
            }
        }

        fn install(&mut self, offer: WindowOffer) {
            self.ordinals[..offer.converted as usize].fill(Ordinal::default());
            self.activated.fill(0);
            self.offer = Some(offer);
            self.failure = None;
            self.terminal = None;
            self.selected_this_block = false;
            self.copied = 0;
        }

        fn fail(&mut self, outcome: WindowOutcome) {
            self.failure.get_or_insert(outcome);
        }

        fn ordinal(&mut self, ordinal: u32) -> Option<&mut Ordinal> {
            if ordinal >= self.offer?.converted {
                self.fail(WindowOutcome::InvalidOffer);
                return None;
            }
            Some(&mut self.ordinals[ordinal as usize])
        }
    }

    /// The callback's finite ledger. Construct and drop it off the callback.
    /// The device owns its sole consumer and joins it before changing epochs.
    pub(crate) struct ConfirmationConsumer {
        channel: ConfirmationChannel,
        epoch: u64,
        windows: [Window; MAX_CONFIRMATION_WINDOWS],
        block: Option<(u64, u64)>,
    }

    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    impl ConfirmationConsumer {
        pub(crate) fn new(channel: ConfirmationChannel) -> Self {
            Self {
                epoch: channel.epoch(),
                channel,
                windows: std::array::from_fn(|_| Window::new()),
                block: None,
            }
        }

        fn drain_offers(&mut self) {
            if self.channel.epoch() != self.epoch {
                return;
            }
            for _ in 0..MAX_CONFIRMATION_WINDOWS {
                let Some(offered) = self.channel.shared.offers.pop() else {
                    break;
                };
                if offered.offer.key.epoch != self.epoch
                    || !self.channel.shared.credits[offered.slot]
                        .matches(offered.offer.key, OFFERED)
                {
                    continue;
                }
                self.windows[offered.slot].install(offered.offer);
            }
        }

        fn window(&mut self, key: ConfirmationKey) -> Option<&mut Window> {
            self.drain_offers();
            if key.epoch != self.epoch || self.channel.epoch() != self.epoch {
                return None;
            }
            self.windows.iter_mut().find(|window| {
                window.terminal.is_none() && window.offer.is_some_and(|offer| offer.key == key)
            })
        }

        pub(crate) fn begin_block(&mut self, start: u64, end: u64) {
            for window in &mut self.windows {
                window.selected_this_block = false;
            }
            if let Some((_, previous_end)) = self.block {
                for window in &mut self.windows {
                    if window.terminal.is_some() {
                        continue;
                    }
                    let Some(offer) = window.offer else {
                        continue;
                    };
                    let words = (offer.converted as usize).div_ceil(ACTIVATED_WORD_BITS);
                    if window.activated[..words]
                        .iter()
                        .enumerate()
                        .any(|(word, &bits)| {
                            activated_offsets(bits).any(|offset| {
                                window.ordinals[word * ACTIVATED_WORD_BITS + offset].target_frame
                                    < previous_end
                            })
                        })
                    {
                        window.fail(WindowOutcome::CopyMissed);
                    }
                }
            }
            self.block = Some((start, end));
            self.drain_offers();
        }

        pub(crate) fn admission(
            &mut self,
            tag: ConfirmationOnset,
            target_frame: u64,
            generation: u64,
            accepted: bool,
        ) {
            let Some(window) = self.window(tag.key) else {
                return;
            };
            let offer = window.offer.expect("matched window");
            if generation != offer.generation {
                window.fail(WindowOutcome::InvalidOffer);
                return;
            }
            let Some(ordinal) = window.ordinal(tag.ordinal) else {
                return;
            };
            if ordinal.state != OrdinalState::Unseen {
                window.fail(WindowOutcome::DuplicateOrdinal);
                return;
            }
            ordinal.target_frame = target_frame;
            ordinal.state = if accepted {
                OrdinalState::Admitted
            } else {
                OrdinalState::Failed
            };
            if target_frame >= offer.end_frame {
                window.fail(WindowOutcome::InvalidOffer);
            } else if !accepted {
                window.fail(WindowOutcome::AdmissionRefused);
            }
        }

        pub(crate) fn superseded(&mut self, tag: ConfirmationOnset) {
            let Some(window) = self.window(tag.key) else {
                return;
            };
            let Some(ordinal) = window.ordinal(tag.ordinal) else {
                return;
            };
            if ordinal.state != OrdinalState::Copied {
                ordinal.state = OrdinalState::Failed;
                window.activated[tag.ordinal as usize / ACTIVATED_WORD_BITS] &=
                    !(1u64 << (tag.ordinal as usize % ACTIVATED_WORD_BITS));
                window.fail(WindowOutcome::Superseded);
            }
        }

        pub(crate) fn activation(&mut self, tag: ConfirmationOnset, outcome: ActivationOutcome) {
            let Some(window) = self.window(tag.key) else {
                return;
            };
            let Some(ordinal) = window.ordinal(tag.ordinal) else {
                return;
            };
            if ordinal.state != OrdinalState::Admitted {
                window.fail(WindowOutcome::DuplicateOrdinal);
                return;
            }
            match outcome {
                ActivationOutcome::Activated
                | ActivationOutcome::Muted
                | ActivationOutcome::DryFallback => {
                    ordinal.state = OrdinalState::Activated;
                    window.activated[tag.ordinal as usize / ACTIVATED_WORD_BITS] |=
                        1u64 << (tag.ordinal as usize % ACTIVATED_WORD_BITS);
                }
                ActivationOutcome::MissingSample => {
                    ordinal.state = OrdinalState::Failed;
                    window.fail(WindowOutcome::MissingSample);
                }
                ActivationOutcome::VoiceRefused => {
                    ordinal.state = OrdinalState::Failed;
                    window.fail(WindowOutcome::VoiceRefused);
                }
            }
        }

        pub(crate) fn selected(&mut self, generation: u64, takeover_frame: u64) {
            self.drain_offers();
            for window in &mut self.windows {
                if window.terminal.is_some() {
                    continue;
                }
                let Some(offer) = window.offer else {
                    continue;
                };
                window.selected_this_block = offer.generation == generation;
                if offer.generation >= generation {
                    continue;
                }
                let superseded = offer.converted == 0
                    || window.ordinals[..offer.converted as usize]
                        .iter()
                        .any(|ordinal| match ordinal.state {
                            OrdinalState::Unseen => offer.end_frame > takeover_frame,
                            OrdinalState::Admitted | OrdinalState::Activated => {
                                ordinal.target_frame >= takeover_frame
                            }
                            OrdinalState::Copied | OrdinalState::Failed => false,
                        });
                if superseded {
                    window.fail(WindowOutcome::Superseded);
                }
            }
        }

        /// The caller has copied this exact rendered interval to host memory.
        /// No earlier hook emits a terminal, including negative outcomes.
        pub(crate) fn copied(
            &mut self,
            start: u64,
            end: u64,
            selected_generation: u64,
            stopped: bool,
        ) {
            if self.channel.epoch() != self.epoch
                || !self.block.is_some_and(|(block_start, block_end)| {
                    block_start <= start && start < end && end <= block_end
                })
            {
                return;
            }
            self.drain_offers();
            for (slot, window) in self.windows.iter_mut().enumerate() {
                let Some(offer) = window.offer else {
                    continue;
                };
                if window.terminal.is_none() {
                    let intersects_window = start.max(offer.start_frame) < end.min(offer.end_frame);
                    if stopped {
                        window.failure = Some(WindowOutcome::Cancelled);
                    }
                    if window.failure.is_none() && intersects_window {
                        let words = (offer.converted as usize).div_ceil(ACTIVATED_WORD_BITS);
                        for (word, bits) in window.activated[..words].iter_mut().enumerate() {
                            for offset in activated_offsets(*bits) {
                                let ordinal =
                                    &mut window.ordinals[word * ACTIVATED_WORD_BITS + offset];
                                if ordinal.target_frame < end {
                                    ordinal.state = OrdinalState::Copied;
                                    *bits &= !(1u64 << offset);
                                    window.copied += 1;
                                }
                            }
                        }
                    }
                    let confirmed = if offer.converted != 0 {
                        intersects_window && window.copied == offer.converted
                    } else {
                        window.selected_this_block
                            && offer.generation == selected_generation
                            && intersects_window
                    };
                    let outcome = window.failure.or({
                        if confirmed {
                            Some(WindowOutcome::Confirmed)
                        } else if end >= offer.end_frame {
                            Some(WindowOutcome::CopyMissed)
                        } else {
                            None
                        }
                    });
                    window.terminal = outcome.map(|outcome| WindowTerminal {
                        key: offer.key,
                        generation: offer.generation,
                        takeover_frame: offer.takeover_frame,
                        outcome,
                        copied_start_frame: start,
                        copied_end_frame: end,
                    });
                }
                let Some(terminal) = window.terminal else {
                    continue;
                };
                let credit = &self.channel.shared.credits[slot];
                if !credit.matches(offer.key, OFFERED) {
                    continue;
                }
                credit.state.store(TERMINAL, Ordering::Release);
                if self
                    .channel
                    .shared
                    .terminals
                    .push(EmittedTerminal { slot, terminal })
                    .is_ok()
                {
                    window.offer = None;
                    window.terminal = None;
                } else {
                    // Four retained credits fit in the fixed return ring.
                    // Keep the outcome retryable if that invariant is broken.
                    credit.state.store(OFFERED, Ordering::Release);
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn offer(channel: &ConfirmationChannel, token: u64, converted: u32) -> WindowOffer {
            WindowOffer {
                key: ConfirmationKey {
                    epoch: channel.epoch(),
                    token,
                },
                generation: 7,
                takeover_frame: 0,
                start_frame: 0,
                end_frame: 256,
                intended: converted,
                converted,
                skipped_loading: 0,
                refused: 0,
                external: 0,
            }
        }

        fn tag(offer: WindowOffer, ordinal: u32) -> ConfirmationOnset {
            ConfirmationOnset {
                key: offer.key,
                ordinal,
            }
        }

        #[test]
        fn a_window_requires_activation_and_a_due_host_copy() {
            let channel = ConfirmationChannel::new();
            let offered = offer(&channel, 1, 1);
            assert!(channel.publish(offered));
            let mut consumer = ConfirmationConsumer::new(channel.clone());
            consumer.begin_block(0, 128);
            consumer.admission(tag(offered, 0), 128, 7, true);
            consumer.activation(tag(offered, 0), ActivationOutcome::Activated);
            consumer.selected(7, 0);
            consumer.copied(0, 128, 7, false);
            assert!(channel.pop_terminal().is_none());
            consumer.begin_block(128, 256);
            consumer.selected(7, 0);
            assert!(channel.pop_terminal().is_none());
            consumer.copied(128, 256, 7, false);
            let terminal = channel.pop_terminal().expect("copied onset");
            assert_eq!(terminal.outcome, WindowOutcome::Confirmed);
            assert_eq!(
                (terminal.copied_start_frame, terminal.copied_end_frame),
                (128, 256)
            );
        }

        #[test]
        fn sparse_future_activations_keep_exact_due_copy_and_missed_copy_state() {
            const SPARSE: [u32; 4] = [0, 63, 64, 4095];
            for miss_middle_copy in [false, true] {
                let channel = ConfirmationChannel::new();
                let mut offered = offer(&channel, 1, MAX_CONFIRMATION_ORDINALS as u32);
                offered.end_frame = 1024;
                assert!(channel.publish(offered));
                let mut consumer = ConfirmationConsumer::new(channel.clone());
                consumer.begin_block(0, 128);
                for (ordinal, target) in SPARSE.into_iter().zip([0, 128, 128, 256]) {
                    consumer.admission(tag(offered, ordinal), target, 7, true);
                    // SBD voices may activate before their scheduled start.
                    consumer.activation(tag(offered, ordinal), ActivationOutcome::Activated);
                }
                consumer.selected(7, 0);
                consumer.copied(0, 128, 7, false);
                assert!(channel.pop_terminal().is_none());
                let window = consumer.window(offered.key).unwrap();
                assert_eq!(window.copied, 1);
                assert_eq!(window.activated[0], 1 << 63);
                assert_eq!(window.activated[1], 1);
                assert_eq!(window.activated[ACTIVATED_WORDS - 1], 1 << 63);

                consumer.begin_block(128, 256);
                assert!(consumer.window(offered.key).unwrap().failure.is_none());
                consumer.selected(7, 0);
                if miss_middle_copy {
                    // An invalid host interval does not discharge activation.
                    consumer.copied(128, 128, 7, false);
                } else {
                    consumer.copied(128, 256, 7, false);
                    assert_eq!(consumer.window(offered.key).unwrap().copied, 3);
                }
                assert!(channel.pop_terminal().is_none());
                consumer.begin_block(256, 384);
                consumer.selected(7, 0);
                consumer.copied(256, 384, 7, false);
                if miss_middle_copy {
                    let terminal = channel.pop_terminal().expect("overwritten due activation");
                    assert_eq!(terminal.outcome, WindowOutcome::CopyMissed);
                    assert_eq!(terminal.copied_end_frame, 384);
                    assert!(terminal.copied_end_frame < offered.end_frame);

                    // Reuse the same ledger slot after a negative terminal;
                    // neither old due nor old future bits may survive install.
                    let mut replacement = offer(&channel, 2, 0);
                    replacement.start_frame = 384;
                    replacement.end_frame = 512;
                    assert!(channel.publish(replacement));
                    consumer.begin_block(384, 512);
                    assert!(
                        consumer
                            .window(replacement.key)
                            .unwrap()
                            .activated
                            .iter()
                            .all(|bits| *bits == 0)
                    );
                    consumer.selected(7, 0);
                    consumer.copied(384, 512, 7, false);
                } else {
                    assert!(channel.pop_terminal().is_none());
                    let window = consumer.window(offered.key).unwrap();
                    assert_eq!(window.copied, SPARSE.len() as u32);
                    assert!(window.activated.iter().all(|bits| *bits == 0));
                    consumer.begin_block(384, 512);
                    for ordinal in 0..offered.converted {
                        if !SPARSE.contains(&ordinal) {
                            consumer.admission(tag(offered, ordinal), 384, 7, true);
                            consumer
                                .activation(tag(offered, ordinal), ActivationOutcome::Activated);
                        }
                    }
                    consumer.selected(7, 0);
                    consumer.copied(384, 512, 7, false);
                }
                assert_eq!(
                    channel
                        .pop_terminal()
                        .expect("complete copied window")
                        .outcome,
                    WindowOutcome::Confirmed
                );
                assert!(channel.pop_terminal().is_none());
            }
        }

        #[test]
        fn admission_alone_and_overwritten_rendering_do_not_confirm() {
            for activate in [false, true] {
                let channel = ConfirmationChannel::new();
                let offered = offer(&channel, 1, 1);
                assert!(channel.publish(offered));
                let mut consumer = ConfirmationConsumer::new(channel.clone());
                consumer.begin_block(0, 128);
                consumer.admission(tag(offered, 0), 0, 7, true);
                if activate {
                    consumer.activation(tag(offered, 0), ActivationOutcome::Activated);
                }
                consumer.selected(7, 0);
                consumer.begin_block(128, 256);
                consumer.copied(128, 256, 7, false);
                assert_eq!(
                    channel.pop_terminal().unwrap().outcome,
                    WindowOutcome::CopyMissed
                );
            }
        }

        #[test]
        fn duplicate_ordinals_and_real_refusals_remain_terminal_failures() {
            for expected in [
                WindowOutcome::DuplicateOrdinal,
                WindowOutcome::AdmissionRefused,
                WindowOutcome::MissingSample,
                WindowOutcome::VoiceRefused,
            ] {
                let channel = ConfirmationChannel::new();
                let offered = offer(&channel, 1, 1);
                assert!(channel.publish(offered));
                let mut consumer = ConfirmationConsumer::new(channel.clone());
                consumer.begin_block(0, 128);
                consumer.admission(
                    tag(offered, 0),
                    0,
                    7,
                    expected != WindowOutcome::AdmissionRefused,
                );
                match expected {
                    WindowOutcome::DuplicateOrdinal => {
                        consumer.admission(tag(offered, 0), 0, 7, true)
                    }
                    WindowOutcome::MissingSample => {
                        consumer.activation(tag(offered, 0), ActivationOutcome::MissingSample);
                        consumer.activation(tag(offered, 0), ActivationOutcome::Activated);
                    }
                    WindowOutcome::VoiceRefused => {
                        consumer.activation(tag(offered, 0), ActivationOutcome::VoiceRefused);
                    }
                    _ => {}
                }
                assert!(channel.pop_terminal().is_none());
                consumer.copied(0, 128, 7, false);
                assert_eq!(channel.pop_terminal().unwrap().outcome, expected);
                consumer.copied(0, 128, 7, false);
                assert!(channel.pop_terminal().is_none());
            }
        }

        #[test]
        fn explicit_mutes_and_permitted_dry_fallbacks_can_confirm() {
            for outcome in [ActivationOutcome::Muted, ActivationOutcome::DryFallback] {
                let channel = ConfirmationChannel::new();
                let mut offered = offer(&channel, 1, 1);
                offered.intended = 4;
                offered.skipped_loading = 1;
                offered.refused = 1;
                offered.external = 1;
                let mut consumer = ConfirmationConsumer::new(channel.clone());
                consumer.begin_block(0, 128);
                assert!(channel.publish(offered));
                consumer.admission(tag(offered, 0), 0, 7, true);
                consumer.activation(tag(offered, 0), outcome);
                consumer.copied(0, 128, 7, false);
                assert_eq!(
                    channel.pop_terminal().unwrap().outcome,
                    WindowOutcome::Confirmed
                );
            }
        }

        #[test]
        fn converted_onsets_need_a_copy_inside_the_offered_window() {
            for copy_start in [0, 256] {
                let channel = ConfirmationChannel::new();
                let mut offered = offer(&channel, 1, 1);
                offered.start_frame = 128;
                let mut consumer = ConfirmationConsumer::new(channel.clone());
                assert!(channel.publish(offered));
                consumer.begin_block(copy_start, copy_start + 128);
                consumer.admission(tag(offered, 0), 0, 7, true);
                consumer.selected(7, 0);
                consumer.activation(tag(offered, 0), ActivationOutcome::Activated);
                consumer.copied(copy_start, copy_start + 128, 7, false);
                if copy_start == 0 {
                    assert!(
                        channel.pop_terminal().is_none(),
                        "an early copy cannot confirm a future window"
                    );
                } else {
                    assert_eq!(
                        channel.pop_terminal().expect("expired window").outcome,
                        WindowOutcome::CopyMissed
                    );
                }
            }
        }

        #[test]
        fn silence_needs_selected_generation_and_a_nonempty_window_copy() {
            let channel = ConfirmationChannel::new();
            let mut offered = offer(&channel, 1, 0);
            offered.start_frame = 128;
            let mut consumer = ConfirmationConsumer::new(channel.clone());
            consumer.begin_block(0, 128);
            assert!(channel.publish(offered));
            consumer.selected(7, 0);
            consumer.copied(0, 128, 7, false);
            assert!(channel.pop_terminal().is_none());
            consumer.begin_block(128, 256);
            consumer.selected(7, 0);
            consumer.copied(128, 128, 7, false);
            assert!(channel.pop_terminal().is_none());
            consumer.copied(128, 256, 7, false);
            assert_eq!(
                channel.pop_terminal().unwrap().outcome,
                WindowOutcome::Confirmed
            );
        }

        #[test]
        fn future_generation_silence_cannot_use_an_old_generations_copy() {
            let channel = ConfirmationChannel::new();
            let offered = offer(&channel, 1, 0);
            assert!(channel.publish(offered));
            let mut consumer = ConfirmationConsumer::new(channel.clone());
            consumer.begin_block(0, 128);
            consumer.selected(6, 0);
            consumer.copied(0, 128, 6, false);
            assert!(channel.pop_terminal().is_none());
            consumer.begin_block(128, 256);
            consumer.selected(7, 0);
            consumer.copied(128, 256, 7, false);
            assert_eq!(
                channel.pop_terminal().unwrap().outcome,
                WindowOutcome::Confirmed
            );
        }

        #[test]
        fn a_late_silence_offer_cannot_confirm_already_rendered_audio() {
            let channel = ConfirmationChannel::new();
            let offered = offer(&channel, 1, 0);
            let mut consumer = ConfirmationConsumer::new(channel.clone());
            consumer.begin_block(0, 128);
            consumer.selected(7, 0);
            assert!(channel.publish(offered));
            consumer.copied(0, 128, 7, false);
            assert!(channel.pop_terminal().is_none());
            consumer.begin_block(128, 256);
            consumer.selected(7, 0);
            consumer.copied(128, 256, 7, false);
            assert_eq!(
                channel.pop_terminal().unwrap().outcome,
                WindowOutcome::Confirmed
            );
        }

        #[test]
        fn expired_late_offers_report_a_missed_copy_instead_of_silence() {
            let channel = ConfirmationChannel::new();
            let mut offered = offer(&channel, 1, 0);
            offered.end_frame = 128;
            let mut consumer = ConfirmationConsumer::new(channel.clone());
            consumer.begin_block(0, 128);
            consumer.selected(7, 0);
            assert!(channel.publish(offered));
            consumer.copied(0, 128, 7, false);
            assert_eq!(
                channel.pop_terminal().unwrap().outcome,
                WindowOutcome::CopyMissed
            );
        }

        #[test]
        fn dispositions_without_ordinals_do_not_consume_the_ordinal_budget() {
            let channel = ConfirmationChannel::new();
            let mut offered = offer(&channel, 1, MAX_CONFIRMATION_ORDINALS as u32);
            offered.intended = u32::MAX;
            offered.refused = u32::MAX - offered.converted;
            assert!(channel.publish(offered));
        }

        #[test]
        fn stop_and_takeover_cannot_turn_unfinished_work_into_valid_silence() {
            for stopped in [false, true] {
                let channel = ConfirmationChannel::new();
                let offered = offer(&channel, 1, 1);
                assert!(channel.publish(offered));
                let mut consumer = ConfirmationConsumer::new(channel.clone());
                consumer.begin_block(0, 128);
                consumer.admission(tag(offered, 0), 128, 7, true);
                if !stopped {
                    consumer.selected(8, 128);
                }
                consumer.copied(0, 128, 8, stopped);
                assert_eq!(
                    channel.pop_terminal().unwrap().outcome,
                    if stopped {
                        WindowOutcome::Cancelled
                    } else {
                        WindowOutcome::Superseded
                    }
                );
            }
        }

        #[test]
        fn an_older_pre_takeover_onset_can_finish_its_real_copy() {
            let channel = ConfirmationChannel::new();
            let offered = offer(&channel, 1, 1);
            assert!(channel.publish(offered));
            let mut consumer = ConfirmationConsumer::new(channel.clone());
            consumer.begin_block(0, 128);
            consumer.admission(tag(offered, 0), 64, 7, true);
            consumer.activation(tag(offered, 0), ActivationOutcome::Activated);
            consumer.selected(8, 128);
            consumer.copied(0, 128, 8, false);
            assert_eq!(
                channel.pop_terminal().unwrap().outcome,
                WindowOutcome::Confirmed
            );
        }

        #[test]
        fn delayed_terminals_hold_all_four_credits_and_survive_new_generations() {
            let channel = ConfirmationChannel::new();
            for token in 1..=MAX_CONFIRMATION_WINDOWS as u64 {
                assert!(channel.publish(offer(&channel, token, 0)));
            }
            let mut consumer = ConfirmationConsumer::new(channel.clone());
            consumer.begin_block(0, 128);
            consumer.selected(7, 0);
            consumer.copied(0, 128, 7, false);
            assert!(!channel.publish(offer(&channel, 5, 0)));
            consumer.begin_block(128, 256);
            consumer.selected(8, 128);
            consumer.copied(128, 256, 8, false);
            let terminal = channel.pop_terminal().unwrap();
            assert_eq!(terminal.key.token, 1);
            assert_eq!(terminal.outcome, WindowOutcome::Confirmed);
            assert!(channel.publish(offer(&channel, 5, 0)));
            for token in 2..=4 {
                let terminal = channel.pop_terminal().unwrap();
                assert_eq!(terminal.key.token, token);
                assert_eq!(terminal.outcome, WindowOutcome::Confirmed);
            }
            assert!(channel.pop_terminal().is_none());
        }

        #[test]
        fn epoch_change_preserves_terminals_and_retires_only_unfinished_credits() {
            let channel = ConfirmationChannel::new();
            let first = offer(&channel, 1, 0);
            let unfinished = offer(&channel, 2, 1);
            assert!(channel.publish(first));
            assert!(channel.publish(unfinished));
            {
                let mut consumer = ConfirmationConsumer::new(channel.clone());
                consumer.begin_block(0, 128);
                consumer.selected(7, 0);
                consumer.copied(0, 128, 7, false);
            }
            assert_eq!(channel.advance_epoch_after_join(), Ok(2));
            assert!(!channel.publish(unfinished));
            for token in 3..=5 {
                assert!(channel.publish(offer(&channel, token, 0)));
            }
            assert!(!channel.publish(offer(&channel, 6, 0)));
            let terminal = channel.pop_terminal().unwrap();
            assert_eq!(terminal.key, first.key);
            assert_eq!(terminal.outcome, WindowOutcome::Confirmed);
            assert!(channel.publish(offer(&channel, 6, 0)));
            assert!(channel.pop_terminal().is_none());
            let mut consumer = ConfirmationConsumer::new(channel.clone());
            consumer.begin_block(0, 128);
            consumer.selected(7, 0);
            consumer.copied(0, 128, 7, false);
            // Reusing the old terminal's slot can put a newer offer before
            // earlier ones in the callback ledger. Identity, not arrival
            // order, governs the producer's retained payload selection.
            let mut tokens = Vec::new();
            for _ in 0..MAX_CONFIRMATION_WINDOWS {
                let terminal = channel.pop_terminal().unwrap();
                assert_eq!(terminal.key.epoch, 2);
                assert_eq!(terminal.outcome, WindowOutcome::Confirmed);
                tokens.push(terminal.key.token);
            }
            tokens.sort_unstable();
            assert_eq!(tokens, [3, 4, 5, 6]);
            assert!(channel.pop_terminal().is_none());
        }

        #[test]
        fn malformed_offers_duplicate_tokens_and_counter_exhaustion_are_refused() {
            let channel = ConfirmationChannel::new();
            let valid = offer(&channel, 1, 1);
            for change in 0..5 {
                let mut invalid = valid;
                match change {
                    0 => {
                        invalid.intended = MAX_CONFIRMATION_ORDINALS as u32 + 1;
                        invalid.converted = invalid.intended;
                    }
                    1 => invalid.refused = u32::MAX,
                    2 => invalid.end_frame = invalid.start_frame,
                    3 => invalid.takeover_frame = invalid.start_frame + 1,
                    _ => invalid.key.token = 0,
                }
                assert!(!channel.publish(invalid));
            }
            assert!(channel.publish(valid));
            assert!(!channel.publish(valid));
            assert!(channel.same_channel(&channel.clone()));
            assert!(!channel.same_channel(&ConfirmationChannel::new()));
            channel.shared.last_token.store(u64::MAX, Ordering::Relaxed);
            assert!(!channel.publish(offer(&channel, u64::MAX, 0)));
            channel.shared.epoch.store(u64::MAX, Ordering::Release);
            assert_eq!(
                channel.advance_epoch_after_join(),
                Err(EpochAdvanceError::Exhausted)
            );
            assert_eq!(channel.epoch(), u64::MAX);
            channel.retire_after_join();
            assert_eq!(channel.epoch(), 0);
            assert!(!channel.publish(offer(&channel, 1, 0)));
            assert_eq!(
                channel.advance_epoch_after_join(),
                Err(EpochAdvanceError::Closed)
            );
        }

        #[test]
        fn cloned_producer_handles_refuse_overlapping_ring_roles() {
            let channel = ConfirmationChannel::new();
            let other = channel.clone();
            let offered = offer(&channel, 1, 0);
            let call = channel.shared.producer_call().unwrap();
            assert!(!other.publish(offered));
            assert!(other.pop_terminal().is_none());
            assert_eq!(other.try_pop_terminal(), TerminalPoll::Busy);
            assert_eq!(
                other.advance_epoch_after_join(),
                Err(EpochAdvanceError::Busy)
            );
            assert_eq!(channel.epoch(), 1);
            drop(call);
            assert_eq!(other.try_pop_terminal(), TerminalPoll::Empty);
            assert!(other.publish(offered));
            assert_eq!(other.advance_epoch_after_join(), Ok(2));
        }

        #[test]
        fn final_retirement_survives_a_paused_producer_and_preserves_emitted_terminals() {
            let channel = ConfirmationChannel::new();
            let completed = offer(&channel, 1, 0);
            let unfinished = offer(&channel, 2, 1);
            assert!(channel.publish(completed));
            assert!(channel.publish(unfinished));
            {
                let mut consumer = ConfirmationConsumer::new(channel.clone());
                consumer.begin_block(0, 128);
                consumer.selected(7, 0);
                consumer.copied(0, 128, 7, false);
            }
            // The old callback owner has ended. A public producer clone is
            // paused inside its SPSC role while the device finally closes.
            std::thread::scope(|scope| {
                let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
                let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(0);
                let other = channel.clone();
                let producer = scope.spawn(move || {
                    let _call = other.shared.producer_call().unwrap();
                    entered_tx.send(()).unwrap();
                    let _ = resume_rx.recv();
                });
                entered_rx.recv().unwrap();
                assert_eq!(channel.try_pop_terminal(), TerminalPoll::Busy);
                assert_eq!(
                    channel.advance_epoch_after_join(),
                    Err(EpochAdvanceError::Busy)
                );
                assert_eq!(channel.epoch(), 1);
                channel.retire_after_join();
                assert_eq!(channel.epoch(), 0);
                assert_eq!(channel.try_pop_terminal(), TerminalPoll::Busy);
                resume_tx.send(()).unwrap();
                producer.join().unwrap();
            });
            // Epoch retirement must follow a real completed drain, never the
            // Busy observation above: the old valid receipt still comes first.
            let TerminalPoll::Terminal(terminal) = channel.try_pop_terminal() else {
                panic!("completed old receipt must survive final closure");
            };
            assert_eq!(terminal.key, completed.key);
            assert_eq!(terminal.outcome, WindowOutcome::Confirmed);
            assert_eq!(channel.try_pop_terminal(), TerminalPoll::Empty);
            assert!(!channel.publish(unfinished));
            assert!(!channel.publish(offer(&channel, 3, 0)));
            assert_eq!(
                channel.advance_epoch_after_join(),
                Err(EpochAdvanceError::Closed)
            );
            assert_eq!(channel.epoch(), 0);
        }

        #[test]
        fn a_tag_cannot_certify_an_onset_from_another_generation() {
            let channel = ConfirmationChannel::new();
            let offered = offer(&channel, 1, 1);
            assert!(channel.publish(offered));
            let mut consumer = ConfirmationConsumer::new(channel.clone());
            consumer.begin_block(0, 128);
            consumer.admission(tag(offered, 0), 0, 6, true);
            consumer.activation(tag(offered, 0), ActivationOutcome::Activated);
            consumer.selected(7, 0);
            consumer.copied(0, 128, 7, false);
            assert_eq!(
                channel.pop_terminal().unwrap().outcome,
                WindowOutcome::InvalidOffer
            );
        }

        #[test]
        fn an_unexpected_full_return_ring_cannot_rewrite_a_completed_terminal() {
            let channel = ConfirmationChannel::new();
            let offered = offer(&channel, 1, 0);
            let filler = EmittedTerminal {
                slot: 0,
                terminal: WindowTerminal {
                    key: ConfirmationKey { epoch: 0, token: 0 },
                    generation: 0,
                    takeover_frame: 0,
                    outcome: WindowOutcome::Cancelled,
                    copied_start_frame: 0,
                    copied_end_frame: 1,
                },
            };
            // Normal publication cannot fill this ring: there are only four
            // credits. Exercise the defensive retry without changing that cap.
            for _ in 0..channel.shared.terminals.capacity() {
                assert!(channel.shared.terminals.push(filler).is_ok());
            }
            assert!(channel.publish(offered));
            let mut consumer = ConfirmationConsumer::new(channel.clone());
            consumer.begin_block(0, 128);
            consumer.selected(7, 0);
            consumer.copied(0, 128, 7, false);
            for _ in 0..channel.shared.terminals.capacity() {
                assert_eq!(channel.pop_terminal(), Some(filler.terminal));
            }
            consumer.begin_block(128, 256);
            consumer.selected(8, 128);
            consumer.copied(128, 256, 8, true);
            let terminal = channel.pop_terminal().unwrap();
            assert_eq!(terminal.key, offered.key);
            assert_eq!(terminal.outcome, WindowOutcome::Confirmed);
            assert_eq!(
                (terminal.copied_start_frame, terminal.copied_end_frame),
                (0, 128)
            );
            assert!(channel.pop_terminal().is_none());
        }
    }
}
