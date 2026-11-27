//! Shared immediate sample and synth audition path for Studio and CLI editors.
//!
//! Audition voices bypass score evaluation, file watching, and cycle-boundary
//! cutovers. They resolve through `rustel_voice::resolve_voice_with_samples`
//! and push directly into [`LiveScalarDevice`] on the device's current
//! generation using reserved choke groups at the top of the `f32` range so
//! previews never collide with score voices or alter transport state.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rustel_audio::{
    AudioEvent, BUNDLED_BD_SAMPLE_ID, DecodedSample, LiveReverbBatch, LiveScalarDevice, SampleId,
};

use crate::RuntimeError;
use crate::samples::{SampleLibrary, ScoreSampleAccess, SourceState};

/// How far ahead of the device clock a preview is placed.
pub const AUDITION_LEAD_SECS: f64 = 0.06;

/// How far ahead of its own moment a scale run's note is handed to the device.
pub const AUDITION_RUN_LEAD_SECS: f64 = 0.15;

/// Default gate length of a single preview; one-shot samples play to their end
/// unless an explicit duration limit is requested.
pub const AUDITION_GATE_SECS: f64 = 2.0;

/// Default gate length for a chord preview so notes overlap clearly.
pub const AUDITION_CHORD_GATE_SECS: f64 = 2.6;

/// How long a single preview waits for its sample or manifest to finish loading.
pub const AUDITION_LOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a chord preview waits for its bank to finish loading.
pub const CHORD_AUDITION_LOAD_TIMEOUT: Duration = Duration::from_secs(4);

/// Maximum simultaneous notes in one chord audition.
pub const MAX_AUDITION_NOTES: usize = 8;

/// Maximum sequential notes in one scale run audition.
pub const MAX_AUDITION_RUN_NOTES: usize = 24;

/// Maximum explicit duration accepted for one audition request.
pub const MAX_AUDITION_DURATION_SECS: f64 = 30.0;

/// Minimum explicit duration accepted for one audition request.
pub const MIN_AUDITION_DURATION_SECS: f64 = 0.01;

/// Maximum linear gain accepted for one audition request.
pub const MAX_AUDITION_GAIN: f32 = 4.0;

/// Visual-analysis slot reserved for audition previews (the highest slot).
pub const AUDITION_UI_VISUAL_SLOT: u8 = 63;

/// Visual-analysis bitmask for [`AUDITION_UI_VISUAL_SLOT`].
pub const AUDITION_VISUALS: u64 = 1 << AUDITION_UI_VISUAL_SLOT;

/// Reserved choke group for audition slot `slot` at the top of the `f32` range
/// where no score-authored `.cut(...)` group collides.
pub fn audition_cut_group(slot: usize) -> f32 {
    f32::from_bits(f32::MAX.to_bits() - slot.min(MAX_AUDITION_RUN_NOTES) as u32)
}

/// Device frame nearest `seconds`, clamped to at least zero.
pub fn audition_frame_at(seconds: f64, sample_rate: u32) -> u64 {
    (seconds * f64::from(sample_rate)).round().max(0.0) as u64
}

/// Split `sound` into `(bank_name, variant_index)`, respecting an explicit
/// `variant_override` when supplied.
pub fn parse_sound_spec(sound: &str, variant_override: Option<f64>) -> (&str, f64) {
    let (name, parsed_index) = match sound.rsplit_once(':') {
        Some((head, tail)) if !head.is_empty() => match tail.parse::<f64>() {
            Ok(index) if index.is_finite() => (head, index),
            _ => (sound, 0.0),
        },
        _ => (sound, 0.0),
    };
    let index = variant_override
        .filter(|value| value.is_finite())
        .unwrap_or(parsed_index);
    (name, index)
}

