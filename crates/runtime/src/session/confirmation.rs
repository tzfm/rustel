//! Producer-owned replay payloads for exact finite audio-window receipts.
//!
//! Only copyable identities cross the audio boundary. A callback never borrows
//! source text or runtime settings, and a delayed completion never snapshots
//! whichever score happens to be installed when the producer next wakes.
//!
//! The byte limits cover source text, immutable snapshot storage, and fixed
//! shared cells, including the legacy voicing's optional top-note MIDI value.
//! Four outstanding payloads plus one confirmed target pin at most five
//! host-dictionary views through this ledger. Their mutable contents and opaque
//! host heaps remain outside these byte limits even when an old snapshot becomes
//! their sole owner; existing parser/query/host limits are not an RSS guarantee.

use rustel_audio::confirmation::{
    ConfirmationChannel, ConfirmationKey, MAX_CONFIRMATION_WINDOWS, TerminalPoll, WindowOffer,
    WindowOutcome,
};

use super::{AudibleSource, RuntimeError, Session};

const MAX_IN_FLIGHT: usize = MAX_CONFIRMATION_WINDOWS;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const MAX_RETAINED_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowDispositions {
    pub intended: u32,
    pub converted: u32,
    pub skipped_loading: u32,
    pub refused: u32,
    pub external: u32,
}

/// No endpoint means a headless/injected producer, never synthetic playback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowReservation {
    Untracked,
    Deferred,
    Ready(ConfirmationKey),
}

struct RetainedWindow {
    key: ConfirmationKey,
    generation: u64,
    source: Option<AudibleSource>,
    offer: Option<WindowOffer>,
    replay_invalidated: bool,
}

pub(super) struct RollbackConfirmations {
    channel: ConfirmationChannel,
    windows: [Option<RetainedWindow>; MAX_IN_FLIGHT],
    next_token: u64,
    confirmed_token: u64,
    confirmed_generation: Option<(ConfirmationKey, u64)>,
}

impl RollbackConfirmations {
    pub(super) fn discard_replay_sources(&mut self) {
        for slot in &mut self.windows {
            if let Some(window) = slot {
                if window.offer.is_none() {
                    *slot = None;
                } else {
                    window.source = None;
                    window.replay_invalidated = true;
                }
            }
        }
    }

    fn new(channel: ConfirmationChannel) -> Self {
        Self {
            channel,
            windows: std::array::from_fn(|_| None),
            next_token: 1,
            confirmed_token: 0,
            confirmed_generation: None,
        }
    }
}

fn retained_bytes(source: &Option<AudibleSource>, limit: usize) -> Option<usize> {
    let Some(source) = source else {
        return Some(0);
    };
    let text = source
        .source
        .len()
        .checked_add(2 * std::mem::size_of::<usize>())?
        .checked_add(std::mem::size_of::<AudibleSource>())?;
    let remaining = limit.checked_sub(text)?;
    source
        .settings
        .retained_snapshot_bytes(remaining)?
        .checked_add(text)
}

impl Session {
    /// Last replay-eligible window confirmed copied on the bound output epoch.
    /// A valid silent window counts. Publication, activation and retained
    /// rollback history alone do not; this does not prove speaker delivery.
    pub fn confirmed_audio_generation(&self) -> Option<u64> {
        let book = self.audio_confirmations.as_ref()?;
        let (key, generation) = book.confirmed_generation?;
        let epoch = book.channel.epoch();
        (epoch != 0 && key.epoch == epoch).then_some(generation)
    }

    /// Bind the actual output's receipt channel before live prefill. Rebinding
    /// the same device is idempotent. A different device must have joined its
    /// predecessor before this call; old completed receipts are drained first.
    #[cfg(feature = "device-audio")]
    pub fn bind_audio_confirmations(
        &mut self,
        channel: ConfirmationChannel,
    ) -> Result<(), RuntimeError> {
        if !self.consume_audio_confirmations() {
            return Err(RuntimeError::Message(
                "audio confirmation drain is pending; retry output binding".into(),
            ));
        }
        if channel.epoch() == 0 {
            return Err(RuntimeError::Message(
                "cannot bind confirmations from a closed output".into(),
            ));
        }
        if self
            .audio_confirmations
            .as_ref()
            .is_some_and(|book| book.channel.same_channel(&channel))
        {
            return Ok(());
        }
        self.audio_confirmations = Some(RollbackConfirmations::new(channel));
        Ok(())
    }

