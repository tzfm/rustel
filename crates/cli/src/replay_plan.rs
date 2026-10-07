use std::sync::Arc;

use rustel_runtime::session_log::{STOP_SOURCE, SessionSave, SessionScript};
#[cfg(feature = "device-audio")]
use rustel_runtime::{LiveProducerStep, ReloadStatus, Session, WatchPoll, WatchTarget};

/// What a replay from `from` opens on and which saves follow, rejected ones
/// included. Live replay and `replay --export` both read this plan.
pub struct ReplayPlan {
    pub opening: ReplayOpening,
    /// Every save strictly after `from`, in tape order.
    pub later: Vec<ReplayCue>,
}

/// The state a replay opens on at offset zero.
pub enum ReplayOpening {
    /// The latest save at or before `from` that the tape marks installed, at
    /// its tape position.
    Sounding { index: usize, save: SessionSave },
    /// No save at or before `from` is marked installed, so the replay is
    /// silent until the first save that installs (in an export, the first
    /// the tape marks installed). `held` is the latest save at or before
    /// `from`, which was rejected.
    Silent { held: Option<SessionSave> },
}

/// One save after `from`.
pub struct ReplayCue {
    /// Seconds after `from`, on the tape's clock.
    pub offset: f64,
    /// Zero-based position on the tape.
    pub index: usize,
    pub save: SessionSave,
}

impl ReplayPlan {
    pub fn new(script: &SessionScript, from: f64) -> Self {
        let opening = match script.installed_save_at(from) {
            Some((index, save)) => ReplayOpening::Sounding {
                index,
                save: save.clone(),
            },
            None => ReplayOpening::Silent {
                held: script
                    .saves
                    .iter()
                    .rev()
                    .find(|save| save.at <= from)
                    .cloned(),
            },
        };
        let later = script
            .saves
            .iter()
            .enumerate()
            .filter(|(_, save)| save.at > from)
            .map(|(index, save)| ReplayCue {
                offset: save.at - from,
                index,
                save: save.clone(),
            })
            .collect();
        Self { opening, later }
    }

    /// Whether any save in the plan installed. A plan where none did has
    /// nothing to bounce.
    pub fn any_installed(&self) -> bool {
        self.installed_saves().next().is_some()
    }

    /// The score live replay writes to the watched file before the first
    /// cue: the sounding opening, or [`STOP_SOURCE`] when the opening is
    /// silent.
    pub fn watched_opening(&self) -> &str {
        match &self.opening {
            ReplayOpening::Sounding { save, .. } => &save.source,
            ReplayOpening::Silent { .. } => STOP_SOURCE,
        }
    }

    /// Whether live replay starts its first installed reload on the score's
    /// own first beat, as `--export` starts its first timeline entry. True
    /// when the opening is silent.
    pub fn first_install_from_zero(&self) -> bool {
        matches!(self.opening, ReplayOpening::Silent { .. })
    }

    /// The `--export` timeline: [`Self::installed_saves`], with entries at or
    /// past `until` omitted so they are never evaluated. Offsets ignore
    /// `--speed`. Each entry shares its save's body.
    pub fn export_timeline(&self, until: Option<f64>) -> Vec<(f64, Arc<str>)> {
        self.installed_saves()
            .filter(|(offset, _)| until.is_none_or(|until| *offset < until))
            .map(|(offset, save)| (offset, Arc::clone(&save.source)))
            .collect()
    }

    /// The saves the tape marks installed, at their offsets: a sounding
    /// opening at zero, then each installed cue.
    fn installed_saves(&self) -> impl Iterator<Item = (f64, &SessionSave)> {
        let opening = match &self.opening {
            ReplayOpening::Sounding { save, .. } => Some((0.0, save)),
            ReplayOpening::Silent { .. } => None,
        };
        let later = self
            .later
            .iter()
            .filter(|cue| cue.save.installed())
            .map(|cue| (cue.offset, &cue.save));
        opening.into_iter().chain(later)
    }
}

/// Starts the first score a live reload installs on its own first beat, as
/// `--export` does. Every producer step arms [`Session::start_next_from_zero`]
/// while the first install is pending; the step that reports an installed
/// score reload stops arming and withdraws the request.
#[cfg(feature = "device-audio")]
pub struct FirstInstallFromZero {
    pending: bool,
}

#[cfg(feature = "device-audio")]
impl FirstInstallFromZero {
    pub fn new(pending: bool) -> Self {
        Self { pending }
    }

