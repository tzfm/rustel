//! Between the score and the projector.
//!
//! The Session records what a score asked to be drawn; `rustel-hydra` owns the
//! window that draws it. This is the piece in between: it turns a recording
//! into a validated program, samples the patterns behind `H(...)` on the
//! Session's own thread where their semantics are the ones the audio uses, and
//! rate-limits everything so a window never sets the pace of a set.
//!
//! Studio owns one [`HydraBridge`]. `rustel <score>` plays the music and
//! reports that the visuals need Studio.

use std::time::{Duration, Instant};

use rustel_core::purity::PurePattern;
use rustel_fraction::Fraction;
use rustel_hydra::{
    HYDRA_DEFAULT_AUDIO_BINS, HYDRA_MAX_AUDIO_BINS, HYDRA_SIGNAL_SAMPLES, HYDRA_SIGNAL_STEP_MS,
    HYDRA_SOURCE_SLOTS, HYDRA_TUI_FRAMES_PER_SECOND, HydraAudioFrame, HydraEvent, HydraFrames,
    HydraHost, HydraProgram, HydraSignalFrame, HydraSource, HydraStatement, HydraTuiFrame,
    HydraTuiSink,
};

use crate::hap_json::ValueJson;
use crate::hydra_input::HydraInputs;
use crate::samples::ScoreSampleAccess;

pub use crate::hydra_input::{
    HydraCameraPicture, HydraInputPolicy, HydraWebcamPreview, HydraWebcamState, HydraWebcamStatus,
};

// The recording crate intentionally carries only JSON, not a dependency on
// the renderer crate. Keep that boundary while still making protocol drift a
// compile error in the crate that joins the two.
const _: () = assert!(
    rustel_jsruntime::HydraCandidate::PROGRAM_VERSION == rustel_hydra::HYDRA_PROGRAM_VERSION
);
const _: () =
    assert!(rustel_jsruntime::HydraCandidate::SOURCE_SLOTS == rustel_hydra::HYDRA_SOURCE_SLOTS);
const _: () =
    assert!(rustel_jsruntime::HydraCandidate::MAX_AUDIO_BINS == rustel_hydra::HYDRA_MAX_AUDIO_BINS);
const _: () = assert!(
    rustel_jsruntime::HydraCandidate::DEFAULT_AUDIO_BINS == rustel_hydra::HYDRA_DEFAULT_AUDIO_BINS
);
const _: () = assert!(rustel_hydra::glsl::compiles_exactly_these_easings(
    rustel_jsruntime::HydraCandidate::EASINGS
));

/// What one evaluated score wants on screen, once its recording has been read
/// back and checked.
pub struct HydraUpdate {
    pub program: HydraProgram,
    pub signals: Vec<PurePattern>,
}

impl HydraUpdate {
    /// Read a score's recording, refusing anything outside the documented
    /// ceilings before it can reach a window thread.
    pub fn from_candidate(candidate: &rustel_jsruntime::HydraCandidate) -> Result<Self, String> {
        // A score that draws nothing is the instruction to stop, and it says
        // so by recording nothing. There is no sketch to read back, and asking
        // the protocol to parse the absence of one is how the picture came to
        // outlive the score that asked for it.
        if candidate.is_empty() {
            return Ok(Self {
                program: HydraProgram::default(),
                signals: Vec::new(),
            });
        }
        let program: HydraProgram = serde_json::from_value(candidate.program())
            .map_err(|error| format!("hydra sketch could not be read back: {error}"))?;
        program.validate().map_err(|error| error.to_string())?;
        Ok(Self {
            program,
            signals: candidate.signals().to_vec(),
        })
    }
}

/// How the terminal wants its frames produced.
///
/// Studio's requested delivery dimensions take precedence over a score's
/// fallback `initHydra({ width, height })` dimensions. This describes what
/// crosses the wire and how it is scaled to get there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HydraFrameRequest {
    /// The size the score's backdrop is read at.
    pub score: (u16, u16),
    /// The size the shelf's full-screen background preview is read at.
    pub preview: (u16, u16),
    /// Whether scaling down averages the source pixels or picks one.
    pub smoothing: bool,
}

