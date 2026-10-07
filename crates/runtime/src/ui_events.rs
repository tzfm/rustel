//! Versioned, line-delimited UI events for editor and terminal clients.
//!
//! The protocol deliberately carries source locations as UTF-8 byte offsets:
//! that is the representation produced by the transpiler and consumed by
//! text editors and terminal clients. Exact cycle positions stay rational
//! strings while
//! wall-clock scheduling values are finite JSON numbers.

use rustel_scheduler::ScheduleTraceEvent;
use rustel_transpiler::{
    ALL_VISUAL_WIDGET_PREFIX, TranspileOptions, VISUAL_WIDGET_METHODS, transpile,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::ui_analysis::{
    UI_SCOPE_SAMPLES, UI_SPECTRUM_BINS, UiAudioAnalysisFrame, UiAudioAnalysisSet,
};

/// Current schema version of the object nested below `ui_events`.
pub const UI_EVENT_PROTOCOL_VERSION: u16 = 2;

/// Default number of complete batches waiting for the background writer.
///
/// The producer uses `try_send`, so this is a memory bound rather than a
/// source of backpressure into scheduling or audio.
pub const DEFAULT_UI_EVENT_QUEUE_CAPACITY: usize = 8;

/// Maximum onsets in one NDJSON line. Small records bound both writer memory
/// and the amount of work an editor must perform in one event-loop turn.
pub const MAX_UI_EVENTS_PER_BATCH: usize = 256;

/// Maximum source ranges retained for one onset.
pub const MAX_UI_EVENT_CONTEXT_RANGES: usize = rustel_scheduler::MAX_TRACE_CONTEXT_RANGES;

/// Display values are useful labels, not required for correct highlighting.
/// Omit an unusually large one rather than inflate every queued batch with it.
pub const MAX_UI_EVENT_VALUE_BYTES: usize = rustel_scheduler::MAX_TRACE_TEXT_BYTES;

/// Current schema version of the object nested below `ui_layout`.
pub const UI_LAYOUT_PROTOCOL_VERSION: u16 = 2;

/// A score can request several independent inline painters, but the layout
/// snapshot must remain cheap for an editor to replace atomically.
pub const MAX_UI_LAYOUT_VISUALS: usize = 64;

/// Interactive score controls are source-bound like painters and use the same
/// deliberately small per-layout ceiling. A client may redraw or replace the
/// complete set without accepting an unbounded score-authored allocation.
pub const MAX_UI_LAYOUT_SLIDERS: usize = 64;

/// Maximum raw JavaScript option bytes retained for one visual call.
pub const MAX_UI_LAYOUT_OPTIONS_BYTES: usize = 4 * 1024;

/// Maximum mini-notation leaf ranges in one installed score layout.
pub const MAX_UI_LAYOUT_MINI_LOCATIONS: usize = 4 * 1024;

/// Generated IDs and canonical kinds are small; explicit caps also keep a
/// deserialized untrusted record bounded before an editor uses either as a key.
pub const MAX_UI_LAYOUT_ID_BYTES: usize = 256;
pub const MAX_UI_LAYOUT_KIND_BYTES: usize = 32;

/// Current schema version of the object nested below `ui_audio`.
pub const UI_AUDIO_PROTOCOL_VERSION: u16 = 1;

/// A defensive ceiling for deserialized telemetry. The device API currently
/// reports ordinary hardware rates, but accepting an arbitrary integer makes
/// downstream frequency-axis arithmetic unnecessarily fragile.
pub const MAX_UI_AUDIO_SAMPLE_RATE: u32 = 768_000;

/// Idle writer polling interval for the separate latest-audio slot. Audio is
/// deliberately lower priority than queued layout/onset control records.
const UI_AUDIO_WRITER_POLL: Duration = Duration::from_millis(10);

/// Revision identifier carried by the protocol: lowercase SHA-256 of the
/// exact UTF-8 score bytes evaluated for this generation.
pub fn source_revision(source: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(source.as_bytes());
    let mut revision = String::with_capacity(digest.len() * 2);
    for byte in digest {
        revision.push(char::from(HEX[usize::from(byte >> 4)]));
        revision.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    revision
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// One installed score's visual call sites and persistent mini-token ranges.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UiLayoutEnvelope {
    pub ui_layout: UiLayout,
}

impl UiLayoutEnvelope {
    pub fn new(layout: UiLayout) -> Self {
        Self { ui_layout: layout }
    }

    pub fn validate(&self) -> Result<(), UiLayoutValidationError> {
        self.ui_layout.validate()
    }
}

/// Revision-gated layout metadata. It contains no evaluated values and no
/// renderer state, so clients can replace the complete snapshot on reload.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UiLayout {
    pub version: u16,
    pub generation: u64,
    pub source_revision: String,
    pub visuals: Vec<UiVisual>,
    #[serde(default)]
    pub sliders: Vec<UiSlider>,
    pub mini_locations: Vec<(usize, usize)>,
}

impl UiLayout {
    pub fn new(
        generation: u64,
        source_revision: String,
        visuals: Vec<UiVisual>,
        sliders: Vec<UiSlider>,
        mini_locations: Vec<(usize, usize)>,
    ) -> Result<Self, UiLayoutValidationError> {
        let layout = Self {
            version: UI_LAYOUT_PROTOCOL_VERSION,
            generation,
            source_revision,
            visuals,
            sliders,
            mini_locations,
        };
        layout.validate()?;
        Ok(layout)
    }

    pub fn validate(&self) -> Result<(), UiLayoutValidationError> {
        if self.version != UI_LAYOUT_PROTOCOL_VERSION {
            return Err(UiLayoutValidationError::UnsupportedVersion(self.version));
        }
        validate_source_revision(&self.source_revision)
            .map_err(|_| UiLayoutValidationError::InvalidSourceRevision)?;
        if self.visuals.len() > MAX_UI_LAYOUT_VISUALS {
            return Err(UiLayoutValidationError::TooManyVisuals(self.visuals.len()));
        }
        if self.sliders.len() > MAX_UI_LAYOUT_SLIDERS {
            return Err(UiLayoutValidationError::TooManySliders(self.sliders.len()));
        }
        if self.mini_locations.len() > MAX_UI_LAYOUT_MINI_LOCATIONS {
            return Err(UiLayoutValidationError::TooManyMiniLocations(
                self.mini_locations.len(),
            ));
        }
        for (index, visual) in self.visuals.iter().enumerate() {
            visual.validate(index)?;
        }
        for (index, slider) in self.sliders.iter().enumerate() {
            slider.validate(index)?;
        }
        for (index, &(start, end)) in self.mini_locations.iter().enumerate() {
            if start >= end {
                return Err(UiLayoutValidationError::InvalidMiniLocation { index, start, end });
            }
        }
        Ok(())
    }
}

/// One numeric `slider(value, min?, max?, step?)` bound to the exact bytes of
/// its first argument. Values are data only: editor clients never evaluate
/// score text to construct a control.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UiSlider {
    pub id: String,
    pub from: usize,
    pub to: usize,
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

impl UiSlider {
    fn validate(&self, index: usize) -> Result<(), UiLayoutValidationError> {
        if self.id.is_empty() || self.id.len() > MAX_UI_LAYOUT_ID_BYTES {
            return Err(UiLayoutValidationError::InvalidSliderId {
                index,
                bytes: self.id.len(),
            });
        }
        if self.from >= self.to {
            return Err(UiLayoutValidationError::InvalidSliderRange {
                index,
                from: self.from,
                to: self.to,
            });
        }
        if !self.value.is_finite()
            || !self.min.is_finite()
            || !self.max.is_finite()
            || self.min >= self.max
            || self.value < self.min
            || self.value > self.max
            || !self.step.is_finite()
            || self.step <= 0.0
        {
            return Err(UiLayoutValidationError::InvalidSliderNumbers { index });
        }
        Ok(())
    }
}

/// One visual directive attached to an exact source call.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiVisual {
    pub id: String,
    pub kind: String,
    pub inline: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u8>,
    pub from: usize,
    pub to: usize,
    /// Original source between the method call's parentheses. It is data for
    /// the UI, never code for this protocol layer to evaluate.
    pub options: String,
}

impl UiVisual {
    fn validate(&self, index: usize) -> Result<(), UiLayoutValidationError> {
        if self.id.is_empty() || self.id.len() > MAX_UI_LAYOUT_ID_BYTES {
            return Err(UiLayoutValidationError::InvalidVisualId {
                index,
                bytes: self.id.len(),
            });
        }
        if self.kind.is_empty()
            || self.kind.len() > MAX_UI_LAYOUT_KIND_BYTES
            || !matches!(
                self.kind.as_str(),
                "punchcard"
                    | "pianoroll"
                    | "wordfall"
                    | "spiral"
                    | "scope"
                    | "tscope"
                    | "pitchwheel"
                    | "spectrum"
                    | "markcss"
            )
        {
            return Err(UiLayoutValidationError::InvalidVisualKind {
                index,
                kind: self.kind.clone(),
            });
        }
        if self.from >= self.to {
            return Err(UiLayoutValidationError::InvalidVisualRange {
                index,
                from: self.from,
                to: self.to,
            });
        }
        if self
            .slot
            .is_some_and(|slot| usize::from(slot) >= MAX_UI_LAYOUT_VISUALS)
        {
            return Err(UiLayoutValidationError::InvalidVisualSlot { index });
        }
        if self.options.len() > MAX_UI_LAYOUT_OPTIONS_BYTES {
            return Err(UiLayoutValidationError::OversizedVisualOptions {
                index,
                bytes: self.options.len(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiLayoutValidationError {
    UnsupportedVersion(u16),
    InvalidSourceRevision,
    TooManyVisuals(usize),
    TooManySliders(usize),
    TooManyMiniLocations(usize),
    InvalidVisualId {
        index: usize,
        bytes: usize,
    },
    InvalidVisualKind {
        index: usize,
        kind: String,
    },
    InvalidVisualRange {
        index: usize,
        from: usize,
        to: usize,
    },
    InvalidVisualSlot {
        index: usize,
    },
    OversizedVisualOptions {
        index: usize,
        bytes: usize,
    },
    InvalidSliderId {
        index: usize,
        bytes: usize,
    },
    InvalidSliderRange {
        index: usize,
        from: usize,
        to: usize,
    },
    InvalidSliderNumbers {
        index: usize,
    },
    InvalidMiniLocation {
        index: usize,
        start: usize,
        end: usize,
    },
    SourceRangeOutsideScore {
        index: usize,
        end: usize,
        source_bytes: usize,
    },
}

impl fmt::Display for UiLayoutValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported UI layout protocol version {version}"
                )
            }
            Self::InvalidSourceRevision => {
                write!(
                    formatter,
                    "UI layout source revision is not lowercase SHA-256"
                )
            }
            Self::TooManyVisuals(count) => write!(
                formatter,
                "UI layout has {count} visuals; maximum is {MAX_UI_LAYOUT_VISUALS}"
            ),
            Self::TooManySliders(count) => write!(
                formatter,
                "UI layout has {count} sliders; maximum is {MAX_UI_LAYOUT_SLIDERS}"
            ),
            Self::TooManyMiniLocations(count) => write!(
                formatter,
                "UI layout has {count} mini locations; maximum is {MAX_UI_LAYOUT_MINI_LOCATIONS}"
            ),
            Self::InvalidVisualId { index, bytes } => write!(
                formatter,
                "UI layout visual {index} has an invalid {bytes}-byte id"
            ),
            Self::InvalidVisualKind { index, kind } => {
                write!(
                    formatter,
                    "UI layout visual {index} has unknown kind {kind:?}"
                )
            }
            Self::InvalidVisualRange { index, from, to } => write!(
                formatter,
                "UI layout visual {index} has an empty or inverted range ({from} >= {to})"
            ),
            Self::InvalidVisualSlot { index } => {
                write!(formatter, "UI layout visual {index} has an invalid slot")
            }
            Self::OversizedVisualOptions { index, bytes } => write!(
                formatter,
                "UI layout visual {index} options have {bytes} bytes; maximum is {MAX_UI_LAYOUT_OPTIONS_BYTES}"
            ),
            Self::InvalidSliderId { index, bytes } => write!(
                formatter,
                "UI layout slider {index} has an invalid {bytes}-byte id"
            ),
            Self::InvalidSliderRange { index, from, to } => write!(
                formatter,
                "UI layout slider {index} has an empty or inverted range ({from} >= {to})"
            ),
            Self::InvalidSliderNumbers { index } => write!(
                formatter,
                "UI layout slider {index} has invalid value, bounds, or step"
            ),
            Self::InvalidMiniLocation { index, start, end } => write!(
                formatter,
                "UI layout mini location {index} is empty or inverted ({start} >= {end})"
            ),
            Self::SourceRangeOutsideScore {
                index,
                end,
                source_bytes,
            } => write!(
                formatter,
                "UI layout range {index} ends at byte {end}, beyond the {source_bytes}-byte score"
            ),
        }
    }
}

impl Error for UiLayoutValidationError {}

/// One slider the text holds right now, before any evaluation.
///
/// This is what lets a control appear the moment its call is well formed and
/// vanish the moment a backspace breaks it, the way strudel.cc's widgets do -
/// the score does not have to be updated first. The `slider` inside carries
/// the value literal's bytes, which are what a drag edits; `call` is the
/// whole `slider(...)` call, which is what a control is drawn over.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveSlider {
    pub call: (usize, usize),
    pub slider: UiSlider,
    /// Direct frequency-control input, for native logarithmic travel.
    /// The serializable UiSlider stays unchanged.
    pub frequency: bool,
    /// The call this slider is the first argument of - `lpf` for
    /// `.lpf(slider(800, 100, 4000))` - so a desk away from the score can
    /// say what a fader is attached to.
    pub label: Option<String>,
}

/// Every well-formed literal slider in the text, in source order.
///
/// The same reading `visual_layout` does, without the rest of the layout: a
/// slider with an expression in it keeps its runtime meaning and is not a
/// control, and one whose numbers are impossible is the linter's to refuse.
pub fn literal_sliders(source: &str) -> Vec<LiveSlider> {
    let output = transpile(
        source,
        &TranspileOptions {
            add_return: false,
            allow_module_syntax: false,
            ..TranspileOptions::default()
        },
    );
    output
        .widgets
        .iter()
        .filter(|widget| widget.widget_type == "slider")
        .filter_map(|widget| {
            let value = widget.value.as_deref()?.parse::<f64>().ok()?;
            let min = widget.min?;
            let max = widget.max?;
            let slider = UiSlider {
                id: widget.id.clone(),
                from: widget.from,
                to: widget.to,
                value,
                min,
                max,
                step: widget.step.unwrap_or((max - min) / 1000.0),
            };
            slider.validate(0).ok()?;
            Some(LiveSlider {
                call: (widget.call_from, widget.call_to),
                slider,
                frequency: widget.frequency,
                label: widget.label.clone(),
            })
        })
        .collect()
}

