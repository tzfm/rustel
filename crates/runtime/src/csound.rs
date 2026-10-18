//! Orchestra definitions and score events for one session.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use rustel_audio::{InsertControls, InsertKey, InsertProvider};
use rustel_csound::{Event, Instrument, Program};
use rustel_jsruntime::CsoundSource;
use rustel_voice::{CsoundRequest, SampleLookup, SampleResolution};
use serde_json::{Map, Value};

use crate::score_import::{ImportRefusal, ScoreImportAccess};
use crate::session::RuntimeError;

/// Orchestra text retained for a fresh export session. Native instances are
/// rebuilt at the output sample rate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CsoundSnapshot(Arc<SnapshotData>);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SnapshotData {
    sources: Vec<String>,
    urls: BTreeSet<String>,
    initialized: bool,
}

impl CsoundSnapshot {
    /// Keep definitions from both snapshots, with newer definitions last.
    pub fn merge(&mut self, newer: &Self) -> Result<(), RuntimeError> {
        let mut merged = self.clone();
        let data = Arc::make_mut(&mut merged.0);
        for source in &newer.0.sources {
            data.sources.retain(|earlier| earlier != source);
            data.sources.push(source.clone());
        }
        data.urls.extend(newer.0.urls.iter().cloned());
        data.initialized |= newer.0.initialized;
        validate_snapshot_size(data)?;
        *self = merged;
        Ok(())
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn retained_snapshot_bytes(&self, limit: usize) -> Option<usize> {
        let data = &self.0;
        let mut bytes = std::mem::size_of::<SnapshotData>()
            .checked_add(2 * std::mem::size_of::<usize>())?
            .checked_add(
                data.sources
                    .capacity()
                    .checked_mul(std::mem::size_of::<String>())?,
            )?;
        if bytes > limit {
            return None;
        }
        for source in &data.sources {
            bytes = bytes.checked_add(source.capacity())?;
            if bytes > limit {
                return None;
            }
        }
        // Charge a full tree node per URL, including spare keys and edges.
        let node_bytes = 11 * std::mem::size_of::<String>() + 16 * std::mem::size_of::<usize>();
        for url in &data.urls {
            bytes = bytes.checked_add(node_bytes)?.checked_add(url.capacity())?;
            if bytes > limit {
                return None;
            }
        }
        Some(bytes)
    }
}

fn validate_snapshot_size(data: &SnapshotData) -> Result<(), RuntimeError> {
    if data.sources.len() > 256
        || data.urls.len() > 256
        || data.sources.iter().map(String::len).sum::<usize>()
            + data.urls.iter().map(String::len).sum::<usize>()
            > 16 * 1024 * 1024
    {
        Err(RuntimeError::ResourceLimit(
            "Csound orchestra history exceeds 256 sources or 16 MiB".into(),
        ))
    } else {
        Ok(())
    }
}

#[derive(Default)]
struct Store {
    allowed: bool,
    snapshot: CsoundSnapshot,
    current: Option<Program>,
    revisions: HashMap<InsertKey, Program>,
    chains: HashSet<u64>,
    errors: Vec<String>,
    render_timeline: Option<RenderTimeline>,
    #[cfg(feature = "device-audio")]
    prepared: Option<HashMap<(InsertKey, u64), Option<f64>>>,
}

#[derive(Clone)]
struct RenderTimeline {
    saves: Arc<[(f64, CsoundSnapshot)]>,
    current: usize,
}

#[derive(Clone, Default)]
pub(crate) struct State(Arc<Mutex<Store>>);

pub(crate) struct Candidate {
    snapshot: CsoundSnapshot,
    program: Option<Program>,
}

impl State {
    pub(crate) fn new(allowed: bool) -> Self {
        Self(Arc::new(Mutex::new(Store {
            allowed,
            ..Store::default()
        })))
    }

    pub(crate) fn set_allowed(&self, allowed: bool) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .allowed = allowed;
    }

