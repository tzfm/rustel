//! Offline render placeholders: silent WAV or raw onset dump.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

/// Whether progress lines are JSON, for a script, rather than text for a
/// person. Set once by the command line from its own `--json`.
static PROGRESS_JSON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_progress_json(on: bool) {
    PROGRESS_JSON.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// A line about how far a long job has got, on stderr: the JSON object
/// when the run was asked for JSON, the sentence otherwise - one or the
/// other, never both on one run.
pub(crate) fn report_progress(json: serde_json::Value, text: impl FnOnce() -> String) {
    if PROGRESS_JSON.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("{json}");
    } else {
        eprintln!("{}", text());
    }
}

use rustel_audio::DspDispatch;

use crate::hap_json::OnsetEventJson;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderFormat {
    /// 16-bit PCM WAV filled with silence. DSP is not claimed.
    Wav,
    /// Deterministic audible sine/envelope PCM from explicit note/frequency
    /// controls. This is a smoke backend, not a Chrome-fidelity claim.
    ScalarWav,
    /// JSON onset timeline written beside or instead of audio.
    OnsetJson,
    /// The scalar render written as 32-bit float WAV, unclamped.
    ///
    /// For comparing against a browser, whose `OfflineAudioContext` returns
    /// unclamped floats: 16-bit clips at ±1.0, so a loud score is measured
    /// through the file format instead of the engine.
    ScalarF32Wav,
    /// The ScalarWav render encoded as 320 kbps mp3 (LAME).
    /// Returns an unsupported-format error unless the `mp3-export` feature is
    /// enabled.
    ScalarMp3,
}

/// Whether this build encodes [`RenderFormat::ScalarMp3`].
pub const MP3_EXPORT: bool = cfg!(feature = "mp3-export");

/// Convert a scheduled onset through the shared `rustel-voice` resolver. All
/// hosts use the same mapping. This adapter translates the runtime's
/// `ValueJson` into a JSON value and reads the cycle from `whole_begin`.
///
/// The event comes back with no live binding on it. Only [`bind_live_controls`]
/// attaches one, and only the live device path calls it.
pub fn scalar_event(
    onset: &OnsetEventJson,
    sample_rate: u32,
    cps: f64,
    samples: &dyn rustel_voice::SampleLookup,
) -> Result<rustel_audio::OnsetEvent, String> {
    scalar_event_detailed(onset, sample_rate, cps, samples).map_err(|error| error.to_string())
}

pub(crate) fn scalar_event_detailed(
    onset: &OnsetEventJson,
    sample_rate: u32,
    cps: f64,
    samples: &dyn rustel_voice::SampleLookup,
) -> Result<rustel_audio::OnsetEvent, rustel_voice::VoiceError> {
    let value = match &onset.value {
        crate::ValueJson::Null => serde_json::Value::Null,
        crate::ValueJson::Bool(flag) => serde_json::Value::Bool(*flag),
        crate::ValueJson::Number(midi) => serde_json::Number::from_f64(*midi)
            .map(serde_json::Value::Number)
            .ok_or_else(|| format!("MIDI note must be finite, got {midi}"))?,
        crate::ValueJson::String(note) => serde_json::Value::String(note.clone()),
        crate::ValueJson::Raw(raw) => raw.clone(),
    };
    // An onset with no readable `whole_begin` has no cycle of its own. The
    // clock time then serves as the musical time.
    let cycle = whole_begin_cycle(&onset.whole_begin).unwrap_or(onset.target_time * cps);
    rustel_voice::resolve_voice_with_samples_detailed(
        &value,
        onset.onset_id,
        onset.duration_secs,
        onset.target_time,
        cycle,
        sample_rate,
        cps,
        samples,
    )
    .map(|mut event| {
        event.controls.preview_epoch = value
            .get("__rustelPreview")
            .and_then(serde_json::Value::as_f64)
            .filter(|epoch| (1.0..=9_007_199_254_740_991.0).contains(epoch) && epoch.fract() == 0.0)
            .map(|epoch| epoch as u64)
            .unwrap_or(0);
        event
            .with_generation(onset.generation)
            .with_ui_visuals(onset.ui_visuals)
    })
}

/// The cycle `whole_begin` names. The text is the scheduler's fraction in its
/// `n/d` form. The result equals the fraction's `to_f64`. The parse does not
/// allocate.
fn whole_begin_cycle(whole_begin: &str) -> Option<f64> {
    let (numer, denom) = whole_begin.split_once('/')?;
    let cycle = rustel_fraction::Fraction::checked_new(numer.parse().ok()?, denom.parse().ok()?)?;
    Some(cycle.to_f64())
}

/// Hand the voice the slider tokens its gain, cutoff and resonance came from,
/// so a drag moves a note which is already sounding.
///
/// Only the live device is ever handed a new value: it is the one path with a
/// control ring, and everywhere else `Ramp::next` returns the starting value
/// forever. Binding regardless is not free - a bound gain leaves the source
/// amplitude and is multiplied back in per sample instead, and that different
/// f32 association moved about a third of the samples of
/// `corpus/songs/avril-14th` by one ULP. A bounce nobody can touch while it
/// runs must not pay for a slider that cannot move.
pub(crate) fn bind_live_controls(event: &mut rustel_audio::OnsetEvent, onset: &OnsetEventJson) {
    event.controls.live_controls = onset.live_controls;
    // Gain modulation currently normalizes against the onset's base and
    // has its own placement around nonlinear effects. Keep that existing
    // graph unchanged instead of claiming a direct live binding for it.
    let gain_target = rustel_audio::ModTarget::Gain;
    if event
        .controls
        .lfos
        .iter()
        .flatten()
        .any(|control| control.fxi.is_none() && control.target == gain_target)
        || event
            .controls
            .envs
            .iter()
            .flatten()
            .any(|control| control.fxi.is_none() && control.target == gain_target)
        || event
            .controls
            .bus_mods
            .iter()
            .flatten()
            .any(|control| control.fxi.is_none() && control.target == gain_target)
    {
        event.controls.live_controls[0] = 0;
    }
    // A filter envelope owns its own absolute cutoff automation. Until
    // those envelope endpoints can be rebound, keep its query-time path.
    // The envelope leaves the resonance alone, so its binding stays.
    if event.controls.filters.lowpass_envelope.is_some() {
        event.controls.live_controls[1] = 0;
    }
}

// Only the device-audio play path builds full event batches today.
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub(crate) fn scalar_events(
    onsets: &[OnsetEventJson],
    sample_rate: u32,
    cps: f64,
    samples: &dyn rustel_voice::SampleLookup,
) -> Result<Vec<rustel_audio::OnsetEvent>, String> {
    onsets
        .iter()
        .map(|onset| scalar_event(onset, sample_rate, cps, samples))
        .collect()
}

pub(crate) fn live_audio_event(
    onset: &OnsetEventJson,
    sample_rate: u32,
    cps: f64,
    samples: &dyn rustel_voice::SampleLookup,
) -> Result<rustel_audio::AudioEvent, rustel_voice::VoiceError> {
    let mut scalar = scalar_event_detailed(onset, sample_rate, cps, samples)?;
    bind_live_controls(&mut scalar, onset);
    Ok(rustel_audio::AudioEvent {
        onset_id: onset.onset_id,
        generation: onset.generation,
        ui_visuals: onset.ui_visuals,
        target_frame: scalar.onset_frame,
        onset_lead: scalar.onset_lead,
        freq_hz: scalar.freq_hz,
        gain: scalar.gain,
        duration_secs: scalar.duration_secs,
        controls: scalar.controls,
        sample: scalar.sample,
        wavetable: scalar.wavetable,
        synth: scalar.synth,
        cut: None,
    })
}

/// Convert the scheduler's floating onset time to the exact frame boundary
/// used by the audio voice resolver.
///
/// Audio absorbs sub-picosecond floating dust before taking the first frame at
/// or after an onset. MIDI cutover must make the identical decision or an
/// onset on the boundary could survive in one output and be replaced in the
/// other.
pub fn onset_frame_at(target_time: f64, sample_rate: u32) -> u64 {
    let exact_frame = target_time * f64::from(sample_rate);
    let nearest = exact_frame.round();
    let exact_frame = if (exact_frame - nearest).abs() < 1e-6 {
        nearest
    } else {
        exact_frame
    };
    let onset_frame = exact_frame.ceil();
    if !onset_frame.is_finite() || onset_frame < 0.0 || onset_frame > u64::MAX as f64 {
        u64::MAX
    } else {
        onset_frame as u64
    }
}

/// The device frame a takeover at `takeover_time` flips generations on. The
/// device keeps the outgoing generation's events aimed before this frame and
/// drops the rest.
pub(crate) fn takeover_frame_at(takeover_time: f64, sample_rate: u32) -> u64 {
    (takeover_time * f64::from(sample_rate)).round().max(0.0) as u64
}

/// The first instant, in seconds, whose onset [`onset_frame_at`] puts on
/// `frame` or later: an onset is before this instant exactly when its frame
/// is before `frame`.
///
/// It is not `frame / sample_rate`. An onset takes the first frame at or
/// after its time, so the instants of `frame` begin just past the frame
/// before it, where the dust `onset_frame_at` absorbs ends.
pub(crate) fn onset_frame_edge(frame: u64, sample_rate: u32) -> f64 {
    /// The quotient below and the product in `onset_frame_at` each round,
    /// which leaves the estimate a few units in the last place off the edge.
    const SEARCH_STEPS: usize = 8;
    let rate = f64::from(sample_rate);
    if frame == 0 {
        // No onset is before the first frame. One frame before time zero
        // is earlier than each onset that `onset_frame_at` puts on a frame.
        return -1.0 / rate;
    }
    let mut edge = (frame as f64 - 1.0 + 1e-6) / rate;
    for _ in 0..SEARCH_STEPS {
        if onset_frame_at(edge, sample_rate) >= frame {
            break;
        }
        edge = edge.next_up();
    }
    for _ in 0..SEARCH_STEPS {
        let before = edge.next_down();
        if onset_frame_at(before, sample_rate) < frame {
            break;
        }
        edge = before;
    }
    edge
}

