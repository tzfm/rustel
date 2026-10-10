//! One plugin: made and prepared on the plugin thread, run in the audio
//! callback, and returned to the plugin thread at the end.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use rustel_audio::{InsertKey, InsertNote, InsertParam, OrbitInsert};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper};

use crate::com::{Changes, Events, Handler, HostApp, Stream, pointer, wide_to_string};
use crate::module::{ClassInfo, Module};
use crate::{Job, ParamInfo, canonical, preset};

/// The engine gives an insert from 1 to 128 frames in one call.
pub(crate) const MAX_BLOCK: usize = 128;
/// A bus with more channels than this is not an audio bus the host serves.
const MAX_BUS_CHANNELS: usize = 64;
const AUDIO: MediaType = MediaTypes_::kAudio as MediaType;
const EVENT: MediaType = MediaTypes_::kEvent as MediaType;
const INPUT: BusDirection = BusDirections_::kInput as BusDirection;
const OUTPUT: BusDirection = BusDirections_::kOutput as BusDirection;
/// The clock a plugin gets until the score gives one.
const DEFAULT_TEMPO: f64 = 120.0;
/// The notes one plugin holds at one time.
const MAX_NOTES: usize = 32;
/// The parameter values and restores that wait for their frame at one time.
const MAX_TIMED_PARAMS: usize = 64;
/// The parameters with a value from a note at one time. A parameter past
/// this count keeps its value when a later note does not carry its key.
const MAX_CHANGED_PARAMS: usize = 32;

/// A parameter value, or a restore of earlier values, at one frame of the
/// instance. A restore has no parameter number.
#[derive(Clone, Copy, Default)]
struct TimedParam {
    at: i64,
    id: Option<u32>,
    value: f64,
}

/// A note the plugin plays or will play. The times are frame counts of the
/// instance.
#[derive(Clone, Copy, Default)]
struct HeldNote {
    on_at: i64,
    off_at: i64,
    key: i16,
    /// The distance from the key in cents.
    tuning: f32,
    velocity: f32,
    started: bool,
}

impl HeldNote {
    /// The start or the end of the note as an event `offset` frames into
    /// the block, at the position `beats` in quarter notes.
    fn event(&self, on: bool, offset: i64, beats: f64) -> Event {
        // SAFETY: an event is numbers only, and all zero is an event.
        let mut event: Event = unsafe { std::mem::zeroed() };
        event.sampleOffset = offset.max(0) as i32;
        event.ppqPosition = beats;
        if on {
            event.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
            event.__field0.noteOn = NoteOnEvent {
                channel: 0,
                pitch: self.key,
                tuning: self.tuning,
                velocity: self.velocity,
                length: 0,
                noteId: -1,
            };
        } else {
            event.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
            event.__field0.noteOff = NoteOffEvent {
                channel: 0,
                pitch: self.key,
                velocity: 0.0,
                noteId: -1,
                tuning: self.tuning,
            };
        }
        event
    }
}

/// A plugin with its two halves connected. The plugin thread owns a
/// `Loaded` value from the first call to the last.
pub(crate) struct Loaded {
    // The order of the fields is the order of release. The plugin objects
    // go before the host objects, and the library goes last.
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    controller: Option<ComPtr<IEditController>>,
    processor: ComPtr<IAudioProcessor>,
    component: ComPtr<IComponent>,
    /// The controller is an object of its own and has its own start and end.
    separate_controller: bool,
    active: Cell<bool>,
    /// The count of running copies of the plugin, held while this one runs.
    running: Option<Arc<std::sync::atomic::AtomicUsize>>,
    _handler: ComWrapper<Handler>,
    _host: ComWrapper<HostApp>,
    _module: Arc<Module>,
}

// SAFETY: a `Loaded` value moves between threads only inside an `Instance`,
// and the plugin thread makes every call except the audio processing.
unsafe impl Send for Loaded {}