    /// Run one producer step, armed while the first install is pending. The
    /// step that reports an installed score reload stops arming and
    /// withdraws the request.
    pub fn step<E>(
        &mut self,
        session: &mut Session,
        step: impl FnOnce(&mut Session) -> Result<LiveProducerStep, E>,
    ) -> Result<LiveProducerStep, E> {
        if self.pending {
            session.start_next_from_zero();
        }
        let result = step(session);
        if self.pending
            && let Ok(LiveProducerStep {
                watch: WatchPoll::Event(event),
                ..
            }) = &result
            && event.status == ReloadStatus::Installed
            && matches!(event.target, WatchTarget::Score(_))
        {
            self.pending = false;
            session.clear_next_from_zero();
        }
        result
    }
}

#[cfg(test)]
mod replay_plan_tests {
    use super::*;
    use rustel_runtime::session_log::{SaveStatus, SessionMode};

    fn installed(at: f64, source: &str) -> SessionSave {
        recorded(at, SaveStatus::Installed, source)
    }

    fn rejected(at: f64, source: &str) -> SessionSave {
        recorded(at, SaveStatus::Rejected, source)
    }

    fn recorded(at: f64, status: SaveStatus, source: &str) -> SessionSave {
        SessionSave {
            at,
            status: status.as_str().into(),
            source: source.into(),
            error: None,
            via: None,
        }
    }

    /// A debug tape, which keeps rejected saves.
    fn tape(saves: Vec<SessionSave>) -> SessionScript {
        SessionScript {
            keeps: SessionMode::Debug.keeps().into(),
            recorded: None,
            baseline_cps: None,
            saves,
            logs: 0,
        }
    }

    fn timeline(script: &SessionScript, from: f64, until: Option<f64>) -> Vec<(f64, Arc<str>)> {
        ReplayPlan::new(script, from).export_timeline(until)
    }

    fn entries(expected: &[(f64, &str)]) -> Vec<(f64, Arc<str>)> {
        expected
            .iter()
            .map(|(at, source)| (*at, (*source).into()))
            .collect()
    }

    /// Offset, tape position and source of each cue live replay delivers.
    fn cues(plan: &ReplayPlan) -> Vec<(f64, usize, String)> {
        plan.later
            .iter()
            .map(|cue| (cue.offset, cue.index, cue.save.source.to_string()))
            .collect()
    }

    /// A save exactly at `--from` is the opening and appears once.
    #[test]
    fn a_save_exactly_at_from_opens_once_and_is_not_rechained() {
        let script = tape(vec![
            installed(0.0, "A"),
            installed(5.0, "B"),
            installed(8.0, "C"),
        ]);
        assert_eq!(
            timeline(&script, 5.0, None),
            entries(&[(0.0, "B"), (3.0, "C")]),
        );
    }

    /// `--duration` keeps the opening and omits saves at or past the window end.
    #[test]
    fn duration_keeps_the_opening_and_truncates_the_chain() {
        let script = tape(vec![installed(0.0, "A"), installed(8.0, "B")]);
        assert_eq!(timeline(&script, 0.0, Some(4.0)), entries(&[(0.0, "A")]));
        assert_eq!(timeline(&script, 0.0, Some(8.0)), entries(&[(0.0, "A")]));
    }

    /// A `--from` past the last save bounces the closing state.
    #[test]
    fn a_from_past_the_end_bounces_the_closing_state() {
        let script = tape(vec![installed(0.0, "A"), installed(8.0, "B")]);
        assert_eq!(timeline(&script, 600.0, None), entries(&[(0.0, "B")]));
    }

    /// A first installed save after `--from` lands at its own offset, not at zero.
    #[test]
    fn a_tape_whose_first_save_is_after_from_opens_silent() {
        let script = tape(vec![installed(2.0, "A"), installed(6.0, "B")]);
        assert_eq!(
            timeline(&script, 0.0, None),
            entries(&[(2.0, "A"), (6.0, "B")]),
        );
        assert_eq!(
            timeline(&script, 1.0, None),
            entries(&[(1.0, "A"), (5.0, "B")]),
        );
    }

    /// A rejected first save leaves the timeline as it is without it.
    #[test]
    fn a_rejected_first_save_leaves_the_timeline_unchanged() {
        let without = tape(vec![installed(30.0, "B")]);
        let with = tape(vec![rejected(0.0, "broken("), installed(30.0, "B")]);
        for script in [&without, &with] {
            assert_eq!(timeline(script, 0.0, None), entries(&[(30.0, "B")]));
            assert_eq!(timeline(script, 10.0, None), entries(&[(20.0, "B")]));
        }
    }

