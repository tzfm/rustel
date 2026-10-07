//! Bounded actor around the non-`Send` studio engine.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{
    Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError, sync_channel,
};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustel_runtime::ui_analysis::UiAudioAnalysisSet;
use rustel_runtime::ui_events::{
    UiAudioMetadata, UiEventSendStatus, UiLayoutEnvelope, UiTraceBatchRequest,
};

use super::engine::Launch;
use super::engine::{
    StudioConfig, StudioDiagnostic, StudioEngine, StudioInstall, StudioMasterBus, StudioSnapshot,
    StudioStop, StudioStopHandle, StudioTick, StudioUpdate, StudioUpdateSendResult,
};
use super::prebake::PrebakeScope;
use super::wav::TakeStatus;
use rustel_runtime::RuntimeError;

const COMMAND_CAPACITY: usize = 2;
const CONTROL_CAPACITY: usize = 32;
const TRACE_CAPACITY: usize = 8;
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(100);
const IDLE_POLL: Duration = Duration::from_millis(50);
const MAX_PENDING_CONTROL: usize = 16;

#[derive(Clone, Debug)]
pub struct EngineFailure {
    pub kind: String,
    pub message: String,
    pub recoverable: bool,
    pub playback_stopped: bool,
}

impl EngineFailure {
    fn runtime(error: &rustel_runtime::RuntimeError, recoverable: bool) -> Self {
        Self {
            kind: error.kind().to_owned(),
            message: error.to_string(),
            recoverable,
            playback_stopped: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EvaluationOutcome {
    pub request_id: u64,
    pub editor_revision: u64,
    pub result: Result<StudioInstall, EngineFailure>,
}

/// What became of one setup evaluation.
///
/// Deliberately not an [`EvaluationOutcome`]: a prebake publishes no
/// generation, no layout and no cutover, so everything the interface does
/// with an evaluation's answer would be wrong about this one.
#[derive(Clone, Debug)]
pub struct PrebakeOutcome {
    pub request_id: u64,
    pub scope: PrebakeScope,
    pub result: Result<(), EngineFailure>,
}

/// A recording command's answer or an automatic final result, not a
/// capture-completeness report.
#[derive(Clone, Debug)]
pub struct RecordingOutcome {
    /// None for a take ended automatically, without an explicit close request.
    pub request_id: Option<u64>,
    pub result: Result<RecordingReply, EngineFailure>,
}

#[derive(Clone, Debug)]
pub enum RecordingReply {
    Started {
        capture_id: u64,
    },
    /// The writer was joined; `status.error` still determines success.
    Finished {
        capture_id: u64,
        status: TakeStatus,
    },
    /// No active or closing recorder remains.
    NoActiveTake,
}

/// The answer to starting or finishing a sample recorded from the input.
#[derive(Clone, Debug)]
pub struct SampleRecordingOutcome {
    pub request_id: u64,
    pub result: Result<SampleReply, EngineFailure>,
}

#[derive(Clone, Debug)]
pub enum SampleReply {
    Started,
    /// The writer was joined; `status.error` still decides success.
    Finished(TakeStatus),
    /// Asked to finish with nothing recording.
    NotRecording,
}

#[derive(Clone, Debug)]
pub enum StudioControlEvent {
    Evaluation(EvaluationOutcome),
    Prebake(PrebakeOutcome),
    Recording(RecordingOutcome),
    SampleRecording(SampleRecordingOutcome),
    Layout(UiLayoutEnvelope),
    Diagnostic(StudioDiagnostic),
    Snapshot(Box<StudioSnapshot>),
    Stopped(Box<StudioStop>),
    EngineFailure(EngineFailure),
}

#[derive(Debug)]
enum EngineCommand {
    Evaluate {
        request_id: u64,
        editor_revision: u64,
        command_epoch: u64,
        source: Arc<str>,
        mini: bool,
        launch: Launch,
        /// The score starts from its own cycle zero rather than joining
        /// the cycle already running. Orthogonal to `launch`, which says
        /// WHEN it lands: a rewinding launch can still wait for a line,
        /// and then the line it lands on is its cycle zero.
        rewind: bool,
        /// A snippet previewed under the set rather than the performer's
        /// own score: what it sounds is not kept for the set once the
        /// score is put back. See
        /// [`StudioEngine::mark_next_install_preview`].
        preview: bool,
    },
    /// Setup JavaScript for the session heap.
    ///
    /// No `mini`: a prebake is JavaScript by definition. No `launch`: setup
    /// replaces no graph, so there is no cycle line to wait for. And no
    /// `command_epoch`: the epoch exists so a Stop can discard a queued
    /// score evaluation that would restart the transport it just cut, and a
    /// prebake never touches the transport. Dropping one would only lose a
    /// setup the artist asked for, on a tab they may have closed since.
    EvaluatePrebake {
        request_id: u64,
        scope: PrebakeScope,
        source: Arc<str>,
    },
    Stop,
    StopImmediate,
    /// Move playback onto a named audio output.
    SetOutput(String),
    /// The output buffer size to ask for, in frames; `None` returns to the
    /// automatic policy. A playing set recycles its output to apply it.
    SetOutputBufferFrames(Option<u32>),
    /// Choose the audio input `s("in")` plays, or none.
    SetInput(Option<String>),
    /// The mixer: one orbit's fader, linear.
    SetOrbitGain {
        orbit: u8,
        gain: f32,
    },
    /// The mixer: the audio input's fader, linear.
    SetInputGain(f32),
    /// Move one of the running score's sliders without re-evaluating.
    SetSlider {
        id: String,
        value: f64,
        smooth: bool,
    },
    /// Play one sound once, outside the score.
    Audition(String, f32),
    /// Play several notes at once on one sound: a chord from the browser.
    AuditionNotes(Vec<f32>, String, f32),
    /// The same, one note after another: a scale as it is heard.
    AuditionRun(Vec<f32>, String, f32, f64),
    /// Silence the sounding preview, leaving the score alone.
    StopAudition,
    ConfigurePiano {
        sound: super::PianoSound,
        volume: u16,
        prepare: bool,
    },
    PianoNoteOn {
        key: u8,
        note: u8,
        velocity: u8,
    },
    PianoNoteOff(u8),
    StopPiano,
    /// Silence everything sounding, for a swapped audition of a whole
    /// snippet: the one being replaced must stop, not ring on under it.
    CutSounding,
    /// Which notes are bound to scene launch. A launch pad is a transport
    /// button, not a key: its press must never arm the keys watcher's
    /// at-once requery, which lands beside the launch's own install and
    /// gets chopped by it - the pad-rewind glitch.
    SetLaunchPads(Vec<(u8, u8)>),
    /// Fetch the map of a `samples("…")` the score names, ahead of the
    /// score being evaluated.
    LookUpSamples(String),
    /// Show a snippet from the shelf in its own picture, or `None` to stop.
    #[cfg(feature = "hydra")]
    HydraPreview(Option<String>),
    /// The theme's own sketch, or none, and whether its s0 requests a camera.
    #[cfg(feature = "hydra")]
    HydraTheme {
        code: Option<String>,
        webcam: bool,
    },
    /// How large a frame each Hydra stream should send - score, then preview -
    /// and whether scaling to it averages or picks.
    #[cfg(feature = "hydra")]
    HydraFrameSize(rustel_runtime::hydra::HydraFrameRequest),
    /// Preview RAM ceiling and unused-sample idle, from the settings sheet.
    SetSampleMemory {
        budget_bytes: usize,
        idle: Duration,
    },
    /// What the performer can reach without a wait: the tabs whose sounds
    /// no memory policy may drop, and those kept by recency.
    SetLiveMaterial(super::engine::LiveMaterial),
    /// Start a take at the path, or close the one in progress.
    Record {
        request_id: u64,
        path: Option<std::path::PathBuf>,
    },
    /// Start recording a sample from the input at the path, or finish the
    /// one in progress.
    RecordSample {
        request_id: u64,
        path: Option<std::path::PathBuf>,
    },
    /// Send an orbit to an output pair.
    RouteOrbit {
        orbit: u8,
        pair: u8,
    },
    /// Send MIDI clock to this port, or none.
    ClockOut(Option<String>),
    /// Follow MIDI clock from this port, or none.
    ClockIn(Option<String>),
    /// Which MIDI ports this machine may open, either way.
    MidiEnablement(super::devices::MidiEnablement),
    #[cfg(test)]
    InjectPanic(rustel_runtime::SessionPanicPoint),
    Shutdown,
}

#[derive(Clone, Debug)]
pub enum EvaluationSendError {
    Full(Arc<str>),
    Disconnected(Arc<str>),
}

impl EvaluationSendError {
    pub fn into_source(self) -> Arc<str> {
        match self {
            Self::Full(source) | Self::Disconnected(source) => source,
        }
    }
}

impl fmt::Display for EvaluationSendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(_) => formatter.write_str("studio evaluation queue is full"),
            Self::Disconnected(_) => formatter.write_str("studio engine worker is disconnected"),
        }
    }
}

impl std::error::Error for EvaluationSendError {}

/// UI-side handle. Structural/control events, trace batches and audio frames
/// have independent bounds so a dense score cannot bury an evaluation error
/// or the stop acknowledgement behind visualization traffic.
pub struct StudioWorker {
    commands: SyncSender<EngineCommand>,
    controls: Receiver<StudioControlEvent>,
    traces: Receiver<UiTraceBatchRequest>,
    latest_audio: Arc<Mutex<Option<(UiAudioMetadata, UiAudioAnalysisSet)>>>,
    stop: StudioStopHandle,
    /// Set by the UI's second Stop press. Unlike the bounded command, this
    /// cannot be lost behind a full queue.
    immediate_stop_requested: Arc<AtomicBool>,
    master: Arc<StudioMasterBus>,
    library: Option<Arc<rustel_runtime::samples::SampleLibrary>>,
    catalogue: super::catalogue::Catalogue,
    command_epoch: Arc<AtomicU64>,
    shutdown_requested: Arc<AtomicBool>,
    next_recording_request: AtomicU64,
    join: Option<JoinHandle<()>>,
}

impl StudioWorker {
    /// Start the engine worker.
    ///
    /// The visuals bridge is handed over rather than created here because the
    /// terminal thread needs its frame sink first - the two halves of the
    /// window feed live on different threads by design.
    pub fn spawn(
        config: StudioConfig,
        #[cfg(feature = "hydra")] hydra: rustel_runtime::hydra::HydraBridge,
    ) -> Result<Self, EngineFailure> {
        Self::spawn_engine(
            move || StudioEngine::new(config),
            #[cfg(feature = "hydra")]
            hydra,
        )
    }

    fn spawn_engine(
        make_engine: impl FnOnce() -> Result<StudioEngine, RuntimeError> + Send + 'static,
        #[cfg(feature = "hydra")] hydra: rustel_runtime::hydra::HydraBridge,
    ) -> Result<Self, EngineFailure> {
        let (commands, command_rx) = sync_channel(COMMAND_CAPACITY);
        let (control_tx, controls) = sync_channel(CONTROL_CAPACITY);
        let (trace_tx, traces) = sync_channel(TRACE_CAPACITY);
        let latest_audio = Arc::new(Mutex::new(None));
        let engine_audio = Arc::clone(&latest_audio);
        let command_epoch = Arc::new(AtomicU64::new(0));
        let engine_epoch = Arc::clone(&command_epoch);
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let engine_shutdown = Arc::clone(&shutdown_requested);
        let immediate_stop_requested = Arc::new(AtomicBool::new(false));
        let engine_immediate_stop = Arc::clone(&immediate_stop_requested);
        let (ready_tx, ready_rx) = sync_channel(1);

        let join = thread::Builder::new()
            .name("studio-engine".into())
            .stack_size(rustel_runtime::QUERY_WORKER_STACK_BYTES)
            .spawn(move || {
                // "Pro Audio" class on Windows, one step below the output
                // callback's own registration (see rustel_audio::mmcss): the
                // feeder keeps its cadence on a busy machine and never
                // outranks the thread it feeds. A no-op everywhere else.
                #[cfg(windows)]
                let _mmcss = rustel_audio::mmcss::ProAudioThread::attach_normal();
                let engine = make_engine();
                match engine {
                    Ok(engine) => {
                        let _ = ready_tx.send(Ok((
                            engine.stop_handle(),
                            engine.master_bus(),
                            engine.sample_library(),
                        )));
                        run_engine(
                            engine,
                            command_rx,
                            control_tx,
                            trace_tx,
                            engine_audio,
                            engine_epoch,
                            engine_shutdown,
                            engine_immediate_stop,
                            #[cfg(feature = "hydra")]
                            hydra,
                        );
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(EngineFailure::runtime(&error, false)));
                    }
                }
            })
            .map_err(|error| EngineFailure {
                kind: "io".into(),
                message: format!("could not start studio engine worker: {error}"),
                recoverable: false,
                playback_stopped: true,
            })?;

        let (stop, master, library) = ready_rx.recv().map_err(|_| EngineFailure {
            kind: "engine".into(),
            message: "studio engine stopped during startup".into(),
            recoverable: false,
            playback_stopped: true,
        })??;

