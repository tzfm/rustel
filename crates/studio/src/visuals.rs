//! Cell-native visual feedback for the terminal studio.
//!
//! Renderers write directly into Ratatui's buffer.  There are no child
//! terminals, progress bars, floating windows or subprocesses hidden behind
//! these views: the piano roll and scope are part of the same frame as the
//! source editor.
//!
//! Piano-roll notes use a block grid. Scopes, spectrum bars, spirals and
//! pitch wheels use a Braille grid for finer movement within each cell. Colour
//! and shape come from the active [`Theme`] and from the option object the
//! score wrote inside its `_visual(...)` call.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

use rustel_runtime::ui_analysis::{UI_SPECTRUM_BINS, UiAudioAnalysisFrame, UiAudioAnalysisSet};
use rustel_runtime::ui_events::{
    UiAudioMetadata, UiEventBatch, UiEventValidationError, UiLayout, UiLayoutEnvelope,
    UiScheduledEvent, UiTraceBatchRequest,
};
#[cfg(feature = "video")]
use rustel_runtime::video::VideoThumbnails;

use super::engine::StudioSnapshot;
use super::theme::{Theme, VisualOptions, hsv, luminance, mix, parse_color, true_rgb, wash};

const MAX_TIMELINE_EVENTS: usize = 2_048;
const HISTORY_SECONDS: f64 = 8.0;

#[derive(Clone, Debug)]
struct ClockAnchor {
    generation: u64,
    wall: Instant,
    device_time: f64,
    cycle: f64,
    cps: f64,
}

impl ClockAnchor {
    fn at(&self, wall: Instant) -> (f64, f64) {
        let elapsed = wall.saturating_duration_since(self.wall).as_secs_f64();
        (self.device_time + elapsed, self.cycle + elapsed * self.cps)
    }
}

/// Revision-gated visual state owned entirely by the UI thread.
#[derive(Debug, Default)]
pub struct VisualState {
    layout: Option<UiLayout>,
    running: bool,
    events: VecDeque<UiScheduledEvent>,
    onset_ids: HashSet<u64>,
    /// What the engine has looked ahead to but not queued yet: the notes a
    /// painter shows coming, replaced wholesale by every preview batch and
    /// retired one by one as the real traces arrive. Never a source
    /// highlight, never "sounding".
    previews: Vec<UiScheduledEvent>,
    /// The identities of the real events on the timeline, so a preview of
    /// an onset that has since really been queued is not drawn twice.
    real_keys: HashSet<OnsetKey>,
    clock: Option<ClockAnchor>,
    /// When playback stopped, while it is stopped: the clock holds there
    /// so the picture stays where it was.
    frozen_at: Option<Instant>,
    audio: Option<UiAudioAnalysisFrame>,
    /// The mix's newest frames as left and right, oldest first; empty
    /// until the device gives a stereo picture.
    sides: Vec<(f32, f32)>,
    visual_audio: BTreeMap<u8, UiAudioAnalysisFrame>,
    /// The audition tap's latest frame. The browser's, not the score's:
    /// it lands whatever generation or state the score is in, so the
    /// preview scope works on a stopped set too.
    audition_audio: Option<UiAudioAnalysisFrame>,
    /// The video clips the layout's painters name, keyed by the sound the
    /// score spells. A clip is asked for until it lands here with a strip
    /// or a reason it has none.
    #[cfg(feature = "video")]
    videos: BTreeMap<String, VideoEntry>,
    /// The spectrogram's memory: one column per audio frame, per tap
    /// (`None` is the master), newest last. strudel.cc's spectrum scrolls
    /// the last frames across the canvas; a terminal keeps them here.
    spectrograms: BTreeMap<Option<u8>, VecDeque<[f32; SPECTROGRAM_BANDS]>>,
    /// The analyser's memory per tap: each band's level after its decay,
    /// and the peak it is holding. Updated per audio frame, not per draw,
    /// so the fall is the same speed on every terminal.
    analysers: BTreeMap<Option<u8>, AnalyserBands>,
    /// The sample rate of the frames, for the analyser's frequency marks.
    audio_sample_rate: u32,
    audio_sequence: u64,
    dropped: u64,
}

/// How many times a video painter asks the library for a clip before it
/// says the picture never arrived. A clip downloads and decodes behind
/// its sound, so a first play can outlast a few polls.
#[cfg(feature = "video")]
const VIDEO_ASK_LIMIT: u8 = 100;

/// One named clip's picture, as a painter finds it.
#[cfg(feature = "video")]
#[derive(Clone, Debug)]
pub enum VideoEntry {
    /// Asked this many times; the library has no strip and no refusal yet.
    Asking(u8),
    /// The decoded strip.
    Ready(std::sync::Arc<VideoThumbnails>),
    /// Why there is no strip.
    Failed(String),
}

#[cfg(feature = "video")]
impl PartialEq for VideoEntry {
    fn eq(&self, other: &Self) -> bool {
        use std::sync::Arc;
        match (self, other) {
            (Self::Asking(asks), Self::Asking(other_asks)) => asks == other_asks,
            (Self::Ready(strip), Self::Ready(other_strip)) => Arc::ptr_eq(strip, other_strip),
            (Self::Failed(message), Self::Failed(other)) => message == other,
            _ => false,
        }
    }
}

/// The sound an event's chain carries: the `s:name` token with the `n:`
/// pick the same sound resolves with, spelled the way the library spells
/// a numbered variant. A name with a space in it stops at the space, the
/// way the chain text does; the painter then says no clip answered to it.
#[cfg(feature = "video")]
fn event_clip_name(event: &UiScheduledEvent) -> Option<String> {
    let value = event.value.as_deref()?;
    let sound = value.split_whitespace().find_map(|token| {
        let (key, spelled) = token.split_once(':')?;
        (key == "s").then_some(spelled)
    })?;
    // The pick the voice plays: `n(2)` in a bank of files is the third
    // one, the same as spelling `name:2`.
    if let Some(pick) = event_chain_number(event, "n")
        && pick.fract() == 0.0
    {
        return Some(format!("{sound}:{}", pick as i64));
    }
    Some(sound.to_owned())
}

/// One `key:number` link of an event's chain, as the number the control
/// holds.
#[cfg(feature = "video")]
fn event_chain_number(event: &UiScheduledEvent, key: &str) -> Option<f64> {
    event
        .value
        .as_deref()?
        .split_whitespace()
        .find_map(|token| {
            let (spelled_key, value) = token.split_once(':')?;
            (spelled_key == key)
                .then(|| value.parse::<f64>().ok())
                .flatten()
                .filter(|number| number.is_finite())
        })
}

/// Where a clip's sound is inside it, in seconds: the event's `begin` and
/// `speed` carry where the voice started and how fast it walks - and
/// `scrub` is begin under another name, so a scrubbed clip seeks its
/// picture. A looping clip wraps. A reversed clip walks to its end; a
/// live scrub that never reaches the event text moves the voice, not yet
/// the picture.
#[cfg(feature = "video")]
fn clip_position(event: &UiScheduledEvent, now: f64, duration: f64) -> f64 {
    let begin = event_chain_number(event, "begin")
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let speed = event_chain_number(event, "speed").unwrap_or(1.0);
    let looping = event_chain_number(event, "loop").unwrap_or(0.0) >= 1.0;
    if !now.is_finite() {
        return begin * duration;
    }
    let elapsed = (now - event.target_time).max(0.0);
    let advanced = if speed >= 0.0 {
        begin * duration + elapsed * speed
    } else {
        duration - elapsed * -speed
    };
    if looping && duration > 0.0 {
        advanced.rem_euclid(duration)
    } else {
        advanced.clamp(0.0, duration)
    }
}

/// The bit the display sets on a preview's onset id, so a preview is told
/// from a real event wherever one is looked at. The scheduler's ids count
/// up from zero and never reach it.
const PREVIEW_ONSET_FLAG: u64 = 1 << 63;

/// Whether an event on the timeline is a preview rather than a trace of
/// something queued to sound.
fn is_preview(event: &UiScheduledEvent) -> bool {
    event.onset_id & PREVIEW_ONSET_FLAG != 0
}

/// What makes an onset the same onset whether it arrived as a preview or,
/// later, as the real trace: its span, its value and where it was written.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct OnsetKey {
    whole_begin: String,
    whole_end: String,
    value: Option<String>,
    context: Vec<(usize, usize)>,
}

impl OnsetKey {
    fn of(event: &UiScheduledEvent) -> Self {
        Self {
            whole_begin: event.whole_begin.clone(),
            whole_end: event.whole_end.clone(),
            value: event.value.clone(),
            context: event.context.clone(),
        }
    }
}

/// Rows of frequency a spectrogram column carries, log-spaced from the
/// lowest bin to Nyquist the way strudel.cc's canvas places them.
pub const SPECTROGRAM_BANDS: usize = 64;
/// Columns kept per tap: enough audio frames to fill the widest pane at
/// one column a frame.
const SPECTROGRAM_COLUMNS: usize = 1024;

/// How far a band falls between audio frames once the sound has gone,
/// in dB - a bar drops from the top to nothing in about a second and a
/// half at the studio's thirty frames a second.
const ANALYSER_DECAY_DB_PER_FRAME: f32 = 2.0;
/// Frames a peak mark stays put before it starts to fall.
const ANALYSER_PEAK_HOLD_FRAMES: u8 = 15;
/// How far a released peak mark falls per frame, in dB.
const ANALYSER_PEAK_FALL_DB_PER_FRAME: f32 = 1.0;

/// The analyser's memory for one tap, in dB per linear FFT bin.
#[derive(Clone, Debug)]
pub struct AnalyserBands {
    pub levels: [f32; UI_SPECTRUM_BINS],
    pub peaks: [f32; UI_SPECTRUM_BINS],
    hold: [u8; UI_SPECTRUM_BINS],
}

impl Default for AnalyserBands {
    fn default() -> Self {
        Self {
            levels: [-120.0; UI_SPECTRUM_BINS],
            peaks: [-120.0; UI_SPECTRUM_BINS],
            hold: [0; UI_SPECTRUM_BINS],
        }
    }
}

impl AnalyserBands {
    /// Take a new frame in: a band rises at once to a louder reading and
    /// falls a fixed amount from a quieter one; its peak holds a moment,
    /// then follows.
    fn feed(&mut self, column: &[f32; UI_SPECTRUM_BINS]) {
        let bands = self
            .levels
            .iter_mut()
            .zip(self.peaks.iter_mut())
            .zip(self.hold.iter_mut().zip(column.iter()));
        for ((level, peak), (hold, &fresh)) in bands {
            *level = fresh.max(*level - ANALYSER_DECAY_DB_PER_FRAME);
            if *level >= *peak {
                *peak = *level;
                *hold = ANALYSER_PEAK_HOLD_FRAMES;
            } else if *hold > 0 {
                *hold -= 1;
            } else {
                *peak = (*peak - ANALYSER_PEAK_FALL_DB_PER_FRAME).max(*level);
            }
        }
    }
}

impl AnalyserBands {
    /// The bands of a single frame, with no history behind them. For a
    /// reader that holds one frame and keeps no decay state, such as the
    /// footer's small spectrum.
    pub fn of(frame: &UiAudioAnalysisFrame) -> Self {
        let mut bands = Self::default();
        bands.feed(&frame.spectrum);
        bands
    }
}

/// Pool linear bins into a logarithmic display column.
pub(super) fn spectrum_level(bins: &[f32], column: usize, columns: usize) -> f32 {
    if bins.is_empty() || columns == 0 || column >= columns {
        return -120.0;
    }
    let edge = |position: usize| {
        if position == columns {
            bins.len()
        } else {
            (((bins.len() + 1) as f32).powf(position as f32 / columns as f32) - 1.0).floor()
                as usize
        }
    };
    let first = edge(column).min(bins.len() - 1);
    let last = edge(column + 1).max(first + 1).min(bins.len());
    bins[first..last].iter().copied().fold(-120.0, f32::max)
}

fn spectrogram_column(frame: &UiAudioAnalysisFrame) -> [f32; SPECTROGRAM_BANDS] {
    std::array::from_fn(|band| spectrum_level(&frame.spectrum, band, SPECTROGRAM_BANDS))
}

impl VisualState {
    pub fn install_layout(&mut self, envelope: UiLayoutEnvelope) {
        let next = &envelope.ui_layout;
        let source_changed = self
            .layout
            .as_ref()
            .is_none_or(|current| current.source_revision != next.source_revision);
        let generation = next.generation;
        self.layout = Some(envelope.ui_layout);
        if source_changed {
            self.events.clear();
            self.onset_ids.clear();
            self.previews.clear();
            self.real_keys.clear();
            self.clock = None;
            // The analysers and their history belong to the score that was
            // playing. A different score is a different picture, so they
            // start again.
            self.audio = None;
            self.visual_audio.clear();
            self.spectrograms.clear();
            self.analysers.clear();
            self.audio_sequence = 0;
            // The clips the new score names are asked for again; the ones
            // the old score failed to send have their chance back, and a
            // strip that is still wanted comes straight from the library's
            // own table, decoded once.
            #[cfg(feature = "video")]
            self.videos.clear();
        } else {
            // The same score under a new generation - a slider moved, or the
            // output was recycled. The music did not change, so neither should
            // the picture: adopt the timeline and its clock into the new
            // generation rather than blanking the widgets until the next
            // trace batch arrives.
            for event in self.events.iter_mut().chain(self.previews.iter_mut()) {
                event.generation = generation;
            }
            if let Some(clock) = self.clock.as_mut() {
                clock.generation = generation;
            }
            // The analysers, their decay and peak-hold, and the spectrogram
            // history stay when the score has not changed. A dragged slider
            // re-queries about eight times a second.
        }
    }

    /// A slider moved. The score is the same one, so the picture is the
    /// same picture: the analysers and their history carry on rather than
    /// starting from nothing eight times a second while a fader is dragged.
    #[cfg(test)]
    pub(crate) fn analysers_are_kept_across_a_generation(&self) -> bool {
        !self.spectrograms.is_empty() || !self.analysers.is_empty() || self.audio.is_some()
    }

    pub fn layout(&self) -> Option<&UiLayout> {
        self.layout.as_ref()
    }

    /// Whether playback is running: false from a stop until the next start.
    pub fn running(&self) -> bool {
        self.running
    }

    /// Whether anything has sounded yet: running now, or stopped after
    /// having run. What tells an empty picture from a held one.
    pub fn sounded(&self) -> bool {
        self.running || self.frozen_at.is_some()
    }

    pub fn start(&mut self) {
        self.running = true;
        self.frozen_at = None;
        // The look-ahead the last set left on the frozen picture is not
        // this set's: the first snapshot brings the real one.
        self.previews.clear();
        self.real_keys.clear();
    }

    /// Stop-time: the picture stays where it was - the scope's last trace,
    /// the roll's last window with what was coming still on it, the
    /// spectrogram's history - the way a canvas on strudel.cc keeps its
    /// last frame when the scheduler stops. The clock freezes so nothing
    /// scrolls on, the marks in the code go out since nothing is sounding,
    /// and the audio sequence starts over for the next set. The previews
    /// stay on the picture; [`Self::start`] is what clears them.
    ///
    /// A canvas that is already stopped does not stop again: the second
    /// word of the engine would reset the audio sequence of the held
    /// picture, and the held frame would take new audio for a moment.
    pub fn stop(&mut self) {
        if !self.running {
            return;
        }
        self.running = false;
        // Repeated Stop acknowledgements must retain the first stop time.
        self.frozen_at.get_or_insert_with(Instant::now);
        self.audio_sequence = 0;
    }

    pub fn install_trace_request(
        &mut self,
        request: UiTraceBatchRequest,
    ) -> Result<bool, UiEventValidationError> {
        let batch = UiEventBatch::from_owned_traces(
            request.device_time,
            request.cycle,
            request.cps,
            request.generation,
            request.source_revision,
            request.traces,
            request.dropped,
        )?;
        Ok(match request.preview_from_cycle {
            Some(from) => self.install_preview(batch, from),
            None => self.install_batch(batch),
        })
    }

    /// Whether a batch may land on the timeline: the state is running and
    /// the batch is of the score on screen. An editor must never paint
    /// ranges from source that is not the source on screen.
    fn accepts(&self, batch: &UiEventBatch) -> bool {
        if !self.running {
            return false;
        }
        let Some(layout) = self.layout.as_ref() else {
            return false;
        };
        batch.generation == layout.generation && batch.source_revision == layout.source_revision
    }

    /// The engine's look ahead of the audio: the haps it has not queued yet,
    /// for the painters to show coming. A batch says what there is from
    /// `from_cycle` on, so it replaces the previews from there and leaves
    /// the ones behind it - a redraw's look behind the playhead, standing
    /// in for a trace that was never queued here - until they age out. An
    /// onset that has since really been queued is left out: the real trace
    /// stands for it. Returns false for a stale generation/revision.
    pub fn install_preview(&mut self, batch: UiEventBatch, from_cycle: f64) -> bool {
        if !self.accepts(&batch) {
            return false;
        }
        self.clock = Some(ClockAnchor {
            generation: batch.generation,
            wall: Instant::now(),
            device_time: batch.device_time,
            cycle: batch.cycle,
            cps: batch.cps,
        });
        self.previews
            .retain(|event| onset_cycle(event).is_some_and(|cycle| cycle < from_cycle));
        let keys = &self.real_keys;
        self.previews.extend(
            batch
                .events
                .into_iter()
                .filter(|event| !keys.contains(&OnsetKey::of(event)))
                .map(|mut event| {
                    event.onset_id |= PREVIEW_ONSET_FLAG;
                    event
                }),
        );
        true
    }

    /// Returns false for a stale generation/revision.  An editor must never
    /// paint ranges from source that is not the source on screen.
    pub fn install_batch(&mut self, batch: UiEventBatch) -> bool {
        if !self.accepts(&batch) {
            return false;
        }
        self.clock = Some(ClockAnchor {
            generation: batch.generation,
            wall: Instant::now(),
            device_time: batch.device_time,
            cycle: batch.cycle,
            cps: batch.cps,
        });
        self.dropped = self.dropped.saturating_add(batch.dropped);
        for event in batch.events {
            if self.onset_ids.insert(event.onset_id) {
                self.real_keys.insert(OnsetKey::of(&event));
                self.events.push_back(event);
            }
        }
        while self.events.len() > MAX_TIMELINE_EVENTS {
            self.evict_front();
        }
        self.prune();
        // The real trace of a previewed onset has arrived: the preview has
        // done its job.
        let keys = &self.real_keys;
        self.previews
            .retain(|preview| !keys.contains(&OnsetKey::of(preview)));
        true
    }

    /// Drop the oldest real event from the timeline.
    fn evict_front(&mut self) {
        if let Some(event) = self.events.pop_front() {
            self.onset_ids.remove(&event.onset_id);
            self.real_keys.remove(&OnsetKey::of(&event));
        }
    }

    /// Install the engine's clock even when the current score emitted no
    /// onsets. A snapshot is authoritative only after the staged session and
    /// the audio device agree on the audible generation and source revision.
    pub fn install_snapshot_clock(&mut self, snapshot: &StudioSnapshot) -> bool {
        self.install_snapshot_clock_at(snapshot, Instant::now())
    }

    fn install_snapshot_clock_at(&mut self, snapshot: &StudioSnapshot, wall: Instant) -> bool {
        if !self.running
            || !snapshot.playing
            || snapshot.audible_generation != Some(snapshot.session_generation)
            || !snapshot.device_time.is_finite()
            || !snapshot.cycle.is_finite()
            || !snapshot.cps.is_finite()
            || snapshot.cps <= 0.0
        {
            return false;
        }
        let Some(layout) = self.layout.as_ref() else {
            return false;
        };
        if layout.generation != snapshot.session_generation
            || snapshot.source_revision.as_deref() != Some(layout.source_revision.as_str())
        {
            return false;
        }
        self.clock = Some(ClockAnchor {
            generation: snapshot.session_generation,
            wall,
            device_time: snapshot.device_time,
            cycle: snapshot.cycle,
            cps: snapshot.cps,
        });
        self.prune_at(wall);
        true
    }

    pub fn install_audio(
        &mut self,
        metadata: UiAudioMetadata,
        analysis: UiAudioAnalysisSet,
    ) -> bool {
        if let Some((_, frame)) = analysis
            .visuals
            .iter()
            .find(|(slot, _)| *slot == super::engine::AUDITION_UI_VISUAL_SLOT)
        {
            self.audition_audio = Some(frame.clone());
        }
        if !self.running
            || self
                .layout
                .as_ref()
                .is_none_or(|layout| layout.generation != metadata.generation)
            || metadata.sequence <= self.audio_sequence
        {
            return false;
        }
        self.audio_sequence = metadata.sequence;
        self.audio_sample_rate = metadata.sample_rate;
        let mut remember = |tap: Option<u8>, frame: &UiAudioAnalysisFrame| {
            let column = spectrogram_column(frame);
            self.analysers.entry(tap).or_default().feed(&frame.spectrum);
            // Room for exactly the history kept, and the oldest column
            // out before the newest goes in: pushing first made a full ring
            // double to twice the columns it would ever hold, for every tap.
            let ring = self
                .spectrograms
                .entry(tap)
                .or_insert_with(|| VecDeque::with_capacity(SPECTROGRAM_COLUMNS));
            while ring.len() >= SPECTROGRAM_COLUMNS {
                ring.pop_front();
            }
            ring.push_back(column);
        };
        remember(None, &analysis.master);
        for (slot, frame) in &analysis.visuals {
            remember(Some(*slot), frame);
        }
        self.sides = analysis.sides;
        self.audio = Some(analysis.master);
        for (slot, frame) in analysis.visuals {
            self.visual_audio.insert(slot, frame);
        }
        true
    }

    /// The spectrogram columns a tap has accumulated, oldest first.
    pub fn spectrogram(&self, slot: Option<u8>) -> Option<&VecDeque<[f32; SPECTROGRAM_BANDS]>> {
        self.spectrograms.get(&slot)
    }

    /// What the spectrograms' histories hold: the room each tap's ring
    /// has taken, which a full history keeps until the score is reset.
    pub fn history_bytes(&self) -> usize {
        self.spectrograms
            .values()
            .map(|ring| ring.capacity() * std::mem::size_of::<[f32; SPECTROGRAM_BANDS]>())
            .sum()
    }

    /// The analyser's bands for a tap, after decay and peak hold.
    pub fn analyser(&self, slot: Option<u8>) -> Option<&AnalyserBands> {
        self.analysers.get(&slot)
    }

    /// The sample rate of the audio frames, or 48 kHz before the first.
    pub fn audio_sample_rate(&self) -> u32 {
        if self.audio_sample_rate == 0 {
            48_000
        } else {
            self.audio_sample_rate
        }
    }

    pub fn current_clock(&self) -> Option<(f64, f64, f64)> {
        self.clock_at(Instant::now())
    }

    /// The transport clock, allowed to keep moving while the visual canvas
    /// is held at its stop frame. During a graceful stop no new events are
    /// scheduled, but audible voices and effects are still draining; the
    /// header uses this clock until the engine reports fully stopped.
    pub fn current_transport_clock(&self) -> Option<(f64, f64, f64)> {
        self.transport_clock_at(Instant::now())
    }

    fn transport_clock_at(&self, wall: Instant) -> Option<(f64, f64, f64)> {
        let layout = self.layout.as_ref()?;
        self.clock
            .as_ref()
            .filter(|anchor| anchor.generation == layout.generation)
            .map(|anchor| {
                let (device_time, cycle) = anchor.at(wall);
                (device_time, cycle, anchor.cps)
            })
    }

    fn clock_at(&self, wall: Instant) -> Option<(f64, f64, f64)> {
        // Stopped: the moment it stopped, however long ago.
        let wall = self.frozen_at.map_or(wall, |frozen| frozen.min(wall));
        self.transport_clock_at(wall)
    }

    /// The mix's newest frames as left and right, oldest first, for the
    /// vectorscope; empty with no stereo picture.
    pub fn sides(&self) -> &[(f32, f32)] {
        &self.sides
    }

    /// An analyser column for the master, fed straight in, so a widget
    /// test has bands to draw without a device.
    #[cfg(test)]
    pub(crate) fn install_analyser_for_tests(&mut self, bins: [f32; UI_SPECTRUM_BINS]) {
        self.analysers.entry(None).or_default().feed(&bins);
        let column = std::array::from_fn(|band| spectrum_level(&bins, band, SPECTROGRAM_BANDS));
        self.spectrograms.entry(None).or_default().push_back(column);
    }

    pub fn audio(&self) -> Option<&UiAudioAnalysisFrame> {
        self.audio.as_ref()
    }

    pub fn audio_for(&self, slot: Option<u8>) -> Option<&UiAudioAnalysisFrame> {
        match slot {
            Some(slot) => self.visual_audio.get(&slot),
            None => self.audio(),
        }
    }

    /// The preview's own audio, from the reserved audition slot: what the
    /// samples browser is sounding, with none of the score in it.
    pub fn audition_audio(&self) -> Option<&UiAudioAnalysisFrame> {
        self.audition_audio.as_ref()
    }

    /// The strip a video painter's sound decoded, or why it has none.
    #[cfg(feature = "video")]
    pub fn video_for(&self, name: &str) -> Option<&VideoEntry> {
        self.videos.get(name)
    }

    /// The set's video clips by name, sorted: what the dock's video
    /// widget steps through with ←/→. Only clips asked for are listed;
    /// the poll asks for the painters' clips and, when a video widget is
    /// in a dock, every sound the set plays.
    #[cfg(feature = "video")]
    pub fn video_feeds(&self) -> Vec<String> {
        let mut feeds: Vec<String> = self.videos.keys().cloned().collect();
        feeds.sort();
        feeds
    }

    /// The strip a named feed is showing and the position inside it, in
    /// seconds, for the dock's video widget and the detached window: the
    /// event sounding now for that clip, else the last that sounded, else
    /// the next to come.
    #[cfg(feature = "video")]
    pub fn video_feed_frame(&self, feed: &str) -> Option<(f64, std::sync::Arc<VideoThumbnails>)> {
        let strip = match self.videos.get(feed) {
            Some(VideoEntry::Ready(strip)) => std::sync::Arc::clone(strip),
            _ => return None,
        };
        let (now, _, _) = self.current_clock().unwrap_or((f64::NAN, 0.0, 0.0));
        let mut active = None;
        let mut past = None;
        let mut future = None;
        for event in self.events() {
            if event_clip_name(event).as_deref() != Some(feed) {
                continue;
            }
            let end = event.target_time + event.duration_seconds.max(0.035);
            if !now.is_finite() {
                future.get_or_insert(event);
            } else if event.target_time <= now && now <= end {
                let newer = active
                    .is_none_or(|held: &UiScheduledEvent| held.target_time < event.target_time);
                if newer {
                    active = Some(event);
                }
            } else if event.target_time <= now {
                let later =
                    past.is_none_or(|held: &UiScheduledEvent| held.target_time < event.target_time);
                if later {
                    past = Some(event);
                }
            } else {
                let sooner = future
                    .is_none_or(|held: &UiScheduledEvent| held.target_time > event.target_time);
                if sooner {
                    future = Some(event);
                }
            }
        }
        let event = active.or(past).or(future);
        // With no event of its own, the clip shows its poster: the first
        // frame is what a docked widget or a window has before the score
        // reaches it.
        let position = event.map_or(0.0, |event| {
            clip_position(event, now, f64::from(strip.duration()))
        });
        Some((position, strip))
    }

    /// Remember a clip's decoded strip, or why it has none. Returns
    /// whether anything changed, so the caller knows to repaint.
    #[cfg(feature = "video")]
    pub fn install_video(&mut self, name: &str, entry: VideoEntry) -> bool {
        if self.videos.get(name) == Some(&entry) {
            return false;
        }
        self.videos.insert(name.to_owned(), entry);
        true
    }

    /// Count one ask for a clip the library has no answer for yet. A clip
    /// still downloading answers `None` or `Decoding` a while longer; past
    /// [`VIDEO_ASK_LIMIT`] asks the painter says so and stops asking.
    /// Returns whether the asking ran out, so the caller repaints.
    #[cfg(feature = "video")]
    pub fn note_video_ask(&mut self, name: &str) -> bool {
        let next = match self.videos.get(name) {
            Some(VideoEntry::Asking(asks)) if *asks >= VIDEO_ASK_LIMIT => {
                Some(VideoEntry::Failed("the picture did not arrive".to_owned()))
            }
            Some(VideoEntry::Asking(asks)) => Some(VideoEntry::Asking(asks + 1)),
            Some(_) => None,
            None => Some(VideoEntry::Asking(1)),
        };
        match next {
            Some(next) => {
                let ran_out = matches!(next, VideoEntry::Failed(_));
                self.videos.insert(name.to_owned(), next);
                ran_out
            }
            None => false,
        }
    }