/// Build the complete revision-gated layout for source that has just been
/// installed as `generation`.
///
/// The transpiler discovers call sites without evaluating their option text.
/// Returning an error rather than truncating keeps a client from presenting a
/// plausible but incomplete score layout.
pub fn visual_layout(
    source: &str,
    generation: u64,
) -> Result<UiLayoutEnvelope, UiLayoutValidationError> {
    let output = transpile(
        source,
        &TranspileOptions {
            add_return: false,
            widget_methods: VISUAL_WIDGET_METHODS
                .iter()
                .map(|method| (*method).to_owned())
                .collect(),
            id: Some("ui_layout".to_owned()),
            allow_module_syntax: false,
            ..TranspileOptions::default()
        },
    );

    let visuals = output
        .widgets
        .iter()
        .filter(|widget| {
            VISUAL_WIDGET_METHODS
                .iter()
                .any(|method| *method == widget.widget_type)
                || widget
                    .widget_type
                    .strip_prefix(ALL_VISUAL_WIDGET_PREFIX)
                    .is_some_and(|painter| VISUAL_WIDGET_METHODS.contains(&painter))
        })
        .map(|widget| {
            let all_painter = widget.widget_type.strip_prefix(ALL_VISUAL_WIDGET_PREFIX);
            let inline = all_painter.is_none()
                && (widget.widget_type.starts_with('_') || widget.widget_type == "markcss");
            UiVisual {
                id: widget.id.clone(),
                kind: all_painter
                    .unwrap_or(widget.widget_type.trim_start_matches('_'))
                    .to_owned(),
                inline,
                // A pattern's painter is tagged to that pattern - except a
                // plain `punchcard()`/`wordfall()`, which on strudel.cc is
                // fed every hap the highlighter sees, the whole stack. Its
                // `_` twin is tagged like the rest.
                slot: (all_painter.is_none()
                    && !matches!(widget.widget_type.as_str(), "punchcard" | "wordfall"))
                .then_some(widget.visual_slot)
                .flatten(),
                from: widget.from,
                to: widget.to,
                options: widget.options.clone().unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();
    // Only literal finite numeric sliders can be represented faithfully by a
    // native editor control. Dynamic JavaScript expressions retain their
    // ordinary runtime semantics but are deliberately absent from the layout.
    let sliders = output
        .widgets
        .iter()
        .filter(|widget| widget.widget_type == "slider")
        .filter_map(|widget| {
            let value = widget.value.as_deref()?.parse::<f64>().ok()?;
            let min = widget.min?;
            let max = widget.max?;
            let slider = UiSlider {
                id: widget.id.clone(),
                from: widget.from,
                to: widget.to,
                value,
                min,
                max,
                step: widget.step.unwrap_or((max - min) / 1000.0),
            };
            slider.validate(0).ok().map(|()| slider)
        })
        .collect::<Vec<_>>();
    let layout = UiLayout::new(
        generation,
        source_revision(source),
        visuals,
        sliders,
        output.mini_locations,
    )?;
    for (index, visual) in layout.visuals.iter().enumerate() {
        if visual.to > source.len() {
            return Err(UiLayoutValidationError::SourceRangeOutsideScore {
                index,
                end: visual.to,
                source_bytes: source.len(),
            });
        }
    }
    for (index, slider) in layout.sliders.iter().enumerate() {
        if slider.to > source.len() {
            return Err(UiLayoutValidationError::SourceRangeOutsideScore {
                index,
                end: slider.to,
                source_bytes: source.len(),
            });
        }
    }
    for (index, &(_, end)) in layout.mini_locations.iter().enumerate() {
        if end > source.len() {
            return Err(UiLayoutValidationError::SourceRangeOutsideScore {
                index,
                end,
                source_bytes: source.len(),
            });
        }
    }
    Ok(UiLayoutEnvelope::new(layout))
}

/// Master and optional per-visual scope/spectrum snapshots for visual clients.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UiAudioEnvelope {
    pub ui_audio: UiAudioFrame,
}

impl UiAudioEnvelope {
    pub fn from_analysis(metadata: UiAudioMetadata, analysis: UiAudioAnalysisFrame) -> Self {
        Self {
            ui_audio: UiAudioFrame {
                version: UI_AUDIO_PROTOCOL_VERSION,
                sequence: metadata.sequence,
                generation: metadata.generation,
                device_time: metadata.device_time,
                stream_id: metadata.stream_id,
                epoch: metadata.epoch,
                end_frame: metadata.end_frame,
                sample_rate: metadata.sample_rate,
                scope: analysis.scope.to_vec(),
                spectrum: analysis.spectrum.to_vec(),
                visuals: None,
            },
        }
    }

    pub fn from_analysis_set(metadata: UiAudioMetadata, analysis: UiAudioAnalysisSet) -> Self {
        let mut envelope = Self::from_analysis(metadata, analysis.master);
        envelope.ui_audio.visuals = Some(
            analysis
                .visuals
                .into_iter()
                .map(|(slot, frame)| UiVisualAudioFrame {
                    slot,
                    scope: frame.scope.to_vec(),
                    spectrum: frame.spectrum.to_vec(),
                })
                .collect(),
        );
        envelope
    }

    pub fn validate(&self) -> Result<(), UiAudioValidationError> {
        self.ui_audio.validate()
    }
}

/// Metadata captured alongside a device analysis window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiAudioMetadata {
    /// Monotonic UI-side replacement sequence. A dropped frame leaves a gap.
    pub sequence: u64,
    pub generation: u64,
    /// Device playback time sampled immediately beside the window copy.
    pub device_time: f64,
    /// Stable stream identity plus tap epoch disambiguate device recycling.
    pub stream_id: u64,
    pub epoch: u64,
    /// Exclusive final-mix frame at the right edge of this window.
    pub end_frame: u64,
    pub sample_rate: u32,
}

/// Flat wire payload consumed by terminal visualizers.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UiAudioFrame {
    pub version: u16,
    pub sequence: u64,
    pub generation: u64,
    pub device_time: f64,
    pub stream_id: u64,
    pub epoch: u64,
    pub end_frame: u64,
    pub sample_rate: u32,
    pub scope: Vec<f32>,
    pub spectrum: Vec<f32>,
    /// Absent in older runtimes. An empty list means no visual tap is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visuals: Option<Vec<UiVisualAudioFrame>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UiVisualAudioFrame {
    pub slot: u8,
    pub scope: Vec<f32>,
    pub spectrum: Vec<f32>,
}

impl UiAudioFrame {
    pub fn validate(&self) -> Result<(), UiAudioValidationError> {
        if self.version != UI_AUDIO_PROTOCOL_VERSION {
            return Err(UiAudioValidationError::UnsupportedVersion(self.version));
        }
        if !self.device_time.is_finite() || self.device_time < 0.0 {
            return Err(UiAudioValidationError::InvalidDeviceTime);
        }
        if self.sample_rate == 0 || self.sample_rate > MAX_UI_AUDIO_SAMPLE_RATE {
            return Err(UiAudioValidationError::InvalidSampleRate(self.sample_rate));
        }
        validate_audio_samples(&self.scope, &self.spectrum)?;
        if let Some(visuals) = &self.visuals {
            if visuals.len() > MAX_UI_LAYOUT_VISUALS {
                return Err(UiAudioValidationError::TooManyVisuals(visuals.len()));
            }
            let mut slots = 0_u64;
            for visual in visuals {
                if usize::from(visual.slot) >= MAX_UI_LAYOUT_VISUALS {
                    return Err(UiAudioValidationError::InvalidVisualSlot(visual.slot));
                }
                let bit = 1_u64 << visual.slot;
                if slots & bit != 0 {
                    return Err(UiAudioValidationError::DuplicateVisualSlot(visual.slot));
                }
                slots |= bit;
                validate_audio_samples(&visual.scope, &visual.spectrum)?;
            }
        }
        Ok(())
    }
}

fn validate_audio_samples(scope: &[f32], spectrum: &[f32]) -> Result<(), UiAudioValidationError> {
    if scope.len() != UI_SCOPE_SAMPLES {
        return Err(UiAudioValidationError::InvalidScopeLength(scope.len()));
    }
    if spectrum.len() != UI_SPECTRUM_BINS {
        return Err(UiAudioValidationError::InvalidSpectrumLength(
            spectrum.len(),
        ));
    }
    if let Some(index) = scope.iter().position(|sample| !sample.is_finite()) {
        return Err(UiAudioValidationError::NonFiniteScope(index));
    }
    if let Some(index) = spectrum.iter().position(|magnitude| !magnitude.is_finite()) {
        return Err(UiAudioValidationError::NonFiniteSpectrum(index));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiAudioValidationError {
    UnsupportedVersion(u16),
    InvalidDeviceTime,
    InvalidSampleRate(u32),
    InvalidScopeLength(usize),
    InvalidSpectrumLength(usize),
    NonFiniteScope(usize),
    NonFiniteSpectrum(usize),
    TooManyVisuals(usize),
    InvalidVisualSlot(u8),
    DuplicateVisualSlot(u8),
}

impl fmt::Display for UiAudioValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported UI audio protocol version {version}")
            }
            Self::InvalidDeviceTime => formatter.write_str("UI audio device time is invalid"),
            Self::InvalidSampleRate(sample_rate) => write!(
                formatter,
                "UI audio sample rate {sample_rate} is outside 1..={MAX_UI_AUDIO_SAMPLE_RATE}"
            ),
            Self::InvalidScopeLength(length) => write!(
                formatter,
                "UI audio scope has {length} samples; expected {UI_SCOPE_SAMPLES}"
            ),
            Self::InvalidSpectrumLength(length) => write!(
                formatter,
                "UI audio spectrum has {length} bins; expected {UI_SPECTRUM_BINS}"
            ),
            Self::NonFiniteScope(index) => {
                write!(formatter, "UI audio scope sample {index} is not finite")
            }
            Self::NonFiniteSpectrum(index) => {
                write!(formatter, "UI audio spectrum bin {index} is not finite")
            }
            Self::TooManyVisuals(count) => {
                write!(
                    formatter,
                    "UI audio has {count} visuals; maximum is {MAX_UI_LAYOUT_VISUALS}"
                )
            }
            Self::InvalidVisualSlot(slot) => {
                write!(
                    formatter,
                    "UI audio visual slot {slot} is outside 0..{MAX_UI_LAYOUT_VISUALS}"
                )
            }
            Self::DuplicateVisualSlot(slot) => {
                write!(
                    formatter,
                    "UI audio visual slot {slot} occurs more than once"
                )
            }
        }
    }
}

impl Error for UiAudioValidationError {}

/// One NDJSON record. The top-level key leaves room for other versioned UI
/// records on the CLI's dedicated stdout stream.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UiEventEnvelope {
    pub ui_events: UiEventBatch,
}

impl UiEventEnvelope {
    pub fn new(batch: UiEventBatch) -> Self {
        Self { ui_events: batch }
    }

    /// Validate all floating-point and source-range invariants before writing.
    pub fn validate(&self) -> Result<(), UiEventValidationError> {
        self.ui_events.validate()
    }
}

/// A clock snapshot and the device-submitted onsets associated with it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UiEventBatch {
    pub version: u16,
    pub device_time: f64,
    pub cycle: f64,
    pub cps: f64,
    pub generation: u64,
    pub source_revision: String,
    pub events: Vec<UiScheduledEvent>,
    /// Events discarded since the previous record entered the UI writer.
    pub dropped: u64,
}

impl UiEventBatch {
    pub fn new(
        device_time: f64,
        cycle: f64,
        cps: f64,
        generation: u64,
        source_revision: String,
        events: Vec<UiScheduledEvent>,
        dropped: u64,
    ) -> Result<Self, UiEventValidationError> {
        let batch = Self {
            version: UI_EVENT_PROTOCOL_VERSION,
            device_time,
            cycle,
            cps,
            generation,
            source_revision,
            events,
            dropped,
        };
        batch.validate()?;
        Ok(batch)
    }

    /// Convert scheduler traces without rounding their cycle positions.
    pub fn from_traces<'a>(
        device_time: f64,
        cycle: f64,
        cps: f64,
        generation: u64,
        source_revision: String,
        traces: impl IntoIterator<Item = &'a ScheduleTraceEvent>,
        dropped: u64,
    ) -> Result<Self, UiEventValidationError> {
        validate_positive_finite("cps", cps)?;
        let events = traces
            .into_iter()
            .take(MAX_UI_EVENTS_PER_BATCH + 1)
            .map(|trace| UiScheduledEvent::from_trace(trace, cps))
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(
            device_time,
            cycle,
            cps,
            generation,
            source_revision,
            events,
            dropped,
        )
    }

    /// Convert owned traces on the writer thread, moving labels and source
    /// ranges instead of cloning them on the live producer.
    pub fn from_owned_traces(
        device_time: f64,
        cycle: f64,
        cps: f64,
        generation: u64,
        source_revision: String,
        traces: Vec<ScheduleTraceEvent>,
        dropped: u64,
    ) -> Result<Self, UiEventValidationError> {
        validate_positive_finite("cps", cps)?;
        if traces.len() > MAX_UI_EVENTS_PER_BATCH {
            return Err(UiEventValidationError::TooManyEvents(traces.len()));
        }
        let events = traces
            .into_iter()
            .map(|trace| UiScheduledEvent::from_owned_trace(trace, cps))
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(
            device_time,
            cycle,
            cps,
            generation,
            source_revision,
            events,
            dropped,
        )
    }

    pub fn validate(&self) -> Result<(), UiEventValidationError> {
        if self.version != UI_EVENT_PROTOCOL_VERSION {
            return Err(UiEventValidationError::UnsupportedVersion(self.version));
        }
        validate_finite("device_time", self.device_time)?;
        validate_finite("cycle", self.cycle)?;
        validate_positive_finite("cps", self.cps)?;
        validate_source_revision(&self.source_revision)?;
        if self.events.len() > MAX_UI_EVENTS_PER_BATCH {
            return Err(UiEventValidationError::TooManyEvents(self.events.len()));
        }
        for (event_index, event) in self.events.iter().enumerate() {
            event.validate(event_index)?;
            if event.generation != self.generation {
                return Err(UiEventValidationError::GenerationMismatch {
                    event_index,
                    batch: self.generation,
                    event: event.generation,
                });
            }
        }
        Ok(())
    }
}