    fn require_permission(&self) -> Result<(), RuntimeError> {
        if self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .allowed
        {
            Ok(())
        } else {
            Err(RuntimeError::CsoundPermission)
        }
    }

    pub(crate) fn for_render(&self) -> Self {
        let store = self.0.lock().unwrap_or_else(|error| error.into_inner());
        Self(Arc::new(Mutex::new(Store {
            allowed: store.allowed,
            snapshot: store.snapshot.clone(),
            render_timeline: store.render_timeline.clone(),
            ..Store::default()
        })))
    }

    pub(crate) fn with_render_timeline(&self, saves: Vec<(f64, CsoundSnapshot)>) -> Self {
        let store = self.0.lock().unwrap_or_else(|error| error.into_inner());
        Self(Arc::new(Mutex::new(Store {
            allowed: store.allowed,
            snapshot: saves
                .first()
                .map(|(_, snapshot)| snapshot)
                .unwrap_or(&store.snapshot)
                .clone(),
            render_timeline: (!saves.is_empty()).then(|| RenderTimeline {
                saves: saves.into(),
                current: 0,
            }),
            ..Store::default()
        })))
    }

    pub(crate) fn snapshot(&self) -> CsoundSnapshot {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot
            .clone()
    }

    pub(crate) fn restore(&self, snapshot: &CsoundSnapshot) {
        self.commit(Some(Candidate {
            snapshot: snapshot.clone(),
            program: None,
        }));
    }

    pub(crate) fn stage(
        &self,
        effects: &[CsoundSource],
        access: &ScoreImportAccess,
    ) -> Result<Option<Candidate>, RuntimeError> {
        if effects.is_empty() {
            return Ok(None);
        }
        self.stage_from_snapshot(&self.snapshot(), effects, access)
    }

    pub(crate) fn stage_from_snapshot(
        &self,
        accepted: &CsoundSnapshot,
        effects: &[CsoundSource],
        access: &ScoreImportAccess,
    ) -> Result<Option<Candidate>, RuntimeError> {
        if !effects.is_empty() {
            self.require_permission()?;
        }
        let mut snapshot = accepted.clone();
        let data = Arc::make_mut(&mut snapshot.0);
        for effect in effects {
            let source = match effect {
                CsoundSource::Code(source) => source.clone(),
                CsoundSource::Url(specifier) => {
                    if data.urls.contains(specifier) {
                        continue;
                    }
                    let url = specifier
                        .strip_prefix("github:")
                        .map(|path| format!("https://raw.githubusercontent.com/{path}"))
                        .unwrap_or_else(|| specifier.clone());
                    let source = if url.starts_with("https:") || url.starts_with("http:") {
                        let url = access.approve(&url).map_err(|error| match error {
                            ImportRefusal::Untrusted { origin } => {
                                RuntimeError::UntrustedImport { origin }
                            }
                            ImportRefusal::Invalid(message) => RuntimeError::Message(message),
                        })?;
                        crate::score_import::fetch(&url).map_err(RuntimeError::Message)?
                    } else {
                        access
                            .local_orchestra(&url)
                            .map_err(RuntimeError::Message)?
                    };
                    data.urls.insert(specifier.clone());
                    source
                }
            };
            if !source.is_empty() && data.sources.last() != Some(&source) {
                data.sources.retain(|earlier| earlier != &source);
                data.sources.push(source);
            }
        }
        data.initialized |= !effects.is_empty();
        if *data == *self.snapshot().0 {
            return Ok(None);
        }
        validate_snapshot_size(data)?;
        let program = if !effects.is_empty() {
            Some(Program::compile(&data.sources).map_err(RuntimeError::Message)?)
        } else {
            None
        };
        Ok(Some(Candidate { snapshot, program }))
    }

    pub(crate) fn commit(&self, candidate: Option<Candidate>) {
        let Some(candidate) = candidate else { return };
        let mut store = self.0.lock().unwrap_or_else(|error| error.into_inner());
        collect_errors(&mut store);
        prune_revisions(&mut store, None);
        if let Some(program) = &candidate.program {
            store.revisions.insert(program.key(), program.clone());
        }
        store.current = candidate.program;
        store.snapshot = candidate.snapshot;
    }