impl Loaded {
    pub(crate) fn new(module: Arc<Module>, class: &ClassInfo) -> Result<Self, String> {
        let host = ComWrapper::new(HostApp);
        let handler = ComWrapper::new(Handler);
        let context: *mut FUnknown = pointer::<_, IHostApplication>(&host).cast();
        module.set_host(context);

        let component: ComPtr<IComponent> = module
            .create(&class.cid)
            .ok_or("the plugin did not make its processor")?;
        if unsafe { component.initialize(context) } != kResultOk {
            return Err("the plugin processor did not start".into());
        }
        let Some(processor) = component.cast::<IAudioProcessor>() else {
            unsafe { component.terminate() };
            return Err("the plugin has no audio processor".into());
        };

        // A plugin keeps its parameters in a controller. The controller is
        // the processor object itself or an object of a second class.
        let mut controller = component.cast::<IEditController>();
        let mut separate_controller = false;
        if controller.is_none() {
            let mut cid: TUID = [0; 16];
            if unsafe { component.getControllerClassId(&mut cid) } == kResultOk
                && let Some(made) = module.create::<IEditController>(&cid)
                && unsafe { made.initialize(context) } == kResultOk
            {
                controller = Some(made);
                separate_controller = true;
            }
        }

        let mut connection = None;
        if let Some(controller) = &controller {
            unsafe { controller.setComponentHandler(pointer(&handler)) };
            if separate_controller
                && let Some(from) = component.cast::<IConnectionPoint>()
                && let Some(to) = controller.cast::<IConnectionPoint>()
            {
                unsafe {
                    from.connect(to.as_ptr());
                    to.connect(from.as_ptr());
                }
                connection = Some((from, to));
            }
        }
        let loaded = Self {
            connection,
            controller,
            processor,
            component,
            separate_controller,
            active: Cell::new(false),
            running: None,
            _handler: handler,
            _host: host,
            _module: module,
        };
        // The controller starts from the state of the processor.
        let state = Stream::new(Vec::new());
        if unsafe { loaded.component.getState(pointer(&state)) } == kResultOk && !state.is_empty() {
            loaded.show_state_to_controller(&state);
        }
        Ok(loaded)
    }

    fn show_state_to_controller(&self, state: &ComWrapper<Stream>) {
        if let Some(controller) = &self.controller {
            state.rewind();
            unsafe { controller.setComponentState(pointer(state)) };
        }
    }

    /// The parameters a score sets: not hidden and not read-only.
    pub(crate) fn params(&self) -> Vec<ParamInfo> {
        let Some(controller) = &self.controller else {
            return Vec::new();
        };
        let skipped = ParameterInfo_::ParameterFlags_::kIsReadOnly
            | ParameterInfo_::ParameterFlags_::kIsHidden;
        let groups = self.groups();
        let mut params: Vec<ParamInfo> = Vec::new();
        for index in 0..unsafe { controller.getParameterCount() } {
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            if unsafe { controller.getParameterInfo(index, &mut info) } != kResultOk
                || info.flags & skipped != 0
            {
                continue;
            }
            let name = wide_to_string(&info.title);
            // A score names a parameter by its title. A title that is
            // empty or already used gives way to the parameter number, and
            // so does `preset`: a score writes that word for a preset file.
            let mut key = canonical(&name);
            if key.is_empty() || key == "preset" || params.iter().any(|param| param.key == key) {
                key = info.id.to_string();
            }
            let mut text: String128 = [0; 128];
            let default = info.defaultNormalizedValue;
            let shown = unsafe { controller.getParamStringByValue(info.id, default, &mut text) };
            params.push(ParamInfo {
                id: info.id,
                name,
                key,
                group: groups.get(&info.unitId).cloned().unwrap_or_default(),
                units: wide_to_string(&info.units),
                default,
                steps: info.stepCount.max(0) as u32,
                default_text: if shown == kResultOk {
                    wide_to_string(&text)
                } else {
                    String::new()
                },
            });
        }
        params
    }

