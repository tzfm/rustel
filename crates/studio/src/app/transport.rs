//! Quitting and play state: the two-press Ctrl+Q quit (the chord itself,
//! claimed after the focused editor's Vim keys, and the press that calls an armed quit
//! off) and the immediate quit that stops audio and flushes scenes, the set
//! and prefs, plus the playing / stopping / evaluating checks, arming,
//! clean-up after playback stops, and the check that skips reinstalling an
//! identical score. It also holds the footer's per-owner error slots
//! (`ErrorOwner`, `AppErrors`) and `set_error` / `clear_error`, which show
//! an error and also write it to the log.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ErrorOwner {
    Editor,
    /// A prebake the checker or the engine refused. It outlives a score's
    /// refusal on purpose: the setup is still broken after the message that
    /// pushed it off the footer.
    Setup,
    Evaluation,
    Save,
    Preferences,
    Engine,
    Interface,
}

impl ErrorOwner {
    /// Every owner, in slot order.
    ///
    /// The footer keeps one message per owner in a fixed array indexed by
    /// this order, so the list and the enum have to agree. [`Self::slot`]
    /// is what keeps them honest: it matches exhaustively, so adding a
    /// variant without adding it here does not compile.
    pub(super) const ALL: [Self; 7] = [
        Self::Editor,
        Self::Setup,
        Self::Evaluation,
        Self::Save,
        Self::Preferences,
        Self::Engine,
        Self::Interface,
    ];

    /// Which slot this owner's message lives in.
    pub(super) const fn slot(self) -> usize {
        match self {
            Self::Editor => 0,
            Self::Setup => 1,
            Self::Evaluation => 2,
            Self::Save => 3,
            Self::Preferences => 4,
            Self::Engine => 5,
            Self::Interface => 6,
        }
    }

    /// Temporary footer errors and their header alerts share a lifetime.
    pub(super) const fn transient_alert(self) -> Option<&'static str> {
        match self {
            Self::Editor => Some("error:editor"),
            Self::Interface => Some("error:interface"),
            Self::Setup | Self::Evaluation | Self::Save | Self::Preferences | Self::Engine => None,
        }
    }

    /// Engine failures stay visible until playback recovers; unlike a
    /// transient alert, their footer message must not expire on a timer.
    pub(super) const fn log_alert(self) -> Option<&'static str> {
        match self {
            Self::Engine => Some("error:engine"),
            _ => self.transient_alert(),
        }
    }
}

pub(super) const ERROR_OWNER_COUNT: usize = ErrorOwner::ALL.len();

const TRANSIENT_MESSAGE_DURATION: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub(super) struct OwnedError {
    sequence: u64,
    message: String,
    pub(super) expires_at: Option<Instant>,
}

#[derive(Default)]
pub(super) struct AppErrors {
    pub(super) entries: [Option<OwnedError>; ERROR_OWNER_COUNT],
    next_sequence: u64,
}

impl AppErrors {
    pub(super) fn set(&mut self, owner: ErrorOwner, message: String) {
        self.set_at(owner, message, Instant::now());
    }

    pub(super) fn set_at(&mut self, owner: ErrorOwner, message: String, now: Instant) {
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.entries[owner.slot()] = Some(OwnedError {
            sequence: self.next_sequence,
            message,
            // A refused gesture or clipboard action is temporary. Broken
            // scores, saves, setup and audio keep their error until resolved.
            expires_at: owner
                .transient_alert()
                .is_some()
                .then_some(now + TRANSIENT_MESSAGE_DURATION),
        });
    }

    /// Remove expired messages, yielding their owners to retire the badges too.
    pub(super) fn expire(&mut self, now: Instant) -> impl Iterator<Item = ErrorOwner> + '_ {
        ErrorOwner::ALL.into_iter().filter(move |owner| {
            let entry = &mut self.entries[owner.slot()];
            if entry
                .as_ref()
                .and_then(|error| error.expires_at)
                .is_some_and(|deadline| now >= deadline)
            {
                *entry = None;
                true
            } else {
                false
            }
        })
    }

    pub(super) fn clear(&mut self, owner: ErrorOwner) -> bool {
        self.entries[owner.slot()].take().is_some()
    }

    /// One owner's message, whether or not it is the one on show.
    pub(super) fn get(&self, owner: ErrorOwner) -> Option<&str> {
        self.entries[owner.slot()]
            .as_ref()
            .map(|error| error.message.as_str())
    }

    pub(super) fn visible(&self) -> Option<&str> {
        self.entries
            .iter()
            .flatten()
            .max_by_key(|error| error.sequence)
            .map(|error| error.message.as_str())
    }
}

impl App {
    pub(super) fn set_notice(&mut self, message: String) {
        self.status_notice = Some((message.clone(), Instant::now() + TRANSIENT_MESSAGE_DURATION));
        self.status = message;
        self.dirty_frame = true;
    }

    /// Retire a confirmation without clearing a newer prompt or error.
    pub(super) fn expire_notice(&mut self, now: Instant) {
        let Some((message, deadline)) = &self.status_notice else {
            return;
        };
        if self.status != *message {
            self.status_notice = None;
        } else if now >= *deadline {
            self.status.clear();
            self.status_notice = None;
            self.dirty_frame = true;
        }
    }