/// What a build without the `mp3-export` feature says to an MP3 export.
const MP3_EXPORT_UNAVAILABLE: &str = "MP3 export requires the `mp3-export` feature";

fn mp3_unavailable() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, MP3_EXPORT_UNAVAILABLE)
}

/// Refuse an MP3 export before any rendering in a build without the encoder.
pub(crate) fn ensure_mp3_available() -> Result<(), crate::RuntimeError> {
    if MP3_EXPORT {
        Ok(())
    } else {
        Err(crate::RuntimeError::Unsupported(
            MP3_EXPORT_UNAVAILABLE.into(),
        ))
    }
}

/// Render with the prepared kernel selection, then LAME-encode at 320 kbps.
/// Answers the frames written and the voices the render refused, as
/// [`render_scalar_pcm_with_refusals`] does.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_scalar_mp3_with_dispatch(
    path: impl AsRef<Path>,
    sample_rate: u32,
    duration_secs: f64,
    onsets: &[OnsetEventJson],
    cps: f64,
    library: Option<&crate::samples::SampleLibrary>,
    dispatch: DspDispatch,
    max_polyphony: usize,
) -> io::Result<(usize, Vec<String>)> {
    if !MP3_EXPORT {
        return Err(mp3_unavailable());
    }
    let (pcm, refused) = render_scalar_pcm_with_refusals(
        sample_rate,
        duration_secs,
        onsets,
        cps,
        library,
        dispatch,
        max_polyphony,
    )?;
    Ok((encode_mp3(path, sample_rate, &pcm)?, refused))
}

/// Encode interleaved stereo f32 as 320 kbps mp3 (LAME), quantised exactly
/// as the WAV writer would.
/// Returns an unsupported-format error without the `mp3-export` feature.
#[cfg(feature = "mp3-export")]
pub fn encode_mp3(path: impl AsRef<Path>, sample_rate: u32, pcm: &[f32]) -> io::Result<usize> {
    use mp3lame_encoder::{Builder, FlushNoGap, InterleavedPcm};

    let mut builder =
        Builder::new().ok_or_else(|| io::Error::other("cannot initialise the mp3 encoder"))?;
    builder
        .set_num_channels(2)
        .map_err(|error| io::Error::other(format!("mp3 channels: {error}")))?;
    builder
        .set_sample_rate(sample_rate)
        .map_err(|error| io::Error::other(format!("mp3 sample rate: {error}")))?;
    builder
        .set_brate(mp3lame_encoder::Bitrate::Kbps320)
        .map_err(|error| io::Error::other(format!("mp3 bitrate: {error}")))?;
    builder
        .set_quality(mp3lame_encoder::Quality::Best)
        .map_err(|error| io::Error::other(format!("mp3 quality: {error}")))?;
    let mut encoder = builder
        .build()
        .map_err(|error| io::Error::other(format!("mp3 encoder: {error}")))?;

    // f32 stereo interleaved -> i16 like the WAV writer quantises.
    let quantised: Vec<i16> = pcm
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect();
    let mut mp3 = Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(
        quantised.len() / 2,
    ));
    let encoded = encoder
        .encode(InterleavedPcm(&quantised), mp3.spare_capacity_mut())
        .map_err(|error| io::Error::other(format!("mp3 encode: {error}")))?;
    // SAFETY: the encoder reports how many of the reserved bytes it wrote.
    unsafe { mp3.set_len(encoded) };
    let flushed = encoder
        .flush::<FlushNoGap>(mp3.spare_capacity_mut())
        .map_err(|error| io::Error::other(format!("mp3 flush: {error}")))?;
    // SAFETY: as above, within the same reservation.
    unsafe { mp3.set_len(mp3.len() + flushed) };
    std::fs::write(path, &mp3)?;
    Ok(quantised.len() / 2)
}

#[cfg(not(feature = "mp3-export"))]
pub fn encode_mp3(_path: impl AsRef<Path>, _sample_rate: u32, _pcm: &[f32]) -> io::Result<usize> {
    Err(mp3_unavailable())
}

/// Bytes per stereo PCM16 frame, the format used by the watched MP3 route's
/// temporary WAV and by `encode_mp3` before encoding.
const PCM16_STEREO_FRAME_BYTES: u64 = 4;

/// Match the frame limit of `write_wav_controlled` for a PCM16 stereo export:
/// the RIFF size field allows `u32::MAX` bytes, including 36 non-PCM bytes.
///
/// This keeps PCM16 WAV and both MP3 routes on the same frame budget, regardless
/// of flags such as `--until-silence`. It is not a memory-byte limit: these
/// routes retain stereo f32 PCM (eight bytes per frame), and MP3 encoding
/// also makes an i16 copy.
const MAX_IN_MEMORY_FRAMES: u64 = (u32::MAX as u64 - 36) / PCM16_STEREO_FRAME_BYTES;

/// The frame count a scalar render of `duration_secs` at `sample_rate`
/// holds, refused when it is not representable or is past
/// [`MAX_IN_MEMORY_FRAMES`]. The comparison is made in `f64` before any
/// cast, so no frame count can wrap on its way to the check.
fn in_memory_render_frames(sample_rate: u32, duration_secs: f64) -> io::Result<usize> {
    let frames = (duration_secs * f64::from(sample_rate)).round();
    if !frames.is_finite() || frames < 0.0 || frames > usize::MAX as f64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scalar render frame count is not representable",
        ));
    }
    if frames > MAX_IN_MEMORY_FRAMES as f64 {
        let pcm_bytes = frames * PCM16_STEREO_FRAME_BYTES as f64;
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{frames:.0} frames need {pcm_bytes:.0} PCM bytes as 16-bit stereo, which an \
                 in-memory render cannot hold (max {MAX_IN_MEMORY_FRAMES} frames)"
            ),
        ));
    }
    Ok(frames as usize)
}

/// The shared in-memory scalar render: prefetch, per-voice skip-and-log,
/// backend preload, PCM out. Answers the PCM and the distinct
/// `voice_refused` messages the render skipped, which a Session queues as
/// diagnostics when direct logging is off, so an offline caller is not left
/// with a silent buffer and no explanation.
pub(crate) fn render_scalar_pcm_with_refusals(
    sample_rate: u32,
    duration_secs: f64,
    onsets: &[OnsetEventJson],
    cps: f64,
    library: Option<&crate::samples::SampleLibrary>,
    dispatch: DspDispatch,
    max_polyphony: usize,
) -> io::Result<(Vec<f32>, Vec<String>)> {
    // Refuse oversized renders before sample loading or PCM allocation.
    let frames = in_memory_render_frames(sample_rate, duration_secs)?;
    let bundled = rustel_voice::BundledOnly;
    let lookup: &dyn rustel_voice::SampleLookup = match library {
        Some(library) => library,
        None => &bundled,
    };
    if let Some(library) = library {
        // Resolution CHAINS: `sample_controls` returns "still loading"
        // before `reverb_controls` ever resolves a custom `ir` name, and a
        // fallback is silent (not an error), so a settled-probe cannot see
        // it. Three fixed rounds cover s → ir → anything an ir resolve
        // itself queues; each round is a cheap re-resolve plus a wait.
        for _ in 0..3 {
            for onset in onsets {
                let _ = scalar_event(onset, sample_rate, cps, lookup);
            }
            library.wait_until_idle(std::time::Duration::from_secs(120));
        }
    }
    let mut refused: Vec<String> = Vec::new();
    let events: Vec<rustel_audio::OnsetEvent> = onsets
        .iter()
        .filter_map(
            |onset| match scalar_event(onset, sample_rate, cps, lookup) {
                Ok(event) => Some(event),
                Err(message) => {
                    if !refused.iter().any(|seen| seen == &message) {
                        if rustel_voice::direct_diagnostic_logging() {
                            eprintln!(
                                "{}",
                                serde_json::json!({ "voice_refused": { "message": &message } })
                            );
                        }
                        refused.push(message);
                    }
                    None
                }
            },
        )
        .collect();
    let mut backend = scalar_backend(dispatch, library, max_polyphony);
    let pcm = rustel_audio::render_pcm(&mut backend, sample_rate, frames, &events)
        .map_err(io::Error::other)?;
    Ok((pcm, refused))
}

fn scalar_backend(
    dispatch: DspDispatch,
    library: Option<&crate::samples::SampleLibrary>,
    max_polyphony: usize,
) -> rustel_audio::ScalarBackend {
    let mut backend = rustel_audio::ScalarBackend::with_dispatch(dispatch);
    backend.set_max_polyphony(max_polyphony);
    if let Some(library) = library {
        for (id, decoded) in library.take_ready() {
            let _ = backend.install_sample(id, Box::new(decoded));
        }
    }
    #[cfg(test)]
    tests::BUILT_DISPATCH.set(Some(backend.dispatch()));
    backend
}

pub fn write_scalar_wav(
    path: impl AsRef<Path>,
    sample_rate: u32,
    duration_secs: f64,
    onsets: &[OnsetEventJson],
    cps: f64,
) -> io::Result<usize> {
    write_scalar_wav_with_samples(path, sample_rate, duration_secs, onsets, cps, None)
}