/// How often the signal schedule is refreshed.
///
/// Each frame carries a third of a second of values, so a refresh is due long
/// before the last one runs out and a late one repeats a value rather than
/// leaving a hole.
const SIGNAL_INTERVAL: Duration = Duration::from_millis(200);

const TUI_INTERVAL: Duration =
    Duration::from_nanos(1_000_000_000 / HYDRA_TUI_FRAMES_PER_SECOND as u64);

/// The engine's end of the visuals window.
pub struct HydraBridge {
    // Inputs drop first, invalidating camera/image workers while their weak
    // renderer sink still has a live host behind it.
    inputs: HydraInputs,
    host: HydraHost,
    /// Raw transport state. Effective input ownership is this AND a score
    /// Hydra program being installed; apply can change the latter while the
    /// transport keeps running.
    transport_drawing: bool,
    previewing: bool,
    theme_installed: bool,
    /// A camera-backed theme is not allowed to open its device until this
    /// exact theme epoch has produced a visible frame successfully.
    pending_theme_camera: Option<u64>,
    /// Active `a.fft` shape for the score renderer. The GPU keeps sixteen
    /// uniform slots, but hydra-synth divides the spectrum by this count.
    audio_bins: usize,
    signals: Vec<PurePattern>,
    next_signal: Option<Instant>,
    next_tui: Option<Instant>,
}