    /// A window that closes before the first installed save is an empty timeline.
    #[test]
    fn a_window_before_anything_sounded_is_an_empty_timeline() {
        for script in [
            tape(vec![rejected(0.0, "broken("), installed(100.0, "B")]),
            tape(vec![installed(100.0, "B")]),
        ] {
            assert_eq!(timeline(&script, 0.0, Some(50.0)), vec![]);
        }
    }

    /// Rejected saves after the opening are left out of the export timeline.
    #[test]
    fn rejected_saves_never_chain() {
        let script = tape(vec![
            installed(0.0, "A"),
            rejected(3.0, "broken("),
            installed(5.0, "B"),
        ]);
        assert_eq!(
            timeline(&script, 0.0, None),
            entries(&[(0.0, "A"), (5.0, "B")]),
        );
    }

    /// `any_installed` is false only when no save in the plan installed.
    #[test]
    fn any_installed_is_false_only_when_nothing_installed() {
        assert!(!ReplayPlan::new(&tape(vec![rejected(0.0, "broken(")]), 0.0).any_installed());
        let late = tape(vec![rejected(0.0, "broken("), installed(9.0, "A")]);
        assert!(ReplayPlan::new(&late, 0.0).any_installed());
        assert!(ReplayPlan::new(&late, 600.0).any_installed());
    }

    /// Live replay opens on the sounding save and delivers every later save,
    /// rejected ones included, with no from-zero start.
    #[test]
    fn the_live_replay_opens_on_what_sounded_and_delivers_every_later_save() {
        let script = tape(vec![
            installed(0.0, "A"),
            rejected(3.0, "broken("),
            installed(5.0, "B"),
        ]);
        let plan = ReplayPlan::new(&script, 2.0);
        assert!(matches!(
            plan.opening,
            ReplayOpening::Sounding { index: 0, .. }
        ));
        assert_eq!(plan.watched_opening(), "A");
        assert!(!plan.first_install_from_zero());
        assert_eq!(
            cues(&plan),
            vec![(1.0, 1, "broken(".to_string()), (3.0, 2, "B".to_string())],
        );
    }

    /// With nothing installed by `--from`, live replay opens on the stop score,
    /// holds the rejected save at or before `--from` for reporting, and starts
    /// the first installed save from zero at its offset.
    #[test]
    fn the_live_replay_opens_silent_before_the_first_install() {
        let rejected_first = tape(vec![rejected(0.0, "broken("), installed(2.0, "A")]);
        let plan = ReplayPlan::new(&rejected_first, 0.0);
        assert!(matches!(
            &plan.opening,
            ReplayOpening::Silent { held: Some(save) } if &*save.source == "broken("
        ));
        assert_eq!(plan.watched_opening(), STOP_SOURCE);
        assert!(plan.first_install_from_zero());
        assert_eq!(cues(&plan), vec![(2.0, 1, "A".to_string())]);

        let late = tape(vec![installed(12.0, "A"), installed(30.0, "B")]);
        let plan = ReplayPlan::new(&late, 0.0);
        assert!(matches!(plan.opening, ReplayOpening::Silent { held: None }));
        assert_eq!(plan.watched_opening(), STOP_SOURCE);
        assert!(plan.first_install_from_zero());
        assert_eq!(
            cues(&plan),
            vec![(12.0, 0, "A".to_string()), (30.0, 1, "B".to_string())],
        );
    }