    /// The sounds a video painter's events name and no strip is held for:
    /// what the next library ask is for. A stopped set keeps its strips,
    /// so this names nothing once every clip has landed.
    ///
    /// `every_sound` asks for whatever the set sounds, not only the
    /// painters' own lanes: the dock's video widget picks its feed from
    /// any of them.
    #[cfg(feature = "video")]
    pub fn video_names_to_ask(&self, every_sound: bool) -> Vec<String> {
        let Some(layout) = self.layout.as_ref() else {
            return Vec::new();
        };
        // An inline painter asks only for its own lane's clips. A stage
        // painter tags no hap, and the dock's widget names no lane, so
        // either asks for whatever the set sounds.
        let mut slot_mask = 0_u64;
        let mut stage = every_sound;
        for visual in &layout.visuals {
            if visual.kind != "video" {
                continue;
            }
            match visual.slot {
                Some(slot) => slot_mask |= 1_u64 << slot,
                None => stage = true,
            }
        }
        if slot_mask == 0 && !stage {
            return Vec::new();
        }
        let mut names: Vec<String> = Vec::new();
        let mut push = |event: &UiScheduledEvent| {
            let Some(name) = event_clip_name(event) else {
                return;
            };
            if !stage && event.ui_visuals & slot_mask == 0 {
                return;
            }
            if matches!(
                self.videos.get(name.as_str()),
                None | Some(VideoEntry::Asking(..=VIDEO_ASK_LIMIT))
            ) && !names.contains(&name)
            {
                names.push(name);
            }
        };
        for event in self.events.iter().chain(self.previews.iter()) {
            push(event);
        }
        names
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Source ranges of every event sounding right now. The fallback colour
    /// is used for events whose pattern named no colour of its own.
    pub fn active_marks(&self, fallback: Color) -> Vec<SourceMark> {
        self.fading_marks(fallback, 0.0)
    }

    /// The sounding events' ranges, and for `fade` seconds after each
    /// event ends its range still, at a strength easing from one to
    /// nothing: a mark that lets go rather than blinks off.
    pub fn fading_marks(&self, fallback: Color, fade: f64) -> Vec<SourceMark> {
        // Nothing sounds while stopped, so nothing is marked as sounding.
        if !self.running {
            return Vec::new();
        }
        let fade = if fade.is_finite() { fade.max(0.0) } else { 0.0 };
        let Some((now, _, _)) = self.current_clock() else {
            return Vec::new();
        };
        let Some(layout) = self.layout.as_ref() else {
            return Vec::new();
        };
        self.events
            .iter()
            .filter_map(|event| {
                let end = event.target_time + event.duration_seconds.max(0.035);
                if event.generation != layout.generation || event.target_time > now {
                    return None;
                }
                let strength = if now < end {
                    1.0
                } else if now < end + fade {
                    (1.0 - (now - end) / fade) as f32
                } else {
                    return None;
                };
                Some((event, strength))
            })
            .flat_map(|(event, strength)| {
                let color = event
                    .color
                    .as_deref()
                    .and_then(parse_color)
                    .unwrap_or(fallback);
                event.context.iter().map(move |&(from, to)| SourceMark {
                    from,
                    to,
                    color,
                    onset_id: event.onset_id,
                    strength,
                })
            })
            .collect()
    }

    /// Everything on the timeline: the traces of what has been queued to
    /// sound, then the previews of what is coming.
    pub fn events(&self) -> impl Iterator<Item = &UiScheduledEvent> {
        self.events.iter().chain(self.previews.iter())
    }

    fn prune(&mut self) {
        self.prune_at(Instant::now());
    }

    fn prune_at(&mut self, wall: Instant) {
        let Some((now, _, _)) = self.clock_at(wall) else {
            return;
        };
        while self
            .events
            .front()
            .is_some_and(|event| event.target_time + event.duration_seconds + HISTORY_SECONDS < now)
        {
            self.evict_front();
        }
        self.previews
            .retain(|event| event.target_time + event.duration_seconds + HISTORY_SECONDS >= now);
    }
}

/// The cycle an event starts on.
fn onset_cycle(event: &UiScheduledEvent) -> Option<f64> {
    parse_fraction(&event.whole_begin).or_else(|| parse_fraction(&event.part_begin))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceMark {
    pub from: usize,
    pub to: usize,
    pub color: Color,
    pub onset_id: u64,
    /// One while the event sounds, easing to nothing through the fade
    /// after it.
    pub strength: f32,
}

/// Which glyphs a canvas becomes: the drawing is the same in points, the
/// rasterisation follows the terminal's tier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Raster {
    /// 1×2 points per cell, `▀▄█`: filled areas, everywhere.
    HalfBlocks,
    /// 2×1 points per cell, `▌▐█`: finer timing in a single-lane roll.
    HorizontalHalfBlocks,
    /// 2×3 points per cell, the Unicode sextants: filled areas, finer.
    Sextants,
    /// 2×4 dots per cell, Braille: lines and dots, everywhere.
    Braille,
    /// Real pixels, this many per cell, shipped as an image.
    Pixels { cell_width: u16, cell_height: u16 },
}

impl Raster {
    /// This raster, coarsened if drawing `area` with it would cost more
    /// points than a frame can afford.
    ///
    /// On the pixel raster a point is one screen pixel, so a painter over
    /// the editor of a large window is several million points. Every frame
    /// rasterises them, converts them to RGBA, compresses them and writes
    /// them to the terminal. Past the budget the raster coarsens and the
    /// terminal scales the picture into the cells it named. The picture is
    /// a little softer, and the frame arrives in time. The budget is the
    /// one the renderer measured, so a terminal that keeps up keeps the
    /// fine raster. Glyph rasters are already coarse and come back
    /// unchanged.
    pub(super) fn within_budget(self, area: Rect) -> Self {
        let budget = super::graphics::canvas_budget();
        let Self::Pixels {
            cell_width,
            cell_height,
        } = self
        else {
            return self;
        };
        let points = u64::from(area.width)
            * u64::from(cell_width)
            * u64::from(area.height)
            * u64::from(cell_height);
        if points <= budget {
            return self;
        }
        let scale = (budget as f64 / points as f64).sqrt();
        let shrink = |side: u16| ((f64::from(side) * scale) as u16).max(1);
        Self::Pixels {
            cell_width: shrink(cell_width),
            cell_height: shrink(cell_height),
        }
    }

    pub fn points_per_cell(self) -> (usize, usize) {
        match self {
            Self::HalfBlocks => (1, 2),
            Self::HorizontalHalfBlocks => (2, 1),
            Self::Sextants => (2, 3),
            Self::Braille => (2, 4),
            Self::Pixels {
                cell_width,
                cell_height,
            } => (
                usize::from(cell_width.max(1)),
                usize::from(cell_height.max(1)),
            ),
        }
    }
}

/// A canvas of points over an area of cells. Painters draw in points -
/// `width()` × `height()` of them - and never learn what a point is; the
/// tier chose that when the canvas was made. A filled canvas keeps every
/// point's colour; the glyph rasters reduce a cell to what they can show.
pub struct Canvas {
    area: Rect,
    raster: Raster,
    columns: usize,
    rows: usize,
    /// One entry per point, row-major.
    points: Vec<Option<Color>>,
}

/// The half-block canvas by its old name, for painters that fill areas.
pub type BlockGrid = Canvas;
/// The Braille canvas by its old name, for painters that draw lines.
pub type BrailleGrid = Canvas;

/// Braille dot bit for a point inside a cell: column 0 or 1, row 0..4.
pub(super) const BRAILLE_BITS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// Ordered thresholds for the eight dots of a Braille cell, eighths. A dot
/// lights when the level it stands for passes its own threshold, so the
/// dot count follows the level and two neighbouring cells at the same level
/// do not light identically: shading, out of glyphs that have none of their
/// own. Shared, so two Braille pictures on one screen dither alike.
pub(super) const BRAILLE_DITHER: [[u8; 2]; 4] = [[0, 4], [6, 2], [1, 5], [7, 3]];

/// Whether the dot at `(dx, dy)` of a Braille cell lights for `level`,
/// 0 through 1.
pub(super) fn braille_dot(level: f32, dx: usize, dy: usize) -> bool {
    level * 8.0 > f32::from(BRAILLE_DITHER[dy][dx]) + 0.5
}

/// A 2×4 patch of luminance as one Braille glyph, indexed `[dy][dx]`.
/// Empty is `⠀` and full is `⣿`; everything between is a dot count, which
/// is the only shading a glyph raster has.
pub(super) fn braille_luma_glyph(patch: [[u8; 2]; 4]) -> char {
    let mut bits = 0u8;
    for (dy, row) in patch.iter().enumerate() {
        for (dx, level) in row.iter().enumerate() {
            if braille_dot(f32::from(*level) / 255.0, dx, dy) {
                bits |= BRAILLE_BITS[dx][dy];
            }
        }
    }
    char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' ')
}

impl Canvas {
    /// A canvas for filled shapes - bars, discs - at the current tier.
    pub fn bars(area: Rect) -> Self {
        let raster = match super::graphics::tier() {
            super::graphics::Tier::Cells => Raster::HalfBlocks,
            super::graphics::Tier::Fine => Raster::Sextants,
            super::graphics::Tier::Pixels => pixel_raster().unwrap_or(Raster::Sextants),
        };
        Self::with_raster(area, raster.within_budget(area))
    }

    /// A canvas for lines and dots at the current tier: Braille's 2x4 dots
    /// on both glyph tiers, an image on Pixels. Quadrant blocks give only
    /// two vertical levels per row, which turns a one-row scope into a
    /// toothed bar; Braille gives four. The glyph tiers differ in their
    /// bars.
    pub fn lines(area: Rect) -> Self {
        let raster = match super::graphics::tier() {
            super::graphics::Tier::Cells | super::graphics::Tier::Fine => Raster::Braille,
            super::graphics::Tier::Pixels => pixel_raster().unwrap_or(Raster::Braille),
        };
        Self::with_raster(area, raster.within_budget(area))
    }

    /// The bars canvas. `BlockGrid::new` and `BrailleGrid::new` both resolve
    /// here, so line painters call [`Canvas::lines`].
    pub fn new(area: Rect) -> Self {
        Self::bars(area)
    }

    /// A canvas on exactly the raster asked for. [`Canvas::bars`] and
    /// [`Canvas::lines`] bound theirs first; a caller that names its own
    /// raster - the minimap, whose whole geometry is one point per screen
    /// pixel - gets what it named.
    pub fn with_raster(area: Rect, raster: Raster) -> Self {
        let (across, down) = raster.points_per_cell();
        let columns = usize::from(area.width) * across;
        let rows = usize::from(area.height) * down;
        Self {
            area,
            raster,
            columns,
            rows,
            points: vec![None; columns * rows],
        }
    }

    pub fn raster(&self) -> Raster {
        self.raster
    }

    pub fn width(&self) -> usize {
        self.columns
    }

    pub fn height(&self) -> usize {
        self.rows
    }

    pub fn set(&mut self, x: usize, y: usize, color: Color) {
        if x >= self.columns || y >= self.rows {
            return;
        }
        self.points[y * self.columns + x] = Some(color);
    }

    pub fn get(&self, x: usize, y: usize) -> Option<Color> {
        if x >= self.columns || y >= self.rows {
            return None;
        }
        self.points[y * self.columns + x]
    }

    /// Fill an inclusive vertical span in one column.
    pub fn column(&mut self, x: usize, from: usize, to: usize, color: Color) {
        for y in from.min(to)..=from.max(to) {
            self.set(x, y, color);
        }
    }

    /// A straight line between two points, so a waveform or a spiral has no
    /// gaps where it moves faster than one point per column.
    pub fn line(&mut self, from: (isize, isize), to: (isize, isize), color: Color) {
        bresenham(from, to, |x, y| self.set(x, y, color));
    }

    /// A filled disc, for a note on the pitch wheel or a marker.
    pub fn disc(&mut self, centre: (isize, isize), radius: isize, color: Color) {
        let radius = radius.max(0);
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dy * dy <= radius * radius {
                    let (x, y) = (centre.0 + dx, centre.1 + dy);
                    if x >= 0 && y >= 0 {
                        self.set(x as usize, y as usize, color);
                    }
                }
            }
        }
    }

    /// Write the canvas into a Ratatui buffer over `background`. Cells with
    /// nothing drawn are left untouched so text drawn beforehand survives.
    /// A pixel canvas leaves the cells alone and hands its image to the
    /// frame instead.
    pub fn paint(self, buffer: &mut Buffer, background: Color) {
        match self.raster {
            Raster::HalfBlocks => self.paint_half_blocks(buffer, background),
            Raster::HorizontalHalfBlocks => self.paint_horizontal_half_blocks(buffer, background),
            Raster::Sextants => self.paint_sextants(buffer, background),
            Raster::Braille => self.paint_braille(buffer, background),
            Raster::Pixels { .. } => self.paint_pixels(),
        }
    }

    fn paint_half_blocks(self, buffer: &mut Buffer, background: Color) {
        let width = self.columns;
        for row in 0..usize::from(self.area.height) {
            for column in 0..width {
                let top = self.points[row * 2 * width + column];
                let bottom = self.points[(row * 2 + 1) * width + column];
                let (symbol, style) = match (top, bottom) {
                    (None, None) => continue,
                    (Some(top), Some(bottom)) if top == bottom => ("█", Style::default().fg(top)),
                    (Some(top), Some(bottom)) => ("▀", Style::default().fg(top).bg(bottom)),
                    (Some(top), None) => ("▀", Style::default().fg(top).bg(background)),
                    (None, Some(bottom)) => ("▄", Style::default().fg(bottom).bg(background)),
                };
                let position = (self.area.x + column as u16, self.area.y + row as u16);
                if let Some(cell) = buffer.cell_mut(position) {
                    cell.set_symbol(symbol).set_style(style);
                }
            }
        }
    }

    fn paint_horizontal_half_blocks(self, buffer: &mut Buffer, background: Color) {
        for row in 0..usize::from(self.area.height) {
            for column in 0..usize::from(self.area.width) {
                let index = row * self.columns + column * 2;
                let (symbol, foreground, back) = match (self.points[index], self.points[index + 1])
                {
                    (None, None) => continue,
                    (Some(left), Some(right)) if left == right => ("█", left, background),
                    (Some(left), Some(right)) => ("▌", left, right),
                    (Some(left), None) => ("▌", left, background),
                    (None, Some(right)) => ("▐", right, background),
                };
                if let Some(cell) =
                    buffer.cell_mut((self.area.x + column as u16, self.area.y + row as u16))
                {
                    cell.set_symbol(symbol)
                        .set_style(Style::default().fg(foreground).bg(back));
                }
            }
        }
    }

    fn paint_sextants(self, buffer: &mut Buffer, background: Color) {
        let width = self.columns;
        for row in 0..usize::from(self.area.height) {
            for cell_x in 0..usize::from(self.area.width) {
                let mut bits = 0u8;
                let mut colors: Vec<(Color, usize)> = Vec::new();
                for dy in 0..3 {
                    for dx in 0..2 {
                        let point = self.points[(row * 3 + dy) * width + cell_x * 2 + dx];
                        if let Some(color) = point {
                            bits |= 1 << (dy * 2 + dx);
                            match colors.iter_mut().find(|(c, _)| *c == color) {
                                Some(entry) => entry.1 += 1,
                                None => colors.push((color, 1)),
                            }
                        }
                    }
                }
                if bits == 0 {
                    continue;
                }
                let color = colors
                    .iter()
                    .max_by_key(|(_, count)| *count)
                    .map(|(color, _)| *color)
                    .unwrap_or(background);
                let position = (self.area.x + cell_x as u16, self.area.y + row as u16);
                if let Some(cell) = buffer.cell_mut(position) {
                    cell.set_char(sextant(bits))
                        .set_style(Style::default().fg(color).bg(background));
                }
            }
        }
    }

    #[allow(clippy::needless_range_loop)]
    fn paint_braille(self, buffer: &mut Buffer, background: Color) {
        let width = self.columns;
        for row in 0..usize::from(self.area.height) {
            for cell_x in 0..usize::from(self.area.width) {
                let mut bits = 0u8;
                let mut color = None;
                for dy in 0..4 {
                    for dx in 0..2 {
                        if let Some(point) = self.points[(row * 4 + dy) * width + cell_x * 2 + dx] {
                            bits |= BRAILLE_BITS[dx][dy];
                            color = Some(point);
                        }
                    }
                }
                if bits == 0 {
                    continue;
                }
                let position = (self.area.x + cell_x as u16, self.area.y + row as u16);
                let symbol = char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' ');
                if let Some(cell) = buffer.cell_mut(position) {
                    cell.set_char(symbol)
                        .set_style(Style::default().fg(color.unwrap_or(background)));
                }
            }
        }
    }

    fn paint_pixels(self) {
        let mut rgba = Vec::with_capacity(self.points.len() * 4);
        for point in &self.points {
            match point {
                Some(color) => {
                    let (r, g, b) = super::graphics::rgb(*color);
                    rgba.extend_from_slice(&[r, g, b, 255]);
                }
                None => rgba.extend_from_slice(&[0, 0, 0, 0]),
            }
        }
        // The picture names the cells it covers rather than trusting its
        // own size to land right. At one point a screen pixel the two are
        // the same thing; when the raster has been coarsened to keep the
        // frame affordable, or when the terminal misreports how big a cell
        // is, this is what still puts the picture exactly where the widget
        // is instead of a fraction of the way across it.
        super::graphics::push_image(super::graphics::PixelImage {
            cells: Some((self.area.width, self.area.height)),
            ..super::graphics::PixelImage::inline(
                self.area,
                self.columns as u32,
                self.rows as u32,
                rgba,
            )
        });
    }
}

/// The pixel raster for this terminal, when its cell size is known.
fn pixel_raster() -> Option<Raster> {
    super::graphics::cell_pixels().map(|(cell_width, cell_height)| Raster::Pixels {
        cell_width,
        cell_height,
    })
}

/// The sextant glyph for six point bits (column 0/1 × row 0..3, bit
/// `row * 2 + column`). The Unicode block leaves out the four patterns that
/// already exist as space, `▌`, `▐` and `█`.
pub fn sextant(bits: u8) -> char {
    match bits {
        0 => ' ',
        0b010101 => '▌',
        0b101010 => '▐',
        0b111111 => '█',
        n => {
            let mut index = u32::from(n) - 1;
            if n > 0b010101 {
                index -= 1;
            }
            if n > 0b101010 {
                index -= 1;
            }
            char::from_u32(0x1FB00 + index).unwrap_or('█')
        }
    }
}

fn bresenham(from: (isize, isize), to: (isize, isize), mut plot: impl FnMut(usize, usize)) {
    let (mut x, mut y) = from;
    let (target_x, target_y) = to;
    let step_x = (target_x - x).signum();
    let step_y = (target_y - y).signum();
    let span_x = (target_x - x).abs();
    let span_y = -(target_y - y).abs();
    let mut error = span_x + span_y;
    loop {
        if x >= 0 && y >= 0 {
            plot(x as usize, y as usize);
        }
        if x == target_x && y == target_y {
            break;
        }
        let doubled = error * 2;
        if doubled >= span_y {
            error += span_y;
            x += step_x;
        }
        if doubled <= span_x {
            error += span_x;
            y += step_y;
        }
    }
}

/// Everything a renderer needs: the live state, the palette, and the options
/// the score wrote inside its `_visual(...)` call.
pub struct VisualRequest<'a> {
    pub kind: &'a str,
    pub slot: Option<u8>,
    pub options: &'a VisualOptions,
    pub state: &'a VisualState,
    pub theme: &'a Theme,
    /// Pane colour behind the widget, used where only half a cell is lit.
    pub background: Color,
    /// Whether the painter draws in its own band of rows rather than on the
    /// stage behind the score. Only an inline painter writes a note when it
    /// has nothing to draw.
    pub inline: bool,
}

impl VisualRequest<'_> {
    /// The colour a pattern asked for, if it asked for one. Everything else
    /// is a renderer default, which is what makes `active`/`inactive` options
    /// and the theme meaningful.
    fn event_color(&self, event: &UiScheduledEvent) -> Option<Color> {
        event.color.as_deref().and_then(parse_color)
    }
}

fn widget_color(request: &VisualRequest<'_>, option: Option<Color>) -> Color {
    widget_color_override(request, option).unwrap_or(request.theme.accent)
}

/// The pattern's `.color()` takes precedence over the widget's `color` option.
fn widget_color_override(request: &VisualRequest<'_>, option: Option<Color>) -> Option<Color> {
    request
        .state
        .events()
        .filter(|event| {
            request
                .slot
                .is_none_or(|slot| event.ui_visuals & (1_u64 << slot) != 0)
        })
        .filter_map(|event| request.event_color(event))
        .last()
        .or(option)
}

/// strudel.cc draws a hap at `velocity * gain` alpha. The event's gain is
/// the voice's, superdough's 0.8 default included, so it is read against
/// that default: a note nobody turned down is drawn in full, a quiet one
/// faintly - never invisibly.
fn gain_alpha(event: &UiScheduledEvent) -> f32 {
    event.gain.map_or(1.0, |gain| (gain / 0.8).clamp(0.25, 1.0))
}

/// Draw any supported visualization. Unknown kinds say so rather than
/// leaving an unexplained blank rectangle.
pub fn render(request: VisualRequest<'_>, area: Rect, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    match request.kind {
        "pianoroll" | "punchcard" => {
            PianoRoll::new(&request, RollPreset::PianoRoll).render(area, buffer)
        }
        "wordfall" => PianoRoll::new(&request, RollPreset::Wordfall).render(area, buffer),
        "spiral" => Spiral::new(&request).render(area, buffer),
        "pitchwheel" => PitchWheel::new(&request).render(area, buffer),
        "scope" | "tscope" => Scope::new(&request).render(area, buffer),
        "spectrum" => Spectrum::new(&request).render(area, buffer),
        #[cfg(feature = "video")]
        "video" => Video::new(&request).render(area, buffer),
        kind => note(
            &request,
            buffer,
            area,
            &format!("no terminal renderer for _{kind}()"),
        ),
    }
}

/// Say in the painter's own band why it drew nothing. A stage painter
/// writes nothing: its note would sit behind the score.
fn note(request: &VisualRequest<'_>, buffer: &mut Buffer, area: Rect, message: &str) {
    if !request.inline {
        return;
    }
    buffer.set_stringn(
        area.x,
        area.y,
        message,
        usize::from(area.width),
        Style::default()
            .fg(request.theme.muted)
            .add_modifier(Modifier::ITALIC),
    );
}

/// One event placed on a time axis, in cycles.
struct PlacedEvent<'a> {
    event: &'a UiScheduledEvent,
    begin: f64,
    end: f64,
    active: bool,
}

/// Events overlapping `[begin, end)` on the cycle timeline.
fn placed_events<'a>(
    state: &'a VisualState,
    slot: Option<u8>,
    begin: f64,
    end: f64,
    now_seconds: f64,
    cps: f64,
) -> Vec<PlacedEvent<'a>> {
    state
        .events()
        .filter(|event| slot.is_none_or(|slot| event.ui_visuals & (1_u64 << slot) != 0))
        .filter_map(|event| {
            let event_begin =
                parse_fraction(&event.whole_begin).or_else(|| parse_fraction(&event.part_begin))?;
            let event_end = parse_fraction(&event.whole_end)
                .or_else(|| parse_fraction(&event.part_end))
                .unwrap_or(event_begin + event.duration_seconds * cps);
            (event_end >= begin && event_begin <= end).then(|| PlacedEvent {
                event,
                begin: event_begin,
                end: event_end.max(event_begin),
                active: is_active(event, now_seconds),
            })
        })
        .collect()
}

/// Whether an event is sounding now. A preview never is: by the time its
/// onset sounds, the real trace stands for it.
fn is_active(event: &UiScheduledEvent, now_seconds: f64) -> bool {
    !is_preview(event)
        && event.target_time <= now_seconds
        && now_seconds < event.target_time + event.duration_seconds.max(MIN_ACTIVE_SECONDS)
}

/// Shortest time an onset stays lit, so a percussive hit is still visible at
/// sixty frames a second.
const MIN_ACTIVE_SECONDS: f64 = 0.035;

/// The text on a labelled note: its `note`, else its sound with
/// `:n` when there is one - never the whole control dump.
fn short_value(event: &UiScheduledEvent) -> String {
    if let Some(label) = event.label.as_ref().filter(|label| !label.is_empty()) {
        return label.clone();
    }
    let value = event.value.as_deref().unwrap_or_default();
    let field = |key: &str| {
        value
            .split_whitespace()
            .find_map(|token| token.strip_prefix(key).map(str::to_owned))
    };
    if let Some(note) = field("note:") {
        return note;
    }
    if let Some(sound) = field("s:") {
        return match field("n:").filter(|n| n != "0" && !n.is_empty()) {
            Some(n) => format!("{sound}:{n}"),
            None => sound,
        };
    }
    value.chars().take(12).collect()
}

/// Row identity for the piano roll: a pitch when the event has one, and the
/// value text otherwise, so a drum pattern gets one lane per sound.
#[derive(Clone, Debug, PartialEq)]
enum Lane {
    Pitch(f32),
    Named(String),
}

impl Lane {
    fn of(event: &UiScheduledEvent) -> Self {
        // The audio route supplies a synth's default frequency after its
        // preview was drawn. That is not a pitch written in the pattern:
        // s("sine") must keep the same named lane before and after onset.
        if let Some(value) = event.value.as_deref() {
            let sound = value
                .split_whitespace()
                .any(|field| field.starts_with("s:"));
            let pitch = value
                .split_whitespace()
                .any(|field| field.starts_with("note:") || field.starts_with("freq:"));
            if sound && !pitch {
                return Self::Named(short_value(event));
            }
        }
        match event.frequency_hz.filter(|hertz| hertz.is_finite()) {
            Some(hertz) => Self::Pitch(frequency_to_midi(hertz)),
            None => Self::Named(short_value(event)),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Pitch(midi) => note_name(*midi),
            Self::Named(name) => name.clone(),
        }
    }

    /// Total order over lanes: named sounds first in name order, then
    /// pitches ascending. `f32` has no `Ord`, so the comparison is spelled
    /// out rather than derived.
    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Named(left), Self::Named(right)) => left.cmp(right),
            (Self::Named(_), Self::Pitch(_)) => std::cmp::Ordering::Less,
            (Self::Pitch(_), Self::Named(_)) => std::cmp::Ordering::Greater,
            (Self::Pitch(left), Self::Pitch(right)) => left.total_cmp(right),
        }
    }
}

/// The piano roll, its `punchcard` twin and the vertical `wordfall`.
///
/// One renderer, as on strudel.cc (`__pianoroll`): time runs along one axis
/// over `cycles` cycles with the playhead pinned at `playhead`, and the
/// score glides underneath it; values occupy lanes across the other axis -
/// one lane per value on screen (`fold: 1`), or a pitch axis (`fold: 0`).
/// `vertical: 1` turns it on its side, time falling down the pane, which is
/// what `wordfall` is.
struct PianoRoll<'a, 'b> {
    request: &'a VisualRequest<'b>,
    cycles: f64,
    playhead: f64,
    labels: bool,
    fold: bool,
    vertical: bool,
    flip_time: bool,
    flip_values: bool,
    hide_inactive: bool,
    background: Color,
    playhead_color: Color,
    active_color: Option<Color>,
    inactive_color: Option<Color>,
    colorize_inactive: bool,
}

/// Which spelling asked for the roll. `wordfall` is strudel.cc's
/// `punchcard({ vertical: 1, labels: 1, stroke: 0, fillActive: 1, active:
/// 'white' })`: the same painter with those folded in as defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RollPreset {
    PianoRoll,
    Wordfall,
}

impl<'a, 'b> PianoRoll<'a, 'b> {
    fn new(request: &'a VisualRequest<'b>, preset: RollPreset) -> Self {
        let options = request.options;
        let theme = request.theme;
        let wordfall = preset == RollPreset::Wordfall;
        Self {
            request,
            cycles: options.number("cycles").unwrap_or(4.0).clamp(0.25, 64.0),
            playhead: options.number("playhead").unwrap_or(0.5).clamp(0.0, 1.0),
            labels: options.flag("labels").unwrap_or(wordfall),
            fold: options.flag("fold").unwrap_or(true),
            vertical: options.flag("vertical").unwrap_or(wordfall),
            flip_time: options.flag("fliptime").unwrap_or(false),
            flip_values: options.flag("flipvalues").unwrap_or(false),
            hide_inactive: options.flag("hideinactive").unwrap_or(false),
            background: options.color("background").unwrap_or(request.background),
            playhead_color: options.color("playheadcolor").unwrap_or(theme.playhead),
            active_color: options.color("active").or(wordfall.then_some(Color::White)),
            inactive_color: options.color("inactive"),
            colorize_inactive: options.flag("colorizeinactive").unwrap_or(true),
        }
    }

    /// The colour policy: a pattern's own `.color()` wins, otherwise
    /// sounding events take the `active` colour and the rest the `inactive`
    /// one. `colorizeInactive: 0` forces every silent event to the same
    /// inactive colour even when its pattern named one. strudel.cc then
    /// draws the bar at `velocity * gain` alpha; a quiet note is a faint one
    /// here too - never an invisible one.
    fn color_of(&self, placed: &PlacedEvent<'_>) -> Color {
        let theme = self.request.theme;
        let named = self.request.event_color(placed.event);
        let colour = if placed.active {
            brighten(named.or(self.active_color).unwrap_or(theme.event))
        } else {
            let base = if self.colorize_inactive { named } else { None };
            dim(
                base.or(self.inactive_color).unwrap_or(theme.event_inactive),
                theme,
            )
        };
        let alpha = gain_alpha(placed.event);
        if alpha >= 0.999 {
            colour
        } else {
            wash(self.background, colour, alpha)
        }
    }
}

/// A label waiting for the bars to be painted: where it starts, in cells,
/// and which cells of its row the bar itself covers.
struct RollLabel {
    cell_x: usize,
    row: usize,
    bar_cells: std::ops::Range<usize>,
    text: String,
    color: Color,
}

