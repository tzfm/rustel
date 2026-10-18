//! Sounds a start or an edit needs while they load: what a start holds its
//! cycle zero for and what a held edit waits on in [`LoadMode::Wait`], the
//! header's loading cue, the one line a load's late sounds are said in, and
//! the log alerts those lines and failed imports raise and resolve.
//!
//! Plugins are part of the same wait, and the session decides it. A start
//! waits for the plugin copies of its first window: see
//! [`rustel_runtime::Session::plugins_pending`]. An edit and a launch wait
//! while the session holds their text for its plugin copies: see
//! [`rustel_runtime::Session::reload_live`].

use std::collections::{BTreeMap, BTreeSet};
#[cfg(feature = "vst")]
use std::time::Instant;

use rustel_runtime::samples::{SampleFailure, SampleLibrary, SoundReadiness, SourceState};
use rustel_runtime::{RuntimeError, SAMPLE_LOADING_DIAGNOSTIC};

use super::super::settings::LoadMode;
use super::{
    STARTUP_SAMPLE_WARM_BUDGET, StudioDiagnostic, StudioEngine, StudioInstall, StudioUpdate,
    StudioUpdateSendResult, TakeoverCut, next_boundary, queue_diagnostic, source_revision,
};

/// How far a start's first window reaches, in cycles.
const FIRST_WINDOW_CYCLES: f64 = 2.0;

/// One thing a start or an edit needs before it sounds whole.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Followed {
    /// A sound as a text names it. A bank keyed by note stands for every
    /// one of its keys once the library knows the name.
    Named { name: String, n: f64 },
    /// A sound as an onset plays it.
    Played { name: String, n: f64, midi: f64 },
    /// A `samples("…")` map the score imports.
    Import(String),
}

impl Followed {
    /// Visit each sound file this stands for, as the name, `n` and note
    /// that pick it; a map stands for none.
    fn each_sound(&self, library: &SampleLibrary, mut each: impl FnMut(&str, f64, f64)) {
        match self {
            Self::Named { name, n } => match library.note_keys(name) {
                Some(keys) => {
                    for midi in keys {
                        each(name, *n, midi);
                    }
                }
                None => each(name, *n, 36.0),
            },
            Self::Played { name, n, midi } => each(name, *n, *midi),
            Self::Import(_) => {}
        }
    }

    /// Visit each file this stands for, with whether it is still loading.
    /// Asked as bets: nothing here moves a load ahead of what plays. A map
    /// loads only while the loader has manifests in hand; one reading
    /// loading with none has settled, and the names it would bring read
    /// unknown.
    fn each_file(&self, library: &SampleLibrary, mut each: impl FnMut(&str, bool)) {
        if let Self::Import(spec) = self {
            let loading = library.manifests_pending() > 0
                && library.samples_source_state(spec) == Some(SourceState::Loading);
            return each(spec, loading);
        }
        self.each_sound(library, |name, n, midi| {
            each(
                name,
                library.readiness_at(name, n, midi) == SoundReadiness::Loading,
            );
        });
    }

    /// Ask at play priority for each of its files still loading, so it
    /// goes ahead of every bet. A file that has settled is not asked
    /// again, so a failure is not retried.
    fn ask_now(&self, library: &SampleLibrary) {
        self.each_sound(library, |name, n, midi| {
            if library.readiness_at(name, n, midi) == SoundReadiness::Loading {
                let _ = rustel_voice::SampleLookup::resolve(library, name, n, midi);
            }
        });
    }
}

/// An edit held until the sounds its text names have loaded, and until the
/// session has the plugin copies of its notes.
#[derive(Debug)]
pub(super) struct HeldEdit {
    source: String,
    mini: bool,
    rewind: bool,
    preview: bool,
    followed: Vec<Followed>,
}

/// What the engine follows while sounds load.
#[derive(Debug, Default)]
pub(super) struct LoadState {
    /// What the installed score needs: its first window after a start,
    /// what its text names after an edit. Let go once none is loading.
    installed: Vec<Followed>,
    /// A start holds its cycle zero until `installed` has loaded and
    /// `start_plugins` is 0.
    start_held: bool,
    /// The plugin copies of the first window not ready, at the last look
    /// of a held start.
    start_plugins: usize,
    /// The last time a copy of the first window became ready, or the start
    /// of the wait: see [`StudioEngine::look_at_start_copies`].
    #[cfg(feature = "vst")]
    start_progress: Option<Instant>,
    pub(super) held_edit: Option<HeldEdit>,
    /// Sounds skipped as still loading since the last late-sounds line.
    late: BTreeSet<String>,
    /// Late-sounds lines said, by alert key, with what each waits on.
    late_alerts: Vec<(String, Vec<Followed>)>,
    late_said: u64,
    /// Failed imports said, by alert key, with the maps each waits on,
    /// until retries bring them all in.
    import_alerts: BTreeMap<String, Vec<String>>,
}

/// Loading as the header shows it: how much of what the playing or the
/// waiting score needs has come in.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LoadingCue {
    /// Files and maps loaded or failed.
    pub settled: usize,
    pub total: usize,
    /// The bank, file or map still loading first, when there is one.
    pub loading: Option<String>,
    /// How many of the loads not settled are plugins.
    pub plugins: usize,
    /// A start or an edit is held until this load is over.
    pub waiting: bool,
}

/// The log alert a failed import raises: `spec` is the import, or the
/// imports one failure left failed, a line each.
pub(crate) fn import_alert(spec: &str) -> String {
    format!("samples:{spec}")
}

/// The sound a loading refusal names - `bd:3` of
/// `sample "bd:3" is still loading` - as the voice spells it.
fn refused_sound(message: &str) -> Option<&str> {
    let (_, rest) = message.split_once('"')?;
    let (sound, _) = rest.split_once('"')?;
    (!sound.is_empty()).then_some(sound)
}

/// The maps a text's live code imports.
fn imports_in(source: &str) -> Vec<Followed> {
    let mut followed = Vec::new();
    for spec in rustel_runtime::sounds::samples_specs(source)
        .into_iter()
        .flatten()
    {
        push_new(&mut followed, Followed::Import(spec));
    }
    followed
}

fn push_new(followed: &mut Vec<Followed>, item: Followed) {
    if !followed.contains(&item) {
        followed.push(item);
    }
}

/// How the log names what a launch or an edit waits for: its sounds, its
/// plugin copies, or both.
fn waits_for_words(sounds: bool, plugins: bool) -> &'static str {
    match (sounds, plugins) {
        (true, true) => "sounds and plugins",
        (false, true) => "plugins",
        _ => "sounds",
    }
}

impl StudioEngine {
    /// Whether starts and edits wait for their sounds, as the master bus
    /// says this turn. Leaving wait lets a held start and a held edit land
    /// on the next turn.
    pub(super) fn waits_for_sounds(&self) -> bool {
        self.master.load_mode() == LoadMode::Wait
    }

    /// What a text needs before it sounds whole, read from the text alone:
    /// the maps its live code imports and the sounds its live code names.
    pub(super) fn followed_in_text(&self, source: &str, mini: bool) -> Vec<Followed> {
        let mut followed = imports_in(source);
        if mini {
            return followed;
        }
        for sound in rustel_runtime::lint::live_sound_names(source) {
            let spellings = if sound.banks.is_empty() {
                vec![sound.name]
            } else {
                sound
                    .banks
                    .iter()
                    .map(|bank| format!("{bank}_{}", sound.name))
                    .collect()
            };
            for name in spellings {
                if !rustel_voice::is_native_synth_sound(&name) {
                    push_new(&mut followed, Followed::Named { name, n: sound.n });
                }
            }
        }
        followed
    }

    /// [`Self::followed_in_text`] less what the playing score's text also
    /// needs: that sounds as well now as it would after the edit, so only
    /// a new sound is worth holding the edit for.
    pub(super) fn new_in_text(&self, source: &str, mini: bool) -> Vec<Followed> {
        let playing = self
            .session
            .active_source()
            .map(|playing| self.followed_in_text(playing, false))
            .unwrap_or_default();
        self.followed_in_text(source, mini)
            .into_iter()
            .filter(|item| !playing.contains(item))
            .collect()
    }

    /// One look of a start at the plugin copies of its first window: see
    /// [`rustel_runtime::Session::plugins_pending`]. The start waits while a
    /// copy became ready in the last [`rustel_runtime::PLUGIN_WAIT`]. After
    /// it, the start goes on, and a note with no copy plays with no plugin.
    #[cfg(feature = "vst")]
    fn look_at_start_copies(&mut self) -> Result<(), RuntimeError> {
        let now = self.device_time();
        let pending = self.session.with_panic_recovery(now, |session| {
            Ok(session.plugins_pending(0.0, FIRST_WINDOW_CYCLES, STARTUP_SAMPLE_WARM_BUDGET))
        })?;
        let progress = match self.load.start_progress {
            Some(progress) if pending >= self.load.start_plugins => progress,
            _ => Instant::now(),
        };
        self.load.start_progress = Some(progress);
        let waits = progress.elapsed() < rustel_runtime::PLUGIN_WAIT;
        self.load.start_plugins = if waits { pending } else { 0 };
        Ok(())
    }