/// `write_scalar_wav` with a sample library: waits for the library to settle
/// (offline renders can afford to block on fetches), installs every decoded
/// sample into the backend, then renders.
pub fn write_scalar_wav_with_samples(
    path: impl AsRef<Path>,
    sample_rate: u32,
    duration_secs: f64,
    onsets: &[OnsetEventJson],
    cps: f64,
    library: Option<&crate::samples::SampleLibrary>,
) -> io::Result<usize> {
    write_scalar_wav_reporting(
        path,
        sample_rate,
        duration_secs,
        onsets,
        cps,
        library,
        false,
        None,
        None,
        rustel_audio::WavSampleFormat::Pcm16,
    )
}

/// [`write_scalar_wav_with_samples`] that can report its progress and be
/// stopped part-way.
///
/// Rendering a long set is minutes of work behind a single call: silence for
/// the whole of it reads as a hang, and a Ctrl-C that does nothing until it
/// finishes is indistinguishable from one.
#[allow(clippy::too_many_arguments)]
pub fn write_scalar_wav_reporting(
    path: impl AsRef<Path>,
    sample_rate: u32,
    duration_secs: f64,
    onsets: &[OnsetEventJson],
    cps: f64,
    library: Option<&crate::samples::SampleLibrary>,
    report: bool,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
    stop_when_silent: Option<rustel_audio::SilenceStop>,
    format: rustel_audio::WavSampleFormat,
) -> io::Result<usize> {
    write_scalar_wav_controlled(
        path,
        sample_rate,
        duration_secs,
        onsets,
        cps,
        library,
        report,
        rustel_audio::RenderControl {
            observer: None,
            cancelled,
            finish: None,
            stop_when_silent,
            limiter: None,
        },
        format,
    )
}

/// [`write_scalar_wav_reporting`] under a [`rustel_audio::RenderControl`]:
/// a block observer for a progress bar or a scope, and a `finish` flag that
/// ends the bounce early with a short fade and keeps the file.
#[allow(clippy::too_many_arguments)]
pub fn write_scalar_wav_controlled(
    path: impl AsRef<Path>,
    sample_rate: u32,
    duration_secs: f64,
    onsets: &[OnsetEventJson],
    cps: f64,
    library: Option<&crate::samples::SampleLibrary>,
    report: bool,
    control: rustel_audio::RenderControl<'_, '_>,
    format: rustel_audio::WavSampleFormat,
) -> io::Result<usize> {
    write_scalar_wav_controlled_with_dispatch(
        path,
        sample_rate,
        duration_secs,
        onsets,
        cps,
        library,
        report,
        control,
        format,
        DspDispatch::automatic(),
        rustel_audio::MAX_POLYPHONY,
    )
    .map(|(bytes, _)| bytes)
}

/// [`write_scalar_wav_controlled`] with the prepared kernel selection and
/// voice budget. Answers the bytes written and the distinct voices the
/// render refused.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_scalar_wav_controlled_with_dispatch(
    path: impl AsRef<Path>,
    sample_rate: u32,
    duration_secs: f64,
    onsets: &[OnsetEventJson],
    cps: f64,
    library: Option<&crate::samples::SampleLibrary>,
    report: bool,
    control: rustel_audio::RenderControl<'_, '_>,
    format: rustel_audio::WavSampleFormat,
    dispatch: DspDispatch,
    max_polyphony: usize,
) -> io::Result<(usize, Vec<String>)> {
    let cancelled = control.cancelled;
    let stopped = || cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed));
    let cancelled_now = || io::Error::other(rustel_audio::RENDER_CANCELLED);
    let bundled = rustel_voice::BundledOnly;
    let lookup: &dyn rustel_voice::SampleLookup = match library {
        Some(library) => library,
        None => &bundled,
    };
    if let Some(library) = library {
        // Resolution chains: `sample_controls` returns "still loading"
        // before `reverb_controls` resolves a custom `ir` name, and a
        // fallback is silent (not an error). Three fixed kick-and-wait
        // rounds cover s, then ir, then anything an ir resolve queues.
        //
        // A long bounce waits here before it writes an audio frame (three
        // passes over every onset in the set), so this loop must check for
        // a stop request. Progress uses the same 2-second cadence as the
        // audio phase, so a short bounce prints nothing.
        let mut spoke_at = std::time::Instant::now();
        for round in 0..3 {
            for onset in onsets {
                if stopped() {
                    return Err(cancelled_now());
                }
                let _ = scalar_event(onset, sample_rate, cps, lookup);
                if report && spoke_at.elapsed() >= std::time::Duration::from_secs(2) {
                    spoke_at = std::time::Instant::now();
                    report_progress(
                        serde_json::json!({
                            "render_progress": {
                                "phase": "samples",
                                "round": round + 1,
                                "of_rounds": 3,
                                "onsets": onsets.len(),
                            }
                        }),
                        || {
                            format!(
                                "rendering: loading samples, pass {} of 3 ({} onsets)",
                                round + 1,
                                onsets.len()
                            )
                        },
                    );
                }
            }
            library.wait_until_idle_cancellable(std::time::Duration::from_secs(120), cancelled);
            if stopped() {
                return Err(cancelled_now());
            }
        }
    }
    // Same skip-and-log contract as the live batch: an unrenderable event
    // drops only its own voice; the rest still play. One bad onset must never
    // silence an offline render either. A run of one refusal prints once;
    // `refused` keeps each distinct message for the caller.
    let mut last_refused: Option<String> = None;
    let mut refused: Vec<String> = Vec::new();
    let events: Vec<rustel_audio::OnsetEvent> = onsets
        .iter()
        .filter_map(
            |onset| match scalar_event(onset, sample_rate, cps, lookup) {
                Ok(event) => Some(event),
                Err(message) => {
                    if last_refused.as_deref() != Some(message.as_str()) {
                        if rustel_voice::direct_diagnostic_logging() {
                            eprintln!(
                                "{}",
                                serde_json::json!({ "voice_refused": { "message": &message } })
                            );
                        }
                        if !refused.contains(&message) {
                            refused.push(message.clone());
                        }
                        last_refused = Some(message);
                    }
                    None
                }
            },
        )
        .collect();
    let frames = (duration_secs * f64::from(sample_rate)).round();
    if !frames.is_finite() || frames < 0.0 || frames > usize::MAX as f64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scalar render frame count is not representable",
        ));
    }
    let mut backend = scalar_backend(dispatch, library, max_polyphony);
    let rustel_audio::RenderControl {
        mut observer,
        finish,
        stop_when_silent,
        limiter,
        ..
    } = control;
    let mut spoke_at = std::time::Instant::now();
    let mut observed = |tick: rustel_audio::RenderTick<'_>| {
        if report && spoke_at.elapsed() >= std::time::Duration::from_secs(2) {
            spoke_at = std::time::Instant::now();
            let written = (tick.frames_written as f64 / f64::from(sample_rate)).round();
            let total = (tick.frames_total as f64 / f64::from(sample_rate)).round();
            let percent =
                (tick.frames_written as f64 / tick.frames_total.max(1) as f64 * 100.0).round();
            report_progress(
                serde_json::json!({
                    "render_progress": {
                        "phase": "audio",
                        "secs_written": written,
                        "of_secs": total,
                        "percent": percent,
                    }
                }),
                || format!("rendering: {written} s of {total} s ({percent}%)"),
            );
        }
        if let Some(observer) = observer.as_deref_mut() {
            observer(tick);
        }
    };
    rustel_audio::write_wav_controlled(
        path,
        &mut backend,
        sample_rate,
        frames as usize,
        &events,
        rustel_audio::RenderControl {
            observer: Some(&mut observed),
            cancelled,
            finish,
            stop_when_silent,
            limiter,
        },
        format,
    )
    .map(|bytes| (bytes, refused))
    .map_err(io::Error::other)
}

/// Read a 16-bit stereo WAV the writer above produced back as interleaved
/// f32, for encoding it another way.
pub fn read_pcm16_stereo_wav(path: &Path) -> io::Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[36..40] != b"data" {
        return Err(io::Error::other("not a WAV this engine wrote"));
    }
    let data = u32::from_le_bytes(bytes[40..44].try_into().unwrap_or([0; 4])) as usize;
    let end = (44 + data).min(bytes.len());
    Ok(bytes[44..end]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32767.0)
        .collect())
}