/// Parse one audition pitch given as a finite MIDI number or note name such as
/// `c4`, `f#3`, or `bb2`.
pub fn parse_audition_note(text: &str) -> Result<f32, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("audition note cannot be empty".to_owned());
    }
    if let Ok(midi) = trimmed.parse::<f64>() {
        if midi.is_finite() && (0.0..=127.0).contains(&midi) {
            return Ok(midi as f32);
        }
        return Err(format!("MIDI note {trimmed:?} must be within 0..=127"));
    }
    let Ok(midi) = rustel_core::util::note_to_midi(trimmed, 3) else {
        return Err(format!("invalid note {trimmed:?}"));
    };
    if midi.is_finite() && (0.0..=127.0).contains(&midi) {
        Ok(midi as f32)
    } else {
        Err(format!("note {trimmed:?} is outside MIDI range 0..=127"))
    }
}

/// Parameters for one immediate audition preview.
#[derive(Clone, Debug)]
pub struct AuditionRequest {
    pub sound: String,
    /// Optional explicit variant index override (`n`). When `None`, a `:index`
    /// suffix on `sound` is used (default `0`).
    pub variant: Option<f64>,
    /// MIDI numbers, 60 being middle C. Empty plays the sound itself.
    pub notes: Vec<f32>,
    pub gain: f32,
    /// Explicit duration cap in seconds. Sets `clip: 1.0` so long one-shot
    /// samples also stop cleanly at this duration. `None` uses the default
    /// audition gate while letting one-shot samples play to their end.
    pub duration_secs: Option<f64>,
    /// Seconds between one note and the next. Zero plays a chord; non-zero
    /// plays a sequential run.
    pub step_secs: f64,
    /// First choke slot in the reserved audition block.
    pub slot: usize,
    /// Target device clock second (`None` places it at `clock + AUDITION_LEAD_SECS`).
    pub at: Option<f64>,
    /// Visual-analysis slot mask (`AUDITION_VISUALS` in Studio, `0` in CLI).
    pub ui_visuals: u64,
}

impl AuditionRequest {
    /// Build a single-sound audition request with default gain and gate.
    pub fn sound(sound: impl Into<String>) -> Self {
        Self {
            sound: sound.into(),
            variant: None,
            notes: Vec::new(),
            gain: 1.0,
            duration_secs: None,
            step_secs: 0.0,
            slot: 0,
            at: None,
            ui_visuals: 0,
        }
    }
}

/// Resolved audio events ready to push into a [`LiveScalarDevice`].
#[derive(Debug)]
pub struct PreparedAudition {
    pub events: Vec<AudioEvent>,
    pub first_time: f64,
    pub horizon_frame: u64,
    pub used_samples: Vec<SampleId>,
    pub clear_tail_from: usize,
}

/// Result of resolving an [`AuditionRequest`] against the sample library.
#[derive(Debug)]
pub enum AuditionOutcome {
    Ready(PreparedAudition),
    Loading,
}

