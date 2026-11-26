//! Canonical JSON shapes for query / play / bench / render reports.

use rustel_core::{Hap, Value};
use rustel_fraction::Fraction;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SpanJson {
    pub begin: String,
    pub end: String,
}

impl SpanJson {
    pub fn from_span(span: rustel_core::TimeSpan) -> Self {
        Self {
            begin: span.begin.show(),
            end: span.end.show(),
        }
    }
}

/// JSON-friendly value: primitives stay typed; nested structures reuse the
/// Value's own `JSON.stringify` text when available.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ValueJson {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Raw(serde_json::Value),
}

impl ValueJson {
    pub fn from_value(value: &Value) -> Self {
        match value {
            Value::Undefined | Value::Null => Self::Null,
            Value::Bool(b) => Self::Bool(*b),
            Value::F64(n) => Self::Number(*n),
            Value::Str(s) => Self::String(s.to_string()),
            // A hap carrying a transformer is an intermediate of the join that
            // produced it; render its identity rather than dropping it.
            Value::Function(f) => Self::String(format!("{f:?}")),
            Value::Pattern(_) | Value::Haps(_) => match value.json_stringify() {
                Some(text) => serde_json::from_str(&text)
                    .map(Self::Raw)
                    .unwrap_or_else(|_| Self::String(text)),
                None => Self::Null,
            },
            Value::JsValue(_) => Self::from_value(&rustel_core::materialize_js_value(value)),
            Value::List(_) | Value::Object(_) => match value.json_stringify() {
                Some(text) => match serde_json::from_str(&text) {
                    Ok(v) => Self::Raw(v),
                    Err(_) => Self::String(text),
                },
                None => Self::Null,
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HapJson {
    pub whole: Option<SpanJson>,
    pub part: SpanJson,
    pub value: ValueJson,
    pub context: Vec<(usize, usize)>,
    pub has_onset: bool,
    /// Upstream-compatible compact show text for diffing against Node snapshots.
    pub show: String,
}

impl HapJson {
    pub fn from_hap(hap: &Hap) -> Self {
        Self {
            whole: hap.whole().map(SpanJson::from_span),
            part: SpanJson::from_span(hap.part()),
            value: ValueJson::from_value(&hap.value),
            context: hap.context.to_vec(),
            has_onset: hap.has_onset(),
            show: hap.show(),
        }
    }
}

pub fn haps_to_json(haps: &[Hap]) -> Vec<HapJson> {
    haps.iter().map(HapJson::from_hap).collect()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct QueryReport {
    pub begin: String,
    pub end: String,
    pub source: String,
    pub haps: Vec<HapJson>,
    /// A thrown query's message, or the failure that dropped every hap. The
    /// CLI prints the report, then exits non-zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_threw: Option<String>,
}

/// A `note` or `freq` no voice can sound: see [`non_finite_pitch`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NonFinitePitch {
    /// The control, `note` or `freq`.
    pub(crate) name: &'static str,
    /// NaN: a synth reads it as absent, and only a sample cannot play it.
    pub(crate) sample_only: bool,
}

/// The pitch control of `value` that stops its voice, as on strudel.cc. An
/// infinite `freq` stops every voice, and a finite one other than 0 leaves
/// `note` unread. An infinite `note` stops every voice. NaN is falsy to a
/// synth, which takes its default, and reaches a sample's playback rate,
/// which refuses it. A talkit voice reads its own pitch.
pub(crate) fn non_finite_pitch(value: &Value) -> Option<NonFinitePitch> {
    let materialized;
    let value = if matches!(value, Value::JsValue(_)) {
        materialized = rustel_core::materialize_js_value(value);
        &materialized
    } else {
        value
    };
    let object = value.as_object()?;
    if object.get("talkit").is_some() {
        return None;
    }
    let stops = |name, sample_only| Some(NonFinitePitch { name, sample_only });
    let freq = object.get("freq").and_then(Value::as_f64);
    let note = object.get("note").and_then(Value::as_f64);
    match freq {
        Some(freq) if freq.is_infinite() => return stops("freq", false),
        Some(freq) if freq != 0.0 && !freq.is_nan() => return None,
        _ => {}
    }
    if note.is_some_and(f64::is_infinite) {
        return stops("note", false);
    }
    if freq.is_some_and(f64::is_nan) {
        return stops("freq", true);
    }
    if note.is_some_and(f64::is_nan) {
        return stops("note", true);
    }
    None
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OnsetEventJson {
    /// Private direct-control provenance; never part of query/report JSON.
    #[serde(skip)]
    pub live_controls: [u64; 4],
    pub onset_id: u64,
    pub generation: u64,
    /// The begin of the onset's whole as `n/d`. The audio path reads the
    /// cycle of the onset from this text.
    pub whole_begin: String,
    /// Gate length derived from the exact whole span and the active CPS.
    pub duration_secs: f64,
    pub target_time: f64,
    /// The tempo, in cycles per second, that a pattern set for this onset.
    /// Absent when the onset plays at the session tempo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cps: Option<f64>,
    pub value: ValueJson,
    pub value_show: String,
    /// The `note` or `freq` that stops this onset's voice in a render. JSON
    /// writes NaN and infinities as null, which a voice reads as no pitch.
    /// A live update refuses such a score before it plays, and outputs that
    /// are no voice read the value as JSON writes it.
    #[serde(skip)]
    pub(crate) non_finite_pitch: Option<NonFinitePitch>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ui_visuals: u64,
    /// `.log()`'s text, printed when this onset fires. Absent for the events
    /// of a score that never asked to log anything, which is nearly all of
    /// them, so it stays out of the serialised form when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_line: Option<String>,
}

impl OnsetEventJson {
    /// The tempo of this onset: the one a pattern set for it, or else
    /// `session_cps`.
    pub fn cps_or(&self, session_cps: f64) -> f64 {
        self.cps.unwrap_or(session_cps)
    }
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PlayReport {
    pub duration_secs: f64,
    pub cps: f64,
    pub generation: u64,
    pub audio_backend: String,
    /// Device status for this operation (`not-requested` or `played`).
    pub device_audio: String,
    pub note: String,
    /// The message from a query that threw, from a lane whose throw a stack
    /// contained, or from a failure that dropped every note, if one did.
    ///
    /// A throw yields no haps, so the render succeeds and writes a file that
    /// is silent where the score is not. Carrying it lets the caller exit
    /// non-zero: a bounce nobody listened to must not pass for correct.
    /// Legitimate silence leaves this `None` and still exits zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_threw: Option<String>,
    pub onsets: Vec<OnsetEventJson>,
}

/// What a MIDI render wrote.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MidiExportReport {
    pub ppq: u16,
    pub tempo_bpm: f64,
    pub meter: String,
    pub tracks: usize,
    pub notes: usize,
    pub messages: usize,
    pub skipped_unsupported: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_names: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RenderReport {
    pub duration_secs: f64,
    pub cps: f64,
    pub sample_rate: u32,
    pub channels: u16,
    pub format: String,
    pub path: String,
    /// Present only for a MIDI file render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi: Option<MidiExportReport>,
    pub audio_backend: String,
    pub device_audio: String,
    pub note: String,
    /// The message from a query that threw, if one did. See
    /// `PlayReport::query_threw`: a bounce whose pattern threw is silent, not
    /// correct, and the caller exits non-zero on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_threw: Option<String>,
    /// A session bounce's first save that the tape marks as installed but
    /// that failed to evaluate on replay, with its error. The previous save
    /// played through that window in its place, so the file is not the set
    /// the tape recorded, and the caller exits non-zero on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_save: Option<String>,
    pub onset_count: usize,
    /// Each distinct refusal of a note the render dropped: a sound it could
    /// not find or load, a control the engine refuses, a plugin that did
    /// not load or start, a plugin copy with no number in its output for
    /// 1 s of play, or a note a plugin copy lost. The file is silent where
    /// those notes are. A note of a plugin that did not start plays with no
    /// plugin, and a copy with no number plays silence to the end of the
    /// render. The diagnostic policy still reports each one as a
    /// `voice-refused` diagnostic.
    /// [`Self::failure`] leaves them out: a Studio export fails on them, and
    /// `rustel export` lists them and succeeds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voices_refused: Vec<String>,
    /// The plugin lines of the render that refuse no note: a plugin chain
    /// with more latency than the render compensates, whose lane plays the
    /// rest late, and the counts of a plugin copy: a sample of its output
    /// that was no number and played as silence, a block the plugin refused
    /// with such a sample, and a value lost to too many values at one time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_lines: Vec<String>,
    /// What the bounce says of each chord it could not voice. It is silent,
    /// as on strudel.cc, which logs it and plays on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_chords: Vec<String>,
}

impl RenderReport {
    /// Why this bounce must not report success, if it must not.
    ///
    /// The file is written either way - a partial bounce is more use than
    /// none - but it is not the score: a script that only checks the exit
    /// status would call it a success. Every caller that writes a bounce must
    /// fail on this, in these words.
    pub fn failure(&self) -> Option<String> {
        if let Some(save) = &self.failed_save {
            return Some(format!(
                "a save that installed live failed to evaluate on replay, so the \
                 previous save plays in its place - {save}"
            ));
        }
        self.query_threw.as_ref().map(|message| {
            // Later windows still schedule notes after a thrown window.
            format!("the pattern threw while querying, so part of the bounce is silent: {message}")
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BenchMetrics {
    pub source: String,
    pub begin: String,
    pub end: String,
    pub iterations: u64,
    pub total_haps: u64,
    pub haps_per_iteration: f64,
    pub elapsed_secs: f64,
    pub queries_per_sec: f64,
    pub haps_per_sec: f64,
}

pub fn fraction_label(f: Fraction) -> String {
    f.show()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controls(entries: &[(&str, Value)]) -> Value {
        let mut map = rustel_core::OrderedMap::default();
        for (name, value) in entries {
            map.insert((*name).into(), value.clone());
        }
        Value::Object(map)
    }

    /// The rules of strudel.cc, for a synth and for a sample.
    #[test]
    fn the_pitch_that_stops_a_voice_follows_strudel() {
        let nan = Value::F64(f64::NAN);
        let inf = Value::F64(f64::INFINITY);
        let low = Value::F64(f64::NEG_INFINITY);
        let stops = |name, sample_only| Some(NonFinitePitch { name, sample_only });
        for (entries, judged) in [
            (vec![("note", nan.clone())], stops("note", true)),
            (vec![("note", low)], stops("note", false)),
            (vec![("freq", inf.clone())], stops("freq", false)),
            (
                vec![("note", inf.clone()), ("freq", Value::F64(440.0))],
                None,
            ),
            (
                vec![("note", Value::F64(60.0)), ("freq", nan.clone())],
                stops("freq", true),
            ),
            (
                vec![("note", inf.clone()), ("freq", nan.clone())],
                stops("note", false),
            ),
            (
                vec![("note", nan.clone()), ("talkit", Value::Str("la".into()))],
                None,
            ),
            (vec![("note", Value::F64(60.0)), ("gain", inf)], None),
        ] {
            assert_eq!(non_finite_pitch(&controls(&entries)), judged, "{entries:?}");
        }
    }
}