/// One onset prepared for a UI client.
///
/// The tuning an event was written in, from `edoScale`: how many divisions
/// the octave has, where it starts, which divisions the scale uses and
/// what they are called. A pitch wheel draws the divisions outside the
/// scale faint.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UiEdoScale {
    pub edo: u16,
    pub root_hz: f32,
    pub degree_indexes: Vec<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interval_labels: Vec<String>,
}

impl From<&rustel_scheduler::TraceEdoScale> for UiEdoScale {
    fn from(scale: &rustel_scheduler::TraceEdoScale) -> Self {
        Self {
            edo: scale.edo,
            root_hz: scale.root_hz,
            degree_indexes: scale.degree_indexes.clone(),
            interval_labels: scale.interval_labels.clone(),
        }
    }
}

/// The live CLI correlates its scheduler trace ID with an onset accepted by at
/// least one output route before constructing this value; refused events are
/// therefore not announced early.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UiScheduledEvent {
    pub onset_id: u64,
    pub generation: u64,
    pub whole_begin: String,
    pub whole_end: String,
    pub part_begin: String,
    pub part_end: String,
    pub target_time: f64,
    pub duration_seconds: f64,
    /// Human-readable value text. `None` means the optional label was omitted;
    /// timing and source ranges remain complete.
    pub value: Option<String>,
    /// Typed visual metadata copied from the scheduled value. These remain
    /// separate from `value`, whose representation is intentionally for
    /// humans rather than protocol parsing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// strudel.cc's `activeLabel`: shown in place of `label` while the
    /// event sounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_label: Option<String>,
    /// The tuning `edoScale` gave the event, for a pitch wheel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<UiEdoScale>,
    /// Actual scalar voice parameters that crossed the producer-side audio
    /// ring. External-only MIDI/OSC/serial events leave these absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_hz: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain: Option<f32>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ui_visuals: u64,
    /// Absolute UTF-8 byte ranges in the evaluated source.
    pub context: Vec<(usize, usize)>,
}

impl UiScheduledEvent {
    pub fn from_trace(
        trace: &ScheduleTraceEvent,
        cps: f64,
    ) -> Result<Self, UiEventValidationError> {
        validate_positive_finite("cps", cps)?;
        let duration_seconds = trace.duration.to_f64() / cps;
        let event = Self {
            onset_id: trace.onset_id,
            generation: trace.generation,
            whole_begin: trace.whole_begin.show(),
            whole_end: trace.whole_end.show(),
            part_begin: trace.part_begin.show(),
            part_end: trace.part_end.show(),
            target_time: trace.target_time,
            duration_seconds,
            value: trace
                .value_show
                .as_ref()
                .filter(|value| value.len() <= MAX_UI_EVENT_VALUE_BYTES)
                .cloned(),
            color: trace.color.clone(),
            label: trace.label.clone(),
            active_label: trace.active_label.clone(),
            scale: trace.scale.as_ref().map(UiEdoScale::from),
            frequency_hz: trace.frequency_hz,
            gain: trace.gain,
            ui_visuals: trace.ui_visuals,
            context: trace
                .context
                .iter()
                .copied()
                .take(MAX_UI_EVENT_CONTEXT_RANGES)
                .collect(),
        };
        event.validate(0)?;
        Ok(event)
    }

    pub fn from_owned_trace(
        trace: ScheduleTraceEvent,
        cps: f64,
    ) -> Result<Self, UiEventValidationError> {
        validate_positive_finite("cps", cps)?;
        let duration_seconds = trace.duration.to_f64() / cps;
        let mut context = trace.context;
        context.truncate(MAX_UI_EVENT_CONTEXT_RANGES);
        let event = Self {
            onset_id: trace.onset_id,
            generation: trace.generation,
            whole_begin: trace.whole_begin.show(),
            whole_end: trace.whole_end.show(),
            part_begin: trace.part_begin.show(),
            part_end: trace.part_end.show(),
            target_time: trace.target_time,
            duration_seconds,
            value: trace
                .value_show
                .filter(|value| value.len() <= MAX_UI_EVENT_VALUE_BYTES),
            color: trace.color,
            label: trace.label,
            active_label: trace.active_label,
            scale: trace.scale.as_ref().map(UiEdoScale::from),
            frequency_hz: trace.frequency_hz,
            gain: trace.gain,
            ui_visuals: trace.ui_visuals,
            context,
        };
        event.validate(0)?;
        Ok(event)
    }

    fn validate(&self, event_index: usize) -> Result<(), UiEventValidationError> {
        validate_finite("target_time", self.target_time)?;
        validate_finite("duration_seconds", self.duration_seconds)?;
        if self.duration_seconds < 0.0 {
            return Err(UiEventValidationError::NegativeDuration { event_index });
        }
        if self.context.len() > MAX_UI_EVENT_CONTEXT_RANGES {
            return Err(UiEventValidationError::TooManyContextRanges {
                event_index,
                count: self.context.len(),
            });
        }
        if let Some(value) = &self.value
            && value.len() > MAX_UI_EVENT_VALUE_BYTES
        {
            return Err(UiEventValidationError::OversizedValue {
                event_index,
                bytes: value.len(),
            });
        }
        for (field, text) in [
            ("color", &self.color),
            ("label", &self.label),
            ("active_label", &self.active_label),
        ] {
            if let Some(text) = text
                && text.len() > MAX_UI_EVENT_VALUE_BYTES
            {
                return Err(UiEventValidationError::OversizedVisualText {
                    event_index,
                    field,
                    bytes: text.len(),
                });
            }
        }
        if let Some(scale) = &self.scale {
            let sound = usize::from(scale.edo) >= 1
                && usize::from(scale.edo) <= rustel_scheduler::MAX_TRACE_EDO_DIVISIONS
                && scale.root_hz.is_finite()
                && scale.root_hz > 0.0
                && scale.degree_indexes.len() <= rustel_scheduler::MAX_TRACE_EDO_DIVISIONS
                && scale.degree_indexes.iter().all(|index| *index < scale.edo)
                && scale.interval_labels.len() <= scale.degree_indexes.len()
                && scale
                    .interval_labels
                    .iter()
                    .all(|label| label.len() <= rustel_scheduler::MAX_TRACE_INTERVAL_LABEL_BYTES);
            if !sound {
                return Err(UiEventValidationError::InvalidVisualNumber {
                    event_index,
                    field: "scale",
                });
            }
        }
        if let Some(frequency_hz) = self.frequency_hz
            && (!frequency_hz.is_finite() || frequency_hz <= 0.0)
        {
            return Err(UiEventValidationError::InvalidVisualNumber {
                event_index,
                field: "frequency_hz",
            });
        }
        if let Some(gain) = self.gain
            && !gain.is_finite()
        {
            return Err(UiEventValidationError::InvalidVisualNumber {
                event_index,
                field: "gain",
            });
        }
        for (range_index, &(start, end)) in self.context.iter().enumerate() {
            if start >= end {
                return Err(UiEventValidationError::InvertedContextRange {
                    event_index,
                    range_index,
                    start,
                    end,
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiEventValidationError {
    UnsupportedVersion(u16),
    NonFinite(&'static str),
    NonPositive(&'static str),
    InvalidSourceRevision,
    TooManyEvents(usize),
    NegativeDuration {
        event_index: usize,
    },
    TooManyContextRanges {
        event_index: usize,
        count: usize,
    },
    OversizedValue {
        event_index: usize,
        bytes: usize,
    },
    OversizedVisualText {
        event_index: usize,
        field: &'static str,
        bytes: usize,
    },
    InvalidVisualNumber {
        event_index: usize,
        field: &'static str,
    },
    GenerationMismatch {
        event_index: usize,
        batch: u64,
        event: u64,
    },
    InvertedContextRange {
        event_index: usize,
        range_index: usize,
        start: usize,
        end: usize,
    },
}

impl fmt::Display for UiEventValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported UI event protocol version {version}")
            }
            Self::NonFinite(field) => write!(formatter, "UI event field {field} is not finite"),
            Self::NonPositive(field) => {
                write!(formatter, "UI event field {field} must be positive")
            }
            Self::InvalidSourceRevision => {
                write!(
                    formatter,
                    "UI event source revision is not lowercase SHA-256"
                )
            }
            Self::TooManyEvents(count) => write!(
                formatter,
                "UI event batch has {count} events; maximum is {MAX_UI_EVENTS_PER_BATCH}"
            ),
            Self::NegativeDuration { event_index } => {
                write!(formatter, "UI event {event_index} has a negative duration")
            }
            Self::TooManyContextRanges { event_index, count } => write!(
                formatter,
                "UI event {event_index} has {count} source ranges; maximum is {MAX_UI_EVENT_CONTEXT_RANGES}"
            ),
            Self::OversizedValue { event_index, bytes } => write!(
                formatter,
                "UI event {event_index} value has {bytes} bytes; maximum is {MAX_UI_EVENT_VALUE_BYTES}"
            ),
            Self::OversizedVisualText {
                event_index,
                field,
                bytes,
            } => write!(
                formatter,
                "UI event {event_index} {field} has {bytes} bytes; maximum is {MAX_UI_EVENT_VALUE_BYTES}"
            ),
            Self::InvalidVisualNumber { event_index, field } => {
                write!(formatter, "UI event {event_index} has an invalid {field}")
            }
            Self::GenerationMismatch {
                event_index,
                batch,
                event,
            } => write!(
                formatter,
                "UI event {event_index} belongs to generation {event}, not batch generation {batch}"
            ),
            Self::InvertedContextRange {
                event_index,
                range_index,
                start,
                end,
            } => write!(
                formatter,
                "UI event {event_index} context range {range_index} is empty or inverted ({start} >= {end})"
            ),
        }
    }
}

impl Error for UiEventValidationError {}

fn validate_finite(field: &'static str, value: f64) -> Result<(), UiEventValidationError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(UiEventValidationError::NonFinite(field))
    }
}

fn validate_positive_finite(field: &'static str, value: f64) -> Result<(), UiEventValidationError> {
    validate_finite(field, value)?;
    if value > 0.0 {
        Ok(())
    } else {
        Err(UiEventValidationError::NonPositive(field))
    }
}

fn validate_source_revision(revision: &str) -> Result<(), UiEventValidationError> {
    if revision.len() == 64
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(UiEventValidationError::InvalidSourceRevision)
    }
}

/// Result of a nonblocking attempt to enqueue one complete NDJSON record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiEventSendStatus {
    Queued,
    DroppedFull,
    Disconnected,
}

/// Raw traces for one homogeneous protocol record.
///
/// The caller supplies the clock snapshot. Conversion to strings and JSON can
/// remain on [`UiEventSink`]'s writer thread by forwarding this request to
/// [`UiEventSink::try_send_traces`].
#[derive(Clone, Debug, PartialEq)]
pub struct UiTraceBatchRequest {
    pub device_time: f64,
    pub cycle: f64,
    pub cps: f64,
    pub generation: u64,
    pub source_revision: String,
    pub traces: Vec<ScheduleTraceEvent>,
    pub dropped: u64,
    /// `Some(cycle)`: the batch is a preview - haps the scheduler has not
    /// queued yet, queried ahead for the painters, all with onsets at or
    /// after that cycle, which is as far as the audio has been scheduled.
    /// Each preview replaces the last; a real trace for the same onset
    /// retires it.
    pub preview_from_cycle: Option<f64>,
}

/// Result of matching scheduler traces to events accepted by an output route.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiTraceCorrelation {
    pub ready: Vec<ScheduleTraceEvent>,
    pub dropped: u64,
}

/// One scheduler onset accepted by at least one live output route.
///
/// Scalar audio supplies pitch/gain metadata. MIDI, OSC, and serial routes do
/// not, but still prove that the onset crossed a real output boundary and may
/// therefore drive source highlights and time-based visualizers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiAcceptedOnset {
    pub generation: u64,
    pub onset_id: u64,
    pub frequency_hz: Option<f32>,
    pub gain: Option<f32>,
}

/// Remove and return only traces whose matching onset reached an output.
///
/// `accepted` must contain only IDs that crossed a producer-side output
/// boundary. Unknown IDs are harmless because they may belong to events with
/// no source trace. A generation mismatch consumes the stale trace and is
/// reported as a drop instead of associating it with the wrong score revision.
pub fn correlate_submitted_traces(
    pending: &mut HashMap<u64, ScheduleTraceEvent>,
    accepted: impl IntoIterator<Item = UiAcceptedOnset>,
) -> UiTraceCorrelation {
    let accepted = accepted.into_iter();
    let mut correlation = UiTraceCorrelation {
        ready: Vec::with_capacity(accepted.size_hint().0),
        dropped: 0,
    };
    for accepted in accepted {
        let Some(mut trace) = pending.remove(&accepted.onset_id) else {
            continue;
        };
        let valid_frequency = accepted
            .frequency_hz
            .is_none_or(|frequency| frequency.is_finite() && frequency >= 0.0);
        let valid_gain = accepted.gain.is_none_or(f32::is_finite);
        if trace.generation == accepted.generation && valid_frequency && valid_gain {
            // Scalar samples intentionally use zero as their no-pitch
            // sentinel. The onset still crossed the audio ring and must reach
            // source highlights; a pitched sample keeps the pitch the
            // scheduler read from the score, an unpitched one has none.
            trace.frequency_hz = accepted
                .frequency_hz
                .filter(|frequency| *frequency > 0.0)
                .or(trace.frequency_hz);
            trace.gain = accepted.gain;
            correlation.ready.push(trace);
        } else {
            correlation.dropped = correlation.dropped.saturating_add(1);
        }
    }
    correlation
}