    pub(super) fn settle_stopped_transport(&mut self) {
        // A stop word for a transport already stopped settles nothing: it
        // would reset the advisories and discard the visual update the
        // held frame is showing - a flicker on a picture at rest. The
        // snapshot trails a real stop, so its last word still says playing
        // when the word the stop owes arrives.
        if !self.stop_requested && !self.is_playing() && !self.is_stopping() {
            return;
        }
        self.audio_advisory.reset();
        self.power_advisory.reset();
        self.worker.discard_visual_updates();
        self.visual.stop();
        self.pending_slider = None;
        // A waiting launch was cancelled by the stop. Keep plain updates:
        // a stop-then-update restart still wants its evaluation outcome.
        self.forget_arming();
        self.landing_name = None;
        self.inflight
            .retain(|_, pending| matches!(pending.launch, Launch::Now));
        #[cfg(feature = "hydra")]
        {
            self.snippet_preview = None;
            self.snippet_heard = false;
        }
        self.stop_requested = false;
        self.dirty_frame = true;
    }

    /// Unclaimed Ctrl+Q quits - except while a keybind learn holds the
    /// Settings sheet, where the chord is refused as a shortcut instead.
    /// `true` when the press was Ctrl+Q.
    pub(super) fn quit_key(&mut self, terminal_event: &Event) -> bool {
        let ctrl_q = matches!(terminal_event, Event::Key(key)
        if key.kind == KeyEventKind::Press
            && key.code == KeyCode::Char('q')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.intersects(
                KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::SUPER
            ));
        if ctrl_q && self.keybind_learn.is_some() && self.settings_sheet.is_some() {
            self.status = "^Q is reserved for quit - choose another shortcut".into();
            self.dirty_frame = true;
            return true;
        }
        if ctrl_q {
            self.close_piano_mode();
            self.request_quit();
            return true;
        }
        false
    }

    /// `^Q`: ask once, then go.
    ///
    /// Quitting is the one shortcut with nothing behind it - the set stops,
    /// the sound stops, and the terminal comes back. Every edited scene is
    /// written on the way out, and a write that fails is reported in the
    /// shell. But a live set is not something to end by a finger-slip, and
    /// `^Q` sits next to `^W` (close scene) and `^E` (split) on the same
    /// hand. The menu's Quit row is a stray click away from its neighbours
    /// too.
    ///
    /// So the first press arms and says so, and the second goes. Any other
    /// key calls it off - the same shape as the theme picker's second `d`,
    /// and deliberately not a timer, which would decide by a number nobody
    /// can defend.
    ///
    /// The external cancellation flag (a signal, `Ctrl-C`) is a different
    /// path entirely and is never asked twice: the answer there was given
    /// outside the studio.
    pub(super) fn request_quit(&mut self) {
        if !self.quit_armed {
            self.quit_armed = true;
            self.status = format!("{} again to quit", self.keybinds.hint(BindAction::Quit));
            self.dirty_frame = true;
            return;
        }
        self.quit_now();
    }

    /// Any press that is not the Quit chord cancels an armed quit.
    pub(super) fn cancel_armed_quit(&mut self, terminal_event: &Event) {
        if let Event::Key(key) = terminal_event
            && key.kind == KeyEventKind::Press
            && self.quit_armed
            && (key.modifiers.contains(KeyModifiers::ALT)
                || self.keybinds.action_for(key) != Some(BindAction::Quit))
        {
            self.quit_armed = false;
            self.status = "quit cancelled".into();
            self.dirty_frame = true;
        }
    }

    /// Quit, no questions: the menu's Quit is a choice made with the eyes
    /// on it, not a chord a hand can land on by accident, so it goes at
    /// once. Everything on screen reaches disk before the shell comes
    /// back; the save worker is joined after the terminal is restored.
    pub(super) fn quit_now(&mut self) {
        self.quit_armed = false;
        // Silence first, then the filing.
        //
        // `request_stop` is an atomic flag the audio callback reads, so the
        // sound ends on the next buffer and over the choke ramp - five
        // milliseconds, not a click. The writes that follow take longer
        // than that: a scene goes to the save worker, the set file and the
        // preferences are each an atomic write, and on the way out the
        // save worker is joined. Doing them first would let a window closed
        // mid-set go on playing after it has left the screen.
        //
        // Nothing about the saving needs the engine: the scores are the
        // editor's text, and the set file and preferences are the
        // interface's own state.
        self.worker.request_stop();
        // A limiter mode still settling is what the strip showed last, so
        // it is part of what reaches disk.
        self.settle_limiter_mode();
        self.flush_dirty_scenes();
        self.flush_manifest();
        self.flush_prefs();
        self.quit = true;
    }