/// Write a minimal RIFF/WAVE file of silent PCM samples.
pub fn write_silent_wav(
    path: impl AsRef<Path>,
    sample_rate: u32,
    channels: u16,
    duration_secs: f64,
) -> io::Result<usize> {
    // Every one of these values derives from caller input. Refuse a value
    // that cannot be encoded: a saturating multiply would hide the overflow
    // and write a plausible header for a file nobody asked for.
    if !duration_secs.is_finite() || duration_secs < 0.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("duration must be finite and non-negative, got {duration_secs}"),
        ));
    }
    if sample_rate == 0 || channels == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("sample_rate and channels must be non-zero, got {sample_rate}/{channels}"),
        ));
    }
    // Range-check every encoded field before narrowing. A wrapped field is
    // worse than a refusal: the header parses cleanly, describes a file that
    // does not exist, and nothing downstream can tell it is wrong. For
    // example, 4,000,000,000 Hz stereo needs a byte rate of 16e9, which does
    // not fit the u32 field.
    let block_align_u32 = u32::from(channels) * 2; // 16-bit
    let block_align = u16::try_from(block_align_u32).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{channels} channels needs a block alignment of {block_align_u32} \
                 bytes, which does not fit the 16-bit RIFF field"
            ),
        )
    })?;
    let byte_rate_u64 = u64::from(sample_rate) * u64::from(block_align);
    let byte_rate = u32::try_from(byte_rate_u64).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{sample_rate}Hz x {channels}ch needs a byte rate of \
                 {byte_rate_u64}, which does not fit the 32-bit RIFF field"
            ),
        )
    })?;
    let block_align_bytes = u32::from(block_align);
    let frames = (duration_secs * f64::from(sample_rate)).round();
    let data_bytes = frames * f64::from(block_align_bytes);
    // RIFF sizes are u32, so anything past that is unrepresentable regardless
    // of how much memory is available - refuse rather than emit a corrupt
    // header. `36 +` is the rest of the RIFF chunk, counted here so the header
    // field below cannot overflow either.
    if !data_bytes.is_finite() || data_bytes + 36.0 > f64::from(u32::MAX) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a {duration_secs}s render at {sample_rate}Hz x {channels}ch needs \
                 {data_bytes:.0} bytes of PCM, which a RIFF file cannot address \
                 (max {})",
                u32::MAX - 36
            ),
        ));
    }
    let data_bytes = data_bytes as u32;
    let mut file = File::create(path)?;

    // RIFF header
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_bytes).to_le_bytes())?;
    file.write_all(b"WAVE")?;

    // fmt chunk
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?; // PCM fmt chunk size
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?; // bits per sample

    // data chunk (silence)
    file.write_all(b"data")?;
    file.write_all(&data_bytes.to_le_bytes())?;
    // Stream the zeros in bounded chunks. One `vec![0u8; data_bytes]` for
    // the whole PCM body would hold hundreds of megabytes for a long render.
    const CHUNK: usize = 64 * 1024;
    let zeros = [0u8; CHUNK];
    let mut remaining = data_bytes as usize;
    while remaining > 0 {
        let n = remaining.min(CHUNK);
        file.write_all(&zeros[..n])?;
        remaining -= n;
    }
    Ok(data_bytes as usize)
}

/// Write a raw onset dump as JSON.
pub fn write_onset_dump(path: impl AsRef<Path>, onsets: &[OnsetEventJson]) -> io::Result<()> {
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, onsets).map_err(io::Error::other)
}

#[cfg(test)]
mod stretch_chain_tests {
    //! `.stretch()` is the first stage of a voice's main chain: the delay and
    //! reverb sends hear the shifted signal, and the vocoder's latency is
    //! compensated so the voice lands on its beat.

    const RATE: usize = crate::session::DEFAULT_SAMPLE_RATE as usize;

    /// The left channel of `seconds` of `score`.
    fn render(score: &str, seconds: f64) -> Vec<f32> {
        let mut session = crate::Session::new().expect("session");
        session.evaluate(score).expect("score");
        let pcm = session.render_pcm(seconds).expect("render");
        pcm.as_chunks::<2>()
            .0
            .iter()
            .map(|frame| frame[0])
            .collect()
    }

    /// The strongest frequency between 400 and 560 Hz in `signal`, to half a hertz.
    fn dominant_hz(signal: &[f32]) -> f64 {
        let power = |hz: f64| {
            let coefficient = 2.0 * (std::f64::consts::TAU * hz / RATE as f64).cos();
            let (mut previous, mut before) = (0.0f64, 0.0f64);
            for &sample in signal {
                let next = f64::from(sample) + coefficient * previous - before;
                before = previous;
                previous = next;
            }
            previous * previous + before * before - coefficient * previous * before
        };
        (800..1120)
            .map(|half| f64::from(half) / 2.0)
            .max_by(|a, b| power(*a).total_cmp(&power(*b)))
            .expect("a frequency")
    }