    /// The value the plugin has now for each of `params`, in the order of
    /// the parameter numbers.
    fn param_values(&self, params: &[ParamInfo]) -> Box<[(u32, f64)]> {
        let Some(controller) = &self.controller else {
            return Box::default();
        };
        let value =
            |param: &ParamInfo| (param.id, unsafe { controller.getParamNormalized(param.id) });
        let mut values: Vec<(u32, f64)> = params.iter().map(value).collect();
        values.sort_by_key(|value| value.0);
        values.dedup_by_key(|value| value.0);
        values.into_boxed_slice()
    }

    /// The path of each parameter group of the plugin, by group number. A
    /// plugin with no groups gives an empty table.
    fn groups(&self) -> std::collections::HashMap<UnitID, String> {
        let Some(units) = self
            .controller
            .as_ref()
            .and_then(|controller| controller.cast::<IUnitInfo>())
        else {
            return Default::default();
        };
        let mut named = std::collections::HashMap::new();
        for index in 0..unsafe { units.getUnitCount() } {
            let mut info: UnitInfo = unsafe { std::mem::zeroed() };
            if unsafe { units.getUnitInfo(index, &mut info) } == kResultOk {
                named.insert(info.id, (info.parentUnitId, wide_to_string(&info.name)));
            }
        }
        // The path of a group is the names from the top level down. The
        // root group has number 0 and adds no name.
        named
            .keys()
            .map(|id| {
                let mut path = Vec::new();
                let mut at = *id;
                while at != kRootUnitId && path.len() < 8 {
                    let Some((parent, name)) = named.get(&at) else {
                        break;
                    };
                    path.push(name.as_str());
                    at = *parent;
                }
                path.reverse();
                (*id, path.join("/"))
            })
            .collect()
    }

    /// Sets the plugin to the state of a `.vstpreset` file.
    pub(crate) fn set_preset(&self, file: &[u8], class: &ClassInfo) -> Result<(), String> {
        let preset = preset::parse(file)?;
        if !preset.class.eq_ignore_ascii_case(&class.id_text()) {
            return Err("the preset is for a different plugin".into());
        }
        let state = Stream::new(preset.component.to_vec());
        if unsafe { self.component.setState(pointer(&state)) } != kResultOk {
            return Err("the plugin refused the preset".into());
        }
        self.show_state_to_controller(&state);
        if let (Some(controller), Some(bytes)) = (&self.controller, preset.controller) {
            let state = Stream::new(bytes.to_vec());
            unsafe { controller.setState(pointer(&state)) };
        }
        Ok(())
    }

    fn bus_channels(&self, direction: BusDirection) -> Vec<usize> {
        let count = unsafe { self.component.getBusCount(AUDIO, direction) }.max(0);
        (0..count)
            .map(|index| {
                let mut arrangement: SpeakerArrangement = 0;
                let known = unsafe {
                    self.processor
                        .getBusArrangement(direction, index, &mut arrangement)
                } == kResultOk;
                let channels = if known {
                    arrangement.count_ones() as usize
                } else {
                    let mut info: BusInfo = unsafe { std::mem::zeroed() };
                    unsafe {
                        self.component
                            .getBusInfo(AUDIO, direction, index, &mut info)
                    };
                    info.channelCount.max(0) as usize
                };
                channels.min(MAX_BUS_CHANNELS)
            })
            .collect()
    }

    /// Asks for stereo on the first input and the first output, and keeps
    /// what the plugin has on every other bus.
    fn ask_for_stereo(&self) {
        let arrangements = |direction| -> Vec<SpeakerArrangement> {
            let count = unsafe { self.component.getBusCount(AUDIO, direction) }.max(0);
            (0..count)
                .map(|index| {
                    let mut arrangement = SpeakerArr::kStereo;
                    if index > 0 {
                        unsafe {
                            self.processor
                                .getBusArrangement(direction, index, &mut arrangement)
                        };
                    }
                    arrangement
                })
                .collect()
        };
        let mut inputs = arrangements(INPUT);
        let mut outputs = arrangements(OUTPUT);
        // A plugin that refuses keeps its own layout. The host reads the
        // layout back after this call.
        unsafe {
            self.processor.setBusArrangements(
                inputs.as_mut_ptr(),
                inputs.len() as i32,
                outputs.as_mut_ptr(),
                outputs.len() as i32,
            )
        };
    }