    /// Whether installing this source would replace the sounding score with
    /// a byte-identical one.
    ///
    /// Known cost, accepted deliberately: re-evaluating identical text is
    /// observable - a score keeping state on `window` advances each time.
    /// Upstream always re-evaluates. Here those scores do not step on a
    /// repeat press, and the status line says to stop first, which restarts
    /// at cycle zero.
    ///
    /// The alternative is to ignore presses inside a time window. That
    /// keeps those scores stepping, but the window length is arbitrary.
    ///
    /// Only while playing and not stopping. Stopped, update is how you start,
    /// and the text being unchanged is beside the point; mid-stop, the next
    /// update is how you cut the tail short.
    ///
    /// And only from the scene that is sounding: a duplicate of it is
    /// another scene with the same text, and launching that one is how the
    /// playing mark, the highlights and the tape move over to it.
    pub(super) fn install_would_be_redundant(&self, scene: SceneId, source: &str) -> bool {
        // A launch waiting for its cycle line is consumed by QUEUEING an
        // evaluation. Returning early would leave it in place, so a later
        // ordinary update would inherit a quantised launch it never asked
        // for. (An armed scene is no reason: the same text re-pressed is
        // answered before this, and a different text is a new launch.)
        if !matches!(self.next_launch, Launch::Now) {
            return false;
        }
        self.is_playing()
            && !self.is_stopping()
            && self.audible_scene == Some(scene)
            && self.installed_revision.as_deref() == Some(source_revision(source).as_str())
    }

    pub(super) fn is_playing(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.playing)
    }

    /// Whether event ranges may be described as sounding in the score.
    /// Tails remain audible during a graceful stop, but they are no longer
    /// scheduled events and must not keep their source text highlighted.
    pub(super) fn sounding_marks_visible(&self) -> bool {
        self.is_playing() && !self.is_stopping() && !self.stop_requested
    }

    /// The arming is over - fired, cancelled or stopped.
    pub(super) fn forget_arming(&mut self) {
        self.armed_scene = None;
        self.armed_request = None;
        self.armed_revision = None;
    }

    pub(super) fn is_stopping(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.stopping)
    }

    pub(super) fn is_evaluating(&self) -> bool {
        // A launch waiting for its cycle line is armed, not evaluating: the
        // header keeps PLAYING and shows the countdown beside it. An update
        // held for its sounds is the same: the loading line says so.
        let armed = self.armed_scene.is_some()
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.launch.is_some());
        let held = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.loading.as_ref())
            .is_some_and(|cue| cue.waiting);
        (!self.inflight.is_empty() && !armed && !held)
            || self.pending_evaluation.is_some()
            || self.prebake_inflight.is_some()
            || !self.prebake_queue.is_empty()
    }

    /// Every error the footer shows is also a line in the log, where it
    /// survives being overwritten by the next one.
    pub(super) fn set_error(&mut self, owner: ErrorOwner, message: String) {
        let kind = match owner {
            ErrorOwner::Editor => "editor",
            ErrorOwner::Setup => "setup",
            ErrorOwner::Evaluation => "update",
            ErrorOwner::Save => "save",
            ErrorOwner::Preferences => "prefs",
            ErrorOwner::Engine => "engine",
            ErrorOwner::Interface => "interface",
        };
        if let Some(alert) = owner.log_alert() {
            self.log
                .push_alert(LogLevel::Error, kind, message.clone(), alert);
        } else {
            self.log.push(LogLevel::Error, kind, message.clone());
        }
        self.errors.set(owner, message);
        self.dirty_frame = true;
    }

    pub(super) fn clear_error(&mut self, owner: ErrorOwner) {
        self.dirty_frame |= self.errors.clear(owner);
        if let Some(alert) = owner.log_alert() {
            self.dirty_frame |= if owner == ErrorOwner::Engine {
                self.log.resolve_alert(alert) > 0
            } else {
                self.log.retire_alert(alert) > 0
            };
        }
    }
}

#[cfg(test)]
mod tests {
    mod transport {
        //! Tests for `app/transport.rs`.

        use crate::app::tests::support::*;
        use crate::app::transport::*;

        /// The one shortcut with nothing behind it asks before it goes.
        #[test]
        fn quit_asks_twice_and_anything_else_calls_it_off() {
            let directory = tempfile::tempdir().expect("temp dir");

            // One press arms and says so; it does not quit.
            let mut app = app_over(directory.path());
            press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
            assert!(!app.quit, "a single ^Q quit the studio");
            assert!(app.quit_armed, "^Q did not arm the quit");
            assert!(
                app.status.contains("again to quit"),
                "the status does not say how to confirm: {}",
                app.status
            );

            // The second goes.
            press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
            assert!(app.quit, "^Q twice did not quit");

            // Any other key calls it off - including the transport, which is the
            // most likely thing to be reaching for when the finger slipped.
            for (code, modifiers) in [
                (KeyCode::Char('s'), KeyModifiers::CONTROL),
                (KeyCode::Esc, KeyModifiers::NONE),
                (KeyCode::Char('x'), KeyModifiers::NONE),
            ] {
                let mut app = app_over(directory.path());
                press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
                assert!(app.quit_armed);
                press(&mut app, code, modifiers);
                assert!(!app.quit_armed, "{code:?} left the quit armed");
                assert!(!app.quit, "{code:?} let the studio quit");
                // And a later ^Q has to start over.
                press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
                assert!(
                    !app.quit,
                    "the disarmed press still counted toward quitting"
                );
                assert!(app.quit_armed);
            }
        }