    /// What the score just installed by a start plays in its first window,
    /// as its onsets resolve, and the maps its text imports.
    fn followed_in_first_window(&mut self, source: &str) -> Result<Vec<Followed>, RuntimeError> {
        let mut followed = imports_in(source);
        let now = self.device_time();
        let sounds = self.session.with_panic_recovery(now, |session| {
            Ok(session.window_sounds(0.0, FIRST_WINDOW_CYCLES, STARTUP_SAMPLE_WARM_BUDGET))
        })?;
        for sound in sounds {
            let name = sound.banked.unwrap_or(sound.s);
            if !rustel_voice::is_native_synth_sound(&name) {
                push_new(
                    &mut followed,
                    Followed::Played {
                        name,
                        n: sound.n,
                        midi: sound.midi,
                    },
                );
            }
        }
        Ok(followed)
    }

    /// Whether any of `followed` is still loading.
    pub(super) fn loading_among(&self, followed: &[Followed]) -> bool {
        let Some(library) = self.session.sample_library() else {
            return false;
        };
        let mut loading = false;
        for item in followed {
            item.each_file(library, |_, still| loading |= still);
            if loading {
                return true;
            }
        }
        false
    }

    /// Ask for the maps a text imports that the library has not been
    /// asked for, so the names they bring read as loading rather than
    /// unknown.
    pub(super) fn look_up_imports_of(&mut self, source: &str) {
        for spec in rustel_runtime::sounds::samples_specs(source)
            .into_iter()
            .flatten()
        {
            let asked = self
                .session
                .sample_library()
                .is_some_and(|library| library.samples_source_state(&spec).is_some());
            if !asked && let Err(error) = self.session.look_up_samples_source(&spec) {
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message("sample-prefetch", error.to_string()),
                );
            }
        }
    }

    /// Hold an edit played now until the sounds its text names have loaded,
    /// in [`LoadMode::Wait`]. True when it is held: the last score keeps
    /// playing, and [`Self::take_launch_outcome`] answers the edit once it
    /// lands. A newer edit or a stop cancels it like a pending launch. The
    /// plugin copies of the edit build while its sounds load.
    pub fn hold_update(&mut self, source: &str, mini: bool, rewind: bool) -> bool {
        if !self.waits_for_sounds()
            || self.live.is_none()
            || self.is_stopping()
            || self.start_is_held()
        {
            return false;
        }
        self.look_up_imports_of(source);
        self.warm_incoming(source);
        let followed = self.new_in_text(source, mini);
        if !self.loading_among(&followed) {
            return false;
        }
        self.cancel_pending_launch();
        self.launch_outcome = None;
        #[cfg(feature = "vst")]
        let plugins = {
            let cycle = self.session.cycle_at_time(self.device_time());
            self.prepare_live(source, mini, if rewind { 0.0 } else { cycle });
            self.session.waits_for_plugins(source)
        };
        #[cfg(not(feature = "vst"))]
        let plugins = false;
        self.load.held_edit = Some(HeldEdit {
            source: source.to_owned(),
            mini,
            rewind,
            preview: std::mem::take(&mut self.next_install_preview),
            followed,
        });
        self.ask_for_waited();
        let waits_for = waits_for_words(true, plugins);
        self.log_launch(&format!("update waits for its {waits_for}"));
        true
    }

    /// Hold an edit the session holds for its plugin copies: see
    /// [`rustel_runtime::Session::reload_live`]. The last score keeps
    /// playing, [`Self::advance_held_edit`] lands the edit when its copies
    /// are ready, and [`Self::take_launch_outcome`] answers it then. The
    /// edit keeps its from-zero request and its preview mark.
    pub(super) fn hold_for_plugins(&mut self, source: &str, mini: bool) -> StudioInstall {
        self.launch_outcome = None;
        self.load.held_edit = Some(HeldEdit {
            source: source.to_owned(),
            mini,
            rewind: self.session.take_next_from_zero(),
            preview: std::mem::take(&mut self.next_install_preview),
            followed: Vec::new(),
        });
        self.log_launch("update waits for its plugins");
        StudioInstall {
            generation: self.session.generation(),
            source_revision: source_revision(source),
            pending_cutover: false,
            answered_repeat: false,
            held: true,
        }
    }

    /// In [`LoadMode::Wait`], ask for what a held start, a held edit or an
    /// armed launch waits on at play priority, as an evaluated update's
    /// window is asked, so a bet placed since does not go ahead of it.
    /// Async leaves what the cue follows to bets.
    pub(super) fn ask_for_waited(&self) {
        if !self.waits_for_sounds() {
            return;
        }
        let Some(library) = self.session.sample_library() else {
            return;
        };
        let start = self
            .start_is_held()
            .then_some(&self.load.installed)
            .into_iter()
            .flatten();
        let edit = self.load.held_edit.iter().flat_map(|held| &held.followed);
        let launch = self
            .pending_launch
            .iter()
            .flat_map(|pending| &pending.followed);
        for item in start.chain(edit).chain(launch) {
            item.ask_now(library);
        }
    }

    /// Land a held edit once its sounds have loaded and the session has its
    /// plugin copies, or at once when the mode no longer waits.
    pub(super) fn advance_held_edit(&mut self) {
        let waits = self.waits_for_sounds();
        let Some(held) = self.load.held_edit.as_ref() else {
            return;
        };
        let loading = waits && self.loading_among(&held.followed);
        #[cfg(feature = "vst")]
        let loading = loading || (waits && self.session.waits_for_plugins(&held.source));
        if loading || self.live.is_none() || self.is_stopping() {
            return;
        }
        let mut held = self.load.held_edit.take().expect("checked");
        // The edit replaces whatever a fired launch was landing, as an
        // edit played now does.
        self.abandon_landing(false, false);
        if held.rewind {
            self.session.start_next_from_zero();
        }
        self.next_install_preview = held.preview;
        match self.reload_live(&held.source, held.mini) {
            Ok(Some(install)) => self.launch_outcome = Some(Ok(install)),
            // The session holds the edit for its plugin copies again: the
            // edit keeps its from-zero request and its preview mark.
            Ok(None) => {
                self.session.clear_next_from_zero();
                held.preview = std::mem::take(&mut self.next_install_preview);
                self.load.held_edit = Some(held);
            }
            Err(error) => {
                self.session.clear_next_from_zero();
                // The edit does not go in play: the session ends its wait.
                #[cfg(feature = "vst")]
                self.session.cancel_plugin_hold();
                self.launch_outcome = Some(Err(error));
            }
        }
    }

    /// In [`LoadMode::Wait`], a launch whose sounds are still loading, or
    /// whose text the session holds for its plugin copies, lets the line
    /// within its head-room pass and aims at the next one.
    pub(super) fn launch_waits_for_sounds(&mut self, now: f64) -> bool {
        let Some(pending) = self.pending_launch.as_ref() else {
            return false;
        };
        if !self.waits_for_sounds() {
            return false;
        }
        let sounds = self.loading_among(&pending.followed);
        #[cfg(feature = "vst")]
        let plugins = self.session.waits_for_plugins(&pending.source);
        #[cfg(not(feature = "vst"))]
        let plugins = false;
        if !sounds && !plugins {
            return false;
        }
        self.launch_takes_next_line(now, waits_for_words(sounds, plugins));
        true
    }

    /// The pending launch lets the line within its head-room pass and aims
    /// at the next one. The first line it lets pass is logged, with what the
    /// launch waits for.
    pub(super) fn launch_takes_next_line(&mut self, now: f64, waits_for: &str) {
        let cps = self.session.cps().max(1e-6);
        let cycle_now = self.session.cycle_at_time(now);
        let headroom_cycles = self.launch_headroom() * cps;
        let Some(pending) = self.pending_launch.as_mut() else {
            return;
        };
        pending.boundary_cycle = next_boundary(cycle_now, pending.unit_cycles, headroom_cycles);
        pending.boundary_time = now + (pending.boundary_cycle - cycle_now) / cps;
        if !std::mem::replace(&mut pending.waited, true) {
            self.log_launch(&format!(
                "{waits_for} still loading - the launch takes the first line after them"
            ));
        }
    }

    /// The text of an edit held for its sounds or its plugin copies.
    pub(super) fn waiting_source(&self) -> Option<&str> {
        self.load
            .held_edit
            .as_ref()
            .map(|held| held.source.as_str())
    }

    /// A start whose cycle zero waits for its first window's sounds.
    pub(super) fn start_is_held(&self) -> bool {
        self.load.start_held
            && self.waits_for_sounds()
            && self
                .live
                .as_ref()
                .is_some_and(|live| live.initial_start_generation.is_some())
    }

    /// Follow what a start's first window plays, and hold its cycle zero
    /// for it in [`LoadMode::Wait`]. The plugin copies of the window hold
    /// the start too.
    pub(super) fn follow_start(&mut self, source: &str) -> Result<(), RuntimeError> {
        self.load.installed = self.followed_in_first_window(source)?;
        #[cfg(feature = "vst")]
        {
            self.load.start_plugins = 0;
            self.load.start_progress = None;
            if self.waits_for_sounds() {
                self.look_at_start_copies()?;
            }
        }
        self.load.start_held = self.waits_for_sounds()
            && (self.loading_among(&self.load.installed) || self.load.start_plugins > 0);
        Ok(())
    }

    /// Follow what an edit's text names once it is installed.
    pub(super) fn follow_edit(&mut self, source: &str, mini: bool) {
        self.load.installed = self.followed_in_text(source, mini);
    }

    /// One turn of a held start: the layout reaches the display so its
    /// sliders can move, and cycle zero stays just ahead. False once the
    /// first window's sounds have loaded and its plugin copies are ready,
    /// when the start goes on. A look that fails ends the wait for the
    /// copies.
    pub(super) fn hold_start(
        &mut self,
        emit: &mut impl FnMut(StudioUpdate) -> StudioUpdateSendResult,
    ) -> bool {
        if !self.start_is_held() {
            return false;
        }
        #[cfg(feature = "vst")]
        if self.load.start_plugins > 0 && self.look_at_start_copies().is_err() {
            self.load.start_plugins = 0;
        }
        if !self.loading_among(&self.load.installed) && self.load.start_plugins == 0 {
            self.load.start_held = false;
            return false;
        }
        let live = self.live.as_ref().expect("a held start is live");
        let anchor = live.device.clock_seconds()
            + live.device.schedule_lead_seconds()
            + self.config.start_preroll.as_secs_f64();
        self.session.finish_transport_start_at(anchor);
        self.ui
            .observe_layout(&mut self.session, &mut self.pending_diagnostics, emit);
        true
    }

    /// Replace a held start with `source`, from its own cycle zero and
    /// waiting for its own first window. A score that fails to evaluate
    /// leaves the held start as it was.
    pub(super) fn replace_held_start(
        &mut self,
        source: &str,
        mini: bool,
    ) -> Result<StudioInstall, RuntimeError> {
        let preview = std::mem::take(&mut self.next_install_preview);
        let transport = self.session.transport();
        let now = self
            .live
            .as_ref()
            .expect("a held start is live")
            .device
            .clock_seconds();
        let generation =
            self.session
                .reload_at_cancellable(source, mini, now, transport.stopped_flag())?;
        let live = self.live.as_mut().expect("a held start is live");
        let anchor = live.device.clock_seconds()
            + live.device.schedule_lead_seconds()
            + self.config.start_preroll.as_secs_f64();
        self.session.restart_transport_at(anchor);
        let _ = self.session.take_requery_takeover();
        live.device.set_generation(generation, 0, TakeoverCut::None);
        live.initial_start_generation = Some(generation);
        live.audition_owned = false;
        self.warm_source(source, STARTUP_SAMPLE_WARM_BUDGET)?;
        self.note_install_for_played(generation, preview, source);
        self.follow_start(source)?;
        Ok(StudioInstall {
            generation,
            source_revision: source_revision(source),
            pending_cutover: false,
            answered_repeat: false,
            held: false,
        })
    }

    /// The header's loading cue: `None` unless something the playing or
    /// the waiting score needs is still loading.
    pub(super) fn loading_cue(&self) -> Option<LoadingCue> {
        if self.live.is_none() || self.is_stopping() {
            return None;
        }
        let mut cue = LoadingCue::default();
        if let Some(library) = self.session.sample_library() {
            let mut followed: Vec<&Followed> = Vec::new();
            let waiting_on = self
                .load
                .held_edit
                .iter()
                .flat_map(|held| &held.followed)
                .chain(
                    self.pending_launch
                        .iter()
                        .flat_map(|pending| &pending.followed),
                );
            for item in self.load.installed.iter().chain(waiting_on) {
                if !followed.contains(&item) {
                    followed.push(item);
                }
            }
            for item in followed {
                item.each_file(library, |name, loading| {
                    cue.total += 1;
                    if loading {
                        cue.loading.get_or_insert_with(|| name.to_owned());
                    } else {
                        cue.settled += 1;
                    }
                });
            }
        }
        // A held start counts the plugin copies of its first window. The
        // count has no names: the session counts copies.
        if self.start_is_held() {
            cue.total += self.load.start_plugins;
            cue.plugins += self.load.start_plugins;
        }
        // A held edit or a launch counts the copies the session holds its
        // text for.
        #[cfg(feature = "vst")]
        {
            let held = self.load.held_edit.iter().map(|held| &held.source);
            let launch = self.pending_launch.iter().map(|pending| &pending.source);
            for source in held.chain(launch) {
                if let Some((pending, total)) = self.session.plugins_waited_for(source) {
                    cue.total += total;
                    cue.settled += total - pending;
                    cue.plugins += pending;
                }
            }
        }
        if cue.settled == cue.total {
            return None;
        }
        cue.waiting = self.start_is_held()
            || self.load.held_edit.is_some()
            || (self.waits_for_sounds()
                && self.pending_launch.as_ref().is_some_and(|pending| {
                    #[cfg(feature = "vst")]
                    if let Some((copies, _)) = self.session.plugins_waited_for(&pending.source)
                        && copies > 0
                    {
                        return true;
                    }
                    self.loading_among(&pending.followed)
                }));
        Some(cue)
    }

    /// Keep a still-loading refusal for the late-sounds line, rather than
    /// saying it per sound.
    pub(super) fn note_late(&mut self, message: &str) {
        if let Some(sound) = refused_sound(message) {
            self.load.late.insert(sound.to_owned());
        }
    }

    /// Say a loader failure; an import's under its alert, so retries that
    /// bring every map it left failed back in resolve it.
    pub(super) fn import_failure(&mut self, failure: SampleFailure) -> StudioDiagnostic {
        let diagnostic = StudioDiagnostic::message("sample-failed", failure.message);
        if failure.maps.is_empty() {
            return diagnostic;
        }
        let specs = failure
            .maps
            .into_iter()
            .map(|map| serde_json::from_str::<String>(&map).unwrap_or(map))
            .collect::<Vec<_>>();
        let alert = import_alert(&specs.join("\n"));
        self.load.import_alerts.insert(alert.clone(), specs);
        diagnostic.raising(alert)
    }

    /// Each turn's bookkeeping: ask for what waits, let go of what has
    /// loaded, say the late-sounds line once a load is over, and resolve
    /// the alerts whose sounds or maps have come in. The session looks
    /// again at the plugin copies of an armed launch, so the cue counts the
    /// copies of now.
    pub(super) fn settle_loads(&mut self) {
        self.ask_for_waited();
        #[cfg(feature = "vst")]
        if let Some(pending) = self.pending_launch.as_ref() {
            self.session.waits_for_plugins(&pending.source);
        }
        if !self.load.installed.is_empty() && !self.loading_among(&self.load.installed) {
            self.load.installed.clear();
            self.load.start_held &= self.load.start_plugins > 0;
        }
        let still_waiting = self.load.held_edit.is_some()
            || self
                .pending_launch
                .as_ref()
                .is_some_and(|pending| self.loading_among(&pending.followed));
        if !self.load.installed.is_empty() || still_waiting {
            self.resolve_alerts();
            return;
        }
        let late = self.late_sounds();
        if !self.loading_among(&late) {
            self.say_late(late);
        }
        self.resolve_alerts();
    }

    /// Stop ends every load: a held start and edit go with the transport,
    /// the cue with them, and what came late so far is said now. The
    /// session forgets the text it holds for its plugin copies.
    pub(super) fn end_loads(&mut self) {
        self.load.installed.clear();
        self.load.start_held = false;
        self.load.start_plugins = 0;
        #[cfg(feature = "vst")]
        self.session.cancel_plugin_hold();
        let late = self.late_sounds();
        self.say_late(late);
        self.resolve_alerts();
    }

    fn late_sounds(&self) -> Vec<Followed> {
        self.load
            .late
            .iter()
            .map(|sound| {
                let (name, n) = match sound.rsplit_once(':') {
                    Some((name, index)) => (name, index.parse::<f64>().unwrap_or(0.0)),
                    None => (sound.as_str(), 0.0),
                };
                Followed::Named {
                    name: name.to_owned(),
                    n,
                }
            })
            .collect()
    }

    fn say_late(&mut self, waits_on: Vec<Followed>) {
        if self.load.late.is_empty() {
            return;
        }
        let sounds = std::mem::take(&mut self.load.late)
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        self.load.late_said += 1;
        let alert = format!("samples:late:{}", self.load.late_said);
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::message(
                SAMPLE_LOADING_DIAGNOSTIC,
                format!("notes skipped while loading: {sounds}"),
            )
            .raising(alert.clone()),
        );
        self.load.late_alerts.push((alert, waits_on));
    }

    fn resolve_alerts(&mut self) {
        let alerts = std::mem::take(&mut self.load.late_alerts);
        for (alert, waits_on) in alerts {
            if self.loading_among(&waits_on) {
                self.load.late_alerts.push((alert, waits_on));
            } else {
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::resolve(alert),
                );
            }
        }
        let Some(library) = self.session.sample_library() else {
            return;
        };
        let back: Vec<String> = self
            .load
            .import_alerts
            .iter()
            .filter(|(_, specs)| {
                specs
                    .iter()
                    .all(|spec| library.samples_source_state(spec) == Some(SourceState::Ready))
            })
            .map(|(alert, _)| alert.clone())
            .collect();
        for alert in back {
            self.load.import_alerts.remove(&alert);
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::resolve(alert),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::*;
    use crate::engine::{Arc, Duration, EnginePressureMonitor, LiveDeviceReport};
    mod loading_pressure {
        use super::*;
        use rustel_audio::RealtimeLoadSnapshot;
        use rustel_runtime::{EnginePressureCause, EnginePressureLevel, ProducerLoadSnapshot};
        use std::time::Instant;

        /// Sample-loading refusals leave the header healthy when device and producer
        /// timing are healthy, including across repeated snippet installations.
        #[test]
        fn rapid_snippet_installs_awaiting_samples_are_not_engine_pressure() {
            let mut engine = silent_engine_for_output_selection();
            engine.session.set_sample_library_for_test(Arc::new(
                rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test("held"),
            ));
            engine
                .evaluate("setcps(0.5)\n$: s(\"held\").fast(2)", false)
                .expect("score whose sample is loading");

            let mut monitor = EnginePressureMonitor::default();
            let start = Instant::now();
            let healthy_device = LiveDeviceReport {
                stream_id: 1,
                realtime_load: RealtimeLoadSnapshot {
                    sample_rate_hz: 48_000,
                    last_callback_frames: 256,
                    last_callback_period_nanos: 5_333_333,
                    fast_load_basis_points: 1_000,
                    slow_load_basis_points: 1_000,
                    peak_load_basis_points: 1_000,
                    ..Default::default()
                },
                ..Default::default()
            };
            for spam in 0..12 {
                let snippet = format!(
                    "setcps(0.5)\n$: note(\"c{}\").s(\"held\").fast(4)",
                    spam % 8
                );
                engine.evaluate(&snippet, false).expect("preview install");
                for _ in 0..6 {
                    engine
                        .tick_at(Duration::from_millis(2), accepted)
                        .expect("tick");
                }
                let pressure = engine.snapshot().pressure.expect("pressure");
                assert_eq!(
                    pressure.producer_refusals, 0,
                    "a preview waiting on its samples is not a capacity refusal: {pressure:#?}"
                );
                assert_eq!(
                    pressure.producer.capacity_refusals(),
                    0,
                    "no capacity refusal may be hidden between snapshots: {pressure:#?}"
                );
                assert_eq!(
                    (
                        pressure.device.callback_errors,
                        pressure.device.ring_refusals
                    ),
                    (0, 0),
                    "loading must not hide a device refusal behind timing pressure: {pressure:#?}"
                );
                assert_ne!(
                    pressure.cause,
                    EnginePressureCause::Refusal,
                    "a preview waiting on its samples is not pressure: {pressure:#?}"
                );

                // Preserve the live refusal counters while controlling timing inputs.
                let healthy_producer = ProducerLoadSnapshot {
                    last_load_basis_points: 1_000,
                    fast_load_basis_points: 1_000,
                    slow_load_basis_points: 1_000,
                    peak_load_basis_points: 1_000,
                    consecutive_over_budget: 0,
                    ..pressure.producer
                };
                let healthy = monitor.sample_at(
                    healthy_device,
                    healthy_producer,
                    256,
                    start + Duration::from_millis(spam * 12),
                );
                assert_eq!(
                    healthy.cause,
                    EnginePressureCause::Healthy,
                    "loading alone must leave the header healthy: {healthy:#?}; live: {pressure:#?}"
                );
                assert_eq!(
                    healthy.level,
                    EnginePressureLevel::Normal,
                    "healthy timing and loading samples must leave the header normal: {healthy:#?}; live: {pressure:#?}"
                );
            }
            let pressure = engine.snapshot().pressure.expect("pressure");
            assert!(
                pressure.producer.loading_refusals > 0,
                "setup really did wait on the library: {pressure:#?}"
            );
        }
    }
    #[cfg(test)]
    mod load_mode_tests {
        //! Tests for the load mode: a start or an edit meeting sounds that are
        //! still loading, the loading cue that follows them, and the one line a
        //! load's late sounds are said in.

        use crate::engine::*;
        use crate::settings::LoadMode;
        use rustel_runtime::samples::{SampleFailure, SampleLibrary};

        /// A stopped studio in `mode` whose sounds come from `library`.
        fn studio(mode: LoadMode, library: &Arc<SampleLibrary>) -> StudioEngine {
            studio_with(mode, library, SessionConfig::default())
        }

        /// [`studio`] over its own session settings.
        fn studio_with(
            mode: LoadMode,
            library: &Arc<SampleLibrary>,
            session: SessionConfig,
        ) -> StudioEngine {
            let mut engine = StudioEngine::new(StudioConfig {
                output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
                session,
                ..StudioConfig::default()
            })
            .expect("studio");
            engine
                .session
                .set_sample_library_for_test(Arc::clone(library));
            engine.master.set_load_mode(mode);
            engine
        }

        /// Start `source` from stopped on the silent output, as a play does once
        /// the output is open.
        fn start(engine: &mut StudioEngine, source: &str) -> StudioInstall {
            engine.session.transport().start();
            let device =
                LiveScalarDevice::start_silent(48_000, engine.generation()).expect("silent output");
            engine
                .session
                .bind_audio_confirmations(device.confirmations())
                .expect("bind the silent output");
            engine
                .start_on(device, source, false, false)
                .expect("the start")
        }

        /// One producer turn, answering what it sent.
        fn turn(engine: &mut StudioEngine) -> Vec<StudioUpdate> {
            let mut sent = Vec::new();
            engine
                .tick(|update| {
                    sent.push(update);
                    Ok(())
                })
                .expect("a turn");
            sent
        }

        /// Turn for `span` of wall time, answering what the turns sent.
        fn turns_for(engine: &mut StudioEngine, span: Duration) -> Vec<StudioUpdate> {
            let until = Instant::now() + span;
            let mut sent = Vec::new();
            while Instant::now() < until {
                sent.extend(turn(engine));
                std::thread::sleep(Duration::from_millis(2));
            }
            sent
        }

        /// Turn until `done`, answering what the turns sent.
        fn turn_until(
            engine: &mut StudioEngine,
            what: &str,
            done: impl Fn(&mut StudioEngine) -> bool,
        ) -> Vec<StudioUpdate> {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut sent = Vec::new();
            while !done(engine) {
                assert!(Instant::now() < deadline, "{what} never happened");
                sent.extend(turn(engine));
                std::thread::sleep(Duration::from_millis(2));
            }
            sent
        }

        fn device_now(engine: &StudioEngine) -> f64 {
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .clock_seconds()
        }

        fn diagnostics(sent: &[StudioUpdate]) -> Vec<&StudioDiagnostic> {
            sent.iter()
                .filter_map(|update| match update {
                    StudioUpdate::Diagnostic(diagnostic) => Some(diagnostic),
                    _ => None,
                })
                .collect()
        }

        fn late_lines(sent: &[StudioUpdate]) -> Vec<&StudioDiagnostic> {
            diagnostics(sent)
                .into_iter()
                .filter(|diagnostic| diagnostic.kind == rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC)
                .collect()
        }

        /// A library whose sound `kick` stays loading until the test finishes it.
        fn loading_kick() -> Arc<SampleLibrary> {
            Arc::new(SampleLibrary::with_loading_sample_for_test("kick"))
        }

        /// Start from stopped in wait: nothing sounds while the kick loads, and
        /// cycle zero, the kick's own onset, falls after the kick came in. An
        /// onset that sounds late still carries cycle zero, so the test pins when
        /// cycle zero is, not only that the first onset is on it.
        #[test]
        fn a_wait_start_holds_cycle_zero_until_its_first_sound_has_loaded() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"kick\")");
            turns_for(&mut engine, Duration::from_millis(300));
            assert!(engine.start_is_held());
            assert!(
                engine.played_by_text.is_empty(),
                "nothing sounds while held"
            );
            let cue = engine.snapshot().loading.expect("the cue follows the kick");
            assert!(cue.waiting);
            assert_eq!((cue.settled, cue.total), (0, 1));
            assert_eq!(cue.loading.as_deref(), Some("kick"));

            let kick = library.finish_loading_sample_for_test();
            let arrived = device_now(&engine);
            turn_until(&mut engine, "the kick", |engine| {
                engine.played_by_text.contains(&kick)
            });
            assert!(
                engine.session.cycle_at_time(arrived) < 0.0,
                "cycle zero comes after the kick came in"
            );
            assert!(engine.snapshot().loading.is_none(), "the cue is gone");

            tests::hold_clock(&engine, true);
            assert!(!engine.stop(Duration::ZERO).expect("stop").acknowledged);
            let device = engine.piano_output.take().expect("parked output");
            engine.session.transport().start();
            let resumed = engine
                .start_on(device, "s(\"kick\")", false, true)
                .expect("restart");
            assert!(engine.live.as_ref().unwrap().device.stop_pending());
            assert!(matches!(
                engine.tick(|_| Ok(())).expect("waiting for the stop fade"),
                StudioTick::Running {
                    step: None,
                    accepted_audio: 0,
                    ..
                }
            ));
            assert_eq!(
                engine.live.as_ref().unwrap().initial_start_generation,
                Some(resumed.generation)
            );
            tests::hold_clock(&engine, false);
            turn_until(&mut engine, "the resumed generation's receipt", |engine| {
                engine.session.confirmed_audio_generation() == Some(resumed.generation)
            });
        }

        /// Async: the start plays at once, the loading kick is skipped, and
        /// its lateness is said once, in one line, when the load is over.
        #[test]
        fn an_async_start_plays_at_once_and_says_the_late_sound_once() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Async, &library);
            start(&mut engine, "s(\"kick\").fast(4)");
            assert!(!engine.start_is_held());
            let mut sent = turns_for(&mut engine, Duration::from_millis(700));
            let cue = engine.snapshot().loading.expect("the cue follows the kick");
            assert!(!cue.waiting, "async holds nothing");
            assert!(late_lines(&sent).is_empty(), "nothing is said per sound");

            let kick = library.finish_loading_sample_for_test();
            sent.extend(turn_until(&mut engine, "the kick", |engine| {
                engine.played_by_text.contains(&kick)
            }));
            sent.extend(turns_for(&mut engine, Duration::from_millis(50)));
            let late = late_lines(&sent);
            assert_eq!(late.len(), 1, "{late:?}");
            assert!(late[0].message.contains("kick:0"), "{}", late[0].message);
            let Some(DiagnosticAlert::Raise(key)) = &late[0].alert else {
                panic!("the late line carries its alert: {:?}", late[0]);
            };
            assert!(
                diagnostics(&sent)
                    .iter()
                    .any(|diagnostic| diagnostic.alert
                        == Some(DiagnosticAlert::Resolve(key.clone()))),
                "and resolves it, its sounds being in"
            );
        }

        /// A rewind whose first window waits whole for its sound skips nothing:
        /// the session holds it and it lands from its own cycle zero once the
        /// sound is in, so no late line names that sound.
        #[test]
        fn a_rewind_held_for_its_sound_says_nothing_was_skipped() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Async, &library);
            start(&mut engine, "s(\"sine\")");
            turns_for(&mut engine, Duration::from_millis(100));
            engine.start_next_from_zero();
            let rewind = engine.evaluate("s(\"kick\")", false).expect("the rewind");
            let mut sent = turns_for(&mut engine, Duration::from_millis(300));
            let audible =
                |engine: &StudioEngine| engine.live.as_ref().expect("playing").device.generation();
            assert_ne!(audible(&engine), rewind.generation, "the rewind is held");

            let kick = library.finish_loading_sample_for_test();
            sent.extend(turn_until(&mut engine, "the kick", |engine| {
                engine.played_by_text.contains(&kick)
            }));
            assert_eq!(audible(&engine), rewind.generation, "and lands whole");
            sent.extend(turns_for(&mut engine, Duration::from_millis(50)));
            assert!(late_lines(&sent).is_empty(), "{:?}", late_lines(&sent));
        }

        /// A sound whose load failed has settled: the start goes on.
        #[test]
        fn a_failed_sound_settles_a_held_start() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"kick\")");
            turns_for(&mut engine, Duration::from_millis(100));
            assert!(engine.start_is_held());
            library.fail_loading_sample_for_test();
            turn_until(&mut engine, "the start going on", |engine| {
                engine
                    .live
                    .as_ref()
                    .is_some_and(|live| live.initial_start_generation.is_none())
            });
            assert!(engine.snapshot().loading.is_none());
        }

        /// A map left reading loading with no job behind it holds nothing for
        /// good: the start waits while the loader has manifests in hand, and goes
        /// on, its cue gone, once the loader is idle.
        #[test]
        fn a_map_nobody_is_fetching_lets_a_held_start_go_on() {
            let library = Arc::new(SampleLibrary::empty());
            let mut engine = studio(LoadMode::Wait, &library);
            let stranded = "github:me/stranded";
            library.note_samples_source_state_for_tests(
                stranded,
                rustel_runtime::samples::SourceState::Loading,
            );
            let hold = library.hold_manifest_worker_for_test();
            start(
                &mut engine,
                &format!("samples('{stranded}')\n$: s(\"sine\")"),
            );
            turns_for(&mut engine, Duration::from_millis(50));
            assert!(engine.start_is_held(), "held while a manifest is in hand");
            assert!(engine.snapshot().loading.is_some_and(|cue| cue.waiting));

            drop(hold);
            turn_until(&mut engine, "the start going on", |engine| {
                engine
                    .live
                    .as_ref()
                    .is_some_and(|live| live.initial_start_generation.is_none())
            });
            assert_eq!(
                library.samples_source_state(stranded),
                Some(rustel_runtime::samples::SourceState::Loading),
                "the map still reads loading"
            );
            assert!(engine.snapshot().loading.is_none());
        }

        /// Two maps on a host that never answers, unique to this run: a trusted
        /// map a past run fetched is kept on disk. The host stays silent while the
        /// listener lives.
        fn silent_maps() -> (std::net::TcpListener, String, String) {
            // Connections wait in its backlog and are never answered.
            let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("a silent host");
            let host = silent.local_addr().expect("its address");
            let run = format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let first = format!("http://{host}/{run}-first.json");
            let later = format!("http://{host}/{run}-later.json");
            (silent, first, later)
        }

        /// A batch whose first map spends its budget on a host that never answers
        /// holds nothing for good: the map it never reached reads failed too, and
        /// the held start goes on, its cue gone, once the loader is idle.
        #[test]
        fn a_batch_spent_on_its_first_map_lets_a_held_start_go_on() {
            let library = Arc::new(SampleLibrary::empty());
            let mut engine = studio(LoadMode::Wait, &library);
            let (_silent, first, later) = silent_maps();
            let hold = library.hold_manifest_worker_for_test();
            library.register_samples_batch_for_test(&[&first, &later], Duration::from_millis(300));
            start(
                &mut engine,
                &format!("samples('{first}')\nsamples('{later}')\n$: s(\"sine\")"),
            );
            turns_for(&mut engine, Duration::from_millis(50));
            assert!(engine.start_is_held(), "held while the batch is in hand");
            assert!(engine.snapshot().loading.is_some_and(|cue| cue.waiting));

            drop(hold);
            turn_until(&mut engine, "the start going on", |engine| {
                engine
                    .live
                    .as_ref()
                    .is_some_and(|live| live.initial_start_generation.is_none())
            });
            for spec in [&first, &later] {
                let state = library.samples_source_state(spec);
                assert!(
                    matches!(state, Some(rustel_runtime::samples::SourceState::Failed(_))),
                    "{spec}: {state:?}"
                );
            }
            assert!(engine.snapshot().loading.is_none());
        }

        /// Stop cancels a held start, a held edit and the cue.
        #[test]
        fn stop_cancels_a_held_start_a_held_edit_and_the_cue() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"kick\")");
            turns_for(&mut engine, Duration::from_millis(50));
            engine.request_stop();
            assert!(
                engine.snapshot().loading.is_none(),
                "the cue goes with Stop"
            );
            turn_until(&mut engine, "the stop", |engine| !engine.is_playing());
            library.finish_loading_sample_for_test();
            assert!(
                engine.played_by_text.is_empty(),
                "the held start never sounds"
            );

            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"sine\")");
            assert!(engine.hold_update("s(\"kick\")", false, false));
            engine.request_stop();
            assert!(matches!(
                engine.take_launch_outcome(),
                Some(Err(RuntimeError::Cancelled))
            ));
            assert!(engine.load.held_edit.is_none());
        }

        /// An edit naming a sound not ready keeps the old score until it is, then
        /// lands whole and is answered.
        #[test]
        fn a_held_edit_keeps_the_old_score_until_its_sounds_load() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"sine\")");
            turns_for(&mut engine, Duration::from_millis(100));
            assert!(engine.hold_update("s(\"kick\")", false, false));
            turns_for(&mut engine, Duration::from_millis(200));
            assert_eq!(engine.active_source(), Some("s(\"sine\")"));
            assert!(engine.take_launch_outcome().is_none());
            assert!(engine.snapshot().loading.expect("the cue").waiting);

            library.finish_loading_sample_for_test();
            turn_until(&mut engine, "the edit landing", |engine| {
                engine.active_source() == Some("s(\"kick\")")
            });
            assert!(matches!(engine.take_launch_outcome(), Some(Ok(_))));
        }

        /// Async lands an edit at once, however its sounds stand.
        #[test]
        fn an_async_edit_lands_at_once() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Async, &library);
            start(&mut engine, "s(\"sine\")");
            assert!(!engine.hold_update("s(\"kick\")", false, false));
        }

        /// An edit that needs no new sound is never delayed: a value changed, a
        /// visual added, or a value changed in a score whose own later sound is
        /// still loading.
        #[test]
        fn an_edit_needing_no_new_sound_is_never_delayed() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"sine\")");
            for edit in [
                "s(\"sine\").gain(0.5)",
                "s(\"sine\").gain(slider(0.4, 0, 1))",
                "s(\"sine\")\nosc(10).out()",
            ] {
                assert!(!engine.hold_update(edit, false, false), "{edit}");
            }

            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"<sine sine kick>\")");
            assert!(!engine.start_is_held(), "the kick is past the first window");
            assert!(!engine.hold_update("s(\"<sine sine kick>\").gain(0.5)", false, false));
        }

        /// The fixture plugins, with the preset `Soft` of the effect, in a new
        /// folder that is the only plugin folder of the process. One test at a
        /// time has the folder.
        #[cfg(feature = "vst")]
        fn fixture_plugins() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
            static FOLDER: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let one = FOLDER.lock().unwrap_or_else(|poison| poison.into_inner());
            let folder = tempfile::tempdir().expect("plugins");
            rustel_vst3_fixture::install(folder.path());
            let presets = folder.path().join("presets");
            let soft = presets.join(rustel_vst3_fixture::NAME);
            std::fs::create_dir_all(&soft).expect("preset folder");
            std::fs::write(
                soft.join("Soft.vstpreset"),
                rustel_vst3_fixture::preset(0.2, 0.0),
            )
            .expect("preset");
            rustel_runtime::vst::pin_standard_folders(vec![folder.path().to_path_buf()]);
            let host = rustel_runtime::vst::host();
            host.set_preset_folder(presets);
            host.wait_idle();
            (one, folder)
        }

        /// The plugin copies of the host now: built, and not ended.
        #[cfg(feature = "vst")]
        fn copies() -> usize {
            let plugins = rustel_runtime::vst::host().plugins();
            plugins.iter().map(|plugin| plugin.running).sum()
        }

        /// [`start`] with the plugins of the process on the output, as a play
        /// opens it. Turns until the copies of the first window are ready, and
        /// the plugin thread has no work.
        #[cfg(feature = "vst")]
        fn start_with_plugins(engine: &mut StudioEngine, source: &str) {
            engine.session.transport().start();
            let device =
                LiveScalarDevice::start_silent(48_000, engine.generation()).expect("silent output");
            engine.session.attach_instruments(&device);
            engine
                .session
                .bind_audio_confirmations(device.confirmations())
                .expect("bind the silent output");
            engine
                .start_on(device, source, false, false)
                .expect("the start");
            turn_until(engine, "the copies of the start", |engine| {
                !engine.start_is_held()
            });
            rustel_runtime::vst::host().wait_idle();
        }

        /// Changing controls must not wait again for the plugin copies of the
        /// playing score, including two chains sharing the default orbit: the
        /// session holds no such edit, and builds no copy.
        #[cfg(feature = "vst")]
        #[test]
        fn plugin_parameter_edits_do_not_reenter_loading() {
            let _plugins = fixture_plugins();
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            let source = r#"$: s("sine").vst("rustel fixture").vst("rustel fixture", {gain: 0.6})
$: s("supersaw").seg(16).vst("rustel fixture", {gain: 0.1})"#;
            start_with_plugins(&mut engine, source);
            let built = copies();
            for edit in [
                source.replace("gain: 0.1", "gain: 0.9"),
                source.replace("gain: 0.1", "gain: slider(0.2, 0, 1), beatgate: 0.5"),
                source.replace("{gain: 0.1}", "{}"),
                format!("// shifted source positions\n{source}"),
                source.replace(".seg(16)", ".seg(8).gain(0.5)"),
                source.replace(".seg(16)", ".seg(16).orbit(2)"),
            ] {
                let install = engine.evaluate(&edit, false).expect("an edit");
                assert!(!install.held, "{edit}");
                assert_eq!(copies(), built, "{edit}");
            }
            drop(engine);
            rustel_runtime::vst::pin_standard_folders(Vec::new());
        }

        /// An edit with a new plugin copy goes in play with the copy ready: the
        /// session builds each new copy before the commit. A copy of the fixture
        /// builds in some ms, inside the short wait of the commit, so the edit
        /// lands at once. An edit with no new chain builds no copy, and the
        /// async mode holds no edit.
        #[cfg(feature = "vst")]
        #[test]
        fn new_plugin_copies_are_still_followed_after_an_edit() {
            let _plugins = fixture_plugins();
            let library = loading_kick();
            let playing = r#"$: s("sine").vst("rustel fixture", {gain: 0.1})"#;
            for (edit, new) in [
                // A new preset, one more effect, a second lane, an instrument.
                (
                    r#"$: s("sine").vst("rustel fixture", {gain: 0.1, preset: "Soft"})"#.to_owned(),
                    1,
                ),
                (format!("{playing}.vst(\"rustel fixture\")"), 1),
                (
                    format!("{playing}\n$: s(\"sine\").vst(\"rustel fixture\")"),
                    1,
                ),
                (r#"$: note("c3").vsti("rustel fixture tone")"#.to_owned(), 1),
                // No new chain: a lane name, a lane above, a muted lane above.
                (playing.replace("$:", "lead:"), 0),
                (format!("$: s(\"sine\")\n{playing}"), 0),
                (
                    format!("_$: s(\"sine\").vst(\"rustel fixture\")\n{playing}"),
                    0,
                ),
            ] {
                let mut engine = studio(LoadMode::Wait, &library);
                start_with_plugins(&mut engine, playing);
                let built = copies();
                let install = engine.evaluate(&edit, false).expect("an edit");
                assert!(!install.held, "{edit}");
                assert_eq!(engine.active_source(), Some(edit.as_str()));
                assert_eq!(copies() - built, new, "{edit}");
            }
            let mut engine = studio(LoadMode::Async, &library);
            start_with_plugins(&mut engine, playing);
            let edit = playing.replace("gain: 0.1", "preset: \"Soft\"");
            assert!(!engine.evaluate(&edit, false).expect("an edit").held);
            drop(engine);
            rustel_runtime::vst::pin_standard_folders(Vec::new());
        }

        /// Preparing a replacement does not take the slot away from the plugin
        /// the current score is still playing. An edit of an orbit keeps each
        /// copy in play, and an edit with a new preset builds its copy for a
        /// slot of its own while the copies in play keep their slots.
        #[cfg(feature = "vst")]
        #[test]
        fn preparing_a_plugin_edit_preserves_the_playing_insert() {
            use rustel_runtime::vst;
            use std::sync::Mutex;

            let _plugins = fixture_plugins();
            let host = vst::host();
            let vst::Resolved::Ready(plugin, _) = host.resolve(rustel_vst3_fixture::NAME, true)
            else {
                panic!("fixture loaded");
            };
            let key = rustel_audio::InsertKey { plugin, preset: 0 };
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            let source = r#"setcpm(90/4)
$: note("g#3*4").s("sine").vst("Rustel Fixture")
    .vst("Rustel Fixture", {gain:0.6}).orbit(1)
$: s("supersaw").seg(16)"#;
            start_with_plugins(&mut engine, source);
            let slots = Arc::new(Mutex::new(Vec::new()));
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .set_insert_provider(Arc::new({
                    let slots = Arc::clone(&slots);
                    let provider = host.provider(false);
                    move |key, sample_rate, slot| {
                        let insert = provider(key, sample_rate, slot);
                        if insert.is_some() {
                            slots.lock().unwrap().push(slot);
                        }
                        insert
                    }
                }));
            // The copies in play still hold their slots.
            let in_play = |engine: &StudioEngine, slots: &[rustel_audio::InsertSlot]| {
                let device = &engine.live.as_ref().unwrap().device;
                slots.iter().all(|slot| device.holds_insert(*slot, key))
            };
            turn_until(&mut engine, "the playing insert", |engine| {
                let slots = slots.lock().unwrap();
                slots.len() == 2 && in_play(engine, &slots)
            });

            for orbit in [2, 1, 2, 1] {
                let incoming = source.replace(".orbit(1)", &format!(".orbit({orbit})"));
                assert!(!engine.hold_update(&incoming, false, false));
                let install = engine.evaluate(&incoming, false).expect("orbit edit");
                assert!(!install.held);
                turn_until(
                    &mut engine,
                    "the orbit edit reaching the output",
                    |engine| {
                        let device = &engine.live.as_ref().unwrap().device;
                        device.generation() == engine.generation()
                            && in_play(engine, &slots.lock().unwrap())
                    },
                );
                assert_eq!(engine.active_source(), Some(incoming.as_str()));
                assert!(engine.load.held_edit.is_none());
                assert!(engine.snapshot().loading.is_none());
                assert_eq!(slots.lock().unwrap().len(), 2);
            }

            // The edit with a new preset waits for its sound, and the session
            // builds its copy in the wait. The copies in play keep their slots
            // until the copy of the edit takes a slot of its own.
            let played = slots.lock().unwrap().clone();
            let incoming = r#"s("kick*16").vst("Rustel Fixture", {preset: "Soft"})"#;
            assert!(engine.hold_update(incoming, false, false));
            turn_until(&mut engine, "the copy of the held edit", |engine| {
                assert!(in_play(engine, &played));
                !engine.session.waits_for_plugins(incoming)
            });
            // The copy is ready before the edit lands.
            assert_eq!(engine.session.plugins_waited_for(incoming), Some((0, 1)));
            library.finish_loading_sample_for_test();
            turn_until(&mut engine, "the copy of the edit in play", |engine| {
                engine.active_source() == Some(incoming) && slots.lock().unwrap().len() == 3
            });
            assert!(in_play(&engine, &played));
            assert!(!played.contains(&slots.lock().unwrap()[2]));
            drop(engine);
            vst::pin_standard_folders(Vec::new());
        }

        /// A newer edit replaces a held one.
        #[test]
        fn a_newer_edit_replaces_a_held_one() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"sine\")");
            assert!(engine.hold_update("s(\"kick\")", false, false));
            engine.cancel_pending_launch();
            assert!(matches!(
                engine.take_launch_outcome(),
                Some(Err(RuntimeError::Cancelled))
            ));
            assert!(engine.hold_update("s(\"kick\").gain(0.5)", false, false));
            library.finish_loading_sample_for_test();
            turn_until(&mut engine, "the newer edit", |engine| {
                engine.active_source() == Some("s(\"kick\").gain(0.5)")
            });
        }

        /// A quantised launch whose sound is still loading lets its line pass and
        /// lands on the first line after the sound is in.
        #[test]
        fn a_quantised_launch_lands_on_the_first_line_after_its_sounds() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"sine\")");
            turns_for(&mut engine, Duration::from_millis(100));
            let armed = engine
                .arm_launch("s(\"kick\")", false, 0.25, false)
                .expect("arm")
                .expect("a line");
            let first_line = armed.boundary_cycle;
            turn_until(&mut engine, "the first line passing", |engine| {
                engine.session.cycle_at_time(device_now(engine)) > first_line + 0.25
            });
            assert!(engine.take_launch_outcome().is_none(), "it has not fired");
            library.finish_loading_sample_for_test();
            let arrived = engine.session.cycle_at_time(device_now(&engine));
            turn_until(&mut engine, "the launch firing", |engine| {
                engine.launch_outcome.is_some()
            });
            let landing = engine
                .landing
                .as_ref()
                .expect("the fired launch")
                .boundary_cycle;
            assert!(landing > arrived, "{landing} after {arrived}");
            assert!(landing - arrived <= 0.25 + 0.1, "the first line after");
        }

        /// A Play pressed while a preview's silent output is open is held, and the
        /// preview ending does not close the output it is about to take over.
        #[test]
        fn a_play_held_over_a_preview_keeps_its_output_and_lands() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "silence");
            engine.live.as_mut().expect("playing").audition_owned = true;
            engine.audition_horizon_frame = 0;
            assert!(engine.hold_update("s(\"kick\")", false, false));
            turns_for(&mut engine, Duration::from_millis(300));
            assert!(
                engine.is_playing() && !engine.is_stopping(),
                "the output stays"
            );
            library.finish_loading_sample_for_test();
            turn_until(&mut engine, "the play landing", |engine| {
                engine.active_source() == Some("s(\"kick\")")
            });
            assert!(!engine.live.as_ref().expect("playing").audition_owned);
            turns_for(&mut engine, Duration::from_millis(100));
            assert!(engine.is_playing() && !engine.is_stopping());
        }

        /// Previews sound through a held start's output as through a playing
        /// one: a preview whose sample comes in while the start waits is pushed
        /// then, not when the start opens, and a run hands each note over at its
        /// own moment rather than all at the downbeat.
        #[test]
        fn previews_play_while_a_start_is_held() {
            let library = Arc::new(SampleLibrary::with_loading_note_bank_for_test(
                "keys",
                &[36.0, 60.0],
            ));
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "note(\"c4\").s(\"keys\")");
            turns_for(&mut engine, Duration::from_millis(50));
            assert!(engine.start_is_held(), "the start waits for its key");

            engine
                .audition_notes(&[36.0], "keys", 0.5)
                .expect("a preview");
            let pending = |engine: &StudioEngine| {
                engine
                    .live
                    .as_ref()
                    .is_some_and(|live| live.audition.is_some())
            };
            assert!(pending(&engine), "the previewed key is loading");
            library.finish_note_key_for_test(36.0);
            turns_for(&mut engine, Duration::from_millis(50));
            assert!(
                !pending(&engine),
                "the preview went out once its key was in"
            );
            assert!(engine.start_is_held(), "while the start still waits");

            engine
                .audition_run(&[60.0, 62.0], "sine", 0.5, 0.2)
                .expect("a run");
            turn_until(&mut engine, "the run's second note", |engine| {
                engine.live.as_ref().is_some_and(|live| live.run.is_none())
            });
            assert!(engine.start_is_held(), "handed over while the start waits");
        }

        /// The cue asks for what it follows as bets, never ahead of what plays,
        /// and follows the banked spelling a `.bank()` plays rather than the
        /// default bank's name the score never plays.
        #[test]
        fn the_cue_asks_only_bets_and_only_for_what_the_bank_plays() {
            let library = Arc::new(SampleLibrary::with_unasked_banks_for_test(&[
                "bd", "tr909_bd",
            ]));
            let mut engine = studio(LoadMode::Async, &library);
            let source = "s(\"bd\").bank(\"tr909\")";
            assert_eq!(
                engine.followed_in_text(source, false),
                [Followed::Named {
                    name: "tr909_bd".into(),
                    n: 0.0
                }]
            );
            engine.follow_edit(source, false);
            super::tests::play_silently(&mut engine);
            let cue = engine.loading_cue().expect("the banked kick is loading");
            assert_eq!(cue.total, 1);
            assert_eq!(
                library.queued_loads_for_test(),
                (0, 1),
                "a bet, and only one"
            );
            assert!(!library.asked_for_test("unasked-bd.wav"));
        }

        /// In wait, what a held edit or an armed launch waits on is asked at play
        /// priority, as an evaluated update's window is, so a bet placed since
        /// does not go ahead of it. Async leaves an armed launch's sounds
        /// to bets until it fires.
        #[test]
        fn a_waiting_update_asks_for_its_sounds_ahead_of_every_bet() {
            let library = Arc::new(SampleLibrary::with_unasked_banks_for_test(&[
                "kick", "snare",
            ]));
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"sine\")");
            assert!(engine.hold_update("s(\"kick\")", false, false));
            assert_eq!(
                library.queued_loads_for_test(),
                (1, 0),
                "the kick plays next"
            );
            engine.cancel_pending_launch();
            engine
                .arm_launch("s(\"snare\")", false, 1.0, false)
                .expect("arm")
                .expect("a line");
            assert_eq!(library.queued_loads_for_test(), (2, 0), "so does the snare");

            let library = Arc::new(SampleLibrary::with_unasked_banks_for_test(&["snare"]));
            let mut engine = studio(LoadMode::Async, &library);
            start(&mut engine, "s(\"sine\")");
            engine
                .arm_launch("s(\"snare\")", false, 1.0, false)
                .expect("arm")
                .expect("a line");
            turn(&mut engine);
            assert_eq!(
                library.queued_loads_for_test(),
                (0, 1),
                "a bet until it fires"
            );
        }

        /// The late-sounds line names every sound that came late during the
        /// load, whichever turn it came late on.
        #[test]
        fn the_late_line_names_every_sound_late_on_any_turn_of_the_load() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Async, &library);
            start(&mut engine, "s(\"kick\")");
            engine.note_late("sample \"snare:0\" is still loading");
            engine.settle_loads();
            engine.note_late("sample \"hat:2\" is still loading");
            engine.settle_loads();
            assert!(
                !engine
                    .pending_diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.kind == rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC),
                "not while the kick loads"
            );
            library.finish_loading_sample_for_test();
            engine.settle_loads();
            let late: Vec<_> = engine
                .pending_diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.kind == rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC)
                .collect();
            assert_eq!(late.len(), 1, "{late:?}");
            assert!(late[0].message.contains("hat:2") && late[0].message.contains("snare:0"));
        }

        /// An edit waits only for what its live code plays: not for a name in a
        /// comment, in a muted lane, or under a bank that does not hold it.
        #[test]
        fn an_edit_waits_only_for_live_names() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"sine\")");
            for edit in [
                "// s(\"kick\")\n$: s(\"sine\")",
                "_$: s(\"kick\")\n$: s(\"sine\")",
                "$: s(\"sine\")\n$_: s(\"kick\")",
                "$: s(\"sine\")\ndrums_ : s(\"kick\")",
                "$: s(\"sine\")\n_$: stack(\n  s(\"sine\"),\n  // kick\n\n  s(\"kick\"),\n)",
                "$: s(\"kick\").bank(\"other\")",
            ] {
                assert!(!engine.hold_update(edit, false, false), "{edit}");
            }
            assert!(engine.hold_update("$: s(\"kick\")", false, false));
        }

        /// A bank keyed by note stands for every key: the edit lands once the
        /// last key's file is in, and the cue counts each key.
        #[test]
        fn a_note_keyed_bank_waits_for_every_key() {
            let library = Arc::new(SampleLibrary::with_loading_note_bank_for_test(
                "keys",
                &[36.0, 60.0],
            ));
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"sine\")");
            let edit = "note(\"c2 c4\").s(\"keys\")";
            assert!(engine.hold_update(edit, false, false));
            let cue = engine.snapshot().loading.expect("the keys load");
            assert_eq!((cue.settled, cue.total), (0, 2));

            library.finish_note_key_for_test(36.0);
            turns_for(&mut engine, Duration::from_millis(100));
            assert!(engine.load.held_edit.is_some(), "one key is not the bank");
            let cue = engine.snapshot().loading.expect("one key still loads");
            assert_eq!((cue.settled, cue.total), (1, 2));

            library.finish_note_key_for_test(60.0);
            turn_until(&mut engine, "the edit landing", |engine| {
                engine.active_source() == Some(edit)
            });
        }

        /// An edit whose sounds come from its own import asks for the import
        /// first, so its names read as loading and it waits for them.
        #[test]
        fn an_edit_asks_for_its_own_import_and_waits_for_it() {
            let library = Arc::new(SampleLibrary::empty());
            let mut session = SessionConfig::default();
            session
                .score_sample_access
                .permit_origin("http://127.0.0.1:9")
                .expect("the test origin");
            let mut engine = studio_with(LoadMode::Wait, &library, session);
            start(&mut engine, "s(\"sine\")");
            let hold = library.hold_manifest_worker_for_test();
            let spec = "http://127.0.0.1:9/kit.json";
            let edit = format!("samples('{spec}')\n$: s(\"kit_bd\")");
            assert_eq!(library.samples_source_state(spec), None);
            assert!(engine.hold_update(&edit, false, false));
            assert_eq!(
                library.samples_source_state(spec),
                Some(rustel_runtime::samples::SourceState::Loading),
                "the import was asked for"
            );
            turns_for(&mut engine, Duration::from_millis(100));
            assert!(
                engine.load.held_edit.is_some(),
                "the edit waits for the map"
            );
            drop(hold);
            turn_until(&mut engine, "the edit landing", |engine| {
                engine.launch_outcome.is_some()
            });
        }

        /// An update with a mistake sent while a start waits leaves the held
        /// start exactly as it was; a good one replaces it from its own cycle
        /// zero and waits for its own first window.
        #[test]
        fn an_update_during_a_held_start_replaces_it_only_if_it_evaluates() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"kick\")");
            turns_for(&mut engine, Duration::from_millis(50));
            let generation = engine.generation();
            assert!(engine.evaluate("s(\"kick\"", false).is_err());
            assert!(engine.start_is_held(), "the held start stays");
            assert_eq!(engine.generation(), generation);
            assert_eq!(engine.active_source(), Some("s(\"kick\")"));

            let install = engine
                .evaluate("s(\"kick\").gain(0.8)", false)
                .expect("a good update");
            assert!(engine.start_is_held(), "it waits for its own first window");
            assert_eq!(
                engine
                    .live
                    .as_ref()
                    .and_then(|live| live.initial_start_generation),
                Some(install.generation)
            );
            let kick = library.finish_loading_sample_for_test();
            let arrived = device_now(&engine);
            turn_until(&mut engine, "the kick", |engine| {
                engine.played_by_text.contains(&kick)
            });
            assert!(engine.session.cycle_at_time(arrived) < 0.0);
        }

        /// A slider moved while a start waits is part of the running score, its
        /// layout having reached the display on the waiting turns, and moving it
        /// does not requery: the first query reads it, and cycle zero stays the
        /// start's.
        #[test]
        fn a_held_start_takes_slider_moves_without_moving_its_cycle_zero() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(&mut engine, "s(\"kick\").gain(slider(0.5, 0, 1))");
            let sent = turns_for(&mut engine, Duration::from_millis(100));
            assert!(engine.start_is_held());
            let layout = sent
                .iter()
                .find_map(|update| match update {
                    StudioUpdate::Layout(layout) => Some(layout),
                    _ => None,
                })
                .expect("the layout reaches the display while the start waits");
            let slider = layout
                .ui_layout
                .sliders
                .first()
                .expect("the slider")
                .id
                .clone();
            let generation = engine.generation();
            engine
                .set_slider(&slider, 0.9)
                .expect("the waiting score's slider moves");
            turns_for(&mut engine, Duration::from_millis(200));
            assert_eq!(engine.generation(), generation, "no requery");

            let kick = library.finish_loading_sample_for_test();
            let arrived = device_now(&engine);
            turn_until(&mut engine, "the kick", |engine| {
                engine.played_by_text.contains(&kick)
            });
            assert!(engine.session.cycle_at_time(arrived) < 0.0);
        }

        /// A key pressed while a start waits does not requery either: the first
        /// query reads the keys as they stand, and cycle zero stays the start's.
        #[test]
        fn a_held_start_takes_key_presses_without_moving_its_cycle_zero() {
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            start(
                &mut engine,
                "const kb = await midikeys('keyboard')\n$: kb(0.25).s(\"tri\")\n$: s(\"kick\")\n",
            );
            turns_for(&mut engine, Duration::from_millis(50));
            assert!(engine.start_is_held());
            let port = engine
                .session
                .midi_input_bus()
                .find("keyboard")
                .expect("the score's keyboard");
            let generation = engine.generation();
            port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
            turns_for(&mut engine, Duration::from_millis(200));
            assert_eq!(engine.generation(), generation, "no requery");

            let kick = library.finish_loading_sample_for_test();
            let arrived = device_now(&engine);
            turn_until(&mut engine, "the kick", |engine| {
                engine.played_by_text.contains(&kick)
            });
            assert!(engine.session.cycle_at_time(arrived) < 0.0);
        }

        /// The alerts `engine` has queued, raised or resolved.
        fn queued_alerts(engine: &StudioEngine) -> Vec<DiagnosticAlert> {
            engine
                .pending_diagnostics
                .iter()
                .filter_map(|diagnostic| diagnostic.alert.clone())
                .collect()
        }

        /// An import's failure is said under the key of the map it names, and
        /// resolved once a retry brings the map in. A failure naming no map is
        /// no import's, whatever its words.
        #[test]
        fn a_failed_import_is_said_under_its_key_and_resolved_when_it_comes_in() {
            let library = Arc::new(SampleLibrary::empty());
            let mut engine = studio(LoadMode::Wait, &library);
            let spec = "github:me/kit";
            let gone = rustel_runtime::samples::SourceState::Failed("the map is gone".into());
            library.note_samples_source_state_for_tests(spec, gone);
            let said = engine.import_failure(SampleFailure {
                message: "the map is gone".into(),
                maps: vec![serde_json::to_string(spec).expect("a string")],
            });
            let key = import_alert(spec);
            assert_eq!(said.alert, Some(DiagnosticAlert::Raise(key.clone())));
            queue_diagnostic(&mut engine.pending_diagnostics, said);
            emit_pending_diagnostics(&mut engine.pending_diagnostics, &mut |_| Ok(()));
            let unrelated =
                engine.import_failure(SampleFailure::from("the map is gone".to_owned()));
            assert_eq!(unrelated.alert, None, "not every failure is an import's");

            engine.settle_loads();
            assert!(engine.pending_diagnostics.is_empty(), "still failed");
            library.note_samples_source_state_for_tests(
                spec,
                rustel_runtime::samples::SourceState::Ready,
            );
            engine.settle_loads();
            assert_eq!(queued_alerts(&engine), [DiagnosticAlert::Resolve(key)]);
        }

        /// A batch spent on its first map says so once, about every map it left
        /// failed: the map it never reached coming in resolves nothing while the
        /// first still fails, and the line resolves once both are in.
        #[test]
        fn a_spent_batch_resolves_only_once_every_map_it_left_failed_is_in() {
            let library = Arc::new(SampleLibrary::empty());
            let mut engine = studio(LoadMode::Wait, &library);
            let (_silent, first, later) = silent_maps();
            library.register_samples_batch_for_test(&[&first, &later], Duration::from_millis(300));
            library.wait_until_idle(Duration::from_secs(10));

            engine.collect_session_diagnostics();
            let alerts = queued_alerts(&engine);
            let [DiagnosticAlert::Raise(key)] = alerts.as_slice() else {
                panic!("the spent batch is said under one alert: {alerts:?}");
            };
            let key = key.clone();
            engine.pending_diagnostics.clear();

            library.note_samples_source_state_for_tests(
                &later,
                rustel_runtime::samples::SourceState::Ready,
            );
            engine.settle_loads();
            assert!(
                queued_alerts(&engine).is_empty(),
                "the first map still fails"
            );
            library.note_samples_source_state_for_tests(
                &first,
                rustel_runtime::samples::SourceState::Ready,
            );
            engine.settle_loads();
            assert_eq!(queued_alerts(&engine), [DiagnosticAlert::Resolve(key)]);
        }

        /// A failed import the session's own scheduling meets before the engine
        /// collects it keeps its alert.
        #[test]
        fn a_failed_import_met_by_a_scheduling_step_keeps_its_alert() {
            let library = Arc::new(SampleLibrary::empty());
            let mut engine = studio(LoadMode::Wait, &library);
            let spec = "github:me/kit";
            engine.session.evaluate("s(\"sine\")").expect("a score");
            library.register_samples_batch_for_test(&[spec], Duration::ZERO);
            library.wait_until_idle(Duration::from_secs(10));

            engine
                .session
                .schedule_audio_through(0.0, 0.5, 48_000)
                .expect("a scheduling step");
            engine.collect_session_diagnostics();
            assert_eq!(
                queued_alerts(&engine),
                [DiagnosticAlert::Raise(import_alert(spec))]
            );
        }

        /// A launch armed while a new score waits for its first window prepares
        /// nothing, so the new score keeps its takeover. The worker fires a due
        /// launch and arms the next one in one command, with no turn between.
        #[cfg(feature = "vst")]
        #[test]
        fn a_launch_armed_while_a_new_score_waits_prepares_nothing() {
            let _plugins = fixture_plugins();
            let library = loading_kick();
            let mut engine = studio(LoadMode::Wait, &library);
            let playing = r#"$: s("sine")"#;
            start_with_plugins(&mut engine, playing);
            let fired = format!("{playing}.gain(0.5)");
            let armed = engine.arm_launch(&fired, false, 0.25, false);
            assert!(armed.expect("arm").is_some());
            let deadline = Instant::now() + Duration::from_secs(10);
            let install = loop {
                super::tests::hold_clock(&engine, true);
                engine.fire_due_launch();
                if let Some(outcome) = engine.take_launch_outcome() {
                    break outcome.expect("the fired launch");
                }
                super::tests::hold_clock(&engine, false);
                assert!(Instant::now() < deadline, "the launch never fired");
                std::thread::sleep(Duration::from_millis(2));
            };
            let next =
                format!("{fired}\n$: s(\"sine\").vst(\"rustel fixture\", {{preset: \"Soft\"}})");
            let armed = engine.arm_launch(&next, false, 0.25, false);
            assert!(armed.expect("arm").is_some());
            assert!(engine.session.plugins_waited_for(&next).is_none());
            let published =
                |engine: &StudioEngine| engine.live.as_ref().unwrap().device.generation();
            assert!(published(&engine) < install.generation);
            super::tests::hold_clock(&engine, false);
            turn_until(
                &mut engine,
                "the first window of the fired launch",
                |engine| published(engine) == install.generation,
            );
            drop(engine);
            rustel_runtime::vst::pin_standard_folders(Vec::new());
        }
    }
}