impl Default for HydraBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl HydraBridge {
    /// A bridge with no window, no thread, and nothing running.
    pub fn new() -> Self {
        let host = HydraHost::new();
        let inputs = HydraInputs::new(host.input_sink());
        Self {
            inputs,
            host,
            transport_drawing: false,
            previewing: false,
            theme_installed: false,
            pending_theme_camera: None,
            audio_bins: HYDRA_DEFAULT_AUDIO_BINS,
            signals: Vec::new(),
            next_signal: None,
            next_tui: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.host.is_open()
    }

    /// A handle the terminal thread can publish its frames through, taken
    /// before the bridge moves to the thread that owns the Session.
    pub fn tui_sink(&self) -> HydraTuiSink {
        self.host.tui_sink()
    }

    /// A non-queued privacy handle for the terminal settings surface.
    pub fn input_policy(&self) -> HydraInputPolicy {
        self.inputs.policy()
    }

    /// A handle the terminal thread can pull Hydra's own frames through, for
    /// the mode that draws them behind the code.
    pub fn frames(&self) -> HydraFrames {
        self.host.frames()
    }

    /// A handle for the shelf's own picture, which is a second Hydra and not
    /// the one the score is drawing through.
    pub fn preview_frames(&self) -> HydraFrames {
        self.host.preview_frames()
    }

    /// A handle for the theme's own picture - furniture behind the code,
    /// shown whenever the score's picture is not.
    pub fn theme_frames(&self) -> HydraFrames {
        self.host.theme_frames()
    }

    /// Tell the renderer how large a frame each stream should send.
    ///
    /// Hydra keeps rendering at full size; this is only what crosses the wire.
    /// The terminal is the one who knows how much of it will be looked at.
    pub fn set_frames_wanted(&self, wanted: HydraFrameRequest) {
        self.host.set_frame_size(wanted.score.0, wanted.score.1);
        self.host
            .set_preview_frame_size(wanted.preview.0, wanted.preview.1);
        self.host.set_smoothing(wanted.smoothing);
    }

    pub fn wants_tui(&self) -> bool {
        self.host.wants_tui()
    }

    pub fn wants_audio(&self) -> bool {
        self.host.wants_audio()
    }

    /// Tell the renderer whether the set is playing. A stopped set wipes the
    /// picture rather than leaving the last frame on screen.
    pub fn set_drawing(&mut self, drawing: bool) {
        self.transport_drawing = drawing;
        self.host.set_drawing(drawing);
        // A music-only score can be playing while the theme remains the
        // visible backdrop. Input ownership follows the same effective score
        // picture condition as the host, not the raw transport state.
        self.inputs.set_drawing(drawing && self.host.is_open());
    }

    pub fn take_events(&self) -> Vec<HydraEvent> {
        let mut events = self.host.take_events();
        events.extend(self.inputs.take_events());
        events
    }

    /// Install what a score asked for. An empty program stops drawing.
    ///
    /// Remote images have no origin grants through this entry point. Hosts
    /// permitting them use [`Self::apply_with_sample_access`] with the session's
    /// sample access policy.
    ///
    /// The score and shelf use separate renderers, so re-evaluating while
    /// browsing updates the score's picture ready for when the preview ends.
    pub fn apply(&mut self, update: HydraUpdate) -> Result<(), String> {
        self.apply_with_sample_access(update, &ScoreSampleAccess::denied())
    }

    /// Install a score with its host-granted sample origin scope. Image
    /// workers carry a snapshot and recheck it before every network fetch.
    /// A grant does not relax the public HTTPS, CORS or same-origin redirect
    /// requirements for images.
    pub fn apply_with_sample_access(
        &mut self,
        update: HydraUpdate,
        access: &ScoreSampleAccess,
    ) -> Result<(), String> {
        let sources = source_plan(&update.program);
        let audio_bins = configured_audio_bins(&update.program);
        self.signals = update.signals;
        self.next_signal = None;
        self.host
            .apply(update.program)
            .map_err(|error| error.to_string())?;
        self.audio_bins = audio_bins;
        self.inputs.set_score_sample_access(access.clone());
        self.inputs.configure(sources);
        // `apply` can add or remove the score's Hydra program while audio is
        // already running. Recompute ownership even though transport did not
        // send another state transition.
        self.inputs
            .set_drawing(self.transport_drawing && self.host.is_open());
        Ok(())
    }

    /// Show a snippet in the shelf's own picture, or, with `None`, stop.
    ///
    /// The score keeps its screen throughout - the snippet renders in a second
    /// Hydra, which is the whole reason there is a second one.
    pub fn preview(&mut self, code: Option<&str>) -> Result<(), String> {
        if code.is_none() && !self.previewing {
            return Ok(());
        }
        self.previewing = code.is_some();
        self.host.preview(code).map_err(|error| error.to_string())
    }

    /// Whether the shelf is showing a snippet of its own.
    pub fn is_previewing(&self) -> bool {
        self.previewing
    }

    /// Install, replace or clear the theme's own sketch - one chain, the
    /// shelf-snippet grammar, no `initHydra` anywhere.
    pub fn theme(&mut self, code: Option<&str>) -> Result<(), String> {
        self.theme_with_camera(code, false)
    }

    /// Install the theme sketch and declare whether its `s0` is the default
    /// camera. Acquisition remains gated by the live webcam policy and only
    /// runs while the theme, rather than a score renderer, is visible.
    pub fn theme_with_camera(&mut self, code: Option<&str>, webcam: bool) -> Result<(), String> {
        if webcam {
            let code = code
                .map(str::trim)
                .filter(|code| !code.is_empty())
                .ok_or_else(|| {
                    "a camera-backed theme needs a Hydra chain that visibly samples s0".to_owned()
                })?;
            let node = rustel_hydra::glsl::parse_chain(code).map_err(|error| error.to_string())?;
            if !node_uses_s0(&node) {
                return Err(
                    "a camera-backed theme needs a Hydra chain that visibly samples s0".into(),
                );
            }
        }
        if code.is_none() && !self.theme_installed {
            self.inputs.set_theme_camera(false);
            self.pending_theme_camera = None;
            return Ok(());
        }
        // Revoke the old theme's camera before enqueueing any replacement.
        // The new theme earns acquisition only after the renderer confirms a
        // successful draw/read for its exact epoch.
        self.inputs.set_theme_camera(false);
        self.pending_theme_camera = None;
        self.host.theme(code).map_err(|error| error.to_string())?;
        self.theme_installed = code.is_some();
        if code.is_some() && webcam {
            self.pending_theme_camera = Some(self.host.theme_epoch());
        }
        Ok(())
    }

    /// Refresh the values behind `H(...)`, at most five times a second.
    ///
    /// The patterns are pure - `H()` refuses anything else - so this is a
    /// handful of native queries with no JavaScript, no allocation of
    /// consequence, and no lock. It runs where the Session lives, so a signal
    /// reads exactly what the audio would read at the same cycle.
    ///
    /// A panic in a signal query is a score fault: this drops the score's
    /// signals and returns the panic for the host to report.
    pub fn tick(&mut self, now: Instant, cycle: f64, cps: f64) -> Result<(), String> {
        if let Some(epoch) = self.pending_theme_camera
            && self.host.theme_drawn(epoch)
        {
            self.activate_theme_camera_epoch(epoch);
        }
        // Policy changes arrive through an atomic handle retained by the UI,
        // not through the engine command queue. Observe them even when this
        // sketch has no H() signals (the overwhelmingly common case).
        self.inputs.reconcile_cameras();
        if self.signals.is_empty() || !self.host.is_open() {
            return Ok(());
        }
        if self.next_signal.is_some_and(|next| now < next) {
            return Ok(());
        }
        self.next_signal = Some(now + SIGNAL_INTERVAL);
        let step_cycles = (HYDRA_SIGNAL_STEP_MS / 1000.0) * cps;
        let signals = &self.signals;
        let slots = match crate::catch_score_panic(|| {
            signals
                .iter()
                .map(|signal| sample(signal, cycle, step_cycles))
                .collect()
        }) {
            Ok(slots) => slots,
            Err(message) => {
                self.signals.clear();
                return Err(format!(
                    "Hydra signal panicked; signals stopped ({message})"
                ));
            }
        };
        self.host.signals(&HydraSignalFrame {
            step: HYDRA_SIGNAL_STEP_MS,
            slots,
        });
        Ok(())
    }

    fn activate_theme_camera_epoch(&mut self, epoch: u64) {
        if self.pending_theme_camera != Some(epoch) {
            return;
        }
        self.pending_theme_camera = None;
        self.inputs.set_theme_camera(true);
    }

    /// Publish a frame of the terminal for `feedStrudel`, at most thirty times
    /// a second. The terminal redraws faster than that when a set is busy.
    pub fn tui(&mut self, now: Instant, frame: impl FnOnce() -> HydraTuiFrame) {
        if !self.host.wants_tui() {
            return;
        }
        if self.next_tui.is_some_and(|next| now < next) {
            return;
        }
        self.next_tui = Some(now + TUI_INTERVAL);
        let _ = self.tui_sink().publish(&frame());
    }

    /// Publish what the engine is playing, for `detectAudio`.
    pub fn audio(&self, frame: &HydraAudioFrame) {
        if !self.host.wants_audio() {
            return;
        }
        // The engine retains the full spectrum for its scopes. Re-slice it
        // here using this program's `a.setBins(n)`, exactly where the score's
        // typed analyser setting and live audio meet.
        if frame.spectrum.is_empty() {
            let mut configured = frame.clone();
            configured.bins[self.audio_bins..].fill(0.0);
            self.host.audio(&configured);
        } else {
            self.host.audio(&audio_frame_with_bins(
                frame.rms,
                &frame.spectrum,
                self.audio_bins,
            ));
        }
    }
}

/// Keep the privacy invariant at the bridge boundary as well as in Studio's
/// theme loader: another product surface must not be able to request a camera
/// for a chain that never makes those pixels visible.
fn node_uses_s0(node: &rustel_hydra::HydraNode) -> bool {
    rustel_hydra::glsl::explicitly_samples(node, "s0")
}

fn source_plan(program: &HydraProgram) -> [Option<HydraSource>; HYDRA_SOURCE_SLOTS] {
    let mut sources = std::array::from_fn(|_| None);
    for statement in &program.statements {
        match statement {
            HydraStatement::ConfigureSource { slot, source } => {
                if let Some(target) = sources.get_mut(usize::from(*slot)) {
                    *target = Some(source.clone());
                }
            }
            HydraStatement::ClearSource { slot } => {
                if let Some(target) = sources.get_mut(usize::from(*slot)) {
                    *target = None;
                }
            }
            HydraStatement::Evaluate {
                node: rustel_hydra::HydraNode::Chain { head, args, calls },
            } if head == "hush" && args.is_empty() && calls.is_empty() => sources.fill(None),
            _ => {}
        }
    }
    sources
}

/// Last analyser configuration wins, matching sequential evaluation in
/// hydra-synth. A new program starts from Hydra's four-bin constructor
/// default rather than inheriting the previous score's setting.
fn configured_audio_bins(program: &HydraProgram) -> usize {
    program
        .statements
        .iter()
        .filter_map(|statement| match statement {
            HydraStatement::ConfigureAudio { bins } => Some(usize::from(*bins)),
            _ => None,
        })
        .next_back()
        .unwrap_or(HYDRA_DEFAULT_AUDIO_BINS)
}

/// One slot's worth of schedule: the value `queryArc(t, t)` would give, once
/// per display frame, for the next third of a second.
fn sample(signal: &PurePattern, cycle: f64, step_cycles: f64) -> Vec<serde_json::Value> {
    let pattern = signal.pattern();
    (0..HYDRA_SIGNAL_SAMPLES)
        .map(|index| {
            let at = cycle + step_cycles * index as f64;
            let Some(at) = Fraction::from_f64(at) else {
                return serde_json::Value::Null;
            };
            // Upstream's `H` is `reify(p).queryArc(getTime(), getTime())[0].value`
            // - a zero-width query at the transport's current position, and
            // whatever is sounding there.
            match pattern.query_arc(at, at).first() {
                Some(hap) => match ValueJson::from_value(&hap.value) {
                    ValueJson::Null => serde_json::Value::Null,
                    ValueJson::Bool(value) => serde_json::Value::Bool(value),
                    ValueJson::Number(value) => serde_json::Number::from_f64(value)
                        .map_or(serde_json::Value::Null, serde_json::Value::Number),
                    ValueJson::String(value) => serde_json::Value::String(value),
                    ValueJson::Raw(value) => value,
                },
                None => serde_json::Value::Null,
            }
        })
        .collect()
}

/// Hydra's default four bands and a level, backed by the fixed-capacity native
/// uniform, from the spectrum the engine already computes for its scopes.
pub fn audio_frame(rms: f32, spectrum: &[f32]) -> HydraAudioFrame {
    audio_frame_with_bins(rms, spectrum, HYDRA_DEFAULT_AUDIO_BINS)
}

/// Divide a spectrum into the configured count of contiguous bands.
///
/// Upstream uses `floor(spectrum.len / bins)` as its spacing. It discards an
/// incomplete tail rather than shifting every boundary, so do the same here;
/// the remaining fixed-capacity uniform slots stay zero.
fn audio_frame_with_bins(rms: f32, spectrum: &[f32], active_bins: usize) -> HydraAudioFrame {
    debug_assert!((1..=HYDRA_MAX_AUDIO_BINS).contains(&active_bins));
    let mut bins = [0.0_f32; rustel_hydra::HYDRA_AUDIO_BINS];
    if !spectrum.is_empty() {
        let width = (spectrum.len() / active_bins).max(1);
        for (index, bin) in bins[..active_bins].iter_mut().enumerate() {
            let from = (index * width).min(spectrum.len());
            let to = ((index + 1) * width).min(spectrum.len());
            let band = &spectrum[from..to];
            if !band.is_empty() {
                // The analysis bins are dBFS. `a.fft` upstream is the
                // browser analyser's normalised 0..1 (floor -100 dB, ceil
                // -30 dB). A sketch writes `scale(0.8 + a.fft[0] * 1.4)`
                // against that range, so map decibels onto it here.
                let level = band.iter().copied().sum::<f32>() / band.len() as f32;
                *bin = ((level + 100.0) / 70.0).clamp(0.0, 1.0);
            }
        }
    }
    HydraAudioFrame {
        bins,
        rms,
        spectrum: spectrum.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_jsruntime::HydraCandidate;

    fn recording(source: &str) -> Box<HydraCandidate> {
        let mut session = crate::Session::new().expect("session");
        session.evaluate(source).expect("score");
        session.take_pending_hydra().expect("a recording")
    }

    #[test]
    fn the_last_configuration_for_each_source_slot_is_the_acquisition_plan() {
        let program = HydraProgram {
            statements: vec![
                HydraStatement::ConfigureSource {
                    slot: 0,
                    source: HydraSource::Camera { device: None },
                },
                HydraStatement::ConfigureSource {
                    slot: 0,
                    source: HydraSource::ImageUrl {
                        url: "https://example.com/latest.png".into(),
                    },
                },
                HydraStatement::ConfigureSource {
                    slot: 2,
                    source: HydraSource::Camera { device: Some(3) },
                },
            ],
            ..HydraProgram::default()
        };
        assert_eq!(
            source_plan(&program),
            [
                Some(HydraSource::ImageUrl {
                    url: "https://example.com/latest.png".into(),
                }),
                None,
                Some(HydraSource::Camera { device: Some(3) }),
                None,
            ]
        );
    }

    #[test]
    fn clear_and_hush_release_sources_sequentially() {
        let command = |head: &str| HydraStatement::Evaluate {
            node: rustel_hydra::HydraNode::Chain {
                head: head.into(),
                args: Vec::new(),
                calls: Vec::new(),
            },
        };
        let program = HydraProgram {
            statements: vec![
                HydraStatement::ConfigureSource {
                    slot: 0,
                    source: HydraSource::Camera { device: None },
                },
                HydraStatement::ClearSource { slot: 0 },
                HydraStatement::ConfigureSource {
                    slot: 1,
                    source: HydraSource::Camera { device: Some(1) },
                },
                command("hush"),
                HydraStatement::ConfigureSource {
                    slot: 2,
                    source: HydraSource::Camera { device: Some(2) },
                },
            ],
            ..HydraProgram::default()
        };
        assert_eq!(
            source_plan(&program),
            [
                None,
                None,
                Some(HydraSource::Camera { device: Some(2) }),
                None,
            ]
        );
    }

    #[test]
    fn apply_recomputes_score_and_theme_camera_ownership_while_transport_runs() {
        let mut bridge = HydraBridge::new();
        bridge.inputs.set_theme_camera(true);
        bridge.set_drawing(true);
        assert_eq!(
            bridge
                .inputs
                .desired_camera(0)
                .map(|request| request.purpose),
            Some(crate::hydra_input::CameraPurpose::Theme),
            "music-only playback leaves the visible theme camera active"
        );

        let camera = HydraProgram {
            statements: vec![HydraStatement::ConfigureSource {
                slot: 0,
                source: HydraSource::Camera { device: None },
            }],
            ..HydraProgram::default()
        };
        bridge
            .apply(HydraUpdate {
                program: camera,
                signals: Vec::new(),
            })
            .expect("camera program");
        assert_eq!(
            bridge
                .inputs
                .desired_camera(0)
                .map(|request| request.purpose),
            Some(crate::hydra_input::CameraPurpose::Score),
            "adding Hydra while already playing transfers ownership to the score"
        );

        bridge
            .apply(HydraUpdate {
                program: HydraProgram::default(),
                signals: Vec::new(),
            })
            .expect("music-only program");
        assert_eq!(
            bridge
                .inputs
                .desired_camera(0)
                .map(|request| request.purpose),
            Some(crate::hydra_input::CameraPurpose::Theme),
            "removing Hydra while transport runs hands ownership back"
        );
    }

    #[test]
    fn theme_camera_request_requires_the_chain_to_sample_s0() {
        let mut bridge = HydraBridge::new();
        for invisible in [
            "osc(8, 0.1).out()",
            "osc(3).out(s0)",
            "osc(3).out(src(s0))",
            "osc(src(s0), 0.1, 0).out()",
            "src(src(s0)).out()",
            "src(s0).blend(1).out()",
            "src(s0).blend(src(1)).out()",
            "src(s0).sum().out()",
            "osc(3, 0.1, 0, s0).out()",
            "osc(3).color(1, 1, 1, 1, s0).out()",
            "render(s0)",
            "render(src(s0))",
        ] {
            let error = bridge
                .theme_with_camera(Some(invisible), true)
                .expect_err("an invisible camera request must be refused");
            assert!(error.contains("visibly samples s0"), "{invisible}: {error}");
            assert_eq!(bridge.inputs.desired_camera(0), None);
        }

        bridge.inputs.set_theme_camera(true);
        bridge
            .theme_with_camera(Some("osc(8, 0.1).modulate(src(s0), 0.2).out()"), true)
            .expect("nested s0 is visible input");
        assert_eq!(
            bridge.inputs.desired_camera(0),
            None,
            "a replacement revokes the old camera until its exact new theme draws"
        );
        assert_eq!(bridge.pending_theme_camera, Some(bridge.host.theme_epoch()));
        let epoch = bridge
            .pending_theme_camera
            .expect("camera waits on an epoch");
        bridge.activate_theme_camera_epoch(epoch.wrapping_add(1));
        assert_eq!(bridge.inputs.desired_camera(0), None);
        bridge.activate_theme_camera_epoch(epoch);
        assert_eq!(
            bridge
                .inputs
                .desired_camera(0)
                .map(|request| request.purpose),
            Some(crate::hydra_input::CameraPurpose::Theme),
            "only the exact successful-render acknowledgement may activate capture"
        );
    }

    #[test]
    fn a_signal_schedule_carries_the_values_the_pattern_holds() {
        let candidate = recording(
            "await initHydra()\nlet pattern = \"3 4 5 [6 7]*2\"\nshape(H(pattern)).out(o0)",
        );
        let update = HydraUpdate::from_candidate(&candidate).expect("reads back");
        assert_eq!(update.program.signals, 1, "the score declared one signal");
        assert_eq!(update.signals.len(), 1, "and its pattern came with it");

        // Samples are one display frame apart at 0.5 cps from cycle 0. Each
        // must be the pattern's value there, never the 0 of a missing signal.
        let step_cycles = (HYDRA_SIGNAL_STEP_MS / 1000.0) * 0.5;
        let samples = sample(&update.signals[0], 0.0, step_cycles);
        assert_eq!(samples.len(), HYDRA_SIGNAL_SAMPLES);
        assert!(
            samples.iter().all(|value| value.as_f64() == Some(3.0)),
            "the first third of a second of `3 4 5 [6 7]*2` is 3: {samples:?}"
        );

        // A third of the way through the cycle the pattern says 4.
        let later = sample(&update.signals[0], 0.3, step_cycles);
        assert_eq!(later[0].as_f64(), Some(4.0), "{later:?}");
    }

    #[test]
    fn audio_frame_uses_hydras_default_four_band_spacing() {
        let spectrum: Vec<f32> = (0..rustel_hydra::HYDRA_AUDIO_BINS)
            .map(|index| -100.0 + index as f32 * 4.0)
            .collect();
        let frame = audio_frame(0.25, &spectrum);
        assert_eq!(frame.bins.len(), rustel_hydra::HYDRA_AUDIO_BINS);
        assert!((frame.bins[0] - (6.0 / 70.0)).abs() < 1e-6);
        assert!((frame.bins[3] - (54.0 / 70.0)).abs() < 1e-6);
        assert_eq!(frame.bins[4..], [0.0; 12]);
        assert_eq!(frame.rms, 0.25);
        assert_eq!(frame.spectrum, spectrum);
    }

    #[test]
    fn set_bins_eight_uses_eight_band_spacing_and_clears_the_unused_capacity() {
        let spectrum: Vec<f32> = (0..rustel_hydra::HYDRA_AUDIO_BINS)
            .map(|index| -100.0 + index as f32 * 4.0)
            .collect();
        let frame = audio_frame_with_bins(0.25, &spectrum, 8);
        // Eight bins across sixteen spectrum points means pairs: these are
        // deliberately different from the former fixed-sixteen mapping.
        assert!((frame.bins[4] - (34.0 / 70.0)).abs() < 1e-6);
        assert!((frame.bins[6] - (50.0 / 70.0)).abs() < 1e-6);
        assert_eq!(frame.bins[8..], [0.0; 8]);
    }

    #[test]
    fn analyser_bin_count_defaults_to_four_and_the_last_setting_wins() {
        assert_eq!(
            configured_audio_bins(&HydraProgram::default()),
            HYDRA_DEFAULT_AUDIO_BINS
        );
        let program = HydraProgram {
            statements: vec![
                HydraStatement::ConfigureAudio { bins: 12 },
                HydraStatement::ConfigureAudio { bins: 8 },
            ],
            ..HydraProgram::default()
        };
        assert_eq!(configured_audio_bins(&program), 8);
    }
}