        #[test]
        fn errors_clear_only_their_owner_and_newest_remaining_error_is_visible() {
            let mut errors = AppErrors::default();
            errors.set(ErrorOwner::Evaluation, "compile failed".into());
            errors.set(ErrorOwner::Editor, "clipboard failed".into());
            assert_eq!(errors.visible(), Some("clipboard failed"));

            assert!(errors.clear(ErrorOwner::Editor));
            assert_eq!(errors.visible(), Some("compile failed"));
            assert!(!errors.clear(ErrorOwner::Save));
            assert_eq!(errors.visible(), Some("compile failed"));

            errors.set(ErrorOwner::Engine, "device failed".into());
            errors.clear(ErrorOwner::Evaluation);
            assert_eq!(errors.visible(), Some("device failed"));
        }

        #[test]
        fn transient_errors_expire_even_when_hidden_and_new_errors_get_five_seconds() {
            let now = Instant::now();
            let mut errors = AppErrors::default();
            errors.set_at(ErrorOwner::Editor, "clipboard failed".into(), now);
            errors.set_at(ErrorOwner::Interface, "last scene".into(), now);
            assert_eq!(errors.expire(now + Duration::from_secs(4)).count(), 0);
            errors.set_at(
                ErrorOwner::Interface,
                "last scene again".into(),
                now + Duration::from_secs(4),
            );
            assert_eq!(
                errors
                    .expire(now + Duration::from_secs(5))
                    .collect::<Vec<_>>(),
                [ErrorOwner::Editor]
            );
            assert!(errors.get(ErrorOwner::Editor).is_none());
            assert_eq!(errors.visible(), Some("last scene again"));
            assert_eq!(errors.expire(now + Duration::from_secs(8)).count(), 0);
            assert_eq!(
                errors
                    .expire(now + Duration::from_secs(9))
                    .collect::<Vec<_>>(),
                [ErrorOwner::Interface]
            );
            assert!(errors.visible().is_none());
        }

        #[test]
        fn theme_notices_expire_without_clearing_prompts_or_errors() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.keep_theme();
            assert!(app.status.contains("remembered in"));
            let old_deadline = Instant::now();
            app.status_notice.as_mut().unwrap().1 = old_deadline;
            app.keep_theme();
            app.expire_notice(old_deadline);
            assert!(
                !app.status.is_empty(),
                "repeating a notice renews its lifetime"
            );

            let deadline = app.status_notice.as_ref().unwrap().1;
            app.request_quit();
            let prompt = app.status.clone();
            app.expire_notice(deadline);
            assert_eq!(app.status, prompt, "the quit prompt still needs an answer");
            assert!(app.status_notice.is_none());

            app.keep_theme();
            app.set_error(ErrorOwner::Save, "save failed".into());
            let deadline = app.status_notice.as_ref().unwrap().1;
            app.expire_notice(deadline);
            assert!(app.status.is_empty(), "a hidden notice expires too");
            assert_eq!(app.errors.visible(), Some("save failed"));
        }

        #[test]
        fn transient_errors_do_not_dismiss_unresolved_score_save_or_engine_errors() {
            for owner in [
                ErrorOwner::Setup,
                ErrorOwner::Evaluation,
                ErrorOwner::Save,
                ErrorOwner::Engine,
            ] {
                let now = Instant::now();
                let mut errors = AppErrors::default();
                errors.set_at(owner, "needs attention".into(), now);
                errors.set_at(ErrorOwner::Interface, "last scene".into(), now);
                assert_eq!(errors.visible(), Some("last scene"));
                assert_eq!(errors.expire(now + Duration::from_secs(5)).count(), 1);
                assert_eq!(errors.visible(), Some("needs attention"));
                assert_eq!(errors.expire(now + Duration::from_secs(3600)).count(), 0);
            }
        }