    /// Prepares the plugin for audio at one sample rate. `params` are the
    /// parameters a score sets: the copy keeps their values at this time.
    pub(crate) fn activate(
        mut self,
        key: InsertKey,
        sample_rate: u32,
        retire: Sender<Job>,
        running: Arc<std::sync::atomic::AtomicUsize>,
        params: &[ParamInfo],
    ) -> Result<Instance, String> {
        let sample32 = SymbolicSampleSizes_::kSample32 as i32;
        if unsafe { self.processor.canProcessSampleSize(sample32) } != kResultOk {
            return Err("the plugin does not process 32-bit audio".into());
        }
        self.ask_for_stereo();
        let input_channels = self.bus_channels(INPUT);
        let output_channels = self.bus_channels(OUTPUT);
        // The order is the order of the VST3 call sequence: the bus
        // layout, the processing setup, the buses on, the plugin on.
        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kRealtime as i32,
            symbolicSampleSize: sample32,
            maxSamplesPerBlock: MAX_BLOCK as i32,
            sampleRate: f64::from(sample_rate),
        };
        if unsafe { self.processor.setupProcessing(&mut setup) } != kResultOk {
            return Err(format!("the plugin refused {sample_rate} Hz"));
        }
        // Only the first input and the first output carry audio. A side
        // input stays off and reads silence.
        for (direction, buses) in [(INPUT, &input_channels), (OUTPUT, &output_channels)] {
            for index in 0..buses.len() {
                unsafe {
                    self.component
                        .activateBus(AUDIO, direction, index as i32, u8::from(index == 0))
                };
            }
        }
        if unsafe { self.component.getBusCount(EVENT, INPUT) } > 0 {
            unsafe { self.component.activateBus(EVENT, INPUT, 0, 1) };
        }
        if unsafe { self.component.setActive(1) } != kResultOk {
            return Err("the plugin did not turn on".into());
        }
        let at_start = self.param_values(params);
        self.active.set(true);
        unsafe { self.processor.setProcessing(1) };
        running.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.running = Some(running);

        // One block of samples for each channel of each bus, the inputs
        // first. The plugin reads and writes these blocks through the
        // pointers, and so does the host.
        let total: usize = input_channels.iter().chain(&output_channels).sum();
        let mut storage = vec![0.0f32; total.max(1) * MAX_BLOCK].into_boxed_slice();
        let base = storage.as_mut_ptr();
        let mut channels: Box<[*mut f32]> = (0..total)
            .map(|channel| unsafe { base.add(channel * MAX_BLOCK) })
            .collect();
        let mut first = 0;
        let mut buses = |counts: &[usize], silent: bool| -> Box<[AudioBusBuffers]> {
            counts
                .iter()
                .enumerate()
                .map(|(index, count)| {
                    let buffers = AudioBusBuffers {
                        numChannels: *count as i32,
                        // A side input holds silence in every channel.
                        silenceFlags: if silent && index > 0 && *count > 0 {
                            u64::MAX >> (64 - count)
                        } else {
                            0
                        },
                        __field0: AudioBusBuffers__type0 {
                            channelBuffers32: if *count == 0 {
                                std::ptr::null_mut()
                            } else {
                                unsafe { channels.as_mut_ptr().add(first) }
                            },
                        },
                    };
                    first += count;
                    buffers
                })
                .collect()
        };
        let inputs = buses(&input_channels, true);
        let outputs = buses(&output_channels, false);
        let main_input = input_channels.first().copied().unwrap_or(0);
        let main_output = output_channels.first().copied().unwrap_or(0);
        let output_start: usize = input_channels.iter().sum();
        Ok(Instance {
            key,
            plugin: Some(self),
            changes: Changes::new(),
            events: Events::new(),
            notes: [HeldNote::default(); MAX_NOTES],
            note_count: 0,
            cut_at: None,
            params: [TimedParam::default(); MAX_TIMED_PARAMS],
            param_count: 0,
            base: at_start,
            changed: [(0, 0); MAX_CHANGED_PARAMS],
            changed_count: 0,
            inputs,
            outputs,
            main_input: (0, main_input),
            main_output: (output_start, main_output),
            channels,
            _storage: storage,
            sample_rate: f64::from(sample_rate),
            frames: 0,
            beats: 0.0,
            tempo: DEFAULT_TEMPO,
            order: 0,
            held: None,
            retire,
        })
    }
}