/// Fold one producer step's scheduler observations into bounded correlation
/// state.
///
/// A failed step cannot prove that any of its fresh traces can reach the audio
/// ring, so all of them become UI loss while previously pending traces remain
/// available for a later retry. When a successful step has fresh traces,
/// entries older than the Session's active generation are discarded before
/// those traces are admitted up to `capacity`. Empty ticks stay O(1); periodic
/// pruning handles generation cleanup between traced batches. All loss
/// accounting saturates rather than wrapping.
pub fn ingest_pending_traces(
    pending: &mut HashMap<u64, ScheduleTraceEvent>,
    fresh: impl IntoIterator<Item = ScheduleTraceEvent>,
    step_succeeded: bool,
    active_generation: u64,
    capacity: usize,
    dropped: &mut u64,
) {
    let mut fresh = fresh.into_iter().peekable();
    if !step_succeeded {
        for _ in fresh {
            *dropped = dropped.saturating_add(1);
        }
        return;
    }

    // Most 2 ms live-loop ticks have no new scheduler traces. Leave the
    // potentially large map untouched on that hot path; the periodic prune
    // still removes old generations within its bounded interval.
    if fresh.peek().is_none() {
        return;
    }

    let before = pending.len();
    pending.retain(|_, trace| trace.generation >= active_generation);
    *dropped = dropped.saturating_add(before.saturating_sub(pending.len()) as u64);

    for trace in fresh {
        if trace.generation < active_generation || pending.len() >= capacity {
            *dropped = dropped.saturating_add(1);
        } else {
            pending.insert(trace.onset_id, trace);
        }
    }
}

/// Discard correlation entries that can no longer describe a future visual.
///
/// The target-time boundary is inclusive: an event exactly `stale_grace_secs`
/// behind the device clock is retained for this pass. Generation rollover is
/// immediate, while refused/unmatched events in the active generation receive
/// the grace window before being counted as loss.
pub fn prune_pending_traces(
    pending: &mut HashMap<u64, ScheduleTraceEvent>,
    active_generation: u64,
    device_time: f64,
    stale_grace_secs: f64,
    dropped: &mut u64,
) {
    let before = pending.len();
    pending.retain(|_, trace| {
        trace.generation >= active_generation && trace.target_time + stale_grace_secs >= device_time
    });
    *dropped = dropped.saturating_add(before.saturating_sub(pending.len()) as u64);
}

/// Hold output-accepted onsets until their generation's layout has entered the
/// writer queue.
///
/// The protocol withholds those records rather than counting them as loss: an
/// onset that already crossed an output boundary must still reach the client
/// once its revision-gated layout snapshot is queued. Only rollover past
/// `active_generation` and overflow past `capacity` are counted as dropped.
pub fn release_traces_when_layout_ready(
    withheld: &mut Vec<ScheduleTraceEvent>,
    newly_accepted: Vec<ScheduleTraceEvent>,
    layout_ready: bool,
    active_generation: u64,
    capacity: usize,
    dropped: &mut u64,
) -> Vec<ScheduleTraceEvent> {
    let before = withheld.len();
    withheld.retain(|trace| trace.generation >= active_generation);
    *dropped = dropped.saturating_add(before.saturating_sub(withheld.len()) as u64);

    if layout_ready {
        if withheld.is_empty() {
            return newly_accepted;
        }
        withheld.extend(newly_accepted);
        return std::mem::take(withheld);
    }

    for trace in newly_accepted {
        if trace.generation < active_generation || withheld.len() >= capacity {
            *dropped = dropped.saturating_add(1);
        } else {
            withheld.push(trace);
        }
    }
    Vec::new()
}

/// Group, bound, and dispatch raw traces without waiting for a UI consumer.
///
/// Each request contains exactly one generation and at most
/// [`MAX_UI_EVENTS_PER_BATCH`] traces. Unknown generations are counted as
/// dropped because their source revision is no longer available. Loss remains
/// accumulated until a request is queued successfully; that record reports all
/// loss since the preceding successful request.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_trace_batches(
    device_time: f64,
    cycle: f64,
    fallback_generation: u64,
    traces: Vec<ScheduleTraceEvent>,
    generation_sources: &BTreeMap<u64, (String, f64)>,
    dropped: &mut u64,
    mut dispatch: impl FnMut(UiTraceBatchRequest) -> UiEventSendStatus,
) {
    if traces.is_empty() && *dropped == 0 {
        return;
    }

    let mut by_generation = BTreeMap::<u64, Vec<_>>::new();
    for trace in traces {
        by_generation
            .entry(trace.generation)
            .or_default()
            .push(trace);
    }
    if by_generation.is_empty() {
        by_generation.insert(fallback_generation, Vec::new());
    }

    for (generation, traces) in by_generation {
        let Some((source_revision, cps)) = generation_sources.get(&generation) else {
            *dropped = dropped.saturating_add(traces.len() as u64);
            continue;
        };

        if traces.is_empty() {
            let reported_dropped = *dropped;
            if dispatch(UiTraceBatchRequest {
                device_time,
                cycle,
                cps: *cps,
                generation,
                source_revision: source_revision.clone(),
                traces,
                dropped: reported_dropped,
                preview_from_cycle: None,
            }) == UiEventSendStatus::Queued
            {
                *dropped = 0;
            }
            continue;
        }

        let mut traces = traces.into_iter();
        loop {
            let chunk: Vec<_> = traces.by_ref().take(MAX_UI_EVENTS_PER_BATCH).collect();
            if chunk.is_empty() {
                break;
            }
            let event_count = chunk.len() as u64;
            let reported_dropped = *dropped;
            match dispatch(UiTraceBatchRequest {
                device_time,
                cycle,
                cps: *cps,
                generation,
                source_revision: source_revision.clone(),
                traces: chunk,
                dropped: reported_dropped,
                preview_from_cycle: None,
            }) {
                UiEventSendStatus::Queued => *dropped = 0,
                UiEventSendStatus::DroppedFull | UiEventSendStatus::Disconnected => {
                    *dropped = reported_dropped.saturating_add(event_count);
                }
            }
        }
    }
}

/// A point-in-time view of sink loss and writer failures.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UiEventSinkStats {
    pub dropped_full: u64,
    pub dropped_disconnected: u64,
    /// Audio snapshots replaced or missed while the single latest-frame slot
    /// was busy. They never consume control queue capacity.
    pub dropped_audio: u64,
    pub invalid_records: u64,
    pub serialization_errors: u64,
    pub write_errors: u64,
    /// Gap counts recovered from trace batches the writer had to reject. They
    /// ride the next accepted trace batch to clients.
    pub orphaned_dropped_events: u64,
}

#[derive(Debug, Default)]
struct UiEventSinkCounters {
    dropped_full: AtomicU64,
    dropped_disconnected: AtomicU64,
    dropped_audio: AtomicU64,
    invalid_records: AtomicU64,
    serialization_errors: AtomicU64,
    write_errors: AtomicU64,
    /// Gap tallies stranded by rejected trace batches. A rejected record can
    /// never reach a client, so its `dropped` count is folded into the next
    /// accepted trace batch instead of being lost.
    orphaned_dropped_events: AtomicU64,
}

#[derive(Debug)]
enum UiEventWrite {
    Envelope(UiEventEnvelope),
    Layout(UiLayoutEnvelope),
    Traces {
        device_time: f64,
        cycle: f64,
        cps: f64,
        generation: u64,
        source_revision: String,
        traces: Vec<ScheduleTraceEvent>,
        dropped: u64,
    },
}

#[derive(Debug)]
// Keeping the fixed arrays inline is intentional: boxing this variant would
// put an allocator call back on the 30 Hz scheduling producer.
#[allow(clippy::large_enum_variant)]
enum UiAudioWrite {
    Envelope(UiAudioEnvelope),
    Analysis {
        metadata: UiAudioMetadata,
        analysis: UiAudioAnalysisSet,
    },
}

/// Bounded, nonblocking handoff to a dedicated NDJSON writer thread.
///
/// Dropping the sink detaches the writer after closing its channel; use
/// [`UiEventSink::finish`] only at a teardown point where waiting is safe.
#[derive(Debug)]
pub struct UiEventSink {
    sender: Option<SyncSender<UiEventWrite>>,
    latest_audio: Arc<Mutex<Option<UiAudioWrite>>>,
    writer_alive: Arc<AtomicBool>,
    writer_done: Arc<AtomicBool>,
    counters: Arc<UiEventSinkCounters>,
    worker: Option<JoinHandle<()>>,
}

struct WriterAliveGuard(Arc<AtomicBool>);

impl Drop for WriterAliveGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl UiEventSink {
    /// Spawn a sink for stdout with the small default queue.
    pub fn stdout() -> io::Result<Self> {
        Self::stdout_with_capacity(DEFAULT_UI_EVENT_QUEUE_CAPACITY)
    }

    /// Spawn a stdout sink with an explicit positive batch capacity.
    pub fn stdout_with_capacity(capacity: usize) -> io::Result<Self> {
        Self::spawn(capacity, |receiver, latest_audio, counters, capacity| {
            let stdout = io::stdout();
            writer_loop(receiver, latest_audio, counters, capacity, |line| {
                let mut stdout = stdout.lock();
                stdout.write_all(line)?;
                stdout.flush()
            });
        })
    }

    /// Spawn a sink targeting an owned writer. Primarily useful for embedding
    /// and deterministic protocol tests.
    pub fn with_writer<W>(capacity: usize, mut writer: W) -> io::Result<Self>
    where
        W: Write + Send + 'static,
    {
        Self::spawn(
            capacity,
            move |receiver, latest_audio, counters, capacity| {
                writer_loop(receiver, latest_audio, counters, capacity, |line| {
                    writer.write_all(line)?;
                    writer.flush()
                });
            },
        )
    }

    fn spawn(
        capacity: usize,
        run: impl FnOnce(
            mpsc::Receiver<UiEventWrite>,
            Arc<Mutex<Option<UiAudioWrite>>>,
            Arc<UiEventSinkCounters>,
            usize,
        ) + Send
        + 'static,
    ) -> io::Result<Self> {
        if capacity == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UI event queue capacity must be positive",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let latest_audio = Arc::new(Mutex::new(None));
        let worker_audio = Arc::clone(&latest_audio);
        let writer_alive = Arc::new(AtomicBool::new(true));
        let worker_alive = Arc::clone(&writer_alive);
        let writer_done = Arc::new(AtomicBool::new(false));
        let worker_done = Arc::clone(&writer_done);
        let counters = Arc::new(UiEventSinkCounters::default());
        let worker_counters = Arc::clone(&counters);
        let worker = thread::Builder::new()
            .name("ui-event-writer".to_owned())
            .spawn(move || {
                let _alive = WriterAliveGuard(worker_alive);
                run(receiver, worker_audio, worker_counters, capacity);
                // Signalled after the run closure returns on every exit path
                // (queue drained, write error, or detached consumer), so a
                // bounded teardown can join without risking a blocked join.
                worker_done.store(true, Ordering::Release);
            })?;
        Ok(Self {
            sender: Some(sender),
            latest_audio,
            writer_alive,
            writer_done,
            counters,
            worker: Some(worker),
        })
    }

    /// Never waits for queue space. A full or failed UI consumer cannot delay
    /// the event producer.
    pub fn try_send(&self, envelope: UiEventEnvelope) -> UiEventSendStatus {
        self.try_send_write(UiEventWrite::Envelope(envelope))
    }

    /// Queue a complete installed-score layout without waiting for writer or
    /// consumer progress. Validation and JSON serialization happen on the
    /// same background thread as onset records.
    pub fn try_send_layout(&self, layout: UiLayoutEnvelope) -> UiEventSendStatus {
        self.try_send_write(UiEventWrite::Layout(layout))
    }

    /// Try to queue a layout while returning ownership when the queue cannot
    /// accept it. Reload loops use this to retry a large snapshot without
    /// cloning its bounded option strings on every poll.
    pub fn try_send_layout_recover(
        &self,
        layout: UiLayoutEnvelope,
    ) -> Result<(), (UiEventSendStatus, UiLayoutEnvelope)> {
        match self.try_send_write_recover(UiEventWrite::Layout(layout)) {
            Ok(()) => Ok(()),
            Err((status, UiEventWrite::Layout(layout))) => Err((status, layout)),
            Err(_) => unreachable!("layout send returned a different record kind"),
        }
    }

    /// Queue a replaceable post-mix audio snapshot without waiting. A full
    /// slot simply loses this frame; a later frame supersedes it. Audio never
    /// occupies the control queue used by layouts and onsets.
    pub fn try_send_audio(&self, audio: UiAudioEnvelope) -> UiEventSendStatus {
        self.try_replace_audio(UiAudioWrite::Envelope(audio))
    }

    /// Queue fixed-array analysis output without allocating JSON vectors on
    /// the scheduling producer. The writer performs that conversion only for
    /// the newest frame that survives coalescing.
    pub fn try_send_audio_analysis(
        &self,
        metadata: UiAudioMetadata,
        analysis: UiAudioAnalysisSet,
    ) -> UiEventSendStatus {
        self.try_replace_audio(UiAudioWrite::Analysis { metadata, analysis })
    }

    fn try_replace_audio(&self, audio: UiAudioWrite) -> UiEventSendStatus {
        if !self.writer_alive.load(Ordering::Acquire) {
            self.counters
                .dropped_disconnected
                .fetch_add(1, Ordering::Relaxed);
            return UiEventSendStatus::Disconnected;
        }
        match self.latest_audio.try_lock() {
            Ok(mut latest) => {
                if !self.writer_alive.load(Ordering::Acquire) {
                    self.counters
                        .dropped_disconnected
                        .fetch_add(1, Ordering::Relaxed);
                    return UiEventSendStatus::Disconnected;
                }
                if latest.replace(audio).is_some() {
                    self.counters.dropped_audio.fetch_add(1, Ordering::Relaxed);
                }
                UiEventSendStatus::Queued
            }
            Err(TryLockError::WouldBlock) => {
                self.counters.dropped_audio.fetch_add(1, Ordering::Relaxed);
                UiEventSendStatus::DroppedFull
            }
            Err(TryLockError::Poisoned(_)) => {
                self.counters
                    .dropped_disconnected
                    .fetch_add(1, Ordering::Relaxed);
                UiEventSendStatus::Disconnected
            }
        }
    }

    /// Queue raw owned traces for conversion and JSON serialization on the
    /// writer thread. This keeps fraction formatting and context/value moves
    /// off the live scheduling continuation.
    #[allow(clippy::too_many_arguments)]
    pub fn try_send_traces(
        &self,
        device_time: f64,
        cycle: f64,
        cps: f64,
        generation: u64,
        source_revision: String,
        traces: Vec<ScheduleTraceEvent>,
        dropped: u64,
    ) -> UiEventSendStatus {
        self.try_send_write(UiEventWrite::Traces {
            device_time,
            cycle,
            cps,
            generation,
            source_revision,
            traces,
            dropped,
        })
    }