    pub(crate) fn score_changed(&self) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .chains
            .clear();
    }

    pub(crate) fn ensure_ready(&self) -> Result<(), String> {
        self.program().map(|_| ())
    }

    pub(crate) fn take_errors(&self) -> Vec<String> {
        let mut store = self.0.lock().unwrap_or_else(|error| error.into_inner());
        collect_errors(&mut store);
        let current = store.current.as_ref().map(Program::key);
        prune_revisions(&mut store, current);
        std::mem::take(&mut store.errors)
    }

    pub(crate) fn record_chain(&self, controls: &rustel_audio::OscillatorControls) {
        if controls
            .instrument
            .is_some_and(|instrument| instrument.score().is_some())
        {
            self.0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .chains
                .insert(controls.insert_chain());
        }
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn named_chains(&self) -> Vec<u64> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .chains
            .iter()
            .copied()
            .collect()
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn discard(&self, controls: &rustel_audio::OscillatorControls) {
        let Some((key, id)) = score_key(controls) else {
            return;
        };
        let mut store = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(prepared) = &mut store.prepared {
            prepared.remove(&(key, id));
        }
        if let Some(program) = store.revisions.get(&key) {
            program.discard(id);
        }
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn track_live_events(&self) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .prepared
            .get_or_insert_with(HashMap::new);
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn admit(
        &self,
        event: &rustel_audio::AudioEvent,
        sample_rate: u32,
        queued_through: f64,
        render_frontier: impl FnOnce() -> f64,
    ) {
        let Some(key) = score_key(&event.controls) else {
            return;
        };
        let deadline = (event.target_frame as f64 / f64::from(sample_rate))
            .max(queued_through)
            .max(render_frontier());
        let mut store = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(target) = store
            .prepared
            .as_mut()
            .and_then(|events| events.get_mut(&key))
        {
            *target = Some(deadline);
        }
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn retain_queued<'a>(
        &self,
        controls: impl Iterator<Item = &'a rustel_audio::OscillatorControls>,
    ) {
        let mut store = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let Some(mut prepared) = store.prepared.take() else {
            return;
        };
        if prepared.is_empty() {
            store.prepared = Some(prepared);
            return;
        }
        let queued: HashSet<_> = controls.filter_map(score_key).collect();
        prepared.retain(|key, target| {
            let keep = target.is_some() || queued.contains(key);
            if !keep && let Some(program) = store.revisions.get(&key.0) {
                program.discard(key.1);
            }
            keep
        });
        store.prepared = Some(prepared);
    }

    /// The completed event-intake clock proves that each older score ID was either
    /// taken by its worker or discarded by the output queue.
    #[cfg(feature = "device-audio")]
    pub(crate) fn reap_completed(&self, seconds: f64) {
        let mut store = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let Some(mut prepared) = store.prepared.take() else {
            return;
        };
        prepared.retain(|key, target| {
            let keep = target.is_none_or(|target| target >= seconds);
            if !keep && let Some(program) = store.revisions.get(&key.0) {
                program.discard(key.1);
            }
            keep
        });
        store.prepared = Some(prepared);
    }

    fn program(&self) -> Result<Program, String> {
        self.require_permission()
            .map_err(|error| error.to_string())?;
        let mut store = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(program) = &store.current {
            return Ok(program.clone());
        }
        let program = Program::compile(&store.snapshot.0.sources)?;
        store.revisions.insert(program.key(), program.clone());
        Arc::make_mut(&mut store.snapshot.0).initialized = true;
        store.current = Some(program.clone());
        Ok(program)
    }

    fn program_at(&self, target_time: f64) -> Result<Program, String> {
        let change = {
            let store = self.0.lock().unwrap_or_else(|error| error.into_inner());
            store.render_timeline.as_ref().and_then(|timeline| {
                let index = timeline
                    .saves
                    .partition_point(|(at, _)| *at <= target_time)
                    .saturating_sub(1);
                (index != timeline.current).then(|| (index, timeline.saves[index].1.clone()))
            })
        };
        if let Some((index, snapshot)) = change {
            self.restore(&snapshot);
            if let Some(timeline) = &mut self
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .render_timeline
            {
                timeline.current = index;
            }
        }
        self.program()
    }

    pub(crate) fn provider(&self, wait: bool) -> Arc<InsertProvider> {
        let state = self.clone();
        Arc::new(move |key, rate, slot| {
            if key.plugin == rustel_csound::PLUGIN_ID {
                let program = state
                    .0
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .revisions
                    .get(&key)
                    .cloned()?;
                return match program.instance(rate) {
                    Ok(instance) => Some(instance),
                    Err(message) => {
                        let mut store = state.0.lock().unwrap_or_else(|error| error.into_inner());
                        if store.errors.len() < 64 && !store.errors.contains(&message) {
                            store.errors.push(message);
                        }
                        None
                    }
                };
            }
            #[cfg(feature = "vst")]
            {
                crate::vst::started()?.insert(key, rate, slot, wait)
            }
            #[cfg(not(feature = "vst"))]
            {
                let _ = (slot, wait);
                None
            }
        })
    }
}

pub(crate) struct Lookup<'a> {
    pub(crate) samples: &'a dyn SampleLookup,
    pub(crate) state: &'a State,
}