impl Widget for PianoRoll<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let state = self.request.state;
        let theme = self.request.theme;
        let options = self.request.options;
        let Some((now_seconds, cycle_now, cps)) = state.current_clock() else {
            note(self.request, buffer, area, "waiting for the first beat");
            return;
        };
        if area.is_empty() {
            return;
        }
        let begin = cycle_now - self.cycles * self.playhead;
        let end = begin + self.cycles;
        let placed = placed_events(state, self.request.slot, begin, end, now_seconds, cps);
        let mut lanes = placed
            .iter()
            .map(|entry| Lane::of(entry.event))
            .collect::<Vec<_>>();
        lanes.sort_by(Lane::compare);
        lanes.dedup();
        let lane_count = lanes.len().max(1);

        // With only one lane, spend the basic block glyphs' extra half-cell
        // on time instead of pitch. These are old Block Elements, supported
        // by console fonts that cannot draw quadrants, sextants or Braille.
        // Multiple lanes keep their existing half-row pitch resolution.
        let mut grid = if !self.vertical
            && lane_count == 1
            && super::graphics::tier() == super::graphics::Tier::Cells
        {
            Canvas::with_raster(area, Raster::HorizontalHalfBlocks)
        } else {
            Canvas::bars(area)
        };
        let (width, height) = (grid.width(), grid.height());
        let pixel_timing = matches!(grid.raster(), Raster::Pixels { .. });
        let glyph_timing = !self.vertical && lane_count == 1 && !pixel_timing;
        let (points_across, points_down) = grid.raster().points_per_cell();
        let (points_across, points_down) = (points_across.max(1), points_down.max(1));
        let vertical = self.vertical;
        // The two axes, in points: time runs along one, values across the
        // other.
        let (time_len, value_len) = if vertical {
            (height, width)
        } else {
            (width, height)
        };
        // A cycle's place along the time axis: left to right, or - on its
        // side - falling from the top, as on strudel.cc. `flipTime` turns
        // either around.
        let time_point = |cycle: f64| -> isize {
            let t = ((cycle - begin) / self.cycles * time_len as f64).round() as isize;
            if vertical != self.flip_time {
                time_len as isize - 1 - t
            } else {
                t
            }
        };
        // A value's place across the value axis, from its height above the
        // lowest: bottom up, or - on its side - left to right. `flipValues`
        // turns either around.
        let value_point = |from_low: usize| -> usize {
            if vertical == self.flip_values {
                value_len.saturating_sub(1 + from_low)
            } else {
                from_low.min(value_len.saturating_sub(1))
            }
        };

        // Without folding the pitched range is spread over the whole value
        // axis, which keeps a melody's contour readable instead of
        // quantising it onto however many notes happen to be on screen.
        let pitches = placed
            .iter()
            .filter_map(|entry| match Lane::of(entry.event) {
                Lane::Pitch(midi) => Some(midi),
                Lane::Named(_) => None,
            })
            .collect::<Vec<_>>();
        let span = pitch_span(&pitches, options);

        // The look: a silent note is a thin stroke through the middle of
        // its lane, the sounding one fills the lane and lights up - the
        // roll reads as lines with one bar alive on it, not as a wall of
        // slabs. Thickness is in the tier's points, so a lane one cell wide
        // still tells the two apart (half a cell against a whole one).
        let lane_size = (value_len / lane_count).max(1);
        let cell_points = if vertical { points_across } else { points_down };
        let thin = (lane_size / 3).max(1);
        let thick = (lane_size * 2 / 3)
            .max(cell_points.min(lane_size))
            .max(thin + 1)
            .min(lane_size);
        let event_gap = ((if vertical { points_down } else { points_across }) / 2).max(1);
        let mut labelled: Vec<RollLabel> = Vec::new();
        // Extremely dense onsets cannot have a raster gap. An ASCII stem
        // has its own side bearings, so adjacent hits still read as ticks.
        let mut ticks = Vec::new();
        for entry in &placed {
            if self.hide_inactive && !entry.active {
                continue;
            }
            let lane = Lane::of(entry.event);
            let color = self.color_of(entry);
            let thickness = if entry.active { thick } else { thin };
            // The lane's centre line, from which the bar grows both ways.
            let centre = match (&lane, self.fold, span) {
                (Lane::Pitch(midi), false, Some((low, high))) => {
                    let normal = ((midi - low) / (high - low)).clamp(0.0, 1.0);
                    (normal * value_len.saturating_sub(1) as f32).round() as usize
                }
                _ => {
                    let index = lanes
                        .iter()
                        .position(|candidate| *candidate == lane)
                        .unwrap_or(0);
                    index * lane_size + lane_size / 2
                }
            };
            let mut low = centre.saturating_sub(thickness / 2);
            if thickness % cell_points == 0 {
                // A bar as tall as whole cells sits on cell boundaries: one
                // solid glyph, not two halves astride a seam.
                low = ((low + cell_points / 2) / cell_points * cell_points)
                    .min(value_len.saturating_sub(thickness));
            }
            let high = (low + thickness.saturating_sub(1)).min(value_len.saturating_sub(1));
            let (v0, v1) = (
                value_point(low).min(value_point(high)),
                value_point(low).max(value_point(high)),
            );
            let (onset, release) = (time_point(entry.begin), time_point(entry.end));
            let (mut t0, mut t1) = (onset.min(release), onset.max(release));
            t1 = t1.max(t0 + 1);
            let compressed = t1 - t0 <= event_gap as isize;
            // A gap after each note, so a run of hits reads as hits - taken
            // from the note's end, whichever way time runs, so the onset
            // edge is exact where the playhead crosses it.
            if t1 - t0 > event_gap as isize * if pixel_timing { 2 } else { 1 } {
                if release > onset {
                    t1 -= event_gap as isize;
                } else {
                    t0 += event_gap as isize;
                }
            }
            // Clip after taking the gap from the real release, so a note
            // entering from outside the viewport keeps its sustained body.
            t0 = t0.clamp(0, time_len as isize);
            t1 = t1.clamp(0, time_len as isize);
            if t0 >= t1 {
                continue;
            }
            if glyph_timing && compressed && onset >= 0 && onset < time_len as isize {
                let attack = if release < onset { onset - 1 } else { onset };
                if attack >= t0 && attack < t1 {
                    ticks.push((
                        entry.active,
                        attack as usize / points_across,
                        v0 / points_down..=v1 / points_down,
                        color,
                    ));
                }
            }
            for t in t0..t1 {
                for v in v0..=v1 {
                    if vertical {
                        grid.set(v, t as usize, color);
                    } else {
                        grid.set(t as usize, v, color);
                    }
                }
            }
            if self.labels {
                // strudel.cc: the pattern's `.activeLabel()` while the note
                // sounds, else its `.label()`, else the note name or the
                // sound.
                let named =
                    |label: &Option<String>| label.clone().filter(|label| !label.is_empty());
                let text = entry
                    .active
                    .then(|| named(&entry.event.active_label))
                    .flatten()
                    .or_else(|| named(&entry.event.label))
                    .unwrap_or_else(|| lane.label());
                let (t0, t1) = (t0 as usize, t1 as usize);
                let (cell_x, row, bar_cells) = if vertical {
                    (
                        v0 / points_across,
                        t0 / points_down,
                        v0 / points_across..(v1 + 1).div_ceil(points_across),
                    )
                } else {
                    (
                        t0 / points_across,
                        ((v0 + v1) / 2) / points_down,
                        t0 / points_across..t1.div_ceil(points_across),
                    )
                };
                labelled.push(RollLabel {
                    cell_x,
                    row,
                    bar_cells,
                    text,
                    color,
                });
            }
        }

        // On its side the playhead is a one-point line across the lanes,
        // over the bars, where the falling notes land.
        if vertical {
            let t = time_point(cycle_now);
            if t >= 0 && (t as usize) < time_len {
                for x in 0..width {
                    grid.set(x, t as usize, self.playhead_color);
                }
            }
        }
        grid.paint(buffer, self.background);
        // Sounding hits win when several onsets collapse onto one cell.
        // Labels and the playhead are painted afterwards and keep priority.
        ticks.sort_by_key(|(active, _, _, _)| *active);
        for (_, column, rows, color) in ticks {
            for row in rows {
                if let Some(cell) = buffer.cell_mut((area.x + column as u16, area.y + row as u16)) {
                    cell.set_symbol("|").set_fg(color).set_bg(self.background);
                }
            }
        }

        // Cycle lines: a faint rule where nothing is drawn, never through a
        // note.
        for cycle in begin.floor() as i64..=end.ceil() as i64 {
            let t = time_point(cycle as f64);
            if t < 0 || t as usize >= time_len {
                continue;
            }
            if vertical {
                let cell_y = area.y + (t as usize / points_down) as u16;
                for x in area.x..area.right() {
                    if let Some(cell) = buffer.cell_mut((x, cell_y))
                        && cell.symbol() == " "
                    {
                        cell.set_char('▔');
                        cell.set_fg(theme.grid);
                    }
                }
            } else {
                let cell_x = area.x + (t as usize / points_across) as u16;
                for y in area.y..area.bottom() {
                    if let Some(cell) = buffer.cell_mut((cell_x, y))
                        && cell.symbol() == " "
                    {
                        cell.set_symbol(super::terminal::symbol("▏"));
                        cell.set_fg(theme.grid);
                    }
                }
            }
        }

        // The label starts in the bar, in the bar's own colour, and runs on
        // past it in that colour when the bar is shorter than the word -
        // the way the browser writes it over the rectangle.
        for label in labelled {
            let y = area.y + label.row as u16;
            if y >= area.bottom() {
                continue;
            }
            for (offset, glyph) in label.text.chars().enumerate() {
                let cell_x = label.cell_x + offset;
                if cell_x >= usize::from(area.width) {
                    break;
                }
                let Some(cell) = buffer.cell_mut((area.x + cell_x as u16, y)) else {
                    break;
                };
                cell.set_char(glyph);
                if label.bar_cells.contains(&cell_x) {
                    cell.set_fg(readable_over(label.color, theme));
                    cell.set_bg(label.color);
                } else {
                    cell.set_fg(label.color);
                }
                cell.modifier.insert(Modifier::BOLD);
            }
        }

        // The playhead stays put while the music moves under it. It is drawn
        // over the painted cells as a thin glyph line: on the point canvas it
        // shares cells with the note bars and loses the colour vote where it
        // crosses one, which cuts the line.
        if !vertical {
            let playhead_x = time_point(cycle_now);
            if playhead_x >= 0 && (playhead_x as usize) < time_len {
                let cell_x = area.x + (playhead_x as usize / points_across) as u16;
                for y in area.y..area.bottom() {
                    if let Some(cell) = buffer.cell_mut((cell_x, y)) {
                        cell.set_symbol(super::terminal::symbol("▏"));
                        cell.set_fg(self.playhead_color);
                    }
                }
            }
        }
    }
}

/// The pitch range when the roll is not folded.
///
/// strudel.cc pins it to `minMidi`..`maxMidi` (10..90) unless `autorange`
/// is set. A pane a few rows tall cannot spread eighty semitones, so when
/// none of the three is named the range follows the notes on screen;
/// naming `minMidi`, `maxMidi` or `autorange: 0` pins it as on strudel.cc.
fn pitch_span(pitches: &[f32], options: &VisualOptions) -> Option<(f32, f32)> {
    if pitches.is_empty() {
        return None;
    }
    let autorange = options.flag("autorange");
    let pinned = autorange == Some(false)
        || options.number("minmidi").is_some()
        || options.number("maxmidi").is_some();
    if pinned && autorange != Some(true) {
        let low = options.number("minmidi").unwrap_or(10.0) as f32;
        let high = options.number("maxmidi").unwrap_or(90.0) as f32;
        if high > low {
            return Some((low, high));
        }
    }
    let mut sorted = pitches.to_vec();
    sorted.sort_by(f32::total_cmp);
    let (low, high) = (sorted[0], sorted[sorted.len() - 1]);
    if high - low >= 1.0 {
        Some((low - 2.0, high + 2.0))
    } else {
        Some((low - 12.0, low + 12.0))
    }
}

/// The spiral, as strudel.cc draws it: an Archimedean spiral of `stretch`
/// turns per cycle with the present sitting `inset` cycles out from the
/// centre. What has already played winds inward, what is about to play
/// lies outside, and every event is an arc along the track - its own
/// colour while it sounds, the `inactiveColor` before and after unless
/// `colorizeInactive`, and fading with its distance from now (`fade`).
/// With `steady` the track turns under a playhead that stays put; `steady:
/// 0` orbits the playhead over a still track. A legend beside the spiral
/// names the events sounding right now, because a two-dot arc cannot.
struct Spiral<'a, 'b> {
    request: &'a VisualRequest<'b>,
    /// Turns per cycle.
    stretch: f64,
    /// Cycles from the centre to the playhead.
    inset: f64,
    /// How much of the clock's rotation the track follows: 1 keeps every
    /// event where it was drawn, 0 keeps the playhead where it is.
    steady: f64,
    fade: bool,
    /// Cycles taken off the end of every arc.
    padding: f64,
    active_color: Option<Color>,
    inactive_color: Option<Color>,
    colorize_inactive: bool,
    playhead_color: Color,
}

/// Cycles of upcoming music drawn outside the playhead - strudel.cc's
/// draw window, which also bounds the fade.
const SPIRAL_LOOKAHEAD_CYCLES: f64 = 2.0;

impl<'a, 'b> Spiral<'a, 'b> {
    fn new(request: &'a VisualRequest<'b>) -> Self {
        let options = request.options;
        Self {
            request,
            stretch: options.number("stretch").unwrap_or(1.0).clamp(0.1, 4.0),
            inset: options.number("inset").unwrap_or(3.0).clamp(0.5, 12.0),
            steady: options
                .number("steady")
                .or_else(|| options.flag("steady").map(f64::from))
                .unwrap_or(1.0)
                .clamp(-4.0, 4.0),
            fade: options.flag("fade").unwrap_or(true),
            padding: options.number("padding").unwrap_or(0.0).clamp(0.0, 1.0),
            active_color: options.color("activecolor"),
            inactive_color: options.color("inactivecolor"),
            colorize_inactive: options.flag("colorizeinactive").unwrap_or(false),
            playhead_color: options
                .color("playheadcolor")
                .unwrap_or(request.theme.playhead),
        }
    }
}

impl Widget for Spiral<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let state = self.request.state;
        let theme = self.request.theme;
        let Some((now_seconds, cycle_now, cps)) = state.current_clock() else {
            note(self.request, buffer, area, "waiting for the first beat");
            return;
        };
        let mut grid = Canvas::lines(area);
        let (width, height) = (grid.width(), grid.height());
        if width < 8 || height < 8 {
            return;
        }
        // A Braille cell is twice as tall as it is wide, so a round spiral
        // spans equal numbers of dots in both directions.
        let side = width.min(height);
        let centre = (side as f64 / 2.0, height as f64 / 2.0);
        let radius_max = side as f64 / 2.0 - 1.0;
        let total_turns = (self.inset + SPIRAL_LOOKAHEAD_CYCLES) * self.stretch;
        // strudel.cc: `rotate = steady * time`, in cycles, then stretched.
        let rotation = self.steady * cycle_now * self.stretch;
        let turn_of = |cycle: f64| (cycle - cycle_now + self.inset) * self.stretch;
        let point_at = |turn: f64, offset: f64| {
            let radius = (radius_max * turn / total_turns + offset).max(0.0);
            let angle = std::f64::consts::TAU * (turn + rotation) - std::f64::consts::FRAC_PI_2;
            (
                (centre.0 + radius * angle.cos()).round() as isize,
                (centre.1 + radius * angle.sin()).round() as isize,
            )
        };

        // The track, as a sparse dotted guide so the events stay legible.
        let steps = (total_turns * 48.0) as usize;
        for step in (0..=steps).step_by(2) {
            let (x, y) = point_at(total_turns * step as f64 / steps as f64, 0.0);
            if x >= 0 && y >= 0 {
                grid.set(x as usize, y as usize, theme.grid);
            }
        }

        let begin = cycle_now - self.inset;
        let end = cycle_now + SPIRAL_LOOKAHEAD_CYCLES;
        let mut placed = placed_events(state, self.request.slot, begin, end, now_seconds, cps);
        // Later events draw over earlier ones, and sounding ones over both.
        placed.sort_by(|left, right| {
            left.active
                .cmp(&right.active)
                .then(left.begin.total_cmp(&right.begin))
        });
        let ring = radius_max / total_turns;
        let thickness = (ring * 0.3).clamp(1.0, 3.0) as isize;
        for entry in &placed {
            let named = self.request.event_color(entry.event);
            let own = named.or(self.active_color).unwrap_or(theme.event);
            let color = if entry.active {
                brighten(own)
            } else if self.colorize_inactive {
                dim(own, theme)
            } else {
                dim(self.inactive_color.unwrap_or(theme.event_inactive), theme)
            };
            // strudel.cc fades an arc to nothing two cycles from now.
            let opacity = if self.fade {
                1.0 - ((entry.begin - cycle_now).abs() / SPIRAL_LOOKAHEAD_CYCLES) as f32
            } else {
                1.0
            };
            if opacity <= 0.0 {
                continue;
            }
            let color = if opacity >= 0.999 {
                color
            } else {
                wash(self.request.background, color, opacity)
            };
            // The arc runs to where the note stops sounding, not to the
            // end of its whole span.
            let clipped_end = entry
                .end
                .min(entry.begin + entry.event.duration_seconds * cps)
                - self.padding;
            let from = turn_of(entry.begin).clamp(0.0, total_turns);
            let to = turn_of(clipped_end)
                .clamp(0.0, total_turns)
                .max(from + 1.0 / 96.0);
            let arc_steps = (((to - from) * 96.0).ceil() as usize).clamp(1, 512);
            for layer in -thickness / 2..=thickness / 2 {
                let mut previous: Option<(isize, isize)> = None;
                for step in 0..=arc_steps {
                    let turn = from + (to - from) * step as f64 / arc_steps as f64;
                    let point = point_at(turn, layer as f64);
                    match previous {
                        Some(last) => grid.line(last, point, color),
                        None if point.0 >= 0 && point.1 >= 0 => {
                            grid.set(point.0 as usize, point.1 as usize, color);
                        }
                        None => {}
                    }
                    previous = Some(point);
                }
            }
        }

        // The playhead crosses the track where the present is - a tick
        // across it rather than strudel.cc's short dash along it, which at
        // Braille size would be two dots.
        let playhead_turn = self.inset * self.stretch;
        let reach = (ring * 0.45).clamp(2.0, 6.0);
        grid.line(
            point_at(playhead_turn, -reach),
            point_at(playhead_turn, reach),
            self.playhead_color,
        );
        grid.paint(buffer, self.request.background);

        let legend_x = area.x.saturating_add((side / 2) as u16 + 1);
        render_now_playing(
            buffer,
            Rect::new(
                legend_x,
                area.y,
                area.right().saturating_sub(legend_x),
                area.height,
            ),
            placed
                .iter()
                .rev()
                .filter(|entry| entry.active)
                .map(|entry| {
                    (
                        Lane::of(entry.event).label(),
                        brighten(
                            self.request
                                .event_color(entry.event)
                                .or(self.active_color)
                                .unwrap_or(theme.event),
                        ),
                    )
                }),
            theme,
        );
    }
}

/// A column of labels naming what is sounding now, drawn to the right of a
/// round widget. Duplicates collapse so a four-voice chord of the same
/// sample is one line.
fn render_now_playing(
    buffer: &mut Buffer,
    area: Rect,
    entries: impl Iterator<Item = (String, Color)>,
    theme: &Theme,
) {
    if area.width < 3 || area.height == 0 {
        return;
    }
    let mut seen = Vec::<String>::new();
    let mut y = area.y;
    for (label, color) in entries {
        if label.is_empty() || seen.contains(&label) {
            continue;
        }
        if y >= area.bottom() {
            break;
        }
        buffer.set_stringn(
            area.x,
            y,
            format!("{} {label}", crate::terminal::symbol("▸")),
            usize::from(area.width),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        );
        seen.push(label);
        y += 1;
    }
    if seen.is_empty() {
        buffer.set_stringn(
            area.x,
            area.y,
            "▹ -",
            usize::from(area.width),
            Style::default().fg(theme.muted),
        );
    }
}

/// The pitch wheel: one octave folded onto a clock face, as strudel.cc
/// draws it. The `root` (a frequency in Hz, or a note name) sits at twelve
/// o'clock, pitch rises clockwise, and each of the `edo` divisions of the
/// octave is a dot. A sounding note is a disc at its place on the rim
/// (`hapcircles`) with, in `mode: 'flake'`, a hand from the centre to it -
/// or, in `mode: 'polygon'`, the sounding notes joined into a shape. There
/// is no outer ring unless `circle` asks for one. The note names sit
/// beside the wheel.
struct PitchWheel<'a, 'b> {
    request: &'a VisualRequest<'b>,
    edo: usize,
    /// The frequency at twelve o'clock.
    root_hz: f64,
    hands: bool,
    polygon: bool,
    discs: bool,
    ring: bool,
}

impl<'a, 'b> PitchWheel<'a, 'b> {
    fn new(request: &'a VisualRequest<'b>) -> Self {
        let options = request.options;
        // strudel.cc's root is `midiToFreq(36)`: a C.
        let c2 = 440.0 * 2f64.powf((36.0 - 69.0) / 12.0);
        let root_hz = options
            .number("root")
            .filter(|hertz| hertz.is_finite() && *hertz > 0.0)
            .or_else(|| {
                options
                    .text("root")
                    .and_then(note_to_midi)
                    .map(|class| 440.0 * 2f64.powf((class + 36.0 - 69.0) / 12.0))
            })
            .unwrap_or(c2);
        let mode = options.text("mode").unwrap_or("flake");
        Self {
            request,
            edo: options.number("edo").unwrap_or(12.0).clamp(0.0, 96.0) as usize,
            root_hz,
            hands: mode == "flake",
            polygon: mode == "polygon",
            discs: options.flag("hapcircles").unwrap_or(true),
            ring: options.flag("circle").unwrap_or(false),
        }
    }
}

impl Widget for PitchWheel<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let state = self.request.state;
        let theme = self.request.theme;
        let Some((now_seconds, _, _)) = state.current_clock() else {
            note(self.request, buffer, area, "waiting for the first beat");
            return;
        };
        let mut grid = Canvas::lines(area);
        let (width, height) = (grid.width(), grid.height());
        if width < 8 || height < 8 {
            return;
        }
        let in_slot = |event: &&UiScheduledEvent| {
            self.request
                .slot
                .is_none_or(|slot| event.ui_visuals & (1_u64 << slot) != 0)
        };
        // strudel.cc reads the tuning `edoScale` wrote on the haps over the
        // widget's own options: a sounding note's first, else the latest.
        let scale = state
            .events()
            .filter(in_slot)
            .filter(|event| event.scale.is_some() && is_active(event, now_seconds))
            .last()
            .or_else(|| {
                state
                    .events()
                    .filter(in_slot)
                    .filter(|event| event.scale.is_some())
                    .last()
            })
            .and_then(|event| event.scale.as_ref());
        let edo = scale
            .map(|scale| usize::from(scale.edo).clamp(1, 96))
            .unwrap_or(self.edo);
        let root_hz = scale
            .map(|scale| f64::from(scale.root_hz))
            .filter(|hertz| hertz.is_finite() && *hertz > 0.0)
            .unwrap_or(self.root_hz);
        // Where a frequency sits on the wheel, in turns clockwise from the
        // root: strudel.cc's `freq2angle`, the octave fraction of the ratio.
        let turn_of = |hertz: f64| (hertz / root_hz).log2().rem_euclid(1.0);
        let in_scale = |division: usize| {
            scale.is_none_or(|scale| {
                u16::try_from(division)
                    .is_ok_and(|division| scale.degree_indexes.contains(&division))
            })
        };
        let side = width.min(height);
        // The wheel sits at the left of its row, with the labels beside it,
        // rather than floating in the middle of an otherwise empty band.
        let centre = (side as f64 / 2.0, height as f64 / 2.0);
        let radius = side as f64 / 2.0 - 3.0;
        let angle_of = |turn: f64| std::f64::consts::TAU * turn - std::f64::consts::FRAC_PI_2;
        let point_at = |angle: f64, radius: f64| {
            (
                (centre.0 + radius * angle.cos()).round() as isize,
                (centre.1 + radius * angle.sin()).round() as isize,
            )
        };

        if self.ring {
            let steps = ((radius * std::f64::consts::TAU * 1.5).ceil() as usize).max(8);
            for step in 0..steps {
                let (x, y) = point_at(angle_of(step as f64 / steps as f64), radius);
                if x >= 0 && y >= 0 {
                    grid.set(x as usize, y as usize, theme.grid);
                }
            }
        }
        // The hour marks, one per division of the octave - faint for the
        // divisions the event's scale leaves out, as on strudel.cc.
        let faint = wash(self.request.background, theme.grid, 0.15);
        for division in 0..edo {
            let (x, y) = point_at(angle_of(division as f64 / edo as f64), radius);
            if x >= 0 && y >= 0 {
                let color = if in_scale(division) {
                    theme.grid
                } else {
                    faint
                };
                grid.set(x as usize, y as usize, color);
            }
        }
        // Twelve o'clock is the root, and gets a slightly larger mark.
        if edo > 0 {
            grid.disc(point_at(angle_of(0.0), radius), 1, theme.rule);
        }

        let mut sounding = Vec::<(f64, String, Color)>::new();
        for event in state.events().filter(in_slot) {
            let Some(hertz) = event
                .frequency_hz
                .filter(|value| value.is_finite() && *value > 0.0)
            else {
                continue;
            };
            if !is_active(event, now_seconds) {
                continue;
            }
            let turn = turn_of(f64::from(hertz));
            let own = brighten(self.request.event_color(event).unwrap_or(theme.event));
            let alpha = gain_alpha(event);
            let color = if alpha >= 0.999 {
                own
            } else {
                wash(self.request.background, own, alpha)
            };
            // The scale's name for the note's degree, beside its name.
            let interval = scale.and_then(|scale| {
                let degree = ((turn * edo as f64).round() as usize) % edo;
                scale
                    .degree_indexes
                    .iter()
                    .position(|index| usize::from(*index) == degree)
                    .and_then(|position| scale.interval_labels.get(position))
                    .filter(|label| !label.is_empty())
            });
            let name = match interval {
                Some(interval) => format!("{} {interval}", note_name(frequency_to_midi(hertz))),
                None => note_name(frequency_to_midi(hertz)),
            };
            sounding.push((turn, name, color));
        }
        for (turn, _, color) in &sounding {
            let angle = angle_of(*turn);
            if self.hands {
                grid.line(
                    point_at(angle, 0.0),
                    point_at(angle, radius - 2.0),
                    dim(*color, theme),
                );
            }
            if self.discs {
                grid.disc(point_at(angle, radius), 2, *color);
            }
        }
        if self.polygon && sounding.len() >= 2 {
            let mut shape = sounding.clone();
            shape.sort_by(|left, right| left.0.total_cmp(&right.0));
            for index in 0..shape.len() {
                let (turn, _, color) = &shape[index];
                let (next_turn, _, _) = &shape[(index + 1) % shape.len()];
                grid.line(
                    point_at(angle_of(*turn), radius),
                    point_at(angle_of(*next_turn), radius),
                    *color,
                );
            }
        }
        grid.paint(buffer, self.request.background);

        let legend_x = area.x.saturating_add((side / 2) as u16 + 1);
        let legend = Rect::new(
            legend_x,
            area.y,
            area.right().saturating_sub(legend_x),
            area.height,
        );
        render_now_playing(
            buffer,
            legend,
            sounding
                .iter()
                .map(|(_, name, color)| (name.clone(), *color)),
            theme,
        );
        if edo != 12 && legend.height > 1 {
            buffer.set_stringn(
                legend.x,
                legend.bottom() - 1,
                format!("{edo} edo"),
                usize::from(legend.width),
                Style::default().fg(theme.muted),
            );
        }
    }
}

/// The oscilloscope: the post-mix waveform as a single Braille line.
/// `scope` and `tscope` are one widget, as on strudel.cc: the trace is
/// aligned (`align`, on by default) to the first falling crossing of
/// `-trigger`, so a steady tone stands still instead of sliding across the
/// pane; `align: 0` shows the buffer as it came. `pos` places the centre
/// line, `scale` the amplitude.
struct Scope<'a, 'b> {
    request: &'a VisualRequest<'b>,
    align: bool,
    trigger: f32,
    scale: f32,
    /// Fill the rows whatever the level: the window's peak is brought up
    /// to the edge, up to [`FIT_MOST`].
    fit: bool,
    /// Amplitude in decibels rather than straight: a quiet tail is
    /// visible without a loud passage leaving the pane.
    log: bool,
    pos: f32,
    color: Option<Color>,
}

impl<'a, 'b> Scope<'a, 'b> {
    fn new(request: &'a VisualRequest<'b>) -> Self {
        let options = request.options;
        // Strudel's default of 0.25 leaves a terminal-sized scope a couple
        // of rows tall, so a score that names no scale gets the trace
        // brought up to the pane instead; naming one turns that off, and
        // `fit` says either outright.
        let scale = options.number("scale");
        Self {
            request,
            align: options.flag("align").unwrap_or(true),
            trigger: options.number("trigger").unwrap_or(0.0).clamp(-1.0, 1.0) as f32,
            scale: scale.unwrap_or(1.0).clamp(0.01, 8.0) as f32,
            fit: options.flag("fit").unwrap_or(scale.is_none()),
            log: options.flag("log").or(options.flag("db")).unwrap_or(false),
            pos: options.number("pos").unwrap_or(0.5).clamp(0.0, 1.0) as f32,
            color: options.color("color"),
        }
    }
}

impl Widget for Scope<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let theme = self.request.theme;
        let Some(audio) = self.request.state.audio_for(self.request.slot) else {
            note(self.request, buffer, area, "waiting for audio");
            return;
        };
        let samples = &audio.scope;
        let start = if self.align {
            falling_crossing(samples, self.trigger)
        } else {
            0
        };
        let window = &samples[start..];
        let scale = if self.fit {
            fit_scale(window)
        } else {
            self.scale
        };
        render_trace(
            window,
            samples.len(),
            scale,
            self.pos,
            self.log,
            widget_color(self.request, self.color),
            theme,
            area,
            buffer,
        );
    }
}

/// The most a fitted trace is brought up by: past this a room's hiss
/// fills the pane and reads as music.
const FIT_MOST: f32 = 12.0;
/// A fitted trace stops just short of the edge, so its peaks are seen to
/// be peaks rather than a line along the top.
const FIT_HEADROOM: f32 = 0.92;
/// Quieter than this the window is silence, and silence is drawn flat
/// rather than magnified into a picture of the noise floor.
const FIT_FLOOR: f32 = 0.002;
/// The dynamic range a logarithmic trace spans: at -60 dB the trace is on
/// the centre line, at full scale it reaches the edge.
const LOG_FLOOR_DB: f32 = -60.0;

/// The gain that brings a window's peak up to the pane, within reason.
fn fit_scale(window: &[f32]) -> f32 {
    let peak = window
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    if peak > FIT_FLOOR {
        (FIT_HEADROOM / peak).min(FIT_MOST)
    } else {
        1.0
    }
}

/// An amplitude in decibels, as a fraction of the pane: full scale at the
/// edge, [`LOG_FLOOR_DB`] and quieter on the centre line, the sign kept so
/// the wave still reads as a wave.
fn decibel_amplitude(sample: f32) -> f32 {
    let magnitude = sample.abs().min(1.0);
    if magnitude <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * magnitude.log10();
    let fraction = 1.0 - (db / LOG_FLOOR_DB).clamp(0.0, 1.0);
    fraction.copysign(sample)
}

/// One waveform as a Braille polyline across `area`, with a dotted centre
/// line at `pos`. `span` samples fill the width, so a trace that starts
/// late (aligned to a trigger) keeps its time scale and ends before the
/// right edge rather than stretching, as on strudel.cc. Shared by the
/// inline scope and the footer's master scope.
#[allow(clippy::too_many_arguments)]
fn render_trace(
    samples: &[f32],
    span: usize,
    scale: f32,
    pos: f32,
    log: bool,
    color: Color,
    theme: &Theme,
    area: Rect,
    buffer: &mut Buffer,
) {
    let mut grid = Canvas::lines(area);
    let (width, height) = (grid.width(), grid.height());
    if width < 2 || height < 2 {
        return;
    }
    let centre = (pos * (height - 1) as f32).round() as usize;
    for x in (0..width).step_by(4) {
        grid.set(x, centre, theme.grid);
    }
    if samples.len() >= 2 {
        let to_y = |sample: f32| {
            let scaled = if log {
                decibel_amplitude(sample * scale)
            } else {
                (sample * scale).clamp(-1.0, 1.0)
            };
            let half = (height - 1) as f32 / 2.0;
            (centre as f32 - scaled * half)
                .round()
                .clamp(0.0, (height - 1) as f32) as isize
        };
        let span = span.max(2);
        let mut previous: Option<(isize, isize)> = None;
        for x in 0..width {
            let index = x * (span - 1) / (width - 1).max(1);
            if index >= samples.len() {
                break;
            }
            let point = (x as isize, to_y(samples[index]));
            match previous {
                Some(from) => grid.line(from, point, color),
                None => grid.set(x, point.1.max(0) as usize, color),
            }
            previous = Some(point);
        }
    }
    grid.paint(buffer, theme.background);
}