    fn try_send_write(&self, write: UiEventWrite) -> UiEventSendStatus {
        match self.try_send_write_recover(write) {
            Ok(()) => UiEventSendStatus::Queued,
            Err((status, _)) => status,
        }
    }

    fn try_send_write_recover(
        &self,
        write: UiEventWrite,
    ) -> Result<(), (UiEventSendStatus, UiEventWrite)> {
        let Some(sender) = &self.sender else {
            self.counters
                .dropped_disconnected
                .fetch_add(1, Ordering::Relaxed);
            return Err((UiEventSendStatus::Disconnected, write));
        };
        match sender.try_send(write) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(write)) => {
                self.counters.dropped_full.fetch_add(1, Ordering::Relaxed);
                Err((UiEventSendStatus::DroppedFull, write))
            }
            Err(TrySendError::Disconnected(write)) => {
                self.counters
                    .dropped_disconnected
                    .fetch_add(1, Ordering::Relaxed);
                Err((UiEventSendStatus::Disconnected, write))
            }
        }
    }

    pub fn stats(&self) -> UiEventSinkStats {
        UiEventSinkStats {
            dropped_full: self.counters.dropped_full.load(Ordering::Relaxed),
            dropped_disconnected: self.counters.dropped_disconnected.load(Ordering::Relaxed),
            dropped_audio: self.counters.dropped_audio.load(Ordering::Relaxed),
            invalid_records: self.counters.invalid_records.load(Ordering::Relaxed),
            serialization_errors: self.counters.serialization_errors.load(Ordering::Relaxed),
            write_errors: self.counters.write_errors.load(Ordering::Relaxed),
            orphaned_dropped_events: self
                .counters
                .orphaned_dropped_events
                .load(Ordering::Relaxed),
        }
    }

    /// Close the channel and wait for already-queued records to be written.
    /// This is intentionally explicit because joining a blocked stdout writer
    /// is inappropriate on the live scheduling path.
    pub fn finish(mut self) -> thread::Result<()> {
        drop(self.sender.take());
        match self.worker.take() {
            Some(worker) => worker.join(),
            None => Ok(()),
        }
    }

    /// Close the channel and give the writer a bounded grace period to drain.
    ///
    /// Teardown for live sessions: queued records get a fair chance to reach
    /// the consumer before process exit, but a writer stuck on a full or dead
    /// pipe never blocks shutdown. Returns whether the writer finished within
    /// `grace`; a still-running writer is left detached to end on its own.
    pub fn finish_timeout(mut self, grace: std::time::Duration) -> bool {
        drop(self.sender.take());
        let deadline = std::time::Instant::now() + grace;
        while !self.writer_done.load(Ordering::Acquire) {
            if std::time::Instant::now() >= deadline {
                // Detach: dropping the handle lets the thread finish in the
                // background; the process must not wait behind a blocked pipe.
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        match self.worker.take() {
            Some(worker) => worker.join().is_ok(),
            None => true,
        }
    }
}

fn writer_loop(
    receiver: mpsc::Receiver<UiEventWrite>,
    latest_audio: Arc<Mutex<Option<UiAudioWrite>>>,
    counters: Arc<UiEventSinkCounters>,
    control_capacity: usize,
    mut write_line: impl FnMut(&[u8]) -> io::Result<()>,
) {
    let mut disconnected = false;
    // Reused across iterations, including idle polls, so a quiet writer does
    // not allocate once per 10 ms.
    let mut control = Vec::with_capacity(control_capacity.saturating_add(1));
    loop {
        control.clear();
        match receiver.recv_timeout(UI_AUDIO_WRITER_POLL) {
            Ok(write) => control.push(write),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => disconnected = true,
        }

        // Freeze the replaceable audio slot while taking every control record
        // that could precede its current frame. Layout is queued before audio
        // is enabled, so this closes the recv-timeout race where the writer
        // observed the audio slot just before noticing its layout in the
        // channel. Only the bounded channel drain happens under this lock;
        // validation, allocation, and I/O remain outside it.
        let audio = take_audio_after_queued_controls(
            &receiver,
            &latest_audio,
            control_capacity,
            &mut control,
            &mut disconnected,
        );

        for write in control.drain(..) {
            if !write_control_record(write, &counters, &mut write_line) {
                return;
            }
        }
        if let Some(audio) = audio
            && !write_audio_record(audio, &counters, &mut write_line)
        {
            return;
        }
        if disconnected {
            return;
        }
    }
}

fn take_audio_after_queued_controls(
    receiver: &mpsc::Receiver<UiEventWrite>,
    latest_audio: &Mutex<Option<UiAudioWrite>>,
    control_capacity: usize,
    control: &mut Vec<UiEventWrite>,
    disconnected: &mut bool,
) -> Option<UiAudioWrite> {
    let mut latest = match latest_audio.lock() {
        Ok(latest) => latest,
        Err(poisoned) => poisoned.into_inner(),
    };
    for _ in 0..control_capacity {
        match receiver.try_recv() {
            Ok(write) => control.push(write),
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                *disconnected = true;
                break;
            }
        }
    }
    latest.take()
}