        Ok(Self {
            commands,
            controls,
            traces,
            latest_audio,
            stop,
            immediate_stop_requested,
            master,
            catalogue: super::catalogue::Catalogue::new(library.as_ref()).map_err(|error| {
                EngineFailure {
                    kind: "io".into(),
                    message: format!("could not start sample catalogue worker: {error}"),
                    recoverable: false,
                    playback_stopped: true,
                }
            })?,
            library,
            command_epoch,
            shutdown_requested,
            next_recording_request: AtomicU64::new(1),
            join: Some(join),
        })
    }

    /// Queue an evaluation without ever waiting behind the engine. Ownership
    /// is returned when the two-slot queue is full so the app can coalesce it
    /// with a newer editor revision.
    pub fn try_evaluate(
        &self,
        request_id: u64,
        editor_revision: u64,
        source: Arc<str>,
        mini: bool,
        launch: Launch,
        rewind: bool,
    ) -> Result<(), EvaluationSendError> {
        self.send_evaluate(
            request_id,
            editor_revision,
            source,
            mini,
            launch,
            rewind,
            false,
        )
    }

    /// [`Self::try_evaluate`] for a snippet previewed under the set: see
    /// [`StudioEngine::mark_next_install_preview`].
    pub fn try_evaluate_snippet_preview(
        &self,
        request_id: u64,
        editor_revision: u64,
        source: Arc<str>,
        mini: bool,
        launch: Launch,
        rewind: bool,
    ) -> Result<(), EvaluationSendError> {
        self.send_evaluate(
            request_id,
            editor_revision,
            source,
            mini,
            launch,
            rewind,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn send_evaluate(
        &self,
        request_id: u64,
        editor_revision: u64,
        source: Arc<str>,
        mini: bool,
        launch: Launch,
        rewind: bool,
        preview: bool,
    ) -> Result<(), EvaluationSendError> {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return Err(EvaluationSendError::Disconnected(source));
        }
        let command_epoch = self.command_epoch.load(Ordering::Acquire);
        match self.commands.try_send(EngineCommand::Evaluate {
            request_id,
            editor_revision,
            command_epoch,
            source,
            mini,
            launch,
            rewind,
            preview,
        }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(EngineCommand::Evaluate { source, .. })) => {
                Err(EvaluationSendError::Full(source))
            }
            Err(TrySendError::Disconnected(EngineCommand::Evaluate { source, .. })) => {
                Err(EvaluationSendError::Disconnected(source))
            }
            Err(_) => unreachable!("send_evaluate sent only Evaluate"),
        }
    }

    /// Queue one setup evaluation, the same way an evaluation is queued:
    /// never waiting behind the engine, and handing the source back when the
    /// two-slot queue is full so the app can keep it at the front of its own.
    pub fn try_evaluate_prebake(
        &self,
        request_id: u64,
        scope: PrebakeScope,
        source: Arc<str>,
    ) -> Result<(), EvaluationSendError> {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return Err(EvaluationSendError::Disconnected(source));
        }
        // Its own match: `try_evaluate` ends in an `unreachable!` keyed to
        // its own variant, which a second sender funnelling through it would
        // turn into a panic on the interface thread.
        match self.commands.try_send(EngineCommand::EvaluatePrebake {
            request_id,
            scope,
            source,
        }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(EngineCommand::EvaluatePrebake { source, .. })) => {
                Err(EvaluationSendError::Full(source))
            }
            Err(TrySendError::Disconnected(EngineCommand::EvaluatePrebake { source, .. })) => {
                Err(EvaluationSendError::Disconnected(source))
            }
            Err(_) => unreachable!("try_evaluate_prebake sent only EvaluatePrebake"),
        }
    }

    /// Immediate cancellation plus a best-effort ownership command. The
    /// atomic stop reaches a QuickJS evaluation even if the command queue is
    /// currently full.
    pub fn request_stop(&self) {
        // Invalidate every Evaluate already in flight or queued. A later
        // user-requested Evaluate observes the new epoch and may restart.
        self.command_epoch.fetch_add(1, Ordering::AcqRel);
        self.stop.request_stop();
        let _ = self.commands.try_send(EngineCommand::Stop);
    }

    /// Cut a graceful drain short. The atomic is the authoritative request:
    /// `StopImmediate` merely wakes the worker and may be dropped when its
    /// bounded queue is full.
    pub fn force_stop(&self) {
        self.command_epoch.fetch_add(1, Ordering::AcqRel);
        self.stop.request_stop();
        self.immediate_stop_requested.store(true, Ordering::Release);
        let _ = self.commands.try_send(EngineCommand::StopImmediate);
    }

    /// Whether the transport has been told to stop. The flag the audio
    /// callback reads, so this is true from the moment the sound is over
    /// rather than from when the engine gets round to saying so.
    pub fn stop_requested(&self) -> bool {
        self.stop.is_stopped()
    }

    /// The engine's sample library, shared so sound names can be checked
    /// on the interface side before they are asked for.
    pub fn library(&self) -> Option<Arc<rustel_runtime::samples::SampleLibrary>> {
        self.library.clone()
    }

    pub(super) fn catalogue(&self) -> Arc<super::catalogue::Snapshot> {
        self.catalogue.snapshot()
    }

    pub(super) fn refresh_catalogue(&self) {
        self.catalogue.refresh();
    }

    pub(super) fn invalidate_catalogue(&self) {
        self.catalogue.invalidate();
    }

    pub(super) fn poll_catalogue(&self) -> bool {
        self.catalogue.poll()
    }

    /// A library of the test's own, so what a scan starts can be checked
    /// without the network.
    #[cfg(test)]
    pub(super) fn set_library(&mut self, library: Arc<rustel_runtime::samples::SampleLibrary>) {
        self.catalogue =
            super::catalogue::Catalogue::new(Some(&library)).expect("catalogue worker");
        self.library = Some(library);
    }

    /// The master fader and meter, shared with the engine.
    pub fn master(&self) -> &Arc<StudioMasterBus> {
        &self.master
    }

    /// Ask the engine to play through a named output. Dropped when the
    /// command queue is full; the UI reports the refusal rather than waiting.
    pub fn try_set_output(&self, name: &str) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::SetOutput(name.to_owned()))
                .is_ok()
    }

    /// Push a slider value to the engine. False when the command queue is
    /// full; the app keeps the newest value and retries on its next turn, so
    /// a fast drag coalesces instead of queueing every intermediate step.
    pub fn try_set_slider(&self, id: &str, value: f64) -> bool {
        self.try_set_slider_with_transition(id, value, false)
    }

    /// Ramp proven continuous audio controls while committing the exact target.
    pub fn try_set_slider_smoothed(&self, id: &str, value: f64) -> bool {
        self.try_set_slider_with_transition(id, value, true)
    }

    fn try_set_slider_with_transition(&self, id: &str, value: f64, smooth: bool) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::SetSlider {
                    id: id.to_owned(),
                    value,
                    smooth,
                })
                .is_ok()
    }

    /// Preview a sound by name (`bd:2`). False when the command queue is
    /// full; a preview is a gesture, not a state, so it is simply dropped.
    /// Ask the engine to preview a snippet. Non-blocking like every other
    /// command here: a dropped preview is a frame of the old picture, which is
    /// not worth waiting on the engine for.
    /// Tell the renderer how large a frame the terminal will actually read.
    /// Dropped when the queue is full: the next resize says it again.
    #[cfg(feature = "hydra")]
    pub fn try_set_hydra_frame_size(
        &self,
        wanted: rustel_runtime::hydra::HydraFrameRequest,
    ) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::HydraFrameSize(wanted))
                .is_ok()
    }

    #[cfg(feature = "hydra")]
    pub fn try_preview_snippet(&self, code: Option<String>) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::HydraPreview(code))
                .is_ok()
    }

    #[cfg(feature = "hydra")]
    pub fn try_set_theme_sketch(&self, code: Option<String>, webcam: bool) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::HydraTheme { code, webcam })
                .is_ok()
    }

    pub fn try_set_live_material(&self, material: super::engine::LiveMaterial) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::SetLiveMaterial(material))
                .is_ok()
    }

    pub fn try_set_sample_memory(&self, budget_bytes: usize, idle: Duration) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::SetSampleMemory { budget_bytes, idle })
                .is_ok()
    }

    pub fn try_audition(&self, sound: &str, gain: f32) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::Audition(sound.to_owned(), gain))
                .is_ok()
    }

    /// Preview a chord: the notes sound together on `sound`.
    pub fn try_audition_notes(&self, notes: &[f32], sound: &str, gain: f32) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::AuditionNotes(
                    notes.to_vec(),
                    sound.to_owned(),
                    gain,
                ))
                .is_ok()
    }

    /// Play notes one after another, `step_secs` apart.
    pub fn try_audition_run(&self, notes: &[f32], sound: &str, gain: f32, step_secs: f64) -> bool {
        !notes.is_empty()
            && self
                .commands
                .try_send(EngineCommand::AuditionRun(
                    notes.to_vec(),
                    sound.to_owned(),
                    gain,
                    step_secs,
                ))
                .is_ok()
    }

    pub fn try_stop_audition(&self) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self.commands.try_send(EngineCommand::StopAudition).is_ok()
    }

    pub fn try_configure_piano(
        &self,
        sound: super::PianoSound,
        volume: u16,
        prepare: bool,
    ) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::ConfigurePiano {
                    sound,
                    volume,
                    prepare,
                })
                .is_ok()
    }

    pub fn try_piano_note_on(&self, key: u8, note: u8, velocity: u8) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::PianoNoteOn {
                    key,
                    note,
                    velocity,
                })
                .is_ok()
    }

    pub fn try_piano_note_off(&self, key: u8) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::PianoNoteOff(key))
                .is_ok()
    }

    pub fn try_stop_piano(&self) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self.commands.try_send(EngineCommand::StopPiano).is_ok()
    }

    /// Silence everything sounding. Used when one audition replaces
    /// another, where letting the first ring out is the bug.
    pub fn try_cut_sounding(&self) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self.commands.try_send(EngineCommand::CutSounding).is_ok()
    }

    /// Tell the engine which notes are bound to scene launch, so those
    /// presses stay transport buttons: out of the musical keys ring and
    /// out of the press count that arms the at-once requery. False when
    /// the queue is full; the next change re-sends the whole set.
    pub fn try_set_launch_pads(&self, pads: Vec<(u8, u8)>) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::SetLaunchPads(pads))
                .is_ok()
    }

    /// Ask the engine to fetch a `samples("…")` the score names, so its
    /// banks are there before the score is evaluated. False when the queue
    /// is full; the checker asks again when it next runs.
    pub fn try_look_up_samples(&self, spec: &str) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::LookUpSamples(spec.to_owned()))
                .is_ok()
    }

    pub fn try_set_input(&self, name: Option<String>) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::SetInput(name))
                .is_ok()
    }

    /// The mixer's orbit fader, linear.
    pub fn try_set_orbit_gain(&self, orbit: u8, gain: f32) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::SetOrbitGain { orbit, gain })
                .is_ok()
    }

    /// The mixer's input fader, linear.
    pub fn try_set_input_gain(&self, gain: f32) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::SetInputGain(gain))
                .is_ok()
    }

    /// The output-latency knob, in frames. False when the
    /// queue is full; the app keeps the newest value and retries, as it
    /// does a slider.
    pub fn try_set_output_buffer_frames(&self, frames: Option<u32>) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::SetOutputBufferFrames(frames))
                .is_ok()
    }

    pub fn try_clock_out(&self, port: Option<String>) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::ClockOut(port))
                .is_ok()
    }

    pub fn try_clock_in(&self, port: Option<String>) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self.commands.try_send(EngineCommand::ClockIn(port)).is_ok()
    }

    /// Say which MIDI ports may be opened. False when the command queue is
    /// full, so the caller keeps its copy and asks again next frame.
    pub fn try_midi_enablement(&self, enabled: super::devices::MidiEnablement) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::MidiEnablement(enabled))
                .is_ok()
    }

    /// Send an orbit to an output pair (0 is the main pair). False when
    /// the command queue is full.
    pub fn try_route_orbit(&self, orbit: u8, pair: u8) -> bool {
        !self.shutdown_requested.load(Ordering::Acquire)
            && self
                .commands
                .try_send(EngineCommand::RouteOrbit { orbit, pair })
                .is_ok()
    }

    /// Start a take (`Some(path)`) or close the current one (`None`).
    /// The returned request ID belongs to one structural recording reply.
    /// None means the command was not queued. A successful start's request ID
    /// also identifies that capture, even when its path is reused later.
    pub fn try_record(&self, path: Option<std::path::PathBuf>) -> Option<u64> {
        try_send_recording(
            &self.commands,
            &self.next_recording_request,
            &self.shutdown_requested,
            path,
        )
    }

    /// Start recording a sample from the input at `path`, or with `None`
    /// finish the one in progress. The request id comes back on the
    /// answer; `None` when the engine could not take the command.
    pub fn try_record_sample(&self, path: Option<std::path::PathBuf>) -> Option<u64> {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return None;
        }
        let request_id = self
            .next_recording_request
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .ok()?;
        self.commands
            .try_send(EngineCommand::RecordSample { request_id, path })
            .ok()?;
        Some(request_id)
    }

    pub fn try_recv_control(&self) -> Result<StudioControlEvent, TryRecvError> {
        self.controls.try_recv()
    }

    pub fn try_recv_trace(&self) -> Result<UiTraceBatchRequest, TryRecvError> {
        self.traces.try_recv()
    }

    pub fn take_latest_audio(&self) -> Option<(UiAudioMetadata, UiAudioAnalysisSet)> {
        self.latest_audio
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    /// Remove visualization traffic produced before a delivered Stop. The
    /// worker emits no new frames after that acknowledgement, so this closes
    /// the cross-channel ordering gap without blocking either side.
    pub fn discard_visual_updates(&self) {
        while self.traces.try_recv().is_ok() {}
        if let Ok(mut slot) = self.latest_audio.lock() {
            *slot = None;
        }
    }

    pub fn shutdown(&mut self) {
        self.shutdown_requested.store(true, Ordering::Release);
        self.command_epoch.fetch_add(1, Ordering::AcqRel);
        self.stop.request_stop();
        // The atomic is authoritative when the bounded queue is full; this
        // command only wakes an idle receiver promptly.
        let _ = self.commands.try_send(EngineCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn try_send_recording(
    commands: &SyncSender<EngineCommand>,
    next_request: &AtomicU64,
    shutdown_requested: &AtomicBool,
    path: Option<std::path::PathBuf>,
) -> Option<u64> {
    if shutdown_requested.load(Ordering::Acquire) {
        return None;
    }
    let request_id = next_request
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .ok()?;
    commands
        .try_send(EngineCommand::Record { request_id, path })
        .ok()?;
    Some(request_id)
}

impl Drop for StudioWorker {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.shutdown();
        }
    }
}

// Seven channels and shared handles plus, in a visuals build, the window
// bridge. Bundling them into a struct would move the same list one line up.
#[allow(clippy::too_many_arguments)]
fn run_engine(
    mut engine: StudioEngine,
    commands: Receiver<EngineCommand>,
    controls: SyncSender<StudioControlEvent>,
    traces: SyncSender<UiTraceBatchRequest>,
    latest_audio: Arc<Mutex<Option<(UiAudioMetadata, UiAudioAnalysisSet)>>>,
    command_epoch: Arc<AtomicU64>,
    shutdown_requested: Arc<AtomicBool>,
    immediate_stop_requested: Arc<AtomicBool>,
    // Owned by this thread for the life of the studio, so the window closes
    // when the engine stops - which is after the terminal has been given back.
    #[cfg(feature = "hydra")] mut hydra: rustel_runtime::hydra::HydraBridge,
) {
    let poll_interval = engine.poll_interval();
    let mut pending_control = VecDeque::new();
    let mut next_snapshot = Instant::now();
    let mut shutdown = false;
    // The evaluate that is waiting for its cycle line, answered when the
    // engine fires or drops it.
    let mut armed: Option<ArmedRequest> = None;
    let mut recording = None;

    while !shutdown && !shutdown_requested.load(Ordering::Acquire) {
        flush_pending_control(&controls, &mut pending_control);

        let timeout = if engine.is_playing() {
            poll_interval
        } else {
            IDLE_POLL
        };
        // Keep one slot reserved for a terminal engine Stop/failure. If the UI
        // is not draining structural outcomes, stop consuming new commands;
        // the bounded command channel then applies backpressure instead of
        // allowing the private deque to grow without limit.
        if can_receive_command(&pending_control) {
            match commands.recv_timeout(timeout) {
                Ok(command) => {
                    shutdown = process_command(
                        &mut engine,
                        command,
                        &command_epoch,
                        &shutdown_requested,
                        &mut pending_control,
                        &mut armed,
                        &mut recording,
                    );
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => shutdown = true,
            }
        } else {
            thread::sleep(timeout);
        }

        if shutdown || shutdown_requested.load(Ordering::Acquire) {
            break;
        }

        // The second Stop press must win even if its wake-up command could
        // not enter the bounded queue. If the command handled it already,
        // its branch cleared this flag and this is a no-op.
        force_stop_if_requested(&mut engine, &immediate_stop_requested, &mut pending_control);

        // Evaluation outcomes must precede any layout emitted by the first
        // producer tick for that generation.
        flush_pending_control(&controls, &mut pending_control);

        // Visuals are driven whether or not anything is sounding: a score can
        // open a window and then be stopped, and the picture should keep its
        // last frame rather than freeze halfway through a save.
        #[cfg(feature = "hydra")]
        {
            let audio = latest_audio
                .lock()
                .ok()
                .and_then(|held| held.as_ref().map(|(_, set)| set.master.clone()));
            if let Some(diagnostic) = engine.drive_hydra(&mut hydra, audio.as_ref()) {
                push_optional_diagnostic(&mut pending_control, diagnostic);
            }
        }

        if engine.is_playing() {
            let tick =
                engine.tick(|update| route_update(update, &controls, &traces, &latest_audio));
            answer_launch(&mut engine, &mut armed, &mut pending_control);
            match tick {
                Ok(StudioTick::Stopped(stop)) => {
                    push_control(&mut pending_control, StudioControlEvent::Stopped(stop))
                }
                Ok(_) => {}
                Err(error) => push_control(
                    &mut pending_control,
                    StudioControlEvent::EngineFailure(EngineFailure {
                        playback_stopped: !engine.is_playing(),
                        ..EngineFailure::runtime(&error, true)
                    }),
                ),
            }
        } else {
            // Stopped, the devices still talk: a pad plugged in now is
            // news now, not at the next evaluation.
            engine.idle_turn(|update| route_update(update, &controls, &traces, &latest_audio));
        }

        // Ticks and launch replies can consume control capacity. Reserve a
        // slot only now, and collect while idle too: audio Stop does not
        // abandon a take whose file is still closing.
        collect_recording(&mut engine, &mut recording, &mut pending_control);

        if Instant::now() >= next_snapshot {
            next_snapshot = Instant::now() + SNAPSHOT_INTERVAL;
            // Skip rather than defer a replaceable snapshot behind structural
            // replies. A slot may have opened since the last failed flush.
            try_send_snapshot(&controls, &pending_control, || engine.snapshot());
        }
    }

    // Close audio before the recording fields' blocking cleanup. The UI
    // may no longer drain controls, so shutdown promises no final delivery.
    let stop_timeout = engine.stop_timeout();
    if let Some(stop) = engine.stop(stop_timeout) {
        let _ = controls.try_send(StudioControlEvent::Stopped(Box::new(stop)));
    }
    flush_pending_control(&controls, &mut pending_control);
}

fn can_receive_command(pending: &VecDeque<StudioControlEvent>) -> bool {
    pending.len() < MAX_PENDING_CONTROL.saturating_sub(1)
}

fn cut_playback(engine: &mut StudioEngine, pending: &mut VecDeque<StudioControlEvent>) {
    let stop_timeout = engine.stop_timeout();
    if let Some(stop) = engine.stop(stop_timeout) {
        push_control(pending, StudioControlEvent::Stopped(Box::new(stop)));
    }
}

fn force_stop_if_requested(
    engine: &mut StudioEngine,
    requested: &AtomicBool,
    pending: &mut VecDeque<StudioControlEvent>,
) {
    if requested.swap(false, Ordering::AcqRel) {
        cut_playback(engine, pending);
    }
}

#[cfg(any(feature = "hydra", test))]
fn push_optional_diagnostic(
    pending: &mut VecDeque<StudioControlEvent>,
    diagnostic: StudioDiagnostic,
) {
    // Visuals still advance under backpressure; only this notice is skipped.
    // Do not consume the terminal reserve or evict an existing failure.
    if can_receive_command(pending) {
        push_control(pending, StudioControlEvent::Diagnostic(diagnostic));
    }
}

fn try_send_snapshot(
    sender: &SyncSender<StudioControlEvent>,
    pending: &VecDeque<StudioControlEvent>,
    snapshot: impl FnOnce() -> StudioSnapshot,
) {
    if pending.is_empty() {
        let _ = sender.try_send(StudioControlEvent::Snapshot(Box::new(snapshot())));
    }
}

/// An evaluate the engine is holding until its cycle line.
struct ArmedRequest {
    request_id: u64,
    editor_revision: u64,
}

struct RecordingCapture {
    capture_id: u64,
    stop_request: Option<u64>,
}

fn collect_recording(
    engine: &mut StudioEngine,
    recording: &mut Option<RecordingCapture>,
    pending: &mut VecDeque<StudioControlEvent>,
) {
    if !can_receive_command(pending) {
        return;
    }
    let Some(status) = engine.try_finish_recording() else {
        return;
    };
    let capture = recording.take().expect("recording started by this worker");
    push_control(
        pending,
        StudioControlEvent::Recording(RecordingOutcome {
            request_id: capture.stop_request,
            result: Ok(RecordingReply::Finished {
                capture_id: capture.capture_id,
                status,
            }),
        }),
    );
}

fn launch_failure(error: RuntimeError) -> EngineFailure {
    if matches!(error, RuntimeError::Cancelled) {
        return EngineFailure {
            kind: "cancelled".into(),
            message: "launch was cancelled before its cycle line".into(),
            recoverable: true,
            playback_stopped: false,
        };
    }
    EngineFailure::runtime(&error, true)
}

/// Answer the armed request with what became of its launch, if anything
/// has. Called wherever the engine may have settled one: after a tick, and
/// after a stop that cancelled it.
fn answer_launch(
    engine: &mut StudioEngine,
    armed: &mut Option<ArmedRequest>,
    pending: &mut VecDeque<StudioControlEvent>,
) {
    if let Some(result) = engine.take_launch_outcome()
        && let Some(request) = armed.take()
    {
        push_control(
            pending,
            StudioControlEvent::Evaluation(EvaluationOutcome {
                request_id: request.request_id,
                editor_revision: request.editor_revision,
                result: result.map_err(launch_failure),
            }),
        );
    }
}

/// An output change a command asked for: another device, or another
/// buffer size on the same one. Both recycle the stream, both can lose it
/// and stop the set, and both are answered by [`answer_output_change`] -
/// only the words differ.
#[derive(Clone, Copy, Debug)]
enum OutputChange<'a> {
    /// A switch to the named output.
    Device(&'a str),
    /// A new output buffer size.
    Latency,
}

impl OutputChange<'_> {
    /// The terminal failure's kind and message when the change stopped the
    /// set.
    fn stopped(self, result: Result<(), RuntimeError>) -> (String, String) {
        match (self, result) {
            (Self::Device(name), Ok(())) => (
                "audio".to_owned(),
                format!("audio stopped while switching to {name}"),
            ),
            (Self::Device(name), Err(error)) => (
                error.kind().to_owned(),
                format!("could not switch to {name}: {error}"),
            ),
            (Self::Latency, Ok(())) => (
                "audio".to_owned(),
                "audio stopped while changing output latency".to_owned(),
            ),
            (Self::Latency, Err(error)) => (
                error.kind().to_owned(),
                format!("audio stopped while changing output latency: {error}"),
            ),
        }
    }

    /// What is said when the set did not stop: playback preserved, or there
    /// was none to lose.
    fn kept(self, result: Result<(), RuntimeError>) -> Option<StudioDiagnostic> {
        match (self, result) {
            // A selection that worked is not a warning. A failed switch
            // still reports as one.
            (Self::Device(name), Ok(())) => Some(StudioDiagnostic::info(
                "audio-device",
                format!("selected output {name}"),
            )),
            (Self::Device(name), Err(error)) => Some(StudioDiagnostic::message(
                "audio-device",
                format!("could not switch to {name}: {error}"),
            )),
            // The row already shows the size it asked for.
            (Self::Latency, Ok(())) => None,
            (Self::Latency, Err(error)) => Some(StudioDiagnostic::message(
                "ui-control",
                format!("output latency was refused: {error}"),
            )),
        }
    }
}

/// Answer an output change once the engine has tried it. A change that
/// stopped the set answers the launch it cancelled, because no later tick
/// will. It also tells the surface that playback ended, so the surface does
/// not show EVALUATING over silence. Any other result is a diagnostic, or
/// nothing.
fn answer_output_change(
    engine: &mut StudioEngine,
    change: OutputChange<'_>,
    was_playing: bool,
    result: Result<(), RuntimeError>,
    armed: &mut Option<ArmedRequest>,
    pending: &mut VecDeque<StudioControlEvent>,
) {
    if was_playing && !engine.is_playing() {
        let (kind, message) = change.stopped(result);
        // No subsequent tick will answer a launch cancelled by output loss.
        answer_launch(engine, armed, pending);
        push_control(
            pending,
            StudioControlEvent::EngineFailure(EngineFailure {
                kind,
                message,
                recoverable: true,
                playback_stopped: true,
            }),
        );
        return;
    }
    if let Some(diagnostic) = change.kept(result) {
        push_control(pending, StudioControlEvent::Diagnostic(diagnostic));
    }
}

fn process_command(
    engine: &mut StudioEngine,
    command: EngineCommand,
    current_epoch: &AtomicU64,
    shutdown_requested: &AtomicBool,
    pending: &mut VecDeque<StudioControlEvent>,
    armed: &mut Option<ArmedRequest>,
    recording: &mut Option<RecordingCapture>,
) -> bool {
    match command {
        EngineCommand::Evaluate {
            request_id,
            editor_revision,
            command_epoch,
            source,
            mini,
            launch,
            rewind,
            preview,
        } => {
            let cancelled = || {
                shutdown_requested.load(Ordering::Acquire)
                    || command_epoch != current_epoch.load(Ordering::Acquire)
            };
            // A launch inside its head-room is due at the next tick;
            // whether that tick or this command runs first is thread
            // timing. It fires first and keeps its line: a repeat press is
            // answered with it, and a newer quantised launch takes the
            // next line.
            if matches!(launch, Launch::Quantised { .. }) && !cancelled() {
                engine.fire_due_launch();
                answer_launch(engine, armed, pending);
            }
            // A newer launch, or an edit played now, answers the one that
            // was still waiting.
            if let Some(previous) = armed.take() {
                // Only the launch still waiting: one that already fired
                // was answered (so it is not `armed`) and owns its line.
                // Dropping its landing here made an A-B-A pad roll restart
                // A from zero a second time one line later.
                engine.cancel_pending_launch();
                let _ = engine.take_launch_outcome();
                // The log's trace of a supersede: it appears whenever an
                // armed launch is replaced, by a pad race or an edit. The
                // request's own cancelled outcome tells the surface; this
                // line is for reading the set back, so it stays off the
                // status line.
                push_control(
                    pending,
                    StudioControlEvent::Diagnostic(StudioDiagnostic::note(
                        "launch",
                        "a second launch superseded the waiting one",
                    )),
                );
                push_control(
                    pending,
                    StudioControlEvent::Evaluation(EvaluationOutcome {
                        request_id: previous.request_id,
                        editor_revision: previous.editor_revision,
                        result: Err(EngineFailure {
                            kind: "cancelled".into(),
                            message: "launch was superseded".into(),
                            recoverable: true,
                            playback_stopped: false,
                        }),
                    }),
                );
            }
            // Said before the arm or the evaluation and taken by the
            // install, as a rewind is.
            engine.mark_next_install_preview(preview);
            if let Launch::Quantised { unit_cycles } = launch
                && !cancelled()
            {
                match engine.arm_launch(&source, mini, unit_cycles, rewind) {
                    Ok(Some(info)) => {
                        // A repeat press answered with the launch already in
                        // flight waits for nothing, so it announces no
                        // countdown.
                        let line = match engine.answered_repeat_generation() {
                            Some(generation) => format!(
                                "repeat press answered - generation {generation} already playing"
                            ),
                            None => format!(
                                "waiting for cycle {:.0} - {:.1}s",
                                info.boundary_cycle, info.seconds_left
                            ),
                        };
                        push_control(
                            pending,
                            StudioControlEvent::Diagnostic(StudioDiagnostic::note("launch", line)),
                        );
                        *armed = Some(ArmedRequest {
                            request_id,
                            editor_revision,
                        });
                        return false;
                    }
                    Ok(None) => {
                        push_control(
                            pending,
                            StudioControlEvent::Diagnostic(StudioDiagnostic::note(
                                "launch",
                                "nothing playing - launched at once",
                            )),
                        );
                    }
                    Err(error) => {
                        push_control(
                            pending,
                            StudioControlEvent::Evaluation(EvaluationOutcome {
                                request_id,
                                editor_revision,
                                result: Err(launch_failure(error)),
                            }),
                        );
                        return false;
                    }
                }
            }
            // In wait mode an edit naming sounds still loading keeps the
            // last score playing, and is answered when it lands.
            if !cancelled() && engine.hold_update(&source, mini, rewind) {
                *armed = Some(ArmedRequest {
                    request_id,
                    editor_revision,
                });
                return false;
            }
            let result = if !cancelled() {
                // Asked for before the evaluation and consumed by the
                // install inside it: a score that fails to evaluate
                // installs nothing, so there is nothing to rewind, and
                // the flag is dropped by the engine with the rest of it.
                if rewind {
                    engine.start_next_from_zero();
                }
                engine
                    .evaluate_guarded(&source, mini, cancelled)
                    .map_err(|error| EngineFailure::runtime(&error, true))
            } else {
                Err(EngineFailure {
                    kind: "cancelled".into(),
                    message: "evaluation was superseded by Stop".into(),
                    recoverable: true,
                    playback_stopped: false,
                })
            };
            push_control(
                pending,
                StudioControlEvent::Evaluation(EvaluationOutcome {
                    request_id,
                    editor_revision,
                    result,
                }),
            );
            false
        }
        EngineCommand::EvaluatePrebake {
            request_id,
            scope,
            source,
        } => {
            // Shutdown is the only thing that cancels a setup: a Stop is
            // about the transport, and an interrupted setup would leave the
            // heap half-mutated with nothing able to say how.
            let result = if shutdown_requested.load(Ordering::Acquire) {
                Err(EngineFailure {
                    kind: "cancelled".into(),
                    message: "the studio was closing".into(),
                    recoverable: true,
                    playback_stopped: false,
                })
            } else {
                engine
                    .evaluate_prebake_guarded(&source, shutdown_requested)
                    .map_err(|error| EngineFailure::runtime(&error, true))
            };
            push_control(
                pending,
                StudioControlEvent::Prebake(PrebakeOutcome {
                    request_id,
                    scope,
                    result,
                }),
            );
            false
        }
        EngineCommand::Stop => {
            if engine.is_playing() {
                // Graceful: the tick loop hushes new onsets, lets the tail
                // ring out, and reports `Stopped` when the device closes -
                // and the launch it cancels is answered on that next tick.
                // This command also wakes the worker after the UI has set the
                // same atomic stop flag, so it must stay idempotent while the
                // drain is already under way.
                engine.request_stop();
            } else {
                // No score can still mean a piano output carrying its tail.
                // Only report idle when there really is no callback to stop.
                let stop = engine
                    .stop(engine.stop_timeout())
                    .unwrap_or_else(StudioStop::idle);
                push_control(pending, StudioControlEvent::Stopped(Box::new(stop)));
            }
            false
        }
        EngineCommand::StopImmediate => {
            // The loop clears the fallback atomic immediately after command
            // processing. If that path got here first, the consumed output
            // makes a later stale wake-up harmless, with no duplicate answer.
            cut_playback(engine, pending);
            false
        }
        EngineCommand::SetOutput(name) => {
            let was_playing = engine.is_playing();
            let result = engine.set_output_device(&name);
            answer_output_change(
                engine,
                OutputChange::Device(&name),
                was_playing,
                result,
                armed,
                pending,
            );
            false
        }
        EngineCommand::SetOutputBufferFrames(frames) => {
            let was_playing = engine.is_playing();
            let result = engine.set_output_buffer_frames(frames);
            answer_output_change(
                engine,
                OutputChange::Latency,
                was_playing,
                result,
                armed,
                pending,
            );
            false
        }
        EngineCommand::SetSlider { id, value, smooth } => {
            if let Err(error) = engine.set_slider_with_transition(&id, value, smooth) {
                push_control(
                    pending,
                    StudioControlEvent::Diagnostic(StudioDiagnostic::message(
                        "ui-control",
                        format!("slider was refused: {error}"),
                    )),
                );
            }
            false
        }
        #[cfg(feature = "hydra")]
        EngineCommand::HydraPreview(code) => {
            engine.set_hydra_preview(code);
            false
        }
        #[cfg(feature = "hydra")]
        EngineCommand::HydraTheme { code, webcam } => {
            engine.set_hydra_theme(code, webcam);
            false
        }
        #[cfg(feature = "hydra")]
        EngineCommand::HydraFrameSize(wanted) => {
            engine.set_hydra_frame_size(wanted);
            false
        }
        EngineCommand::Audition(sound, gain) => {
            if let Err(error) = engine.audition(&sound, gain) {
                push_control(
                    pending,
                    StudioControlEvent::Diagnostic(StudioDiagnostic::message(
                        "preview",
                        format!("could not preview {sound}: {error}"),
                    )),
                );
            }
            false
        }
        EngineCommand::AuditionRun(notes, sound, gain, step_secs) => {
            if let Err(error) = engine.audition_run(&notes, &sound, gain, step_secs) {
                push_control(
                    pending,
                    StudioControlEvent::Diagnostic(StudioDiagnostic::message(
                        "preview",
                        format!("could not preview the run on {sound}: {error}"),
                    )),
                );
            }
            false
        }
        EngineCommand::AuditionNotes(notes, sound, gain) => {
            if let Err(error) = engine.audition_notes(&notes, &sound, gain) {
                push_control(
                    pending,
                    StudioControlEvent::Diagnostic(StudioDiagnostic::message(
                        "preview",
                        format!("could not preview the chord on {sound}: {error}"),
                    )),
                );
            }
            false
        }
        EngineCommand::ConfigurePiano {
            sound,
            volume,
            prepare,
        } => {
            engine.set_piano_settings(sound, volume);
            if prepare && let Err(error) = engine.prepare_piano() {
                push_control(
                    pending,
                    StudioControlEvent::Diagnostic(StudioDiagnostic::message(
                        "piano",
                        error.to_string(),
                    )),
                );
            }
            false
        }
        EngineCommand::PianoNoteOn {
            key,
            note,
            velocity,
        } => {
            if let Err(error) = engine.piano_note_on(key, note, velocity) {
                push_control(
                    pending,
                    StudioControlEvent::Diagnostic(StudioDiagnostic::message(
                        "piano",
                        error.to_string(),
                    )),
                );
            }
            false
        }
        EngineCommand::PianoNoteOff(key) => {
            engine.piano_note_off(key);
            false
        }
        EngineCommand::StopPiano => {
            engine.stop_piano();
            false
        }
        EngineCommand::StopAudition => {
            engine.stop_audition();
            false
        }
        EngineCommand::CutSounding => {
            engine.cut_sounding();
            false
        }
        EngineCommand::SetSampleMemory { budget_bytes, idle } => {
            engine.set_sample_memory_policy(budget_bytes, idle);
            false
        }
        EngineCommand::SetLiveMaterial(material) => {
            engine.set_live_material(&material);
            false
        }
        EngineCommand::LookUpSamples(spec) => {
            // A refusal is recorded against the spec for the checker to
            // show where it is written; nothing to say here.
            let _ = engine.look_up_samples(&spec);
            false
        }
        EngineCommand::Record { request_id, path } => {
            let result = if let Some(path) = path {
                Some(
                    engine
                        .start_recording(path)
                        .map(|()| {
                            *recording = Some(RecordingCapture {
                                capture_id: request_id,
                                stop_request: None,
                            });
                            RecordingReply::Started {
                                capture_id: request_id,
                            }
                        })
                        .map_err(|error| EngineFailure::runtime(&error, true)),
                )
            } else if recording
                .as_ref()
                .is_some_and(|capture| capture.stop_request.is_some())
            {
                Some(Err(EngineFailure {
                    kind: "record".into(),
                    message: "the take is already closing".into(),
                    recoverable: true,
                    playback_stopped: false,
                }))
            } else if engine.request_recording_close() {
                recording
                    .as_mut()
                    .expect("recording started by this worker")
                    .stop_request = Some(request_id);
                None
            } else {
                debug_assert!(recording.is_none());
                Some(Ok(RecordingReply::NoActiveTake))
            };
            if let Some(result) = result {
                push_control(
                    pending,
                    StudioControlEvent::Recording(RecordingOutcome {
                        request_id: Some(request_id),
                        result,
                    }),
                );
            }
            false
        }
        EngineCommand::RecordSample { request_id, path } => {
            let result = match path {
                Some(path) => engine
                    .start_sample_recording(path)
                    .map(|()| SampleReply::Started)
                    .map_err(|error| EngineFailure::runtime(&error, true)),
                None => Ok(engine
                    .stop_sample_recording()
                    .map_or(SampleReply::NotRecording, SampleReply::Finished)),
            };
            push_control(
                pending,
                StudioControlEvent::SampleRecording(SampleRecordingOutcome { request_id, result }),
            );
            false
        }
        EngineCommand::SetInput(name) => {
            engine.set_input_device(name.as_deref());
            false
        }
        EngineCommand::SetLaunchPads(pads) => {
            engine.set_launch_pads(pads);
            false
        }
        EngineCommand::SetOrbitGain { orbit, gain } => {
            engine.set_orbit_gain(usize::from(orbit), gain);
            false
        }
        EngineCommand::SetInputGain(gain) => {
            engine.set_input_gain(gain);
            false
        }
        EngineCommand::RouteOrbit { orbit, pair } => {
            engine.route_orbit(usize::from(orbit), pair);
            false
        }
        EngineCommand::MidiEnablement(enabled) => {
            engine.set_midi_enablement(enabled);
            false
        }
        EngineCommand::ClockOut(port) => {
            if let Err(error) = engine.set_clock_out(port.as_deref()) {
                push_control(
                    pending,
                    StudioControlEvent::Diagnostic(StudioDiagnostic::message(
                        "clock",
                        format!("clock out: {error}"),
                    )),
                );
            }
            false
        }
        EngineCommand::ClockIn(port) => {
            if let Err(error) = engine.set_clock_in(port.as_deref()) {
                push_control(
                    pending,
                    StudioControlEvent::Diagnostic(StudioDiagnostic::message(
                        "clock",
                        format!("clock in: {error}"),
                    )),
                );
            }
            false
        }
        EngineCommand::Shutdown => true,
        #[cfg(test)]
        EngineCommand::InjectPanic(point) => {
            engine.inject_panic_for_test(point);
            false
        }
    }
}

fn route_update(
    update: StudioUpdate,
    controls: &SyncSender<StudioControlEvent>,
    traces: &SyncSender<UiTraceBatchRequest>,
    latest_audio: &Mutex<Option<(UiAudioMetadata, UiAudioAnalysisSet)>>,
) -> StudioUpdateSendResult {
    match update {
        StudioUpdate::Audio { metadata, analysis } => {
            if let Ok(mut slot) = latest_audio.try_lock() {
                *slot = Some((metadata, *analysis));
            }
            Ok(())
        }
        StudioUpdate::Traces(request) => match traces.try_send(request) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(request)) => Err((
                UiEventSendStatus::DroppedFull,
                StudioUpdate::Traces(request),
            )),
            Err(TrySendError::Disconnected(request)) => Err((
                UiEventSendStatus::Disconnected,
                StudioUpdate::Traces(request),
            )),
        },
        StudioUpdate::Layout(layout) => send_structural(
            controls,
            StudioControlEvent::Layout(layout),
            |event| match event {
                StudioControlEvent::Layout(layout) => StudioUpdate::Layout(layout),
                _ => unreachable!(),
            },
        ),
        StudioUpdate::Diagnostic(diagnostic) => send_structural(
            controls,
            StudioControlEvent::Diagnostic(diagnostic),
            |event| match event {
                StudioControlEvent::Diagnostic(diagnostic) => StudioUpdate::Diagnostic(diagnostic),
                _ => unreachable!(),
            },
        ),
    }
}

