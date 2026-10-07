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
            Value::Str(s) => Self::String(s.clone()),
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
            whole: hap.whole.map(SpanJson::from_span),
            part: SpanJson::from_span(hap.part),
            value: ValueJson::from_value(&hap.value),
            context: hap.context.clone(),
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
    /// A thrown query's message. The CLI prints the report, then exits non-zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_threw: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OnsetEventJson {
    /// Private direct-control provenance; never part of query/report JSON.
    #[serde(skip)]
    pub live_controls: [u64; 2],
    pub onset_id: u64,
    pub generation: u64,
    pub whole_begin: String,
    /// Gate length derived from the exact whole span and the active CPS.
    pub duration_secs: f64,
    pub target_time: f64,
    pub value: ValueJson,
    pub value_show: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ui_visuals: u64,
    /// `.log()`'s text, printed when this onset fires. Absent for the events
    /// of a score that never asked to log anything, which is nearly all of
    /// them, so it stays out of the serialised form when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_line: Option<String>,
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
    /// The message from a query that threw, or from a lane whose throw a
    /// stack contained, if one did.
    ///
    /// A throw yields no haps, so the render succeeds and writes a file that
    /// is silent where the score is not. Carrying it lets the caller exit
    /// non-zero: a bounce nobody listened to must not pass for correct.
    /// Legitimate silence leaves this `None` and still exits zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_threw: Option<String>,
    pub onsets: Vec<OnsetEventJson>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RenderReport {
    pub duration_secs: f64,
    pub cps: f64,
    pub sample_rate: u32,
    pub channels: u16,
    pub format: String,
    pub path: String,
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