impl Drop for Loaded {
    fn drop(&mut self) {
        if let Some(running) = &self.running {
            running.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        unsafe {
            if self.active.get() {
                self.processor.setProcessing(0);
                self.component.setActive(0);
            }
            if let Some((from, to)) = &self.connection {
                from.disconnect(to.as_ptr());
                to.disconnect(from.as_ptr());
            }
            if let Some(controller) = &self.controller {
                controller.setComponentHandler(std::ptr::null_mut());
                if self.separate_controller {
                    controller.terminate();
                }
            }
            self.component.terminate();
        }
    }
}

/// A plugin on an orbit. The audio callback owns the value.
pub(crate) struct Instance {
    key: InsertKey,
    /// Goes back to the plugin thread at the end.
    plugin: Option<Loaded>,
    changes: ComWrapper<Changes>,
    events: ComWrapper<Events>,
    notes: [HeldNote; MAX_NOTES],
    note_count: usize,
    /// A rewind ends each note at this frame count of the instance.
    cut_at: Option<i64>,
    params: [TimedParam; MAX_TIMED_PARAMS],
    param_count: usize,
    /// The value of each parameter at the build of the copy, after the
    /// preset, in the order of the parameter numbers.
    base: Box<[(u32, f64)]>,
    /// Each parameter delivered from a note, and the frame count of the
    /// instance at its last delivered value.
    changed: [(u32, i64); MAX_CHANGED_PARAMS],
    changed_count: usize,
    inputs: Box<[AudioBusBuffers]>,
    outputs: Box<[AudioBusBuffers]>,
    /// The first channel and the channel count of the bus that carries audio.
    main_input: (usize, usize),
    main_output: (usize, usize),
    channels: Box<[*mut f32]>,
    _storage: Box<[f32]>,
    sample_rate: f64,
    frames: i64,
    /// The position in quarter notes, and the quarter notes in one minute.
    beats: f64,
    tempo: f64,
    /// The place of this instance in the order the host built them.
    order: u64,
    /// Alive while an engine holds this instance: see [`Instance::hold`].
    held: Option<std::sync::Arc<()>>,
    retire: Sender<Job>,
}

// SAFETY: one thread owns an instance at a time. The pointers go to memory
// the instance owns, and the plugin objects go back to the plugin thread.
unsafe impl Send for Instance {}

impl Instance {
    /// Queues a value or a restore. The last value for one parameter and
    /// one frame wins. Chords need only one restore at each frame.
    fn set_at(&mut self, id: Option<u32>, at: i64, value: f64) {
        let waiting = &mut self.params[..self.param_count];
        if let Some(known) = waiting
            .iter_mut()
            .find(|known| known.id == id && known.at == at)
        {
            known.value = value;
        } else if self.param_count < MAX_TIMED_PARAMS {
            self.params[self.param_count] = TimedParam { at, id, value };
            self.param_count += 1;
        }
    }

    /// Ends the plugin copy now. Only the plugin thread calls this.
    pub(crate) fn end_here(mut self: Box<Self>) {
        drop(self.plugin.take());
    }

    pub(crate) fn order(&self) -> u64 {
        self.order
    }