fn send_structural(
    sender: &SyncSender<StudioControlEvent>,
    event: StudioControlEvent,
    restore: impl FnOnce(StudioControlEvent) -> StudioUpdate,
) -> StudioUpdateSendResult {
    match sender.try_send(event) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(event)) => Err((UiEventSendStatus::DroppedFull, restore(event))),
        Err(TrySendError::Disconnected(event)) => {
            Err((UiEventSendStatus::Disconnected, restore(event)))
        }
    }
}

fn push_control(pending: &mut VecDeque<StudioControlEvent>, event: StudioControlEvent) {
    if let StudioControlEvent::EngineFailure(failure) = &event
        && let Some(existing) = pending.iter_mut().find(|pending| {
            matches!(pending, StudioControlEvent::EngineFailure(current) if current.kind == failure.kind)
        })
    {
        *existing = event;
        return;
    }
    if pending.len() >= MAX_PENDING_CONTROL {
        // Snapshots and repeated engine failures are replaceable. Evaluation
        // and Stop/recording outcomes are not: their callers need an answer.
        if let Some(index) = pending.iter().position(|event| {
            matches!(
                event,
                StudioControlEvent::Snapshot(_) | StudioControlEvent::EngineFailure(_)
            )
        }) {
            pending.remove(index);
        } else if matches!(event, StudioControlEvent::EngineFailure(_)) {
            return;
        }
    }
    pending.push_back(event);
}

