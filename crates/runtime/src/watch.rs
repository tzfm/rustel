//! Reusable, polling file-watch state for the local live-coding loop.
//!
//! OS notification APIs can miss or coalesce changes: ordinary writes can
//! arrive in pieces and atomic saves often appear as remove/create/rename
//! bursts. This adapter samples bounded regular-file contents and requires one
//! observation to remain stable for a debounce window before it is evaluated.
//! The caller supplies monotonic time, which keeps continuity tests fully
//! deterministic and lets a CLI choose either polling or an OS wake source.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Session;

/// A watched score is source code, not an unbounded allocation input.
pub const MAX_WATCH_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

/// Capacity refusals a horizon-budgeted watch retries before its identity is
/// final.
///
/// A resource limit hit inside the slice the horizon granted says more about
/// the turn than about the file, so the same bytes get another attempt under
/// the next turn's budget; a refusal past this count is final like any other.
const LIVE_CAPACITY_REFUSAL_RETRIES: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchLanguage {
    JavaScript,
    Mini,
}

/// What a stable file identity is allowed to replace.
///
/// Language and target are deliberately separate: a prebake is JavaScript,
/// but unlike a score it must never replace the active graph or bump the scheduler
/// generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchTarget {
    Score(WatchLanguage),
    Prebake,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReloadStatus {
    Installed,
    Rejected,
}