    /// Marks the instance as held by an engine. The mark the host gets is
    /// alive until the instance ends.
    pub(crate) fn hold(&mut self) -> std::sync::Weak<()> {
        let held = std::sync::Arc::new(());
        let mark = std::sync::Arc::downgrade(&held);
        self.held = Some(held);
        mark
    }

    pub(crate) fn set_order(&mut self, order: u64) {
        self.order = order;
    }

    fn channel(&mut self, channel: usize, frames: usize) -> &mut [f32] {
        unsafe { std::slice::from_raw_parts_mut(self.channels[channel], frames) }
    }

    /// Puts the note starts and the note ends of the next `frames` frames
    /// in the event list. The sort puts an end before a start at the same
    /// frame, so a key is free for the note that follows on the same key.
    /// The last pass ends a note that starts and ends in these frames.
    fn queue_note_events(&mut self, frames: usize) {
        self.events.clear();
        let now = self.frames;
        let end = now + frames as i64;
        let beats_for_frame = self.tempo / 60.0 / self.sample_rate;
        let beats = self.beats;
        let position = |at: i64| beats + (at - now).max(0) as f64 * beats_for_frame;
        for ends in [true, false, true] {
            let mut at = 0;
            while at < self.note_count {
                let note = &mut self.notes[at];
                if ends && note.started && note.off_at < end {
                    let event = note.event(false, note.off_at - now, position(note.off_at));
                    self.events.push(event);
                    self.note_count -= 1;
                    self.notes[at] = self.notes[self.note_count];
                    continue;
                }
                if !ends && !note.started && note.on_at < end {
                    note.started = true;
                    let event = note.event(true, note.on_at - now, position(note.on_at));
                    self.events.push(event);
                }
                at += 1;
            }
        }
        self.events.sort();
        if self.cut_at.is_some_and(|cut| cut < end) {
            self.cut_at = None;
        }
    }

    /// Puts the parameter values of the next `frames` frames in the change
    /// list, each at its own frame.
    fn queue_param_changes(&mut self, frames: usize) {
        self.changes.clear();
        // Notes sometimes arrive out of order. Restore before all values at one
        // frame so the notes of a chord keep each other's parameters.
        self.params[..self.param_count]
            .sort_unstable_by_key(|param| (param.at, param.id.is_some()));
        let now = self.frames;
        let end = now + frames as i64;
        let mut kept = 0;
        for at in 0..self.param_count {
            let param = self.params[at];
            if param.at < end {
                let offset = (param.at - now).max(0) as i32;
                if let Some(id) = param.id {
                    if self.changes.push(id, offset, param.value) {
                        let changed = &mut self.changed[..self.changed_count];
                        if let Some(known) = changed.iter_mut().find(|known| known.0 == id) {
                            known.1 = param.at;
                        } else if self.changed_count < MAX_CHANGED_PARAMS {
                            self.changed[self.changed_count] = (id, param.at);
                            self.changed_count += 1;
                        }
                    }
                } else {
                    let mut changed = 0;
                    for index in 0..self.changed_count {
                        let (id, set_at) = self.changed[index];
                        let restored = set_at < param.at
                            && self
                                .base
                                .binary_search_by_key(&id, |known| known.0)
                                .is_ok_and(|found| {
                                    self.changes.push(id, offset, self.base[found].1)
                                });
                        if !restored {
                            self.changed[changed] = (id, set_at);
                            changed += 1;
                        }
                    }
                    self.changed_count = changed;
                }
            } else {
                self.params[kept] = param;
                kept += 1;
            }
        }
        self.param_count = kept;
    }
}

impl OrbitInsert for Instance {
    fn key(&self) -> InsertKey {
        self.key
    }

    fn set_param(&mut self, param: InsertParam, frames: u32) {
        if !param.value.is_finite() {
            return;
        }
        let at = self.frames + i64::from(frames);
        self.set_at(Some(param.id), at, f64::from(param.value.clamp(0.0, 1.0)));
    }