/// The footer's master scope: the mix leaving the machine, as a small trace
/// beside the level meter.
pub struct MasterScope<'a> {
    pub audio: Option<&'a rustel_runtime::ui_analysis::UiAudioAnalysisFrame>,
    pub theme: &'a Theme,
    pub playing: bool,
}

impl Widget for MasterScope<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let samples = match self.audio {
            Some(audio) if self.playing => audio.scope.as_slice(),
            _ => &[],
        };
        // A few periods from a rising zero crossing, scaled so the trace
        // fills its rows whatever the level: a scope that reads as a
        // waveform, not as a flat dotted line under a quiet mix.
        let start = rising_zero_crossing(samples);
        let span = (samples.len() / 4)
            .clamp(64, 768)
            .min(samples.len().saturating_sub(start));
        let window = &samples[start..start + span];
        render_trace(
            window,
            window.len(),
            fit_scale(window),
            0.5,
            false,
            self.theme.accent,
            self.theme,
            area,
            buffer,
        );
    }
}

/// strudel.cc's scope trigger: the first sample at or under `-trigger`
/// whose predecessor was above it - a falling crossing - within the first
/// half of the buffer, so at least half a window is always drawn.
fn falling_crossing(samples: &[f32], trigger: f32) -> usize {
    let limit = samples.len() / 2;
    for index in 1..limit {
        if samples[index - 1] > -trigger && samples[index] <= -trigger {
            return index;
        }
    }
    0
}

/// First rising zero crossing in the first half of the buffer, so a triggered
/// trace starts at the same phase every frame.
pub(super) fn rising_zero_crossing(samples: &[f32]) -> usize {
    let limit = samples.len() / 2;
    for index in 1..limit {
        if samples[index - 1] <= 0.0 && samples[index] > 0.0 {
            return index;
        }
    }
    0
}

/// A spectrogram's shape, shared by the two the studio draws: time across
/// with the newest frame at the right edge and older ones scrolling away
/// to the left, frequency up the rows, low at the bottom, log-spaced.
///
/// A row pools every band that falls in it and keeps the loudest, so a
/// narrow harmonic never falls between two rows and never disappears
/// because the pane is a cell narrower than it was.
pub(super) struct SpectrogramGrid<'a> {
    columns: &'a VecDeque<[f32; SPECTROGRAM_BANDS]>,
    width: usize,
    height: usize,
    /// Point columns one frame occupies: a wider column is a slower scroll.
    speed: usize,
    min_db: f32,
    max_db: f32,
}

/// The bottom of a spectrogram's window: quieter than this is the ground.
/// The dock's other analyser styles measure from here too, so a band that
/// half fills a bar half lights the waterfall.
pub(super) const SPECTROGRAM_FLOOR_DB: f32 = -80.0;

/// How the dB window is bent before it becomes brightness. Ordinary
/// material sits around -45 to -6 dBFS a band, which over an 80 dB window
/// is 0.44 to 0.93 - a bright wash with no contrast in it, and a noise
/// floor still painted at an eighth of the ramp. Bending it sinks the
/// floor under the drawing threshold and stretches the octaves the music
/// is actually in across the whole ramp.
const SPECTROGRAM_GAMMA: f32 = 1.6;

/// Under this a point is silence rather than quiet, and a glyph cell is
/// left alone so whatever was drawn under it survives. A pixel is not: an
/// unset pixel is a transparent hole, and a spectrogram with holes in it
/// is not a darker spectrogram.
const SPECTROGRAM_SILENCE: f32 = 0.008;

impl<'a> SpectrogramGrid<'a> {
    pub(super) fn new(
        columns: &'a VecDeque<[f32; SPECTROGRAM_BANDS]>,
        width: usize,
        height: usize,
        speed: usize,
        min_db: f32,
        max_db: f32,
    ) -> Self {
        Self {
            columns,
            width,
            height,
            speed: speed.max(1),
            min_db,
            max_db: max_db.max(min_db + 1.0),
        }
    }

    /// The canvas a spectrogram wants. Continuous tone needs two colours a
    /// cell, which only half blocks give among the glyph rasters - a
    /// sextant cell keeps one colour by majority and a Braille cell one by
    /// accident - so both glyph tiers take half blocks and the picture
    /// gets finer only where there are real pixels to get finer with.
    pub(super) fn canvas(area: Rect) -> Canvas {
        match super::graphics::tier() {
            super::graphics::Tier::Pixels => Canvas::bars(area),
            _ => Canvas::with_raster(area, Raster::HalfBlocks),
        }
    }

    /// The level at a point, 0 through 1, or `None` where the ring does not
    /// reach back that far: the picture hugs the right edge and leaves the
    /// rest of the pane alone until it has something to say there.
    pub(super) fn level(&self, x: usize, y: usize) -> Option<f32> {
        if self.width == 0 || self.height == 0 || x >= self.width || y >= self.height {
            return None;
        }
        let back = (self.width - 1 - x) / self.speed;
        let column = self.columns.len().checked_sub(back + 1)?;
        let column = self.columns.get(column)?;
        let decibels = if self.height > SPECTROGRAM_BANDS {
            // More rows than bands - a pixel raster - so read between the
            // band centres: pooling there would stamp every band as a slab
            // two or three points tall and the picture would stair-step.
            let place =
                (self.height - 1 - y) as f32 * SPECTROGRAM_BANDS as f32 / self.height as f32 - 0.5;
            let below = place.floor().clamp(0.0, (SPECTROGRAM_BANDS - 1) as f32) as usize;
            let above = (below + 1).min(SPECTROGRAM_BANDS - 1);
            let blend = (place - below as f32).clamp(0.0, 1.0);
            column[below] + (column[above] - column[below]) * blend
        } else {
            // Fewer rows than bands: a row pools every band that falls in
            // it and keeps the loudest, so a narrow harmonic never falls
            // between two rows.
            let first = ((self.height - 1 - y) * SPECTROGRAM_BANDS / self.height)
                .min(SPECTROGRAM_BANDS - 1);
            let last = ((self.height - y) * SPECTROGRAM_BANDS / self.height)
                .clamp(first + 1, SPECTROGRAM_BANDS);
            column[first..last]
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max)
        };
        let level = ((decibels - self.min_db) / (self.max_db - self.min_db)).clamp(0.0, 1.0);
        Some(level.powf(SPECTROGRAM_GAMMA))
    }

    /// Fill a half-block or pixel canvas, asking `shade` for the colour of
    /// every point it reaches - the quiet ones included, so the picture
    /// reads as a ground with light on it rather than as light alone.
    /// Silence is skipped on a glyph raster, where an untouched cell keeps
    /// whatever was drawn under it, and painted on a pixel one, where an
    /// untouched point is a transparent hole.
    pub(super) fn paint(&self, grid: &mut Canvas, shade: impl Fn(usize, usize, f32) -> Color) {
        let silence = if matches!(grid.raster(), Raster::Pixels { .. }) {
            -1.0
        } else {
            SPECTROGRAM_SILENCE
        };
        for y in 0..self.height {
            for x in 0..self.width {
                let Some(level) = self.level(x, y) else {
                    continue;
                };
                if level < silence {
                    continue;
                }
                grid.set(x, y, shade(x, y, level));
            }
        }
    }

    /// Draw as Braille dots straight into the buffer: two columns of time
    /// and four rows of frequency a cell, the level as dot density by an
    /// ordered dither. A Braille cell carries one colour whatever is asked
    /// of it, so the shading has to come out of the dots and the colour
    /// says only how loud the loudest dot in the cell is.
    #[allow(clippy::needless_range_loop)]
    pub(super) fn paint_braille(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        background: Color,
        shade: impl Fn(usize, usize, f32) -> Color,
    ) {
        for row in 0..area.height {
            for column in 0..area.width {
                let mut bits = 0u8;
                let mut loudest = 0.0f32;
                let mut reached = false;
                for dy in 0..4 {
                    for dx in 0..2 {
                        let x = usize::from(column) * 2 + dx;
                        let y = usize::from(row) * 4 + dy;
                        let Some(level) = self.level(x, y) else {
                            continue;
                        };
                        reached = true;
                        loudest = loudest.max(level);
                        if braille_dot(level, dx, dy) {
                            bits |= BRAILLE_BITS[dx][dy];
                        }
                    }
                }
                if !reached {
                    continue;
                }
                let position = (area.x + column, area.y + row);
                let symbol = char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' ');
                if let Some(cell) = buffer.cell_mut(position) {
                    cell.set_char(symbol).set_style(
                        Style::default()
                            .fg(shade(
                                usize::from(column) * 2,
                                usize::from(row) * 4,
                                loudest,
                            ))
                            .bg(background),
                    );
                }
            }
        }
    }
}

/// A colour's hue, saturation and value, when it has knowable ones. A
/// theme that defers to the terminal's palette has none, and nothing may
/// be invented for it.
fn hsv_parts(color: Color) -> Option<(f32, f32, f32)> {
    let (red, green, blue) = true_rgb(color)?;
    let (red, green, blue) = (
        f32::from(red) / 255.0,
        f32::from(green) / 255.0,
        f32::from(blue) / 255.0,
    );
    let top = red.max(green).max(blue);
    let span = top - red.min(green).min(blue);
    let hue = if span <= f32::EPSILON {
        0.0
    } else if top == red {
        60.0 * (((green - blue) / span) % 6.0)
    } else if top == green {
        60.0 * ((blue - red) / span + 2.0)
    } else {
        60.0 * ((red - green) / span + 4.0)
    };
    let saturation = if top <= 0.0 { 0.0 } else { span / top };
    Some(((hue + 360.0) % 360.0, saturation, top))
}

/// A spectrogram's colour ramp: four stops away from the ground, climbing
/// in hue as well as in light.
///
/// One colour faded into the background reads as a smear however carefully
/// it is faded - a terminal eye resolves a step of hue far more easily
/// than a step of brightness inside one hue - so the widget's own colour
/// becomes the middle of a ramp that starts colder and darker than it and
/// ends warmer and brighter, and the level chooses where along that a
/// point sits.
pub(super) struct SpectrogramRamp {
    ground: Color,
    stops: [Color; 4],
}

impl SpectrogramRamp {
    pub(super) fn new(theme: &Theme, ground: Color, tint: Color) -> Self {
        let stops = match hsv_parts(tint) {
            Some((hue, saturation, value)) => [
                hsv(hue - 30.0, (saturation * 1.15).min(1.0), value * 0.45),
                tint,
                hsv(hue + 20.0, saturation * 0.70, (value * 1.2).min(1.0)),
                hsv(hue + 28.0, saturation * 0.20, 1.0),
            ],
            // A theme that defers to the terminal's palette has no RGB to
            // take apart, and a blend against it snaps rather than invent
            // one. Its own meter colours are four steps it has already
            // chosen, and stepping through those beats fading between two.
            None => [
                theme.meter.low,
                theme.meter.mid,
                theme.meter.high,
                theme.meter.peak,
            ],
        };
        // The quiet end of the picture is painted, not left blank: on a
        // pixel raster an untouched point is a hole, so the ground fills
        // it. A ground the terminal owns has no pixel value - the raster
        // would paint it black and a glyph cell would draw the default
        // foreground - so the palette's muted colour stands in. The ramp
        // then starts at a grey the terminal can show.
        let ground = if true_rgb(ground).is_some() {
            ground
        } else if matches!(theme.muted, Color::Reset) {
            stops[0]
        } else {
            theme.muted
        };
        Self { ground, stops }
    }

    /// The colour of a point at `level`, 0 through 1.
    pub(super) fn at(&self, level: f32) -> Color {
        let level = level.clamp(0.0, 1.0);
        let ramp = [
            (0.0, self.ground),
            (0.30, self.stops[0]),
            (0.62, self.stops[1]),
            (0.86, self.stops[2]),
            (1.0, self.stops[3]),
        ];
        for pair in ramp.windows(2) {
            let ((low, from), (high, to)) = (pair[0], pair[1]);
            if level <= high {
                return mix(from, to, ((level - low) / (high - low)).clamp(0.0, 1.0));
            }
        }
        self.stops[3]
    }
}

/// Theme colours across frequency, from low on the left to high on the right.
pub(super) fn spectrum_color(theme: &Theme, column: usize, columns: usize) -> Color {
    if columns <= 1 {
        return theme.accent;
    }
    let stops = [
        theme.syntax.punctuation,
        theme.accent,
        theme.syntax.string,
        theme.syntax.number,
        theme.meter.peak,
    ];
    let position = column.min(columns - 1) as f32 / (columns - 1) as f32 * (stops.len() - 1) as f32;
    let index = (position as usize).min(stops.len() - 2);
    mix(stops[index], stops[index + 1], position - index as f32)
}

/// The spectrum, as a terminal can show it: an analyser. Log-spaced bars
/// fill the pane's width, each the loudest band in its slice of the
/// octaves, rising at once and falling smoothly, with a peak mark that
/// holds a moment; frequency marks sit under them when there is a row to
/// spare. `min`/`max` are the dB window, `color` the bars' colour (the
/// pattern's own `.color()` wins).
///
/// strudel.cc's spectrum is a spectrogram - time scrolling across a canvas,
/// level as brightness - and `scroll: 1` draws that instead: the newest
/// frame at the right edge, frequency up the rows, `speed` columns a frame.
struct Spectrum<'a, 'b> {
    request: &'a VisualRequest<'b>,
    min_db: f32,
    max_db: f32,
    speed: usize,
    scroll: bool,
    color: Option<Color>,
}

impl<'a, 'b> Spectrum<'a, 'b> {
    fn new(request: &'a VisualRequest<'b>) -> Self {
        let options = request.options;
        let min_db = options.number("min").unwrap_or(-80.0) as f32;
        let max_db = options.number("max").unwrap_or(0.0) as f32;
        Self {
            request,
            min_db,
            max_db: max_db.max(min_db + 1.0),
            // Columns per frame; a fraction is one column every few frames,
            // which the ring's own pace already gives, so it floors at one.
            speed: (options.number("speed").unwrap_or(1.0).round().max(1.0) as usize).min(16),
            scroll: options.flag("scroll").unwrap_or(false),
            color: options.color("color"),
        }
    }

    fn level(&self, db: f32) -> f32 {
        ((db - self.min_db) / (self.max_db - self.min_db)).clamp(0.0, 1.0)
    }

    /// The analyser: bars across the width, marks underneath.
    fn render_bars(&self, bands: &AnalyserBands, area: Rect, buffer: &mut Buffer) {
        let theme = self.request.theme;
        let color_override = widget_color_override(self.request, self.color);
        let marks = area.height >= 5;
        let bars_area = Rect::new(area.x, area.y, area.width, area.height - u16::from(marks));
        let mut grid = Canvas::lines(bars_area);
        let (points_across, _) = grid.raster().points_per_cell();
        let (width, height) = (grid.width(), grid.height());
        if width == 0 || height == 0 {
            return;
        }
        let bars = usize::from(bars_area.width);
        for bar in 0..bars {
            let color = color_override.unwrap_or_else(|| spectrum_color(theme, bar, bars));
            let peak_color = wash(self.request.background, color, 0.6);
            let crest = brighten(color);
            let level = self.level(spectrum_level(&bands.levels, bar, bars));
            let peak = self.level(spectrum_level(&bands.peaks, bar, bars));
            let x0 = bar * points_across;
            let x1 = ((bar + 1) * points_across).min(width);
            let lit = (level * height as f32).round() as usize;
            for x in x0..x1 {
                for y in height.saturating_sub(lit)..height {
                    let shade = if y == height - lit { crest } else { color };
                    grid.set(x, y, shade);
                }
                let peak_row = (peak * height as f32).round() as usize;
                if peak_row > lit + 1 && peak_row <= height {
                    grid.set(x, height - peak_row, peak_color);
                }
            }
        }
        grid.paint(buffer, self.request.background);

        if marks {
            let y = area.bottom() - 1;
            let bins = UI_SPECTRUM_BINS as f32;
            let nyquist = self.request.state.audio_sample_rate() as f32 / 2.0;
            for (hertz, text) in [(100.0, "100"), (1_000.0, "1k"), (10_000.0, "10k")] {
                if hertz >= nyquist {
                    continue;
                }
                let bin = hertz / nyquist * bins;
                let bar = ((bin + 1.0).ln() / (bins + 1.0).ln() * bars as f32) as usize;
                let x = area.x + bar.min(bars - 1) as u16;
                if x + text.len() as u16 <= area.right() {
                    buffer.set_stringn(x, y, text, text.len(), Style::default().fg(theme.muted));
                }
            }
        }
    }

    /// strudel.cc's picture: each audio frame a column, the newest at the
    /// right edge, frequency up the rows, level as the widget's colour
    /// over the ground.
    fn render_scroll(
        &self,
        columns: &VecDeque<[f32; SPECTROGRAM_BANDS]>,
        area: Rect,
        buffer: &mut Buffer,
    ) {
        let mut grid = SpectrogramGrid::canvas(area);
        let (width, height) = (grid.width(), grid.height());
        if width == 0 || height == 0 {
            return;
        }
        let background = self.request.background;
        let ramp = SpectrogramRamp::new(
            self.request.theme,
            background,
            widget_color(self.request, self.color),
        );
        SpectrogramGrid::new(columns, width, height, self.speed, self.min_db, self.max_db)
            .paint(&mut grid, |_, _, level| ramp.at(level));
        grid.paint(buffer, background);
    }
}

impl Widget for Spectrum<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let state = self.request.state;
        if self.scroll {
            match state
                .spectrogram(self.request.slot)
                .filter(|columns| !columns.is_empty())
            {
                Some(columns) => self.render_scroll(columns, area, buffer),
                None => note(self.request, buffer, area, "waiting for audio"),
            }
            return;
        }
        match state.analyser(self.request.slot) {
            Some(bands) => self.render_bars(bands, area, buffer),
            None => note(self.request, buffer, area, "waiting for audio"),
        }
    }
}

/// The video painter: a clip's own picture where its sound is. The frame
/// is the one the clock says is showing, so the picture plays with the
/// sample and holds when the transport holds.
#[cfg(feature = "video")]
struct Video<'a, 'b> {
    request: &'a VisualRequest<'b>,
}

#[cfg(feature = "video")]
impl<'a, 'b> Video<'a, 'b> {
    fn new(request: &'a VisualRequest<'b>) -> Self {
        Self { request }
    }

    /// The clip to show: the event sounding now, else the last that
    /// sounded, else the next to come. Among events of one tier, a clip
    /// the library answered with a strip outranks a newer one it has not:
    /// a stage painter sees every lane's events, and a plain sound must
    /// not take the picture from the clip beside it.
    fn clip(&self, now: f64) -> Option<(&UiScheduledEvent, String)> {
        let state = self.request.state;
        let slot = self.request.slot;
        struct Candidate<'a> {
            event: &'a UiScheduledEvent,
            name: String,
            ready: bool,
        }
        let ready = |name: &str| matches!(state.video_for(name), Some(VideoEntry::Ready(_)));
        // `forward` says the tier wants the newest event; `false`, the
        // soonest.
        let beats = |held: &Candidate<'_>, next: &Candidate<'_>, forward: bool| {
            if next.ready != held.ready {
                return next.ready;
            }
            if forward {
                held.event.target_time < next.event.target_time
            } else {
                held.event.target_time > next.event.target_time
            }
        };
        let mut active: Option<Candidate<'_>> = None;
        let mut past: Option<Candidate<'_>> = None;
        let mut future: Option<Candidate<'_>> = None;
        for event in state.events() {
            if !slot.is_none_or(|slot| event.ui_visuals & (1_u64 << slot) != 0) {
                continue;
            }
            let Some(name) = event_clip_name(event) else {
                continue;
            };
            let next = Candidate {
                event,
                ready: ready(&name),
                name,
            };
            let end = event.target_time + event.duration_seconds.max(0.035);
            if !now.is_finite() {
                // No clock yet: the first clip the score names will do.
                future.get_or_insert(next);
            } else if event.target_time <= now && now <= end {
                if active.as_ref().is_none_or(|held| beats(held, &next, true)) {
                    active = Some(next);
                }
            } else if event.target_time <= now {
                if past.as_ref().is_none_or(|held| beats(held, &next, true)) {
                    past = Some(next);
                }
            } else if future.as_ref().is_none_or(|held| beats(held, &next, false)) {
                future = Some(next);
            }
        }
        let chosen = active.or(past).or(future)?;
        Some((chosen.event, chosen.name))
    }
}

#[cfg(feature = "video")]
impl Widget for Video<'_, '_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let state = self.request.state;
        let (now, _, _) = state.current_clock().unwrap_or((f64::NAN, 0.0, 0.0));
        let Some((event, name)) = self.clip(now) else {
            note(self.request, buffer, area, "no video event in this lane");
            return;
        };
        let strip = match state.video_for(&name) {
            Some(VideoEntry::Ready(strip)) => strip,
            Some(VideoEntry::Failed(message)) => {
                note(self.request, buffer, area, &format!("{name}: {message}"));
                return;
            }
            Some(VideoEntry::Asking(_)) | None => {
                note(self.request, buffer, area, &format!("waiting for {name}"));
                return;
            }
        };
        // Where the sound is inside the clip, from the event's own chain.
        let duration = f64::from(strip.duration());
        let position = clip_position(event, now, duration);
        let Some(frame) = strip.frame_at(position as f32) else {
            note(
                self.request,
                buffer,
                area,
                &format!("{name} decoded to no frames"),
            );
            return;
        };
        let mut canvas = Canvas::bars(area);
        let (canvas_width, canvas_height) = (canvas.width(), canvas.height());
        let (frame_width, frame_height) = (strip.width() as usize, strip.height() as usize);
        // The frame keeps its shape inside the canvas points and sits
        // centred, however much larger or smaller it is than the rows it
        // was given; the points it leaves untouched show the pane behind.
        let scale = (canvas_width as f64 / frame_width.max(1) as f64)
            .min(canvas_height as f64 / frame_height.max(1) as f64);
        let fit = |frame_side: usize, canvas_side: usize| {
            (((frame_side as f64 * scale).round().max(1.0)) as usize).min(canvas_side)
        };
        let (draw_width, draw_height) = (
            fit(frame_width, canvas_width),
            fit(frame_height, canvas_height),
        );
        let (x0, y0) = (
            (canvas_width - draw_width) / 2,
            (canvas_height - draw_height) / 2,
        );
        for y in 0..draw_height {
            let source_row = (y * frame_height / draw_height) * frame_width;
            for x in 0..draw_width {
                let at = (source_row + x * frame_width / draw_width) * 4;
                if let [red, green, blue, ..] = frame.get(at..at + 3).unwrap_or_default() {
                    canvas.set(x0 + x, y0 + y, Color::Rgb(*red, *green, *blue));
                }
            }
        }
        canvas.paint(buffer, self.request.background);
    }
}

fn parse_fraction(value: &str) -> Option<f64> {
    let (numerator, denominator) = value.split_once('/')?;
    let numerator = numerator.trim().parse::<f64>().ok()?;
    let denominator = denominator.trim().parse::<f64>().ok()?;
    (denominator != 0.0)
        .then_some(numerator / denominator)
        .filter(|value| value.is_finite())
}

fn frequency_to_midi(frequency: f32) -> f32 {
    69.0 + 12.0 * (frequency.max(0.001) / 440.0).log2()
}

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

pub fn note_name(midi: f32) -> String {
    let rounded = midi.round() as i32;
    let octave = rounded.div_euclid(12) - 1;
    let name = NOTE_NAMES[rounded.rem_euclid(12) as usize];
    format!("{name}{octave}")
}

/// Pitch class of a note name such as `C`, `f#` or `Bb`.
fn note_to_midi(name: &str) -> Option<f64> {
    let mut characters = name.chars();
    let letter = characters.next()?.to_ascii_uppercase();
    let base: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let accidental: i32 = match characters.next() {
        Some('#' | 's') => 1,
        Some('b') => -1,
        None => 0,
        _ => return None,
    };
    Some(f64::from((base + accidental).rem_euclid(12)))
}

fn dim(color: Color, theme: &Theme) -> Color {
    wash(theme.background, color, 0.45)
}

fn brighten(color: Color) -> Color {
    mix(color, Color::Rgb(255, 255, 255), 0.25)
}

/// Text drawn over a filled bar has to survive both a bright and a dark fill.
fn readable_over(fill: Color, theme: &Theme) -> Color {
    if luminance(fill) > 0.55 {
        theme.background
    } else {
        theme.foreground
    }
}

// ---------------------------------------------------------------------------
// The painters' reference entries.
//
// The documentation lives beside the renderer that owns it: upstream's words
// where the painter mirrors strudel.cc (Documentation text from the Strudel
// project (AGPL-3.0-or-later), https://strudel.cc). The terminal's own
// divergences live in PAINTER_TERMINAL beside these entries, because they
// describe only the terminal's rendering, not the painter's contract.
// ---------------------------------------------------------------------------

use rustel_core::reference::{ReferenceEntry, ReferenceParam};

/// What this engine's terminal does with a painter and with each of its
/// options, where that differs from the documentation.
///
/// These notes describe only the terminal's own rendering, so they live apart
/// from the presentation-neutral [`ReferenceEntry`]: a painter's reference
/// entry carries no terminal field, and only the studio's reference renderer
/// reads this table. An option the terminal honours exactly as documented is
/// simply absent.
pub(super) struct PainterTerminal {
    /// The painter's plain name, as the reference entry spells it.
    pub painter: &'static str,
    /// How the terminal draws the painter as a whole.
    pub entry: &'static str,
    /// `(option, note)` pairs for the options that behave differently here.
    pub options: &'static [(&'static str, &'static str)],
    /// The options the terminal reads as numbers where the documentation
    /// gives another default or no bounds.
    pub numbers: &'static [TerminalNumber],
}

/// A number option as the terminal reads it: the value it takes when the
/// score leaves the option out, and the bounds it holds the value to. A
/// switch is 0 or 1.
pub(super) struct TerminalNumber {
    pub option: &'static str,
    pub default: f64,
    pub min: f64,
    pub max: f64,
}

const fn number(option: &'static str, default: f64, min: f64, max: f64) -> TerminalNumber {
    TerminalNumber {
        option,
        default,
        min,
        max,
    }
}

/// Terminal behaviour of every painter that differs from the documentation.
pub(super) const PAINTER_TERMINAL: &[PainterTerminal] = &[
    PainterTerminal {
        painter: "pianoroll",
        entry: "In the terminal every option below is honoured except the ones marked. A silent note is a thin stroke through its lane and the sounding one fills it, with a gap after each hit; a plain pianoroll() paints on the stage behind the score, _pianoroll() in rows under its own line. Without fold the pitch range follows the notes on screen unless minMidi, maxMidi or autorange pins it: eight rows cannot spread 10..90. A note's gain fades its bar, as the browser's alpha does. A single lane uses left/right half-blocks for finer timing on basic terminals; hits too dense for a gap use thin ASCII ticks. Several hits can still share a cell.",
        options: &[
            (
                "labels",
                "the pattern's own label() wins, and activeLabel while it sounds",
            ),
            (
                "overscan",
                "ignored: the window is the one option pair, cycles and playhead",
            ),
            (
                "hideNegative",
                "ignored: the roll starts at the score's first cycle",
            ),
            ("smear", "ignored: a cell grid keeps no previous frame"),
            (
                "fill",
                "ignored: a filled cell has no outline to leave instead",
            ),
            (
                "fillActive",
                "ignored: filled and stroked are the same cell here",
            ),
            (
                "stroke",
                "ignored: a filled cell has no outline to leave instead",
            ),
            (
                "strokeActive",
                "ignored: filled and stroked are the same cell here",
            ),
            (
                "fontFamily",
                "ignored: the terminal's font is the terminal's",
            ),
        ],
        // Folded unless told not to: eight rows cannot spread 10..90. And
        // the range follows the notes until it is pinned.
        numbers: &[
            number("fold", 1.0, 0.0, 1.0),
            number("autorange", 1.0, 0.0, 1.0),
        ],
    },
    PainterTerminal {
        painter: "punchcard",
        entry: "The same painter as pianoroll here, fed by the highlighter: a plain punchcard() shows every event the highlighter sees, the whole stack, while _punchcard() is tagged to its own pattern. Every pianoroll option applies, with the same exceptions.",
        options: &[],
        numbers: &[],
    },
    PainterTerminal {
        painter: "wordfall",
        entry: "A punchcard on its side with labels on: time falls from the top, one lane per value across. Every pianoroll option applies, with the same exceptions.",
        options: &[],
        numbers: &[],
    },
    PainterTerminal {
        painter: "spiral",
        entry: "In the terminal the spiral is drawn in Braille, with a dotted guide track and a legend naming what sounds now. The units are the same everywhere: stretch is turns per cycle, inset is cycles from the centre to the playhead, steady is how much of the clock's turn the track follows.",
        options: &[
            (
                "size",
                "ignored: the spiral fills the rows its call is given",
            ),
            (
                "thickness",
                "ignored: an arc is as thick as the dots it is drawn with",
            ),
            ("cap", "ignored: dots have no end caps"),
            (
                "playheadLength",
                "ignored: the playhead is a tick across the track, which at this size a dash could not be",
            ),
            ("playheadThickness", "ignored: see playheadLength"),
            ("logSpiral", "ignored, as on strudel.cc"),
        ],
        numbers: &[
            number("stretch", 1.0, 0.1, 4.0),
            number("inset", 3.0, 0.5, 12.0),
            number("steady", 1.0, -4.0, 4.0),
            number("padding", 0.0, 0.0, 1.0),
        ],
    },
    PainterTerminal {
        painter: "pitchwheel",
        entry: "In the terminal the wheel sits at the left of its rows with the note names beside it, drawn in Braille. An edoScale'd event sets edo and root itself, and the divisions outside its scale are drawn faint.",
        options: &[
            ("root", "a frequency in Hz - a note name is taken too"),
            (
                "thickness",
                "ignored: a pixel width has no meaning on a cell grid",
            ),
            ("hapRadius", "ignored: a note is a two-dot disc"),
            (
                "mode",
                "flake draws a hand to each note, polygon joins them",
            ),
            ("margin", "ignored: the wheel is fitted to its rows"),
        ],
        numbers: &[],
    },
    PainterTerminal {
        painter: "scope",
        entry: "scope and tscope are one widget here: the trace is aligned unless align: 0. It fills its pane instead of a fixed quarter-pane scale, which in a six-row widget would be two rows tall.",
        options: &[
            ("color", "the pattern's own color() wins over it"),
            ("thickness", "ignored: the trace is one Braille dot thick"),
            (
                "trigger",
                "the level the trace starts at, not a switch - align is the switch",
            ),
        ],
        // A scope that names no scale is fitted to its pane at a scale of
        // 1. Naming one, even 1, ends the fit.
        numbers: &[
            number("scale", 1.0, 0.01, 8.0),
            number("pos", 0.5, 0.0, 1.0),
            number("trigger", 0.0, -1.0, 1.0),
        ],
    },
    PainterTerminal {
        painter: "spectrum",
        entry: "In the terminal this is an analyser by default, not a scrolling spectrogram: log-spaced bars across the width of the pane, low frequencies at the left, each bar the loudest band in its slice, rising at once and falling smoothly, with a peak mark that holds a moment and 100/1k/10k marks underneath when the pane is five rows or taller. scroll: 1 draws the spectrogram instead - time across with the newest frame at the right edge, frequency up the rows, level as colour over the ground - on half blocks on either glyph tier, and on real pixels where the terminal has them.",
        options: &[
            ("thickness", "ignored, as on strudel.cc"),
            ("speed", "columns a frame, with scroll: 1"),
            ("scroll", "only here"),
            ("color", "only here"),
        ],
        numbers: &[
            number("min", -80.0, -120.0, 0.0),
            number("max", 0.0, -120.0, 0.0),
        ],
    },
    PainterTerminal {
        painter: "video",
        entry: "Small by default: eight rows under its own line, a real picture on a kitty terminal and coloured cells on the glyph tiers, the clip's shape kept inside the rows. A plain video() paints the stage behind the score. The frame is the one the clock says is showing; between the clip's kept frames the earlier one shows, and past the last one the last one holds.",
        options: &[],
        numbers: &[],
    },
    PainterTerminal {
        painter: "markcss",
        entry: "Accepted and never drawn: the studio highlights sounding events in its own theme's colours, which a CSS rule cannot describe.",
        options: &[],
        numbers: &[],
    },
];