/// One stable changed file that was either installed or rejected once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReloadEvent {
    pub path: PathBuf,
    pub target: WatchTarget,
    pub status: ReloadStatus,
    pub generation_before: u64,
    pub generation_after: u64,
    pub error_kind: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchPoll {
    Unchanged,
    Pending,
    Event(ReloadEvent),
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Snapshot {
    Source(String),
    Failure { kind: String, message: String },
}

enum Observation {
    Unchanged,
    Changed,
    Pending,
    Ready(Snapshot),
}

enum ApplyAttempt {
    Applied(u64),
    Deferred,
    Failed(crate::RuntimeError),
}

/// A stable-content watcher. Construction treats the current state as the
/// already-loaded baseline, so merely starting a watch cannot bump the
/// schedule generation.
pub struct FileWatch {
    path: PathBuf,
    target: WatchTarget,
    debounce: Duration,
    observed: Snapshot,
    observed_since: Duration,
    delivered: Snapshot,
    /// The text of a score save first seen this poll, kept until the producer
    /// takes it. The save is on disk a whole debounce before it installs, and
    /// that gap is the only chance anything has to start its sounds decoding
    /// before the first onset asks for them.
    #[cfg_attr(not(any(feature = "device-audio", test)), allow(dead_code))]
    changed_source: Option<String>,
    /// Capacity refusals already retried for the observed identity; reset when
    /// a new identity appears (see [`LIVE_CAPACITY_REFUSAL_RETRIES`]).
    capacity_refusals: u8,
    /// The observed identity's last evaluation was a capacity refusal this
    /// watch still retries.
    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    capacity_retry_held: bool,
}

impl FileWatch {
    pub fn new(path: impl Into<PathBuf>, language: WatchLanguage, debounce: Duration) -> Self {
        let path = path.into();
        let baseline = read_snapshot(&path);
        Self::with_baseline(path, WatchTarget::Score(language), baseline, debounce)
    }

    /// Construct from the exact source the caller already evaluated.
    ///
    /// Reading disk again here creates a TOCTOU hole: a save between initial
    /// evaluation and watcher construction becomes the new "already loaded"
    /// baseline and is never evaluated. Supplying the loaded bytes makes that
    /// save observable on the first poll.
    pub fn from_loaded_source(
        path: impl Into<PathBuf>,
        target: WatchTarget,
        source: impl Into<String>,
        debounce: Duration,
    ) -> Self {
        Self::with_baseline(
            path.into(),
            target,
            Snapshot::Source(source.into()),
            debounce,
        )
    }

    fn with_baseline(
        path: PathBuf,
        target: WatchTarget,
        baseline: Snapshot,
        debounce: Duration,
    ) -> Self {
        Self {
            path,
            target,
            debounce,
            observed: baseline.clone(),
            observed_since: Duration::ZERO,
            delivered: baseline,
            changed_source: None,
            capacity_refusals: 0,
            capacity_retry_held: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The text of a score save observed since this was last asked, once.
    ///
    /// Handed over while the save is still debouncing, so the producer can
    /// bet on its sounds before deciding anything about the save itself. A
    /// save that changes again mid-write simply supersedes the earlier bet;
    /// a rejected save's names are warmed all the same, because a name worth
    /// warming is worth warming even when the file it came from is broken.
    #[cfg_attr(not(any(feature = "device-audio", test)), allow(dead_code))]
    pub(crate) fn take_changed_source(&mut self) -> Option<String> {
        self.changed_source.take()
    }

    /// Whether the last rejection this watch reported was a capacity refusal
    /// it retries, so the same bytes are still due another evaluation.
    #[cfg(feature = "device-audio")]
    pub(crate) fn holds_capacity_retry(&self) -> bool {
        self.capacity_retry_held
    }

    /// Observe a score without applying it and distinguish a newly changed
    /// identity from one already stable enough to evaluate.
    ///
    /// The live producer needs both facts: a new save may release a retained
    /// setup retry, while a ready identity with no continuation sample
    /// must wait through one measured scheduling tail before it receives an
    /// execution budget.
    #[cfg(feature = "device-audio")]
    pub(crate) fn observe_live_score_at(
        &mut self,
        session: &Session,
        observed_at: Duration,
    ) -> (WatchPoll, bool, bool) {
        debug_assert!(matches!(self.target, WatchTarget::Score(_)));
        if session.transport().is_stopped() {
            return (WatchPoll::Stopped, false, false);
        }
        match self.observation_at(observed_at) {
            Observation::Unchanged => (WatchPoll::Unchanged, false, false),
            Observation::Changed => (WatchPoll::Pending, true, false),
            Observation::Pending => (WatchPoll::Pending, false, false),
            Observation::Ready(_) => (WatchPoll::Pending, false, true),
        }
    }

    /// Observe a setup without applying it and report whether the unchanged
    /// candidate is already stable enough to evaluate.
    ///
    /// The live producer uses the `ready` bit to take one normal continuation
    /// measurement before the first setup attempt when no high-water sample
    /// exists yet. Returning ordinary `Pending` keeps the identity retryable.
    #[cfg(feature = "device-audio")]
    pub(crate) fn observe_live_prebake_at(
        &mut self,
        session: &Session,
        observed_at: Duration,
    ) -> (WatchPoll, bool) {
        debug_assert_eq!(self.target, WatchTarget::Prebake);
        if session.transport().is_stopped() {
            return (WatchPoll::Stopped, false);
        }
        match self.observation_at(observed_at) {
            Observation::Unchanged => (WatchPoll::Unchanged, false),
            Observation::Changed | Observation::Pending => (WatchPoll::Pending, false),
            Observation::Ready(_) => (WatchPoll::Pending, true),
        }
    }

    /// Observe and, once stable, apply at most one reload for this identity.
    /// Failed identities are also delivered once: retrying the same malformed
    /// file every poll burns CPU and repeats identical diagnostics.
    pub fn poll(&mut self, session: &mut Session, now: Duration) -> WatchPoll {
        self.poll_at(session, now, now.as_secs_f64())
    }

    /// Observe at a wall-clock instant and install at an independent scheduler
    /// clock instant.
    ///
    /// Tests and non-audio callers normally use [`Self::poll`], where both are
    /// the same. Live device playback must debounce against elapsed wall time
    /// while anchoring replacement to the sample clock actually heard by the
    /// callback; conflating them makes scheduler continuity depend on device
    /// startup and buffering latency.
    pub fn poll_at(
        &mut self,
        session: &mut Session,
        observed_at: Duration,
        schedule_now: f64,
    ) -> WatchPoll {
        self.poll_at_inner(session, observed_at, schedule_now, true, None)
    }

    /// Apply a stable setup identity only when the scheduler's current audio
    /// horizon can afford it.
    ///
    /// A temporary lack of cover is `Pending`, not a rejected evaluation. In
    /// particular, the identity is not committed: the producer will refill
    /// the old generation and retry these exact bytes on its next watch poll.
    /// A capacity refusal inside the granted slice is reported, then retried
    /// the same way (see [`LIVE_CAPACITY_REFUSAL_RETRIES`]).
    #[cfg(feature = "device-audio")]
    pub(crate) fn poll_live_prebake_at(
        &mut self,
        session: &mut Session,
        observed_at: Duration,
        schedule_now: f64,
        continuation_reserve: Duration,
    ) -> WatchPoll {
        debug_assert_eq!(self.target, WatchTarget::Prebake);
        self.poll_at_inner(
            session,
            observed_at,
            schedule_now,
            true,
            Some(continuation_reserve),
        )
    }

    /// Apply one stable score identity within the scheduler cover that is
    /// actually left at the device clock.
    ///
    /// The clock is sampled once to derive the execution budget and again only
    /// after successful evaluation to anchor the replacement generation. A
    /// deferral therefore neither runs JavaScript nor commits the file
    /// identity, while a long but successful evaluation cannot anchor the new
    /// graph to a stale pre-evaluation sample.
    /// A capacity refusal inside the granted slice is reported, then retried
    /// on the next poll (see [`LIVE_CAPACITY_REFUSAL_RETRIES`]).
    #[cfg(feature = "device-audio")]
    pub(crate) fn poll_live_score_with_clock(
        &mut self,
        session: &mut Session,
        observed_at: Duration,
        continuation_reserve: Duration,
        commit_rejection: bool,
        schedule_now: &mut dyn FnMut() -> f64,
    ) -> WatchPoll {
        debug_assert!(matches!(self.target, WatchTarget::Score(_)));
        self.poll_with_attempt(
            session,
            observed_at,
            commit_rejection,
            true,
            |session, target, source| {
                let WatchTarget::Score(language) = target else {
                    unreachable!("live score poll used for a setup watcher")
                };
                let transport = session.transport();
                match session.evaluate_live_score_cancellable(
                    source,
                    language == WatchLanguage::Mini,
                    continuation_reserve,
                    transport.stopped_flag(),
                    &mut *schedule_now,
                ) {
                    Ok(crate::session::LiveScoreAttempt::Applied(generation)) => {
                        ApplyAttempt::Applied(generation)
                    }
                    Ok(crate::session::LiveScoreAttempt::Deferred) => ApplyAttempt::Deferred,
                    Err(error) => ApplyAttempt::Failed(error),
                }
            },
        )
    }

    fn poll_at_inner(
        &mut self,
        session: &mut Session,
        observed_at: Duration,
        schedule_now: f64,
        commit_rejection: bool,
        live_prebake_reserve: Option<Duration>,
    ) -> WatchPoll {
        // Only a horizon-derived budget makes a capacity refusal retryable;
        // under the full offline budget it is a verdict on the source.
        self.poll_with_attempt(
            session,
            observed_at,
            commit_rejection,
            live_prebake_reserve.is_some(),
            |session, target, source| match target {
                WatchTarget::Score(language) => {
                    // A save that names a sound, scale or chord that does not
                    // exist is refused like a syntax error, so the last good
                    // score keeps playing instead of a silent one taking over.
                    let library = session.sample_library().cloned();
                    if let Some(reason) = crate::lint::rejection(
                        source,
                        language == WatchLanguage::Mini,
                        library.as_deref(),
                    ) {
                        return ApplyAttempt::Failed(crate::RuntimeError::Message(format!(
                            "refused before playing - {reason}"
                        )));
                    }
                    let transport = session.transport();
                    match session.reload_at_cancellable(
                        source,
                        language == WatchLanguage::Mini,
                        schedule_now,
                        transport.stopped_flag(),
                    ) {
                        Ok(generation) => ApplyAttempt::Applied(generation),
                        Err(error) => ApplyAttempt::Failed(error),
                    }
                }
                WatchTarget::Prebake => {
                    let transport = session.transport();
                    if let Some(reserve) = live_prebake_reserve {
                        match session.evaluate_live_prebake_cancellable(
                            source,
                            schedule_now,
                            reserve,
                            transport.stopped_flag(),
                        ) {
                            Ok(crate::session::LivePrebakeAttempt::Applied) => {
                                ApplyAttempt::Applied(session.generation())
                            }
                            Ok(crate::session::LivePrebakeAttempt::Deferred) => {
                                ApplyAttempt::Deferred
                            }
                            Err(error) => ApplyAttempt::Failed(error),
                        }
                    } else {
                        match session.evaluate_prebake_cancellable(source, transport.stopped_flag())
                        {
                            Ok(()) => ApplyAttempt::Applied(session.generation()),
                            Err(error) => ApplyAttempt::Failed(error),
                        }
                    }
                }
            },
        )
    }

    fn poll_with_attempt(
        &mut self,
        session: &mut Session,
        observed_at: Duration,
        commit_rejection: bool,
        retry_capacity_refusal: bool,
        mut apply: impl FnMut(&mut Session, WatchTarget, &str) -> ApplyAttempt,
    ) -> WatchPoll {
        if session.transport().is_stopped() {
            return WatchPoll::Stopped;
        }

        let current = match self.observation_at(observed_at) {
            Observation::Unchanged => return WatchPoll::Unchanged,
            Observation::Changed | Observation::Pending => return WatchPoll::Pending,
            Observation::Ready(current) => current,
        };
        let generation_before = session.generation();
        let mut capacity_refused = false;
        let poll = match &current {
            Snapshot::Source(source) => match apply(session, self.target, source) {
                ApplyAttempt::Applied(generation_after) => WatchPoll::Event(ReloadEvent {
                    path: self.path.clone(),
                    target: self.target,
                    status: ReloadStatus::Installed,
                    generation_before,
                    generation_after,
                    error_kind: None,
                    message: None,
                }),
                ApplyAttempt::Deferred => return WatchPoll::Pending,
                ApplyAttempt::Failed(error) => {
                    capacity_refused = matches!(error, crate::RuntimeError::ResourceLimit(_));
                    WatchPoll::Event(ReloadEvent {
                        path: self.path.clone(),
                        target: self.target,
                        status: ReloadStatus::Rejected,
                        generation_before,
                        generation_after: session.generation(),
                        error_kind: Some(error.kind().into()),
                        message: Some(error.to_string()),
                    })
                }
            },
            Snapshot::Failure { kind, message } => WatchPoll::Event(ReloadEvent {
                path: self.path.clone(),
                target: self.target,
                status: ReloadStatus::Rejected,
                generation_before,
                generation_after: session.generation(),
                error_kind: Some(kind.clone()),
                message: Some(message.clone()),
            }),
        };
        #[cfg(any(feature = "device-audio", test))]
        if matches!(
            &poll,
            WatchPoll::Event(ReloadEvent {
                status: ReloadStatus::Rejected,
                error_kind: Some(kind),
                ..
            }) if kind.as_str() == "resource-limit"
        ) {
            session.record_atomic_producer_refusal();
        }
        // A retryable capacity refusal leaves the identity uncommitted, so the
        // next poll evaluates the same bytes under the next turn's budget.
        let transient_capacity_refusal = capacity_refused
            && retry_capacity_refusal
            && self.capacity_refusals < LIVE_CAPACITY_REFUSAL_RETRIES;
        if transient_capacity_refusal {
            self.capacity_refusals += 1;
        }
        self.capacity_retry_held = transient_capacity_refusal;
        if (commit_rejection
            || matches!(
                &poll,
                WatchPoll::Event(ReloadEvent {
                    status: ReloadStatus::Installed,
                    ..
                })
            ))
            && !transient_capacity_refusal
        {
            self.delivered = current;
        }
        poll
    }

    fn observation_at(&mut self, observed_at: Duration) -> Observation {
        let current = read_snapshot(&self.path);
        if current != self.observed {
            // Before the move: this is the one moment the new text is in hand
            // and the debounce has not started running out yet.
            if let (Snapshot::Source(source), WatchTarget::Score(_)) = (&current, self.target) {
                self.changed_source = Some(source.clone());
            }
            self.observed = current;
            self.observed_since = observed_at;
            // A new identity starts with its own capacity-retry allowance.
            self.capacity_refusals = 0;
            self.capacity_retry_held = false;
            return Observation::Changed;
        }
        if current == self.delivered {
            return Observation::Unchanged;
        }
        if observed_at.saturating_sub(self.observed_since) < self.debounce {
            return Observation::Pending;
        }
        Observation::Ready(current)
    }
}

fn read_snapshot(path: &Path) -> Snapshot {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => {
            return Snapshot::Failure {
                kind: "io".into(),
                message: format!("cannot read watched file {}: {error}", path.display()),
            };
        }
    };
    match file.metadata() {
        Ok(metadata) if !metadata.is_file() => {
            return Snapshot::Failure {
                kind: "io".into(),
                message: format!("watched path {} is not a regular file", path.display()),
            };
        }
        Ok(_) => {}
        Err(error) => {
            return Snapshot::Failure {
                kind: "io".into(),
                message: format!("cannot inspect watched file {}: {error}", path.display()),
            };
        }
    }

    let mut bytes = Vec::new();
    match file
        .by_ref()
        .take(MAX_WATCH_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
    {
        Ok(_) if bytes.len() as u64 > MAX_WATCH_SOURCE_BYTES => {
            return Snapshot::Failure {
                kind: "resource-limit".into(),
                message: format!(
                    "watched source {} exceeds the {} byte limit",
                    path.display(),
                    MAX_WATCH_SOURCE_BYTES
                ),
            };
        }
        Ok(_) => {}
        Err(error) => {
            return Snapshot::Failure {
                kind: "io".into(),
                message: format!("cannot read watched file {}: {error}", path.display()),
            };
        }
    }

    match String::from_utf8(bytes) {
        Ok(source) => Snapshot::Source(source),
        Err(error) => Snapshot::Failure {
            kind: "evaluation".into(),
            message: format!("watched file {} is not UTF-8: {error}", path.display()),
        },
    }
}

#[cfg(test)]
mod changed_source_tests {
    use super::*;

    const DEBOUNCE: Duration = Duration::from_millis(100);

    fn watch_over(path: &Path, target: WatchTarget, baseline: &str) -> FileWatch {
        FileWatch::from_loaded_source(path, target, baseline, DEBOUNCE)
    }

    /// The producer gets the save's text while it is still debouncing.
    ///
    /// That gap is the only room a decode has: the install and the
    /// replacement's first prefill query happen in one producer turn,
    /// microseconds apart, and an onset that asks for a sound not yet decoded
    /// is skipped for good.
    #[test]
    fn a_saved_score_hands_over_its_text_once_while_it_is_still_debouncing() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("score.strudel.txt");
        let baseline = r#"s("bd sd")"#;
        std::fs::write(&path, baseline).expect("write baseline");
        let mut watch = watch_over(&path, WatchTarget::Score(WatchLanguage::Mini), baseline);
        assert!(
            watch.take_changed_source().is_none(),
            "a watch that has seen no save has nothing to bet on"
        );

        let saved = r#"s("bd sd hh:0")"#;
        std::fs::write(&path, saved).expect("save");
        assert!(matches!(
            watch.observation_at(Duration::ZERO),
            Observation::Changed
        ));
        assert_eq!(
            watch.take_changed_source().as_deref(),
            Some(saved),
            "the new text, at the moment it is first seen"
        );
        assert!(
            watch.take_changed_source().is_none(),
            "one save is one bet, not one per poll"
        );

        // Still inside the debounce: the text has been out for a whole
        // debounce by the time the identity is stable enough to install.
        assert!(matches!(
            watch.observation_at(DEBOUNCE / 2),
            Observation::Pending
        ));
        assert!(watch.take_changed_source().is_none());
        assert!(matches!(
            watch.observation_at(DEBOUNCE * 2),
            Observation::Ready(_)
        ));
        assert!(watch.take_changed_source().is_none());
    }

    /// A save that never parses still names sounds, and a name worth warming
    /// is worth warming when the file it came from is broken: the next save
    /// that fixes the typo should not also be the one that starts the fetch.
    #[test]
    fn an_unparseable_save_still_offers_its_text() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("score.strudel.txt");
        std::fs::write(&path, "s(\"bd\")").expect("write baseline");
        let mut watch = watch_over(&path, WatchTarget::Score(WatchLanguage::Mini), "s(\"bd\")");

        let broken = r#"s("bd hh:0"   <- unclosed"#;
        std::fs::write(&path, broken).expect("save");
        assert!(matches!(
            watch.observation_at(Duration::ZERO),
            Observation::Changed
        ));
        assert_eq!(watch.take_changed_source().as_deref(), Some(broken));
    }

    /// A setup is not a score. Its own warming is the studio's, on the setup
    /// seam; handing it down the score path would warm the wrong text.
    #[test]
    fn a_prebake_change_offers_nothing_to_the_score_warm() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("prebake.js");
        std::fs::write(&path, "// setup").expect("write baseline");
        let mut watch = watch_over(&path, WatchTarget::Prebake, "// setup");

        std::fs::write(&path, "samples('github:x/y')").expect("save");
        assert!(matches!(
            watch.observation_at(Duration::ZERO),
            Observation::Changed
        ));
        assert!(watch.take_changed_source().is_none());
    }
}