    fn restore_params(&mut self, frames: u32) {
        let at = self.frames + i64::from(frames);
        self.set_at(None, at, 0.0);
    }

    fn note(&mut self, note: InsertNote, frames: u32) {
        if !note.pitch.is_finite() {
            return;
        }
        let key = note.pitch.round().clamp(0.0, 127.0);
        let on_at = self.frames + i64::from(frames);
        let coincident = self.notes[..self.note_count]
            .iter()
            .position(|held| held.key == key as i16 && held.on_at == on_at);
        if self.note_count == MAX_NOTES && coincident.is_none() {
            return;
        }
        let mut off_at = on_at + i64::from(note.frames.max(1));
        // A note of the score before a rewind ends at the rewind.
        if let Some(cut) = self.cut_at.filter(|cut| on_at < *cut) {
            off_at = off_at.min(cut);
        }
        // A key plays one note at a time. A plugin ends a note by its key,
        // so the end of a note one frame after the start of the next note
        // on the key would end the new note. The note before ends where
        // the note after starts.
        for held in &mut self.notes[..self.note_count] {
            if held.key != key as i16 {
                continue;
            }
            if held.on_at < on_at {
                held.off_at = held.off_at.min(on_at);
            } else if held.on_at > on_at {
                off_at = off_at.min(held.on_at);
            }
        }
        if let Some(at) = coincident {
            // Coincident notes on one key share one start and keep the
            // longer duration, bounded by the next note or cut above.
            self.notes[at].off_at = self.notes[at].off_at.max(off_at);
            return;
        }
        self.notes[self.note_count] = HeldNote {
            on_at,
            off_at,
            key: key as i16,
            tuning: (note.pitch - key) * 100.0,
            velocity: note.velocity.clamp(0.0, 1.0),
            started: false,
        };
        self.note_count += 1;
    }

    fn cut_notes(&mut self, frames: u32) {
        let cut = self.frames + i64::from(frames);
        self.cut_at = Some(cut);
        let mut kept = 0;
        for at in 0..self.note_count {
            let mut note = self.notes[at];
            if note.started || note.on_at < cut {
                note.off_at = note.off_at.min(cut);
                self.notes[kept] = note;
                kept += 1;
            }
        }
        self.note_count = kept;
        // A value that waits for a frame after the cut is a value of a note
        // the rewind took away.
        let mut kept = 0;
        for at in 0..self.param_count {
            if self.params[at].at < cut {
                self.params[kept] = self.params[at];
                kept += 1;
            }
        }
        self.param_count = kept;
    }

    fn busy(&self) -> bool {
        // A cut that waits needs the frame count to go on: a sleeping
        // instance does not count frames.
        self.note_count > 0 || self.param_count > 0 || self.cut_at.is_some()
    }

    fn reset(&mut self) {
        self.param_count = 0;
        self.cut_at = None;
        // A note that sounds ends at the next block. A note that did not
        // start is gone.
        let now = self.frames;
        let mut kept = 0;
        for at in 0..self.note_count {
            if self.notes[at].started {
                self.notes[at].off_at = now;
                self.notes[kept] = self.notes[at];
                kept += 1;
            }
        }
        self.note_count = kept;
    }

