//! Sounds a start or an edit needs while they load: what a start holds its
//! cycle zero for and what a held edit waits on in [`LoadMode::Wait`], the
//! header's loading cue, the one line a load's late sounds are said in, and
//! the log alerts those lines and failed imports raise and resolve.
//!
//! Plugins are part of the same wait. A start waits for each plugin of its
//! first window to be ready for its orbit, as the host answers for the
//! score in play. An edit and a launch are not in play yet, so their wait
//! reads the text: each new plugin call, with its preset and the orbit of
//! its statement. With no one orbit in the text, the wait is for the load.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(feature = "vst")]
use std::time::{Duration, Instant};

use rustel_runtime::samples::{SampleFailure, SampleLibrary, SoundReadiness, SourceState};
use rustel_runtime::{RuntimeError, SAMPLE_LOADING_DIAGNOSTIC};

use super::super::settings::LoadMode;
use super::{
    STARTUP_SAMPLE_WARM_BUDGET, StudioDiagnostic, StudioEngine, StudioInstall, StudioUpdate,
    StudioUpdateSendResult, TakeoverCut, next_boundary, queue_diagnostic, source_revision,
};

/// How far a start's first window reaches, in cycles.
const FIRST_WINDOW_CYCLES: f64 = 2.0;

/// How long a start, an edit or a launch waits for a plugin. A plugin still
/// in its load after 20 seconds plays late: an effect note plays dry and an
/// instrument note is silent.
#[cfg(feature = "vst")]
const PLUGIN_WAIT: Duration = Duration::from_secs(20);