/// The terminal's note for a painter as a whole; empty when the terminal does
/// exactly what the documentation says.
pub(super) fn painter_terminal_entry(painter: &str) -> &'static str {
    let painter = painter.strip_prefix('_').unwrap_or(painter);
    PAINTER_TERMINAL
        .iter()
        .find(|painter_terminal| painter_terminal.painter == painter)
        .map(|painter_terminal| painter_terminal.entry)
        .unwrap_or("")
}

/// The terminal's note for one of a painter's options; empty when the option
/// is honoured exactly as documented.
pub(super) fn painter_terminal_option(painter: &str, option: &str) -> &'static str {
    let painter = painter.strip_prefix('_').unwrap_or(painter);
    PAINTER_TERMINAL
        .iter()
        .find(|painter_terminal| painter_terminal.painter == painter)
        .and_then(|painter_terminal| {
            painter_terminal
                .options
                .iter()
                .find(|(name, _)| *name == option)
                .map(|(_, note)| *note)
        })
        .unwrap_or("")
}

/// How the terminal reads one of a painter's options as a number, when it
/// differs from the documentation.
pub(super) fn painter_terminal_number(
    painter: &str,
    option: &str,
) -> Option<&'static TerminalNumber> {
    let painter = painter.strip_prefix('_').unwrap_or(painter);
    PAINTER_TERMINAL
        .iter()
        .find(|painter_terminal| painter_terminal.painter == painter)?
        .numbers
        .iter()
        .find(|number| number.option.eq_ignore_ascii_case(option))
}