impl SampleLookup for Lookup<'_> {
    fn resolve(&self, s: &str, n: f64, midi: f64) -> SampleResolution {
        self.samples.resolve(s, n, midi)
    }

    fn bus_names(&self) -> Option<&rustel_voice::BusNames> {
        self.samples.bus_names()
    }

    fn insert(
        &self,
        request: &rustel_voice::PluginRequest<'_>,
    ) -> Result<Option<InsertControls>, String> {
        self.samples.insert(request)
    }

    fn csound(&self, request: &CsoundRequest<'_>) -> Result<InsertControls, String> {
        let program = self.state.program_at(request.target_time)?;
        let event = score_event(request)?;
        let id = program.prepare(event)?;
        #[cfg(feature = "device-audio")]
        if let Some(prepared) = &mut self
            .state
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .prepared
        {
            prepared.insert((program.key(), id), None);
        }
        Ok(InsertControls::new(program.key()).with_score(id))
    }
}

pub(crate) struct ValidationLookup<'a>(pub(crate) &'a dyn SampleLookup);

impl SampleLookup for ValidationLookup<'_> {
    fn resolve(&self, s: &str, n: f64, midi: f64) -> SampleResolution {
        self.0.resolve(s, n, midi)
    }

    fn bus_names(&self) -> Option<&rustel_voice::BusNames> {
        self.0.bus_names()
    }

    fn insert(
        &self,
        request: &rustel_voice::PluginRequest<'_>,
    ) -> Result<Option<InsertControls>, String> {
        self.0.insert(request)
    }

    fn csound(&self, request: &CsoundRequest<'_>) -> Result<InsertControls, String> {
        score_event(request)?.validate()?;
        Ok(InsertControls::new(InsertKey::default()))
    }
}

fn collect_errors(store: &mut Store) {
    for program in store.revisions.values() {
        for message in program.take_errors() {
            if store.errors.len() < 64 && !store.errors.contains(&message) {
                store.errors.push(message);
            }
        }
    }
}

fn prune_revisions(store: &mut Store, current: Option<InsertKey>) {
    let mut retired = Vec::new();
    store.revisions.retain(|key, program| {
        let keep = Some(*key) == current
            || program.pending_events() != 0
            || program.active_instances() != 0;
        if !keep {
            retired.push(program.clone());
        }
        keep
    });
    for program in retired {
        for message in program.take_errors() {
            if store.errors.len() < 64 && !store.errors.contains(&message) {
                store.errors.push(message);
            }
        }
    }
}