/// Resolve `request` into ready-to-push [`AudioEvent`]s without blocking.
///
/// Returns [`AuditionOutcome::Loading`] when the sample is still fetching or
/// decoding, or when its decoded PCM has not yet been installed into
/// `retained_samples`.
pub fn prepare_audition(
    request: &AuditionRequest,
    library: &SampleLibrary,
    retained_samples: &HashMap<SampleId, DecodedSample>,
    sample_rate: u32,
    clock_seconds: f64,
    generation: u64,
    cps: f64,
) -> Result<AuditionOutcome, RuntimeError> {
    let (name, index) = parse_sound_spec(&request.sound, request.variant);
    let build_value = |note: Option<f32>| {
        let mut map = serde_json::Map::new();
        map.insert("s".into(), serde_json::Value::from(name));
        map.insert("n".into(), serde_json::Value::from(index));
        if let Some(note) = note {
            map.insert("note".into(), serde_json::Value::from(note));
        }
        if request.duration_secs.is_some() {
            map.insert("clip".into(), serde_json::Value::from(1.0));
        }
        serde_json::Value::Object(map)
    };
    let values: Vec<serde_json::Value> = if request.notes.is_empty() {
        vec![build_value(None)]
    } else {
        request
            .notes
            .iter()
            .copied()
            .map(|note| build_value(Some(note)))
            .collect()
    };

    let gate = if let Some(duration) = request.duration_secs {
        duration
    } else if request.step_secs > 0.0 {
        (request.step_secs * 5.0).clamp(0.45, 0.9)
    } else if request.notes.is_empty() {
        AUDITION_GATE_SECS
    } else {
        AUDITION_CHORD_GATE_SECS
    };

    // Split gain across simultaneous chord notes so chords sit near the same
    // loudness as a single hit; runs play one note at a time.
    let spread = if request.step_secs > 0.0 {
        1.0
    } else {
        1.0 / (values.len() as f32).sqrt()
    };

    let earliest = clock_seconds + AUDITION_LEAD_SECS;
    let first_time = request.at.map_or(earliest, |at| at.max(earliest));
    let mut events = Vec::with_capacity(values.len());
    let mut used_samples = Vec::new();

    for (step, value) in values.iter().enumerate() {
        let target_time = first_time + step as f64 * request.step_secs;
        let onset = match rustel_voice::resolve_voice_with_samples(
            value,
            u64::MAX,
            gate,
            target_time,
            sample_rate,
            cps,
            library,
        ) {
            Ok(onset) => onset,
            Err(message) if message.contains("still loading") => {
                return Ok(AuditionOutcome::Loading);
            }
            Err(message) => return Err(RuntimeError::Message(message)),
        };

        // Wait until the device bank actually holds the decoded sample PCM.
        // The bundled bd slot is pre-installed at stream open.
        if let Some(sample) = &onset.sample {
            if sample.sample != BUNDLED_BD_SAMPLE_ID
                && !retained_samples.contains_key(&sample.sample)
            {
                return Ok(AuditionOutcome::Loading);
            }
            used_samples.push(sample.sample);
        }

        events.push(AudioEvent {
            onset_id: u64::MAX,
            generation,
            target_frame: onset.onset_frame,
            onset_lead: onset.onset_lead,
            freq_hz: onset.freq_hz,
            gain: onset.gain * request.gain * spread,
            duration_secs: onset.duration_secs,
            ui_visuals: request.ui_visuals,
            controls: onset.controls,
            sample: onset.sample,
            wavetable: onset.wavetable,
            synth: onset.synth,
            cut: Some(audition_cut_group(request.slot + step)),
        });
    }

    let rate_u64 = u64::from(sample_rate);
    let horizon_frame = events
        .iter()
        .map(|event| {
            let duration_frames = (event.duration_secs.max(0.0) * rate_u64 as f32).ceil() as u64;
            event.target_frame.saturating_add(duration_frames)
        })
        .max()
        .unwrap_or(0);

    let clear_tail_from = if request.step_secs > 0.0 {
        MAX_AUDITION_RUN_NOTES + 1
    } else {
        values.len()
    };

    Ok(AuditionOutcome::Ready(PreparedAudition {
        events,
        first_time,
        horizon_frame,
        used_samples,
        clear_tail_from,
    }))
}

/// Push a resolved [`PreparedAudition`] into `device`, clearing unused choke
/// slots from any wider prior chord preview.
pub fn push_prepared_audition(
    device: &LiveScalarDevice,
    prepared: &PreparedAudition,
    ui_visuals: u64,
) -> Result<(), RuntimeError> {
    let mut reverbs = LiveReverbBatch::default();
    for event in &prepared.events {
        reverbs
            .observe(device, event)
            .map_err(RuntimeError::ResourceLimit)?;
    }
    reverbs.flush(device);

    let tail_frame = audition_frame_at(prepared.first_time, device.sample_rate());
    for slot in prepared.clear_tail_from..=MAX_AUDITION_RUN_NOTES {
        let _ = device.push(AudioEvent {
            onset_id: u64::MAX,
            generation: device.generation(),
            target_frame: tail_frame,
            onset_lead: 0.0,
            freq_hz: 440.0,
            gain: 0.0,
            duration_secs: 0.01,
            ui_visuals,
            controls: Default::default(),
            sample: None,
            wavetable: None,
            synth: None,
            cut: Some(audition_cut_group(slot)),
        });
    }

    for event in &prepared.events {
        if !device.push(event.clone()) {
            return Err(RuntimeError::Message(
                "the device is busy; try the preview again".into(),
            ));
        }
    }
    Ok(())
}

