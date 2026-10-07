//! Layout delivery, trace correlation, preview updates, and UI audio analysis.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use rustel_audio::{LIVE_ANALYSIS_WINDOW_SAMPLES, LiveScalarDevice};
use rustel_scheduler::ScheduleTraceEvent;

use rustel_runtime::Session;
use rustel_runtime::ui_analysis::{UiAudioAnalysisSet, UiAudioAnalyzer};
use rustel_runtime::ui_events::{
    MAX_UI_LAYOUT_SLIDERS, MAX_UI_LAYOUT_VISUALS, UiAcceptedOnset, UiAudioMetadata,
    UiEventSendStatus, UiLayout, UiLayoutEnvelope, UiLayoutValidationError, UiSlider,
    UiTraceBatchRequest, correlate_submitted_traces, dispatch_trace_batches, ingest_pending_traces,
    prune_pending_traces, source_revision, visual_layout,
};

use super::{
    AUDITION_VISUALS, PendingDiagnostics, StudioDiagnostic, StudioUpdate, StudioUpdateSendResult,
    queue_diagnostic,
};

const MAX_PENDING_UI_TRACES: usize = 8_192;
const STALE_UI_TRACE_GRACE_SECS: f64 = 1.0;
const UI_TRACE_PRUNE_INTERVAL: Duration = Duration::from_millis(250);
/// How often the painters are told what is coming beyond the audio's
/// horizon, and how far ahead: strudel.cc's roll shows two cycles of
/// upcoming bars, and a little more covers the batch interval.
const PREVIEW_INTERVAL: Duration = Duration::from_millis(250);
const PREVIEW_LOOKAHEAD_CYCLES: f64 = 2.5;
/// How far behind the playhead a redraw reads: the roll's default window
/// is four cycles with the playhead halfway, so this covers what a painter
/// shows of the past.
const PREVIEW_LOOKBEHIND_CYCLES: f64 = 2.5;

#[derive(Debug, Default)]
pub(super) struct LayoutDelivery {
    observed_generation: Option<u64>,
    delivered_generation: Option<u64>,
    source_revision: Option<String>,
    sliders: BTreeMap<String, UiSlider>,
    pub(super) pending: Option<UiLayoutEnvelope>,
    visual_audio_mask: u64,
}

impl LayoutDelivery {
    pub(super) fn observe(
        &mut self,
        source: &str,
        generation: u64,
    ) -> Result<bool, UiLayoutValidationError> {
        if self.observed_generation == Some(generation) {
            return Ok(false);
        }
        let parsed = visual_layout(source, generation);
        self.observed_generation = Some(generation);
        self.delivered_generation = None;
        self.source_revision = None;
        self.sliders.clear();
        self.pending = None;
        self.visual_audio_mask = 0;
        let layout = match parsed {
            Ok(layout) => layout,
            Err(error) => {
                // A rejected visual annotation must still replace the prior
                // generation's widgets. Deliver a revision-correct empty
                // layout, then surface the validation error separately.
                let revision = source_revision(source);
                self.source_revision = Some(revision.clone());
                self.pending = Some(UiLayoutEnvelope::new(UiLayout::new(
                    generation,
                    revision,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )?));
                return Err(error);
            }
        };
        self.source_revision = Some(layout.ui_layout.source_revision.clone());
        self.visual_audio_mask = layout
            .ui_layout
            .visuals
            .iter()
            .filter(|visual| matches!(visual.kind.as_str(), "scope" | "tscope" | "spectrum"))
            .filter_map(|visual| visual.slot)
            .fold(0_u64, |mask, slot| mask | (1_u64 << slot));
        self.sliders.extend(
            layout
                .ui_layout
                .sliders
                .iter()
                .take(MAX_UI_LAYOUT_SLIDERS)
                .cloned()
                .map(|slider| (slider.id.clone(), slider)),
        );
        self.pending = Some(layout);
        Ok(true)
    }