    /// Consume exact copied-window outcomes on the producer thread. This does
    /// not evaluate a score, change the current generation, or claim physical
    /// device delivery. Immutable snapshot storage is revalidated before
    /// retaining a target; mutable dictionary contents keep their existing
    /// semantics and are outside this ledger's byte budget. `false` means another
    /// producer call or a joined epoch transition requires a later drain before
    /// selecting a rollback target or rebinding its output.
    pub fn consume_audio_confirmations(&mut self) -> bool {
        let Some(mut book) = self.audio_confirmations.take() else {
            return true;
        };
        let epoch = book.channel.epoch();
        let mut drained = false;
        for _ in 0..=MAX_IN_FLIGHT {
            let terminal = match book.channel.try_pop_terminal() {
                TerminalPoll::Terminal(terminal) => terminal,
                TerminalPoll::Empty => {
                    drained = true;
                    break;
                }
                TerminalPoll::Busy => {
                    self.audio_confirmations = Some(book);
                    return false;
                }
            };
            let Some(index) = book.windows.iter().position(|entry| {
                entry
                    .as_ref()
                    .is_some_and(|entry| entry.key == terminal.key)
            }) else {
                continue;
            };
            let entry = book.windows[index]
                .take()
                .expect("matched retained receipt");
            let exact = entry.offer.as_ref().is_some_and(|offer| {
                offer.generation == terminal.generation
                    && offer.takeover_frame == terminal.takeover_frame
            });
            if exact
                && terminal.outcome == WindowOutcome::Confirmed
                && entry.key.token > book.confirmed_token
                && retained_bytes(&entry.source, MAX_PAYLOAD_BYTES).is_some()
            {
                book.confirmed_token = entry.key.token;
                book.confirmed_generation = Some((terminal.key, terminal.generation));
                if !entry.replay_invalidated {
                    self.install_rollback_source(entry.source);
                }
            }
        }
        // Epoch changes are published only after joining the old callback.
        // Drain its completed terminals BEFORE retiring unfinished ownership;
        // a valid delayed B receipt must not be erased by a stream restart.
        // An empty read preceding the joined owner's final publication is not
        // an empty read of the retired epoch. Drain again on the next turn.
        if !drained || book.channel.epoch() != epoch {
            self.audio_confirmations = Some(book);
            return false;
        }
        for entry in &mut book.windows {
            if entry.as_ref().is_some_and(|entry| {
                entry.key.epoch != epoch
                    || (entry.offer.is_none() && entry.generation != self.generation())
            }) {
                *entry = None;
            }
        }
        self.audio_confirmations = Some(book);
        true
    }