/// How often a held start looks at the plugins of its first window. Each
/// look queries the score.
#[cfg(feature = "vst")]
const PLUGIN_LOOK_EVERY: Duration = Duration::from_millis(20);

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
    /// A `.vst()` or `.vsti()` call of a text. The wait for its plugin
    /// ends at `until`.
    #[cfg(feature = "vst")]
    Plugin {
        call: rustel_runtime::lint::NamedPlugin,
        until: Instant,
    },
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
            #[cfg(feature = "vst")]
            Self::Plugin { .. } => {}
        }
    }

    /// Visit each file this stands for, with whether it is still loading.
    /// Asked as bets: nothing here moves a load ahead of what plays. A map
    /// loads only while the loader has manifests in hand; one reading
    /// loading with none has settled, and the names it would bring read
    /// unknown. A plugin look starts its load and builds its copy for the
    /// orbit, since the text with its call is not the score yet. The copy
    /// stays in the host until the notes of the new score need the copy.
    fn each_file(&self, library: &SampleLibrary, mut each: impl FnMut(&str, bool)) {
        if let Self::Import(spec) = self {
            let loading = library.manifests_pending() > 0
                && library.samples_source_state(spec) == Some(SourceState::Loading);
            return each(spec, loading);
        }
        #[cfg(feature = "vst")]
        if let Self::Plugin { call, until } = self {
            let loading = Instant::now() < *until
                && rustel_runtime::vst::loading(call, library.render_rate());
            return each(&call.name, loading);
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

/// An edit held until the sounds its text names have loaded.
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
    /// The plugin requests of the first window not ready for their orbits,
    /// at the last look of a held start.
    start_plugins: usize,
    /// The time of the last look, and the time the wait for the plugins
    /// ends.
    #[cfg(feature = "vst")]
    plugin_looks: Option<(Instant, Instant)>,
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
        let new = self
            .followed_in_text(source, mini)
            .into_iter()
            .filter(|item| !playing.contains(item));
        #[cfg(feature = "vst")]
        let new = new.chain(self.plugins_in(source, mini));
        new.collect()
    }

    /// New plugin requirements of a text, in [`LoadMode::Wait`]. Parameter
    /// edits keep the same copy, even if its controls or source positions
    /// changed. Async follows no plugin.
    #[cfg(feature = "vst")]
    fn plugins_in(&self, source: &str, mini: bool) -> Vec<Followed> {
        if mini || !self.waits_for_sounds() {
            return Vec::new();
        }
        let playing = self
            .session
            .active_source()
            .map(|source| self.session.plugin_calls_for_source(source))
            .unwrap_or_default();
        let until = Instant::now() + PLUGIN_WAIT;
        let mut plugins = Vec::new();
        for call in self.session.plugin_calls_for_source(source) {
            if playing.iter().any(|old| {
                old.name == call.name
                    && old.instrument == call.instrument
                    && old.stage == call.stage
                    && old.preset == call.preset
                    && old.orbit == call.orbit
            }) {
                continue;
            }
            push_new(&mut plugins, Followed::Plugin { call, until });
        }
        plugins
    }

    /// The plugin requests of the first window not ready for their orbits:
    /// see [`rustel_runtime::Session::plugins_pending`]. The rate is the
    /// rate of the output, as the warm of the start gave the rate to the
    /// host. The output of a start not yet live holds no plugin.
    #[cfg(feature = "vst")]
    fn first_window_plugins(&self) -> usize {
        // The warm of the start gave the host each plugin of the window.
        // With no host, the window has no plugin, and this starts none.
        if rustel_runtime::vst::started().is_none() {
            return 0;
        }
        let Some(library) = self.session.sample_library() else {
            return 0;
        };
        self.session.plugins_pending(
            0.0,
            FIRST_WINDOW_CYCLES,
            STARTUP_SAMPLE_WARM_BUDGET,
            library.render_rate(),
            |slot, key| {
                self.live
                    .as_ref()
                    .is_some_and(|live| live.device.holds_insert(slot, key))
            },
        )
    }

    /// One look of a held start at the plugins of its first window, each
    /// [`PLUGIN_LOOK_EVERY`] at most. After [`PLUGIN_WAIT`] the start goes
    /// on with no plugin.
    #[cfg(feature = "vst")]
    fn look_at_start_plugins(&mut self) {
        let Some((looked_at, until)) = self.load.plugin_looks else {
            return;
        };
        let now = Instant::now();
        if self.load.start_plugins == 0 || now.duration_since(looked_at) < PLUGIN_LOOK_EVERY {
            return;
        }
        self.load.start_plugins = if now < until {
            self.first_window_plugins()
        } else {
            0
        };
        self.load.plugin_looks = Some((now, until));
    }

    /// How the log names what of `followed` still loads.
    fn loading_words(&self, followed: &[Followed]) -> &'static str {
        #[cfg(feature = "vst")]
        {
            let loads = |plugin: bool| {
                followed.iter().any(|item| {
                    matches!(item, Followed::Plugin { .. }) == plugin
                        && self.loading_among(std::slice::from_ref(item))
                })
            };
            match (loads(false), loads(true)) {
                (true, true) => return "sounds and plugins",
                (false, true) => return "plugins",
                _ => {}
            }
        }
        #[cfg(not(feature = "vst"))]
        let _ = followed;
        "sounds"
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
    /// lands. A newer edit or a stop cancels it like a pending launch.
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
        let waits_for = self.loading_words(&followed);
        self.cancel_pending_launch();
        self.launch_outcome = None;
        self.load.held_edit = Some(HeldEdit {
            source: source.to_owned(),
            mini,
            rewind,
            preview: std::mem::take(&mut self.next_install_preview),
            followed,
        });
        self.ask_for_waited();
        self.log_launch(&format!("update waits for its {waits_for}"));
        true
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

    /// Land a held edit once its sounds have loaded, or at once when the
    /// mode no longer waits.
    pub(super) fn advance_held_edit(&mut self) {
        let due =
            self.load.held_edit.as_ref().is_some_and(|held| {
                !self.waits_for_sounds() || !self.loading_among(&held.followed)
            });
        if !due || self.live.is_none() || self.is_stopping() {
            return;
        }
        let held = self.load.held_edit.take().expect("checked");
        // The edit replaces whatever a fired launch was landing, as an
        // edit played now does.
        self.abandon_landing(false, false);
        if held.rewind {
            self.session.start_next_from_zero();
        }
        self.next_install_preview = held.preview;
        let outcome = self.reload_live(&held.source, held.mini);
        if outcome.is_err() {
            self.session.clear_next_from_zero();
        }
        self.launch_outcome = Some(outcome);
    }

    /// In [`LoadMode::Wait`], a launch whose sounds are still loading lets
    /// the line within its head-room pass and aims at the next one. The
    /// first line it lets pass is logged.
    pub(super) fn launch_waits_for_sounds(&mut self, now: f64) -> bool {
        let Some(pending) = self.pending_launch.as_ref() else {
            return false;
        };
        if !self.waits_for_sounds() || !self.loading_among(&pending.followed) {
            return false;
        }
        let waits_for = self.loading_words(&pending.followed);
        let cps = self.session.cps().max(1e-6);
        let cycle_now = self.session.cycle_at_time(now);
        let headroom_cycles = self.launch_headroom() * cps;
        let pending = self.pending_launch.as_mut().expect("checked");
        pending.boundary_cycle = next_boundary(cycle_now, pending.unit_cycles, headroom_cycles);
        pending.boundary_time = now + (pending.boundary_cycle - cycle_now) / cps;
        if !std::mem::replace(&mut pending.waited, true) {
            self.log_launch(&format!(
                "{waits_for} still loading - the launch takes the first line after them"
            ));
        }
        true
    }

    /// The text of an edit held for its sounds.
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
    /// for it in [`LoadMode::Wait`]. The plugins of the window hold the
    /// start too.
    pub(super) fn follow_start(&mut self, source: &str) -> Result<(), RuntimeError> {
        self.load.installed = self.followed_in_first_window(source)?;
        #[cfg(feature = "vst")]
        {
            let now = Instant::now();
            self.load.start_plugins = if self.waits_for_sounds() {
                self.first_window_plugins()
            } else {
                0
            };
            self.load.plugin_looks = Some((now, now + PLUGIN_WAIT));
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
    /// first window's sounds have loaded and its plugins are ready, when
    /// the start goes on.
    pub(super) fn hold_start(
        &mut self,
        emit: &mut impl FnMut(StudioUpdate) -> StudioUpdateSendResult,
    ) -> bool {
        if !self.start_is_held() {
            return false;
        }
        #[cfg(feature = "vst")]
        self.look_at_start_plugins();
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
        })
    }

    /// The header's loading cue: `None` unless something the playing or
    /// the waiting score needs is still loading.
    pub(super) fn loading_cue(&self) -> Option<LoadingCue> {
        if self.live.is_none() || self.is_stopping() {
            return None;
        }
        let library = self.session.sample_library()?;
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
        let mut cue = LoadingCue::default();
        for item in followed {
            #[cfg(feature = "vst")]
            let plugin = usize::from(matches!(item, Followed::Plugin { .. }));
            item.each_file(library, |name, loading| {
                cue.total += 1;
                if loading {
                    cue.loading.get_or_insert_with(|| name.to_owned());
                    #[cfg(feature = "vst")]
                    {
                        cue.plugins += plugin;
                    }
                } else {
                    cue.settled += 1;
                }
            });
        }
        // A held start counts the plugins of its first window. The count
        // has no names: the host answers for a slot.
        if self.start_is_held() {
            cue.total += self.load.start_plugins;
            cue.plugins += self.load.start_plugins;
        }
        if cue.settled == cue.total {
            return None;
        }
        cue.waiting = self.start_is_held()
            || self.load.held_edit.is_some()
            || (self.waits_for_sounds()
                && self
                    .pending_launch
                    .as_ref()
                    .is_some_and(|pending| self.loading_among(&pending.followed)));
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
    /// the alerts whose sounds or maps have come in.
    pub(super) fn settle_loads(&mut self) {
        self.ask_for_waited();
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
    /// the cue with them, and what came late so far is said now.
    pub(super) fn end_loads(&mut self) {
        self.load.installed.clear();
        self.load.start_held = false;
        self.load.start_plugins = 0;
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
