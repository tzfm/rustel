//! Quitting and play state: the two-press Ctrl+Q quit (the chord itself,
//! claimed ahead of every other key, and the press that calls an armed quit
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

const TRANSIENT_ERROR_DURATION: Duration = Duration::from_secs(5);

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
                .then_some(now + TRANSIENT_ERROR_DURATION),
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
    pub(super) fn settle_stopped_transport(&mut self) {
        self.audio_advisory.reset();
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

    /// Ctrl+Q quits from anywhere - except while a keybind learn holds the
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