fn flush_pending_control(
    sender: &SyncSender<StudioControlEvent>,
    pending: &mut VecDeque<StudioControlEvent>,
) {
    while let Some(event) = pending.pop_front() {
        match sender.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(event)) => {
                pending.push_front(event);
                break;
            }
            Err(TrySendError::Disconnected(_)) => {
                pending.clear();
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::engine::{ClockStatus, StudioDiagnosticLevel};
    use super::*;

    mod panic_recovery {
        use super::*;

        #[test]
        fn engine_thread_survives_query_panic_and_accepts_the_next_edit() {
            let worker = StudioWorker::spawn_engine(
                || Ok(crate::engine::tests::silent_engine_for_output_selection()),
                #[cfg(feature = "hydra")]
                rustel_runtime::hydra::HydraBridge::new(),
            )
            .expect("worker");
            let good = "note('c3').s('sine').fast(8)";
            worker
                .try_evaluate(1, 1, Arc::from(good), false, Launch::Now, false)
                .unwrap();
            let first = wait_for(&worker, |event| {
                matches!(
                    event,
                    StudioControlEvent::Snapshot(snapshot)
                        if snapshot.playing
                            && snapshot.confirmed_audio_generation == Some(snapshot.session_generation)
                            && snapshot.source_revision.as_deref()
                                == Some(rustel_runtime::ui_events::source_revision(good).as_str())
                )
            });
            let StudioControlEvent::Snapshot(first) = first else {
                unreachable!()
            };
            worker
                .commands
                .send(EngineCommand::InjectPanic(
                    rustel_runtime::SessionPanicPoint::Query,
                ))
                .unwrap();
            let error = wait_for(&worker, |event| {
                matches!(
                    event,
                    StudioControlEvent::Diagnostic(diagnostic) if diagnostic.kind == "panic"
                )
            });
            let StudioControlEvent::Diagnostic(error) = error else {
                unreachable!()
            };
            assert!(error.recoverable);
            assert_eq!(error.level, StudioDiagnosticLevel::Error);
            assert!(error.message.contains("injected native Query panic"));
            let restored = wait_for(&worker, |event| {
                matches!(
                    event,
                    StudioControlEvent::Snapshot(snapshot)
                        if snapshot.playing
                            && snapshot.confirmed_audio_generation == Some(snapshot.session_generation)
                            && snapshot.session_generation > first.session_generation
                )
            });
            let StudioControlEvent::Snapshot(restored) = restored else {
                unreachable!()
            };
            assert_eq!(restored.source_revision, first.source_revision);
            assert!(!worker.join.as_ref().unwrap().is_finished());
            worker
                .try_evaluate(
                    2,
                    2,
                    Arc::from("note('d3').s('sine')"),
                    false,
                    Launch::Now,
                    false,
                )
                .unwrap();
            let next = wait_for(
                &worker,
                |event| matches!(event, StudioControlEvent::Evaluation(outcome) if outcome.request_id == 2),
            );
            let StudioControlEvent::Evaluation(next) = next else {
                unreachable!()
            };
            assert!(
                next.result.is_ok(),
                "the recovered worker refused a healthy edit"
            );
        }

        fn wait_for(
            worker: &StudioWorker,
            wanted: impl Fn(&StudioControlEvent) -> bool,
        ) -> StudioControlEvent {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut last_event = None;
            loop {
                if let Ok(event) = worker.try_recv_control() {
                    if let StudioControlEvent::Evaluation(outcome) = &event {
                        assert!(outcome.result.is_ok(), "score failed: {:?}", outcome.result);
                    }
                    if wanted(&event) {
                        return event;
                    }
                    last_event = Some(event);
                }
                assert!(
                    Instant::now() < deadline,
                    "worker did not answer before the deadline; last event: {last_event:?}"
                );
                assert!(
                    !worker.join.as_ref().unwrap().is_finished(),
                    "engine thread exited"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    #[test]
    fn output_selection_errors_do_not_stop_preserved_playback() {
        for message in [
            "discovery refused",
            "replacement refused; previous output reopened; refill pending",
        ] {
            let mut engine = super::super::engine::tests::silent_engine_for_output_selection();
            assert!(engine.arm_launch("~", true, 4.0, false).unwrap().is_some());
            let was_playing = engine.is_playing();
            let error = RuntimeError::Audio(message.to_owned());
            let expected = format!("could not switch to requested-output: {error}");
            let mut pending = VecDeque::new();
            let mut armed = Some(ArmedRequest {
                request_id: 7,
                editor_revision: 9,
            });

            answer_output_change(
                &mut engine,
                OutputChange::Device("requested-output"),
                was_playing,
                Err(error),
                &mut armed,
                &mut pending,
            );

            assert!(engine.is_playing());
            let StudioControlEvent::Diagnostic(diagnostic) = pending.pop_front().unwrap() else {
                panic!("preserved playback needs a warning, not a terminal failure");
            };
            assert_eq!(diagnostic.kind, "audio-device");
            assert_eq!(diagnostic.level, StudioDiagnosticLevel::Warning);
            assert_eq!(diagnostic.message, expected);
            assert!(diagnostic.recoverable);
            assert_eq!(armed.as_ref().unwrap().request_id, 7);
            assert!(engine.take_launch_outcome().is_none());
            assert!(pending.is_empty());
        }
    }

    #[test]
    fn failed_output_recovery_answers_the_cancelled_launch_and_reports_terminal_failure() {
        let mut engine = super::super::engine::tests::silent_engine_for_output_selection();
        assert!(engine.arm_launch("~", true, 4.0, false).unwrap().is_some());
        let mut armed = Some(ArmedRequest {
            request_id: 7,
            editor_revision: 9,
        });
        let was_playing = engine.is_playing();
        engine
            .stop(Duration::from_millis(100))
            .expect("close failed output");
        let error =
            RuntimeError::Audio("requested output failed; previous output also failed".to_owned());
        let expected = format!("could not switch to requested-output: {error}");
        let mut pending = VecDeque::from([StudioControlEvent::EngineFailure(EngineFailure {
            kind: "audio".to_owned(),
            message: "earlier recoverable audio failure".to_owned(),
            recoverable: true,
            playback_stopped: false,
        })]);

        answer_output_change(
            &mut engine,
            OutputChange::Device("requested-output"),
            was_playing,
            Err(error),
            &mut armed,
            &mut pending,
        );

        assert!(!engine.is_playing());
        assert!(armed.is_none());
        assert_eq!(pending.len(), 2);
        let outcome = pending
            .iter()
            .find_map(|event| match event {
                StudioControlEvent::Evaluation(outcome) => Some(outcome),
                _ => None,
            })
            .expect("the cancelled launch must be answered");
        assert_eq!(outcome.request_id, 7);
        assert_eq!(outcome.editor_revision, 9);
        let cancellation = outcome.result.as_ref().expect_err("launch was cancelled");
        assert_eq!(cancellation.kind, "cancelled");
        let failure = pending
            .iter()
            .find_map(|event| match event {
                StudioControlEvent::EngineFailure(failure) => Some(failure),
                _ => None,
            })
            .expect("output loss needs a terminal failure");
        assert_eq!(failure.kind, "audio");
        assert_eq!(failure.message, expected);
        assert!(failure.recoverable);
        assert!(failure.playback_stopped);
        assert!(engine.take_launch_outcome().is_none());
    }

    /// A latency change is answered by the same door as a device switch,
    /// in its own words: playback lost answers the waiting launch and says
    /// the set stopped; playback kept says only a refusal.
    #[test]
    fn a_latency_change_is_answered_like_an_output_switch_in_its_own_words() {
        let refused = || RuntimeError::Audio("recycle refused".to_owned());
        for result in [Ok(()), Err(refused())] {
            let mut engine = super::super::engine::tests::silent_engine_for_output_selection();
            assert!(engine.arm_launch("~", true, 4.0, false).unwrap().is_some());
            let mut armed = Some(ArmedRequest {
                request_id: 7,
                editor_revision: 9,
            });
            let was_playing = engine.is_playing();
            engine
                .stop(Duration::from_millis(100))
                .expect("the recycle lost the output");
            let expected = match &result {
                Ok(()) => (
                    "audio".to_owned(),
                    "audio stopped while changing output latency".to_owned(),
                ),
                Err(error) => (
                    error.kind().to_owned(),
                    format!("audio stopped while changing output latency: {error}"),
                ),
            };
            let mut pending = VecDeque::new();
            answer_output_change(
                &mut engine,
                OutputChange::Latency,
                was_playing,
                result,
                &mut armed,
                &mut pending,
            );
            assert!(armed.is_none(), "the cancelled launch is answered");
            assert!(pending.iter().any(|event| matches!(
                event,
                StudioControlEvent::Evaluation(EvaluationOutcome {
                    request_id: 7,
                    result: Err(_),
                    ..
                })
            )));
            let failure = pending
                .iter()
                .find_map(|event| match event {
                    StudioControlEvent::EngineFailure(failure) => Some(failure),
                    _ => None,
                })
                .expect("output loss needs a terminal failure");
            assert_eq!((failure.kind.clone(), failure.message.clone()), expected);
            assert!(failure.recoverable && failure.playback_stopped);
        }

        // Playback kept: a refusal is a warning on the control, and a size
        // that took needs no words - the row shows it.
        let mut engine = super::super::engine::tests::silent_engine_for_output_selection();
        let mut armed = None;
        let mut pending = VecDeque::new();
        answer_output_change(
            &mut engine,
            OutputChange::Latency,
            true,
            Err(refused()),
            &mut armed,
            &mut pending,
        );
        assert!(engine.is_playing());
        let StudioControlEvent::Diagnostic(diagnostic) = pending.pop_front().unwrap() else {
            panic!("preserved playback needs a warning, not a terminal failure");
        };
        assert_eq!(diagnostic.kind, "ui-control");
        assert_eq!(diagnostic.level, StudioDiagnosticLevel::Warning);
        assert_eq!(
            diagnostic.message,
            format!("output latency was refused: {}", refused())
        );
        assert!(pending.is_empty());
        answer_output_change(
            &mut engine,
            OutputChange::Latency,
            true,
            Ok(()),
            &mut armed,
            &mut pending,
        );
        assert!(pending.is_empty(), "a size that took says nothing");
    }

    #[test]
    fn output_selection_while_idle_reports_selection_without_playback() {
        let mut engine = StudioEngine::new(StudioConfig::default()).expect("studio");
        let mut pending = VecDeque::new();
        assert!(!process_command(
            &mut engine,
            EngineCommand::SetOutput("requested-output".to_owned()),
            &AtomicU64::new(0),
            &AtomicBool::new(false),
            &mut pending,
            &mut None,
            &mut None,
        ));

        assert!(!engine.is_playing());
        assert_eq!(engine.output_name().as_deref(), Some("requested-output"));
        let StudioControlEvent::Diagnostic(diagnostic) = pending.pop_front().unwrap() else {
            panic!("an idle selection needs no terminal failure");
        };
        assert_eq!(diagnostic.kind, "audio-device");
        assert_eq!(diagnostic.message, "selected output requested-output");
        assert!(pending.is_empty());
    }

    #[test]
    fn output_selection_error_while_idle_is_not_new_output_loss() {
        let mut engine = StudioEngine::new(StudioConfig::default()).expect("studio");
        let mut pending = VecDeque::new();
        answer_output_change(
            &mut engine,
            OutputChange::Device("requested-output"),
            false,
            Err(RuntimeError::Audio("output refused".to_owned())),
            &mut None,
            &mut pending,
        );
        assert!(!engine.is_playing());
        assert!(matches!(
            pending.pop_front().unwrap(),
            StudioControlEvent::Diagnostic(_)
        ));
        assert!(pending.is_empty());
    }

    #[test]
    fn recording_requests_retry_after_backpressure_without_reusing_ids() {
        let (sender, receiver) = sync_channel(1);
        let next = AtomicU64::new(1);
        let shutdown = AtomicBool::new(false);
        let path = std::path::PathBuf::from("take.wav");
        assert_eq!(
            try_send_recording(&sender, &next, &shutdown, Some(path.clone())),
            Some(1)
        );
        assert_eq!(try_send_recording(&sender, &next, &shutdown, None), None);
        assert!(
            matches!(receiver.try_recv().unwrap(), EngineCommand::Record {
            request_id: 1, path: Some(received),
        } if received == path)
        );
        assert_eq!(try_send_recording(&sender, &next, &shutdown, None), Some(3));
        assert!(matches!(
            receiver.try_recv().unwrap(),
            EngineCommand::Record {
                request_id: 3,
                path: None,
            }
        ));
        shutdown.store(true, Ordering::Release);
        assert_eq!(try_send_recording(&sender, &next, &shutdown, None), None);
        assert_eq!(next.load(Ordering::Relaxed), 4);
        shutdown.store(false, Ordering::Release);
        drop(receiver);
        assert_eq!(try_send_recording(&sender, &next, &shutdown, None), None);
    }

    #[test]
    fn recording_request_ids_refuse_exhaustion_without_wrapping() {
        let (sender, receiver) = sync_channel(1);
        let next = AtomicU64::new(u64::MAX - 1);
        let shutdown = AtomicBool::new(false);
        assert_eq!(
            try_send_recording(&sender, &next, &shutdown, None),
            Some(u64::MAX - 1)
        );
        receiver.try_recv().unwrap();
        for _ in 0..2 {
            assert_eq!(try_send_recording(&sender, &next, &shutdown, None), None);
            assert_eq!(next.load(Ordering::Relaxed), u64::MAX);
        }
    }

    #[test]
    fn recording_final_status_survives_control_backpressure_and_status_noise() {
        let (sender, receiver) = sync_channel(1);
        sender
            .try_send(StudioControlEvent::Snapshot(Box::default()))
            .unwrap();
        let status = TakeStatus {
            path: "take.wav".into(),
            sample_rate: 48_000,
            frames: 7,
            bytes: 44,
            error: Some("write failed".into()),
            final_signal: Some(super::super::wav::RawSignalSummary {
                sample_count: 14,
                nonfinite_count: 1,
                finite_peak: 1.25,
            }),
        };
        let mut pending = VecDeque::new();
        push_control(
            &mut pending,
            StudioControlEvent::Recording(RecordingOutcome {
                request_id: Some(9),
                result: Ok(RecordingReply::Finished {
                    capture_id: 7,
                    status: status.clone(),
                }),
            }),
        );
        for index in 0..MAX_PENDING_CONTROL {
            push_control(
                &mut pending,
                StudioControlEvent::EngineFailure(EngineFailure {
                    kind: format!("noise-{index}"),
                    message: "status".into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            );
        }
        assert_eq!(pending.len(), MAX_PENDING_CONTROL);
        assert!(!can_receive_command(&pending));
        flush_pending_control(&sender, &mut pending);
        assert_eq!(pending.len(), MAX_PENDING_CONTROL);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            StudioControlEvent::Snapshot(_)
        ));
        flush_pending_control(&sender, &mut pending);
        let StudioControlEvent::Recording(outcome) = receiver.try_recv().unwrap() else {
            panic!("the structural recording result must remain first");
        };
        assert_eq!(outcome.request_id, Some(9));
        assert!(matches!(outcome.result, Ok(RecordingReply::Finished {
            capture_id: 7, status: received,
        }) if received == status));
    }

    #[test]
    fn snapshots_cannot_overtake_a_start_reply_when_a_control_slot_opens() {
        let (sender, receiver) = sync_channel(1);
        sender
            .try_send(StudioControlEvent::Snapshot(Box::default()))
            .unwrap();
        let mut pending = VecDeque::new();
        push_control(
            &mut pending,
            StudioControlEvent::Recording(RecordingOutcome {
                request_id: Some(3),
                result: Ok(RecordingReply::Started { capture_id: 3 }),
            }),
        );
        flush_pending_control(&sender, &mut pending);
        assert_eq!(pending.len(), 1);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            StudioControlEvent::Snapshot(_)
        ));
        // The UI frees a slot after the failed flush, before the periodic update.
        try_send_snapshot(&sender, &pending, || panic!("snapshot must be skipped"));
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        flush_pending_control(&sender, &mut pending);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            StudioControlEvent::Recording(RecordingOutcome {
                request_id: Some(3),
                result: Ok(RecordingReply::Started { capture_id: 3 })
            })
        ));
        assert!(pending.is_empty());
        try_send_snapshot(&sender, &pending, StudioSnapshot::default);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            StudioControlEvent::Snapshot(_)
        ));
    }

    #[test]
    fn repeated_stop_without_a_capture_has_its_own_reply() {
        let mut engine = StudioEngine::new(StudioConfig::default()).unwrap();
        let epoch = AtomicU64::new(0);
        let shutdown = AtomicBool::new(false);
        let mut pending = VecDeque::new();
        let mut recording = None;
        for request_id in [8, 9] {
            process_command(
                &mut engine,
                EngineCommand::Record {
                    request_id,
                    path: None,
                },
                &epoch,
                &shutdown,
                &mut pending,
                &mut None,
                &mut recording,
            );
            assert!(recording.is_none());
            assert_eq!(pending.len(), 1);
            assert!(
                matches!(pending.pop_front().unwrap(), StudioControlEvent::Recording(
                RecordingOutcome { request_id: received, result: Ok(RecordingReply::NoActiveTake) }
            ) if received == Some(request_id))
            );
        }
    }

    fn record_command(
        engine: &mut StudioEngine,
        request_id: u64,
        path: Option<std::path::PathBuf>,
        recording: &mut Option<RecordingCapture>,
        pending: &mut VecDeque<StudioControlEvent>,
    ) {
        assert!(!process_command(
            engine,
            EngineCommand::Record { request_id, path },
            &AtomicU64::new(0),
            &AtomicBool::new(false),
            pending,
            &mut None,
            recording,
        ));
    }

    #[test]
    fn a_held_close_keeps_its_request_and_allows_audio_stop() {
        use super::super::engine::tests::HeldRecording;

        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut held = HeldRecording::new(directory.path().join("take.wav"), 48_000, false, false);
        let mut recording = Some(RecordingCapture {
            capture_id: 7,
            stop_request: None,
        });
        let mut pending = VecDeque::new();
        record_command(&mut held.engine, 8, None, &mut recording, &mut pending);
        held.wait_until_held();
        collect_recording(&mut held.engine, &mut recording, &mut pending);
        assert!(pending.is_empty());
        assert!(!held.closing_finished());
        assert_eq!(recording.as_ref().unwrap().stop_request, Some(8));

        let refused = directory.path().join("refused.wav");
        for (request_id, path) in [(9, None), (10, Some(refused.clone()))] {
            record_command(
                &mut held.engine,
                request_id,
                path,
                &mut recording,
                &mut pending,
            );
            assert!(matches!(pending.pop_front().unwrap(),
                StudioControlEvent::Recording(RecordingOutcome { request_id: Some(id), result: Err(error) })
                if id == request_id && error.recoverable && !error.playback_stopped));
            assert_eq!(recording.as_ref().unwrap().capture_id, 7);
            assert_eq!(recording.as_ref().unwrap().stop_request, Some(8));
        }
        assert!(!refused.exists());
        assert!(matches!(
            held.engine.tick_at(Duration::ZERO, |_| Ok(())).unwrap(),
            StudioTick::Running { step: Some(_), .. }
        ));

        // The first Stop drains. A distinct second press cuts immediately,
        // including a source that is deliberately still held open.
        assert!(!process_command(
            &mut held.engine,
            EngineCommand::Stop,
            &AtomicU64::new(0),
            &AtomicBool::new(false),
            &mut pending,
            &mut None,
            &mut recording,
        ));
        assert!(matches!(
            held.engine.tick_at(Duration::ZERO, |_| Ok(())).unwrap(),
            StudioTick::Stopping
        ));
        assert!(held.engine.is_stopping());

        assert!(!process_command(
            &mut held.engine,
            EngineCommand::StopImmediate,
            &AtomicU64::new(0),
            &AtomicBool::new(false),
            &mut pending,
            &mut None,
            &mut recording,
        ));
        assert!(!held.engine.is_playing());
        assert!(matches!(
            pending.pop_front().unwrap(),
            StudioControlEvent::Stopped(_)
        ));
        assert!(pending.is_empty());
        assert!(!held.closing_finished());
        held.release_and_wait();
        collect_recording(&mut held.engine, &mut recording, &mut pending);
        assert!(recording.is_none());
        assert!(matches!(pending.pop_front().unwrap(),
            StudioControlEvent::Recording(RecordingOutcome { request_id: Some(8),
                result: Ok(RecordingReply::Finished { capture_id: 7, status }) })
            if status.error.is_none() && status.final_signal.is_some()));
        collect_recording(&mut held.engine, &mut recording, &mut pending);
        assert!(pending.is_empty());
    }

    #[test]
    fn automatic_close_keeps_capture_identity_and_can_accept_a_stop_request() {
        use super::super::engine::tests::HeldRecording;

        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        for (tick_first, attach_request) in [(true, false), (false, true), (true, true)] {
            let mut held = HeldRecording::new(
                directory
                    .path()
                    .join(format!("old-rate-{tick_first}-{attach_request}.wav")),
                44_100,
                false,
                false,
            );
            let mut recording = Some(RecordingCapture {
                capture_id: 7,
                stop_request: None,
            });
            let mut pending = VecDeque::new();
            if tick_first {
                held.engine.tick_at(Duration::ZERO, |_| Ok(())).unwrap();
            }
            if attach_request {
                // Attach either to an existing automatic close or to one
                // discovered by this request's own final pump.
                record_command(&mut held.engine, 8, None, &mut recording, &mut pending);
            }
            held.wait_until_held();
            collect_recording(&mut held.engine, &mut recording, &mut pending);
            assert!(pending.is_empty());
            held.release_and_wait();
            collect_recording(&mut held.engine, &mut recording, &mut pending);
            assert!(recording.is_none());
            assert!(matches!(pending.pop_front().unwrap(),
                StudioControlEvent::Recording(RecordingOutcome { request_id,
                    result: Ok(RecordingReply::Finished { capture_id: 7, status }) })
                if request_id == attach_request.then_some(8) && status.sample_rate == 44_100
                    && status.error.is_none() && status.final_signal.is_some()));
            collect_recording(&mut held.engine, &mut recording, &mut pending);
            assert!(pending.is_empty());
        }
    }

    #[test]
    fn pump_overflow_retains_the_finished_capture_through_control_backpressure() {
        use super::super::engine::tests::HeldRecording;
        use super::super::wav::RawSignalSummary;

        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("overflow.wav");
        let mut held = HeldRecording::new(path.clone(), 48_000, false, false);
        let mut recording = Some(RecordingCapture {
            capture_id: 7,
            stop_request: None,
        });
        let mut pending = VecDeque::new();
        held.pump_frame_overflow();
        assert!(!held.engine.is_recording());
        assert!(held.engine.is_playing());

        // Attach an explicit stop to the already closing, overflowed take.
        record_command(&mut held.engine, 8, None, &mut recording, &mut pending);
        held.wait_until_held();
        collect_recording(&mut held.engine, &mut recording, &mut pending);
        assert!(pending.is_empty());
        assert!(!held.closing_finished());
        assert_eq!(recording.as_ref().unwrap().capture_id, 7);
        assert_eq!(recording.as_ref().unwrap().stop_request, Some(8));

        for id in 0..MAX_PENDING_CONTROL - 1 {
            push_control(
                &mut pending,
                StudioControlEvent::Recording(RecordingOutcome {
                    request_id: Some(100 + id as u64),
                    result: Ok(RecordingReply::NoActiveTake),
                }),
            );
        }
        held.release_and_wait();
        for _ in 0..2 {
            collect_recording(&mut held.engine, &mut recording, &mut pending);
            assert!(held.closing_finished());
            assert_eq!(pending.len(), MAX_PENDING_CONTROL - 1);
            assert_eq!(recording.as_ref().unwrap().capture_id, 7);
            assert_eq!(recording.as_ref().unwrap().stop_request, Some(8));
        }
        assert!(pending.pop_front().is_some());
        collect_recording(&mut held.engine, &mut recording, &mut pending);
        assert_eq!(
            pending.len(),
            MAX_PENDING_CONTROL - 1,
            "Stop retains its slot"
        );
        assert!(recording.is_none());
        let StudioControlEvent::Recording(outcome) = pending.pop_back().unwrap() else {
            panic!("the final reply must follow the older structural replies");
        };
        assert_eq!(outcome.request_id, Some(8));
        let Ok(RecordingReply::Finished {
            capture_id: 7,
            status,
        }) = outcome.result
        else {
            panic!("the finished capture must retain its status");
        };
        assert_eq!(status.path, path);
        assert_eq!(
            status.error.as_deref(),
            Some("recording frame count overflow")
        );
        // No overflowing batch reached the queue or PCM writer.
        assert_eq!(status.frames, 0);
        assert_eq!(status.bytes, 44);
        assert_eq!(status.final_signal, Some(RawSignalSummary::default()));
        let remaining = pending.len();
        collect_recording(&mut held.engine, &mut recording, &mut pending);
        assert_eq!(pending.len(), remaining);
        assert_eq!(held.engine.try_finish_recording(), None);
    }

    #[test]
    fn a_finished_writer_stays_owned_until_a_final_reply_fits() {
        use super::super::engine::tests::HeldRecording;

        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        for (read_only, panic_on_release) in [(false, false), (true, false), (true, true)] {
            let path = directory
                .path()
                .join(format!("backpressure-{read_only}-{panic_on_release}.wav"));
            let mut held = HeldRecording::new(path.clone(), 48_000, read_only, panic_on_release);
            let mut recording = Some(RecordingCapture {
                capture_id: 7,
                stop_request: None,
            });
            let mut pending = VecDeque::new();
            record_command(&mut held.engine, 8, None, &mut recording, &mut pending);
            held.wait_until_held();
            assert!(held.engine.stop(Duration::from_millis(200)).is_some());
            held.release_and_wait();
            for id in 0..MAX_PENDING_CONTROL - 1 {
                push_control(
                    &mut pending,
                    StudioControlEvent::Recording(RecordingOutcome {
                        request_id: Some(100 + id as u64),
                        result: Ok(RecordingReply::NoActiveTake),
                    }),
                );
            }
            for _ in 0..2 {
                collect_recording(&mut held.engine, &mut recording, &mut pending);
                assert!(held.closing_finished());
                assert_eq!(pending.len(), MAX_PENDING_CONTROL - 1);
                assert_eq!(recording.as_ref().unwrap().stop_request, Some(8));
            }
            assert!(pending.pop_front().is_some());
            collect_recording(&mut held.engine, &mut recording, &mut pending);
            assert_eq!(
                pending.len(),
                MAX_PENDING_CONTROL - 1,
                "Stop retains its slot"
            );
            assert!(recording.is_none());
            let StudioControlEvent::Recording(outcome) = pending.pop_back().unwrap() else {
                panic!("the final reply must follow the older structural replies");
            };
            assert_eq!(outcome.request_id, Some(8));
            let Ok(RecordingReply::Finished {
                capture_id: 7,
                status,
            }) = outcome.result
            else {
                panic!("the finished capture must retain its status");
            };
            assert_eq!(status.path, path);
            assert_eq!(status.error.is_some(), read_only);
            assert_eq!(status.final_signal.is_some(), !panic_on_release);
            if panic_on_release {
                let error = status.error.unwrap();
                assert!(error.ends_with("; recording writer thread panicked"));
                assert_eq!(error.matches("recording writer thread panicked").count(), 1);
            }
            let remaining = pending.len();
            collect_recording(&mut held.engine, &mut recording, &mut pending);
            assert_eq!(pending.len(), remaining);
        }
    }

    #[test]
    fn latest_audio_replaces_without_entering_control_queue() {
        let (control_tx, _control_rx) = sync_channel(1);
        let (trace_tx, _trace_rx) = sync_channel(1);
        let latest = Mutex::new(None);
        let metadata = UiAudioMetadata {
            sequence: 1,
            generation: 2,
            device_time: 0.5,
            stream_id: 3,
            epoch: 4,
            end_frame: 24_000,
            sample_rate: 48_000,
        };
        let analysis = UiAudioAnalysisSet {
            master: rustel_runtime::ui_analysis::UiAudioAnalysisFrame {
                scope: [0.0; rustel_runtime::ui_analysis::UI_SCOPE_SAMPLES],
                spectrum: [-120.0; rustel_runtime::ui_analysis::UI_SPECTRUM_BINS],
            },
            visuals: Vec::new(),
            sides: Vec::new(),
        };
        assert!(
            route_update(
                StudioUpdate::Audio {
                    metadata,
                    analysis: Box::new(analysis.clone()),
                },
                &control_tx,
                &trace_tx,
                &latest,
            )
            .is_ok()
        );
        assert_eq!(latest.lock().unwrap().as_ref().unwrap().1, analysis);
    }

    #[test]
    fn optional_diagnostics_respect_admission_without_evicting_outcomes() {
        for occupied in [
            MAX_PENDING_CONTROL - 2,
            MAX_PENDING_CONTROL - 1,
            MAX_PENDING_CONTROL,
        ] {
            let mut pending = VecDeque::new();
            for request_id in 0..occupied {
                push_control(
                    &mut pending,
                    StudioControlEvent::Recording(RecordingOutcome {
                        request_id: Some(request_id as u64),
                        result: Ok(RecordingReply::NoActiveTake),
                    }),
                );
            }
            let admitted = can_receive_command(&pending);
            push_optional_diagnostic(&mut pending, StudioDiagnostic::info("hydra", "first"));
            for _ in 0..MAX_PENDING_CONTROL {
                push_optional_diagnostic(&mut pending, StudioDiagnostic::info("hydra", "later"));
            }
            assert_eq!(pending.len(), occupied + usize::from(admitted));
            for request_id in 0..occupied {
                assert!(matches!(pending.pop_front().unwrap(),
                    StudioControlEvent::Recording(RecordingOutcome {
                        request_id: Some(id), result: Ok(RecordingReply::NoActiveTake),
                    }) if id == request_id as u64));
            }
            if admitted {
                assert!(matches!(pending.pop_front().unwrap(),
                    StudioControlEvent::Diagnostic(diagnostic)
                    if diagnostic.kind == "hydra" && diagnostic.message == "first"));
            }
            assert!(pending.is_empty());
        }
    }

    #[test]
    fn optional_diagnostics_preserve_failures_and_recording_fifo_progress() {
        let (sender, receiver) = sync_channel(1);
        sender
            .try_send(StudioControlEvent::Snapshot(Box::default()))
            .unwrap();
        let status = TakeStatus {
            path: "take.wav".into(),
            sample_rate: 48_000,
            frames: 7,
            bytes: 44,
            error: Some("write failed".into()),
            final_signal: Some(super::super::wav::RawSignalSummary {
                sample_count: 14,
                nonfinite_count: 1,
                finite_peak: 1.25,
            }),
        };
        let mut pending = VecDeque::new();
        push_control(
            &mut pending,
            StudioControlEvent::Recording(RecordingOutcome {
                request_id: Some(9),
                result: Ok(RecordingReply::Finished {
                    capture_id: 7,
                    status: status.clone(),
                }),
            }),
        );
        push_control(
            &mut pending,
            StudioControlEvent::EngineFailure(EngineFailure {
                kind: "audio".into(),
                message: "device failed".into(),
                recoverable: true,
                playback_stopped: true,
            }),
        );
        for request_id in 0..MAX_PENDING_CONTROL - 2 {
            push_control(
                &mut pending,
                StudioControlEvent::Recording(RecordingOutcome {
                    request_id: Some(100 + request_id as u64),
                    result: Ok(RecordingReply::NoActiveTake),
                }),
            );
        }
        push_optional_diagnostic(&mut pending, StudioDiagnostic::info("hydra", "skipped"));
        flush_pending_control(&sender, &mut pending);
        assert_eq!(pending.len(), MAX_PENDING_CONTROL);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            StudioControlEvent::Snapshot(_)
        ));

        flush_pending_control(&sender, &mut pending);
        assert!(matches!(receiver.try_recv().unwrap(),
            StudioControlEvent::Recording(RecordingOutcome { request_id: Some(9),
                result: Ok(RecordingReply::Finished { capture_id: 7, status: received })
            }) if received == status));
        flush_pending_control(&sender, &mut pending);
        assert!(matches!(receiver.try_recv().unwrap(),
            StudioControlEvent::EngineFailure(failure)
            if failure.kind == "audio" && failure.message == "device failed"
                && failure.recoverable && failure.playback_stopped));
        push_optional_diagnostic(&mut pending, StudioDiagnostic::info("hydra", "resumed"));
        assert_eq!(pending.len(), MAX_PENDING_CONTROL - 1);
        for request_id in 0..MAX_PENDING_CONTROL - 2 {
            flush_pending_control(&sender, &mut pending);
            assert!(matches!(receiver.try_recv().unwrap(),
                StudioControlEvent::Recording(RecordingOutcome {
                    request_id: Some(id), result: Ok(RecordingReply::NoActiveTake),
                }) if id == 100 + request_id as u64));
        }
        flush_pending_control(&sender, &mut pending);
        assert!(matches!(receiver.try_recv().unwrap(),
            StudioControlEvent::Diagnostic(diagnostic)
            if diagnostic.kind == "hydra" && diagnostic.message == "resumed"));
        assert!(pending.is_empty());
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn pending_control_drops_snapshot_before_an_outcome() {
        let snapshot = StudioSnapshot {
            playing: false,
            stopping: false,
            session_generation: 0,
            audible_generation: None,
            confirmed_audio_generation: None,
            source_revision: None,
            cps: 0.5,
            device_time: 0.0,
            cycle: 0.0,
            device: None,
            input_device: None,
            input_channels: 0,
            input_lag_frames: 0,
            input_peak: 0.0,
            recording: None,
            launch: None,
            orbits: Vec::new(),
            output_pairs: 1,
            clock: ClockStatus {
                out_port: None,
                in_port: None,
                external_bpm: None,
                locked: false,
            },
            pressure: None,
            audition_loading: None,
            loading: None,
            sample_memory: Default::default(),
            script_heap_bytes: 0,
            audio_memory: None,
        };
        let mut pending = VecDeque::new();
        for _ in 0..MAX_PENDING_CONTROL {
            pending.push_back(StudioControlEvent::Snapshot(Box::new(snapshot.clone())));
        }
        let failure = StudioControlEvent::EngineFailure(EngineFailure {
            kind: "audio".into(),
            message: "gone".into(),
            recoverable: true,
            playback_stopped: false,
        });
        push_control(&mut pending, failure);
        assert_eq!(pending.len(), MAX_PENDING_CONTROL);
        assert!(
            pending
                .iter()
                .any(|event| matches!(event, StudioControlEvent::EngineFailure(_)))
        );
    }

    #[test]
    fn pending_control_never_evicts_an_evaluation_for_status_noise() {
        let mut pending = VecDeque::new();
        for index in 0..MAX_PENDING_CONTROL {
            push_control(
                &mut pending,
                StudioControlEvent::EngineFailure(EngineFailure {
                    kind: format!("noise-{index}"),
                    message: "recoverable".into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            );
        }
        push_control(
            &mut pending,
            StudioControlEvent::Evaluation(EvaluationOutcome {
                request_id: 7,
                editor_revision: 9,
                result: Err(EngineFailure {
                    kind: "compile".into(),
                    message: "bad score".into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            }),
        );

        assert_eq!(pending.len(), MAX_PENDING_CONTROL);
        assert!(pending.iter().any(|event| {
            matches!(event, StudioControlEvent::Evaluation(outcome) if outcome.request_id == 7)
        }));
    }

    #[test]
    fn structural_saturation_backpressures_command_consumption() {
        let mut pending = VecDeque::new();
        for request_id in 0..MAX_PENDING_CONTROL.saturating_sub(1) {
            pending.push_back(StudioControlEvent::Evaluation(EvaluationOutcome {
                request_id: request_id as u64,
                editor_revision: request_id as u64,
                result: Err(EngineFailure {
                    kind: "compile".into(),
                    message: "bad score".into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            }));
        }
        assert!(!can_receive_command(&pending));
        push_control(
            &mut pending,
            StudioControlEvent::Stopped(Box::new(StudioStop::idle())),
        );
        assert_eq!(pending.len(), MAX_PENDING_CONTROL);
    }

    #[test]
    fn idle_stop_is_acknowledged() {
        let mut engine = StudioEngine::new(StudioConfig::default()).unwrap();
        let epoch = AtomicU64::new(0);
        let shutdown = AtomicBool::new(false);
        let mut pending = VecDeque::new();

        process_command(
            &mut engine,
            EngineCommand::Stop,
            &epoch,
            &shutdown,
            &mut pending,
            &mut None,
            &mut None,
        );

        assert!(matches!(
            pending.front(),
            Some(StudioControlEvent::Stopped(stop)) if stop.acknowledged
        ));
    }

    #[test]
    fn a_stop_epoch_discards_queued_evaluation_before_device_start() {
        let mut engine = StudioEngine::new(StudioConfig::default()).unwrap();
        let epoch = AtomicU64::new(2);
        let shutdown = AtomicBool::new(false);
        let mut pending = VecDeque::new();

        process_command(
            &mut engine,
            EngineCommand::Evaluate {
                request_id: 11,
                editor_revision: 4,
                command_epoch: 1,
                source: Arc::<str>::from("note(60)"),
                mini: false,
                launch: Launch::Now,
                rewind: false,
                preview: false,
            },
            &epoch,
            &shutdown,
            &mut pending,
            &mut None,
            &mut None,
        );

        assert!(!engine.is_playing());
        assert!(pending.iter().any(|event| {
            matches!(
                event,
                StudioControlEvent::Evaluation(EvaluationOutcome {
                    request_id: 11,
                    result: Err(EngineFailure { kind, .. }),
                    ..
                }) if kind == "cancelled"
            )
        }));
    }

    /// A quantised-rewind Evaluate exactly as the studio sends it.
    fn rewind_evaluate(request_id: u64, source: &str) -> EngineCommand {
        EngineCommand::Evaluate {
            request_id,
            editor_revision: request_id,
            command_epoch: 0,
            source: Arc::<str>::from(source),
            mini: false,
            launch: Launch::Quantised { unit_cycles: 0.25 },
            rewind: true,
            preview: false,
        }
    }

    /// The worker's own turn - tick, answer the armed request, collect the
    /// evaluation outcomes it pushed - repeated until `until` says stop.
    fn drive_worker(
        engine: &mut StudioEngine,
        armed: &mut Option<ArmedRequest>,
        pending: &mut VecDeque<StudioControlEvent>,
        outcomes: &mut Vec<EvaluationOutcome>,
        until: impl Fn(
            &mut StudioEngine,
            &Option<ArmedRequest>,
            &VecDeque<StudioControlEvent>,
            &Vec<EvaluationOutcome>,
        ) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !until(engine, armed, pending, outcomes) {
            assert!(
                Instant::now() < deadline,
                "the worker sequence never reached its goal"
            );
            if engine.is_playing() {
                engine.tick(|_| Ok(())).expect("the worker's tick");
                answer_launch(engine, armed, pending);
            }
            collect_outcomes(pending, outcomes);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Drain the pushed events, keeping the evaluation outcomes in order.
    fn collect_outcomes(
        pending: &mut VecDeque<StudioControlEvent>,
        outcomes: &mut Vec<EvaluationOutcome>,
    ) {
        pending.drain(..).for_each(|event| {
            if let StudioControlEvent::Evaluation(outcome) = event {
                outcomes.push(outcome);
            }
        });
    }

    /// Hand `command` to the worker as its loop does, with no Stop pending.
    fn press(
        engine: &mut StudioEngine,
        command: EngineCommand,
        armed: &mut Option<ArmedRequest>,
        pending: &mut VecDeque<StudioControlEvent>,
    ) {
        process_command(
            engine,
            command,
            &AtomicU64::new(0),
            &AtomicBool::new(false),
            pending,
            armed,
            &mut None,
        );
    }

    /// Whether a supersede note was pushed.
    fn superseded(pending: &VecDeque<StudioControlEvent>) -> bool {
        pending.iter().any(|event| {
            matches!(
                event,
                StudioControlEvent::Diagnostic(diagnostic)
                    if diagnostic.message == "a second launch superseded the waiting one"
            )
        })
    }

    /// The device time the countdown's line falls on. A rewind restarts the
    /// cycle count at its line, so lines on either side of one compare by
    /// time. Exact only with the clock held.
    fn countdown_line_at(engine: &mut StudioEngine) -> Option<f64> {
        let now = super::super::engine::tests::clock_now(engine);
        engine.snapshot().launch.map(|info| now + info.seconds_left)
    }

    /// One beat of [`rewind_evaluate`]'s quantise unit at the playing tempo.
    fn beat_seconds(engine: &mut StudioEngine) -> f64 {
        0.25 / engine.snapshot().cps
    }

    /// Press 2 arrives while press 1 is still pending, outside its head-room.
    /// The worker cancels press 1 before it arms press 2. Press 1 is answered
    /// cancelled, press 2 fires on press 1's line, and exactly one generation
    /// is installed.
    #[test]
    fn a_repeat_press_while_a_launch_is_still_pending_supersedes_through_the_worker() {
        use super::super::engine::tests::hold_clock;
        let mut engine = super::super::engine::tests::silent_engine_for_output_selection();
        let source = "s(\"tri\")";
        let epoch = AtomicU64::new(0);
        let shutdown = AtomicBool::new(false);
        let mut armed: Option<ArmedRequest> = None;
        let mut pending = VecDeque::new();
        let mut outcomes = Vec::new();

        // Held until press 2 is handled: press 1's line stays a head-room
        // or more away.
        hold_clock(&engine, true);
        process_command(
            &mut engine,
            rewind_evaluate(1, source),
            &epoch,
            &shutdown,
            &mut pending,
            &mut armed,
            &mut None,
        );
        assert!(armed.is_some(), "press 1 arms against the playing engine");
        let line = engine
            .snapshot()
            .launch
            .expect("press 1 is waiting for its line")
            .boundary_cycle;

        // Press 2 of the eager double, before anything fired: the worker's
        // supersede path runs cancel_pending_launch, then arm_launch.
        pending.clear();
        process_command(
            &mut engine,
            rewind_evaluate(2, source),
            &epoch,
            &shutdown,
            &mut pending,
            &mut armed,
            &mut None,
        );
        let launch_lines: Vec<&StudioDiagnostic> = pending
            .iter()
            .filter_map(|event| match event {
                StudioControlEvent::Diagnostic(diagnostic) if diagnostic.kind == "launch" => {
                    Some(diagnostic)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            launch_lines
                .iter()
                .filter(|line| line.message == "a second launch superseded the waiting one")
                .count(),
            1,
            "one supersede line: {launch_lines:?}"
        );
        assert!(
            launch_lines
                .iter()
                .all(|line| line.level == super::super::engine::StudioDiagnosticLevel::Note),
            "launch decisions stay off the status line: {launch_lines:?}"
        );
        let boundary = engine
            .snapshot()
            .launch
            .expect("press 2 is waiting for its line")
            .boundary_cycle;
        assert_eq!(boundary, line, "press 2 keeps press 1's line");

        hold_clock(&engine, false);
        drive_worker(
            &mut engine,
            &mut armed,
            &mut pending,
            &mut outcomes,
            |_, _, _, outcomes| outcomes.len() == 2,
        );
        assert_eq!(outcomes[0].request_id, 1, "press 1's cancel was answered");
        assert!(outcomes[0].result.is_err(), "press 1 was superseded");
        assert_eq!(outcomes[1].request_id, 2, "press 2's fire was answered");
        let install = outcomes[1].result.as_ref().expect("press 2 fires");
        assert_eq!(
            engine.generation(),
            install.generation,
            "exactly one generation was installed"
        );
        assert_eq!(
            engine.snapshot().launch.map(|info| info.boundary_cycle),
            Some(boundary),
            "the landing counts down to the line press 2 announced"
        );
    }

    /// A held key repeats faster than the worker is sure to tick. A repeat
    /// handled once the armed rewind's head-room has opened, before a tick
    /// fired it, meets the launch in flight: press 1 fires on its own line,
    /// press 2 is answered with it, and one generation is installed.
    #[test]
    fn a_repeat_press_inside_the_head_room_is_answered_by_the_launch_it_fires() {
        use super::super::engine::tests::{clock_now, hold_clock, hold_clock_until, launch_is_due};
        let mut engine = super::super::engine::tests::silent_engine_for_output_selection();
        let source = "s(\"tri\")";
        let mut armed = None;
        let mut pending = VecDeque::new();
        let mut outcomes = Vec::new();

        press(
            &mut engine,
            rewind_evaluate(1, source),
            &mut armed,
            &mut pending,
        );
        // The clock stops where the next tick would fire press 1, and press 2
        // is handled before that tick.
        hold_clock_until(&engine, launch_is_due);
        let line_at = countdown_line_at(&mut engine).expect("press 1 waits for its line");
        assert!(
            line_at > clock_now(&engine),
            "the clock stopped before the line"
        );
        press(
            &mut engine,
            rewind_evaluate(2, source),
            &mut armed,
            &mut pending,
        );
        answer_launch(&mut engine, &mut armed, &mut pending);
        let was_superseded = superseded(&pending);
        collect_outcomes(&mut pending, &mut outcomes);
        assert_eq!(
            countdown_line_at(&mut engine),
            Some(line_at),
            "the repeat keeps press 1's line: {outcomes:?}"
        );
        assert!(!was_superseded, "nothing was superseded");
        let [first, second] = outcomes.as_slice() else {
            panic!("both presses are answered: {outcomes:?}");
        };
        assert_eq!(first.request_id, 1);
        let install = first.result.clone().expect("press 1 fires on its line");
        assert!(!install.answered_repeat, "press 1 installs");
        assert_eq!(second.request_id, 2);
        let answer = second.result.as_ref().expect("press 2 is answered");
        assert!(answer.answered_repeat, "press 2 installs nothing");
        assert_eq!(answer.generation, install.generation);
        assert!(armed.is_none(), "nothing waits for a later line");

        // Past the line, press 1's install is still the only one.
        hold_clock(&engine, false);
        drive_worker(
            &mut engine,
            &mut armed,
            &mut pending,
            &mut outcomes,
            |engine, _, _, _| clock_now(engine) > line_at + 0.1,
        );
        assert_eq!(
            engine.generation(),
            install.generation,
            "exactly one generation was installed"
        );
        assert_eq!(outcomes.len(), 2, "nothing else was answered");
    }

    /// A different score pressed inside the armed launch's head-room takes
    /// the next beat: the armed launch keeps its own line, as the tick that
    /// was due would have fired it.
    #[test]
    fn a_different_launch_inside_the_head_room_takes_the_next_beat() {
        use super::super::engine::tests::{hold_clock, hold_clock_until, launch_is_due};
        let mut engine = super::super::engine::tests::silent_engine_for_output_selection();
        let mut armed = None;
        let mut pending = VecDeque::new();
        let mut outcomes = Vec::new();

        press(
            &mut engine,
            rewind_evaluate(1, "s(\"tri\")"),
            &mut armed,
            &mut pending,
        );
        hold_clock_until(&engine, launch_is_due);
        let line_at = countdown_line_at(&mut engine).expect("A waits for its line");
        press(
            &mut engine,
            rewind_evaluate(2, "s(\"saw\")"),
            &mut armed,
            &mut pending,
        );
        answer_launch(&mut engine, &mut armed, &mut pending);
        assert!(!superseded(&pending), "A was not superseded");
        collect_outcomes(&mut pending, &mut outcomes);
        let [a] = outcomes.as_slice() else {
            panic!("only A is answered yet: {outcomes:?}");
        };
        let a = a.result.clone().expect("A fires on its line").generation;
        let b_at = countdown_line_at(&mut engine).expect("B waits for its line");
        assert!(
            (b_at - (line_at + beat_seconds(&mut engine))).abs() < 1e-6,
            "B waits for the beat after A's line: {b_at} after {line_at}"
        );

        hold_clock(&engine, false);
        drive_worker(
            &mut engine,
            &mut armed,
            &mut pending,
            &mut outcomes,
            |_, _, _, outcomes| outcomes.len() == 2,
        );
        assert_eq!(outcomes[1].request_id, 2);
        let b = outcomes[1].result.as_ref().expect("B fires").generation;
        assert!(b > a, "B installs after A");
    }

    /// Past the grace, a held key's press arms the next beat and restarts
    /// the score there, so holding the key restarts it on every beat.
    #[test]
    fn a_press_past_the_grace_arms_the_next_beat_through_the_worker() {
        use super::super::engine::tests::{clock_now, hold_clock, hold_clock_until, launch_is_due};
        let mut engine = super::super::engine::tests::silent_engine_for_output_selection();
        let source = "s(\"tri\")";
        let mut armed = None;
        let mut pending = VecDeque::new();
        let mut outcomes = Vec::new();

        press(
            &mut engine,
            rewind_evaluate(1, source),
            &mut armed,
            &mut pending,
        );
        // Press 1 fires on the worker's tick, with the clock held where it
        // is due.
        hold_clock_until(&engine, launch_is_due);
        drive_worker(
            &mut engine,
            &mut armed,
            &mut pending,
            &mut outcomes,
            |_, _, _, outcomes| outcomes.len() == 1,
        );
        let first = outcomes[0]
            .result
            .clone()
            .expect("press 1 fires")
            .generation;
        let line_at = countdown_line_at(&mut engine).expect("press 1 waits for its line");
        let beat = beat_seconds(&mut engine);

        // The worker's turns run the line past and then the grace, half a
        // beat.
        hold_clock(&engine, false);
        drive_worker(
            &mut engine,
            &mut armed,
            &mut pending,
            &mut outcomes,
            |engine, _, _, _| clock_now(engine) > line_at + beat / 2.0,
        );
        hold_clock(&engine, true);
        press(
            &mut engine,
            rewind_evaluate(2, source),
            &mut armed,
            &mut pending,
        );
        assert!(!superseded(&pending), "nothing was waiting to supersede");
        collect_outcomes(&mut pending, &mut outcomes);
        assert_eq!(outcomes.len(), 1, "press 2 is not answered as a repeat");
        assert!(armed.is_some(), "press 2 waits for its own line");
        let restart_at = countdown_line_at(&mut engine).expect("press 2 waits for its line");
        assert!(
            (restart_at - (line_at + beat)).abs() < 1e-6,
            "press 2 arms the next beat: {restart_at} after {line_at}"
        );

        hold_clock(&engine, false);
        drive_worker(
            &mut engine,
            &mut armed,
            &mut pending,
            &mut outcomes,
            |_, _, _, outcomes| outcomes.len() == 2,
        );
        let restart = outcomes[1].result.as_ref().expect("press 2 fires");
        assert!(!restart.answered_repeat, "press 2 restarts the score");
        assert!(restart.generation > first, "from a fresh install");
    }

    /// Press 2 arrives after press 1 fired and its landing settled past the
    /// line. The grace memory answers it, and nothing arms or installs again.
    #[test]
    fn a_repeat_press_after_the_line_is_answered_through_the_worker() {
        let mut engine = super::super::engine::tests::silent_engine_for_output_selection();
        let source = "s(\"tri\")";
        let epoch = AtomicU64::new(0);
        let shutdown = AtomicBool::new(false);
        let mut armed: Option<ArmedRequest> = None;
        let mut pending = VecDeque::new();
        let mut outcomes = Vec::new();

        // Press 1: armed, fired, and its outcome answered.
        process_command(
            &mut engine,
            rewind_evaluate(1, source),
            &epoch,
            &shutdown,
            &mut pending,
            &mut armed,
            &mut None,
        );
        drive_worker(
            &mut engine,
            &mut armed,
            &mut pending,
            &mut outcomes,
            |_, _, _, outcomes| outcomes.len() == 1,
        );
        let generation = engine.generation();
        assert!(
            engine.snapshot().launch.is_some(),
            "the fired launch waits for its line"
        );

        // Run the line past: the landing settles into the grace memory and
        // the countdown goes - there is nothing left to wait for.
        drive_worker(
            &mut engine,
            &mut armed,
            &mut pending,
            &mut outcomes,
            |engine, _, _, _| engine.snapshot().launch.is_none(),
        );

        // Press 2, a beat's width after press 1 fired and landed. The
        // worker adopts it and the grace answers at once - collected by the
        // same answer_launch that pairs outcomes with their request.
        process_command(
            &mut engine,
            rewind_evaluate(2, source),
            &epoch,
            &shutdown,
            &mut pending,
            &mut armed,
            &mut None,
        );
        drive_worker(
            &mut engine,
            &mut armed,
            &mut pending,
            &mut outcomes,
            |_, _, _, outcomes| outcomes.len() == 2,
        );
        assert_eq!(outcomes[1].request_id, 2);
        let answer = outcomes[1].result.as_ref().expect("answered with success");
        assert_eq!(
            answer.generation, generation,
            "the answer is the generation already sounding"
        );
        assert!(answer.answered_repeat, "and says nothing was installed");
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            engine.tick(|_| Ok(())).expect("tick inside the grace");
            answer_launch(&mut engine, &mut armed, &mut pending);
            pending.clear();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            engine.generation(),
            generation,
            "a repeat press never re-installs the score"
        );
        assert!(
            engine.snapshot().launch.is_none(),
            "an answered repeat shows no countdown"
        );
    }

    /// A fires, B is armed over it, and A is pressed again before A's line.
    /// The supersede of B keeps A's fired landing, so A is installed once.
    #[test]
    fn an_a_b_a_pad_roll_through_the_worker_installs_a_once() {
        use super::super::engine::tests::{clock_now, hold_clock};
        let mut engine = super::super::engine::tests::silent_engine_for_output_selection();
        let epoch = AtomicU64::new(0);
        let shutdown = AtomicBool::new(false);
        let mut armed: Option<ArmedRequest> = None;
        let mut pending = VecDeque::new();
        let mut outcomes: Vec<EvaluationOutcome> = Vec::new();

        process_command(
            &mut engine,
            rewind_evaluate(1, "s(\"tri\")"),
            &epoch,
            &shutdown,
            &mut pending,
            &mut armed,
            &mut None,
        );
        // Tick to A's fire with the clock held from the firing tick on, so
        // the next two presses land before A's line.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            hold_clock(&engine, true);
            engine.tick(|_| Ok(())).expect("tick");
            answer_launch(&mut engine, &mut armed, &mut pending);
            collect_outcomes(&mut pending, &mut outcomes);
            if !outcomes.is_empty() {
                break;
            }
            hold_clock(&engine, false);
            assert!(Instant::now() < deadline, "A never fired");
            std::thread::sleep(Duration::from_millis(2));
        }
        let a = outcomes[0].result.as_ref().expect("A fires").generation;

        process_command(
            &mut engine,
            rewind_evaluate(2, "s(\"saw\")"),
            &epoch,
            &shutdown,
            &mut pending,
            &mut armed,
            &mut None,
        );
        assert!(armed.is_some(), "B waits for its line");
        process_command(
            &mut engine,
            rewind_evaluate(3, "s(\"tri\")"),
            &epoch,
            &shutdown,
            &mut pending,
            &mut armed,
            &mut None,
        );
        answer_launch(&mut engine, &mut armed, &mut pending);
        collect_outcomes(&mut pending, &mut outcomes);
        assert_eq!(outcomes.len(), 3, "B cancelled, the second A answered");
        assert!(outcomes[1].result.is_err(), "B was superseded");
        assert_eq!(
            outcomes[2].result.as_ref().expect("A again").generation,
            a,
            "the second A is answered with the A in flight"
        );
        assert!(armed.is_none(), "nothing waits for another line");

        // Well past both lines, A is still the only install.
        hold_clock(&engine, false);
        let until = clock_now(&engine) + 1.0;
        while clock_now(&engine) < until {
            engine.tick(|_| Ok(())).expect("tick past the lines");
            answer_launch(&mut engine, &mut armed, &mut pending);
            collect_outcomes(&mut pending, &mut outcomes);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(engine.generation(), a, "A is installed exactly once");
        assert_eq!(outcomes.len(), 3, "nothing else fired");
    }

    /// A latency change whose recycle loses the output stops the set. The
    /// waiting launch is answered cancelled, and the surface learns that
    /// playback stopped.
    #[test]
    fn a_latency_change_that_stops_the_set_answers_the_armed_launch() {
        let mut engine = super::super::engine::tests::silent_engine_for_output_selection();
        engine.set_fail_output_recycles_for_test(true);
        let epoch = AtomicU64::new(0);
        let shutdown = AtomicBool::new(false);
        let mut armed: Option<ArmedRequest> = None;
        let mut pending = VecDeque::new();
        process_command(
            &mut engine,
            rewind_evaluate(5, "s(\"tri\")"),
            &epoch,
            &shutdown,
            &mut pending,
            &mut armed,
            &mut None,
        );
        assert!(armed.is_some(), "armed against the playing engine");
        pending.clear();

        process_command(
            &mut engine,
            EngineCommand::SetOutputBufferFrames(Some(256)),
            &epoch,
            &shutdown,
            &mut pending,
            &mut armed,
            &mut None,
        );
        assert!(!engine.is_playing(), "the failed recycle stopped the set");
        assert!(armed.is_none(), "the armed request was answered");
        assert!(pending.iter().any(|event| matches!(
            event,
            StudioControlEvent::Evaluation(EvaluationOutcome {
                request_id: 5,
                result: Err(_),
                ..
            })
        )));
        assert!(pending.iter().any(|event| matches!(
            event,
            StudioControlEvent::EngineFailure(EngineFailure {
                playback_stopped: true,
                ..
            })
        )));
    }
}