    /// Live replay starts the first installed save on its own first beat, as
    /// `--export` does, after an output-recycle requery and a rejected save, and
    /// the next save continues that count in both. The live half mirrors
    /// `play_live`'s startup and producer steps, without the audio device.
    #[cfg(feature = "device-audio")]
    #[test]
    fn live_replay_and_export_agree_on_where_the_first_saves_start() {
        use rustel_runtime::WatchLanguage;
        use std::time::Duration;

        let script = tape(vec![
            rejected(5.0, "note("),
            installed(12.5, r#"note("<c3 e3 g3 a3>")"#),
            installed(20.0, r#"note("<c4 e4>")"#),
        ]);
        let plan = ReplayPlan::new(&script, 0.0);
        let [rejected_cue, first, second] = &plan.later[..] else {
            panic!("the plan did not hold three cues");
        };
        let directory = tempfile::tempdir().expect("temp dir");

        let mut bounce = Session::new().expect("session");
        bounce
            .render_session(
                &plan.export_timeline(Some(21.0)),
                crate::REPLAY_EXPORT_TAIL_SECS,
                Some(21.0),
                &directory.path().join("bounce.wav"),
                false,
            )
            .expect("bounce");
        assert_eq!(bounce.cycle_at_time(first.offset), 0.0);
        assert!(bounce.cycle_at_time(second.offset) > 0.0);

        let score = directory.path().join("replay.strudel");
        std::fs::write(&score, plan.watched_opening()).expect("watched file");
        let input = crate::SourceInput {
            file: Some(score.clone()),
            eval: None,
            sample_access: crate::SampleAccessArgs::default(),
        };
        let mut live = Session::new().expect("session");
        let (loaded, startup_error) = crate::load_watch_musician_sources(
            &mut live,
            &input,
            None,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .expect("watch startup");
        assert!(startup_error.is_none(), "{startup_error:?}");
        let mut producer = crate::build_live_producer(
            &score,
            WatchLanguage::JavaScript,
            &loaded,
            Duration::from_millis(100),
            Duration::from_millis(20),
            Duration::from_millis(2),
        )
        .expect("live producer");
        live.restart_transport_at(0.0);
        let mut first_install = FirstInstallFromZero::new(plan.first_install_from_zero());
        assert!(
            live.requery_after_output_recycle_at(1.0)
                .expect("recycle requery")
                .is_some(),
            "the stop score was not requeried"
        );

        let mut observed = Duration::ZERO;
        let mut deliver = |cue: &ReplayCue| {
            std::fs::write(&score, cue.save.source.as_bytes()).expect("write cue");
            for _ in 0..4 {
                observed += Duration::from_millis(60);
                let step = first_install
                    .step(&mut live, |session| {
                        producer.step(
                            session,
                            observed,
                            cue.offset,
                            48_000,
                            |_, _, _| {},
                            |_| true,
                        )
                    })
                    .expect("producer step");
                if let WatchPoll::Event(event) = step.watch {
                    return (event.status, live.cycle_at_time(cue.offset));
                }
            }
            panic!("the save at {} s did not reload", cue.save.at);
        };

        assert_eq!(deliver(rejected_cue).0, ReloadStatus::Rejected);
        assert_eq!(
            deliver(first),
            (ReloadStatus::Installed, bounce.cycle_at_time(first.offset)),
            "live replay and --export start the first installed save on different beats"
        );
        assert_eq!(
            deliver(second),
            (ReloadStatus::Installed, bounce.cycle_at_time(second.offset)),
            "live replay and --export disagree on where the second save continues"
        );
    }

    /// A from-zero request is withdrawn when the first install is reported, even
    /// when the reload that consumed it ran on an earlier step, so the next reload
    /// continues the running count.
    #[cfg(feature = "device-audio")]
    #[test]
    fn a_deferred_first_install_report_withdraws_the_from_zero_request() {
        use rustel_runtime::{ReloadEvent, WatchLanguage};

        fn stepped(watch: WatchPoll) -> Result<LiveProducerStep, ()> {
            Ok(LiveProducerStep {
                watch,
                prebake_watch: WatchPoll::Unchanged,
                scheduled: 0,
                pushed: 0,
                pending: 0,
                backpressured: false,
            })
        }

        let mut session = Session::new().expect("session");
        session.evaluate(STOP_SOURCE).expect("stop score");
        let mut first_install = FirstInstallFromZero::new(true);

        // The first reload consumes the request; its cutover is deferred.
        first_install
            .step(&mut session, |session| {
                session
                    .reload_at(r#"note("c3 e3")"#, false, 4.0)
                    .expect("first reload");
                stepped(WatchPoll::Unchanged)
            })
            .expect("first step");
        assert_eq!(session.cycle_at_time(4.0), 0.0);

        // A later step reports that install without reloading.
        first_install
            .step(&mut session, |_| {
                stepped(WatchPoll::Event(ReloadEvent {
                    path: "replay.strudel".into(),
                    target: WatchTarget::Score(WatchLanguage::JavaScript),
                    status: ReloadStatus::Installed,
                    generation_before: 1,
                    generation_after: 2,
                    error_kind: None,
                    message: None,
                }))
            })
            .expect("reporting step");

        // The next reload continues the count.
        session
            .reload_at(r#"note("g3")"#, false, 8.0)
            .expect("second reload");
        assert_eq!(session.cycle_at_time(8.0), (8.0 - 4.0) * session.cps());
    }
}