#[cfg(feature = "device-audio")]
fn score_key(controls: &rustel_audio::OscillatorControls) -> Option<(InsertKey, u64)> {
    let instrument = controls.instrument?;
    Some((instrument.key, instrument.score()?))
}

fn score_event(request: &CsoundRequest<'_>) -> Result<Event, String> {
    let value = request.value;
    let midi = value.contains_key("csoundm");
    let instrument = value
        .get(if midi { "csoundm" } else { "csound" })
        .ok_or("Csound instrument is missing")?;
    let instrument = match instrument {
        Value::String(name) => Instrument::Named(name.clone()),
        Value::Number(number) if midi => {
            Instrument::Number(number.as_f64().ok_or("invalid Csound instrument")?)
        }
        _ => return Err("Csound instrument must be a name or a csoundm number".into()),
    };
    let number = |key: &str, default: f64| -> Result<f64, String> {
        match value.get(key) {
            None => Ok(default),
            Some(value) => control_number(value)
                .filter(|value| value.is_finite())
                .ok_or_else(|| format!("Csound {key} must be a finite number")),
        }
    };
    let (p4, p5, frequency_key, frequency) = if midi {
        (
            12.0 * (request.frequency / 261.62558).log2() + 60.0,
            127.0 * number("gain", 1.0)? * number("velocity", 0.9)?,
            "frequency",
            request.frequency,
        )
    } else {
        (
            request.frequency.round(),
            number("gain", 0.8)? * 0.2,
            "freq",
            request.frequency.round(),
        )
    };
    let mut controls = value.clone();
    controls.shift_remove("csound");
    controls.shift_remove("csoundm");
    controls.insert(frequency_key.into(), Value::from(frequency));
    Ok(Event {
        instrument,
        p4,
        p5,
        p6: control_map(&controls),
        duration: request.duration_secs,
    })
}

fn control_map(value: &Map<String, Value>) -> String {
    value
        .iter()
        .flat_map(|(key, value)| [key.clone(), control_text(value)])
        .collect::<Vec<_>>()
        .join("/")
}