        #[test]
        fn interface_error_clears_on_an_idle_turn_but_stays_in_the_log() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            assert_eq!(app.log.unseen(), 0);
            app.set_error(ErrorOwner::Interface, "could not write scene".into());
            assert_eq!(app.log.unseen(), 1);
            let error = app
                .errors
                .get(ErrorOwner::Interface)
                .expect("interface error")
                .to_owned();
            app.errors.entries[ErrorOwner::Interface.slot()]
                .as_mut()
                .unwrap()
                .expires_at = Some(Instant::now());
            app.dirty_frame = false;
            app.pump_turn();
            assert!(app.errors.get(ErrorOwner::Interface).is_none());
            assert_eq!(app.log.unseen(), 0, "the header alert expires too");
            assert!(
                app.dirty_frame,
                "expiry redraws the footer even without a keypress"
            );
            assert!(app.log.lines().iter().any(|line| line.text == error));
        }

        #[test]
        fn transient_alerts_clear_only_their_owner_and_can_be_raised_again() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.set_error(ErrorOwner::Save, "save failed".into());
            app.log.push(LogLevel::Warn, "device", "device warning");
            app.set_error(ErrorOwner::Editor, "clipboard failed".into());
            app.set_error(ErrorOwner::Interface, "last scene".into());
            app.set_error(ErrorOwner::Interface, "last scene again".into());
            assert_eq!(app.log.unseen(), 5);

            app.errors.entries[ErrorOwner::Interface.slot()]
                .as_mut()
                .unwrap()
                .expires_at = Some(Instant::now());
            app.pump_turn();
            assert_eq!(app.log.unseen(), 3, "both expired interface alerts retire");
            assert_eq!(app.errors.visible(), Some("clipboard failed"));

            app.clear_error(ErrorOwner::Editor);
            assert_eq!(app.log.unseen(), 2, "unrelated warnings stay visible");
            assert_eq!(app.errors.visible(), Some("save failed"));
            app.set_error(ErrorOwner::Interface, "last scene again".into());
            assert_eq!(app.log.unseen(), 3, "a repeated error alerts again");
            app.clear_error(ErrorOwner::Interface);
            assert_eq!(app.log.unseen(), 2);
            assert!(app.log.lines().iter().any(|line| line.text == "last scene"));
            assert!(
                app.log.lines().iter().all(|line| !line.resolved),
                "a passing message that timed out or gave way is not resolved"
            );
        }

        #[test]
        fn stopping_hides_score_marks_before_the_snapshot_catches_up() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.snapshot = Some(crate::engine::StudioSnapshot {
                playing: true,
                stopping: false,
                ..Default::default()
            });
            assert!(app.sounding_marks_visible(), "playing events are marked");

            app.handle_editor_effect(EditorEffect::Stop).expect("stop");
            assert!(
                app.snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.playing && !snapshot.stopping),
                "the deliberately stale snapshot still says playing"
            );
            assert!(
                !app.sounding_marks_visible(),
                "the local stop request hides marks while tails ring"
            );
            assert!(
                !app.visual.running(),
                "the visual canvas freezes while tails ring"
            );

            app.stop_requested = false;
            app.snapshot = Some(crate::engine::StudioSnapshot {
                playing: true,
                stopping: true,
                ..Default::default()
            });
            assert!(
                !app.sounding_marks_visible(),
                "the acknowledged draining state also hides marks"
            );
            app.snapshot = Some(crate::engine::StudioSnapshot::default());
            assert!(
                !app.sounding_marks_visible(),
                "a fully stopped score has no marks"
            );
        }

        /// Every owner has a slot. The footer keeps one message per owner in a
        /// fixed array, so an owner whose index falls outside it takes the whole
        /// studio down the first time anything reports through it. A failed
        /// widget install uses this path during every evaluation that moves an
        /// inline visual.
        #[test]
        fn every_error_owner_can_be_set_read_and_cleared() {
            let mut errors = AppErrors::default();
            for owner in ErrorOwner::ALL {
                errors.set(owner, format!("{owner:?} spoke"));
                assert_eq!(
                    errors.get(owner).unwrap_or_default(),
                    format!("{owner:?} spoke"),
                    "{owner:?} has no slot of its own"
                );
                assert!(errors.clear(owner), "{owner:?} did not clear");
                assert!(errors.get(owner).is_none());
            }
            assert_eq!(
                ErrorOwner::ALL.len(),
                ERROR_OWNER_COUNT,
                "the slots and the owners disagree"
            );
            for (index, owner) in ErrorOwner::ALL.into_iter().enumerate() {
                assert_eq!(owner.slot(), index, "{owner:?} is not at its own index");
            }
        }

        #[test]
        fn pressing_update_on_the_score_already_sounding_installs_nothing() {
            // Update is held down as often as it is pressed, and each repeat used
            // to tear down a generation and build a byte-identical one. The
            // cutovers stack faster than the music absorbs them, and it is
            // audible.
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let source = "$: s(\"bd*4\")";

            let current = app.scenes.current().id;
            app.installed_revision = Some(source_revision(source));
            app.audible_scene = Some(current);
            app.snapshot = Some(sounding(true, false));
            assert!(
                app.install_would_be_redundant(current, source),
                "the sounding score is already this one"
            );

            // A single edited character is a different score.
            assert!(!app.install_would_be_redundant(current, "$: s(\"bd*8\")"));
        }

        /// A duplicate is another scene with the same text: launching it is a
        /// launch, not a repeat press, and the playing mark moves over to it.
        #[test]
        fn launching_a_duplicate_of_the_sounding_scene_installs_it() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let source = "$: s(\"sine*4\")";
            let first = app.scenes.current().id;
            app.scenes.current_mut().editor = Editor::new(source).expect("editor");
            app.installed_revision = Some(source_revision(source));
            app.audible_scene = Some(first);
            app.snapshot = Some(sounding(true, false));
            assert!(app.install_would_be_redundant(first, source));

            app.new_scene(true);
            let copy = app.scenes.current().id;
            assert_ne!(copy, first);
            assert_eq!(app.scenes.current().editor.source(), source, "a copy");
            assert!(
                !app.install_would_be_redundant(copy, source),
                "the same text from another scene is a launch"
            );
            // Sent, the copy is the one the engine will answer for.
            press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
            assert!(
                app.pending_evaluation.is_some() || !app.inflight.is_empty(),
                "the copy was evaluated rather than refused as unchanged"
            );
            assert!(!app.status.starts_with("unchanged"), "{}", app.status);
        }

        /// ^⇧S rewinds once; the scene's own ⟲ rewinds every time.
        ///
        /// The flag is the scene's rather than the gesture's, so a scene
        /// without one is an ordinary update however it is played, and a
        /// scene with one rewinds however it is played. A live edit that
        /// jumped back to the downbeat on every save would be unusable, which
        /// is why the ordinary kind is the default and stays it.
        #[test]
        fn a_rewinding_launch_starts_the_score_from_its_own_beginning() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let playing = app.scenes.current().id;
            app.scenes.current_mut().editor = Editor::new("$: s(\"sine*4\")").expect("editor");
            app.audible_scene = Some(playing);
            app.snapshot = Some(sounding(true, false));

            // The queue is two-deep and the engine may already have taken it,
            // so the newest request is looked for in both places.
            let queued = |app: &mut App| {
                let newest = app
                    .inflight
                    .keys()
                    .copied()
                    .max()
                    .and_then(|id| app.inflight.remove(&id));
                match (app.pending_evaluation.take(), newest) {
                    (Some(pending), _) => pending.rewind,
                    (None, Some(sent)) => sent.rewind,
                    (None, None) => panic!("an evaluation was queued"),
                }
            };

            // An ordinary update of the score already sounding joins the cycle
            // it is already in.
            press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
            assert!(!queued(&mut app), "an edit stays in time");

            // ^⇧S is that update with a rewind, whatever the setting says.
            press(
                &mut app,
                KeyCode::Char('s'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert!(queued(&mut app), "^⇧S plays from the top");
            assert!(
                !app.next_rewind,
                "and is spent on the score it was asked for"
            );

            // The menu row is the same gesture.
            app.dispatch_menu_action(crate::menu::MenuAction::RewindUpdate)
                .expect("update from the top");
            assert!(queued(&mut app), "Transport ▸ Update from the top");

            // A scene flagged to rewind does it on every ordinary update,
            // with no second key and no setting: the flag is the scene's.
            app.new_scene(false);
            let moved_to = app.scenes.current().id;
            assert_ne!(moved_to, playing);
            app.scenes.current_mut().editor = Editor::new("$: s(\"sawtooth*8\")").expect("editor");
            press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
            assert!(
                !queued(&mut app),
                "an unflagged scene joins the cycle already running"
            );
            press(
                &mut app,
                KeyCode::Char('u'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert!(app.scenes.current_rewinds(), "^⇧U flags the scene");
            press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
            assert!(queued(&mut app), "and now ^S alone rewinds it");
            // It is the scene's and not the studio's: the one that was
            // playing before still joins the running cycle.
            let back = app
                .scenes
                .scenes()
                .iter()
                .position(|scene| scene.id == playing)
                .expect("the scene that was playing");
            app.scenes.select(back);
            press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
            assert!(!queued(&mut app), "the other scene is unchanged");
        }

        /// A pad launching a flagged scene rewinds it.
        ///
        /// This is the whole reason the flag lives on the scene: a performer
        /// has one pad per scene, and mapping a second pad to "the rewinding
        /// kind of play" would double the mapping for every scene that wants
        /// it. The pad path selects the scene and evaluates, the same as ^S,
        /// and reads the same flag.
        #[test]
        fn a_pad_launching_a_flagged_scene_rewinds_it() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let playing = app.scenes.current().id;
            app.scenes.current_mut().editor = Editor::new("$: s(\"sine*4\")").expect("editor");
            app.audible_scene = Some(playing);
            app.snapshot = Some(sounding(true, false));

            // A second scene, with a pad on it.
            app.new_scene(false);
            app.scenes.current_mut().editor = Editor::new("$: s(\"sawtooth*8\")").expect("editor");
            let pad = crate::pads::Pad {
                note: 36,
                channel: 10,
            };
            assert!(app.scenes.learn_pad(pad));
            let flagged = app.scenes.current_index();

            let rewound = |app: &mut App| {
                let newest = app
                    .inflight
                    .keys()
                    .copied()
                    .max()
                    .and_then(|id| app.inflight.remove(&id));
                match (app.pending_evaluation.take(), newest) {
                    (Some(pending), _) => pending.rewind,
                    (None, Some(sent)) => sent.rewind,
                    (None, None) => panic!("an evaluation was queued"),
                }
            };

            // Unflagged, the pad plays it into the cycle already running.
            app.launch_scene(flagged).expect("the pad launches it");
            assert!(!rewound(&mut app), "an unflagged scene joins the cycle");

            // Flagged, the same pad plays it from its own beginning - with no
            // second mapping and nothing else pressed.
            assert_eq!(app.scenes.toggle_rewind(), Some(true));
            app.launch_scene(flagged)
                .expect("the pad launches it again");
            assert!(rewound(&mut app), "the scene's ⟲ is read by the pad path");
        }

        /// A rewind plays the same text again rather than refusing it.
        ///
        /// An ordinary update of the sounding score is refused as unchanged. A
        /// rewind is that restart, so unchanged text is not a reason to drop
        /// it.
        #[test]
        fn a_rewind_replays_text_an_ordinary_update_would_refuse() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let source = "$: s(\"sine*4\")";
            let playing = app.scenes.current().id;
            app.scenes.current_mut().editor = Editor::new(source).expect("editor");
            app.installed_revision = Some(source_revision(source));
            app.audible_scene = Some(playing);
            app.snapshot = Some(sounding(true, false));
            assert!(app.install_would_be_redundant(playing, source));

            // Unchanged text, ordinary update: refused, and nothing queued.
            press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
            assert!(app.status.starts_with("unchanged"), "{}", app.status);
            assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());

            let rewound = |app: &mut App| {
                let newest = app
                    .inflight
                    .keys()
                    .copied()
                    .max()
                    .and_then(|id| app.inflight.remove(&id));
                match (app.pending_evaluation.take(), newest) {
                    (Some(pending), _) => Some(pending.rewind),
                    (None, Some(sent)) => Some(sent.rewind),
                    (None, None) => None,
                }
            };

            // ^⇧S on the same text plays it again, from the top.
            press(
                &mut app,
                KeyCode::Char('s'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert_eq!(
                rewound(&mut app),
                Some(true),
                "a rewind update was refused as unchanged: {}",
                app.status
            );

            // And so does a scene that wears ⟲, on an ordinary ^S.
            assert_eq!(app.scenes.toggle_rewind(), Some(true));
            press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
            assert_eq!(
                rewound(&mut app),
                Some(true),
                "a flagged scene was refused as unchanged: {}",
                app.status
            );
        }

        #[test]
        fn update_still_starts_a_stopped_studio_on_unchanged_text() {
            // Stopped, update is how you start; the text being unchanged is
            // beside the point. Mid-stop, the next update is how you cut the
            // tail short - neither may be swallowed.
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let source = "$: s(\"bd*4\")";
            let current = app.scenes.current().id;
            app.installed_revision = Some(source_revision(source));
            app.audible_scene = Some(current);

            app.snapshot = None;
            assert!(!app.install_would_be_redundant(current, source), "stopped");

            app.snapshot = Some(sounding(false, false));
            assert!(
                !app.install_would_be_redundant(current, source),
                "not playing"
            );

            app.snapshot = Some(sounding(true, true));
            assert!(!app.install_would_be_redundant(current, source), "stopping");
        }

        #[test]
        fn an_armed_launch_is_never_redundant() {
            // Queueing an evaluation is what consumes the arming; returning early
            // would leave it in place and hand a later ordinary update a
            // quantised launch it never asked for.
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let source = "$: s(\"bd*4\")";
            let current = app.scenes.current().id;
            app.installed_revision = Some(source_revision(source));
            app.audible_scene = Some(current);
            app.snapshot = Some(sounding(true, false));
            assert!(
                app.install_would_be_redundant(current, source),
                "the baseline is redundant"
            );

            app.next_launch = Launch::Quantised { unit_cycles: 4.0 };
            assert!(
                !app.install_would_be_redundant(current, source),
                "a pending launch"
            );

            // An armed scene is no exemption on its own: the text that is armed
            // is answered before this predicate runs, and any other text is a
            // new launch.
            app.next_launch = Launch::Now;
            app.armed_scene = Some(current);
            assert!(
                app.install_would_be_redundant(current, source),
                "the sounding text"
            );
        }

        /// The worker answers a superseded launch with a cancel for the old
        /// request after the new one is armed; that answer must leave the new
        /// arming - and its countdown - alone. Only the armed request's own
        /// outcome ends it.
        #[test]
        fn a_superseded_outcome_leaves_the_new_arming_alone() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            app.snapshot = Some(sounding(true, false));
            app.armed_scene = Some(scene);
            app.armed_request = Some(2);
            app.armed_revision = Some("armed".into());
            app.landing_name = Some("intro".into());
            let cancelled = |request_id| EvaluationOutcome {
                request_id,
                editor_revision: 0,
                result: Err(crate::EngineFailure {
                    kind: "cancelled".into(),
                    message: "launch was superseded".into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            };
            app.handle_evaluation(cancelled(1));
            assert_eq!(app.armed_scene, Some(scene), "the old request's answer");
            assert_eq!(app.armed_request, Some(2));
            app.handle_evaluation(cancelled(2));
            assert_eq!(app.armed_scene, None, "its own answer ends it");
            assert_eq!(app.armed_revision, None);
        }

        /// Stop forgets the arming, drops the request that carried it, and
        /// does not leak a refused or stopped launch into the next update.
        #[test]
        fn stop_forgets_the_arming_and_the_launch_it_carried() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            app.snapshot = Some(sounding(true, false));
            app.armed_scene = Some(scene);
            app.armed_request = Some(7);
            app.armed_revision = Some("armed".into());
            app.next_launch = Launch::Quantised { unit_cycles: 1.0 };
            app.handle_editor_effect(EditorEffect::Stop).expect("stop");
            assert_eq!(app.armed_scene, None);
            assert_eq!(app.armed_request, None);
            assert!(matches!(app.next_launch, Launch::Now), "no launch leaks");

            app.armed_scene = Some(scene);
            app.armed_request = Some(8);
            app.inflight.insert(
                8,
                PendingEvaluation {
                    record: true,
                    request_id: 8,
                    scene,
                    editor_revision: Revision(0),
                    source: Arc::from("$: s(\"bd\")"),
                    launch: Launch::Quantised { unit_cycles: 1.0 },
                    rewind: false,
                },
            );
            app.inflight.insert(
                9,
                PendingEvaluation {
                    record: true,
                    request_id: 9,
                    scene,
                    editor_revision: Revision(0),
                    source: Arc::from("$: s(\"hh\")"),
                    launch: Launch::Now,
                    rewind: false,
                },
            );
            app.handle_control(StudioControlEvent::Stopped(Box::new(
                crate::engine::StudioStop::idle(),
            )));
            assert_eq!(app.armed_scene, None);
            assert!(
                !app.inflight.contains_key(&8),
                "the quantised request is dropped"
            );
            assert!(
                app.inflight.contains_key(&9),
                "a plain update still wants its outcome"
            );
            assert!(app.is_evaluating(), "and is still being waited for");
            app.handle_evaluation(EvaluationOutcome {
                request_id: 9,
                editor_revision: 0,
                result: Err(crate::EngineFailure {
                    kind: "cancelled".into(),
                    message: "evaluation was superseded by Stop".into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            });
            assert!(!app.is_evaluating(), "nothing left to wait for");
        }

        /// The text that is already armed is not re-armed by pressing again:
        /// inside the head-room a re-arm slips the launch a whole line later.
        #[test]
        fn pressing_update_on_the_armed_text_does_not_re_arm() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let source = "$: s(\"sine*4\")";
            app.snapshot = Some(sounding(true, false));
            app.armed_scene = Some(app.scenes.current().id);
            app.armed_request = Some(3);
            app.armed_revision = Some(source_revision(source));
            let before = app.next_request_id;
            app.handle_editor_effect(EditorEffect::Evaluate {
                revision: app.editor().revision(),
                source: source.into(),
            })
            .expect("evaluate");
            assert_eq!(app.next_request_id, before, "nothing was queued");
            assert!(app.status.contains("already armed"), "{}", app.status);
            assert_eq!(app.armed_request, Some(3), "still the same arming");
        }

        #[test]
        fn stopping_forgets_what_was_sounding_before_the_snapshot_catches_up() {
            // The worker stops on an atomic immediately; the snapshot saying so
            // arrives up to 100ms later. Until then the predicate would still see
            // playing=true, and a Stop followed at once by Update would be read as
            // redundant instead of restarting.
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let source = "$: s(\"bd*4\")";
            let current = app.scenes.current().id;
            app.installed_revision = Some(source_revision(source));
            app.audible_scene = Some(current);
            app.snapshot = Some(sounding(true, false));

            app.handle_editor_effect(EditorEffect::Stop).expect("stop");
            assert!(
                app.installed_revision.is_none(),
                "a stop must forget what was sounding at once"
            );
            // The snapshot still lags, and the update is no longer redundant.
            assert!(!app.install_would_be_redundant(current, source));
        }

        #[test]
        fn nothing_is_redundant_before_the_first_install() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            app.snapshot = Some(sounding(true, false));
            let current = app.scenes.current().id;
            assert!(!app.install_would_be_redundant(current, "$: s(\"bd*4\")"));
        }

        /// A stop pressed while the playback is already stopped is not a
        /// stop: no engine word goes out, the transport is not asked to
        /// stop again, and the status keeps whatever it was saying. The
        /// word would only come back as a second settle - a flicker on a
        /// picture at rest.
        #[test]
        fn a_stop_for_a_transport_already_stopped_says_nothing() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            app.snapshot = Some(sounding(false, false));
            app.status.clear();

            app.handle_editor_effect(EditorEffect::Stop).expect("stop");

            assert!(!app.stop_requested, "no stop was asked of the engine");
            assert!(app.status.is_empty(), "the stop said: {}", app.status);
        }

        /// A stop pressed before the first snapshot of a play still stops
        /// it: the update is on its way, and the engine must cancel it.
        #[test]
        fn a_stop_while_a_play_is_on_its_way_asks_the_engine() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            app.snapshot = Some(sounding(false, false));
            app.handle_editor_effect(EditorEffect::Evaluate {
                revision: app.editor().revision(),
                source: "$: note(60).s(\"sine\")".into(),
            })
            .expect("evaluate");
            assert!(!app.inflight.is_empty(), "the play went out");

            app.handle_editor_effect(EditorEffect::Stop).expect("stop");

            assert!(app.stop_requested, "the stop was asked of the engine");
        }

        /// The second stop word of the engine - a stray Stopped, or the
        /// snapshot after a settle - settles nothing: the advisories stay,
        /// the visual update the held frame is showing is not discarded,
        /// and no frame is redrawn for it.
        #[test]
        fn a_second_settle_for_a_settled_transport_redraws_no_frame() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            app.snapshot = Some(sounding(true, false));
            app.handle_editor_effect(EditorEffect::Stop).expect("stop");
            app.handle_control(StudioControlEvent::Stopped(Box::new(
                crate::engine::StudioStop::idle(),
            )));

            assert!(!app.stop_requested, "the settle cleared the request");
            assert!(!app.is_playing(), "the settle ended the playback");
            assert!(!app.is_stopping(), "the settle ended the stopping");

            app.dirty_frame = false;
            app.settle_stopped_transport();

            assert!(
                !app.dirty_frame,
                "the second settle redrew a picture at rest"
            );
        }
    }
}