/// The painters' entries, in the order the renderer dispatches them.
pub(super) const REFERENCE_ENTRIES: &[ReferenceEntry] = &[
    ReferenceEntry {
        name: "pianoroll",
        synonyms: &[],
        summary: "Visualises a pattern as a scrolling 'pianoroll', displayed in the background of the editor.",
        description: "Visualises a pattern as a scrolling 'pianoroll', displayed in the background of the editor. To show a pianoroll for all running patterns, use `all(pianoroll)`. To have a pianoroll appear below\na pattern instead, prefix with `_`, e.g.: `sound(\"bd sd\")._pianoroll()`.",
        params: &[
            ReferenceParam {
                name: "options",
                r#type: "Object",
                description: "Object containing all the optional following parameters as key value pairs:",
            },
            ReferenceParam {
                name: "cycles",
                r#type: "integer",
                description: "number of cycles to be displayed at the same time - defaults to 4",
            },
            ReferenceParam {
                name: "playhead",
                r#type: "number",
                description: "location of the active notes on the time axis - 0 to 1, defaults to 0.5",
            },
            ReferenceParam {
                name: "vertical",
                r#type: "boolean",
                description: "displays the roll vertically - 0 by default",
            },
            ReferenceParam {
                name: "labels",
                r#type: "boolean",
                description: "displays labels on individual notes (see the label function) - 0 by default",
            },
            ReferenceParam {
                name: "flipTime",
                r#type: "boolean",
                description: "reverse the direction of the roll - 0 by default",
            },
            ReferenceParam {
                name: "flipValues",
                r#type: "boolean",
                description: "reverse the relative location of notes on the value axis - 0 by default",
            },
            ReferenceParam {
                name: "overscan",
                r#type: "number",
                description: "lookup X cycles outside of the cycles window to display notes in advance - 1 by default",
            },
            ReferenceParam {
                name: "hideNegative",
                r#type: "boolean",
                description: "hide notes with negative time (before starting playing the pattern) - 0 by default",
            },
            ReferenceParam {
                name: "smear",
                r#type: "boolean",
                description: "notes leave a solid trace - 0 by default",
            },
            ReferenceParam {
                name: "fold",
                r#type: "boolean",
                description: "notes takes the full value axis width - 0 by default",
            },
            ReferenceParam {
                name: "active",
                r#type: "string",
                description: "hexadecimal or CSS color of the active notes - defaults to #FFCA28",
            },
            ReferenceParam {
                name: "inactive",
                r#type: "string",
                description: "hexadecimal or CSS color of the inactive notes - defaults to #7491D2",
            },
            ReferenceParam {
                name: "background",
                r#type: "string",
                description: "hexadecimal or CSS color of the background - defaults to transparent",
            },
            ReferenceParam {
                name: "playheadColor",
                r#type: "string",
                description: "hexadecimal or CSS color of the line representing the play head - defaults to white",
            },
            ReferenceParam {
                name: "fill",
                r#type: "boolean",
                description: "notes are filled with color (otherwise only the label is displayed) - 0 by default",
            },
            ReferenceParam {
                name: "fillActive",
                r#type: "boolean",
                description: "active notes are filled with color - 0 by default",
            },
            ReferenceParam {
                name: "stroke",
                r#type: "boolean",
                description: "notes are shown with colored borders - 0 by default",
            },
            ReferenceParam {
                name: "strokeActive",
                r#type: "boolean",
                description: "active notes are shown with colored borders - 0 by default",
            },
            ReferenceParam {
                name: "hideInactive",
                r#type: "boolean",
                description: "only active notes are shown - 0 by default",
            },
            ReferenceParam {
                name: "colorizeInactive",
                r#type: "boolean",
                description: "use note color for inactive notes - 1 by default",
            },
            ReferenceParam {
                name: "fontFamily",
                r#type: "string",
                description: "define the font used by notes labels - defaults to 'monospace'",
            },
            ReferenceParam {
                name: "minMidi",
                r#type: "integer",
                description: "minimum note value to display on the value axis - defaults to 10",
            },
            ReferenceParam {
                name: "maxMidi",
                r#type: "integer",
                description: "maximum note value to display on the value axis - defaults to 90",
            },
            ReferenceParam {
                name: "autorange",
                r#type: "boolean",
                description: "automatically calculate the minMidi and maxMidi parameters - 0 by default",
            },
        ],
        examples: &[
            "note(\"c2 a2 eb2\")\n.euclid(5,8)\n.s('sawtooth')\n.lpenv(4).lpf(300)\n.pianoroll({ labels: 1 })",
        ],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "punchcard",
        synonyms: &[],
        summary: "A pianoroll fed by the highlighter's events: the whole stack for `punchcard()`, its own pattern for `_punchcard()`.",
        description: "Visualises a pattern as a scrolling 'pianoroll', displayed in the background of the editor. To show a pianoroll for all running patterns, use `all(pianoroll)`. To have a pianoroll appear below\na pattern instead, prefix with `_`, e.g.: `sound(\"bd sd\")._pianoroll()`.\n\nHere and everywhere else this language plays, `pianoroll` and `punchcard` are one renderer fed differently: a plain `punchcard()` shows every event the highlighter sees, the whole stack, while `_punchcard()` is tagged to its own pattern like `_pianoroll()`.",
        params: &[ReferenceParam {
            name: "options",
            r#type: "Object",
            description: "the same options as pianoroll",
        }],
        examples: &[
            "note(\"c a f e\").color(\"white\").punchcard()",
            "note(\"c a f e\").color(\"white\")\n.punchcard()\n.color(\"cyan\")",
        ],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "wordfall",
        synonyms: &[],
        summary: "Displays a vertical pianoroll with event labels.",
        description: "Displays a vertical pianoroll with event labels.\nSupports all the same options as pianoroll.",
        params: &[],
        examples: &[],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "spiral",
        synonyms: &[],
        summary: "Displays a spiral visual.",
        description: "Displays a spiral visual.",
        params: &[
            ReferenceParam {
                name: "options",
                r#type: "Object",
                description: "Object containing all the optional following parameters as key value pairs:",
            },
            ReferenceParam {
                name: "stretch",
                r#type: "number",
                description: "controls the rotations per cycle ratio, where 1 = 1 cycle / 360 degrees",
            },
            ReferenceParam {
                name: "size",
                r#type: "number",
                description: "the diameter of the spiral",
            },
            ReferenceParam {
                name: "thickness",
                r#type: "number",
                description: "line thickness",
            },
            ReferenceParam {
                name: "cap",
                r#type: "string",
                description: "style of line ends: butt (default), round, square",
            },
            ReferenceParam {
                name: "inset",
                r#type: "number",
                description: "number of rotations before spiral starts (default 3)",
            },
            ReferenceParam {
                name: "playheadColor",
                r#type: "string",
                description: "color of playhead, defaults to white",
            },
            ReferenceParam {
                name: "playheadLength",
                r#type: "number",
                description: "length of playhead in rotations, defaults to 0.02",
            },
            ReferenceParam {
                name: "playheadThickness",
                r#type: "number",
                description: "thickness of playheadrotations, defaults to thickness",
            },
            ReferenceParam {
                name: "padding",
                r#type: "number",
                description: "space around spiral",
            },
            ReferenceParam {
                name: "steady",
                r#type: "number",
                description: "steadyness of spiral vs playhead. 1 = spiral doesn't move, playhead does.",
            },
            ReferenceParam {
                name: "activeColor",
                r#type: "number",
                description: "color of active segment. defaults to foreground of theme",
            },
            ReferenceParam {
                name: "inactiveColor",
                r#type: "number",
                description: "color of inactive segments. defaults to gutterForeground of theme",
            },
            ReferenceParam {
                name: "colorizeInactive",
                r#type: "boolean",
                description: "wether or not to colorize inactive segments, defaults to 0",
            },
            ReferenceParam {
                name: "fade",
                r#type: "boolean",
                description: "wether or not past and future should fade out. defaults to 1",
            },
            ReferenceParam {
                name: "logSpiral",
                r#type: "boolean",
                description: "wether or not the spiral should be logarithmic. defaults to 0",
            },
        ],
        examples: &[
            "note(\"c2 a2 eb2\")\n.euclid(5,8)\n.s('sawtooth')\n.lpenv(4).lpf(300)\n._spiral({ steady: .96 })",
        ],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "pitchwheel",
        synonyms: &[],
        summary: "Renders a pitch circle to visualize frequencies within one octave",
        description: "Renders a pitch circle to visualize frequencies within one octave",
        params: &[
            ReferenceParam {
                name: "hapcircles",
                r#type: "number",
                description: "",
            },
            ReferenceParam {
                name: "circle",
                r#type: "number",
                description: "",
            },
            ReferenceParam {
                name: "edo",
                r#type: "number",
                description: "",
            },
            ReferenceParam {
                name: "root",
                r#type: "string",
                description: "",
            },
            ReferenceParam {
                name: "thickness",
                r#type: "number",
                description: "",
            },
            ReferenceParam {
                name: "hapRadius",
                r#type: "number",
                description: "",
            },
            ReferenceParam {
                name: "mode",
                r#type: "string",
                description: "",
            },
            ReferenceParam {
                name: "margin",
                r#type: "number",
                description: "",
            },
        ],
        examples: &[
            "n(\"0 .. 12\").scale(\"C:chromatic\")\n.s(\"sawtooth\")\n.lpf(500)\n._pitchwheel()",
        ],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "scope",
        synonyms: &["tscope"],
        summary: "Renders an oscilloscope for the time domain of the audio signal.",
        description: "Renders an oscilloscope for the time domain of the audio signal.",
        params: &[
            ReferenceParam {
                name: "config",
                r#type: "object",
                description: "optional config with options:",
            },
            ReferenceParam {
                name: "align",
                r#type: "boolean",
                description: "if 1, the scope will be aligned to the first zero crossing. defaults to 1",
            },
            ReferenceParam {
                name: "color",
                r#type: "string",
                description: "line color as hex or color name. defaults to white.",
            },
            ReferenceParam {
                name: "thickness",
                r#type: "number",
                description: "line thickness. defaults to 3",
            },
            ReferenceParam {
                name: "scale",
                r#type: "number",
                description: "scales the y-axis. Defaults to 0.25",
            },
            ReferenceParam {
                name: "pos",
                r#type: "number",
                description: "y-position relative to screen height. 0 = top, 1 = bottom of screen",
            },
            ReferenceParam {
                name: "trigger",
                r#type: "number",
                description: "amplitude value that is used to align the scope. defaults to 0.",
            },
        ],
        examples: &["s(\"sawtooth\")._scope()"],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "spectrum",
        synonyms: &[],
        summary: "Renders a spectrum analyzer for the incoming audio signal.",
        description: "Renders a spectrum analyzer for the incoming audio signal.",
        params: &[
            ReferenceParam {
                name: "config",
                r#type: "object",
                description: "optional config with options:",
            },
            ReferenceParam {
                name: "thickness",
                r#type: "integer",
                description: "line thickness in px (default 3)",
            },
            ReferenceParam {
                name: "speed",
                r#type: "integer",
                description: "scroll speed (default 1)",
            },
            ReferenceParam {
                name: "min",
                r#type: "integer",
                description: "min db (default -80)",
            },
            ReferenceParam {
                name: "max",
                r#type: "integer",
                description: "max db (default 0)",
            },
            ReferenceParam {
                name: "scroll",
                r#type: "boolean",
                description: "draw a scrolling spectrogram instead of the analyser (default 0)",
            },
            ReferenceParam {
                name: "color",
                r#type: "string",
                description: "the bars' colour; the pattern's own color() wins over it",
            },
        ],
        examples: &[
            "n(\"<0 4 <2 3> 1>*3\")\n.off(1/8, add(n(5)))\n.off(1/5, add(n(7)))\n.scale(\"d3:minor:pentatonic\")\n.s('sine')\n.dec(.3).room(.5)\n._spectrum()",
        ],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "video",
        synonyms: &[],
        summary: "Shows the picture of a video sample while its sound plays.",
        description: "Shows the picture of a video sample while its sound plays. The frame follows the pattern's clock, so the picture plays with the sample and holds when the transport holds. The picture decodes once per clip and stays small. VP8 WebM and Motion JPEG decode in this build; a clip in another video codec says which codec it is and what to re-encode it as.",
        params: &[],
        examples: &["$: s(\"bd\")._video()"],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
    ReferenceEntry {
        name: "markcss",
        synonyms: &[],
        summary: "Overrides the css of highlighted events.",
        description: "Overrides the css of highlighted events. Make sure to use single quotes!",
        params: &[ReferenceParam {
            name: "css",
            r#type: "string",
            description: "the css to give a sounding event's highlight, in single quotes",
        }],
        examples: &["note(\"c a f e\")\n.markcss('text-decoration:underline')"],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
];

#[cfg(test)]
mod tests {
    use super::Raster;
    use crate::engine::ClockStatus;
    use ratatui::layout::Rect;

    /// A pixel canvas over a big pane is millions of points, every one of
    /// them rasterised, compressed and pushed through the terminal on
    /// every frame. Past the budget the raster coarsens and the terminal
    /// scales the picture back into the cells it named.
    #[test]
    fn a_pixel_canvas_stays_inside_its_budget() {
        let fine = Raster::Pixels {
            cell_width: 18,
            cell_height: 36,
        };
        let small = Rect::new(0, 0, 20, 8);
        assert_eq!(fine.within_budget(small), fine, "a small pane is untouched");
        let pane = Rect::new(0, 0, 200, 60);
        let coarse = fine.within_budget(pane);
        assert_ne!(coarse, fine, "a whole-screen pane is coarsened");
        let budget = crate::graphics::canvas_budget();
        let (across, down) = coarse.points_per_cell();
        let points = (usize::from(pane.width) * across * usize::from(pane.height) * down) as u64;
        assert!(points <= budget, "{points} points is over the budget");
        assert!(
            points * 2 > budget,
            "{points} points gives up more than it had to"
        );
        // The glyph rasters are already coarse; the budget is not theirs.
        for raster in [
            Raster::HalfBlocks,
            Raster::HorizontalHalfBlocks,
            Raster::Sextants,
            Raster::Braille,
        ] {
            assert_eq!(raster.within_budget(pane), raster);
        }
    }

    /// Both glyph tiers draw lines in Braille.
    #[test]
    fn lines_are_braille_on_every_glyph_tier() {
        use crate::graphics::Tier;
        use crate::visuals::{Canvas, Raster};
        let area = ratatui::layout::Rect::new(0, 0, 4, 2);
        at_tier(Tier::Cells, || {
            assert_eq!(Canvas::lines(area).raster(), Raster::Braille);
        });
        at_tier(Tier::Fine, || {
            assert_eq!(Canvas::lines(area).raster(), Raster::Braille);
        });
    }

    #[test]
    fn a_canvas_rasterises_the_same_points_four_ways() {
        let area = Rect::new(0, 0, 2, 1);
        // Half blocks: top point lit in column 0.
        let mut canvas = Canvas::with_raster(area, Raster::HalfBlocks);
        assert_eq!((canvas.width(), canvas.height()), (2, 2));
        canvas.set(0, 0, Color::Red);
        let mut buffer = Buffer::empty(area);
        canvas.paint(&mut buffer, Color::Black);
        assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "▀");
        // Sextants: 2×3 per cell; the left column lit is `▌`, one corner a
        // glyph from the legacy-computing block.
        let mut canvas = Canvas::with_raster(area, Raster::Sextants);
        assert_eq!((canvas.width(), canvas.height()), (4, 3));
        canvas.column(0, 0, 2, Color::Red);
        canvas.set(2, 0, Color::Blue);
        let mut buffer = Buffer::empty(area);
        canvas.paint(&mut buffer, Color::Black);
        assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "▌");
        assert_eq!(buffer.cell((1, 0)).unwrap().symbol(), "\u{1FB00}");
        assert_eq!(sextant(0b111111), '█');
        assert_eq!(sextant(0b101010), '▐');
        assert_eq!(sextant(0b000011), '\u{1FB02}');
        assert_eq!(sextant(0b111110), '\u{1FB3B}');
        // Braille: 2×4 dots, one colour per cell.
        let mut canvas = Canvas::with_raster(area, Raster::Braille);
        assert_eq!((canvas.width(), canvas.height()), (4, 4));
        canvas.set(0, 0, Color::Green);
        let mut buffer = Buffer::empty(area);
        canvas.paint(&mut buffer, Color::Black);
        assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "⠁");
        assert_eq!(buffer.cell((1, 0)).unwrap().symbol(), " ");
        // Pixels: nothing in the buffer, an image for the frame.
        let _ = super::super::graphics::take_images();
        let mut canvas = Canvas::with_raster(
            area,
            Raster::Pixels {
                cell_width: 3,
                cell_height: 2,
            },
        );
        assert_eq!((canvas.width(), canvas.height()), (6, 2));
        canvas.set(5, 1, Color::Rgb(1, 2, 3));
        let mut buffer = Buffer::empty(area);
        canvas.paint(&mut buffer, Color::Black);
        assert_eq!(buffer.cell((1, 0)).unwrap().symbol(), " ");
        let images = super::super::graphics::take_images();
        assert_eq!(images.len(), 1);
        assert_eq!((images[0].width, images[0].height), (6, 2));
        assert_eq!(&images[0].rgba[(6 + 5) * 4..], &[1, 2, 3, 255]);
        assert_eq!(&images[0].rgba[..4], &[0, 0, 0, 0]);
    }

    use super::*;
    use rustel_runtime::ui_analysis::{UI_SCOPE_SAMPLES, UI_SPECTRUM_BINS};

    fn theme() -> Theme {
        Theme::built_in_default()
    }

    fn event(onset_id: u64, begin: &str, end: &str, target_time: f64) -> UiScheduledEvent {
        UiScheduledEvent {
            onset_id,
            generation: 1,
            whole_begin: begin.to_owned(),
            whole_end: end.to_owned(),
            part_begin: begin.to_owned(),
            part_end: end.to_owned(),
            target_time,
            duration_seconds: 0.5,
            value: Some("bd".to_owned()),
            color: None,
            label: Some("bd".to_owned()),
            active_label: None,
            scale: None,
            frequency_hz: None,
            gain: Some(1.0),
            ui_visuals: 0,
            context: vec![(0, 4)],
        }
    }

    /// A mark lets go over the fade after its event: half a second past
    /// the end it is halfway gone through a one-second fade, gone through
    /// a shorter one, and never there with none.
    #[test]
    fn a_mark_fades_after_its_event() {
        let gone = event(1, "0/1", "1/2", 9.0);
        let state = playing("$: s(\"bd\")", vec![gone], 0.0);
        assert!(
            state.active_marks(Color::White).is_empty(),
            "ended half a second ago"
        );
        assert!(state.fading_marks(Color::White, 0.4).is_empty());
        let marks = state.fading_marks(Color::White, 1.0);
        assert_eq!(marks.len(), 1);
        assert!(
            (0.3..=0.6).contains(&marks[0].strength),
            "halfway through the fade: {}",
            marks[0].strength
        );
        assert_eq!((marks[0].from, marks[0].to), (0, 4));
        let sounding = event(2, "0/1", "1/2", 10.0);
        let state = playing("$: s(\"bd\")", vec![sounding], 0.0);
        assert_eq!(state.fading_marks(Color::White, 1.0)[0].strength, 1.0);
    }

    #[test]
    fn timeline_visuals_select_only_events_tagged_for_their_receiver() {
        let mut first = event(1, "0/1", "1/2", 10.0);
        first.ui_visuals = 1_u64 << 0;
        let mut second = event(2, "0/1", "1/2", 10.0);
        second.ui_visuals = 1_u64 << 1;
        let state = playing(
            "stack(s('bd')._spiral(), s('sd')._pianoroll())",
            vec![first, second],
            0.0,
        );

        let selected = placed_events(&state, Some(1), 0.0, 1.0, 10.0, 1.0);
        assert_eq!(
            selected
                .iter()
                .map(|placed| placed.event.onset_id)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(
            placed_events(&state, None, 0.0, 1.0, 10.0, 1.0)
                .iter()
                .map(|placed| placed.event.onset_id)
                .collect::<Vec<_>>(),
            vec![1, 2],
            "an all(...) timeline must remain global"
        );
    }

    /// A running state whose clock is anchored at cycle `cycle`, carrying
    /// `events`.
    impl VisualState {
        /// A look-ahead said to cover everything: what the engine sends
        /// while nothing has been redrawn behind the playhead.
        fn install_preview_from_zero(&mut self, batch: UiEventBatch) -> bool {
            self.install_preview(batch, 0.0)
        }
    }

    /// A preview batch of `events`, as the engine's look-ahead would send
    /// it, on the same clock the tests' real batches use.
    fn preview_batch(state: &VisualState, events: Vec<UiScheduledEvent>) -> UiEventBatch {
        let revision = state.layout().expect("layout").source_revision.clone();
        UiEventBatch::new(10.0, 0.0, 1.0, 1, revision, events, 0).expect("batch")
    }

    /// The engine looks ahead of the audio and sends previews of the notes
    /// to come: the roll draws them, silent, beyond the real ones; each
    /// preview replaces the last; the real trace of a previewed onset
    /// retires it; a preview never highlights the source, never sounds,
    /// and goes with the timeline it belongs to.
    #[test]
    fn a_preview_draws_ahead_is_replaced_retired_by_its_real_onset_and_never_sounds() {
        let real = event(1, "0/1", "1/2", 10.0);
        let mut state = playing("$: s(\"bd\")._pianoroll()", vec![real], 0.0);
        let coming = |onset_id: u64, begin: &str, end: &str| {
            let mut event = event(onset_id, begin, end, 10.0);
            event.value = Some("hh".to_owned());
            event.label = Some("hh".to_owned());
            event
        };
        // Its target_time says "now", so only its being a preview keeps it
        // from sounding.
        assert!(
            state.install_preview_from_zero(preview_batch(&state, vec![coming(0, "1/1", "3/2")]))
        );
        assert_eq!(state.events().count(), 2);
        assert_eq!(state.events().filter(|event| is_preview(event)).count(), 1);
        let text = rows(&draw(
            "pianoroll",
            "{cycles: 2, playhead: 0.25}",
            &state,
            Rect::new(0, 0, 40, 4),
        ));
        let glyphs_at = |from: usize, to: usize, pick: fn(char) -> bool| {
            text.lines()
                .flat_map(|line| line.chars().skip(from).take(to - from).collect::<Vec<_>>())
                .filter(|glyph| pick(*glyph))
                .count()
        };
        assert!(
            glyphs_at(10, 20, |g| g == '█') > 0,
            "the real note sounds:\n{text}"
        );
        assert!(
            glyphs_at(30, 40, |g| matches!(g, '▀' | '▄')) > 0,
            "the preview is drawn ahead, as a stroke:\n{text}"
        );
        assert_eq!(
            glyphs_at(30, 40, |g| g == '█'),
            0,
            "a preview never fills its lane:\n{text}"
        );
        let marks = state.active_marks(Color::White);
        assert_eq!(marks.len(), 1, "only the real note highlights the source");
        assert!(
            marks
                .iter()
                .all(|mark| mark.onset_id & PREVIEW_ONSET_FLAG == 0)
        );

        // The next look-ahead replaces the last.
        assert!(
            state.install_preview_from_zero(preview_batch(&state, vec![coming(0, "2/1", "5/2")]))
        );
        assert_eq!(state.events().count(), 2);
        assert!(state.events().any(|event| event.whole_begin == "2/1"));
        assert!(!state.events().any(|event| event.whole_begin == "1/1"));

        // The real trace of the previewed onset arrives: no duplicate.
        let revision = state.layout().expect("layout").source_revision.clone();
        let mut queued = coming(2, "2/1", "5/2");
        queued.target_time = 12.0;
        assert!(state.install_batch(
            UiEventBatch::new(10.0, 0.0, 1.0, 1, revision.clone(), vec![queued], 0).expect("batch")
        ));
        assert_eq!(state.events().count(), 2);
        assert_eq!(state.events().filter(|event| is_preview(event)).count(), 0);

        // A preview of an onset already queued is not taken at all.
        assert!(
            state.install_preview_from_zero(preview_batch(&state, vec![coming(0, "2/1", "5/2")]))
        );
        assert_eq!(state.events().count(), 2);

        // A stop leaves the picture where it was, what was coming included;
        // the next start clears the look-ahead, as does a change of source.
        // The real events stay through both.
        assert!(
            state.install_preview_from_zero(preview_batch(&state, vec![coming(0, "3/1", "7/2")]))
        );
        assert_eq!(state.events().filter(|event| is_preview(event)).count(), 1);
        state.stop();
        assert_eq!(
            state.events().filter(|event| is_preview(event)).count(),
            1,
            "the look-ahead stays on the frozen picture"
        );
        state.start();
        assert_eq!(state.events().filter(|event| is_preview(event)).count(), 0);
        assert_eq!(
            state.events().count(),
            2,
            "the real events stay in the picture"
        );
        state.start();
        assert!(
            state.install_batch(
                UiEventBatch::new(
                    10.0,
                    0.0,
                    1.0,
                    1,
                    revision,
                    vec![event(3, "0/1", "1/2", 10.0)],
                    0
                )
                .expect("batch")
            )
        );
        assert!(
            state.install_preview_from_zero(preview_batch(&state, vec![coming(0, "3/1", "7/2")]))
        );
        // The two that were there plus the new batch's own pair: the
        // picture stays until a new layout replaces it, below.
        assert_eq!(state.events().count(), 4);
        state.install_layout(
            rustel_runtime::ui_events::visual_layout("$: s(\"sd\")._pianoroll()", 1)
                .expect("layout"),
        );
        assert_eq!(
            state.events().count(),
            0,
            "a new score starts with an empty timeline"
        );
    }

    /// strudel.cc's `activeLabel` names a note while it sounds; `label` the
    /// rest of the time.
    #[test]
    fn the_roll_writes_the_active_label_only_while_the_note_sounds() {
        let mut sounding = event(1, "0/1", "1/2", 10.0);
        sounding.frequency_hz = Some(261.6);
        sounding.label = Some("a".to_owned());
        sounding.active_label = Some("!".to_owned());
        let mut later = event(2, "1/2", "1/1", 12.0);
        later.frequency_hz = Some(392.0);
        later.label = Some("b".to_owned());
        later.active_label = Some("?".to_owned());
        let state = playing(
            "$: note(\"c4 g4\")._pianoroll()",
            vec![sounding, later],
            0.25,
        );
        let text = rows(&draw(
            "pianoroll",
            "{cycles: 1, playhead: 0.25, labels: 1}",
            &state,
            Rect::new(0, 0, 40, 4),
        ));
        assert!(text.contains('!') && text.contains('b'), "{text}");
        assert!(!text.contains('a') && !text.contains('?'), "{text}");
    }

    /// The tuning `edoScale` wrote on the notes wins over the widget's
    /// options: its divisions and root draw the wheel, the divisions the
    /// scale leaves out are faint, and a sounding note is named with its
    /// interval.
    #[test]
    fn the_pitch_wheel_draws_the_events_scale_faint_where_the_scale_is_not() {
        let theme = theme();
        let scale = rustel_runtime::ui_events::UiEdoScale {
            edo: 12,
            root_hz: 261.63,
            degree_indexes: vec![0, 2, 4, 5, 7, 9, 11],
            interval_labels: ["P1", "M2", "M3", "P4", "P5", "M6", "M7"]
                .iter()
                .map(|label| (*label).to_owned())
                .collect(),
        };
        let chord = [261.63, 329.63, 392.0]
            .iter()
            .enumerate()
            .map(|(index, hertz)| {
                let mut note = pitched(index as u64 + 1, "0/1", "1/1", *hertz, 10.0);
                note.scale = Some(scale.clone());
                note
            })
            .collect();
        let state = playing("$: note(\"c4 e4 g4\")._pitchwheel()", chord, 0.0);
        let area = Rect::new(0, 0, 60, 12);
        // Options that would draw a five-division wheel rooted on A.
        let picture = draw("pitchwheel", "{edo: 5, root: 440}", &state, area);
        let faint = wash(theme.background, theme.grid, 0.15);
        assert!(
            cells_in(&picture, faint).len() >= 3,
            "the five divisions outside C major are faint:\n{}",
            rows(&picture)
        );
        assert!(
            !cells_in(&picture, theme.grid).is_empty(),
            "the scale's own marks stay"
        );
        let text = rows(&picture);
        assert!(
            !text.contains("edo"),
            "twelve divisions, from the event:\n{text}"
        );
        assert!(text.contains("C4 P1") && text.contains("G4 P5"), "{text}");
        // Without a scale on the events the options decide.
        let plain = playing(
            "$: note(\"c4 e4 g4\")._pitchwheel()",
            [261.63, 329.63, 392.0]
                .iter()
                .enumerate()
                .map(|(index, hertz)| pitched(index as u64 + 1, "0/1", "1/1", *hertz, 10.0))
                .collect(),
            0.0,
        );
        let picture = draw("pitchwheel", "{edo: 5, root: 440}", &plain, area);
        assert!(cells_in(&picture, faint).is_empty());
        assert!(rows(&picture).contains("5 edo"));
    }

    fn playing(source: &str, events: Vec<UiScheduledEvent>, cycle: f64) -> VisualState {
        let envelope = rustel_runtime::ui_events::visual_layout(source, 1).expect("layout");
        let revision = envelope.ui_layout.source_revision.clone();
        let mut state = VisualState::default();
        state.install_layout(envelope);
        state.start();
        let batch = UiEventBatch::new(10.0, cycle, 1.0, 1, revision, events, 0).expect("batch");
        assert!(state.install_batch(batch));
        state
    }

    fn audio(state: &mut VisualState, scope: [f32; UI_SCOPE_SAMPLES]) {
        let metadata = UiAudioMetadata {
            sequence: 1,
            generation: 1,
            device_time: 0.5,
            stream_id: 1,
            epoch: 1,
            end_frame: 24_000,
            sample_rate: 48_000,
        };
        assert!(state.install_audio(
            metadata,
            UiAudioAnalysisSet {
                master: UiAudioAnalysisFrame {
                    scope,
                    spectrum: [-120.0; UI_SPECTRUM_BINS],
                },
                visuals: Vec::new(),
                sides: Vec::new(),
            },
        ));
    }

    fn symbols(buffer: &Buffer) -> String {
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    /// Rows of glyphs, for looking at a render.
    fn rows(buffer: &Buffer) -> String {
        let width = usize::from(buffer.area.width);
        let all = symbols(buffer);
        let chars: Vec<char> = all.chars().collect();
        chars
            .chunks(width)
            .map(|row| row.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A silent note is a thin stroke and the sounding note fills its lane.
    /// The cycle rule never crosses a note, and a label sits inside its bar.
    #[test]
    fn a_silent_note_is_a_stroke_and_the_sounding_one_fills_its_lane() {
        let mut events = Vec::new();
        for (index, (midi, name)) in [(60.0, "c4"), (67.0, "g4")].iter().enumerate() {
            let mut event = event(
                index as u64,
                &format!("{index}/2"),
                &format!("{}/2", index + 1),
                10.0 + index as f64,
            );
            event.frequency_hz = Some(440.0 * 2f32.powf((midi - 69.0) / 12.0));
            event.label = Some((*name).to_owned());
            event.value = Some((*name).to_owned());
            event.duration_seconds = 0.9;
            events.push(event);
        }
        let state = playing("$: note(\"c4 g4\")._pianoroll()", events, 0.25);
        let text = rows(&draw(
            "pianoroll",
            "{cycles: 1, playhead: 0.25}",
            &state,
            Rect::new(0, 0, 40, 4),
        ));
        let lines: Vec<&str> = text.lines().collect();
        // Lane of c4 (bottom, sounding at device time 10): a full block.
        assert!(
            lines[2].contains("███") || lines[3].contains("███"),
            "{text}"
        );
        // Lane of g4 (top, silent): a half-height stroke, no full block.
        assert!(
            lines[0].contains("▄▄▄") || lines[1].contains("▄▄▄") || lines[0].contains("▀▀▀"),
            "{text}"
        );
        assert!(
            !lines[0].contains("███") && !lines[1].contains("███"),
            "{text}"
        );
        // Cycle rules are drawn only where nothing else is.
        assert!(text.contains('▏'), "{text}");

        let labelled = rows(&draw(
            "pianoroll",
            "{cycles: 1, playhead: 0.25, labels: 1}",
            &state,
            Rect::new(0, 0, 40, 4),
        ));
        // The pattern's own label wins, as on strudel.cc; a note without
        // one is written by name.
        let c4_row = labelled
            .lines()
            .position(|line| line.contains("c4"))
            .expect("c4 labelled");
        let g4_row = labelled
            .lines()
            .position(|line| line.contains("g4"))
            .expect("g4 labelled");
        assert!(
            c4_row > g4_row,
            "labels sit in their own lanes:\n{labelled}"
        );
        let c4_col = labelled.lines().nth(c4_row).unwrap().find("c4").unwrap();
        assert!(
            c4_col < 20,
            "the label sits on the bar, not off to the right:\n{labelled}"
        );
    }

    #[test]
    fn horizontal_half_blocks_keep_subcell_position_and_both_note_colors() {
        let area = Rect::new(7, 3, 4, 1);
        let mut canvas = Canvas::with_raster(area, Raster::HorizontalHalfBlocks);
        assert_eq!((canvas.width(), canvas.height()), (8, 1));
        canvas.set(0, 0, Color::Red);
        canvas.set(3, 0, Color::Blue);
        canvas.set(4, 0, Color::Red);
        canvas.set(5, 0, Color::Blue);
        let mut buffer = Buffer::empty(area);
        canvas.paint(&mut buffer, Color::Black);
        assert_eq!(buffer.cell((7, 3)).unwrap().symbol(), "▌");
        assert_eq!(buffer.cell((8, 3)).unwrap().symbol(), "▐");
        let mixed = buffer.cell((9, 3)).unwrap();
        assert_eq!(mixed.symbol(), "▌");
        assert_eq!((mixed.fg, mixed.bg), (Color::Red, Color::Blue));
        assert_eq!(buffer.cell((10, 3)).unwrap().symbol(), " ");
    }

    fn repeated_roll_events(count: usize, two_lanes: bool) -> Vec<UiScheduledEvent> {
        (0..count)
            .map(|index| {
                let mut hit = event(
                    index as u64,
                    &format!("{index}/{count}"),
                    &format!("{}/{count}", index + 1),
                    10.0 + index as f64 / count as f64,
                );
                hit.duration_seconds = 1.0 / count as f64;
                hit.frequency_hz = Some(if two_lanes && index % 2 == 1 {
                    880.0
                } else {
                    440.0
                });
                hit
            })
            .collect()
    }

    #[test]
    fn dense_single_lane_rolls_keep_visible_attacks_on_narrow_consoles() {
        let _symbols = crate::terminal::ForceSymbolsForTest::set(false);
        let state = playing(
            "$: s(\"sine*16\")._pianoroll()",
            repeated_roll_events(16, false),
            0.0,
        );
        for kind in ["pianoroll", "punchcard"] {
            for width in [8, 16, 32] {
                for flip in [0, 1] {
                    let options = format!("{{cycles: 1, playhead: 0, flipTime: {flip}}}");
                    let buffer = draw(kind, &options, &state, Rect::new(0, 0, width, 6));
                    let text = rows(&buffer);
                    assert!(
                        text.chars()
                            .all(|c| matches!(c, ' ' | '\n' | '|' | '█' | '▌' | '▐')),
                        "{text}"
                    );
                    let attacks = text
                        .lines()
                        .map(|row| {
                            row.chars()
                                .filter(|c| {
                                    if width == 8 {
                                        *c == '|'
                                    } else {
                                        matches!(c, '▌' | '▐')
                                    }
                                })
                                .count()
                        })
                        .max()
                        .unwrap();
                    assert!(
                        attacks >= usize::from(width.min(16)) - 2,
                        "{kind} width={width} flip={flip}:\n{text}"
                    );
                    assert!(!text.contains("████"), "separate hits merged: {text}");
                }
            }
        }
    }

    #[test]
    fn compressed_attack_ticks_do_not_overwrite_separate_pitch_lanes() {
        let state = playing(
            "$: note(\"a4 a5\")._pianoroll()",
            repeated_roll_events(32, true),
            0.0,
        );
        for tier in [crate::graphics::Tier::Cells, crate::graphics::Tier::Fine] {
            let buffer = draw_at(
                tier,
                "pianoroll",
                "{cycles: 1, playhead: 0}",
                &state,
                Rect::new(0, 0, 8, 1),
            );
            assert!(
                !symbols(&buffer).contains('|'),
                "whole-cell ticks erase half-row lanes"
            );
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|cell| !matches!(cell.symbol(), " " | "▏"))
            );
        }
    }

    #[test]
    fn sustained_clipped_notes_keep_their_body_and_dense_labels_and_playhead_win() {
        let mut held = event(1, "-1/1", "2/1", 9.0);
        held.duration_seconds = 3.0;
        let state = playing("$: s(\"sine\")._pianoroll()", vec![held], 0.0);
        for flip in [0, 1] {
            let options = format!("{{cycles: 1, playhead: 0, flipTime: {flip}}}");
            let buffer = draw("pianoroll", &options, &state, Rect::new(0, 0, 8, 6));
            let text = rows(&buffer);
            assert!(text.contains("███████"), "sustain lost its body: {text}");
            assert!(!text.contains('|'), "offscreen onset became a tick: {text}");
        }
        let mut hits = repeated_roll_events(16, false);
        for hit in &mut hits {
            hit.label = Some("n".into());
        }
        let state = playing("$: s(\"sine*16\")._pianoroll()", hits, 0.0);
        let buffer = draw(
            "pianoroll",
            "{cycles: 1, playhead: 0, labels: 1}",
            &state,
            Rect::new(0, 0, 8, 6),
        );
        assert!(symbols(&buffer).contains('n'), "labels survive ticks");
        for y in 0..6 {
            let cell = buffer.cell((0, y)).unwrap();
            assert_eq!(cell.symbol(), "▏");
            assert_eq!(cell.fg, theme().playhead);
        }
        for area in [Rect::new(0, 0, 0, 6), Rect::new(0, 0, 8, 0)] {
            assert!(draw("pianoroll", "", &state, area).content.is_empty());
        }
    }

    /// The graphics tier is a process-wide setting and the tests run in
    /// parallel: a test that draws at the default tier (Cells) holds this
    /// for reading, one that switches tiers holds it for writing and
    /// switches back before letting go.
    static TIER_GATE: std::sync::RwLock<()> = std::sync::RwLock::new(());

    /// Run `body` with the tier set to `tier`, alone, and restore Cells.
    fn at_tier<T>(tier: crate::graphics::Tier, body: impl FnOnce() -> T) -> T {
        /// Puts Cells back when the body is done - or when it panics, so a
        /// failing test does not leave the tier changed for the next one.
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::graphics::set_tier(crate::graphics::Tier::Cells);
            }
        }
        let _gate = TIER_GATE
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let _restore = Restore;
        crate::graphics::set_tier(tier);
        body()
    }

    fn draw_at(
        tier: crate::graphics::Tier,
        kind: &str,
        options: &str,
        state: &VisualState,
        area: Rect,
    ) -> Buffer {
        at_tier(tier, || render_now(kind, options, state, area))
    }

    /// Pin the tier to Cells - the tests' baseline, whatever the process
    /// default is - for as long as the guard lives.
    fn cells_tier() -> std::sync::RwLockWriteGuard<'static, ()> {
        let gate = TIER_GATE
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        crate::graphics::set_tier(crate::graphics::Tier::Cells);
        gate
    }

    fn draw(kind: &str, options: &str, state: &VisualState, area: Rect) -> Buffer {
        let _gate = cells_tier();
        render_now(kind, options, state, area)
    }

    fn render_now(kind: &str, options: &str, state: &VisualState, area: Rect) -> Buffer {
        let theme = theme();
        let parsed = VisualOptions::parse(options);
        let mut buffer = Buffer::empty(area);
        render(
            VisualRequest {
                kind,
                slot: None,
                options: &parsed,
                state,
                theme: &theme,
                background: theme.background,
                inline: true,
            },
            area,
            &mut buffer,
        );
        buffer
    }

    /// Braille has no brightness of its own, so a picture drawn in it has
    /// to shade by counting dots. Empty and full are the ends; everything
    /// between is a count, and two cells at the same grey do not light the
    /// same dots or a flat wall would read as a grid.
    #[test]
    fn braille_luma_glyphs_shade_rather_than_threshold() {
        assert_eq!(braille_luma_glyph([[0; 2]; 4]), '\u{2800}');
        assert_eq!(braille_luma_glyph([[255; 2]; 4]), '\u{28ff}');
        let grey = braille_luma_glyph([[128; 2]; 4]);
        assert!(
            grey != '\u{2800}' && grey != '\u{28ff}',
            "a mid grey is neither empty nor full: {grey:?}"
        );
        // Every dot of the ordered matrix has its own threshold, so the
        // eight samples of one cell do not all cross at once.
        for level in [40u8, 90, 150, 200] {
            let glyph = braille_luma_glyph([[level; 2]; 4]);
            assert!(
                (0x2800..=0x28ff).contains(&(glyph as u32)),
                "{glyph:?} is not Braille"
            );
        }
        let counts: Vec<u32> = [0u8, 64, 128, 192, 255]
            .into_iter()
            .map(|level| (braille_luma_glyph([[level; 2]; 4]) as u32 - 0x2800).count_ones())
            .collect();
        assert!(
            counts.windows(2).all(|pair| pair[0] <= pair[1]),
            "more light must never mean fewer dots: {counts:?}"
        );
        assert_eq!(counts.first(), Some(&0));
        assert_eq!(counts.last(), Some(&8));
    }

    #[test]
    fn a_block_grid_writes_solid_halves_and_leaves_empty_cells_alone() {
        let _tier = cells_tier();
        let area = Rect::new(0, 0, 3, 2);
        let mut grid = Canvas::bars(area);
        assert_eq!((grid.width(), grid.height()), (3, 4));
        grid.set(0, 0, Color::Rgb(1, 2, 3));
        grid.set(0, 1, Color::Rgb(1, 2, 3));
        grid.set(1, 0, Color::Rgb(9, 9, 9));
        grid.set(2, 1, Color::Rgb(4, 4, 4));
        // Out of range writes are ignored rather than panicking.
        grid.set(99, 99, Color::White);

        let mut buffer = Buffer::empty(area);
        grid.paint(&mut buffer, Color::Black);
        assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "█");
        assert_eq!(buffer.cell((1, 0)).unwrap().symbol(), "▀");
        assert_eq!(buffer.cell((1, 0)).unwrap().bg, Color::Black);
        assert_eq!(buffer.cell((2, 0)).unwrap().symbol(), "▄");
        assert_eq!(buffer.cell((0, 1)).unwrap().symbol(), " ");
    }

    #[test]
    fn two_different_colours_in_one_cell_keep_both_halves() {
        let _tier = cells_tier();
        let area = Rect::new(0, 0, 1, 1);
        let mut grid = Canvas::bars(area);
        grid.set(0, 0, Color::Rgb(255, 0, 0));
        grid.set(0, 1, Color::Rgb(0, 0, 255));
        let mut buffer = Buffer::empty(area);
        grid.paint(&mut buffer, Color::Black);
        let cell = buffer.cell((0, 0)).unwrap();
        assert_eq!(cell.symbol(), "▀");
        assert_eq!(cell.fg, Color::Rgb(255, 0, 0));
        assert_eq!(cell.bg, Color::Rgb(0, 0, 255));
    }

    #[test]
    fn a_line_between_two_points_has_no_gaps() {
        let _tier = cells_tier();
        let area = Rect::new(0, 0, 8, 8);
        let mut grid = Canvas::bars(area);
        grid.line((0, 0), (7, 15), Color::White);
        for x in 0..8 {
            assert!(
                (0..16).any(|y| grid.get(x, y).is_some()),
                "column {x} was skipped"
            );
        }
    }

    #[test]
    fn the_piano_roll_draws_solid_bars_and_a_playhead_not_braille() {
        let state = playing(
            "$: s(\"bd sd\")._pianoroll()",
            vec![event(1, "0/1", "1/2", 10.0), event(2, "1/2", "1/1", 10.5)],
            0.75,
        );
        let buffer = draw("pianoroll", "", &state, Rect::new(0, 0, 40, 6));
        let text = symbols(&buffer);
        assert!(text.contains('█'), "expected solid blocks: {text:?}");
        assert!(
            !text
                .chars()
                .any(|glyph| ('\u{2800}'..='\u{28ff}').contains(&glyph)),
            "no Braille dots should remain"
        );
        // The playhead sits at the middle of the pane by default.
        let playhead_column = (0..6)
            .filter_map(|y| buffer.cell((20, y)))
            .filter(|cell| cell.fg == theme().playhead)
            .count();
        assert!(playhead_column > 0, "the playhead is missing");
    }

    #[test]
    fn a_punchcard_labels_its_lanes() {
        let state = playing(
            "$: s(\"bd\")._punchcard({labels: 1})",
            vec![event(1, "0/1", "1/1", 10.0)],
            0.5,
        );
        let text = symbols(&draw(
            "punchcard",
            "{labels: 1}",
            &state,
            Rect::new(0, 0, 40, 6),
        ));
        assert!(text.contains("bd"), "expected an event label: {text:?}");
        let bare = symbols(&draw("punchcard", "", &state, Rect::new(0, 0, 40, 6)));
        assert!(
            !bare.contains("bd"),
            "no labels unless asked, as on strudel.cc: {bare:?}"
        );
    }

    #[test]
    fn piano_roll_options_move_the_playhead_and_change_the_window() {
        let state = playing(
            "$: s(\"bd\")._pianoroll()",
            vec![event(1, "0/1", "1/1", 10.0)],
            0.5,
        );
        let theme = theme();
        let left = draw(
            "pianoroll",
            "{ playhead: 0, cycles: 2 }",
            &state,
            Rect::new(0, 0, 40, 4),
        );
        let lit = |buffer: &Buffer, x: u16| {
            (0..4)
                .filter_map(|y| buffer.cell((x, y)))
                .any(|cell| cell.fg == theme.playhead)
        };
        assert!(lit(&left, 0), "playhead 0 pins the head to the left edge");
        assert!(!lit(&left, 20));
    }

    #[test]
    fn a_triggered_scope_starts_at_a_rising_zero_crossing() {
        let mut scope = [0.0f32; UI_SCOPE_SAMPLES];
        for (index, sample) in scope.iter_mut().enumerate() {
            *sample = (std::f32::consts::TAU * 4.0 * index as f32 / UI_SCOPE_SAMPLES as f32).sin();
        }
        // A trace that starts a quarter turn in has no zero at index zero.
        let mut shifted = scope;
        shifted.rotate_left(37);
        assert_ne!(rising_zero_crossing(&shifted), 0);
        assert!(shifted[rising_zero_crossing(&shifted)] > 0.0);

        let mut state = playing("$: s(\"bd\")._scope()", Vec::new(), 0.0);
        audio(&mut state, shifted);
        // The scope draws as a Braille line at the Cells tier and at Fine.
        let text = symbols(&draw("tscope", "", &state, Rect::new(0, 0, 30, 4)));
        assert!(
            is_braille_line(&text),
            "the scope is a Braille line at Cells: {text:?}"
        );
        let fine = symbols(&draw_at(
            crate::graphics::Tier::Fine,
            "tscope",
            "",
            &state,
            Rect::new(0, 0, 30, 4),
        ));
        assert!(
            is_braille_line(&fine),
            "the same scope is Braille at Fine: {fine:?}"
        );
    }

    /// An inline scope without audio says so in its own band.
    #[test]
    fn a_scope_without_audio_says_so_instead_of_drawing_nothing() {
        let state = playing("$: s(\"bd\")._scope()", Vec::new(), 0.0);
        let text = symbols(&draw("scope", "", &state, Rect::new(0, 0, 30, 4)));
        assert!(text.contains("waiting for audio"), "{text:?}");
    }

    /// The same two notes as above, on their side: lanes stand next to
    /// each other, time falls from the top, the playhead is a line across.
    #[test]
    fn vertical_lays_the_lanes_side_by_side_with_time_falling_down() {
        let mut events = Vec::new();
        for (index, (midi, name)) in [(60.0, "c4"), (67.0, "g4")].iter().enumerate() {
            let mut event = event(
                index as u64,
                &format!("{index}/2"),
                &format!("{}/2", index + 1),
                10.0 + index as f64,
            );
            event.frequency_hz = Some(440.0 * 2f32.powf((midi - 69.0) / 12.0));
            event.label = Some((*name).to_owned());
            event.value = Some((*name).to_owned());
            event.duration_seconds = 0.9;
            events.push(event);
        }
        let state = playing("$: note(\"c4 g4\")._pianoroll()", events, 0.25);
        let text = rows(&draw(
            "pianoroll",
            "{cycles: 1, playhead: 0.25, vertical: 1}",
            &state,
            Rect::new(0, 0, 20, 8),
        ));
        let lines: Vec<&str> = text.lines().collect();
        let blocks = |line: &str, from: usize, to: usize| {
            line.chars()
                .skip(from)
                .take(to - from)
                .filter(|glyph| matches!(glyph, '█' | '▀' | '▄'))
                .count()
        };
        // c4 (lane 0, left; sounding, cycle 0..0.5 = the bottom half) is a
        // wide bar at the bottom left; g4 (lane 1, right; silent, 0.5..1 =
        // the top half) a narrower stroke at the top right.
        assert!(
            blocks(lines[7], 0, 10) >= 4,
            "c4 fills its lane at the bottom:\n{text}"
        );
        assert_eq!(
            blocks(lines[7], 10, 20),
            0,
            "nothing of g4 at the bottom:\n{text}"
        );
        assert!(
            blocks(lines[0], 10, 20) >= 2,
            "g4 is a stroke at the top right:\n{text}"
        );
        assert!(
            blocks(lines[0], 10, 20) < blocks(lines[7], 0, 10),
            "the silent note is the narrower one:\n{text}"
        );
        assert_eq!(
            blocks(lines[0], 0, 10),
            0,
            "nothing of c4 at the top:\n{text}"
        );
        // The playhead lies across the lanes, a quarter of the way up.
        assert!(
            lines[5].chars().skip(10).any(|glyph| glyph == '▄'),
            "the playhead runs across the lanes:\n{text}"
        );

        // `wordfall` is the same roll on its side with labels on.
        let fall = rows(&draw(
            "wordfall",
            "{cycles: 1, playhead: 0.25}",
            &state,
            Rect::new(0, 0, 20, 8),
        ));
        assert!(
            fall.contains("c4") && fall.contains("g4"),
            "wordfall labels:\n{fall}"
        );
        let c4_row = fall.lines().position(|line| line.contains("c4")).unwrap();
        let g4_row = fall.lines().position(|line| line.contains("g4")).unwrap();
        assert!(c4_row > g4_row, "the sooner note sits lower:\n{fall}");
    }

    /// `flipTime` turns time around on either axis.
    #[test]
    fn flip_time_runs_the_roll_backwards() {
        let mut event = event(0, "0/1", "1/2", 10.0);
        event.frequency_hz = Some(261.6);
        event.duration_seconds = 0.9;
        let state = playing("$: note(\"c4\")._pianoroll()", vec![event], 0.25);
        let blocks_at = |options: &str| {
            let text = rows(&draw("pianoroll", options, &state, Rect::new(0, 0, 20, 2)));
            let line = text
                .lines()
                .find(|line| line.contains('█'))
                .unwrap_or("")
                .to_owned();
            (
                line.find('█').unwrap_or(usize::MAX),
                line.rfind('█').unwrap_or(0),
            )
        };
        let (left, _) = blocks_at("{cycles: 1, playhead: 0}");
        let (_, right) = blocks_at("{cycles: 1, playhead: 0, flipTime: 1}");
        assert!(
            left < 10,
            "the note begins at the left when time runs right"
        );
        assert!(right >= 10, "flipped, the note begins at the right");
    }

    /// Every audio tap keeps a column history whether or not a spectrogram
    /// draws it, so a full history must hold its size: pushing before
    /// popping made each full ring grow to twice the columns it keeps.
    #[test]
    fn a_full_spectrogram_history_keeps_its_size() {
        let mut state = playing("$: s(\"bd\")._spectrum()", Vec::new(), 0.0);
        let frames = SPECTROGRAM_COLUMNS as u64 + 300;
        for sequence in 1..=frames {
            let mut spectrum = [-120.0; UI_SPECTRUM_BINS];
            spectrum[0] = -((sequence % 100) as f32);
            assert!(state.install_audio(
                UiAudioMetadata {
                    sequence,
                    generation: 1,
                    device_time: 0.01 * sequence as f64,
                    stream_id: 1,
                    epoch: 1,
                    end_frame: 480 * sequence,
                    sample_rate: 48_000,
                },
                UiAudioAnalysisSet {
                    master: UiAudioAnalysisFrame {
                        scope: [0.0; UI_SCOPE_SAMPLES],
                        spectrum,
                    },
                    visuals: Vec::new(),
                    sides: Vec::new(),
                },
            ));
        }
        let ring = state.spectrogram(None).expect("the master's history");
        assert_eq!(ring.len(), SPECTROGRAM_COLUMNS);
        assert!(
            ring.capacity() < 2 * SPECTROGRAM_COLUMNS,
            "a history of {SPECTROGRAM_COLUMNS} columns holds room for {}",
            ring.capacity()
        );
        let newest = spectrogram_column(&UiAudioAnalysisFrame {
            scope: [0.0; UI_SCOPE_SAMPLES],
            spectrum: {
                let mut spectrum = [-120.0; UI_SPECTRUM_BINS];
                spectrum[0] = -((frames % 100) as f32);
                spectrum
            },
        });
        assert_eq!(ring.back(), Some(&newest), "the newest column is last");
        // What the memory breakdown reads is the room the ring holds, not
        // the columns in it.
        assert_eq!(
            state.history_bytes(),
            ring.capacity() * SPECTROGRAM_BANDS * std::mem::size_of::<f32>()
        );
    }

    /// strudel.cc's spectrum is a spectrogram: each audio frame is a column,
    /// the newest at the right edge, low frequencies at the bottom.
    #[test]
    fn the_spectrum_scrolls_frames_in_from_the_right_low_bands_at_the_bottom() {
        let mut state = playing("$: s(\"bd\")._spectrum()", Vec::new(), 0.0);
        let mut install = |sequence: u64, spectrum: [f32; UI_SPECTRUM_BINS]| {
            state.install_audio(
                UiAudioMetadata {
                    sequence,
                    generation: 1,
                    device_time: 0.5 * sequence as f64,
                    stream_id: 1,
                    epoch: 1,
                    end_frame: 24_000 * sequence,
                    sample_rate: 48_000,
                },
                UiAudioAnalysisSet {
                    master: UiAudioAnalysisFrame {
                        scope: [0.0; UI_SCOPE_SAMPLES],
                        spectrum,
                    },
                    visuals: Vec::new(),
                    sides: Vec::new(),
                },
            )
        };
        assert!(install(1, [-120.0; UI_SPECTRUM_BINS]));
        let mut loud_low = [-120.0; UI_SPECTRUM_BINS];
        for bin in loud_low.iter_mut().take(4) {
            *bin = -6.0;
        }
        assert!(install(2, loud_low));
        let area = Rect::new(0, 0, 20, 4);
        // The picture is a ground with light on it: every cell it reaches
        // is drawn, and a quiet one is drawn in the ground's own colour.
        // So "lit" is a colour, not a glyph.
        let ground = theme().background;
        let energised = |colour: Color| colour != ground && colour != Color::Reset;
        let lit = |buffer: &Buffer, x: u16, y: u16| {
            buffer
                .cell((x, y))
                .is_some_and(|cell| energised(cell.fg) || energised(cell.bg))
        };
        let picture = draw("spectrum", "{ scroll: 1 }", &state, area);
        assert!(
            lit(&picture, 19, 3),
            "the newest frame lights the bottom right:\n{}",
            rows(&picture)
        );
        assert!(
            !lit(&picture, 19, 0),
            "only the low bands are loud:\n{}",
            rows(&picture)
        );
        assert!(
            !lit(&picture, 18, 3),
            "the quiet frame before it is dark:\n{}",
            rows(&picture)
        );
        assert!(
            picture
                .cell((0, 0))
                .is_some_and(|cell| cell.symbol() == " "),
            "and the pane is left alone where the ring does not reach:\n{}",
            rows(&picture)
        );
        // `speed` is how many columns a frame advances.
        let wide = draw("spectrum", "{ scroll: 1, speed: 2 }", &state, area);
        assert!(
            lit(&wide, 18, 3) && lit(&wide, 19, 3),
            "speed 2 makes a frame two columns wide:\n{}",
            rows(&wide)
        );
        assert!(!lit(&wide, 17, 3), "{}", rows(&wide));
    }

    fn pitched(index: u64, begin: &str, end: &str, hertz: f32, at: f64) -> UiScheduledEvent {
        let mut event = event(index, begin, end, at);
        event.frequency_hz = Some(hertz);
        event.duration_seconds = 0.9;
        event
    }

    fn cells_in(buffer: &Buffer, color: Color) -> Vec<(u16, u16)> {
        let mut cells = Vec::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if buffer.cell((x, y)).is_some_and(|cell| cell.fg == color) {
                    cells.push((x, y));
                }
            }
        }
        cells
    }

    fn painted(buffer: &Buffer) -> usize {
        buffer
            .content
            .iter()
            .filter(|cell| cell.symbol() != " ")
            .count()
    }

    /// strudel.cc's spiral units: with `steady` (the default) the track
    /// turns with the clock so the playhead tick moves; `steady: 0` keeps
    /// it put. `stretch` is turns per cycle, so the track grows with it.
    #[test]
    fn the_spiral_turns_with_the_clock_unless_told_not_to_and_stretch_is_turns_per_cycle() {
        let theme = theme();
        let area = Rect::new(0, 0, 24, 12);
        let events = || vec![pitched(1, "0/1", "1/2", 261.6, 10.0)];
        let tick = |options: &str, cycle: f64| {
            let state = playing("$: note(\"c4\")._spiral()", events(), cycle);
            cells_in(&draw("spiral", options, &state, area), theme.playhead)
        };
        assert!(!tick("", 0.0).is_empty(), "the playhead tick is drawn");
        assert_ne!(
            tick("", 0.0),
            tick("", 0.3),
            "steady: the tick orbits as the clock turns"
        );
        assert_eq!(
            tick("{steady: 0}", 0.0),
            tick("{steady: 0}", 0.3),
            "steady 0: the tick stays put"
        );

        let state = playing("$: note(\"c4\")._spiral()", events(), 0.0);
        let track =
            |options: &str| cells_in(&draw("spiral", options, &state, area), theme.grid).len();
        assert!(
            track("{stretch: 2}") > track("{stretch: 1}"),
            "two turns a cycle is a longer track than one"
        );
    }

    /// An arc fades with its distance from now; `fade: 0` keeps every arc
    /// at full strength, and `colorizeInactive` lets a silent arc keep the
    /// pattern's colour instead of the inactive one.
    #[test]
    fn spiral_arcs_fade_with_distance_and_keep_their_colour_only_when_asked() {
        let theme = theme();
        let area = Rect::new(0, 0, 24, 12);
        // Two silent notes: one a tenth of a cycle ahead, one nearly two.
        let near = pitched(1, "1/10", "1/5", 261.6, 30.0);
        let mut far = pitched(2, "19/10", "2/1", 261.6, 40.0);
        far.color = Some("red".into());
        let state = playing("$: note(\"c4\")._spiral()", vec![near, far], 0.0);
        let inactive = dim(theme.event_inactive, &theme);
        let full = cells_in(&draw("spiral", "{fade: 0}", &state, area), inactive).len();
        let faded = cells_in(&draw("spiral", "", &state, area), inactive).len();
        assert!(
            faded < full,
            "the far arc is mixed toward the background ({faded} < {full})"
        );
        let red = dim(parse_color("red").unwrap(), &theme);
        assert!(cells_in(&draw("spiral", "{fade: 0}", &state, area), red).is_empty());
        assert!(
            !cells_in(
                &draw("spiral", "{fade: 0, colorizeInactive: 1}", &state, area),
                red
            )
            .is_empty(),
            "colorizeInactive keeps the pattern's colour on a silent arc"
        );
    }

    /// strudel.cc's `root` is a frequency: `root: 440` puts A at twelve
    /// o'clock; with the default C root the same A sits on the left.
    #[test]
    fn the_pitch_wheel_root_is_a_frequency_at_twelve_o_clock() {
        let theme = theme();
        let area = Rect::new(0, 0, 60, 12);
        let state = playing(
            "$: note(\"a4\")._pitchwheel()",
            vec![pitched(1, "0/1", "1/1", 440.0, 10.0)],
            0.0,
        );
        let disc = |options: &str| {
            // The wheel is 24 cells wide; the legend names the note to
            // its right in the same colour and is not the disc.
            let mut cells = cells_in(
                &draw("pitchwheel", options, &state, area),
                brighten(theme.event),
            );
            cells.retain(|(x, _)| *x < 24);
            assert!(!cells.is_empty(), "the sounding note is drawn: {options}");
            let n = cells.len() as f32;
            (
                cells.iter().map(|(x, _)| f32::from(*x)).sum::<f32>() / n,
                cells.iter().map(|(_, y)| f32::from(*y)).sum::<f32>() / n,
            )
        };
        // The wheel is 12 rows tall at the left; its centre is near (6, 5.5).
        let (_, y_at_a_root) = disc("{root: 440}");
        assert!(
            y_at_a_root < 4.0,
            "A at the top with root 440: y {y_at_a_root}"
        );
        let (x_default, _) = disc("");
        assert!(x_default < 5.0, "A on the left of a C wheel: x {x_default}");
        let (_, y_named) = disc("{root: 'a'}");
        assert!(
            y_named < 4.0,
            "a note name is accepted as the root: y {y_named}"
        );
    }

    /// `hapcircles: 0` drops the discs, `circle: 1` draws the ring, and
    /// `mode: 'polygon'` joins the sounding notes instead of drawing hands.
    #[test]
    fn the_pitch_wheel_honours_hapcircles_circle_and_polygon() {
        let area = Rect::new(0, 0, 60, 12);
        let chord = vec![
            pitched(1, "0/1", "1/1", 261.6, 10.0),
            pitched(2, "0/1", "1/1", 329.6, 10.0),
            pitched(3, "0/1", "1/1", 392.0, 10.0),
        ];
        let state = playing("$: note(\"c4 e4 g4\")._pitchwheel()", chord, 0.0);
        let count = |options: &str| painted(&draw("pitchwheel", options, &state, area));
        assert!(count("{hapcircles: 0}") < count(""), "no discs is less ink");
        assert!(count("{circle: 1}") > count(""), "the ring is more ink");
        let none = count("{mode: 'none', hapcircles: 0}");
        assert!(
            count("{mode: 'polygon', hapcircles: 0}") > none,
            "the polygon joins the notes"
        );
        assert!(
            count("{mode: 'flake', hapcircles: 0}") > none,
            "the hands reach the notes"
        );
    }

    /// `scope` and `tscope` are one aligned scope: a shifted buffer of the
    /// same tone draws the same picture, `align: 0` shows it as it came,
    /// `trigger` is the level the trace starts at, and `pos` places it.
    #[test]
    fn the_scope_fills_its_pane_unless_a_score_names_a_scale() {
        // A tone at a twelfth of full scale: a point or two tall in four
        // rows, and within the gain a fit is allowed.
        let mut quiet = [0.0f32; UI_SCOPE_SAMPLES];
        for (index, sample) in quiet.iter_mut().enumerate() {
            *sample =
                (std::f32::consts::TAU * 4.0 * index as f32 / UI_SCOPE_SAMPLES as f32).sin() * 0.08;
        }
        let mut state = playing("$: s(\"bd\")._scope()", Vec::new(), 0.0);
        audio(&mut state, quiet);
        let area = Rect::new(0, 0, 30, 4);
        let rows_used = |options: &str| {
            let buffer = draw("scope", options, &state, area);
            (0..area.height)
                .filter(|y| {
                    (0..area.width).any(|x| buffer.cell((x, *y)).is_some_and(|c| c.symbol() != " "))
                })
                .count()
        };
        // Fitted by default: the trace is brought up to the pane.
        assert!(rows_used("") >= 3, "the quiet tone fills its rows");
        // A scale named turns the fitting off, and the tone lies flat.
        assert!(
            rows_used("{scale: 1}") <= 2,
            "a named scale is the scale asked for"
        );
        assert!(rows_used("{fit: 0}") <= 2, "and fit says so outright");
        assert!(rows_used("{scale: 1, fit: 1}") >= 3, "either way round");

        // The gain has a ceiling, and silence is left flat rather than
        // magnified into a picture of the noise floor.
        assert_eq!(fit_scale(&[0.0, 0.0, 0.0]), 1.0);
        assert_eq!(fit_scale(&[0.0, 0.0005, -0.001]), 1.0, "silence is silence");
        assert_eq!(fit_scale(&[0.5, -0.25]), FIT_HEADROOM / 0.5);
        assert_eq!(fit_scale(&[0.01, -0.004]), FIT_MOST, "no further than this");

        // Logarithmic amplitude: full scale at the edge, the floor on the
        // centre line, the sign kept.
        assert_eq!(decibel_amplitude(1.0), 1.0);
        assert_eq!(decibel_amplitude(0.0), 0.0);
        assert_eq!(decibel_amplitude(-1.0), -1.0);
        assert!(
            (decibel_amplitude(0.001) - 0.0).abs() < 1e-6,
            "-60 dB is the floor"
        );
        let half = decibel_amplitude(0.5);
        assert!(
            (half - 0.8996).abs() < 0.001,
            "-6 dB is most of the way out: {half}"
        );
        assert!(decibel_amplitude(-0.5) < 0.0, "the sign is kept");
        // Drawn, a quiet tone reaches further in decibels than straight.
        let straight = rows_used("{scale: 1}");
        assert!(
            rows_used("{scale: 1, log: 1}") > straight,
            "log lifts what linear leaves flat"
        );
        assert!(
            rows_used("{scale: 1, db: 1}") > straight,
            "db is the same word"
        );
    }

    #[test]
    fn the_scope_aligns_by_default_on_a_falling_crossing_and_honours_trigger_and_pos() {
        let mut tone = [0.0f32; UI_SCOPE_SAMPLES];
        for (index, sample) in tone.iter_mut().enumerate() {
            *sample = (std::f32::consts::TAU * 4.0 * index as f32 / UI_SCOPE_SAMPLES as f32).sin();
        }
        let mut shifted = tone;
        shifted.rotate_left(37);
        let start = falling_crossing(&shifted, 0.0);
        assert!(
            shifted[start - 1] > 0.0 && shifted[start] <= 0.0,
            "a falling crossing"
        );
        let start = falling_crossing(&shifted, 0.5);
        assert!(
            shifted[start - 1] > -0.5 && shifted[start] <= -0.5,
            "trigger is a level"
        );

        let mut straight = playing("$: s(\"bd\")._scope()", Vec::new(), 0.0);
        audio(&mut straight, tone);
        let mut rotated = playing("$: s(\"bd\")._scope()", Vec::new(), 0.0);
        audio(&mut rotated, shifted);
        let area = Rect::new(0, 0, 30, 4);
        // Aligned traces start at the same phase; they end where their
        // data ends, so only the left of the picture is compared.
        let left = |buffer: &Buffer| {
            rows(buffer)
                .lines()
                .map(|line| line.chars().take(20).collect::<String>())
                .collect::<Vec<_>>()
        };
        let picture =
            |state: &VisualState, options: &str| left(&draw("scope", options, state, area));
        assert_eq!(
            picture(&straight, ""),
            picture(&rotated, ""),
            "aligned, the tone stands still"
        );
        assert_eq!(
            picture(&straight, ""),
            left(&draw("tscope", "", &straight, area))
        );
        assert_ne!(
            picture(&straight, "{align: 0}"),
            picture(&rotated, "{align: 0}"),
            "unaligned, the shift shows"
        );
        assert_ne!(picture(&straight, ""), picture(&straight, "{trigger: 0.5}"));

        let rows_used = |options: &str| {
            let buffer = draw("scope", options, &straight, area);
            (0..area.height)
                .filter(|y| {
                    (0..area.width).any(|x| buffer.cell((x, *y)).is_some_and(|c| c.symbol() != " "))
                })
                .collect::<Vec<_>>()
        };
        assert!(
            !rows_used("{pos: 0.05, scale: 0.1}").contains(&3),
            "pos near 0 keeps off the bottom row"
        );
        assert!(
            !rows_used("{pos: 0.95, scale: 0.1}").contains(&0),
            "pos near 1 keeps off the top row"
        );
    }

    /// The spectrogram is half blocks on the Fine tier too (a sextant cell
    /// keeps one colour), a row pools every band it covers, and it draws in
    /// the pattern's own colour when the pattern set one.
    #[test]
    fn the_spectrogram_keeps_its_shades_on_fine_pools_its_rows_and_takes_the_pattern_colour() {
        let theme = theme();
        let mut state = playing("$: s(\"bd\")._spectrum()", Vec::new(), 0.0);
        let mut spectrum = [-120.0; UI_SPECTRUM_BINS];
        // One narrow peak, in bins no single band row would sample alone.
        for bin in spectrum.iter_mut().skip(39).take(5) {
            *bin = -6.0;
        }
        assert!(state.install_audio(
            UiAudioMetadata {
                sequence: 1,
                generation: 1,
                device_time: 0.5,
                stream_id: 1,
                epoch: 1,
                end_frame: 24_000,
                sample_rate: 48_000,
            },
            UiAudioAnalysisSet {
                master: UiAudioAnalysisFrame {
                    scope: [0.0; UI_SCOPE_SAMPLES],
                    spectrum,
                },
                visuals: Vec::new(),
                sides: Vec::new(),
            },
        ));
        let area = Rect::new(0, 0, 20, 4);
        let fine = draw_at(
            crate::graphics::Tier::Fine,
            "spectrum",
            "{ scroll: 1 }",
            &state,
            area,
        );
        let glyphs = symbols(&fine);
        assert!(
            glyphs.chars().any(|glyph| matches!(glyph, '▀' | '▄' | '█')),
            "half blocks on Fine: {glyphs:?}"
        );
        assert!(
            !glyphs
                .chars()
                .any(|glyph| ('\u{1fb00}'..='\u{1fbff}').contains(&glyph)),
            "no sextants: {glyphs:?}"
        );
        assert!(
            fine.cell((19, 1)).is_some_and(|cell| cell.symbol() != " "),
            "the peak lights its row:\n{}",
            rows(&fine)
        );

        // The pattern's colour wins over the accent.
        let plain = draw("spectrum", "{ scroll: 1 }", &state, area);
        let mut red = event(1, "0/1", "1/1", 10.0);
        red.color = Some("red".into());
        let mut coloured = playing("$: s(\"bd\")._spectrum()", vec![red], 0.0);
        assert!(coloured.install_audio(
            UiAudioMetadata {
                sequence: 1,
                generation: 1,
                device_time: 0.5,
                stream_id: 1,
                epoch: 1,
                end_frame: 24_000,
                sample_rate: 48_000,
            },
            UiAudioAnalysisSet {
                master: UiAudioAnalysisFrame {
                    scope: [0.0; UI_SCOPE_SAMPLES],
                    spectrum,
                },
                visuals: Vec::new(),
                sides: Vec::new(),
            },
        ));
        let tinted = draw("spectrum", "{ scroll: 1 }", &coloured, area);
        let lit = |buffer: &Buffer| buffer.cell((19, 1)).map(|cell| (cell.fg, cell.bg)).unwrap();
        assert_ne!(
            lit(&plain),
            lit(&tinted),
            "the pattern's .color() tints the spectrogram"
        );
        let _ = theme;
    }

    fn analyser_state(frames: &[[f32; UI_SPECTRUM_BINS]]) -> VisualState {
        let mut state = playing("$: s(\"bd\")._spectrum()", Vec::new(), 0.0);
        for (index, spectrum) in frames.iter().enumerate() {
            let sequence = index as u64 + 1;
            assert!(state.install_audio(
                UiAudioMetadata {
                    sequence,
                    generation: 1,
                    device_time: 0.5 * sequence as f64,
                    stream_id: 1,
                    epoch: 1,
                    end_frame: 24_000 * sequence,
                    sample_rate: 48_000,
                },
                UiAudioAnalysisSet {
                    master: UiAudioAnalysisFrame {
                        scope: [0.0; UI_SCOPE_SAMPLES],
                        spectrum: *spectrum,
                    },
                    visuals: Vec::new(),
                    sides: Vec::new(),
                },
            ));
        }
        state
    }

    fn column_height(buffer: &Buffer, x: u16) -> usize {
        (0..buffer.area.height)
            .filter(|y| {
                buffer
                    .cell((x, *y))
                    .is_some_and(|cell| cell.symbol() != " ")
            })
            .count()
    }

    /// The spectrum is an analyser: bars across the whole width, low
    /// frequencies at the left, tall where the sound is and empty where it
    /// is not - whatever the pane's width.
    #[test]
    fn the_spectrum_is_bars_that_fill_the_width_low_on_the_left() {
        let mut low = [-120.0; UI_SPECTRUM_BINS];
        for bin in low.iter_mut().take(3) {
            *bin = -3.0;
        }
        let state = analyser_state(&[low]);
        for width in [24_u16, 80, 200] {
            let area = Rect::new(0, 0, width, 4);
            let picture = draw("spectrum", "", &state, area);
            assert!(
                column_height(&picture, 0) >= 3,
                "the low bar stands at the left of a {width}-wide pane:\n{}",
                rows(&picture)
            );
            assert_eq!(
                column_height(&picture, width - 1),
                0,
                "nothing sounds up top, so the right stays empty:\n{}",
                rows(&picture)
            );
        }
        // Every band loud: bars reach the right edge of any width.
        let loud = [-3.0; UI_SPECTRUM_BINS];
        let state = analyser_state(&[loud]);
        for width in [24_u16, 80, 200] {
            let picture = draw("spectrum", "", &state, Rect::new(0, 0, width, 4));
            assert!(
                (0..width).all(|x| column_height(&picture, x) == 4),
                "the bars fill a {width}-wide pane without gaps:\n{}",
                rows(&picture)
            );
        }
    }

    /// A dim keeps 45% of its colour on a coloured ground, the share the
    /// painters mixed toward the background before the terminal themes.
    #[test]
    fn a_dim_keeps_45_percent_of_its_colour_on_a_coloured_ground() {
        let mut theme = theme();
        theme.background = Color::Rgb(20, 20, 20);
        // 20 + (128 - 20) * 0.45 = 68.6, rounded to 69.
        assert_eq!(
            dim(Color::Rgb(128, 128, 128), &theme),
            Color::Rgb(69, 69, 69)
        );
    }

    #[test]
    fn fades_keep_their_colour_on_a_ground_the_terminal_owns() {
        let terminal = Theme::built_in("terminal").unwrap();
        assert_eq!(terminal.background, Color::Reset);
        // A dim toward a ground that is not a colour would snap to the
        // ground and paint black on the pixel raster: the colour stands.
        assert_eq!(
            dim(terminal.event_inactive, &terminal),
            terminal.event_inactive
        );
        assert_eq!(
            wash(Color::Reset, Color::Rgb(255, 0, 0), 0.5),
            Color::Rgb(255, 0, 0)
        );
        assert_eq!(
            wash(Color::Reset, Color::Rgb(255, 0, 0), 0.25),
            Color::Rgb(255, 0, 0)
        );
        // A ground that is a colour still takes its share, in the same
        // order. The quarter is asymmetric on purpose: a swapped call
        // lands on 191, not 64.
        assert_eq!(
            wash(Color::Rgb(0, 0, 0), Color::Rgb(255, 0, 0), 0.25),
            Color::Rgb(64, 0, 0)
        );
    }

    /// The spectrogram paints its quiet level, so its lowest colour must
    /// be one the terminal can show. A reset ground would paint black or
    /// the default foreground, and the picture would open on a slab.
    #[test]
    fn a_spectrum_ramp_on_a_terminal_ground_starts_visible() {
        let theme = Theme::built_in("terminal").expect("terminal ships");
        let ramp = SpectrogramRamp::new(&theme, theme.background, theme.accent);
        for step in 0..=100 {
            let level = step as f32 / 100.0;
            let colour = ramp.at(level);
            assert_ne!(colour, Color::Reset, "level {level} has no colour");
            assert_ne!(colour, Color::Black, "level {level} paints black");
        }
    }

    /// A silent note is the case the reports showed as black bars. The
    /// whole painter path runs under the terminal theme: no cell that
    /// carries a glyph may come back `Reset`, which the pixel raster
    /// paints as plain black, and the bars wear the palette's own slots.
    #[test]
    fn a_roll_under_the_terminal_theme_paints_no_black_bars() {
        let state = playing(
            "$: s(\"bd sd\")._pianoroll()",
            vec![event(1, "0/1", "1/2", 10.0), event(2, "1/2", "1/1", 10.5)],
            0.75,
        );
        let theme = Theme::built_in("terminal").expect("terminal ships");
        let parsed = VisualOptions::parse("");
        let area = Rect::new(0, 0, 40, 6);
        let mut buffer = Buffer::empty(area);
        render(
            VisualRequest {
                kind: "pianoroll",
                slot: None,
                options: &parsed,
                state: &state,
                theme: &theme,
                background: theme.background,
                inline: true,
            },
            area,
            &mut buffer,
        );
        let painted = buffer
            .content
            .iter()
            .filter(|cell| cell.symbol() != " ")
            .collect::<Vec<_>>();
        assert!(!painted.is_empty(), "the roll drew nothing");
        for cell in painted {
            assert_ne!(
                cell.fg,
                Color::Reset,
                "a painted cell fell back to the terminal's ground"
            );
            // Silent notes wear slot 8, sounding ones slot 14, and the
            // playhead crossing a bar recolours that cell with slot 3.
            assert!(
                matches!(
                    cell.fg,
                    Color::Indexed(3) | Color::Indexed(8) | Color::Indexed(14)
                ),
                "a bar wore {} instead of the palette's slots",
                cell.fg
            );
        }
    }

    #[test]
    fn spectrum_colors_follow_frequency_and_theme() {
        let _gate = cells_tier();
        let mut indexed = theme();
        indexed.syntax.punctuation = Color::Indexed(5);
        indexed.accent = Color::Indexed(6);
        indexed.syntax.string = Color::Indexed(2);
        indexed.syntax.number = Color::Indexed(3);
        indexed.meter.peak = Color::Indexed(1);
        for theme in [
            theme(),
            Theme::built_in("rustel-light").unwrap(),
            Theme::built_in("mono").unwrap(),
            indexed,
        ] {
            for level in [-8.0, -32.0] {
                let state = analyser_state(&[[level; UI_SPECTRUM_BINS]]);
                let options = VisualOptions::parse("");
                let request = VisualRequest {
                    kind: "spectrum",
                    slot: None,
                    options: &options,
                    state: &state,
                    theme: &theme,
                    background: theme.background,
                    inline: true,
                };
                let area = Rect::new(0, 0, 9, 4);
                let mut buffer = Buffer::empty(area);
                render(request, area, &mut buffer);
                for (x, expected) in [
                    (0, theme.syntax.punctuation),
                    (1, mix(theme.syntax.punctuation, theme.accent, 0.5)),
                    (2, theme.accent),
                    (4, theme.syntax.string),
                    (6, theme.syntax.number),
                    (8, theme.meter.peak),
                ] {
                    for y in [2, 3] {
                        let cell = &buffer[(x, y)];
                        assert_eq!(cell.symbol(), "⣿");
                        assert_eq!(cell.fg, expected, "{} at {x},{y}", theme.name);
                    }
                }
            }
        }
    }

    #[test]
    fn spectrum_explicit_colors_override_the_theme_gradient() {
        let mut state = analyser_state(&[[-8.0; UI_SPECTRUM_BINS]]);
        let area = Rect::new(0, 0, 9, 4);
        for color in ["#123456", "#55d6e8"] {
            let picture = draw("spectrum", &format!("{{ color: '{color}' }}"), &state, area);
            for x in 0..area.width {
                assert_eq!(picture[(x, 3)].fg, parse_color(color).unwrap());
            }
        }
        let mut red = event(1, "0/1", "1/1", 10.0);
        red.color = Some("red".into());
        state.events.push_back(red);
        let picture = draw("spectrum", "{ color: '#123456' }", &state, area);
        for x in 0..area.width {
            assert_eq!(picture[(x, 3)].fg, parse_color("red").unwrap());
        }
    }

    #[test]
    fn spectrum_bars_move_in_quarter_cell_steps() {
        use crate::graphics::Tier;
        for tier in [Tier::Cells, Tier::Fine] {
            at_tier(tier, || {
                let mut bits = 0u8;
                for (level, row) in [(-60.0, 3), (-40.0, 2), (-20.0, 1), (0.0, 0)] {
                    bits |= BRAILLE_BITS[0][row] | BRAILLE_BITS[1][row];
                    let state = analyser_state(&[[level; UI_SPECTRUM_BINS]]);
                    let picture = render_now("spectrum", "", &state, Rect::new(0, 0, 1, 1));
                    let expected = char::from_u32(0x2800 + u32::from(bits)).unwrap();
                    assert_eq!(picture.cell((0, 0)).unwrap().symbol(), expected.to_string());
                }
            });
        }
    }

    #[test]
    fn spectrum_resolves_narrow_peaks_and_includes_the_last_bin() {
        for bin in [420, UI_SPECTRUM_BINS - 1] {
            let mut spectrum = [-120.0; UI_SPECTRUM_BINS];
            spectrum[bin] = 0.0;
            let state = analyser_state(&[spectrum]);
            let picture = draw("spectrum", "", &state, Rect::new(0, 0, 200, 4));
            let lit = (0..200)
                .filter(|x| column_height(&picture, *x) > 0)
                .collect::<Vec<_>>();
            assert_eq!(
                lit.len(),
                1,
                "a narrow peak occupies one display column: {lit:?}"
            );
            if bin == UI_SPECTRUM_BINS - 1 {
                assert_eq!(lit, vec![199]);
                assert_eq!(
                    state.spectrogram(None).unwrap().back().unwrap()[SPECTROGRAM_BANDS - 1],
                    0.0
                );
            }
        }
    }

    #[test]
    fn the_spectrum_bars_decay_and_hold_their_peaks() {
        let loud = [-3.0; UI_SPECTRUM_BINS];
        let quiet = [-120.0; UI_SPECTRUM_BINS];
        let area = Rect::new(0, 0, 40, 8);
        let up = draw("spectrum", "", &analyser_state(&[loud]), area);
        let state = analyser_state(&[loud, quiet, quiet, quiet]);
        let falling = draw("spectrum", "", &state, area);
        let dots = |buffer: &Buffer| {
            (0..7)
                .flat_map(|y| {
                    let symbol = buffer
                        .cell((0, y))
                        .unwrap()
                        .symbol()
                        .chars()
                        .next()
                        .unwrap();
                    let bits = u32::from(symbol).saturating_sub(0x2800) as u8;
                    BRAILLE_BITS[0].map(|mask| bits & mask != 0)
                })
                .collect::<Vec<_>>()
        };
        let full = dots(&up);
        let later = dots(&falling);
        assert_eq!(full.iter().filter(|dot| **dot).count(), 27);
        assert_eq!(later.iter().filter(|dot| **dot).count(), 26);
        assert!(later[1], "the peak stays at its original height");
        assert!(!later[2], "the bar falls below the peak");
        assert!(later[3..].iter().all(|dot| *dot));
        let bands = state.analyser(None).unwrap();
        assert_eq!(bands.levels[0], -9.0);
        assert_eq!(bands.peaks[0], -3.0);

        let mut frames = vec![loud];
        frames.extend([quiet; 16]);
        let released = analyser_state(&frames);
        assert_eq!(released.analyser(None).unwrap().peaks[0], -4.0);
    }

    /// With a row to spare the analyser writes its frequency marks.
    #[test]
    fn the_spectrum_marks_its_frequencies_when_it_has_the_room() {
        let state = analyser_state(&[[-3.0; UI_SPECTRUM_BINS]]);
        let tall = rows(&draw("spectrum", "", &state, Rect::new(0, 0, 60, 6)));
        assert!(tall.contains("1k") && tall.contains("10k"), "{tall}");
        let short = rows(&draw("spectrum", "", &state, Rect::new(0, 0, 60, 3)));
        assert!(
            !short.contains("1k"),
            "no room for marks in three rows:\n{short}"
        );
    }

    #[test]
    fn the_spectrum_honours_its_decibel_window() {
        let mut state = playing("$: s(\"bd\")._spectrum()", Vec::new(), 0.0);
        let metadata = UiAudioMetadata {
            sequence: 1,
            generation: 1,
            device_time: 0.5,
            stream_id: 1,
            epoch: 1,
            end_frame: 24_000,
            sample_rate: 48_000,
        };
        assert!(state.install_audio(
            metadata,
            UiAudioAnalysisSet {
                master: UiAudioAnalysisFrame {
                    scope: [0.0; UI_SCOPE_SAMPLES],
                    spectrum: [-40.0; UI_SPECTRUM_BINS],
                },
                visuals: Vec::new(),
                sides: Vec::new(),
            },
        ));
        let area = Rect::new(0, 0, 20, 4);
        let filled = |options: &str| {
            draw("spectrum", options, &state, area)
                .content
                .iter()
                .filter(|cell| cell.symbol() != " ")
                .count()
        };
        // The analyser's bars, not the spectrogram: a window whose floor sits
        // above -40 dB draws nothing, one that puts -40 dB near its top
        // lights every band.
        assert_eq!(
            filled("{ min: -30, max: 0 }"),
            0,
            "a level under the floor was drawn"
        );
        assert!(
            filled("{ min: -50, max: -38 }") > filled("{ min: -30, max: 0 }"),
            "the decibel window was ignored"
        );

        // The spectrogram reads the same window by its own route, and
        // nothing else proves it does. A floor above the signal leaves the
        // picture at the ground; a window that puts -40 dB near its top
        // lights it.
        let ground = theme().background;
        let energised = |colour: Color| colour != ground && colour != Color::Reset;
        let scrolled = |options: &str| {
            draw("spectrum", options, &state, area)
                .content
                .iter()
                .filter(|cell| energised(cell.fg) || energised(cell.bg))
                .count()
        };
        assert_eq!(
            scrolled("{ scroll: 1, min: -30, max: 0 }"),
            0,
            "a level under the floor lit the spectrogram"
        );
        assert!(
            scrolled("{ scroll: 1, min: -50, max: -38 }") > 0,
            "the spectrogram ignored its decibel window"
        );
    }

    #[test]
    fn the_spiral_and_pitch_wheel_draw_without_panicking_at_any_size() {
        let mut events = vec![event(1, "0/1", "1/2", 10.0)];
        events[0].frequency_hz = Some(440.0);
        let state = playing("$: note(\"a4\")._spiral()", events, 2.5);
        for size in [(1, 1), (4, 2), (20, 10), (60, 24)] {
            let area = Rect::new(0, 0, size.0, size.1);
            for kind in ["spiral", "pitchwheel"] {
                let _ = draw(kind, "", &state, area);
            }
        }
        let text = symbols(&draw("spiral", "", &state, Rect::new(0, 0, 60, 14)));
        assert!(
            is_braille_line(&text),
            "the spiral draws in Braille at Cells: {text:?}"
        );
        assert!(
            text.contains("▸ A4"),
            "the sounding note is named beside the spiral: {text:?}"
        );
    }

    /// Whether a rendering contains Braille dots and no block glyphs.
    fn is_braille_line(text: &str) -> bool {
        text.chars()
            .any(|glyph| ('\u{2800}'..='\u{28ff}').contains(&glyph))
            && !text.chars().any(|glyph| matches!(glyph, '█' | '▀' | '▄'))
    }

    #[test]
    fn a_scope_trace_is_a_thin_line_not_a_filled_pane() {
        let mut scope = [0.0f32; UI_SCOPE_SAMPLES];
        for (index, sample) in scope.iter_mut().enumerate() {
            *sample = (std::f32::consts::TAU * 2.0 * index as f32 / UI_SCOPE_SAMPLES as f32).sin();
        }
        let mut state = playing("$: s(\"bd\")._scope()", Vec::new(), 0.0);
        audio(&mut state, scope);
        let area = Rect::new(0, 0, 40, 6);
        let buffer = draw("scope", "", &state, area);
        // Two full cycles across the pane: most cells stay empty, so the
        // trace reads as a line rather than a wall of glyphs.
        let painted = buffer
            .content
            .iter()
            .filter(|cell| cell.symbol() != " ")
            .count();
        assert!(
            painted < (area.width * area.height / 2) as usize,
            "{painted} of {} cells were painted",
            area.width * area.height
        );
        assert!(painted > 30, "the trace is missing ({painted} cells)");
    }

    #[test]
    fn the_pitch_wheel_has_no_outer_circle_and_names_its_notes_on_the_left() {
        let mut events = vec![event(1, "0/1", "1/1", 10.0)];
        events[0].frequency_hz = Some(440.0);
        events[0].duration_seconds = 4.0;
        let state = playing("$: note(\"a4\")._pitchwheel()", events, 0.0);
        let area = Rect::new(0, 0, 60, 10);
        let buffer = draw("pitchwheel", "", &state, area);
        let text = symbols(&buffer);
        assert!(text.contains("▸ A4"), "{text:?}");
        // The wheel is at the left: nothing but the legend is drawn past
        // the square it occupies, and the ring is dots rather than a circle.
        let wheel_cells = (0..area.height)
            .flat_map(|y| (0..5).map(move |x| (x, y)))
            .filter(|&(x, y)| buffer.cell((x, y)).is_some_and(|cell| cell.symbol() != " "))
            .count();
        let far_right_cells = (0..area.height)
            .flat_map(|y| (40..area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| buffer.cell((x, y)).is_some_and(|cell| cell.symbol() != " "))
            .count();
        assert!(wheel_cells > 0, "the wheel is not at the left: {text:?}");
        assert_eq!(far_right_cells, 0, "something is drawn far right: {text:?}");
        let braille = text
            .chars()
            .filter(|glyph| ('\u{2800}'..='\u{28ff}').contains(glyph))
            .count();
        assert!(
            braille < 40,
            "a full ring would need many more cells: {braille}"
        );
    }

    #[test]
    fn a_pitch_wheel_lights_the_class_of_a_sounding_note() {
        let mut events = vec![event(1, "0/1", "1/1", 10.0)];
        events[0].frequency_hz = Some(440.0);
        events[0].duration_seconds = 4.0;
        let state = playing("$: note(\"a4\")._pitchwheel()", events, 0.0);
        let buffer = draw("pitchwheel", "", &state, Rect::new(0, 0, 24, 12));
        let theme = theme();
        let lit = buffer
            .content
            .iter()
            .filter(|cell| cell.fg != theme.grid && cell.fg != theme.rule && cell.symbol() != " ")
            .count();
        assert!(lit > 0, "an active note should light a spoke");
    }

    #[test]
    fn an_unsupported_kind_explains_itself() {
        let state = playing("$: s(\"bd\")._pianoroll()", Vec::new(), 0.0);
        let text = symbols(&draw("markcss", "", &state, Rect::new(0, 0, 40, 2)));
        assert!(
            text.contains("no terminal renderer for _markcss()"),
            "{text:?}"
        );
    }

    #[test]
    fn note_names_and_pitch_classes_round_trip() {
        assert_eq!(note_name(69.0), "A4");
        assert_eq!(note_name(60.0), "C4");
        assert_eq!(note_to_midi("C"), Some(0.0));
        assert_eq!(note_to_midi("f#"), Some(6.0));
        assert_eq!(note_to_midi("Bb"), Some(10.0));
        assert_eq!(note_to_midi("H"), None);
    }

    #[test]
    fn fractions_keep_exact_protocol_strings_until_rendering() {
        assert_eq!(parse_fraction("3/2"), Some(1.5));
        assert_eq!(parse_fraction("1/0"), None);
        assert_eq!(parse_fraction("wat"), None);
    }

    #[test]
    fn active_marks_use_the_theme_colour_when_a_pattern_names_none() {
        let mut events = vec![event(1, "0/1", "1/1", 10.0)];
        events[0].duration_seconds = 4.0;
        let state = playing("$: s(\"bd\")._pianoroll()", events, 0.0);
        let marks = state.active_marks(Color::Rgb(1, 2, 3));
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].color, Color::Rgb(1, 2, 3));
        assert_eq!((marks[0].from, marks[0].to), (0, 4));
    }

    #[test]
    fn stopped_visual_state_rejects_queued_audio_from_the_old_stream() {
        let mut visual = VisualState::default();
        visual.install_layout(
            rustel_runtime::ui_events::visual_layout("note(60)._scope()", 3).unwrap(),
        );
        let metadata = UiAudioMetadata {
            sequence: 1,
            generation: 3,
            device_time: 0.5,
            stream_id: 8,
            epoch: 1,
            end_frame: 24_000,
            sample_rate: 48_000,
        };
        let analysis = UiAudioAnalysisSet {
            master: UiAudioAnalysisFrame {
                scope: [0.0; UI_SCOPE_SAMPLES],
                spectrum: [-120.0; UI_SPECTRUM_BINS],
            },
            visuals: Vec::new(),
            sides: Vec::new(),
        };

        assert!(!visual.install_audio(metadata, analysis.clone()));
        visual.start();
        assert!(visual.install_audio(metadata, analysis.clone()));
        visual.stop();
        assert!(!visual.install_audio(
            UiAudioMetadata {
                sequence: 2,
                ..metadata
            },
            analysis
        ));
        assert!(
            visual.audio().is_some(),
            "the last frame stays on screen after a stop"
        );
    }

    /// A redraw after an update reads the evaluated score behind the
    /// playhead as well as ahead, and what it drew there stands under the
    /// look-aheads that follow: each of those replaces the previews from
    /// its own cycle on, not the past.
    #[test]
    fn a_redraw_behind_the_playhead_stands_under_later_look_aheads() {
        let mut state = playing("$: s(\"bd\")._pianoroll()", Vec::new(), 4.0);
        let coming = |begin: &str, end: &str, at: f64| {
            let mut event = event(0, begin, end, at);
            event.ui_visuals = 1;
            event
        };
        assert!(state.install_preview(
            preview_batch(
                &state,
                vec![
                    coming("2/1", "5/2", 8.0),
                    coming("3/1", "7/2", 9.0),
                    coming("5/1", "11/2", 11.0),
                ]
            ),
            1.5,
        ));
        assert_eq!(state.events().count(), 3, "the redraw covers the window");

        // The next look-ahead starts where the audio's schedule ends.
        assert!(state.install_preview(
            preview_batch(&state, vec![coming("6/1", "13/2", 12.0)]),
            4.5,
        ));
        let onsets = state
            .events()
            .map(|event| event.whole_begin.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            onsets,
            ["2/1", "3/1", "6/1"],
            "behind the playhead stays; from the look-ahead's start on is replaced"
        );
    }

    /// Stopping holds the picture where it was: the clock stops rather than
    /// resets, the frames stay, and only the marks in the code go out.
    #[test]
    fn stopping_freezes_the_picture_where_it_was() {
        let mut visual = VisualState::default();
        visual.install_layout(
            rustel_runtime::ui_events::visual_layout("note(60)._scope()", 3).unwrap(),
        );
        visual.start();
        let wall = Instant::now();
        visual.clock = Some(ClockAnchor {
            generation: 3,
            wall,
            device_time: 0.0,
            cycle: 0.0,
            cps: 1.0,
        });
        let (_, before, _) = visual
            .clock_at(wall + std::time::Duration::from_secs(1))
            .expect("a clock");
        assert!((before - 1.0).abs() < 1e-6, "{before}");

        visual.previews.push(event(7, "1/1", "2/1", 11.0));
        visual.stop();
        assert_eq!(
            visual.previews.len(),
            1,
            "what was coming stays on the frozen picture"
        );
        let frozen = visual.frozen_at.expect("frozen at the stop");
        let (_, at_stop, _) = visual.clock_at(frozen).expect("still a clock");
        let (_, later, _) = visual
            .clock_at(frozen + std::time::Duration::from_secs(5))
            .expect("still a clock");
        assert!(
            (later - at_stop).abs() < 1e-6,
            "the roll does not scroll on after a stop: {at_stop} then {later}"
        );
        assert!(
            visual.active_marks(Color::Reset).is_empty(),
            "nothing is marked as sounding"
        );
        visual.start();
        assert!(
            visual.frozen_at.is_none(),
            "playing again lets the clock run"
        );
        assert!(
            visual.previews.is_empty(),
            "and starts with its own look-ahead"
        );
    }

    #[test]
    fn repeated_stops_never_advance_the_frozen_cycle() {
        let mut visual = VisualState::default();
        visual.install_layout(
            rustel_runtime::ui_events::visual_layout("s('sine')._pianoroll()", 3).unwrap(),
        );
        visual.start();
        visual.clock = Some(ClockAnchor {
            generation: 3,
            wall: Instant::now(),
            device_time: 3.84,
            cycle: 1.92,
            cps: 0.5,
        });
        visual.stop();
        let frozen = visual.frozen_at;
        let held = visual.current_clock().unwrap();
        let (_, draining_later, _) = visual
            .transport_clock_at(frozen.unwrap() + std::time::Duration::from_secs(2))
            .unwrap();
        assert!(
            draining_later > held.1,
            "the header clock keeps moving while the visual canvas stays held"
        );
        for _ in 0..32 {
            visual.stop();
            assert_eq!(visual.frozen_at, frozen);
            assert_eq!(visual.current_clock(), Some(held));
        }
        visual.start();
        assert!(visual.frozen_at.is_none());
        visual.stop();
        assert!(visual.frozen_at.unwrap() >= frozen.unwrap());
    }

    #[test]
    fn synth_pianoroll_previews_and_audio_onsets_share_one_lane() {
        use rustel_runtime::ui_events::{UiAcceptedOnset, correlate_submitted_traces};
        for (source, accepted_frequency, pitched) in [
            ("$: s(\"sine*16\")._pianoroll()", 440.0, false),
            ("$: s(\"piano*16\")._pianoroll()", 0.0, false),
            ("$: s(\"sine*16\").freq(220)._pianoroll()", 220.0, true),
            ("$: note(\"a3*16\").s(\"sine\")._pianoroll()", 220.0, true),
        ] {
            let mut session = rustel_runtime::Session::new().unwrap();
            session.evaluate(source).unwrap();
            let traces = session
                .preview_traces(0.0, 1.0, session.generation())
                .unwrap();
            assert_eq!(traces.len(), 16, "{source}");
            let mut lanes = Vec::new();
            for trace in traces {
                let preview = UiScheduledEvent::from_trace(&trace, session.cps()).unwrap();
                let accepted = UiAcceptedOnset {
                    generation: trace.generation,
                    onset_id: trace.onset_id,
                    frequency_hz: Some(accepted_frequency),
                    gain: Some(1.0),
                };
                let mut pending = std::collections::HashMap::from([(trace.onset_id, trace)]);
                let real = correlate_submitted_traces(&mut pending, [accepted])
                    .ready
                    .pop()
                    .unwrap();
                let real = UiScheduledEvent::from_owned_trace(real, session.cps()).unwrap();
                lanes.extend([Lane::of(&preview), Lane::of(&real)]);
            }
            lanes.sort_by(Lane::compare);
            lanes.dedup();
            assert_eq!(lanes.len(), 1, "{source}: {lanes:?}");
            assert_eq!(matches!(lanes[0], Lane::Pitch(_)), pitched, "{source}");
        }
    }

    #[test]
    fn a_missing_slot_update_keeps_its_last_complete_visual_frame() {
        let mut visual = VisualState::default();
        visual.install_layout(
            rustel_runtime::ui_events::visual_layout("note(60)._scope()", 3).unwrap(),
        );
        visual.start();
        let metadata = UiAudioMetadata {
            sequence: 1,
            generation: 3,
            device_time: 0.5,
            stream_id: 8,
            epoch: 1,
            end_frame: 24_000,
            sample_rate: 48_000,
        };
        let first_visual = UiAudioAnalysisFrame {
            scope: [0.25; UI_SCOPE_SAMPLES],
            spectrum: [-30.0; UI_SPECTRUM_BINS],
        };
        assert!(visual.install_audio(
            metadata,
            UiAudioAnalysisSet {
                master: UiAudioAnalysisFrame {
                    scope: [0.5; UI_SCOPE_SAMPLES],
                    spectrum: [-20.0; UI_SPECTRUM_BINS],
                },
                visuals: vec![(0, first_visual.clone())],
                sides: Vec::new(),
            },
        ));

        assert!(visual.install_audio(
            UiAudioMetadata {
                sequence: 2,
                end_frame: 24_128,
                ..metadata
            },
            UiAudioAnalysisSet {
                master: UiAudioAnalysisFrame {
                    scope: [0.75; UI_SCOPE_SAMPLES],
                    spectrum: [-10.0; UI_SPECTRUM_BINS],
                },
                visuals: Vec::new(),
                sides: Vec::new(),
            },
        ));
        assert_eq!(visual.audio_for(Some(0)), Some(&first_visual));
        assert_eq!(
            visual.audio_for(None).expect("master").scope[0],
            0.75,
            "the footer master should still advance"
        );
    }

    /// A slider re-query installs the same layout under a new generation.
    /// The analysers and their history must survive it.
    #[test]
    fn a_new_generation_of_the_same_score_keeps_the_analysers() {
        let source = "note(60)._scope()._spectrum()";
        let mut visual = VisualState::default();
        visual.install_layout(rustel_runtime::ui_events::visual_layout(source, 7).unwrap());
        visual.start();
        visual.install_audio(
            UiAudioMetadata {
                sequence: 1,
                generation: 7,
                device_time: 0.5,
                stream_id: 1,
                epoch: 1,
                end_frame: 24_000,
                sample_rate: 48_000,
            },
            UiAudioAnalysisSet {
                master: UiAudioAnalysisFrame {
                    scope: [0.5; UI_SCOPE_SAMPLES],
                    spectrum: [-40.0; UI_SPECTRUM_BINS],
                },
                visuals: Vec::new(),
                sides: Vec::new(),
            },
        );
        assert!(
            visual.analysers_are_kept_across_a_generation(),
            "there is something to keep"
        );

        // The same score, a new generation: a slider moved.
        visual.install_layout(rustel_runtime::ui_events::visual_layout(source, 8).unwrap());
        assert!(
            visual.analysers_are_kept_across_a_generation(),
            "the picture is the same picture"
        );

        // A different score is a different picture, and starts again.
        visual.install_layout(
            rustel_runtime::ui_events::visual_layout("note(62)._scope()._spectrum()", 9).unwrap(),
        );
        assert!(
            !visual.analysers_are_kept_across_a_generation(),
            "a score that changed starts its analysers over"
        );
    }

    #[test]
    fn silent_generation_cutover_replaces_the_old_visual_clock() {
        let first_layout =
            rustel_runtime::ui_events::visual_layout("note(60)._pianoroll()", 7).unwrap();
        let first_revision = first_layout.ui_layout.source_revision.clone();
        let mut visual = VisualState::default();
        visual.install_layout(first_layout);
        visual.start();

        let first_snapshot = StudioSnapshot {
            playing: true,
            stopping: false,
            session_generation: 7,
            audible_generation: Some(7),
            confirmed_audio_generation: Some(7),
            source_revision: Some(first_revision),
            csound: Default::default(),
            cps: 0.5,
            device_time: 10.0,
            cycle: 4.0,
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
                link: None,
            },
            pressure: None,
            audition_loading: None,
            loading: None,
            sample_memory: Default::default(),
            script_heap_bytes: 0,
            audio_memory: None,
        };
        let first_wall = Instant::now();
        assert!(visual.install_snapshot_clock_at(&first_snapshot, first_wall));
        assert_eq!(
            visual.clock_at(first_wall + std::time::Duration::from_secs(2)),
            Some((12.0, 5.0, 0.5))
        );

        // No trace batch follows this silent layout. Installing it must stop
        // the preceding generation's 0.5-CPS anchor immediately.
        let silent_layout =
            rustel_runtime::ui_events::visual_layout("silence._pianoroll()", 8).unwrap();
        let silent_revision = silent_layout.ui_layout.source_revision.clone();
        visual.install_layout(silent_layout);
        assert_eq!(visual.clock_at(first_wall), None);
        assert!(!visual.install_snapshot_clock_at(&first_snapshot, first_wall));

        let silent_snapshot = StudioSnapshot {
            playing: true,
            stopping: false,
            session_generation: 8,
            audible_generation: Some(8),
            confirmed_audio_generation: Some(7),
            source_revision: Some(silent_revision),
            csound: Default::default(),
            cps: 2.0,
            device_time: 20.0,
            cycle: 40.0,
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
                link: None,
            },
            pressure: None,
            audition_loading: None,
            loading: None,
            sample_memory: Default::default(),
            script_heap_bytes: 0,
            audio_memory: None,
        };
        let cutover_wall = first_wall + std::time::Duration::from_secs(10);
        assert!(visual.install_snapshot_clock_at(&silent_snapshot, cutover_wall));
        assert_eq!(
            visual.clock_at(cutover_wall + std::time::Duration::from_secs(3)),
            Some((23.0, 46.0, 2.0))
        );
    }

    /// The terminal's own defaults are the values its painters take: an
    /// option written at its terminal default draws what no option draws,
    /// so a knob that comes on at it changes nothing on the screen. A scope
    /// that names its scale is no longer fitted to its pane, which is the
    /// one difference the picture shows.
    #[test]
    fn a_terminal_default_draws_what_no_option_draws() {
        let area = Rect::new(0, 0, 32, 10);
        let notes = || {
            vec![
                pitched(1, "0/1", "1/2", 261.6, 10.0),
                pitched(2, "1/2", "1/1", 523.2, 20.0),
            ]
        };
        let mut checked = 0;
        for painter in PAINTER_TERMINAL {
            for number in painter.numbers {
                if (painter.painter, number.option) == ("scope", "scale") {
                    continue;
                }
                let source = format!("$: note(\"c4 c5\")._{}()", painter.painter);
                let mut state = playing(&source, notes(), 0.2);
                let mut scope = [0.0f32; UI_SCOPE_SAMPLES];
                for (index, sample) in scope.iter_mut().enumerate() {
                    *sample = (std::f32::consts::TAU * 3.0 * index as f32
                        / UI_SCOPE_SAMPLES as f32)
                        .sin();
                }
                let mut spectrum = [0.0f32; UI_SPECTRUM_BINS];
                for (index, bin) in spectrum.iter_mut().enumerate() {
                    *bin = -110.0 + 100.0 * index as f32 / UI_SPECTRUM_BINS as f32;
                }
                assert!(state.install_audio(
                    UiAudioMetadata {
                        sequence: 1,
                        generation: 1,
                        device_time: 0.5,
                        stream_id: 1,
                        epoch: 1,
                        end_frame: 24_000,
                        sample_rate: 48_000,
                    },
                    UiAudioAnalysisSet {
                        master: UiAudioAnalysisFrame { scope, spectrum },
                        visuals: Vec::new(),
                        sides: Vec::new(),
                    },
                ));
                // A stopped clock holds the picture still between the
                // two draws.
                state.stop();
                let written = format!("{{{}: {}}}", number.option, number.default);
                let plain = draw(painter.painter, "", &state, area);
                assert!(painted(&plain) > 0, "{} draws", painter.painter);
                assert_eq!(
                    rows(&plain),
                    rows(&draw(painter.painter, &written, &state, area)),
                    "{} {written}",
                    painter.painter
                );
                assert!(
                    (number.min..=number.max).contains(&number.default),
                    "{} {written} is inside its bounds",
                    painter.painter
                );
                checked += 1;
            }
        }
        assert!(checked >= 8, "every terminal number is drawn");
    }

    #[cfg(feature = "video")]
    mod video {
        //! The video painter: the clip a lane's sound names, its frame
        //! following the clock, and the poll that asks for each strip once.

        use super::super::*;
        use super::{draw, event, playing, rows};

        /// A two-frame strip: all red, then all blue.
        fn strip() -> rustel_runtime::video::VideoThumbnails {
            let red = [255, 0, 0, 255].repeat(4);
            let blue = [0, 0, 255, 255].repeat(4);
            rustel_runtime::video::VideoThumbnails::from_frames(2, 2, vec![(0.0, red), (0.5, blue)])
                .expect("strip")
        }

        fn clip_event(target_time: f64) -> UiScheduledEvent {
            let mut event = event(1, "0/1", "1/2", target_time);
            event.value = Some("s:clip.webm note:60".to_owned());
            event.label = None;
            // The lane's one visual is slot 0, the transpiler's first.
            event.ui_visuals = 1;
            event
        }

        #[test]
        fn an_event_names_the_sound_its_chain_carries() {
            let mut event = clip_event(10.0);
            assert_eq!(event_clip_name(&event), Some("clip.webm".to_owned()));
            event.value = Some("note:60 gain:1".to_owned());
            assert_eq!(event_clip_name(&event), None, "no sound, no clip");
            event.value = Some("speed:2 s:loop.mkv s:again.webm".to_owned());
            assert_eq!(
                event_clip_name(&event),
                Some("loop.mkv".to_owned()),
                "the first sound"
            );
            // The pick the voice plays names the file the picture shows.
            event.value = Some("s:clips n:2 note:60".to_owned());
            assert_eq!(
                event_clip_name(&event),
                Some("clips:2".to_owned()),
                "n(2) is the third file, the name:2 spelling"
            );
            event.value = Some("s:clips n:2.5".to_owned());
            assert_eq!(
                event_clip_name(&event),
                Some("clips".to_owned()),
                "a fractional pick is no file index"
            );
        }

        #[test]
        fn a_picture_larger_than_its_rows_still_fills_them() {
            // A real clip keeps up to 160 px on its long edge, while the
            // band's rows hold a fraction of that in points. The frame
            // must shrink into them, not collapse to a single cell.
            let frame = [80, 40, 160, 255].repeat(160 * 90);
            let wide =
                rustel_runtime::video::VideoThumbnails::from_frames(160, 90, vec![(0.0, frame)])
                    .expect("strip");
            let mut state = playing("$: s(\"clip.webm\")._video()", vec![clip_event(10.0)], 0.0);
            assert!(state.install_video("clip.webm", VideoEntry::Ready(std::sync::Arc::new(wide))));
            let picture = draw("video", "", &state, Rect::new(0, 0, 80, 8));
            let painted = picture
                .content
                .iter()
                .filter(|cell| cell.fg == Color::Rgb(80, 40, 160))
                .count();
            assert!(
                painted > 100,
                "the shrunken frame filled the band: {painted} cells"
            );
        }

        #[test]
        fn the_picture_starts_where_the_sound_does_and_walks_its_speed() {
            // `begin` skips into the clip the ratio names: the second half
            // of a half-second clip is the blue frame, at once.
            let mut begun = playing("$: s(\"clip.webm\")._video()", vec![clip_event(10.0)], 0.0);
            let mut chain = clip_event(10.0);
            chain.value = Some("s:clip.webm begin:1".to_owned());
            begun.events.pop_front();
            begun.events.push_back(chain);
            assert!(
                begun.install_video("clip.webm", VideoEntry::Ready(std::sync::Arc::new(strip())))
            );
            let picture = draw("video", "", &begun, Rect::new(0, 0, 20, 8));
            assert!(
                picture
                    .content
                    .iter()
                    .any(|cell| cell.fg == Color::Rgb(0, 0, 255)),
                "the clip starts at its second half"
            );

            // Twice the speed reaches the same place in half the time:
            // 0.25 s in at 2x is the blue frame.
            let mut sped = playing("$: s(\"clip.webm\")._video()", vec![clip_event(9.75)], 0.0);
            let mut chain = clip_event(9.75);
            chain.value = Some("s:clip.webm speed:2".to_owned());
            sped.events.pop_front();
            sped.events.push_back(chain);
            assert!(
                sped.install_video("clip.webm", VideoEntry::Ready(std::sync::Arc::new(strip())))
            );
            let picture = draw("video", "", &sped, Rect::new(0, 0, 20, 8));
            assert!(
                picture
                    .content
                    .iter()
                    .any(|cell| cell.fg == Color::Rgb(0, 0, 255)),
                "0.2 s at twice the speed is the later frame"
            );
        }

        #[test]
        fn the_painter_draws_the_frame_the_clock_is_showing() {
            let mut state = playing("$: s(\"clip.webm\")._video()", vec![clip_event(10.0)], 0.0);
            assert!(
                state.install_video("clip.webm", VideoEntry::Ready(std::sync::Arc::new(strip()))),
                "the strip lands"
            );
            // The event sounds now, so the first frame shows: red.
            let picture = draw("video", "", &state, Rect::new(0, 0, 20, 8));
            let red = picture
                .content
                .iter()
                .filter(|cell| cell.fg == Color::Rgb(255, 0, 0))
                .count();
            assert!(red > 100, "the first frame filled the pane: {red} cells");
            assert!(
                picture
                    .content
                    .iter()
                    .all(|cell| cell.fg != Color::Rgb(0, 0, 255)),
                "the second frame is not showing yet"
            );
        }

        #[test]
        fn the_frame_follows_the_position_into_the_clip() {
            // The event began 0.6 s ago: past the second frame's time, the
            // picture holds the blue one.
            let mut state = playing("$: s(\"clip.webm\")._video()", vec![clip_event(9.4)], 0.0);
            assert!(
                state.install_video("clip.webm", VideoEntry::Ready(std::sync::Arc::new(strip())))
            );
            let picture = draw("video", "", &state, Rect::new(0, 0, 20, 8));
            let blue = picture
                .content
                .iter()
                .filter(|cell| cell.fg == Color::Rgb(0, 0, 255))
                .count();
            assert!(blue > 100, "the later frame filled the pane: {blue} cells");
        }

        #[test]
        fn a_clip_that_has_not_decoded_says_so_and_a_refusal_names_itself() {
            let state = playing("$: s(\"clip.webm\")._video()", vec![clip_event(10.0)], 0.0);
            let waiting = draw("video", "", &state, Rect::new(0, 0, 40, 8));
            assert!(
                rows(&waiting).contains("waiting for clip.webm"),
                "says the clip is on its way"
            );
            let mut refused = playing("$: s(\"clip.webm\")._video()", vec![clip_event(10.0)], 0.0);
            assert!(refused.install_video(
                "clip.webm",
                VideoEntry::Failed("the picture is H.264".to_owned())
            ));
            let picture = draw("video", "", &refused, Rect::new(0, 0, 40, 8));
            assert!(
                rows(&picture).contains("H.264"),
                "the refusal is the note: {}",
                rows(&picture)
            );
        }

        #[test]
        fn the_poll_names_each_clip_once_and_stops_at_its_strip() {
            let mut state = playing("$: s(\"clip.webm\")._video()", vec![clip_event(10.0)], 0.0);
            let asks = state.video_names_to_ask(false);
            assert_eq!(asks, vec!["clip.webm".to_owned()]);
            // Another lane's sound is not this painter's to ask for: the
            // spiral's lane is slot 1.
            let mut other = clip_event(10.2);
            other.onset_id = 2;
            other.ui_visuals = 1_u64 << 1;
            other.value = Some("s:other.webm".to_owned());
            let wider = playing(
                "stack(s('clip.webm')._video(), s('other.webm')._spiral())",
                vec![clip_event(10.0), other],
                0.0,
            );
            assert_eq!(
                wider.video_names_to_ask(false),
                vec!["clip.webm".to_owned()],
                "the spiral's sound is not asked for"
            );
            // The dock's video widget picks its feed from any lane, so
            // its ask is for every sound the set plays.
            let mut every: Vec<String> = wider.video_names_to_ask(true);
            every.sort();
            assert_eq!(
                every,
                vec!["clip.webm".to_owned(), "other.webm".to_owned()],
                "every sound, whatever its lane"
            );
            state.note_video_ask("clip.webm");
            assert_eq!(
                state.video_names_to_ask(false),
                vec!["clip.webm".to_owned()],
                "an unanswered ask is asked again"
            );
            assert!(
                state.install_video("clip.webm", VideoEntry::Ready(std::sync::Arc::new(strip())))
            );
            assert!(
                state.video_names_to_ask(false).is_empty(),
                "a strip answers the ask"
            );
        }

        #[test]
        fn a_looping_clip_wraps_its_picture_back_to_the_start() {
            // Three frames: red, green, blue, the last at half a second.
            let strip = rustel_runtime::video::VideoThumbnails::from_frames(
                2,
                2,
                vec![
                    (0.0, [255, 0, 0, 255].repeat(4)),
                    (0.25, [0, 255, 0, 255].repeat(4)),
                    (0.5, [0, 0, 255, 255].repeat(4)),
                ],
            )
            .expect("strip");
            let mut looping = playing("$: s(\"clip.webm\")._video()", vec![clip_event(9.3)], 0.0);
            let mut chain = clip_event(9.3);
            chain.value = Some("s:clip.webm loop:1".to_owned());
            looping.events.pop_back();
            looping.events.push_back(chain);
            assert!(
                looping.install_video("clip.webm", VideoEntry::Ready(std::sync::Arc::new(strip)))
            );
            // 0.7 s in: past the clip's end, so the loop wraps to 0.2 s -
            // the red frame - where the unlooping clip holds the blue one.
            let picture = draw("video", "", &looping, Rect::new(0, 0, 20, 8));
            assert!(
                picture
                    .content
                    .iter()
                    .any(|cell| cell.fg == Color::Rgb(255, 0, 0)),
                "the loop wrapped the picture to its start"
            );
            assert!(
                picture
                    .content
                    .iter()
                    .all(|cell| cell.fg != Color::Rgb(0, 0, 255)),
                "the last frame is not held while the clip loops"
            );
        }

        #[test]
        fn a_named_feed_shows_its_own_event_s_place_in_the_clip() {
            // The dock's widget and the detached window ask by name: the
            // position is the named clip's own event, not another's.
            let mut a_event = clip_event(10.0);
            a_event.value = Some("s:a.webm".to_owned());
            let mut state = playing(
                "stack(s('a.webm')._video(), s('b.webm')._video())",
                vec![a_event],
                0.0,
            );
            assert!(state.install_video("a.webm", VideoEntry::Ready(std::sync::Arc::new(strip()))));
            assert!(state.install_video("b.webm", VideoEntry::Ready(std::sync::Arc::new(strip()))));
            // `a.webm` has its event sounding now; `b.webm` has none, and
            // shows its poster frame - the clip's first.
            assert!(state.video_feed_frame("a.webm").is_some());
            let (position, _) = state
                .video_feed_frame("b.webm")
                .expect("a landed strip always has a frame to show");
            assert_eq!(position, 0.0, "the poster is the clip's first frame");
            assert_eq!(state.video_feeds(), vec!["a.webm", "b.webm"]);
        }

        #[test]
        fn a_clip_that_never_answers_is_said_so_and_left_alone() {
            let mut state = playing("$: s(\"clip.webm\")._video()", vec![clip_event(10.0)], 0.0);
            let mut ran_out = false;
            for _ in 0..=VIDEO_ASK_LIMIT {
                ran_out = state.note_video_ask("clip.webm");
            }
            assert!(ran_out, "the last ask says the asking ran out");
            assert!(
                matches!(state.video_for("clip.webm"), Some(VideoEntry::Failed(_))),
                "the asking ran out"
            );
            assert!(
                state.video_names_to_ask(false).is_empty(),
                "a failed clip is not asked for again"
            );
            // A new score gives the clip its chance back: the failed note
            // is gone, and the next events ask again. A strip that lands
            // meanwhile comes straight from the library's own table.
            state.install_layout(
                rustel_runtime::ui_events::visual_layout("$: s(\"clip.webm\")._video().slow(2)", 2)
                    .expect("layout"),
            );
            assert!(
                state.video_for("clip.webm").is_none(),
                "the new score holds no stale refusal"
            );
        }
    }
}