fn control_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value
            .as_f64()
            .map_or_else(|| value.to_string(), |value| value.to_string()),
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    control_text(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

fn control_number(value: &Value) -> Option<f64> {
    match value {
        Value::Null => Some(0.0),
        Value::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
        Value::Number(value) => value.as_f64(),
        Value::String(_) | Value::Array(_) => {
            let text = control_text(value);
            let text = text.trim();
            if text.is_empty() {
                Some(0.0)
            } else {
                text.parse().ok()
            }
        }
        Value::Object(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "device-audio")]
    #[test]
    fn snapshot_budget_includes_source_capacity_and_cached_urls() {
        let empty = CsoundSnapshot::default();
        let baseline = empty.retained_snapshot_bytes(usize::MAX).unwrap();
        let mut source = String::with_capacity(4_096);
        source.push_str("instr probe\nendin");
        let snapshot = CsoundSnapshot(Arc::new(SnapshotData {
            sources: vec![source],
            urls: BTreeSet::from(["https://example.test/probe.orc".to_owned()]),
            initialized: true,
        }));
        let bytes = snapshot.retained_snapshot_bytes(usize::MAX).unwrap();
        assert!(bytes > baseline + 4_096);
        assert_eq!(snapshot.retained_snapshot_bytes(bytes), Some(bytes));
        assert_eq!(snapshot.retained_snapshot_bytes(bytes - 1), None);
        assert_eq!(empty.retained_snapshot_bytes(0), None);
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn pending_scores_survive_until_output_completes_or_the_backlog_drops_them() {
        let state = State::new(true);
        let program = match state.program() {
            Ok(program) => program,
            Err(error)
                if std::env::var_os("RUSTEL_CSOUND_LIBRARY").is_none()
                    && error.contains("Csound 6 is not available") =>
            {
                return;
            }
            Err(error) => panic!("native Csound failed: {error}"),
        };
        state.track_live_events();
        let lookup = Lookup {
            samples: &rustel_voice::BundledOnly,
            state: &state,
        };
        let value = serde_json::json!({"csound": "triangle"});
        let controls = || rustel_audio::OscillatorControls {
            instrument: Some(
                lookup
                    .csound(&CsoundRequest {
                        value: value.as_object().unwrap(),
                        frequency: 440.0,
                        duration_secs: 0.1,
                        target_time: 0.0,
                    })
                    .unwrap(),
            ),
            ..Default::default()
        };
        let queued = controls();
        let _refused = controls();
        assert_eq!(program.pending_events(), 2);
        state.retain_queued(std::iter::once(&queued));
        assert_eq!(program.pending_events(), 1);
        let event = rustel_audio::AudioEvent {
            onset_id: 1,
            generation: 1,
            ui_visuals: 0,
            target_frame: 8_000,
            onset_lead: 0.0,
            freq_hz: 440.0,
            gain: 1.0,
            duration_secs: 0.1,
            controls: queued,
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        };
        state.admit(&event, 8_000, 1.0, || 0.5);
        state.retain_queued(std::iter::empty());
        state.reap_completed(1.0);
        assert_eq!(program.pending_events(), 1);
        state.reap_completed(1.001);
        assert_eq!(program.pending_events(), 0);
        let late = rustel_audio::AudioEvent {
            controls: controls(),
            ..event
        };
        state.admit(&late, 8_000, 1.0, || 2.0);
        state.reap_completed(1.5);
        assert_eq!(program.pending_events(), 1);
        state.reap_completed(2.0);
        assert_eq!(program.pending_events(), 1);
        state.reap_completed(2.001);
        assert_eq!(program.pending_events(), 0);
        let held = rustel_audio::AudioEvent {
            controls: controls(),
            ..event
        };
        state.admit(&held, 8_000, 4.0, || 2.0);
        state.reap_completed(3.0);
        assert_eq!(program.pending_events(), 1);
        state.reap_completed(4.0);
        assert_eq!(program.pending_events(), 1);
        state.reap_completed(4.001);
        assert_eq!(program.pending_events(), 0);
        let dropped = controls();
        let empty = CsoundSnapshot::default();
        state.restore(&empty);
        assert_eq!(state.snapshot(), empty);
        {
            let store = state.0.lock().unwrap();
            assert!(store.current.is_none());
            assert!(store.revisions.contains_key(&program.key()));
        }
        assert_eq!(program.pending_events(), 1);
        state.discard(&dropped);
        assert_eq!(program.pending_events(), 0);
        assert!(state.take_errors().is_empty());
        assert!(state.0.lock().unwrap().revisions.is_empty());
    }

    #[test]
    fn score_conventions_preserve_custom_controls() {
        let value = serde_json::json!({"csound": "pad", "cutoff": 2400, "gain": "0.5", "optional": null, "values": [1,null,2]});
        let request = CsoundRequest {
            value: value.as_object().unwrap(),
            frequency: 440.4,
            duration_secs: 0.25,
            target_time: 0.0,
        };
        let event = score_event(&request).unwrap();
        assert_eq!(event.p4, 440.0);
        assert_eq!(event.p5, 0.1);
        assert_eq!(event.duration, 0.25);
        assert!(event.p6.contains("cutoff/2400"));
        assert!(event.p6.contains("freq/440"));
        assert!(!event.p6.contains("csound/"));
        assert!(event.p6.contains("optional//values/1,,2"));
        let value = serde_json::json!({"csoundm": 1.25, "gain": 0.5, "velocity": 0.8});
        let request = CsoundRequest {
            value: value.as_object().unwrap(),
            frequency: 440.4,
            duration_secs: 0.25,
            target_time: 0.0,
        };
        let event = score_event(&request).unwrap();
        assert!((event.p4 - 69.0157).abs() < 0.0001);
        assert!((event.p5 - 50.8).abs() < 0.0001);
        assert!(event.p6.contains("frequency/440.4"));
        assert!(!event.p6.contains("csoundm/"));
    }
}