/// Push a single zero-gain choke event for audition `slot` at `target_frame`.
pub fn push_audition_choke_at(
    device: &LiveScalarDevice,
    slot: usize,
    target_frame: u64,
    ui_visuals: u64,
    choke_only: bool,
) {
    let _ = device.push(AudioEvent {
        onset_id: u64::MAX,
        generation: device.generation(),
        target_frame,
        onset_lead: 0.0,
        freq_hz: 440.0,
        gain: 0.0,
        duration_secs: 0.01,
        ui_visuals,
        controls: rustel_audio::OscillatorControls {
            choke_only,
            ..Default::default()
        },
        sample: None,
        wavetable: None,
        synth: None,
        cut: Some(audition_cut_group(slot)),
    });
}

/// Silence all audition choke slots on `device` without disturbing score voices.
/// Returns the target frame of the choke events.
pub fn push_audition_stop(device: &LiveScalarDevice, ui_visuals: u64) -> u64 {
    let target_time = device.clock_seconds() + AUDITION_LEAD_SECS;
    let target_frame = audition_frame_at(target_time, device.sample_rate());
    for slot in 0..=MAX_AUDITION_RUN_NOTES {
        push_audition_choke_at(device, slot, target_frame, ui_visuals, false);
    }
    target_frame
}

#[derive(Clone, Debug)]
struct PendingAudition {
    request: AuditionRequest,
    source: Option<String>,
    deadline: Instant,
}

/// Immediate outcome of submitting an audition request to [`AuditionTracker`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditionStart {
    /// Voices were pushed straight to the audio device.
    Played,
    /// Waiting asynchronously for a sample or manifest to finish loading.
    Loading,
}

/// Per-tick status returned by [`AuditionTracker::poll`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuditionPoll {
    Idle,
    Pending,
    Played,
    Failed { sound: String, message: String },
}

/// Non-blocking single-slot audition controller for producer loops.
///
/// A new audition request replaces any pending request that has not yet played
/// so moving rapidly through a sample browser previews only the latest selection.
#[derive(Default)]
pub struct AuditionTracker {
    pending: Option<PendingAudition>,
    horizon_frame: u64,
    last_duration_secs: f64,
}