fn write_control_record(
    write: UiEventWrite,
    counters: &UiEventSinkCounters,
    write_line: &mut impl FnMut(&[u8]) -> io::Result<()>,
) -> bool {
    let line = match write {
        UiEventWrite::Envelope(envelope) => {
            if envelope.validate().is_err() {
                counters.invalid_records.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            serialize_record(&envelope, counters)
        }
        UiEventWrite::Layout(layout) => {
            if layout.validate().is_err() {
                counters.invalid_records.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            serialize_record(&layout, counters)
        }
        UiEventWrite::Traces {
            device_time,
            cycle,
            cps,
            generation,
            source_revision,
            traces,
            dropped,
        } => {
            // Fold any gap counts stranded by previously rejected trace
            // batches into this record so loss accounting never loses a
            // rejected batch's tally. The swap keeps a rejected fold itself
            // recoverable by the next accepted record.
            let stranded = counters.orphaned_dropped_events.swap(0, Ordering::Relaxed);
            let dropped = dropped.saturating_add(stranded);
            let event_count = traces.len() as u64;
            let envelope = match UiEventBatch::from_owned_traces(
                device_time,
                cycle,
                cps,
                generation,
                source_revision,
                traces,
                dropped,
            ) {
                Ok(batch) => UiEventEnvelope::new(batch),
                Err(_) => {
                    counters.invalid_records.fetch_add(1, Ordering::Relaxed);
                    counters
                        .orphaned_dropped_events
                        .fetch_add(dropped.saturating_add(event_count), Ordering::Relaxed);
                    return true;
                }
            };
            if envelope.validate().is_err() {
                counters.invalid_records.fetch_add(1, Ordering::Relaxed);
                counters
                    .orphaned_dropped_events
                    .fetch_add(dropped.saturating_add(event_count), Ordering::Relaxed);
                return true;
            }
            serialize_record(&envelope, counters)
        }
    };
    write_serialized(line, counters, write_line)
}

fn write_audio_record(
    audio: UiAudioWrite,
    counters: &UiEventSinkCounters,
    write_line: &mut impl FnMut(&[u8]) -> io::Result<()>,
) -> bool {
    let audio = match audio {
        UiAudioWrite::Envelope(audio) => audio,
        UiAudioWrite::Analysis { metadata, analysis } => {
            UiAudioEnvelope::from_analysis_set(metadata, analysis)
        }
    };
    if audio.validate().is_err() {
        counters.invalid_records.fetch_add(1, Ordering::Relaxed);
        return true;
    }
    let line = serialize_record(&audio, counters);
    write_serialized(line, counters, write_line)
}

fn serialize_record(record: &impl Serialize, counters: &UiEventSinkCounters) -> Option<Vec<u8>> {
    match serde_json::to_vec(record) {
        Ok(line) => Some(line),
        Err(_) => {
            counters
                .serialization_errors
                .fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

fn write_serialized(
    line: Option<Vec<u8>>,
    counters: &UiEventSinkCounters,
    write_line: &mut impl FnMut(&[u8]) -> io::Result<()>,
) -> bool {
    let Some(mut line) = line else {
        return true;
    };
    line.push(b'\n');
    if write_line(&line).is_err() {
        counters.write_errors.fetch_add(1, Ordering::Relaxed);
        false
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_fraction::Fraction;
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("writer lock").extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct GatedWriter {
        entered: mpsc::Sender<()>,
        released: Arc<(Mutex<bool>, Condvar)>,
        finished: mpsc::Sender<()>,
    }

    impl Write for GatedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.entered.send(());
            let (released, wake) = &*self.released;
            let mut released = released.lock().expect("release lock");
            while !*released {
                released = wake.wait(released).expect("release wait");
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for GatedWriter {
        fn drop(&mut self) {
            let _ = self.finished.send(());
        }
    }

    struct FirstWriteGatedSharedWriter {
        captured: Arc<Mutex<Vec<u8>>>,
        entered: mpsc::Sender<()>,
        released: Arc<(Mutex<bool>, Condvar)>,
        first: bool,
    }

    impl Write for FirstWriteGatedSharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.first {
                self.first = false;
                let _ = self.entered.send(());
                let (released, wake) = &*self.released;
                let mut released = released.lock().expect("release lock");
                while !*released {
                    released = wake.wait(released).expect("release wait");
                }
            }
            self.captured
                .lock()
                .expect("captured output")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FailingWriter(mpsc::Sender<()>);

    impl Write for FailingWriter {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            let _ = self.0.send(());
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "test failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn trace() -> ScheduleTraceEvent {
        ScheduleTraceEvent {
            onset_id: 9,
            generation: 3,
            whole_begin: Fraction::new(1, 3),
            whole_end: Fraction::new(2, 3),
            part_begin: Fraction::new(5, 12),
            part_end: Fraction::new(7, 12),
            duration: Fraction::new(1, 6),
            target_time: 12.25,
            value_show: Some("bd".to_owned()),
            color: Some("cyan".to_owned()),
            label: Some("kick".to_owned()),
            active_label: None,
            scale: None,
            frequency_hz: None,
            gain: None,
            ui_visuals: 0,
            context: vec![(4, 8), (11, 13)],
        }
    }

    fn trace_with(onset_id: u64, generation: u64) -> ScheduleTraceEvent {
        ScheduleTraceEvent {
            onset_id,
            generation,
            ..trace()
        }
    }

    fn trace_at(onset_id: u64, generation: u64, target_time: f64) -> ScheduleTraceEvent {
        ScheduleTraceEvent {
            target_time,
            ..trace_with(onset_id, generation)
        }
    }

    fn envelope() -> UiEventEnvelope {
        UiEventEnvelope::new(
            UiEventBatch::from_traces(
                12.0,
                4.5,
                2.0,
                3,
                source_revision("$: s(\"bd\")"),
                [&trace()],
                7,
            )
            .expect("valid batch"),
        )
    }

    fn layout_source() -> &'static str {
        "$: note(\"c4 e4\")._pianoroll({ labels: true })\n\
$: s(\"bd sd\").gain(slider(.25, 0, 1, .05)).scope()\n\
$: note(\"g4\").markcss('outline:2px solid cyan')\n\
all(pianoroll)"
    }

    fn layout() -> UiLayoutEnvelope {
        visual_layout(layout_source(), 17).expect("valid visual layout")
    }

    fn ui_visual() -> UiVisual {
        UiVisual {
            id: "ui_layout_widget_scope_0_0-9".to_owned(),
            kind: "scope".to_owned(),
            inline: false,
            slot: None,
            from: 0,
            to: 9,
            options: String::new(),
        }
    }

    fn ui_slider() -> UiSlider {
        UiSlider {
            id: "12:15".to_owned(),
            from: 12,
            to: 15,
            value: 0.25,
            min: 0.0,
            max: 1.0,
            step: 0.05,
        }
    }

    fn audio_analysis() -> UiAudioAnalysisFrame {
        let mut scope = [0.0; UI_SCOPE_SAMPLES];
        scope[3] = -0.75;
        scope[4] = 0.5;
        let mut spectrum = [0.0; UI_SPECTRUM_BINS];
        spectrum[7] = 0.9;
        UiAudioAnalysisFrame { scope, spectrum }
    }

    fn audio_metadata() -> UiAudioMetadata {
        UiAudioMetadata {
            sequence: 12,
            generation: 17,
            device_time: 4.25,
            stream_id: 8,
            epoch: 3,
            end_frame: 2048,
            sample_rate: 48_000,
        }
    }

    fn audio_analysis_set() -> UiAudioAnalysisSet {
        let master = audio_analysis();
        let mut visual = master.clone();
        visual.scope[3] = 0.25;
        visual.spectrum[7] = -18.0;
        UiAudioAnalysisSet {
            master,
            visuals: vec![(0, visual.clone()), (63, visual)],
            sides: Vec::new(),
        }
    }

    fn audio() -> UiAudioEnvelope {
        UiAudioEnvelope::from_analysis(audio_metadata(), audio_analysis())
    }

    #[test]
    fn serializes_stable_versioned_shape_and_exact_fractions() {
        let value = serde_json::to_value(envelope()).expect("serialize envelope");
        let batch = &value["ui_events"];
        assert_eq!(batch["version"], UI_EVENT_PROTOCOL_VERSION);
        assert_eq!(batch["device_time"], 12.0);
        assert_eq!(batch["cycle"], 4.5);
        assert_eq!(batch["cps"], 2.0);
        assert_eq!(batch["generation"], 3);
        assert_eq!(batch["source_revision"], source_revision("$: s(\"bd\")"));
        assert_eq!(batch["dropped"], 7);
        let event = &batch["events"][0];
        assert_eq!(event["onset_id"], 9);
        assert_eq!(event["whole_begin"], "1/3");
        assert_eq!(event["whole_end"], "2/3");
        assert_eq!(event["part_begin"], "5/12");
        assert_eq!(event["part_end"], "7/12");
        assert_eq!(event["target_time"], 12.25);
        assert_eq!(event["duration_seconds"], 1.0 / 12.0);
        assert_eq!(event["value"], "bd");
        assert_eq!(event["color"], "cyan");
        assert_eq!(event["label"], "kick");
        assert_eq!(event["context"], serde_json::json!([[4, 8], [11, 13]]));
    }

    #[test]
    fn source_revision_is_sha256_of_exact_bytes() {
        assert_eq!(
            source_revision("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(source_revision("abc"), source_revision("abc\n"));
    }

    /// strudel.cc feeds a plain `punchcard()` every hap the highlighter
    /// sees - the whole stack - while `_punchcard()` and both spellings of
    /// `pianoroll` are tagged to their own pattern. `wordfall` is a
    /// punchcard on its side and follows it.
    #[test]
    fn a_plain_punchcard_sees_the_whole_stack_and_its_underscored_twin_its_own_pattern() {
        let layout = visual_layout(
            "$: s(\"bd\").punchcard()\n$: s(\"hh\")._punchcard()\n$: s(\"sd\").pianoroll()\n$: s(\"cp\").wordfall()\n$: s(\"oh\")._wordfall({ labels: 0 })",
            1,
        )
        .expect("layout")
        .ui_layout;
        assert_eq!(
            layout
                .visuals
                .iter()
                .map(|visual| (visual.kind.as_str(), visual.inline, visual.slot.is_some()))
                .collect::<Vec<_>>(),
            vec![
                ("punchcard", false, false),
                ("punchcard", true, true),
                ("pianoroll", false, true),
                ("wordfall", false, false),
                ("wordfall", true, true),
            ]
        );
    }

    #[test]
    fn visual_layout_serializes_call_sites_options_and_mini_token_ranges() {
        let layout = layout();
        let value = serde_json::to_value(&layout).expect("serialize layout");
        assert_eq!(
            value.as_object().expect("top-level object").keys().count(),
            1
        );
        let record = &value["ui_layout"];
        assert_eq!(record["version"], UI_LAYOUT_PROTOCOL_VERSION);
        assert_eq!(record["generation"], 17);
        assert_eq!(record["source_revision"], source_revision(layout_source()));

        assert_eq!(layout.ui_layout.visuals.len(), 4);
        assert_eq!(
            layout
                .ui_layout
                .visuals
                .iter()
                .map(|visual| (visual.kind.as_str(), visual.inline, visual.options.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("pianoroll", true, "{ labels: true }"),
                ("scope", false, ""),
                ("markcss", true, "'outline:2px solid cyan'"),
                ("pianoroll", false, ""),
            ]
        );
        assert_eq!(
            layout
                .ui_layout
                .visuals
                .iter()
                .map(|visual| visual.slot)
                .collect::<Vec<_>>(),
            vec![Some(0), Some(1), None, None]
        );
        for visual in &layout.ui_layout.visuals {
            assert!(visual.from < visual.to);
            assert!(visual.to <= layout_source().len());
            assert!(layout_source()[visual.from..visual.to].contains(&visual.kind));
        }

        assert_eq!(layout.ui_layout.sliders.len(), 1);
        let slider = &layout.ui_layout.sliders[0];
        assert_eq!(&layout_source()[slider.from..slider.to], ".25");
        assert_eq!(slider.value, 0.25);
        assert_eq!((slider.min, slider.max, slider.step), (0.0, 1.0, 0.05));
        assert_eq!(record["sliders"][0]["id"], slider.id);

        let mini_tokens = layout
            .ui_layout
            .mini_locations
            .iter()
            .map(|&(from, to)| &layout_source()[from..to])
            .collect::<Vec<_>>();
        assert_eq!(mini_tokens, ["c4", "e4", "bd", "sd", "g4"]);
        assert_eq!(
            record["mini_locations"],
            serde_json::to_value(&layout.ui_layout.mini_locations).expect("mini ranges")
        );
    }

    #[test]
    fn visual_layout_enforces_visual_option_and_mini_location_bounds() {
        let revision = source_revision("score");
        assert_eq!(
            UiLayout::new(
                1,
                revision.clone(),
                vec![ui_visual(); MAX_UI_LAYOUT_VISUALS + 1],
                Vec::new(),
                Vec::new(),
            ),
            Err(UiLayoutValidationError::TooManyVisuals(
                MAX_UI_LAYOUT_VISUALS + 1
            ))
        );

        assert_eq!(
            UiLayout::new(
                1,
                revision.clone(),
                Vec::new(),
                vec![ui_slider(); MAX_UI_LAYOUT_SLIDERS + 1],
                Vec::new(),
            ),
            Err(UiLayoutValidationError::TooManySliders(
                MAX_UI_LAYOUT_SLIDERS + 1
            ))
        );

        let mut oversized = ui_visual();
        oversized.options = "x".repeat(MAX_UI_LAYOUT_OPTIONS_BYTES + 1);
        assert_eq!(
            UiLayout::new(1, revision.clone(), vec![oversized], Vec::new(), Vec::new(),),
            Err(UiLayoutValidationError::OversizedVisualOptions {
                index: 0,
                bytes: MAX_UI_LAYOUT_OPTIONS_BYTES + 1,
            })
        );

        assert_eq!(
            UiLayout::new(
                1,
                revision,
                Vec::new(),
                Vec::new(),
                vec![(0, 1); MAX_UI_LAYOUT_MINI_LOCATIONS + 1],
            ),
            Err(UiLayoutValidationError::TooManyMiniLocations(
                MAX_UI_LAYOUT_MINI_LOCATIONS + 1
            ))
        );
    }

    #[test]
    fn visual_layout_rejects_source_that_exceeds_wire_limits() {
        let too_many_visuals = std::iter::repeat_n("p.scope()", MAX_UI_LAYOUT_VISUALS + 1)
            .collect::<Vec<_>>()
            .join(";\n");
        assert_eq!(
            visual_layout(&too_many_visuals, 1),
            Err(UiLayoutValidationError::TooManyVisuals(
                MAX_UI_LAYOUT_VISUALS + 1
            ))
        );

        let too_many_sliders = std::iter::repeat_n("slider(.5)", MAX_UI_LAYOUT_SLIDERS + 1)
            .collect::<Vec<_>>()
            .join(";\n");
        assert_eq!(
            visual_layout(&too_many_sliders, 1),
            Err(UiLayoutValidationError::TooManySliders(
                MAX_UI_LAYOUT_SLIDERS + 1
            ))
        );

        let oversized_options = format!("p.scope({})", "x".repeat(MAX_UI_LAYOUT_OPTIONS_BYTES + 1));
        assert_eq!(
            visual_layout(&oversized_options, 1),
            Err(UiLayoutValidationError::OversizedVisualOptions {
                index: 0,
                bytes: MAX_UI_LAYOUT_OPTIONS_BYTES + 1,
            })
        );
    }

    #[test]
    fn a_live_control_exists_exactly_while_its_call_is_well_formed() {
        // Well formed: one control, whose call covers `slider(` through `)`
        // and whose value range covers the literal.
        let source = "$: s(\"bd\").gain(slider(0.5, 0, 1, 0.05))";
        let live = literal_sliders(source);
        assert_eq!(live.len(), 1);
        let control = &live[0];
        assert_eq!(
            &source[control.call.0..control.call.1],
            "slider(0.5, 0, 1, 0.05)"
        );
        assert_eq!(&source[control.slider.from..control.slider.to], "0.5");

        // The backspace that breaks the call takes the control with it.
        assert_eq!(
            literal_sliders("$: s(\"bd\").gain(slider(0.5, 0, 1, 0.05"),
            []
        );
        // An expression keeps its runtime meaning and is not a control.
        assert_eq!(
            literal_sliders("let x = 1\n$: s(\"bd\").gain(slider(x, 0, 1))"),
            []
        );
        // Impossible numbers are the linter's to refuse, not a control's to
        // wear.
        assert_eq!(
            literal_sliders("$: s(\"bd\").gain(slider(0, 0.1, 0.1, 1))"),
            []
        );
    }

    #[test]
    fn live_frequency_metadata_matches_control_aliases_without_changing_slider_wire_data() {
        for (canonical, names) in [
            ("cutoff", &["cutoff", "ctf", "lpf", "lp"][..]),
            ("hcutoff", &["hcutoff", "hpf", "hp"][..]),
            ("bandf", &["bandf", "bpf", "bp"][..]),
            ("freq", &["freq"][..]),
        ] {
            for name in names {
                assert_eq!(
                    rustel_core::controls::canonical_control_name(name),
                    Some(canonical)
                );
                let source = format!("s('saw').{name}(slider(440, 20, 20000, 0.25))");
                let live = literal_sliders(&source);
                assert_eq!(live.len(), 1);
                assert!(live[0].frequency, "{name}");
                let wire = serde_json::to_value(&live[0].slider).unwrap();
                assert!(wire.get("frequency").is_none());
                assert_eq!(wire.as_object().unwrap().len(), 7);
                assert_eq!(wire["value"], 440.0);
                assert_eq!(wire["step"], 0.25);
                let layout = visual_layout(&source, 1).unwrap();
                assert_eq!(layout.ui_layout.sliders, vec![live[0].slider.clone()]);
            }
        }
    }

    #[test]
    fn live_frequency_metadata_is_structural_and_keeps_unrelated_sliders_linear() {
        let source = r#"const shared = slider(440, 20, 20000);
s('saw').lpf(shared).gain(slider(440, 20, 20000));
s('saw').hpf /* a comment */ (((slider(440, 20, 20000))));
s('saw').bandf(slider(440, 20, 20000) * 2);
unknown(slider(440, 20, 20000));"#;
        let live = literal_sliders(source);
        assert_eq!(
            live.iter()
                .map(|slider| slider.frequency)
                .collect::<Vec<_>>(),
            [false, false, true, false, false]
        );
        for slider in live {
            assert_eq!(&source[slider.slider.from..slider.slider.to], "440");
            assert_eq!(
                &source[slider.call.0..slider.call.1],
                "slider(440, 20, 20000)"
            );
        }
    }

    #[test]
    fn an_unrelated_recoverable_error_keeps_live_frequency_travel_metadata() {
        let intact = "s('saw').lpf(slider(1000,100,10000,.01));";
        let expected = literal_sliders(intact);
        assert_eq!(expected.len(), 1);
        assert!(expected[0].frequency);
        // A missing const initializer is an OXC diagnostic with a recovered
        // AST, unlike a missing expression or a trailing member-access dot.
        let broken_elsewhere = format!("{intact}\nconst x;");
        assert!(
            !transpile(&broken_elsewhere, &TranspileOptions::default())
                .diagnostics
                .is_empty()
        );
        assert_eq!(literal_sliders(&broken_elsewhere), expected);
        let broken_here = "lpf(slider(1000,100,10000,.01), () => { const x; })";
        let local = literal_sliders(broken_here);
        assert_eq!(local.len(), 1);
        assert!(!local[0].frequency);
    }

    #[test]
    fn slider_layout_uses_the_strudel_default_step() {
        let layout = visual_layout("slider(.5, -2, 2)", 1).expect("slider layout");
        assert_eq!(layout.ui_layout.sliders.len(), 1);
        let slider = &layout.ui_layout.sliders[0];
        assert_eq!((slider.value, slider.min, slider.max), (0.5, -2.0, 2.0));
        assert_eq!(slider.step, 0.004);
    }

    #[test]
    fn dynamic_slider_bounds_run_but_are_not_advertised_as_native_controls() {
        let source = "const low = 0, high = 2; slider(.5, low, high, .1)";
        let layout = visual_layout(source, 1).expect("dynamic-bound slider layout");
        assert!(
            layout.ui_layout.sliders.is_empty(),
            "a dynamic range was misrepresented as a literal native control"
        );

        let mut session = crate::Session::new().expect("session");
        session
            .evaluate(source)
            .expect("dynamic-bound slider remains valid score code");
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query dynamic-bound slider");
        assert_eq!(haps[0].value.as_f64(), Some(0.5));
    }

    #[test]
    fn audio_frame_serializes_stable_bounded_shape_and_snapshot_metadata() {
        let audio = audio();
        audio.validate().expect("valid audio frame");
        let value = serde_json::to_value(&audio).expect("serialize audio frame");
        assert_eq!(
            value.as_object().expect("top-level object").keys().count(),
            1
        );
        let frame = &value["ui_audio"];
        assert_eq!(frame["version"], UI_AUDIO_PROTOCOL_VERSION);
        assert_eq!(frame["sequence"], 12);
        assert_eq!(frame["generation"], 17);
        assert_eq!(frame["device_time"], 4.25);
        assert_eq!(frame["stream_id"], 8);
        assert_eq!(frame["epoch"], 3);
        assert_eq!(frame["end_frame"], 2048);
        assert_eq!(frame["sample_rate"], 48_000);
        assert_eq!(
            frame["scope"].as_array().expect("scope").len(),
            UI_SCOPE_SAMPLES
        );
        assert_eq!(
            frame["spectrum"].as_array().expect("spectrum").len(),
            UI_SPECTRUM_BINS
        );
        assert_eq!(frame["scope"][3], -0.75);
        assert_eq!(
            frame["spectrum"][7],
            serde_json::to_value(0.9f32).expect("f32 JSON")
        );
    }

    #[test]
    fn audio_frame_rejects_invalid_numbers_and_sample_rates() {
        let mut invalid = audio();
        invalid.ui_audio.scope[5] = f32::NAN;
        assert_eq!(
            invalid.validate(),
            Err(UiAudioValidationError::NonFiniteScope(5))
        );
        invalid.ui_audio.scope[5] = 0.0;
        invalid.ui_audio.spectrum[6] = f32::INFINITY;
        assert_eq!(
            invalid.validate(),
            Err(UiAudioValidationError::NonFiniteSpectrum(6))
        );
        invalid.ui_audio.spectrum[6] = 0.0;
        invalid.ui_audio.sample_rate = 0;
        assert_eq!(
            invalid.validate(),
            Err(UiAudioValidationError::InvalidSampleRate(0))
        );
        invalid.ui_audio.sample_rate = 48_000;
        invalid.ui_audio.device_time = -0.1;
        assert_eq!(
            invalid.validate(),
            Err(UiAudioValidationError::InvalidDeviceTime)
        );
    }

    #[test]
    fn audio_frame_keeps_master_and_visuals_together() {
        let audio = UiAudioEnvelope::from_analysis_set(audio_metadata(), audio_analysis_set());
        audio.validate().expect("valid per-visual audio");
        let value = serde_json::to_value(&audio).expect("serialize");
        let frame = &value["ui_audio"];
        assert_eq!(frame["scope"][3], -0.75);
        assert_eq!(frame["visuals"][0]["slot"], 0);
        assert_eq!(frame["visuals"][0]["scope"][3], 0.25);
        assert_eq!(frame["visuals"][1]["slot"], 63);
        assert_eq!(frame["visuals"][1]["spectrum"][7], -18.0);
        assert_eq!(
            serde_json::from_value::<UiAudioEnvelope>(value).unwrap(),
            audio
        );
    }

    #[test]
    fn audio_frame_distinguishes_legacy_from_missing_visual_taps() {
        let legacy = audio();
        let value = serde_json::to_value(&legacy).unwrap();
        assert!(value["ui_audio"].get("visuals").is_none());
        assert_eq!(
            serde_json::from_value::<UiAudioEnvelope>(value).unwrap(),
            legacy
        );

        let mut analysis = audio_analysis_set();
        analysis.visuals.clear();
        let empty = UiAudioEnvelope::from_analysis_set(audio_metadata(), analysis);
        empty.validate().expect("empty visual set is valid");
        let value = serde_json::to_value(&empty).unwrap();
        assert_eq!(value["ui_audio"]["visuals"], serde_json::json!([]));
    }

    #[test]
    fn audio_frame_rejects_invalid_visuals() {
        let valid = UiAudioEnvelope::from_analysis_set(audio_metadata(), audio_analysis_set());
        let invalid = |edit: fn(&mut Vec<UiVisualAudioFrame>)| {
            let mut audio = valid.clone();
            edit(audio.ui_audio.visuals.as_mut().unwrap());
            audio.validate()
        };
        assert_eq!(
            invalid(|visuals| visuals.resize(65, visuals[0].clone())),
            Err(UiAudioValidationError::TooManyVisuals(65))
        );
        assert_eq!(
            invalid(|visuals| visuals[0].slot = 64),
            Err(UiAudioValidationError::InvalidVisualSlot(64))
        );
        assert_eq!(
            invalid(|visuals| visuals[1].slot = 0),
            Err(UiAudioValidationError::DuplicateVisualSlot(0))
        );
        assert_eq!(
            invalid(|visuals| {
                visuals[0].scope.pop();
            }),
            Err(UiAudioValidationError::InvalidScopeLength(511))
        );
        assert_eq!(
            invalid(|visuals| visuals[1].spectrum.clear()),
            Err(UiAudioValidationError::InvalidSpectrumLength(0))
        );
        assert_eq!(
            invalid(|visuals| visuals[0].scope[1] = f32::NAN),
            Err(UiAudioValidationError::NonFiniteScope(1))
        );
        assert_eq!(
            invalid(|visuals| visuals[1].spectrum[2] = f32::INFINITY),
            Err(UiAudioValidationError::NonFiniteSpectrum(2))
        );
    }

    #[test]
    fn rejects_non_finite_values_before_json_serialization() {
        let mut invalid = envelope();
        invalid.ui_events.device_time = f64::NAN;
        assert_eq!(
            invalid.validate(),
            Err(UiEventValidationError::NonFinite("device_time"))
        );
        invalid.ui_events.device_time = 1.0;
        invalid.ui_events.events[0].duration_seconds = f64::INFINITY;
        assert_eq!(
            invalid.validate(),
            Err(UiEventValidationError::NonFinite("duration_seconds"))
        );
    }

    #[test]
    fn rejects_events_from_a_different_generation() {
        let mut invalid = envelope();
        invalid.ui_events.events[0].generation += 1;
        assert_eq!(
            invalid.validate(),
            Err(UiEventValidationError::GenerationMismatch {
                event_index: 0,
                batch: 3,
                event: 4,
            })
        );
    }

    #[test]
    fn omits_only_an_oversized_optional_value() {
        let mut trace = trace();
        trace.value_show = Some("x".repeat(MAX_UI_EVENT_VALUE_BYTES + 1));
        let event = UiScheduledEvent::from_trace(&trace, 1.0).expect("valid event");
        assert_eq!(event.value, None);
        assert_eq!(event.context, vec![(4, 8), (11, 13)]);
        assert_eq!(event.whole_begin, "1/3");
    }

    #[test]
    fn trace_conversion_bounds_events_and_context_ranges() {
        let traces = vec![trace(); MAX_UI_EVENTS_PER_BATCH + 1];
        assert_eq!(
            UiEventBatch::from_traces(0.0, 0.0, 1.0, 3, source_revision("score"), traces.iter(), 0,),
            Err(UiEventValidationError::TooManyEvents(
                MAX_UI_EVENTS_PER_BATCH + 1
            ))
        );

        let mut many_ranges = trace();
        many_ranges.context = (0..MAX_UI_EVENT_CONTEXT_RANGES + 5)
            .map(|index| (index, index + 1))
            .collect();
        let event = UiScheduledEvent::from_owned_trace(many_ranges, 1.0).expect("bounded trace");
        assert_eq!(event.context.len(), MAX_UI_EVENT_CONTEXT_RANGES);
    }

    #[test]
    fn correlates_only_accepted_outputs_and_rejects_wrong_generations() {
        let mut pending = HashMap::from([
            (1, trace_with(1, 7)),
            (2, trace_with(2, 7)),
            (3, trace_with(3, 7)),
        ]);

        let correlation = correlate_submitted_traces(
            &mut pending,
            [
                UiAcceptedOnset {
                    generation: 7,
                    onset_id: 1,
                    frequency_hz: Some(55.0),
                    gain: Some(0.8),
                },
                UiAcceptedOnset {
                    generation: 8,
                    onset_id: 2,
                    frequency_hz: Some(110.0),
                    gain: Some(0.7),
                },
                UiAcceptedOnset {
                    generation: 7,
                    onset_id: 99,
                    frequency_hz: Some(220.0),
                    gain: Some(0.6),
                },
            ],
        );

        assert_eq!(
            correlation
                .ready
                .iter()
                .map(|trace| trace.onset_id)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(correlation.ready[0].frequency_hz, Some(55.0));
        assert_eq!(correlation.ready[0].gain, Some(0.8));
        assert_eq!(correlation.dropped, 1);
        assert_eq!(pending.keys().copied().collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn correlation_drops_invalid_submitted_audio_metadata() {
        let mut pending = HashMap::from([
            (1, trace_with(1, 7)),
            (2, trace_with(2, 7)),
            (3, trace_with(3, 7)),
        ]);
        let correlation = correlate_submitted_traces(
            &mut pending,
            [
                UiAcceptedOnset {
                    generation: 7,
                    onset_id: 1,
                    frequency_hz: Some(f32::NAN),
                    gain: Some(1.0),
                },
                UiAcceptedOnset {
                    generation: 7,
                    onset_id: 2,
                    frequency_hz: Some(440.0),
                    gain: Some(f32::INFINITY),
                },
                UiAcceptedOnset {
                    generation: 7,
                    onset_id: 3,
                    frequency_hz: Some(-1.0),
                    gain: Some(1.0),
                },
            ],
        );

        assert!(correlation.ready.is_empty());
        assert_eq!(correlation.dropped, 3);
        assert!(pending.is_empty());
    }

    #[test]
    fn correlation_keeps_sample_onsets_and_omits_the_zero_pitch_sentinel() {
        let mut pending = HashMap::from([(1, trace_with(1, 7))]);
        let correlation = correlate_submitted_traces(
            &mut pending,
            [UiAcceptedOnset {
                generation: 7,
                onset_id: 1,
                frequency_hz: Some(0.0),
                gain: Some(0.8),
            }],
        );

        assert_eq!(correlation.ready.len(), 1);
        assert_eq!(correlation.ready[0].onset_id, 1);
        assert_eq!(correlation.ready[0].frequency_hz, None);
        assert_eq!(correlation.ready[0].gain, Some(0.8));
        assert!(pending.is_empty());
    }

    /// A pitched sample's voice reports the zero sentinel too; the pitch the
    /// scheduler read from the score survives it. A voice with a frequency
    /// of its own still has the last word.
    #[test]
    fn correlation_keeps_the_scored_pitch_of_a_pitched_sample() {
        let pitched = ScheduleTraceEvent {
            frequency_hz: Some(261.63),
            ..trace_with(1, 7)
        };
        let synth = ScheduleTraceEvent {
            frequency_hz: Some(261.63),
            ..trace_with(2, 7)
        };
        let mut pending = HashMap::from([(1, pitched), (2, synth)]);
        let correlation = correlate_submitted_traces(
            &mut pending,
            [
                UiAcceptedOnset {
                    generation: 7,
                    onset_id: 1,
                    frequency_hz: Some(0.0),
                    gain: Some(0.8),
                },
                UiAcceptedOnset {
                    generation: 7,
                    onset_id: 2,
                    frequency_hz: Some(440.0),
                    gain: Some(0.8),
                },
            ],
        );
        assert_eq!(correlation.ready[0].frequency_hz, Some(261.63));
        assert_eq!(correlation.ready[1].frequency_hz, Some(440.0));
    }

    #[test]
    fn correlation_keeps_external_only_onsets_without_audio_metadata() {
        let mut pending = HashMap::from([(1, trace_with(1, 7))]);
        let correlation = correlate_submitted_traces(
            &mut pending,
            [UiAcceptedOnset {
                generation: 7,
                onset_id: 1,
                frequency_hz: None,
                gain: None,
            }],
        );

        assert_eq!(correlation.ready.len(), 1);
        assert_eq!(correlation.ready[0].onset_id, 1);
        assert_eq!(correlation.ready[0].frequency_hz, None);
        assert_eq!(correlation.ready[0].gain, None);
        assert!(pending.is_empty());
    }

    #[test]
    fn successful_ingest_purges_old_generations_and_enforces_the_exact_cap() {
        let mut pending = HashMap::from([(1, trace_with(1, 4)), (2, trace_with(2, 5))]);
        let fresh = vec![
            trace_with(3, 4),
            trace_with(4, 5),
            trace_with(5, 5),
            trace_with(6, 5),
        ];
        let mut dropped = 0;

        ingest_pending_traces(&mut pending, fresh, true, 5, 3, &mut dropped);

        assert_eq!(pending.len(), 3);
        assert!(pending.contains_key(&2));
        assert!(pending.contains_key(&4));
        assert!(pending.contains_key(&5));
        assert!(!pending.contains_key(&1), "old pending generation survived");
        assert!(
            !pending.contains_key(&3),
            "old fresh generation was admitted"
        );
        assert!(!pending.contains_key(&6), "capacity was exceeded");
        assert_eq!(dropped, 3, "purge, stale fresh trace, and overflow");
    }

    #[test]
    fn failed_ingest_discards_fresh_traces_but_preserves_retryable_pending_state() {
        let mut pending = HashMap::from([(1, trace_with(1, 4))]);
        let mut dropped = u64::MAX - 1;

        ingest_pending_traces(
            &mut pending,
            [trace_with(2, 5), trace_with(3, 5)],
            false,
            5,
            8,
            &mut dropped,
        );

        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key(&1));
        assert_eq!(dropped, u64::MAX, "loss accounting wrapped");
    }

    #[test]
    fn empty_successful_ingest_leaves_generation_cleanup_to_the_periodic_prune() {
        let mut pending = HashMap::from([(1, trace_with(1, 4))]);
        let mut dropped = 0;

        ingest_pending_traces(&mut pending, Vec::new(), true, 5, 8, &mut dropped);

        assert!(pending.contains_key(&1));
        assert_eq!(dropped, 0);
    }

    #[test]
    fn stale_prune_is_generation_safe_and_keeps_the_inclusive_grace_boundary() {
        let mut pending = HashMap::from([
            (1, trace_at(1, 4, 100.0)),
            (2, trace_at(2, 5, 9.0)),
            (3, trace_at(3, 5, 8.999)),
            (4, trace_at(4, 5, 10.25)),
        ]);
        let mut dropped = 7;

        prune_pending_traces(&mut pending, 5, 10.0, 1.0, &mut dropped);

        assert_eq!(pending.len(), 2);
        assert!(pending.contains_key(&2), "inclusive boundary was pruned");
        assert!(pending.contains_key(&4), "future trace was pruned");
        assert!(!pending.contains_key(&1), "old generation survived");
        assert!(!pending.contains_key(&3), "expired trace survived");
        assert_eq!(dropped, 9);
    }

    #[test]
    fn accepted_traces_are_withheld_until_layout_is_ready_then_released() {
        let mut withheld = Vec::new();
        let mut dropped = 0;

        let first = release_traces_when_layout_ready(
            &mut withheld,
            vec![trace_with(1, 5), trace_with(2, 5)],
            false,
            5,
            8,
            &mut dropped,
        );
        assert!(
            first.is_empty(),
            "layout gate must withhold accepted onsets"
        );
        assert_eq!(withheld.len(), 2);
        assert_eq!(dropped, 0);

        let second = release_traces_when_layout_ready(
            &mut withheld,
            vec![trace_with(3, 5)],
            true,
            5,
            8,
            &mut dropped,
        );
        assert_eq!(
            second
                .iter()
                .map(|trace| trace.onset_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(withheld.is_empty());
        assert_eq!(dropped, 0);
    }

    #[test]
    fn withheld_traces_drop_only_on_generation_rollover_or_capacity() {
        let mut withheld = vec![trace_with(1, 4), trace_with(2, 5)];
        let mut dropped = 0;

        let released = release_traces_when_layout_ready(
            &mut withheld,
            vec![trace_with(3, 5), trace_with(4, 5)],
            false,
            5,
            2,
            &mut dropped,
        );
        assert!(released.is_empty());
        assert_eq!(
            withheld
                .iter()
                .map(|trace| trace.onset_id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        // old generation purged + one overflow past capacity
        assert_eq!(dropped, 2);
    }

    #[test]
    fn dispatch_groups_generations_and_chunks_at_the_protocol_limit() {
        let generation_sources = BTreeMap::from([
            (3, (source_revision("old score"), 1.5)),
            (4, (source_revision("new score"), 2.0)),
        ]);
        let mut traces = vec![trace_with(1, 3), trace_with(2, 3)];
        traces.extend(
            (0..MAX_UI_EVENTS_PER_BATCH + 1).map(|index| trace_with(100 + index as u64, 4)),
        );
        let mut requests = Vec::new();
        let mut dropped = 0;

        dispatch_trace_batches(
            12.0,
            4.5,
            4,
            traces,
            &generation_sources,
            &mut dropped,
            |request| {
                requests.push(request);
                UiEventSendStatus::Queued
            },
        );

        assert_eq!(dropped, 0);
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests
                .iter()
                .map(|request| (request.generation, request.traces.len()))
                .collect::<Vec<_>>(),
            vec![(3, 2), (4, MAX_UI_EVENTS_PER_BATCH), (4, 1)]
        );
        for request in &requests {
            assert!(request.traces.len() <= MAX_UI_EVENTS_PER_BATCH);
            assert!(
                request
                    .traces
                    .iter()
                    .all(|trace| trace.generation == request.generation)
            );
        }
        assert_eq!(requests[0].cps, 1.5);
        assert_eq!(requests[1].cps, 2.0);
        assert_eq!(requests[1].source_revision, source_revision("new score"));
    }

    #[test]
    fn unknown_generation_loss_is_carried_into_the_next_known_record() {
        let generation_sources = BTreeMap::from([(4, (source_revision("known score"), 2.0))]);
        let mut requests = Vec::new();
        let mut dropped = 0;

        dispatch_trace_batches(
            12.0,
            4.5,
            4,
            vec![trace_with(1, 3), trace_with(2, 4)],
            &generation_sources,
            &mut dropped,
            |request| {
                requests.push(request);
                UiEventSendStatus::Queued
            },
        );

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].generation, 4);
        assert_eq!(requests[0].traces[0].onset_id, 2);
        assert_eq!(requests[0].dropped, 1);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn failed_dispatch_counts_its_events_until_a_record_is_queued() {
        let generation_sources = BTreeMap::from([(3, (source_revision("score"), 2.0))]);
        let traces = (0..MAX_UI_EVENTS_PER_BATCH + 1)
            .map(|index| trace_with(index as u64, 3))
            .collect();
        let mut reported = Vec::new();
        let mut outcomes = [UiEventSendStatus::DroppedFull, UiEventSendStatus::Queued].into_iter();
        let mut dropped = 7;

        dispatch_trace_batches(
            12.0,
            4.5,
            3,
            traces,
            &generation_sources,
            &mut dropped,
            |request| {
                reported.push((request.traces.len(), request.dropped));
                outcomes.next().expect("one outcome per chunk")
            },
        );

        assert_eq!(
            reported,
            vec![
                (MAX_UI_EVENTS_PER_BATCH, 7),
                (1, 7 + MAX_UI_EVENTS_PER_BATCH as u64),
            ]
        );
        assert_eq!(dropped, 0);

        dispatch_trace_batches(
            13.0,
            5.0,
            3,
            Vec::new(),
            &generation_sources,
            &mut dropped,
            |_| panic!("zero loss should not emit an empty record"),
        );
    }

    #[test]
    fn accumulated_loss_can_be_flushed_by_an_empty_record() {
        let generation_sources = BTreeMap::from([(3, (source_revision("score"), 2.0))]);
        let mut requests = Vec::new();
        let mut dropped = 11;

        dispatch_trace_batches(
            12.0,
            4.5,
            3,
            Vec::new(),
            &generation_sources,
            &mut dropped,
            |request| {
                requests.push(request);
                UiEventSendStatus::Queued
            },
        );

        assert_eq!(requests.len(), 1);
        assert!(requests[0].traces.is_empty());
        assert_eq!(requests[0].dropped, 11);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn blocked_writer_only_fills_its_bounded_queue_and_sink_drop_never_joins_it() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let released = Arc::new((Mutex::new(false), Condvar::new()));
        let writer = GatedWriter {
            entered: entered_tx,
            released: Arc::clone(&released),
            finished: finished_tx,
        };
        let sink = UiEventSink::with_writer(1, writer).expect("spawn gated writer");

        assert_eq!(sink.try_send(envelope()), UiEventSendStatus::Queued);
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer did not enter its blocking write");
        assert_eq!(sink.try_send(envelope()), UiEventSendStatus::Queued);
        assert_eq!(
            sink.try_send(envelope()),
            UiEventSendStatus::DroppedFull,
            "producer waited for or exceeded the one-record queue"
        );

        let (sink_dropped_tx, sink_dropped_rx) = mpsc::channel();
        std::thread::spawn(move || {
            drop(sink);
            let _ = sink_dropped_tx.send(());
        });
        sink_dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("dropping the sink joined a blocked writer");

        let (release_lock, wake) = &*released;
        *release_lock.lock().expect("release lock") = true;
        wake.notify_all();
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("detached writer did not drain and exit after release");
    }

    #[test]
    fn full_queue_returns_layout_ownership_for_allocation_free_retry() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let released = Arc::new((Mutex::new(false), Condvar::new()));
        let writer = GatedWriter {
            entered: entered_tx,
            released: Arc::clone(&released),
            finished: finished_tx,
        };
        let sink = UiEventSink::with_writer(1, writer).expect("spawn gated writer");
        assert_eq!(sink.try_send(envelope()), UiEventSendStatus::Queued);
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer did not enter its blocking write");
        assert_eq!(sink.try_send(envelope()), UiEventSendStatus::Queued);

        let pending = layout();
        let (status, recovered) = sink
            .try_send_layout_recover(pending.clone())
            .expect_err("full queue accepted layout");
        assert_eq!(status, UiEventSendStatus::DroppedFull);
        assert_eq!(recovered, pending);

        let (release_lock, wake) = &*released;
        *release_lock.lock().expect("release lock") = true;
        wake.notify_all();
        drop(sink);
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer did not finish after release");
    }

    #[test]
    fn audio_coalesces_outside_control_queue_and_control_drains_first() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let released = Arc::new((Mutex::new(false), Condvar::new()));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let writer = FirstWriteGatedSharedWriter {
            captured: Arc::clone(&captured),
            entered: entered_tx,
            released: Arc::clone(&released),
            first: true,
        };
        let sink = UiEventSink::with_writer(1, writer).expect("spawn gated writer");
        assert_eq!(sink.try_send(envelope()), UiEventSendStatus::Queued);
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer did not block on first control record");
        assert_eq!(sink.try_send_layout(layout()), UiEventSendStatus::Queued);

        let first_audio = audio();
        let mut newest_audio = audio();
        newest_audio.ui_audio.sequence += 1;
        assert_eq!(sink.try_send_audio(first_audio), UiEventSendStatus::Queued);
        assert_eq!(
            sink.try_send_audio(newest_audio.clone()),
            UiEventSendStatus::Queued
        );
        assert_eq!(sink.stats().dropped_audio, 1);

        let (release_lock, wake) = &*released;
        *release_lock.lock().expect("release lock") = true;
        wake.notify_all();
        sink.finish().expect("writer thread");

        let bytes = captured.lock().expect("captured output").clone();
        let records = std::str::from_utf8(&bytes)
            .expect("NDJSON is UTF-8")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON record"))
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 3);
        assert!(records[0].get("ui_events").is_some());
        assert!(records[1].get("ui_layout").is_some());
        assert_eq!(records[2]["ui_audio"]["sequence"], 13);
    }

    #[test]
    fn timeout_race_takes_a_queued_layout_before_published_audio() {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .try_send(UiEventWrite::Layout(layout()))
            .expect("queue layout");
        let latest_audio = Mutex::new(Some(UiAudioWrite::Envelope(audio())));
        let mut control = Vec::new();
        let mut disconnected = false;

        let taken_audio = take_audio_after_queued_controls(
            &receiver,
            &latest_audio,
            1,
            &mut control,
            &mut disconnected,
        );

        assert!(!disconnected);
        assert!(matches!(control.as_slice(), [UiEventWrite::Layout(_)]));
        assert!(matches!(taken_audio, Some(UiAudioWrite::Envelope(_))));
    }

    #[test]
    fn audio_reports_disconnected_after_writer_failure() {
        let (attempted_tx, attempted_rx) = mpsc::channel();
        let sink =
            UiEventSink::with_writer(1, FailingWriter(attempted_tx)).expect("spawn failing writer");
        assert_eq!(sink.try_send(envelope()), UiEventSendStatus::Queued);
        attempted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer did not attempt output");

        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if sink.try_send_audio(audio()) == UiEventSendStatus::Disconnected {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "audio slot stayed connected after writer failure"
            );
            std::thread::yield_now();
        }
        assert_eq!(sink.stats().write_errors, 1);
        sink.finish().expect("writer thread");
    }

    #[test]
    fn background_writer_emits_one_ndjson_record() {
        let writer = SharedWriter::default();
        let captured = Arc::clone(&writer.0);
        let sink = UiEventSink::with_writer(1, writer).expect("spawn writer");
        assert_eq!(sink.try_send(envelope()), UiEventSendStatus::Queued);
        sink.finish().expect("writer thread");

        let bytes = captured.lock().expect("captured output").clone();
        assert_eq!(bytes.iter().filter(|&&byte| byte == b'\n').count(), 1);
        let decoded: UiEventEnvelope =
            serde_json::from_slice(&bytes).expect("one JSON record followed by whitespace");
        assert_eq!(decoded, envelope());
    }

    #[test]
    fn background_writer_emits_one_layout_record() {
        let writer = SharedWriter::default();
        let captured = Arc::clone(&writer.0);
        let sink = UiEventSink::with_writer(1, writer).expect("spawn writer");
        assert_eq!(sink.try_send_layout(layout()), UiEventSendStatus::Queued);
        sink.finish().expect("writer thread");

        let bytes = captured.lock().expect("captured output").clone();
        assert_eq!(bytes.iter().filter(|&&byte| byte == b'\n').count(), 1);
        let decoded: UiLayoutEnvelope =
            serde_json::from_slice(&bytes).expect("one layout record followed by whitespace");
        assert_eq!(decoded, layout());
    }

    #[test]
    fn background_writer_emits_one_audio_record() {
        let writer = SharedWriter::default();
        let captured = Arc::clone(&writer.0);
        let sink = UiEventSink::with_writer(1, writer).expect("spawn writer");
        assert_eq!(
            sink.try_send_audio_analysis(audio_metadata(), audio_analysis_set()),
            UiEventSendStatus::Queued
        );
        sink.finish().expect("writer thread");

        let bytes = captured.lock().expect("captured output").clone();
        assert_eq!(bytes.iter().filter(|&&byte| byte == b'\n').count(), 1);
        let decoded: UiAudioEnvelope =
            serde_json::from_slice(&bytes).expect("one audio record followed by whitespace");
        assert_eq!(
            decoded,
            UiAudioEnvelope::from_analysis_set(audio_metadata(), audio_analysis_set())
        );
    }

    #[test]
    fn background_writer_converts_owned_traces() {
        let writer = SharedWriter::default();
        let captured = Arc::clone(&writer.0);
        let sink = UiEventSink::with_writer(1, writer).expect("spawn writer");
        assert_eq!(
            sink.try_send_traces(
                12.0,
                4.5,
                2.0,
                3,
                source_revision("score"),
                vec![trace()],
                0,
            ),
            UiEventSendStatus::Queued
        );
        sink.finish().expect("writer thread");

        let bytes = captured.lock().expect("captured output").clone();
        let decoded: UiEventEnvelope = serde_json::from_slice(&bytes).expect("trace record");
        assert_eq!(decoded.ui_events.events[0].context, vec![(4, 8), (11, 13)]);
        assert_eq!(decoded.ui_events.source_revision, source_revision("score"));
    }

    #[test]
    fn writer_discards_invalid_records_without_serializing_null_floats() {
        let writer = SharedWriter::default();
        let captured = Arc::clone(&writer.0);
        let sink = UiEventSink::with_writer(1, writer).expect("spawn writer");
        let mut invalid = envelope();
        invalid.ui_events.cycle = f64::NAN;
        assert_eq!(sink.try_send(invalid), UiEventSendStatus::Queued);
        sink.finish().expect("writer thread");

        assert!(captured.lock().expect("captured output").is_empty());
    }

    #[test]
    fn writer_discards_invalid_layout_records() {
        let writer = SharedWriter::default();
        let captured = Arc::clone(&writer.0);
        let sink = UiEventSink::with_writer(1, writer).expect("spawn writer");
        let mut invalid = layout();
        invalid.ui_layout.mini_locations = vec![(5, 5)];
        assert_eq!(sink.try_send_layout(invalid), UiEventSendStatus::Queued);
        sink.finish().expect("writer thread");

        assert!(captured.lock().expect("captured output").is_empty());
    }

    #[test]
    fn writer_discards_invalid_audio_records() {
        let writer = SharedWriter::default();
        let captured = Arc::clone(&writer.0);
        let sink = UiEventSink::with_writer(1, writer).expect("spawn writer");
        let mut invalid = audio();
        invalid.ui_audio.device_time = f64::NAN;
        assert_eq!(sink.try_send_audio(invalid), UiEventSendStatus::Queued);
        sink.finish().expect("writer thread");

        assert!(captured.lock().expect("captured output").is_empty());
    }

    #[test]
    fn a_rejected_trace_batch_hands_its_dropped_count_to_the_next_accepted_one() {
        let writer = SharedWriter::default();
        let captured = Arc::clone(&writer.0);
        let sink = UiEventSink::with_writer(4, writer).expect("spawn writer");

        // A non-positive cps fails conversion after the queue accepted the
        // record, so its carried gap count would be lost without recovery.
        assert_eq!(
            sink.try_send_traces(
                1.0,
                0.0,
                0.0,
                1,
                source_revision("score"),
                vec![trace_with(1, 1)],
                7,
            ),
            UiEventSendStatus::Queued
        );
        // The valid successor must report the rejected batch's tally.
        assert_eq!(
            sink.try_send_traces(
                2.0,
                1.0,
                2.0,
                1,
                source_revision("score"),
                vec![trace_with(2, 1)],
                3,
            ),
            UiEventSendStatus::Queued
        );
        sink.finish().expect("writer thread");

        let output = captured.lock().expect("captured output");
        let text = String::from_utf8(output.clone()).expect("utf-8 NDJSON");
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid NDJSON"))
            .collect();
        assert_eq!(lines.len(), 1, "the invalid batch was discarded: {lines:?}");
        // 7 carried gaps + the rejected batch's own trace + 3 successor gaps.
        assert_eq!(lines[0]["ui_events"]["dropped"], 11);
        assert_eq!(lines[0]["ui_events"]["events"][0]["onset_id"], 2);
    }

    #[test]
    fn zero_capacity_is_rejected() {
        let error = UiEventSink::with_writer(0, io::sink()).expect_err("zero capacity");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