    fn sync_slider_values(&mut self, session: &Session) {
        let ids = self.sliders.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            let Some(slider) = self.sliders.get(&id) else {
                continue;
            };
            let Ok(Some(value)) = session.slider_value(&id) else {
                continue;
            };
            if value < slider.min || value > slider.max {
                continue;
            }
            self.set_slider_value(&id, value);
        }
    }

    /// The bounds of a slider the evaluated generation registered - the one
    /// `observe` last read, which may still be waiting to cut over.
    pub(super) fn slider(&self, id: &str) -> Option<&UiSlider> {
        self.sliders.get(id)
    }

    pub(super) fn set_slider_value(&mut self, id: &str, value: f64) {
        if let Some(slider) = self.sliders.get_mut(id) {
            slider.value = value;
        }
        if let Some(layout) = self.pending.as_mut()
            && let Some(slider) = layout
                .ui_layout
                .sliders
                .iter_mut()
                .find(|slider| slider.id == id)
        {
            slider.value = value;
        }
    }

    pub(super) fn try_deliver(
        &mut self,
        emit: &mut impl FnMut(StudioUpdate) -> StudioUpdateSendResult,
    ) -> bool {
        let Some(layout) = self.pending.take() else {
            return false;
        };
        let generation = layout.ui_layout.generation;
        match emit(StudioUpdate::Layout(layout)) {
            Ok(()) => {
                self.delivered_generation = Some(generation);
                true
            }
            Err((_, StudioUpdate::Layout(layout))) => {
                self.pending = Some(layout);
                false
            }
            Err(_) => unreachable!("layout handoff returned a different update kind"),
        }
    }

    pub(super) fn ready_for(&self, generation: u64) -> bool {
        self.delivered_generation == Some(generation)
    }

    /// Whether the display has any layout at all, whichever generation it
    /// describes. Enough to place a widget and to ask for its audio.
    pub(super) fn delivered(&self) -> bool {
        self.delivered_generation.is_some()
    }
}

pub(super) struct StudioUiState {
    pub(super) layout: LayoutDelivery,
    generation_sources: BTreeMap<u64, (String, f64)>,
    pending_traces: HashMap<u64, ScheduleTraceEvent>,
    fresh_traces: Vec<ScheduleTraceEvent>,
    dropped: u64,
    next_prune: Duration,
    next_audio: Duration,
    next_preview: Duration,
    /// A layout has just been delivered: the next preview redraws the whole
    /// window from the evaluated score, behind the playhead as well as
    /// ahead, instead of reading on from where the audio's schedule ends.
    redraw_pending: bool,
    audio_interval: Duration,
    audio_enabled: bool,
    analyzer: Option<UiAudioAnalyzer>,
    visual_analyzers: Vec<Option<UiAudioAnalyzer>>,
    audio_samples: [f32; LIVE_ANALYSIS_WINDOW_SAMPLES],
    audio_sequence: u64,
    pub(super) visual_audio_mask: u64,
    visual_audio_revision: Option<String>,
}

impl StudioUiState {
    pub(super) fn new(audio_interval: Duration) -> Self {
        Self {
            layout: LayoutDelivery::default(),
            generation_sources: BTreeMap::new(),
            // Grows to what is pending, usually a few dozen. Reserving the
            // cap would hold 6 MB that playback touches page by page.
            pending_traces: HashMap::new(),
            fresh_traces: Vec::new(),
            dropped: 0,
            next_prune: Duration::ZERO,
            next_audio: Duration::ZERO,
            next_preview: Duration::ZERO,
            redraw_pending: false,
            audio_interval,
            audio_enabled: false,
            analyzer: None,
            visual_analyzers: (0..MAX_UI_LAYOUT_VISUALS).map(|_| None).collect(),
            audio_samples: [0.0; LIVE_ANALYSIS_WINDOW_SAMPLES],
            audio_sequence: 0,
            visual_audio_mask: 0,
            visual_audio_revision: None,
        }
    }