impl AuditionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn horizon_frame(&self) -> u64 {
        self.horizon_frame
    }

    pub fn last_duration_secs(&self) -> f64 {
        self.last_duration_secs
    }

    /// Submit one audition request with an optional external `samples(...)`
    /// source spec. Never blocks on network or disk I/O.
    #[allow(clippy::too_many_arguments)]
    pub fn request(
        &mut self,
        request: AuditionRequest,
        source: Option<&str>,
        library: &SampleLibrary,
        access: &ScoreSampleAccess,
        device: &LiveScalarDevice,
        retained_samples: &HashMap<SampleId, DecodedSample>,
        cps: f64,
    ) -> Result<AuditionStart, RuntimeError> {
        let source = source
            .map(str::trim)
            .filter(|spec| !spec.is_empty())
            .map(str::to_owned);

        let wait = if request.notes.is_empty() {
            AUDITION_LOAD_TIMEOUT
        } else {
            CHORD_AUDITION_LOAD_TIMEOUT
        };

        if let Some(spec) = source.as_deref() {
            match library.samples_source_state(spec) {
                Some(SourceState::Ready) => {}
                Some(SourceState::Failed(reason))
                    if !library.samples_source_rested(spec, false) =>
                {
                    return Err(RuntimeError::Message(format!("{spec}: {reason}")));
                }
                _ => {
                    library
                        .look_up_samples_source(spec, access)
                        .map_err(RuntimeError::Message)?;
                    match library.samples_source_state(spec) {
                        Some(SourceState::Ready) => {}
                        Some(SourceState::Failed(reason)) => {
                            return Err(RuntimeError::Message(format!("{spec}: {reason}")));
                        }
                        Some(SourceState::Loading) | None => {
                            self.pending = Some(PendingAudition {
                                request,
                                source,
                                deadline: Instant::now() + wait,
                            });
                            return Ok(AuditionStart::Loading);
                        }
                    }
                }
            }
        }

        match prepare_audition(
            &request,
            library,
            retained_samples,
            device.sample_rate(),
            device.clock_seconds(),
            device.generation(),
            cps,
        ) {
            Ok(AuditionOutcome::Ready(prepared)) => {
                push_prepared_audition(device, &prepared, request.ui_visuals)?;
                let start_frame = audition_frame_at(prepared.first_time, device.sample_rate());
                self.horizon_frame = prepared.horizon_frame;
                self.last_duration_secs = prepared.horizon_frame.saturating_sub(start_frame) as f64
                    / f64::from(device.sample_rate().max(1));
                self.pending = None;
                Ok(AuditionStart::Played)
            }
            Ok(AuditionOutcome::Loading) => {
                self.pending = Some(PendingAudition {
                    request,
                    source,
                    deadline: Instant::now() + wait,
                });
                Ok(AuditionStart::Loading)
            }
            Err(error) => {
                // Default or source manifests may still be landing right after startup.
                if library.manifests_pending() > 0 {
                    self.pending = Some(PendingAudition {
                        request,
                        source,
                        deadline: Instant::now() + wait,
                    });
                    Ok(AuditionStart::Loading)
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Retry any pending audition once ready samples or manifests have landed.
    pub fn poll(
        &mut self,
        library: &SampleLibrary,
        device: &LiveScalarDevice,
        retained_samples: &HashMap<SampleId, DecodedSample>,
        cps: f64,
    ) -> AuditionPoll {
        let Some(pending) = self.pending.clone() else {
            return AuditionPoll::Idle;
        };

        if let Some(spec) = pending.source.as_deref() {
            match library.samples_source_state(spec) {
                Some(SourceState::Ready) => {}
                Some(SourceState::Failed(reason)) => {
                    self.pending = None;
                    return AuditionPoll::Failed {
                        sound: pending.request.sound,
                        message: format!("{spec}: {reason}"),
                    };
                }
                Some(SourceState::Loading) | None => {
                    if Instant::now() >= pending.deadline {
                        self.pending = None;
                        return AuditionPoll::Failed {
                            sound: pending.request.sound,
                            message: format!("{spec}: did not load in time to preview"),
                        };
                    }
                    return AuditionPoll::Pending;
                }
            }
        }

        match prepare_audition(
            &pending.request,
            library,
            retained_samples,
            device.sample_rate(),
            device.clock_seconds(),
            device.generation(),
            cps,
        ) {
            Ok(AuditionOutcome::Ready(prepared)) => {
                self.pending = None;
                match push_prepared_audition(device, &prepared, pending.request.ui_visuals) {
                    Ok(()) => {
                        let start_frame =
                            audition_frame_at(prepared.first_time, device.sample_rate());
                        self.horizon_frame = prepared.horizon_frame;
                        self.last_duration_secs = prepared.horizon_frame.saturating_sub(start_frame)
                            as f64
                            / f64::from(device.sample_rate().max(1));
                        AuditionPoll::Played
                    }
                    Err(error) => AuditionPoll::Failed {
                        sound: pending.request.sound,
                        message: error.to_string(),
                    },
                }
            }
            Ok(AuditionOutcome::Loading) => {
                if Instant::now() >= pending.deadline {
                    self.pending = None;
                    let message =
                        format!("{} did not load in time to preview", pending.request.sound);
                    AuditionPoll::Failed {
                        sound: pending.request.sound,
                        message,
                    }
                } else {
                    AuditionPoll::Pending
                }
            }
            Err(error) => {
                if library.manifests_pending() > 0 && Instant::now() < pending.deadline {
                    AuditionPoll::Pending
                } else {
                    self.pending = None;
                    AuditionPoll::Failed {
                        sound: pending.request.sound,
                        message: error.to_string(),
                    }
                }
            }
        }
    }

    /// Cancel any pending audition and silence active audition voices.
    pub fn stop(&mut self, device: &LiveScalarDevice, ui_visuals: u64) -> u64 {
        self.pending = None;
        let frame = push_audition_stop(device, ui_visuals);
        self.horizon_frame = frame;
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_spec_and_note_parsing_cover_variants_and_midi_names() {
        assert_eq!(parse_sound_spec("crate_sd:1", None), ("crate_sd", 1.0));
        assert_eq!(parse_sound_spec("bd", None), ("bd", 0.0));
        assert_eq!(parse_sound_spec("bd:2", Some(5.0)), ("bd", 5.0));

        assert_eq!(parse_audition_note("60").unwrap(), 60.0);
        assert_eq!(parse_audition_note("c4").unwrap(), 60.0);
        assert_eq!(parse_audition_note("a4").unwrap(), 69.0);
        assert!(parse_audition_note("999").is_err());
        assert!(parse_audition_note("not-a-note").is_err());
    }

    #[test]
    fn audition_transitions_from_loading_to_ready_and_respects_duration_clip() {
        let library = SampleLibrary::with_loading_sample_for_test("kick");
        let mut retained = HashMap::new();
        let mut request = AuditionRequest::sound("kick");
        request.duration_secs = Some(0.5);
        request.gain = 0.8;

        assert!(matches!(
            prepare_audition(&request, &library, &retained, 48_000, 1.0, 7, 0.5).unwrap(),
            AuditionOutcome::Loading
        ));

        let id = library.finish_loading_sample_frames_for_test(48_000);
        for (ready_id, decoded) in library.take_ready() {
            retained.insert(ready_id, decoded);
        }
        assert!(retained.contains_key(&id));

        let AuditionOutcome::Ready(prepared) =
            prepare_audition(&request, &library, &retained, 48_000, 1.0, 7, 0.5).unwrap()
        else {
            panic!("expected ready audition after sample installed");
        };
        assert_eq!(prepared.events.len(), 1);
        let event = &prepared.events[0];
        assert_eq!(event.generation, 7);
        assert_eq!(event.onset_id, u64::MAX);
        assert_eq!(event.cut, Some(audition_cut_group(0)));
        assert_eq!(
            event.sample.as_ref().map(|sample| sample.hold),
            Some(rustel_audio::SampleHold::Hap),
            "explicit duration must gate long samples via clip"
        );
        assert!((event.duration_secs - 0.5).abs() < 1e-4);
    }

    #[test]
    fn audition_tracker_plays_synth_immediately_and_stops_cleanly_on_silent_device() {
        let library = SampleLibrary::empty();
        let access = ScoreSampleAccess::denied();
        let device = LiveScalarDevice::start_output(Some(rustel_audio::SILENT_OUTPUT_NAME), 1)
            .expect("silent device");
        let retained = HashMap::new();
        let mut tracker = AuditionTracker::new();

        let mut req = AuditionRequest::sound("sine");
        req.notes = vec![60.0, 64.0, 67.0];
        req.duration_secs = Some(0.25);

        assert_eq!(
            tracker
                .request(req, None, &library, &access, &device, &retained, 0.5)
                .unwrap(),
            AuditionStart::Played
        );
        assert!(tracker.horizon_frame() > 0);

        let unknown = tracker.request(
            AuditionRequest::sound("definitely_missing_sound"),
            None,
            &library,
            &access,
            &device,
            &retained,
            0.5,
        );
        assert!(unknown.is_err());

        let stop_frame = tracker.stop(&device, 0);
        assert_eq!(tracker.horizon_frame(), stop_frame);
        assert!(!tracker.is_pending());

        device.stop_and_wait(Duration::from_millis(200));
    }
}