    fn sync(&mut self, beats: f64, tempo: f32, frames: u32) {
        self.tempo = f64::from(tempo);
        self.beats = beats - f64::from(frames) * self.tempo / 60.0 / self.sample_rate;
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len()).min(MAX_BLOCK);
        let Some(plugin) = &self.plugin else {
            return;
        };
        if frames == 0 {
            return;
        }
        let processor = plugin.processor.as_ptr();
        match self.main_input {
            (_, 0) => {}
            (first, 1) => {
                let mono = self.channel(first, frames);
                for (mono, (left, right)) in mono.iter_mut().zip(left.iter().zip(right.iter())) {
                    *mono = (left + right) * 0.5;
                }
            }
            (first, _) => {
                self.channel(first, frames).copy_from_slice(&left[..frames]);
                self.channel(first + 1, frames)
                    .copy_from_slice(&right[..frames]);
            }
        }
        for bus in self.outputs.iter_mut() {
            bus.silenceFlags = 0;
        }
        // The output starts as the input of an effect, and as silence for
        // an instrument. A plugin with its bypass on can end the call with
        // no write: a DAW with one block for input and output then has the
        // input in the output, and so has this host.
        let dry = self.main_input.1 > 0;
        match self.main_output {
            (_, 0) => {}
            (first, 1) => {
                let mono = self.channel(first, frames);
                for (mono, (left, right)) in mono.iter_mut().zip(left.iter().zip(right.iter())) {
                    *mono = if dry { (left + right) * 0.5 } else { 0.0 };
                }
            }
            (first, _) => {
                for (channel, block) in [(first, &*left), (first + 1, &*right)] {
                    let output = self.channel(channel, frames);
                    match dry {
                        true => output.copy_from_slice(&block[..frames]),
                        false => output.fill(0.0),
                    }
                }
            }
        }
        self.queue_note_events(frames);
        self.queue_param_changes(frames);
        let state = ProcessContext_::StatesAndFlags_::kPlaying
            | ProcessContext_::StatesAndFlags_::kTempoValid
            | ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid
            | ProcessContext_::StatesAndFlags_::kBarPositionValid
            | ProcessContext_::StatesAndFlags_::kTimeSigValid
            | ProcessContext_::StatesAndFlags_::kContTimeValid;
        let mut context: ProcessContext = unsafe { std::mem::zeroed() };
        context.state = state as u32;
        context.sampleRate = self.sample_rate;
        context.projectTimeSamples = self.frames;
        context.continousTimeSamples = self.frames;
        context.projectTimeMusic = self.beats;
        context.barPositionMusic = (self.beats / 4.0).floor() * 4.0;
        context.tempo = self.tempo;
        context.timeSigNumerator = 4;
        context.timeSigDenominator = 4;
        let mut data = ProcessData {
            processMode: ProcessModes_::kRealtime as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: frames as i32,
            numInputs: self.inputs.len() as i32,
            numOutputs: self.outputs.len() as i32,
            inputs: self.inputs.as_mut_ptr(),
            outputs: self.outputs.as_mut_ptr(),
            inputParameterChanges: pointer(&self.changes),
            outputParameterChanges: std::ptr::null_mut(),
            inputEvents: pointer(&self.events),
            outputEvents: std::ptr::null_mut(),
            processContext: &mut context,
        };
        // SAFETY: the plugin is active, and every pointer in `data` goes to
        // memory this instance owns for the full call.
        unsafe { ((*(*processor).vtbl).process)(processor, &mut data) };
        self.changes.clear();
        self.frames += frames as i64;
        self.beats += frames as f64 * self.tempo / 60.0 / self.sample_rate;
        // A plugin marks a channel with no sound, and has no duty to write
        // such a channel.
        let (first, count) = self.main_output;
        let silent = self.outputs.first().map_or(0, |bus| bus.silenceFlags);
        for channel in 0..count.min(2) {
            if silent & (1 << channel) != 0 {
                self.channel(first + channel, frames).fill(0.0);
            }
        }
        match self.main_output {
            // A plugin with no audio output leaves the signal as it is.
            (_, 0) => {}
            (first, 1) => {
                let mono = self.channel(first, frames);
                left[..frames].copy_from_slice(mono);
                right[..frames].copy_from_slice(mono);
            }
            (first, _) => {
                left[..frames].copy_from_slice(self.channel(first, frames));
                right[..frames].copy_from_slice(self.channel(first + 1, frames));
            }
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let Some(plugin) = self.plugin.take() else {
            return;
        };
        // The plugin ends on the plugin thread. With no such thread left,
        // the plugin ends here.
        if let Err(returned) = self.retire.send(Box::new(move || drop(plugin))) {
            (returned.0)();
        }
    }
}