    pub(super) fn reset(&mut self) {
        let audio_interval = self.audio_interval;
        *self = Self::new(audio_interval);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn after_step(
        &mut self,
        session: &mut Session,
        device: &LiveScalarDevice,
        observed_at: Duration,
        step_succeeded: bool,
        stopping: bool,
        accepted_audio: Vec<UiAcceptedOnset>,
        diagnostics: &mut PendingDiagnostics,
        emit: &mut impl FnMut(StudioUpdate) -> StudioUpdateSendResult,
    ) {
        self.dropped = self
            .dropped
            .saturating_add(session.take_schedule_trace_events_dropped());
        self.fresh_traces.clear();
        session.drain_schedule_trace_events_into(&mut self.fresh_traces);

        let audible_generation = device.generation();
        let session_generation = session.generation();
        self.observe_layout(session, diagnostics, emit);
        // Two different questions, and conflating them is what would break the
        // note highlighting. `layout_ready` means the display has a layout at
        // all, and gates the widgets and the master analysis. `audible_layout`
        // means that layout describes the sounding generation, and gates
        // everything keyed by generation: correlating the old generation's
        // events against the new source's spans would light the wrong
        // characters, and a voice carries its widget's slot bit as numbered
        // by the generation that evaluated it.
        let layout_ready = self.layout.delivered();
        let audible_layout = self.layout.ready_for(audible_generation);
        // The footer's master scope wants the mix whenever anything is
        // audible, so analysis is no longer gated on a score asking for it.
        if layout_ready != self.audio_enabled {
            device.set_analysis_enabled(layout_ready);
            self.audio_enabled = layout_ready;
        }
        // The audition tap is always armed: a preview can sound the moment
        // the browser asks, whatever the score's own visuals are doing. The
        // score's own taps follow the audible layout: the reset below records
        // the device's CURRENT generation as the capture floor, and resetting
        // before the cutover would admit the old score's voices, under the
        // old slot numbering, into the new score's widgets.
        let score_visuals = if audible_layout {
            self.layout.visual_audio_mask
        } else {
            0
        };
        let visual_audio_mask = score_visuals | AUDITION_VISUALS;
        let visual_audio_revision = self.layout.source_revision.clone();
        if audible_layout && visual_audio_revision != self.visual_audio_revision {
            device.reset_visual_analysis_mask(visual_audio_mask);
            self.visual_audio_revision = visual_audio_revision;
            self.visual_audio_mask = visual_audio_mask;
            for analyzer in &mut self.visual_analyzers {
                *analyzer = None;
            }
        } else if visual_audio_mask != self.visual_audio_mask {
            device.set_visual_analysis_mask(visual_audio_mask);
            self.visual_audio_mask = visual_audio_mask;
        }
        let oldest_revision = audible_generation.saturating_sub(1);
        self.generation_sources
            .retain(|generation, _| *generation >= oldest_revision);

        ingest_pending_traces(
            &mut self.pending_traces,
            self.fresh_traces.drain(..),
            step_succeeded,
            audible_generation,
            MAX_PENDING_UI_TRACES,
            &mut self.dropped,
        );
        let correlation = correlate_submitted_traces(&mut self.pending_traces, accepted_audio);
        self.dropped = self.dropped.saturating_add(correlation.dropped);
        let ready = if audible_layout {
            correlation.ready
        } else {
            self.dropped = self.dropped.saturating_add(correlation.ready.len() as u64);
            Vec::new()
        };

        if self.audio_enabled && observed_at >= self.next_audio {
            self.next_audio = observed_at.saturating_add(self.audio_interval);
            // The frame is tagged with the generation of the layout on
            // screen, not the audible one: its slots are numbered by that
            // layout, and the display drops audio for a generation its
            // layout is not. Tagging the audible generation left the master
            // scope dark from the evaluate to the cutover.
            let layout_generation = self
                .layout
                .delivered_generation
                .unwrap_or(audible_generation);
            self.emit_audio(device, layout_generation, emit);
        }
        if observed_at >= self.next_prune {
            self.next_prune = observed_at.saturating_add(UI_TRACE_PRUNE_INTERVAL);
            prune_pending_traces(
                &mut self.pending_traces,
                audible_generation,
                device.clock_seconds(),
                STALE_UI_TRACE_GRACE_SECS,
                &mut self.dropped,
            );
        }

        let device_time = device.clock_seconds();
        let cycle = session.cycle_at_time(device_time);
        dispatch_trace_batches(
            device_time,
            cycle,
            audible_generation,
            ready,
            &self.generation_sources,
            &mut self.dropped,
            |request| match emit(StudioUpdate::Traces(request)) {
                Ok(()) => UiEventSendStatus::Queued,
                Err((status, _)) => status,
            },
        );

        // The preview for the painters: every interval, read ahead of the
        // audio from the score that the display's layout describes. The
        // device may not have taken that score yet, because a replacement
        // waits for its prefill. The display has taken it, and the display
        // judges the batch. A preview of the audible score would leave the
        // roll empty from the update to the cutover. A stop has no preview.
        if !stopping
            && self.layout.ready_for(session_generation)
            && !session.transport().is_stopped()
            && observed_at >= self.next_preview
        {
            self.next_preview = observed_at.saturating_add(PREVIEW_INTERVAL);
            let redraw = std::mem::take(&mut self.redraw_pending);
            if let Err(error) = self.emit_preview(
                session,
                device_time,
                cycle,
                session_generation,
                redraw,
                emit,
            ) {
                queue_diagnostic(diagnostics, StudioDiagnostic::runtime(&error, true));
            }
        }
    }

    /// Read the evaluated score's layout and hand it to the display: every
    /// producer turn, and each turn a start holds for its sounds, so its
    /// sliders exist before it sounds.
    pub(super) fn observe_layout(
        &mut self,
        session: &mut Session,
        diagnostics: &mut PendingDiagnostics,
        emit: &mut impl FnMut(StudioUpdate) -> StudioUpdateSendResult,
    ) {
        let session_generation = session.generation();
        // The layout comes from the evaluated score, not from the audible
        // one. They differ while a replacement waits for its first window to
        // prefill: one tick when the query goes through, longer when it
        // stays retryable, for example on a sample that is still loading.
        // Reading the evaluated score shows a new widget before the sound
        // swaps.
        //
        // Early placement is safe. The onset mapping and the slot taps in
        // `after_step` wait for the device to take that generation, and the
        // preview reads the same evaluated score. The widget is on screen
        // before its data, as upstream's is. The previous generation pays
        // the cost: the display holds one layout, so its onsets and clock
        // are not shown for the rest of the swap.
        if let Some(source) = session.active_source() {
            // The evaluated generation's source and cps, kept from the
            // moment it is evaluated: the previews drawn from it ahead of
            // the cutover carry them, as the traces do once the device has
            // taken it.
            self.generation_sources
                .entry(session_generation)
                .or_insert_with(|| (source_revision(source), session.cps()));
            match self.layout.observe(source, session_generation) {
                Ok(true) => self.layout.sync_slider_values(session),
                Ok(false) => {}
                Err(error) => queue_diagnostic(
                    diagnostics,
                    StudioDiagnostic::message("ui-layout", error.to_string()),
                ),
            }
        }

        if self.layout.try_deliver(emit) {
            // The display clears its picture for a new layout. Redraw it at
            // once from the evaluated score, the way strudel.cc re-queries
            // the pattern on evaluate: a colour changed in the score is on
            // the roll the moment the update lands, not a cycle later when
            // the audio's schedule reaches the new score.
            self.redraw_pending = true;
            self.next_preview = Duration::ZERO;
        }
    }

    /// One preview batch: the onsets from where the audio's schedule ends to
    /// `PREVIEW_LOOKAHEAD_CYCLES` past now. A redraw starts
    /// `PREVIEW_LOOKBEHIND_CYCLES` behind now instead. An empty batch tells
    /// the display to drop what it was showing from there on. Its clock is
    /// the same device time, cycle and cps a trace batch carries. A query
    /// the budget refuses, or a full channel, means no preview this time.
    /// The next interval tries again, and nothing here counts as a dropped
    /// trace. The only error returned is a `RuntimeError::Panic` from the
    /// query.
    fn emit_preview(
        &mut self,
        session: &mut Session,
        device_time: f64,
        cycle_now: f64,
        generation: u64,
        redraw: bool,
        emit: &mut impl FnMut(StudioUpdate) -> StudioUpdateSendResult,
    ) -> Result<(), rustel_runtime::RuntimeError> {
        let Some((source_revision, cps)) = self.generation_sources.get(&generation) else {
            return Ok(());
        };
        let from = if redraw {
            cycle_now - PREVIEW_LOOKBEHIND_CYCLES
        } else {
            session.scheduled_to_cycle()
        };
        let to = cycle_now + PREVIEW_LOOKAHEAD_CYCLES;
        let traces = match session.with_panic_recovery(device_time, |session| {
            session.preview_traces(from, to, generation)
        }) {
            Ok(traces) => traces,
            Err(error @ rustel_runtime::RuntimeError::Panic(_)) => {
                return Err(error);
            }
            Err(_) => return Ok(()),
        };
        let _ = emit(StudioUpdate::Traces(UiTraceBatchRequest {
            device_time,
            cycle: cycle_now,
            cps: *cps,
            generation,
            source_revision: source_revision.clone(),
            traces,
            dropped: 0,
            preview_from_cycle: Some(from),
        }));
        Ok(())
    }

    fn emit_audio(
        &mut self,
        device: &LiveScalarDevice,
        audible_generation: u64,
        emit: &mut impl FnMut(StudioUpdate) -> StudioUpdateSendResult,
    ) {
        let Some(snapshot) = device.copy_analysis_window(&mut self.audio_samples) else {
            return;
        };
        self.audio_sequence = self.audio_sequence.saturating_add(1);
        let master = self
            .analyzer
            .get_or_insert_with(UiAudioAnalyzer::new)
            .analyze(&self.audio_samples);
        let mut visuals = Vec::with_capacity(self.visual_audio_mask.count_ones() as usize);
        let mut slots = self.visual_audio_mask;
        while slots != 0 {
            let slot = slots.trailing_zeros() as u8;
            slots &= slots - 1;
            let Some(visual_snapshot) =
                device.copy_visual_analysis_window(slot, &mut self.audio_samples)
            else {
                continue;
            };
            if visual_snapshot.stream_id != snapshot.stream_id
                || visual_snapshot.end_frame != snapshot.end_frame
            {
                continue;
            }
            let analyzer =
                self.visual_analyzers[usize::from(slot)].get_or_insert_with(UiAudioAnalyzer::new);
            visuals.push((slot, analyzer.analyze(&self.audio_samples)));
        }
        let mut sides = [(0.0f32, 0.0f32); rustel_runtime::ui_analysis::UI_SIDES_SAMPLES];
        let sides = if device.copy_analysis_sides(&mut sides) {
            sides.to_vec()
        } else {
            Vec::new()
        };
        let device_time = snapshot.end_frame as f64 / f64::from(snapshot.sample_rate.max(1));
        let _ = emit(StudioUpdate::Audio {
            metadata: UiAudioMetadata {
                sequence: self.audio_sequence,
                generation: audible_generation,
                device_time,
                stream_id: snapshot.stream_id,
                epoch: snapshot.epoch,
                end_frame: snapshot.end_frame,
                sample_rate: snapshot.sample_rate,
            },
            analysis: Box::new(UiAudioAnalysisSet {
                master,
                visuals,
                sides,
            }),
        });
    }
}