    /// Reserve source bytes and an in-flight credit before Scheduler can drain
    /// the candidate window. A full ledger is ordinary backpressure, not an
    /// empty successful retry of an already consumed scheduling pass.
    pub(crate) fn reserve_audio_confirmation(&mut self) -> Result<WindowReservation, RuntimeError> {
        if !self.consume_audio_confirmations() {
            return Ok(WindowReservation::Deferred);
        }
        let Some(book) = self.audio_confirmations.as_ref() else {
            return Ok(WindowReservation::Untracked);
        };
        if book.channel.epoch() == 0 {
            return Err(RuntimeError::Message(
                "cannot prepare audio confirmation for a closed output".into(),
            ));
        }
        let generation = self.generation();
        let reused = book.windows.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|entry| entry.generation == generation && entry.offer.is_none())
        });
        let Some(slot) = reused.or_else(|| book.windows.iter().position(Option::is_none)) else {
            return Ok(WindowReservation::Deferred);
        };
        // A retry has not published a window, and same-generation setup can
        // replace immutable settings before its next query. Refresh that one
        // capture, budgeting against every OTHER retained payload. Published
        // captures remain tied to the query that produced their exact offer.
        let Some(outstanding) = retained_bytes(&self.audible_source, MAX_PAYLOAD_BYTES)
            .and_then(|previous| {
                book.windows
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != slot)
                    .try_fold(previous, |sum, (_, entry)| {
                        sum.checked_add(match entry {
                            Some(entry) => retained_bytes(&entry.source, MAX_PAYLOAD_BYTES)?,
                            None => 0,
                        })
                    })
            })
            .filter(|sum| *sum <= MAX_RETAINED_BYTES)
        else {
            return Ok(WindowReservation::Deferred);
        };
        // Select settings once; do not measure one publication then capture a
        // different one. Snapshotting only clones fixed-size ownership handles.
        let settings = self.js.snapshot_published_runtime_settings();
        let text_bytes = self.last_source.as_ref().map_or(0, |source| source.len());
        let bytes = if self.last_source.is_none() {
            Some(0)
        } else {
            text_bytes
                .checked_add(std::mem::size_of::<AudibleSource>())
                .and_then(|text| text.checked_add(2 * std::mem::size_of::<usize>()))
                .and_then(|text| {
                    MAX_PAYLOAD_BYTES
                        .checked_sub(text)
                        .and_then(|room| settings.retained_snapshot_bytes(room))
                        .and_then(|native| text.checked_add(native))
                })
        }
        .ok_or_else(|| {
            RuntimeError::ResourceLimit(
                "live rollback snapshot exceeds its retained-byte limit".into(),
            )
        })?;
        if outstanding
            .checked_add(bytes)
            .is_none_or(|sum| sum > MAX_RETAINED_BYTES)
        {
            return Ok(WindowReservation::Deferred);
        }
        let (key, next_token) = if let Some(index) = reused {
            (
                book.windows[index]
                    .as_ref()
                    .expect("reused reservation")
                    .key,
                None,
            )
        } else {
            let token = book.next_token;
            let next = token.checked_add(1).ok_or_else(|| {
                RuntimeError::ResourceLimit("live confirmation token counter exhausted".into())
            })?;
            (
                ConfirmationKey {
                    epoch: book.channel.epoch(),
                    token,
                },
                Some(next),
            )
        };
        if reused.is_some() {
            // Admission succeeded. Release the superseded text before cloning
            // its replacement, so refresh does not transiently retain both
            // payloads outside the aggregate budget. Keep the token/credit.
            self.audio_confirmations
                .as_mut()
                .expect("bound receipt channel")
                .windows[slot]
                .as_mut()
                .expect("reused reservation")
                .source = None;
        }
        // Only the admitted payload clones score-sized text. Immutable native
        // dictionaries and fixed history cells are charged even while shared;
        // mutable dictionary contents remain outside this budget under their
        // existing ownership contract.
        let captured = self.last_source.as_ref().map(|source| AudibleSource {
            generation,
            source: source.clone(),
            mini: self.last_path == super::EvaluateSource::MiniRust,
            settings,
            cps: self.scheduler.cps(),
            cycle_zero_time: self
                .scheduler
                .time_at_cycle(rustel_fraction::Fraction::ZERO),
            #[cfg(feature = "vst")]
            insert_orbits: self.insert_orbits,
        });
        let book = self
            .audio_confirmations
            .as_mut()
            .expect("bound receipt channel");
        if let Some(next_token) = next_token {
            book.next_token = next_token;
        }
        book.windows[slot] = Some(RetainedWindow {
            key,
            generation,
            source: captured,
            replay_invalidated: false,
            offer: None,
        });
        Ok(WindowReservation::Ready(key))
    }

    pub(crate) fn publish_audio_confirmation(
        &mut self,
        offer: WindowOffer,
    ) -> Result<(), RuntimeError> {
        let book = self.audio_confirmations.as_mut().ok_or_else(|| {
            RuntimeError::Message("audio confirmation published without its bound output".into())
        })?;
        let entry = book
            .windows
            .iter_mut()
            .flatten()
            .find(|entry| entry.key == offer.key)
            .ok_or_else(|| {
                RuntimeError::Message("audio confirmation lost its retained payload".into())
            })?;
        if entry.generation != offer.generation || entry.offer.is_some() {
            return Err(RuntimeError::Message(
                "audio confirmation publication identity mismatch".into(),
            ));
        }
        // At most four retained credits exist. A credit is returned only by
        // consuming a terminal (or after the old callback has joined), so its
        // four-entry offer queue necessarily has room before this publication.
        if !book.channel.publish(offer) {
            return Err(RuntimeError::Message(
                "audio confirmation credit did not reserve publication capacity".into(),
            ));
        }
        entry.offer = Some(offer);
        Ok(())
    }

    pub(crate) fn cancel_unpublished_audio_confirmation(&mut self, key: ConfirmationKey) {
        if let Some(book) = &mut self.audio_confirmations {
            for entry in &mut book.windows {
                if entry
                    .as_ref()
                    .is_some_and(|entry| entry.key == key && entry.offer.is_none())
                {
                    *entry = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use rustel_audio::device::ManualLiveOutput;
    use rustel_core::compose::Alignment;
    use rustel_core::rng::RngMode;
    use rustel_core::settings::RuntimeSettings;
    use rustel_fraction::Fraction;

    use super::super::SessionConfig;
    use super::*;
    use rustel_audio::TakeoverCut;

    fn silent_session() -> (Session, ManualLiveOutput) {
        let mut session = Session::with_config(SessionConfig {
            cps: 1.0,
            horizon: 0.5,
            sample_rate: 48_000,
            ..SessionConfig::default()
        })
        .expect("session");
        session.set_direct_diagnostic_logging(false);
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.0);
        session.evaluate_mini("~").expect("initial silence");
        let output = ManualLiveOutput::new(48_000, session.generation()).expect("output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind output confirmations");
        (session, output)
    }

    fn reserve(session: &mut Session) -> ConfirmationKey {
        match session.reserve_audio_confirmation().expect("reservation") {
            WindowReservation::Ready(key) => key,
            other => panic!("expected a free receipt credit, got {other:?}"),
        }
    }

    // These helpers construct finite windows explicitly to isolate ledger
    // policy. Receipt production still runs the ordinary gated callback and
    // host-copy body; no test inserts a terminal or advances a device clock.
    fn publish_silence(
        session: &mut Session,
        key: ConfirmationKey,
        start_frame: u64,
        end_frame: u64,
    ) {
        session
            .publish_audio_confirmation(WindowOffer {
                key,
                generation: session.generation(),
                takeover_frame: start_frame,
                start_frame,
                end_frame,
                intended: 0,
                converted: 0,
                skipped_loading: 0,
                refused: 0,
                external: 0,
            })
            .expect("publish silence window");
    }

    fn copy_silence(output: &mut ManualLiveOutput) {
        let before = output.device().report();
        let mut pcm = [f32::NAN; 128 * 2];
        output.render(&mut pcm);
        assert!(pcm.iter().all(|sample| *sample == 0.0));
        let after = output.device().report();
        assert_eq!(after.callbacks, before.callbacks + 1);
        assert_eq!(after.submitted_frames, before.submitted_frames + 128);
        assert_eq!(after.accepted_events, 0);
        assert_eq!(after.refused_voices, 0);
        assert_eq!(after.callback_errors, 0);
        assert_eq!(after.ring_role_conflicts, 0);
        assert_eq!(after.callback_scope_misses, 0);
    }

    fn retained_windows(session: &Session) -> usize {
        session
            .audio_confirmations
            .as_ref()
            .expect("bound channel")
            .windows
            .iter()
            .flatten()
            .count()
    }

    #[test]
    fn confirmed_generation_waits_for_a_due_host_copy_and_producer_drain() {
        let (mut session, mut output) = silent_session();
        let generation_a = session.generation();
        let key_a = reserve(&mut session);
        publish_silence(&mut session, key_a, 0, 128);
        assert_eq!(session.confirmed_audio_generation(), None);
        assert_eq!(output.device().generation(), generation_a);
        copy_silence(&mut output);
        assert_eq!(session.confirmed_audio_generation(), None);
        assert!(session.consume_audio_confirmations());
        assert_eq!(session.confirmed_audio_generation(), Some(generation_a));

        let generation_b = session.reload_at("~ ~", true, 0.0).unwrap();
        let key_b = reserve(&mut session);
        publish_silence(&mut session, key_b, 256, 384);
        output
            .device()
            .set_generation(generation_b, 256, TakeoverCut::None);
        assert_eq!(output.device().generation(), generation_b);
        assert!(session.consume_audio_confirmations());
        assert_eq!(session.confirmed_audio_generation(), Some(generation_a));
        // The first copy ends before B's window. A selected silent window
        // confirms on its first intersecting copy, not at its end boundary.
        copy_silence(&mut output);
        assert!(session.consume_audio_confirmations());
        assert_eq!(session.confirmed_audio_generation(), Some(generation_a));
        copy_silence(&mut output);
        assert_eq!(session.confirmed_audio_generation(), Some(generation_a));
        assert!(session.consume_audio_confirmations());
        assert_eq!(session.confirmed_audio_generation(), Some(generation_b));
    }

    #[test]
    fn confirmed_generation_is_not_inherited_by_a_new_output_channel() {
        let (mut session, mut output) = silent_session();
        let generation = session.generation();
        let key = reserve(&mut session);
        publish_silence(&mut session, key, 0, 128);
        copy_silence(&mut output);
        assert!(session.consume_audio_confirmations());
        assert_eq!(session.confirmed_audio_generation(), Some(generation));
        drop(output);
        assert_eq!(session.confirmed_audio_generation(), None);
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation
        );

        let replacement = ManualLiveOutput::new(48_000, generation).unwrap();
        session
            .bind_audio_confirmations(replacement.device().confirmations())
            .unwrap();
        assert_eq!(session.confirmed_audio_generation(), None);
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation
        );
    }

    fn settings_view(settings: &RuntimeSettings) -> (RngMode, Alignment, String) {
        settings.with(|| {
            (
                rustel_core::rng::rng_mode(),
                rustel_core::compose::default_alignment(),
                rustel_core::voicings::selected_default_voicings(),
            )
        })
    }

    #[test]
    fn four_retained_windows_backpressure_before_query_and_reuse_after_real_copy() {
        let (mut session, mut output) = silent_session();
        let audible_generation = session.generation();
        for token in 1..=MAX_IN_FLIGHT as u64 {
            let key = reserve(&mut session);
            assert_eq!(key.token, token);
            publish_silence(&mut session, key, 0, 128);
        }
        assert_eq!(retained_windows(&session), MAX_IN_FLIGHT);

        let queried = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&queried);
        session
            .set_pattern(rustel_core::state_signal(move |_| {
                observed.fetch_add(1, Ordering::Relaxed);
                rustel_core::Value::F64(1.0)
            }))
            .expect("native query counter");
        let before_queries = queried.load(Ordering::Relaxed);
        let before_cursor = session.scheduler.scheduled_to_cycle();
        let before_token = session.audio_confirmations.as_ref().unwrap().next_token;
        assert_eq!(
            session
                .reserve_audio_confirmation()
                .expect("ordinary backpressure"),
            WindowReservation::Deferred
        );
        let mut producer =
            crate::LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let device = output.device();
        let step = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || device.clock_seconds(),
                48_000,
                |generation, takeover, cut| device.set_generation(generation, takeover, cut),
                |event| device.push(event),
            )
            .expect("full receipts defer the producer");
        assert!(step.backpressured);
        assert_eq!((step.scheduled, step.pushed, step.pending), (0, 0, 0));
        assert_eq!(queried.load(Ordering::Relaxed), before_queries);
        assert_eq!(session.scheduler.scheduled_to_cycle(), before_cursor);
        assert_eq!(
            session.audio_confirmations.as_ref().unwrap().next_token,
            before_token
        );
        assert_eq!(output.device().generation(), audible_generation);
        assert_eq!(output.device().report().callbacks, 0);

        copy_silence(&mut output);
        assert_eq!(retained_windows(&session), MAX_IN_FLIGHT);
        session.consume_audio_confirmations();
        assert_eq!(retained_windows(&session), 0);
        assert_eq!(reserve(&mut session).token, before_token);
        // Positive control: the native graph really does observe a query.
        session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("native query");
        assert!(queried.load(Ordering::Relaxed) > before_queries);
    }

    #[test]
    fn exhausted_receipt_tokens_do_not_wrap_or_install_a_reservation() {
        let (mut session, output) = silent_session();
        session.audio_confirmations.as_mut().unwrap().next_token = u64::MAX;
        let before_cursor = session.scheduler.scheduled_to_cycle();
        let error = session
            .reserve_audio_confirmation()
            .expect_err("exhausted token");
        assert!(matches!(error, RuntimeError::ResourceLimit(message)
            if message.contains("token counter exhausted")));
        assert_eq!(
            session.audio_confirmations.as_ref().unwrap().next_token,
            u64::MAX
        );
        assert_eq!(retained_windows(&session), 0);
        assert_eq!(session.scheduler.scheduled_to_cycle(), before_cursor);
        assert_eq!(output.device().report().callbacks, 0);
    }

    #[test]
    fn a_delayed_copy_retains_its_exact_source_and_settings_after_session_advances() {
        let (mut session, mut output) = silent_session();
        assert!(session.audible_source.is_none());
        session.set_rng_mode(RngMode::Precise);
        session.set_default_join(Alignment::Out);
        session.set_default_voicings(Some("guidetones"));
        let generation_b = session.reload_at("~ ~", true, 0.0).expect("source B");
        let key_b = reserve(&mut session);
        publish_silence(&mut session, key_b, 0, 128);
        output
            .device()
            .set_generation(generation_b, 0, TakeoverCut::None);

        session.set_rng_mode(RngMode::Legacy);
        session.set_default_join(Alignment::Mix);
        session.set_default_voicings(Some("lefthand"));
        let generation_c = session.reload_at("~ ~ ~", true, 0.0).expect("source C");
        assert!(session.audible_source.is_none());
        copy_silence(&mut output);
        assert!(session.audible_source.is_none());
        session.consume_audio_confirmations();

        let retained = session.audible_source.as_ref().expect("confirmed B");
        assert_eq!(retained.generation, generation_b);
        assert_eq!(retained.source.as_ref(), "~ ~");
        assert!(retained.mini);
        assert_eq!(
            settings_view(&retained.settings),
            (RngMode::Precise, Alignment::Out, "guidetones".into())
        );
        assert_eq!(session.generation(), generation_c);
        assert_eq!(session.confirmed_audio_generation(), Some(generation_b));
        assert_eq!(session.last_source.as_deref(), Some("~ ~ ~"));
        assert_eq!(
            settings_view(&session.js.snapshot_published_runtime_settings()),
            (RngMode::Legacy, Alignment::Mix, "lefthand".into())
        );
        assert_eq!(retained_windows(&session), 0);
    }

    #[test]
    fn a_direct_pattern_clears_textual_rollback_only_after_its_real_confirmation() {
        let (mut session, mut output) = silent_session();
        let generation_a = session.generation();
        assert!(session.audible_source.is_none());
        let key_a = reserve(&mut session);
        publish_silence(&mut session, key_a, 0, 128);
        copy_silence(&mut output);
        session.consume_audio_confirmations();
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation_a
        );
        assert_eq!(
            session.audible_source.as_ref().unwrap().source.as_ref(),
            "~"
        );
        session
            .set_pattern(rustel_core::silence())
            .expect("native silence");
        let generation_b = session.generation();
        let key = reserve(&mut session);
        assert!(
            session
                .audio_confirmations
                .as_ref()
                .unwrap()
                .windows
                .iter()
                .flatten()
                .find(|entry| entry.key == key)
                .unwrap()
                .source
                .is_none()
        );
        publish_silence(&mut session, key, 128, 256);
        session.consume_audio_confirmations();
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation_a
        );
        output
            .device()
            .set_generation(generation_b, 128, TakeoverCut::None);
        copy_silence(&mut output);
        assert!(session.audible_source.is_some());
        session.consume_audio_confirmations();
        assert!(session.audible_source.is_none());
        assert!(session.last_source.is_none());
        assert_eq!(session.confirmed_audio_generation(), Some(generation_b));
        assert_eq!(session.generation(), generation_b);
        assert_eq!(retained_windows(&session), 0);
    }

    #[test]
    fn oversized_candidate_text_is_refused_before_retaining_another_copy() {
        let (mut session, output) = silent_session();
        // This is payload admission, not score parsing: existing owned text
        // must be measured before a receipt duplicates it.
        session.last_source = Some(" ".repeat(MAX_PAYLOAD_BYTES).into());
        let before_cursor = session.scheduler.scheduled_to_cycle();
        let before_token = session.audio_confirmations.as_ref().unwrap().next_token;
        let error = session
            .reserve_audio_confirmation()
            .expect_err("oversized source");
        assert!(matches!(error, RuntimeError::ResourceLimit(_)));
        assert_eq!(retained_windows(&session), 0);
        assert_eq!(
            session.audio_confirmations.as_ref().unwrap().next_token,
            before_token
        );
        assert_eq!(session.scheduler.scheduled_to_cycle(), before_cursor);
        assert_eq!(output.device().report().callbacks, 0);
        assert!(session.audible_source.is_none());
    }

    #[test]
    fn a_same_generation_retry_refreshes_settings_but_not_a_published_payload() {
        let (mut session, mut output) = silent_session();
        let generation = session.generation();
        session.set_rng_mode(RngMode::Legacy);
        session.set_default_join(Alignment::Mix);
        session.set_default_voicings(Some("lefthand"));
        // Model a prefill attempt that reserved ownership but had to retry
        // before publishing a window. Setup/native setters keep generation.
        let key = reserve(&mut session);
        let next_token = session.audio_confirmations.as_ref().unwrap().next_token;
        session.set_rng_mode(RngMode::Precise);
        session.set_default_join(Alignment::Out);
        session.set_default_voicings(Some("guidetones"));
        assert_eq!(session.generation(), generation);

        let mut producer =
            crate::LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let device = output.device();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || device.clock_seconds(),
                48_000,
                |generation, takeover, cut| device.set_generation(generation, takeover, cut),
                |event| device.push(event),
            )
            .expect("fresh scheduling uses the refreshed reservation");
        let book = session.audio_confirmations.as_ref().unwrap();
        assert_eq!(book.next_token, next_token);
        let entry = book
            .windows
            .iter()
            .flatten()
            .find(|entry| entry.key == key)
            .unwrap();
        assert!(entry.offer.is_some());
        assert_eq!(
            settings_view(&entry.source.as_ref().unwrap().settings),
            (RngMode::Precise, Alignment::Out, "guidetones".into()),
        );
        assert_eq!(retained_windows(&session), 1);
        assert!(session.audible_source.is_none());

        session.set_rng_mode(RngMode::Legacy);
        session.set_default_join(Alignment::Mix);
        session.set_default_voicings(Some("lefthand"));
        let newer = reserve(&mut session);
        assert_ne!(newer, key, "published payloads cannot be refreshed");
        assert_eq!(newer.token, next_token);
        assert_eq!(retained_windows(&session), 2);
        copy_silence(&mut output);
        session.consume_audio_confirmations();
        let confirmed = session.audible_source.as_ref().expect("real copied window");
        assert_eq!(confirmed.generation, generation);
        assert_eq!(confirmed.source.as_ref(), "~");
        assert_eq!(
            settings_view(&confirmed.settings),
            (RngMode::Precise, Alignment::Out, "guidetones".into()),
        );
        assert_eq!(
            settings_view(&session.js.snapshot_published_runtime_settings()),
            (RngMode::Legacy, Alignment::Mix, "lefthand".into()),
        );
        assert_eq!(retained_windows(&session), 1);
    }

    #[test]
    fn refreshed_and_published_snapshot_storage_are_admitted_before_capture() {
        let (mut session, output) = silent_session();
        let initial_settings = settings_view(&session.js.snapshot_published_runtime_settings());
        let key = reserve(&mut session);
        let before_token = session.audio_confirmations.as_ref().unwrap().next_token;
        let before_cursor = session.scheduler.scheduled_to_cycle();
        let oversized = "x".repeat(MAX_PAYLOAD_BYTES);
        session.set_default_voicings(Some(&oversized));
        let error = session
            .reserve_audio_confirmation()
            .expect_err("oversized refreshed snapshot");
        assert!(matches!(error, RuntimeError::ResourceLimit(_)));
        assert_eq!(
            session.audio_confirmations.as_ref().unwrap().next_token,
            before_token
        );
        assert_eq!(retained_windows(&session), 1);
        assert_eq!(session.scheduler.scheduled_to_cycle(), before_cursor);
        let entry = session
            .audio_confirmations
            .as_ref()
            .unwrap()
            .windows
            .iter()
            .flatten()
            .find(|entry| entry.key == key)
            .unwrap();
        assert!(entry.offer.is_none());
        assert_eq!(entry.source.as_ref().unwrap().source.as_ref(), "~");
        assert_eq!(
            settings_view(&entry.source.as_ref().unwrap().settings),
            initial_settings
        );

        session.set_default_voicings(Some("lefthand"));
        assert_eq!(reserve(&mut session), key);
        publish_silence(&mut session, key, 0, 128);
        let published_settings = session
            .audio_confirmations
            .as_ref()
            .unwrap()
            .windows
            .iter()
            .flatten()
            .find(|entry| entry.key == key)
            .unwrap()
            .source
            .as_ref()
            .unwrap()
            .settings
            .clone();
        // Exercise defensive revalidation without refreshing the published
        // payload from an unrelated current Session publication.
        published_settings.with(|| rustel_core::voicings::set_default_voicings(&oversized));
        session
            .reload_at("~ ~", true, 0.0)
            .expect("next silent source");
        assert_eq!(
            session
                .reserve_audio_confirmation()
                .expect("current retained budget"),
            WindowReservation::Deferred
        );
        assert_eq!(
            session.audio_confirmations.as_ref().unwrap().next_token,
            before_token
        );
        assert_eq!(retained_windows(&session), 1);

        published_settings.with(|| rustel_core::voicings::set_default_voicings("lefthand"));
        assert_eq!(reserve(&mut session).token, before_token);
        assert_eq!(retained_windows(&session), 2);
        assert_eq!(output.device().report().callbacks, 0);
        assert!(session.audible_source.is_none());
    }

    #[test]
    fn an_initial_all_refused_conversion_keeps_skip_policy_without_confirming_silence() {
        let mut session = Session::with_config(SessionConfig {
            cps: 1.0,
            horizon: 0.5,
            sample_rate: 48_000,
            ..SessionConfig::default()
        })
        .expect("session");
        session.set_direct_diagnostic_logging(false);
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.0);
        session
            .evaluate_mini("c4*64")
            .expect("initial bare mini haps");
        let mut output = ManualLiveOutput::new(48_000, session.generation()).expect("output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind output confirmations");
        let mut producer =
            crate::LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let device = output.device();
        let step = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || device.clock_seconds(),
                48_000,
                |generation, takeover, cut| device.set_generation(generation, takeover, cut),
                |event| device.push(event),
            )
            .expect("startup conversion refusals remain skipped");
        assert_eq!((step.scheduled, step.pushed, step.pending), (0, 0, 0));
        assert_eq!(retained_windows(&session), 0);
        assert!(session.audible_source.is_none());
        copy_silence(&mut output);
        session.consume_audio_confirmations();
        assert!(session.audible_source.is_none());
        assert_eq!(retained_windows(&session), 0);
    }

    #[test]
    fn a_caught_query_exception_does_not_confirm_accidental_silence() {
        let (mut session, mut output) = silent_session();
        let key_a = reserve(&mut session);
        publish_silence(&mut session, key_a, 0, 128);
        copy_silence(&mut output);
        session.consume_audio_confirmations();
        let generation_a = session.audible_source.as_ref().unwrap().generation;

        let queries = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&queries);
        session
            .set_pattern(rustel_core::state_signal(move |_| {
                observed.fetch_add(1, Ordering::Relaxed);
                rustel_core::signal_query_error(|| "native query failure".into());
                rustel_core::Value::F64(1.0)
            }))
            .expect("native throwing graph");
        output
            .device()
            .set_generation(session.generation(), 128, TakeoverCut::None);
        let mut producer =
            crate::LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let device = output.device();
        let step = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || device.clock_seconds(),
                48_000,
                |generation, takeover, cut| device.set_generation(generation, takeover, cut),
                |event| device.push(event),
            )
            .expect("caught exceptions keep existing silent-window policy");
        assert!(queries.load(Ordering::Relaxed) > 0);
        assert_eq!((step.scheduled, step.pushed, step.pending), (0, 0, 0));
        copy_silence(&mut output);
        session.consume_audio_confirmations();
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation_a
        );
        assert_eq!(retained_windows(&session), 0);
    }

    #[test]
    fn device_retirement_consumes_completed_old_epoch_before_dropping_unfinished_windows() {
        let (mut session, mut output) = silent_session();
        let generation_b = session.reload_at("~ ~", true, 0.0).expect("source B");
        let key_b = reserve(&mut session);
        publish_silence(&mut session, key_b, 0, 128);
        output
            .device()
            .set_generation(generation_b, 0, TakeoverCut::None);
        let generation_c = session.reload_at("~ ~ ~", true, 0.0).expect("source C");
        let key_c = reserve(&mut session);
        publish_silence(&mut session, key_c, 256, 384);
        assert_eq!(key_b.epoch, key_c.epoch);
        copy_silence(&mut output);
        assert_eq!(retained_windows(&session), 2);
        let channel = output.device().confirmations();
        drop(output);
        assert_eq!(
            channel.epoch(),
            0,
            "final Drop permanently closes the channel"
        );

        session.consume_audio_confirmations();
        let retained = session
            .audible_source
            .as_ref()
            .expect("completed B survives retirement");
        assert_eq!(session.confirmed_audio_generation(), None);
        assert_eq!(retained.generation, generation_b);
        assert_eq!(retained.source.as_ref(), "~ ~");
        assert_eq!(session.generation(), generation_c);
        assert_eq!(session.last_source.as_deref(), Some("~ ~ ~"));
        assert_eq!(retained_windows(&session), 0);
        assert!(channel.pop_terminal().is_none());
        assert!(session.reserve_audio_confirmation().is_err());
        assert!(session.bind_audio_confirmations(channel).is_err());
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation_b,
            "a closed output cannot erase its completed rollback target"
        );
    }
}