    /// The first frame at or after `from` whose level reaches a tenth of the
    /// peak of `left[from..to]`.
    fn onset(left: &[f32], from: usize, to: usize) -> usize {
        let window = &left[from..to];
        let peak = window
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 1e-4, "nothing sounds between {from} and {to}");
        from + window
            .iter()
            .position(|sample| sample.abs() >= 0.1 * peak)
            .expect("a frame at the peak")
    }

    /// At a factor of 0 the vocoder shifts nothing, so a stretched voice lines
    /// up with the unstretched one.
    #[test]
    fn an_identity_stretch_lands_where_the_unstretched_voice_does() {
        let plain = render("s(\"~ white\")", 2.0);
        let stretched = render("s(\"~ white\").stretch(0)", 2.0);
        let window = RATE * 6 / 5..RATE * 17 / 10;
        let correlation = |lag: isize| -> f64 {
            window
                .clone()
                .map(|frame| {
                    let shifted = frame.checked_add_signed(lag).expect("in range");
                    f64::from(plain[frame]) * f64::from(stretched[shifted])
                })
                .sum()
        };
        let lag = (-400..=400)
            .max_by(|a, b| correlation(*a).total_cmp(&correlation(*b)))
            .expect("a lag");
        assert!(
            lag.abs() <= 1,
            "the stretched voice lands {lag} frames from the unstretched one"
        );
    }

    /// The echo repeats the shifted note one delay time after it.
    #[test]
    fn a_stretched_voice_echoes_its_shifted_signal_one_delay_time_later() {
        let left = render(
            "note(\"~ a4\").s(\"sine\").clip(0.2).stretch(0.1)\
         .delay(0.5).delaytime(0.5).delayfeedback(0.2)",
            2.0,
        );
        let dry_hz = dominant_hz(&left[RATE * 21 / 20..RATE * 6 / 5]);
        let echo_hz = dominant_hz(&left[RATE * 31 / 20..RATE * 17 / 10]);
        assert!(
            dry_hz > 470.0,
            "the dry note is shifted up from 440: {dry_hz}"
        );
        assert!(
            (echo_hz - dry_hz).abs() <= 2.0,
            "the echo sounds at {echo_hz} Hz where the dry note sounds at {dry_hz} Hz"
        );
        let dry = onset(&left, RATE * 9 / 10, RATE * 13 / 10);
        let echo = onset(&left, RATE * 13 / 10, RATE * 19 / 10);
        let gap = echo as isize - dry as isize;
        assert!(
            (gap - RATE as isize / 2).abs() <= 64,
            "the echo arrives {gap} frames after the dry note, not one delay time"
        );
    }

    /// The reverb carries the shifted note, and starts with it.
    #[test]
    fn a_stretched_voice_reverberates_its_shifted_signal_from_its_onset() {
        let reverb_lead = |stretch: &str| {
            let dry = render(
                &format!("note(\"~ a4\").s(\"sine\").clip(0.2){stretch}"),
                2.0,
            );
            let wet = render(
                &format!("note(\"~ a4\").s(\"sine\").clip(0.2){stretch}.room(1).dry(0)"),
                2.0,
            );
            let from = RATE * 9 / 10;
            let to = RATE * 3 / 2;
            let lead = onset(&wet, from, to) as isize - onset(&dry, from, to) as isize;
            (lead, dominant_hz(&dry[RATE * 21 / 20..RATE * 6 / 5]), wet)
        };
        let (plain_lead, _, _) = reverb_lead("");
        let (stretched_lead, dry_hz, wet) = reverb_lead(".stretch(0.1)");
        let tail_hz = dominant_hz(&wet[RATE * 13 / 10..RATE * 18 / 10]);
        assert!(
            dry_hz > 470.0,
            "the dry note is shifted up from 440: {dry_hz}"
        );
        assert!(
            (tail_hz - dry_hz).abs() <= 3.0,
            "the reverb sounds at {tail_hz} Hz where the dry note sounds at {dry_hz} Hz"
        );
        assert!(
            (stretched_lead - plain_lead).abs() <= 128,
            "a stretched voice's reverb starts {stretched_lead} frames from its dry note; \
         an unstretched voice's starts {plain_lead} frames from it"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exact PCM cannot distinguish these kernel choices. Observe only the
    // backend constructed by this test thread, without changing selection.
    std::thread_local! {
        pub(super) static BUILT_DISPATCH: std::cell::Cell<Option<DspDispatch>> = const {
            std::cell::Cell::new(None)
        };
    }

    fn assert_built_dispatch(expected: DspDispatch) {
        let actual = BUILT_DISPATCH.take().expect("constructed scalar backend");
        let selection = |dispatch: DspDispatch| {
            (
                dispatch.is_forced_portable(),
                dispatch.convolution_kernel_kind(),
                dispatch.supersaw_kernel_kind(),
                dispatch.wavetable_kernel_kind(),
            )
        };
        assert_eq!(selection(actual), selection(expected));
    }

    const DISPATCH_SAMPLE_RATE: u32 = 48_000;
    const DISPATCH_DURATION: f64 = 0.125;

    fn native_render_session(dispatch: DspDispatch) -> crate::Session {
        use rustel_core::{Value, pure};

        let mut session = crate::Session::with_config(crate::SessionConfig {
            cps: 1.0,
            sample_rate: DISPATCH_SAMPLE_RATE,
            dsp_dispatch: dispatch,
            ..crate::SessionConfig::default()
        })
        .expect("render session");
        session.set_direct_diagnostic_logging(false);
        session
            .set_pattern(pure(Value::object(vec![
                ("s".into(), Value::Str("supersaw".into())),
                ("note".into(), Value::F64(48.0)),
                ("gain".into(), Value::F64(0.1)),
                ("unison".into(), Value::F64(8.0)),
                ("detune".into(), Value::F64(0.35)),
                ("spread".into(), Value::F64(0.8)),
                ("room".into(), Value::F64(0.2)),
                ("roomsize".into(), Value::F64(0.2)),
            ])))
            .expect("native pattern");
        assert!(!session.active_needs_host());
        session
    }

    fn assert_exact_render(expected: &[f32], actual: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        assert!(actual.iter().any(|sample| sample.abs() > 1e-6));
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert!(actual.is_finite() && expected.is_finite());
            assert_eq!(actual.to_bits(), expected.to_bits(), "sample {index}");
        }
    }

    #[test]
    fn native_session_render_routes_retain_dispatch_and_exact_pcm() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("render directory");
        let automatic = DspDispatch::automatic();
        let onsets = native_render_session(automatic)
            .play(DISPATCH_DURATION)
            .expect("dispatch workload onsets")
            .onsets;
        assert_eq!(onsets.len(), 1);
        let event = scalar_event(
            &onsets[0],
            DISPATCH_SAMPLE_RATE,
            1.0,
            &rustel_voice::BundledOnly,
        )
        .expect("dispatch workload voice");
        assert!(matches!(
            event.synth,
            Some(rustel_audio::SynthSource::Supersaw { voices: 8.0, .. })
        ));
        assert!(event.controls.reverb.is_some_and(|reverb| reverb.wet > 0.0));
        let expected = native_render_session(automatic)
            .render_pcm(DISPATCH_DURATION)
            .expect("reference PCM");
        assert_eq!(expected.len(), 12_000);
        assert_built_dispatch(automatic);
        #[cfg(feature = "mp3-export")]
        let reference_mp3 = {
            let path = directory.path().join("reference.mp3");
            encode_mp3(&path, DISPATCH_SAMPLE_RATE, &expected).expect("reference MP3");
            std::fs::read(path).expect("reference MP3 bytes")
        };

        for dispatch in [automatic, DspDispatch::portable()] {
            let preference = if dispatch.is_forced_portable() {
                "portable"
            } else {
                "auto"
            };
            BUILT_DISPATCH.set(None);
            let actual = native_render_session(dispatch)
                .render_pcm(DISPATCH_DURATION)
                .expect("selected PCM");
            assert_built_dispatch(dispatch);
            assert_exact_render(&expected, &actual);

            for (name, format) in [
                ("float.wav", RenderFormat::ScalarF32Wav),
                ("pcm16.wav", RenderFormat::ScalarWav),
                #[cfg(feature = "mp3-export")]
                ("controlled.mp3", RenderFormat::ScalarMp3),
            ] {
                let path = directory.path().join(format!("{preference}-{name}"));
                assert!(!path.exists(), "render destination must be fresh");
                let mut pcm = Vec::new();
                let mut observe = |tick: rustel_audio::RenderTick<'_>| {
                    pcm.extend_from_slice(tick.block);
                };
                BUILT_DISPATCH.set(None);
                let report = native_render_session(dispatch)
                    .render_controlled(
                        DISPATCH_DURATION,
                        &path,
                        format,
                        false,
                        Some(&mut observe),
                        None,
                    )
                    .expect("controlled render");
                assert_built_dispatch(dispatch);
                assert_eq!(report.duration_secs, DISPATCH_DURATION, "{name}");
                assert_exact_render(&expected, &pcm);
                let bytes = std::fs::read(path).expect("encoded audio");
                assert!(!bytes.is_empty());
                if format == RenderFormat::ScalarF32Wav {
                    let decoded = rustel_audio::decode_wav(&bytes).expect("float WAV");
                    assert_exact_render(&expected, decoded.pcm());
                }
            }

            #[cfg(feature = "mp3-export")]
            {
                // Without an observer MP3 takes the shared in-memory PCM route,
                // not the staged WAV route exercised above.
                let path = directory.path().join(format!("direct-{preference}.mp3"));
                assert!(!path.exists(), "direct MP3 destination must be fresh");
                BUILT_DISPATCH.set(None);
                native_render_session(dispatch)
                    .render(DISPATCH_DURATION, &path, RenderFormat::ScalarMp3)
                    .expect("direct MP3 render");
                assert_built_dispatch(dispatch);
                assert_eq!(std::fs::read(path).expect("MP3 bytes"), reference_mp3);
            }
        }
    }

    #[test]
    fn export_limiter_applies_to_file_renders_without_a_progress_observer() {
        let directory = tempfile::tempdir().expect("render directory");
        let render = |name, format| {
            let mut session = native_render_session(DspDispatch::automatic());
            session.set_export_limiter(Some(rustel_audio::RenderLimiter {
                settings: rustel_audio::LimiterSettings {
                    threshold_db: -40.0,
                    character: rustel_audio::LimiterCharacter::Transparent,
                },
                makeup: false,
            }));
            let path = directory.path().join(name);
            session
                .render(DISPATCH_DURATION, &path, format)
                .expect("render");
            path
        };
        let wav = render("limited.wav", RenderFormat::ScalarWav);
        let pcm = read_pcm16_stereo_wav(&wav).expect("WAV");
        let peak = pcm
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!((0.009..0.0101).contains(&peak), "ceiling: {peak}");
        #[cfg(feature = "mp3-export")]
        {
            let mp3 = render("limited.mp3", RenderFormat::ScalarMp3);
            let expected = directory.path().join("expected.mp3");
            encode_mp3(&expected, DISPATCH_SAMPLE_RATE, &pcm).expect("encode limited WAV");
            assert_eq!(
                std::fs::read(mp3).unwrap(),
                std::fs::read(expected).unwrap()
            );
        }
    }

    #[test]
    fn saved_session_render_routes_retain_dispatch() {
        let source = include_str!("../tests/e2e/scores/corpus/regressions/supersaw-plain.strudel");
        let saves = [(0.0, source.to_owned())];
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("render directory");
        for mp3 in [
            false,
            #[cfg(feature = "mp3-export")]
            true,
        ] {
            let mut expected = None;
            for dispatch in [DspDispatch::automatic(), DspDispatch::portable()] {
                let mut session = crate::Session::with_config(crate::SessionConfig {
                    cps: 1.0,
                    sample_rate: DISPATCH_SAMPLE_RATE,
                    dsp_dispatch: dispatch,
                    ..crate::SessionConfig::default()
                })
                .expect("replay session");
                session.set_direct_diagnostic_logging(false);
                let preference = if dispatch.is_forced_portable() {
                    "portable"
                } else {
                    "auto"
                };
                let extension = if mp3 { "mp3" } else { "wav" };
                let path = directory
                    .path()
                    .join(format!("replay-{preference}.{extension}"));
                assert!(!path.exists(), "replay destination must be fresh");
                BUILT_DISPATCH.set(None);
                let report = session
                    .render_session(&saves, 0.0, Some(DISPATCH_DURATION), &path, mp3)
                    .expect("saved session render");
                assert!(report.onset_count > 0);
                assert!(report.query_threw.is_none());
                assert_built_dispatch(dispatch);
                let bytes = std::fs::read(path).expect("replay audio bytes");
                assert!(!bytes.is_empty());
                if !mp3 {
                    let decoded = rustel_audio::decode_wav(&bytes).expect("replay WAV");
                    assert_eq!(decoded.pcm().len(), 12_000);
                    assert!(decoded.pcm().iter().all(|sample| sample.is_finite()));
                    assert!(decoded.pcm().iter().any(|sample| sample.abs() > 1e-6));
                }
                if let Some(expected) = &expected {
                    assert_eq!(&bytes, expected);
                } else {
                    expected = Some(bytes);
                }
            }
        }
    }

    #[test]
    fn automatic_wav_wrapper_keeps_the_existing_default() {
        let config = crate::SessionConfig::default();
        assert!(!config.dsp_dispatch.is_forced_portable());
        let mut session = native_render_session(config.dsp_dispatch);
        let play = session.play(DISPATCH_DURATION).expect("native onsets");
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("render directory");

        BUILT_DISPATCH.set(None);
        write_scalar_wav(
            directory.path().join("automatic.wav"),
            DISPATCH_SAMPLE_RATE,
            DISPATCH_DURATION,
            &play.onsets,
            1.0,
        )
        .expect("automatic WAV");
        assert_built_dispatch(config.dsp_dispatch);
    }

    #[test]
    fn an_in_memory_render_over_the_pcm_budget_is_refused_not_allocated() {
        // The mp3 shortcut and `Session::render_pcm` hold the whole render
        // body in memory, where the streamed WAV writer holds one block.
        // Without a budget the only ceiling on `vec![0.0f32; frames * 2]`
        // was `usize::MAX` frames. The budget is checked on the pure frame
        // count, so a regression here fails an assertion rather than
        // allocating; the CLI test covers the wiring end to end.
        let max = MAX_IN_MEMORY_FRAMES as usize;
        // The boundary is the Pcm16 WAV twin's: the largest whole frame
        // whose 16-bit stereo body plus header a RIFF file can address.
        const _: () = assert!(
            MAX_IN_MEMORY_FRAMES * PCM16_STEREO_FRAME_BYTES + 36 <= u32::MAX as u64
                && (MAX_IN_MEMORY_FRAMES + 1) * PCM16_STEREO_FRAME_BYTES + 36 > u32::MAX as u64
        );
        assert_eq!(
            in_memory_render_frames(1, max as f64).expect("at the ceiling"),
            max
        );
        let past = in_memory_render_frames(1, max as f64 + 1.0)
            .expect_err("one frame past the ceiling must be refused");
        assert_eq!(past.kind(), io::ErrorKind::InvalidInput);
        assert!(
            past.to_string()
                .contains("which an in-memory render cannot hold"),
            "{past}"
        );
        // Six hours at 48 kHz fits the 16-bit twin (and `--format wav`), so
        // it must fit here too: a 4-bytes-per-frame budget, not the f32
        // buffer's 8, keeps the two mp3 routes admitting the same renders.
        assert_eq!(
            in_memory_render_frames(48_000, 6.0 * 3600.0).expect("6 h at 48 kHz fits"),
            1_036_800_000
        );
        // A legal CLI ask past it: 24 h at 48 kHz.
        assert!(in_memory_render_frames(48_000, 86_400.0).is_err());
        // A hostile tape end: a sample rate that only just fits a u32, for a
        // day. Frame counts this large must not wrap on their way to the
        // comparison.
        let hostile = in_memory_render_frames(u32::MAX, 86_400.0)
            .expect_err("a huge sample rate must be refused");
        assert!(hostile.to_string().contains("PCM bytes"), "{hostile}");
        // An ordinary render still runs: the budget is a ceiling, not a
        // smaller default.
        render_scalar_pcm_with_refusals(
            48_000,
            1.0,
            &[],
            1.0,
            None,
            DspDispatch::automatic(),
            rustel_audio::MAX_POLYPHONY,
        )
        .expect("an ordinary in-memory render still renders");
    }

    // The direct note-grammar probe now exercises the shared resolver.
    use rustel_voice::note_to_hz;

    fn onset(value: serde_json::Value) -> OnsetEventJson {
        OnsetEventJson {
            onset_id: 7,
            generation: 1,
            whole_begin: "0/1".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 3],
            ui_visuals: 0,
            value: crate::ValueJson::Raw(value),
            value_show: String::new(),
            log_line: None,
        }
    }

    #[test]
    fn preview_ownership_reaches_audio_controls_without_tagging_the_score() {
        for (value, epoch) in [
            (
                serde_json::json!({"s": "sine", "note": "c4", "__rustelPreview": 42.0}),
                42,
            ),
            (serde_json::json!({"s": "sine", "note": "c4"}), 0),
        ] {
            let event =
                scalar_event(&onset(value), 48_000, 0.5, &rustel_voice::BundledOnly).unwrap();
            assert_eq!(event.controls.preview_epoch, epoch);
        }
    }

    #[test]
    fn note_mapping_matches_the_strudel_note_table() {
        for (note, expected) in [
            ("c", 130.812_782_650_299_3),
            ("c4", 261.625_565_300_598_6),
            ("a4", 440.0),
            ("fbb1", 38.890_872_965_260_115),
            ("e##-2", 5.781_162_854_869_287_5),
        ] {
            let got = note_to_hz(note).unwrap();
            assert!((got - expected).abs() < 1e-10, "{note}: {got}");
        }
        for invalid in ["", "h4", "c-", "c+4", "c 4"] {
            assert!(note_to_hz(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn scalar_conversion_pins_freq_gain_and_bundled_sample_controls() {
        let event = scalar_event(
            &onset(serde_json::json!({
                "freq": 220.0,
                "note": "c9",
                "gain": 0.25
            })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .unwrap();
        assert_eq!(event.freq_hz, 220.0);
        assert_eq!(event.gain, 0.25);
        assert_eq!(event.duration_secs, 0.25);
        assert_eq!(event.controls.waveform, rustel_audio::Waveform::Triangle);
        assert_eq!(event.controls.envelope, rustel_audio::Envelope::default());

        let sample = scalar_event(
            &onset(serde_json::json!({
                "s": "bd",
                "speed": 1.25,
                "begin": 0.1,
                "end": 0.85
            })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("bundled bd sample");
        assert_eq!(
            sample.sample,
            Some(rustel_audio::SampleControls {
                sample: rustel_audio::BUNDLED_BD_SAMPLE_ID,
                playback_rate: 1.25,
                begin: 0.1,
                end: 0.85,
                hold: rustel_audio::SampleHold::Slice,
                muted: false,
                loop_secs: None,
                envelope_peak: 1.0,
                reversed: false,
                nudge_secs: 0.0,
                cut: None,
            })
        );
        assert_eq!(sample.controls.envelope.attack_secs, 0.001);
        assert_eq!(sample.controls.envelope.decay_secs, 0.001);
        assert_eq!(sample.controls.envelope.sustain, 1.0);

        let error = scalar_event(
            &onset(serde_json::json!({"s": "sd"})),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect_err("unknown sample name must not be mapped to an oscillator");
        assert!(error.contains("unknown sound"), "{error}");
    }

    #[test]
    fn bundled_sample_pitch_hold_and_zero_speed_match_the_reference() {
        let note = scalar_event(
            &onset(serde_json::json!({ "s": "bd", "note": "c4" })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("pitched sample");
        assert_eq!(note.sample.expect("sample").playback_rate, 4.0);

        let frequency = scalar_event(
            &onset(serde_json::json!({ "s": "bd", "freq": 440.0 })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("frequency-pitched sample");
        let expected = 2.0f32.powf(33.0 / 12.0);
        assert!((frequency.sample.expect("sample").playback_rate - expected).abs() < 1e-6);

        let released = scalar_event(
            &onset(serde_json::json!({ "s": "bd", "release": 0.05 })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("explicit sample release");
        assert_eq!(
            released.sample.expect("sample").hold,
            rustel_audio::SampleHold::Hap
        );

        let muted = scalar_event(
            &onset(serde_json::json!({
                "s": "bd",
                "speed": 0,
                "begin": 99,
                "nudge": 1
            })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("speed zero is an early per-hap no-op");
        assert!(muted.sample.expect("sample").muted);
    }

    #[test]
    fn unmeasured_sample_paths_are_explicit_refusals() {
        // The scalar chain applies the same FilterChain to sample voices and
        // oscillators. Nudge, loop, unit, and negative-speed behavior resolve
        // through the same event conversion path.
        let nudged = scalar_event(
            &onset(serde_json::json!({ "s": "bd", "nudge": 0.01 })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("nudge delays the source, not the envelope");
        assert_eq!(nudged.sample.expect("sample").nudge_secs, 0.01);

        let looped = scalar_event(
            &onset(serde_json::json!({ "s": "bd", "loop": 1, "loopEnd": 0.5 })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("loop resolves to hap-held looping");
        let looped = looped.sample.expect("sample");
        assert_eq!(looped.hold, rustel_audio::SampleHold::Hap);
        let (start, end) = looped.loop_secs.expect("loop region");
        assert_eq!(start, 0.0);
        assert!(end > 0.0);

        let reversed = scalar_event(
            &onset(serde_json::json!({ "s": "bd", "speed": -1 })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("negative speed reverses");
        let reversed = reversed.sample.expect("sample");
        assert!(reversed.reversed);
        assert_eq!(reversed.playback_rate, 1.0);

        let filtered = scalar_event(
            &onset(serde_json::json!({ "s": "bd", "cutoff": 800, "lpenv": 2 })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("filters on samples resolve through the shared FilterChain");
        assert!(filtered.sample.is_some());
        assert!(filtered.controls.filters.lowpass.is_some());

        let error = scalar_event(
            &onset(serde_json::json!({ "s": "BD" })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect_err("sample bank lookup is case-sensitive on strudel.cc");
        assert!(error.contains("unknown sound"), "{error}");
    }

    #[test]
    fn scalar_conversion_carries_pinned_controls() {
        let event = scalar_event(
            &onset(serde_json::json!({
                "s": "sqr",
                "freq": 220.0,
                "gain": 0.4,
                "velocity": 0.5,
                "postgain": 0.8,
                "attack": 0.02,
                "decay": 0.03,
                "sustain": 0.25,
                "release": 0.1,
                "pan": 2.0
            })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("convert controls");
        assert_eq!(event.controls.waveform, rustel_audio::Waveform::Square);
        assert_eq!(event.controls.velocity, 0.5);
        assert_eq!(event.controls.postgain, 0.8);
        assert_eq!(event.controls.pan, Some(1.0));
        assert_eq!(
            event.controls.envelope,
            rustel_audio::Envelope {
                attack_secs: 0.02,
                decay_secs: 0.03,
                sustain: 0.25,
                release_secs: 0.1,
            }
        );
    }

    #[test]
    fn live_event_keeps_the_bundled_sample_source() {
        let onset = onset(serde_json::json!({
            "s": "bd",
            "speed": 1.25,
            "begin": 0.1,
            "end": 0.85
        }));
        let event = live_audio_event(&onset, 48_000, 0.5, &rustel_voice::BundledOnly)
            .expect("live sample event");
        assert_eq!(
            event.sample,
            Some(rustel_audio::SampleControls {
                sample: rustel_audio::BUNDLED_BD_SAMPLE_ID,
                playback_rate: 1.25,
                begin: 0.1,
                end: 0.85,
                hold: rustel_audio::SampleHold::Slice,
                muted: false,
                loop_secs: None,
                envelope_peak: 1.0,
                reversed: false,
                nudge_secs: 0.0,
                cut: None,
            })
        );
    }

    #[test]
    fn a_live_onset_seeds_its_lfo_from_the_cycle_and_gates_on_the_clock() {
        let lfo_onset = |whole_begin: &str| OnsetEventJson {
            whole_begin: whole_begin.into(),
            target_time: 10.4,
            ..onset(serde_json::json!({
                "s": "sawtooth",
                "cutoff": 800.0,
                "lfo": { "a": { "control": "cutoff", "rate": 7.0 } }
            }))
        };
        let event = |whole_begin: &str| {
            live_audio_event(
                &lfo_onset(whole_begin),
                48_000,
                0.5,
                &rustel_voice::BundledOnly,
            )
            .expect("live event")
        };
        let phase0 = |event: &rustel_audio::AudioEvent| {
            event.controls.lfos[0].expect("the lfo() modulator").phase0
        };

        // Cycle 3/4 at 0.5 cps is 1.5 s, and frac(1.5 * 7) is 0.5.
        let live = event("3/4");
        assert_eq!(phase0(&live), 0.5);
        assert_eq!(live.target_frame, 499_200);

        // No readable cycle: the clock time gives frac(10.4 * 7).
        assert!((phase0(&event("")) - 0.8).abs() < 1e-4);
    }

    #[test]
    fn scalar_conversion_carries_filters_and_selects_ladder_models() {
        let event = scalar_event(
            &onset(serde_json::json!({
                "s": "sawtooth",
                "freq": 330.0,
                "cutoff": 700.0,
                "resonance": 6.0,
                "hcutoff": 80.0,
                "bandf": 1100.0,
                "bandq": 4.0,
                "ftype": "12db"
            })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("convert static filters");
        assert_eq!(
            event.controls.filters.lowpass,
            Some(rustel_audio::StaticBiquad {
                frequency_hz: 700.0,
                q: 6.0,
            })
        );
        assert_eq!(
            event.controls.filters.highpass,
            Some(rustel_audio::StaticBiquad {
                frequency_hz: 80.0,
                q: 1.0,
            })
        );
        assert_eq!(
            event.controls.filters.bandpass,
            Some(rustel_audio::StaticBiquad {
                frequency_hz: 1100.0,
                q: 4.0,
            })
        );
        assert_eq!(
            event.controls.filters.stages,
            rustel_audio::FilterStages::One
        );

        for supported in [
            serde_json::json!(0),
            serde_json::json!(3.5),
            serde_json::json!(-3),
            serde_json::json!(2.9),
            serde_json::json!("24db"),
        ] {
            assert!(
                scalar_event(
                    &onset(serde_json::json!({
                        "s": "sawtooth",
                        "freq": 330.0,
                        "cutoff": 700.0,
                        "ftype": supported
                    })),
                    48_000,
                    0.5,
                    &rustel_voice::BundledOnly
                )
                .is_ok(),
                "numeric ftype must use pinned modulo selection"
            );
        }
        let stages_for = |ftype: serde_json::Value| {
            scalar_event(
                &onset(serde_json::json!({
                    "s": "sawtooth",
                    "freq": 330.0,
                    "cutoff": 700.0,
                    "ftype": ftype
                })),
                48_000,
                0.5,
                &rustel_voice::BundledOnly,
            )
            .expect("filter model renders natively")
            .controls
            .filters
            .stages
        };

        // The STRING is the only way to the ladder.
        assert_eq!(
            stages_for(serde_json::json!("ladder")),
            rustel_audio::FilterStages::Ladder
        );

        // A NUMBER cannot reach it, however `getFilterType` resolves. `lpMap`
        // carries `model: 'ftype'`, so `pickAndRename` hands `createFilter` the
        // RAW value and `model === 'ladder'` compares a number to a string;
        // only the separate `ftype === '24db'` test reads the resolved name.
        // Measured against Chromium, `ftype(1)` and `ftype("12db")` render
        // BIT-IDENTICALLY.
        assert_eq!(
            stages_for(serde_json::json!(1)),
            rustel_audio::FilterStages::One,
            "numeric ftype must not reach the ladder, as strudel.cc's cannot"
        );
        assert_eq!(
            stages_for(serde_json::json!(0)),
            rustel_audio::FilterStages::One
        );
        assert_eq!(
            stages_for(serde_json::json!(2)),
            rustel_audio::FilterStages::Two
        );
        // The modulo still wraps, so the docs' `.ftype("<0 1 2>")` sweep keeps
        // working past 2.
        assert_eq!(
            stages_for(serde_json::json!(4)),
            rustel_audio::FilterStages::One
        );
        assert_eq!(
            stages_for(serde_json::json!(5)),
            rustel_audio::FilterStages::Two
        );
        assert_eq!(
            stages_for(serde_json::json!(-1)),
            rustel_audio::FilterStages::Two
        );
    }

    #[test]
    fn scalar_conversion_resolves_pinned_filter_envelope_and_24db_cascade() {
        let event = scalar_event(
            &onset(serde_json::json!({
                "s": "square",
                "freq": 220.0,
                "cutoff": 400.0,
                "resonance": 2.0,
                "lpenv": 3.0,
                "fanchor": 0.25,
                "lpattack": 0.08,
                "lpdecay": 0.12,
                "lpsustain": 0.3,
                "lprelease": 0.1,
                "ftype": "24db"
            })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("convert filter envelope");
        assert_eq!(
            event.controls.filters.stages,
            rustel_audio::FilterStages::Two
        );
        assert_eq!(
            event.controls.filters.lowpass_envelope,
            Some(rustel_audio::FilterEnvelope {
                attack_secs: 0.08,
                decay_secs: 0.12,
                sustain: 0.3,
                release_secs: 0.1,
                min_hz: 2.0f64.powf(-0.75) * 400.0,
                max_hz: 2.0f64.powf(2.25) * 400.0,
            })
        );
    }

    #[test]
    fn scalar_envelope_defaults_depend_on_control_presence() {
        let default = scalar_event(
            &onset(serde_json::json!({ "note": "c4" })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("default envelope");
        assert_eq!(default.gain, 0.8);
        assert_eq!(default.controls.envelope, rustel_audio::Envelope::default());

        let attack_only = scalar_event(
            &onset(serde_json::json!({ "note": "c4", "attack": 0.2 })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("attack-only envelope");
        assert_eq!(attack_only.controls.envelope.sustain, 1.0);
        assert_eq!(attack_only.controls.envelope.decay_secs, 0.001);
        assert_eq!(attack_only.controls.envelope.release_secs, 0.01);

        let decay_only = scalar_event(
            &onset(serde_json::json!({ "note": "c4", "decay": 0.2 })),
            48_000,
            0.5,
            &rustel_voice::BundledOnly,
        )
        .expect("decay-only envelope");
        assert_eq!(decay_only.controls.envelope.attack_secs, 0.001);
        assert_eq!(decay_only.controls.envelope.sustain, 0.001);
    }

    #[test]
    fn live_conversion_does_not_drop_scalar_fields() {
        let mut onset = onset(serde_json::json!({
            "s": "square",
            "freq": 220.0,
            "gain": 0.4,
            "velocity": 0.5,
            "postgain": 0.8,
            "attack": 0.02,
            "decay": 0.03,
            "sustain": 0.25,
            "release": 0.1,
            "pan": 1.0,
            "cutoff": 700.0,
            "resonance": 6.0
        }));
        onset.generation = 9;
        onset.ui_visuals = 0b101;
        onset.live_controls = [11, 12, 13];
        onset.target_time = 480.25 / 48_000.0;
        let scalar = scalar_event(&onset, 48_000, 0.5, &rustel_voice::BundledOnly)
            .expect("offline conversion");
        let live = live_audio_event(&onset, 48_000, 0.5, &rustel_voice::BundledOnly)
            .expect("live conversion");
        assert_eq!(live.target_frame, scalar.onset_frame);
        assert_eq!(scalar.onset_frame, 481);
        assert!((scalar.onset_lead - 0.75).abs() < f32::EPSILON);
        assert_eq!(live.onset_lead, scalar.onset_lead);
        assert_eq!(live.freq_hz, scalar.freq_hz);
        assert_eq!(live.gain, scalar.gain);
        assert_eq!(live.duration_secs, scalar.duration_secs);
        // Everything but the slider tokens crosses both conversions. Only the
        // live device can be handed a new value while the note sounds, so only
        // it carries the binding - and only it pays the per-sample gain.
        assert_eq!(
            live.controls,
            rustel_audio::OscillatorControls {
                live_controls: [11, 12, 13],
                ..scalar.controls
            }
        );
        assert_eq!(scalar.controls.live_controls, [0; 3]);
        assert_eq!(scalar.generation, onset.generation);
        assert_eq!(scalar.ui_visuals, onset.ui_visuals);
        assert_eq!(live.generation, onset.generation);
        assert_eq!(live.ui_visuals, onset.ui_visuals);
    }

    #[test]
    fn gain_modulation_keeps_the_constant_graph_and_pcm_even_with_slider_provenance() {
        let mut onset = onset(serde_json::json!({
            "s": "sine", "gain": 0.5,
            "lfo": { "a": { "control": "gain", "rate": 4, "depth": 4 } }
        }));
        let constant = scalar_event(&onset, 48_000, 0.5, &rustel_voice::BundledOnly).unwrap();
        onset.live_controls = [11, 0, 0];
        let mut bound = scalar_event(&onset, 48_000, 0.5, &rustel_voice::BundledOnly).unwrap();
        bind_live_controls(&mut bound, &onset);
        assert!(
            bound
                .controls
                .lfos
                .iter()
                .flatten()
                .any(|lfo| lfo.target == rustel_audio::ModTarget::Gain)
        );
        assert_eq!(bound.controls.live_controls, [0; 3]);
        assert_eq!(bound, constant);
        let render = |event| {
            rustel_audio::render_pcm(
                &mut rustel_audio::ScalarBackend::new(),
                48_000,
                24_000,
                &[event],
            )
            .unwrap()
        };
        assert_eq!(render(bound), render(constant));
    }

    #[test]
    fn cutoff_envelope_keeps_its_own_automation_and_reports_hide_binding_tokens() {
        let mut onset =
            onset(serde_json::json!({ "s": "sine", "gain": 0.5, "cutoff": 800, "lpenv": 2 }));
        onset.live_controls = [11, 12, 13];
        let mut event = scalar_event(&onset, 48_000, 0.5, &rustel_voice::BundledOnly).unwrap();
        bind_live_controls(&mut event, &onset);
        assert!(event.controls.filters.lowpass_envelope.is_some());
        assert_eq!(event.controls.live_controls, [11, 0, 13]);
        assert!(
            serde_json::to_value(onset)
                .unwrap()
                .get("live_controls")
                .is_none()
        );
    }

    #[test]
    fn scalar_conversion_leaves_slider_bindings_to_the_live_device() {
        let mut onset = onset(serde_json::json!({
            "s": "square", "note": "c4", "gain": 0.693, "cutoff": 900
        }));
        let literal = scalar_event(&onset, 48_000, 1.0, &rustel_voice::BundledOnly)
            .expect("literal controls");
        onset.live_controls = [11, 12, 13];
        let slider = scalar_event(&onset, 48_000, 1.0, &rustel_voice::BundledOnly)
            .expect("slider provenance");
        assert_eq!(slider.controls.live_controls, [0; 3]);
        assert_eq!(slider, literal);
    }

    #[test]
    fn an_offline_render_folds_a_slider_gain_in_exactly_as_a_literal_one() {
        // A bound gain leaves the source amplitude and is multiplied back in
        // per sample, so a drag moves a note that is already sounding. Offline
        // nothing can move the slider, and the extra multiply is a different
        // f32 association: it shifted about a third of the samples of
        // `corpus/songs/avril-14th` by one ULP and broke its golden.
        for score in [
            r#"note("c4 e4").s("square").gain(GAIN)"#,
            // The split path as well: a nonlinear shaper is fed the source
            // BEFORE the gain, so the gain is divided back out of the source
            // amplitude and multiplied in after the chain.
            r#"note("c4 e4").s("square").transient(0.5).gain(GAIN)"#,
        ] {
            let mut expected: Option<Vec<f32>> = None;
            for gain in ["0.693", "slider(0.693)"] {
                let mut session = crate::Session::with_config(crate::SessionConfig {
                    cps: 1.0,
                    sample_rate: DISPATCH_SAMPLE_RATE,
                    ..crate::SessionConfig::default()
                })
                .expect("slider render session");
                session.set_direct_diagnostic_logging(false);
                session
                    .evaluate(&score.replace("GAIN", gain))
                    .expect("score with a gain");
                let pcm = session.render_pcm(DISPATCH_DURATION).expect("render");
                match &expected {
                    Some(expected) => assert_exact_render(expected, &pcm),
                    None => expected = Some(pcm),
                }
            }
        }
    }
}

#[cfg(test)]
mod limit_control_tests {
    /// `.limit()` holds the voice's ceiling all the way to the samples.
    ///
    /// The unit tests prove the DSP; this proves the wiring. A control
    /// that parses and rides the event must also reach the audio.
    #[test]
    fn a_limited_voice_never_passes_its_ceiling() {
        let loud = "$: s(\"bd*4\").distort(\"9:1\").postgain(8)";
        let render = |source: &str| {
            let mut session = crate::Session::new().expect("session");
            session.evaluate(source).expect("score");
            session.render_pcm(1.0).expect("render")
        };
        let peak = |pcm: &[f32]| pcm.iter().fold(0.0f32, |worst, s| worst.max(s.abs()));
        let peak_of = |source: &str| peak(&render(source));

        // Unlimited, this is eight times full scale.
        let bare = peak_of(loud);
        assert!(bare >= 4.0, "the test signal is not loud enough: {bare}");

        // Held, and held AT the number asked for rather than merely under
        // it: -6 dBFS is 0.5011872, and a limiter that landed at 0.3 would
        // be reducing far more than it was told to.
        let held = peak_of(&format!("{loud}.limit(\"-6\")"));
        assert!(
            (held - 0.501_187_2).abs() < 1e-4,
            "a -6 dBFS ceiling gave {held}, not the 0.5011872 asked for"
        );

        // Every character holds, at the ceiling and not under it...
        let mut rendered = Vec::new();
        for character in ["transparent", "punchy", "warm", "hard"] {
            let pcm = render(&format!("{loud}.limit(\"-12:{character}\")"));
            let held = peak(&pcm);
            assert!(
                (held - 0.251_188_64).abs() < 1e-4,
                "{character}: a -12 dBFS ceiling gave {held}"
            );
            rendered.push((character, pcm));
        }
        // ...and they are four different limiters, not one name that the
        // wire dropped. Same ceiling, so the peak cannot tell them apart;
        // different runways and releases, so the waveform under it can.
        for pair in rendered.windows(2) {
            let [(before, first), (after, second)] = pair else {
                unreachable!("windows(2)")
            };
            let apart = first
                .iter()
                .zip(second.iter())
                .fold(0.0f32, |worst, (a, b)| worst.max((a - b).abs()));
            assert!(
                apart > 1e-3,
                "{before} and {after} rendered the same: limitchar never reached the DSP"
            );
        }

        // Every way the documentation says you may call it, because the
        // ceiling being an ordinary argument is the whole point of the
        // second one: `limit(slider(...), "hard")` is what puts a fader on
        // it, and it only works if the character can be passed separately
        // rather than glued into a mini-notation string.
        for call in [
            "limit(\"-6:hard\")",
            "limit(-6)",
            "limit(-6, \"hard\")",
            "limit(slider(-6, -24, 0), \"hard\")",
        ] {
            let held = peak_of(&format!("{loud}.{call}"));
            assert!(
                (held - 0.501_187_2).abs() < 1e-4,
                "{call} gave {held}, not the -6 dBFS it asked for"
            );
        }
        // And the character really is the second argument, not decoration:
        // same ceiling, so only the waveform under it can tell them apart.
        let render_call = |call: &str| render(&format!("{loud}.{call}"));
        let hard = render_call("limit(-6, \"hard\")");
        let warm = render_call("limit(-6, \"warm\")");
        let apart = hard
            .iter()
            .zip(warm.iter())
            .fold(0.0f32, |worst, (a, b)| worst.max((a - b).abs()));
        assert!(
            apart > 1e-3,
            "the second argument never reached the character: {apart}"
        );

        // `.stretch()` is first in the voice's chain, ahead of the insert,
        // so the ceiling holds a stretched voice too, although the vocoder
        // does not preserve level.
        let stretched = peak_of(&format!("{loud}.limit(\"-6\").stretch(0.5)"));
        assert!(
            (stretched - 0.501_187_2).abs() < 1e-4,
            "a stretched voice's -6 dBFS ceiling gave {stretched}"
        );

        // Every voice that asks for a limiter gets one, also past sixteen
        // simultaneous voices: `all(x => x.limit(...))` asks for one per
        // voice in the whole set.
        for voices in [1usize, 16, 17, 40] {
            let stack: Vec<String> = (0..voices)
                .map(|n| {
                    format!(
                        "$: s(\"sawtooth\").note({}).postgain(8).release(4).limit(\"-6\")",
                        40 + n
                    )
                })
                .collect();
            let peak = peak_of(&stack.join("\n"));
            // They sum, so the stack is louder than one voice; what has to
            // hold is that no voice is louder than the ceiling, which the
            // sum being at most `voices` times it is enough to show.
            let each = peak / voices as f32;
            assert!(
                each <= 0.501_187_2 + 1e-3,
                "{voices} limited voices average {each} a voice against a 0.5011872 ceiling: \
                 one of them did not get a limiter"
            );
        }

        // And a ceiling above the signal changes nothing audible.
        let quiet = "$: s(\"bd*4\").postgain(0.2)";
        let plain = peak_of(quiet);
        let limited = peak_of(&format!("{quiet}.limit(\"-1\")"));
        assert!(
            (plain - limited).abs() < 0.01,
            "a ceiling nothing reaches should be inaudible: {plain} vs {limited}"
        );
    }
}

#[cfg(test)]
mod onset_frame_edge_tests {
    use super::*;

    /// The edge is the exact boundary of [`onset_frame_at`]: the instant on
    /// it has the frame, and the instant one unit in the last place before
    /// it has an earlier one. Checked across rates and frame magnitudes,
    /// where the quotient rounds differently.
    #[test]
    fn the_edge_is_the_first_instant_on_its_frame() {
        for sample_rate in [8_000, 44_100, 48_000, 88_200, 96_000, 192_000] {
            let frames = (1..=4_096u64)
                .chain((0..40).map(|power| (1u64 << power) + 1))
                .chain((1..2_000).map(|step| step * 6_000 + 1_280))
                .chain([49_280, 16_800, 1_000_000_007, 172_800_000_000]);
            for frame in frames {
                let edge = onset_frame_edge(frame, sample_rate);
                assert!(
                    onset_frame_at(edge, sample_rate) >= frame,
                    "{sample_rate} Hz: the edge of frame {frame} is on an earlier frame"
                );
                assert!(
                    onset_frame_at(edge.next_down(), sample_rate) < frame,
                    "{sample_rate} Hz: an instant before the edge of frame {frame} is on it"
                );
            }
        }
    }

    /// An onset on the frame before, or within the tolerance after it, is
    /// before the edge. An onset further after it is not: its frame is the
    /// next one.
    #[test]
    fn the_edge_lies_just_past_the_frame_before() {
        let edge = onset_frame_edge(49_280, 48_000);
        assert!(49_279.0 / 48_000.0 < edge);
        assert!(49_279.000_000_5 / 48_000.0 < edge);
        assert!(49_279.7 / 48_000.0 >= edge);
        assert!(49_280.0 / 48_000.0 >= edge);
        // The edge is more than one frame before a takeover time that
        // rounds to the frame.
        assert_eq!(takeover_frame_at(49_280.4 / 48_000.0, 48_000), 49_280);
        assert!(49_280.4 / 48_000.0 - edge > 1.0 / 48_000.0);
    }

    /// No onset has a frame before the first frame, so no onset is before
    /// its edge: not the onset at time zero, and not an onset before time
    /// zero that `onset_frame_at` puts on frame zero.
    #[test]
    fn no_onset_is_before_the_edge_of_the_first_frame() {
        let edge = onset_frame_edge(0, 48_000);
        assert!(edge.is_finite());
        for on_the_first_frame in [0.0, -0.000_000_5 / 48_000.0, -0.9 / 48_000.0] {
            assert_eq!(onset_frame_at(on_the_first_frame, 48_000), 0);
            assert!(on_the_first_frame >= edge);
        }
    }
}
