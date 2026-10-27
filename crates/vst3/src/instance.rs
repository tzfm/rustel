//! One plugin: made and prepared on the plugin thread, run in the audio
//! callback or on a plugin worker of the engine, one thread at a time, and
//! returned to the plugin thread at the end.

use std::cell::Cell;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;

use rustel_audio::{
    INSERT_DRAIN_QUIET_SECONDS, InsertKey, InsertNote, InsertParam, MAX_INSERT_PARAMS, OrbitInsert,
};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper};

use crate::com::{
    Changes, Events, Handler, HostApp, MAX_CHANGES, MAX_EVENTS, Stream, pointer, wide_to_string,
};
use crate::module::{ClassInfo, Module};
use crate::{Job, ParamInfo, ParamRole, canonical, preset};

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
/// The notes one plugin holds at one time: one for each key.
const MAX_NOTES: usize = 128;
// One block carries the start and the end of each note at most. The event
// list has room for all of them: a lost end is a note that does not stop.
const _: () = assert!(MAX_EVENTS >= 2 * MAX_NOTES);
/// The parameter values and restores that wait for their frame at one time.
const MAX_TIMED_PARAMS: usize = 512;
/// The parameters with a value from a note at one time. A parameter past
/// this count keeps its value when a later note does not carry its key.
const MAX_CHANGED_PARAMS: usize = 32;
// One block carries the values of a note and the restore of the values of
// the note before.
const _: () = assert!(MAX_CHANGES >= 2 * MAX_INSERT_PARAMS);
/// A block with a peak at this level or below is silent: see [`Played`].
const QUIET_PEAK: f32 = 1e-6;
/// The longest tail of a copy, in seconds. A plugin with a longer tail, or
/// with a tail with no end, counts as this. The long flush plays out this
/// much at most: see [`play_out`].
const LONGEST_TAIL: f64 = 3.0;
/// The silent blocks after the latency in the quiet check of a flush: see
/// [`play_out`].
const CHECK_BLOCKS: usize = 4;

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

/// Where a plugin copy is, as its cell says: see [`CopyState::place`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Place {
    /// The host keeps the copy for its slot, or the copy ended.
    Home,
    /// An engine holds the copy: see [`Instance::hold`].
    Engine,
    /// The engine gave the copy back, and the plugin thread takes it home.
    Returning,
}

/// The cell of one plugin copy. The host creates the cell when it creates
/// the plugin. The copy keeps the same cell to its end. Each field is an
/// atomic. Any thread reads and writes the cell with no lock.
#[derive(Default)]
pub(crate) struct CopyState {
    /// Where the copy is: a [`Place`].
    place: AtomicU8,
    /// The latency of the plugin in frames. The plugin thread reads it
    /// from the plugin after the first blocks of the copy.
    pub latency: AtomicU32,
    /// The tail of the plugin in frames, 3 s at most: the time its sound
    /// goes on after its input goes silent. The plugin thread reads it with
    /// the latency.
    pub tail: AtomicU32,
    /// The plugin asked for a restart because its latency changed. Only
    /// the plugin thread acts on the flag.
    pub latency_changed: AtomicBool,
    /// The stamp of the preset file at the build of the copy: see
    /// [`crate::cache::Stamp::number`]. A copy of preset 0 reads no file and
    /// has no stamp.
    pub preset: AtomicU64,
    /// A read of the preset folders found the preset file of the copy
    /// changed or gone while the copy was in an engine, on its way home or
    /// in its retune. Only the plugin thread acts on the mark: the copy ends
    /// at its park, and its retune builds a new copy from the file.
    pub preset_changed: AtomicBool,
}

impl CopyState {
    /// Where the copy is.
    pub(crate) fn place(&self) -> Place {
        match self.place.load(Ordering::Relaxed) {
            place if place == Place::Engine as u8 => Place::Engine,
            place if place == Place::Returning as u8 => Place::Returning,
            _ => Place::Home,
        }
    }

    pub(crate) fn set_place(&self, place: Place) {
        self.place.store(place as u8, Ordering::Relaxed);
    }
}

/// The processing setup of a copy at one sample rate: 32-bit audio in
/// blocks of [`MAX_BLOCK`] frames at most, in real time.
fn processing(sample_rate: u32) -> ProcessSetup {
    ProcessSetup {
        processMode: ProcessModes_::kRealtime as i32,
        symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
        maxSamplesPerBlock: MAX_BLOCK as i32,
        sampleRate: f64::from(sample_rate),
    }
}

/// The frames of the silent blocks a copy runs after it turns on: 0.1 s.
/// See [`Loaded::activate`].
fn warm_frames(sample_rate: u32) -> u64 {
    u64::from(sample_rate) / 10
}

/// A plugin with its two halves connected. The plugin thread owns a
/// `Loaded` value from the first call to the last. The value is one field
/// so that its drop can move the whole value to the main thread, which
/// releases it.
pub(crate) struct Loaded(ManuallyDrop<Whole>);

/// The fields of [`Loaded`].
pub(crate) struct Whole {
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
    running: Option<Arc<AtomicUsize>>,
    /// The cell of the copy, shared with the handler.
    state: Arc<CopyState>,
    _handler: ComWrapper<Handler>,
    _host: ComWrapper<HostApp>,
    _module: Arc<Module>,
}

// SAFETY: a `Whole` value moves between threads only inside an `Instance`,
// and the plugin thread makes every call except the audio processing. On
// macOS the value is created and released on the process's main thread
// while the plugin thread waits for each step.
unsafe impl Send for Whole {}

impl Deref for Loaded {
    type Target = Whole;

    fn deref(&self) -> &Whole {
        &self.0
    }
}

impl DerefMut for Loaded {
    fn deref_mut(&mut self) -> &mut Whole {
        &mut self.0
    }
}

impl Loaded {
    /// Creates the plugin: the factory call and the calls up to the
    /// plugin's start state. On macOS the creation runs on the process's
    /// main thread, and the plugin thread waits for it: a plugin may
    /// create objects that AppKit allows only on that thread.
    pub(crate) fn new(module: Arc<Module>, class: &ClassInfo) -> Result<Self, String> {
        let class = class.clone();
        crate::main_thread::run(move || Self::make(module, &class))
    }

    fn make(module: Arc<Module>, class: &ClassInfo) -> Result<Self, String> {
        let host = ComWrapper::new(HostApp);
        let cell = Arc::new(CopyState::default());
        let handler = Handler::new(Arc::clone(&cell));
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
        let loaded = Self(ManuallyDrop::new(Whole {
            connection,
            controller,
            processor,
            component,
            separate_controller,
            active: Cell::new(false),
            running: None,
            state: cell,
            _handler: handler,
            _host: host,
            _module: module,
        }));
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
        // The parameter the plugin names as its dry/wet mix, when it names
        // one to a host.
        let mix = controller
            .cast::<IParameterFunctionName>()
            .and_then(|named| {
                let mut id: ParamID = 0;
                let found = unsafe {
                    named.getParameterIDFromFunctionName(
                        kRootUnitId,
                        FunctionNameType::kDryWetMix,
                        &mut id,
                    )
                };
                (found == kResultOk).then_some(id)
            });
        let mut params: Vec<ParamInfo> = Vec::new();
        let mut keys = std::collections::HashSet::new();
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
            if key.is_empty() || key == "preset" || keys.contains(&key) {
                key = info.id.to_string();
            }
            keys.insert(key.clone());
            let mut text: String128 = [0; 128];
            // A plugin sometimes gives a start value that is no number. Such
            // a value has no form in a score.
            let default = match info.defaultNormalizedValue {
                value if value.is_finite() => value.clamp(0.0, 1.0),
                _ => 0.0,
            };
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
                role: if info.flags & ParameterInfo_::ParameterFlags_::kIsBypass != 0 {
                    ParamRole::Bypass
                } else if info.flags & ParameterInfo_::ParameterFlags_::kIsProgramChange != 0 {
                    ParamRole::Program
                } else if Some(info.id) == mix {
                    ParamRole::Mix
                } else {
                    ParamRole::Plain
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
        running: Arc<AtomicUsize>,
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
        let mut setup = processing(sample_rate);
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
        // The plugin turned on after each restart request it made so far.
        self.state.latency_changed.store(false, Ordering::Relaxed);
        let at_start = self.param_values(params);
        self.active.set(true);
        unsafe { self.processor.setProcessing(1) };
        running.fetch_add(1, Ordering::Relaxed);
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
        let state = Arc::clone(&self.state);
        let mut instance = Instance {
            key,
            plugin: Some(self),
            any_thread: true,
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
            played: Played::default(),
            state,
            retire,
        };
        // Silent blocks before the audio starts: 0.1 seconds. Some plugins
        // do work at their first blocks that they do one time only: lazy
        // setup, memory allocation, license and disk reads. Some
        // instruments also drop a note that comes in their first blocks. A
        // DAW runs a plugin all the time, so a plugin does not count on a
        // first note in its first block. This runs on the plugin thread,
        // where such work costs no audio deadline.
        let warm_up = |instance: &mut Instance| {
            let (mut left, mut right) = ([0.0f32; MAX_BLOCK], [0.0f32; MAX_BLOCK]);
            for _ in 0..warm_frames(sample_rate).div_ceil(MAX_BLOCK as u64) {
                left.fill(0.0);
                right.fill(0.0);
                instance.process(&mut left, &mut right);
            }
        };
        warm_up(&mut instance);
        // A plugin that finds its latency in its first blocks asks for a
        // restart there. The build restarts it one time at most, with the
        // silent blocks again. A request after the plugin turns on again
        // leaves the flag set.
        if let Some(plugin) = &instance.plugin
            && plugin.state.latency_changed.swap(false, Ordering::Acquire)
        {
            if let Err(error) = plugin.restart(None) {
                // The copy ends here, in its build, so the count of running
                // copies drops before the host stores the failure.
                drop(instance.plugin.take());
                return Err(error);
            }
            warm_up(&mut instance);
        }
        // The build reads the latency and the tail one time, after the
        // first blocks. The clock and the counts of the flush start from
        // here, so the first blocks count no frame.
        instance.read_latency_and_tail();
        instance.rebase();
        Ok(instance)
    }

    /// Turns the plugin off and on, as a plugin asks for when its latency
    /// changes. With a sample rate, the plugin takes the rate while it is
    /// off, with the setup of its build. Runs on the plugin thread while no
    /// block runs.
    fn restart(&self, sample_rate: Option<u32>) -> Result<(), String> {
        // SAFETY: the calls take no pointer, and the plugin thread makes
        // them while no block runs.
        unsafe {
            self.processor.setProcessing(0);
            self.component.setActive(0);
        }
        self.active.set(false);
        if let Some(sample_rate) = sample_rate {
            let mut setup = processing(sample_rate);
            // SAFETY: the setup lives for the call, and the plugin is off.
            if unsafe { self.processor.setupProcessing(&mut setup) } != kResultOk {
                return Err(format!("the plugin refused {sample_rate} Hz"));
            }
        }
        // SAFETY: the call takes no pointer, and the plugin thread makes
        // it while no block runs.
        if unsafe { self.component.setActive(1) } != kResultOk {
            return Err("the plugin did not turn on".into());
        }
        // The plugin turned on after each restart request it made so far.
        self.state.latency_changed.store(false, Ordering::Relaxed);
        self.active.set(true);
        // SAFETY: as above.
        unsafe { self.processor.setProcessing(1) };
        Ok(())
    }
}

impl Drop for Loaded {
    fn drop(&mut self) {
        // The plugin ends on the process's main thread on macOS: the end
        // calls and the releases after them may destroy objects that
        // belong on that thread, so the whole value moves there and every
        // field of it drops there.
        //
        // SAFETY: the value moves out of the one field, which is a
        // `ManuallyDrop`, so no part of it drops a second time here.
        let whole = unsafe { std::ptr::read(&*self.0) };
        crate::main_thread::run(move || whole.end());
    }
}

impl Whole {
    /// The plugin's end calls, in the order the VST3 call sequence asks.
    /// The fields drop where this runs: the final release of a plugin
    /// object can destroy objects that belong on this thread.
    fn end(self) {
        if let Some(running) = &self.running {
            running.fetch_sub(1, Ordering::Relaxed);
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

/// A plugin on an orbit. The audio engine owns the value, and runs its
/// blocks in the callback or on a plugin worker, one thread at a time.
pub(crate) struct Instance {
    key: InsertKey,
    /// Goes back to the plugin thread at the end.
    plugin: Option<Loaded>,
    /// False keeps the blocks of the copy in the audio callback: see
    /// [`Instance::set_any_thread`].
    any_thread: bool,
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
    /// The counts of the flush of the copy: see [`Self::flush_tail`].
    played: Played,
    /// The cell of the copy: see [`CopyState`].
    state: Arc<CopyState>,
    retire: Sender<Job>,
}

// SAFETY: one thread owns an instance at a time and makes each call on it:
// the plugin thread at the build, then the audio engine, which runs the
// blocks in the callback or on a plugin worker, one thread at a time. The
// drop of a `Lent` sends a live copy back to the plugin thread in a job of
// the line. The plugin thread plays out its tail or sets it up at a new
// rate while no engine holds it. The copy then waits for its slot under
// the lock of the host, and the next engine takes it under that lock. The
// pointers go to memory the instance owns, and the plugin objects go back
// to the plugin thread at the end.
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

    /// Marks the instance as held by an engine, until the engine gives it
    /// back or it ends. The host reads the mark in the cell it gets.
    pub(crate) fn hold(&self) -> Arc<CopyState> {
        self.state.set_place(Place::Engine);
        Arc::clone(&self.state)
    }

    /// The cell of the copy.
    pub(crate) fn cell(&self) -> &CopyState {
        &self.state
    }

    /// The latency of the plugin in frames, as the plugin thread last read
    /// it.
    pub(crate) fn latency(&self) -> u32 {
        self.state.latency.load(Ordering::Relaxed)
    }

    /// Reads the latency and the tail of the plugin into the cell. Runs on
    /// the plugin thread while no block runs.
    fn read_latency_and_tail(&self) {
        let Some(plugin) = &self.plugin else {
            return;
        };
        // SAFETY: the calls take no pointer, and the plugin thread makes
        // them while no block runs.
        let (latency, tail) = unsafe {
            (
                plugin.processor.getLatencySamples(),
                plugin.processor.getTailSamples(),
            )
        };
        plugin.state.latency.store(latency, Ordering::Relaxed);
        // A tail with no end, kInfiniteTail, counts as the longest tail.
        let longest = (self.sample_rate * LONGEST_TAIL) as u32;
        plugin
            .state
            .tail
            .store(tail.min(longest), Ordering::Relaxed);
    }

    /// The quiet time of the copy in frames: its tail, 0.25 s at least and
    /// 3 s at most. A copy quiet this long has no sound left to give.
    fn quiet_time(&self) -> u64 {
        let least = (self.sample_rate * f64::from(INSERT_DRAIN_QUIET_SECONDS)) as u64;
        u64::from(self.tail_frames()).max(least)
    }

    /// True when the copy has no tail to play out: it ran no frame since its
    /// last flush, or it is quiet for its quiet time and holds no note and
    /// no value. Makes no plugin call.
    pub(crate) fn skips_flush(&self) -> bool {
        self.played.skips(self.quiet_time(), self.busy())
    }

    /// Plays out the tail of a copy an engine gave back, on the plugin
    /// thread, so the next note of the copy has no sound of its life before.
    /// Each note ends first. A copy that skips ([`Self::skips_flush`]) runs
    /// no block, and restarts only when the plugin asked for a restart. Each
    /// other copy restarts and runs silent blocks: see [`play_out`]. The
    /// blocks stop when `closed` turns true. Then the plugin thread reads
    /// the latency and the tail, and the clock of the copy starts again:
    /// see [`Self::rebase`]. `Err` has the reason the plugin did not turn
    /// on again.
    pub(crate) fn flush_tail(&mut self, closed: &AtomicBool) -> Result<(), String> {
        self.reset();
        let flushed = if !self.skips_flush() {
            let quiet_time = self.quiet_time();
            let latency = u64::from(self.latency());
            let rate = self.sample_rate as u32;
            let closed = || closed.load(Ordering::Relaxed);
            play_out(self, quiet_time, latency, rate, &closed)
        } else if self.state.latency_changed.load(Ordering::Acquire) {
            Flushed::restart(self)
        } else {
            Ok(())
        };
        self.read_latency_and_tail();
        self.rebase();
        flushed
    }

    /// Sets the copy up again at `sample_rate`, on the plugin thread, with
    /// no new build and no preset read. Each note ends first, and the plugin
    /// takes the rate while it is off. Then silent blocks run as at the
    /// build, the plugin thread reads the latency and the tail again, and
    /// the clock of the copy starts again: see [`Self::rebase`]. `Err` has
    /// the reason the plugin did not take the rate.
    pub(crate) fn retune(&mut self, sample_rate: u32) -> Result<(), String> {
        self.reset();
        if let Some(plugin) = &self.plugin {
            plugin.restart(Some(sample_rate))?;
        }
        self.sample_rate = f64::from(sample_rate);
        for _ in 0..warm_frames(sample_rate).div_ceil(MAX_BLOCK as u64) {
            self.silent_block();
        }
        self.read_latency_and_tail();
        self.rebase();
        Ok(())
    }

    /// Starts the clock of the copy again from frame 0, after its build, a
    /// flush or a retune, and the counts of the flush with it. Each value a
    /// note gave the copy before then is a value of the life before: the
    /// next note of the copy restores it. Makes no plugin call.
    pub(crate) fn rebase(&mut self) {
        self.frames = 0;
        self.beats = 0.0;
        for (_, set_at) in &mut self.changed[..self.changed_count] {
            *set_at = -1;
        }
        self.played = Played::default();
    }

    pub(crate) fn set_order(&mut self, order: u64) {
        self.order = order;
    }

    /// With false, the engine runs each block of the copy in the audio
    /// callback, for a plugin that breaks on another thread. A new copy
    /// runs its blocks on any thread.
    pub(crate) fn set_any_thread(&mut self, any_thread: bool) {
        self.any_thread = any_thread;
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

    fn set_param_now(&mut self, param: InsertParam) {
        if !param.value.is_finite() {
            return;
        }
        let value = f64::from(param.value.clamp(0.0, 1.0));
        let id = Some(param.id);
        // A value that waits is the value of a note in the queue.
        let waiting = &mut self.params[..self.param_count];
        for known in waiting.iter_mut().filter(|known| known.id == id) {
            known.value = value;
        }
        self.set_at(id, self.frames, value);
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

    fn any_thread(&self) -> bool {
        self.any_thread
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
        self.run(left, right, true);
    }

    fn tail_frames(&self) -> u32 {
        self.state.tail.load(Ordering::Relaxed)
    }

    /// One block of the copy while the score does not play: the plugin gets
    /// the block with its playing flag off, and the musical position stands.
    /// The block counts for the flush as a block of
    /// [`OrbitInsert::process`] does.
    fn drain(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.run(left, right, false);
    }
}

impl Instance {
    /// One block of the copy: with `playing`, a block of the score, and
    /// with no `playing`, a block of [`OrbitInsert::drain`]. Each block
    /// counts for the flush, with no allocation: see [`Played`].
    fn run(&mut self, left: &mut [f32], right: &mut [f32], playing: bool) {
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
        let mut state = ProcessContext_::StatesAndFlags_::kTempoValid
            | ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid
            | ProcessContext_::StatesAndFlags_::kBarPositionValid
            | ProcessContext_::StatesAndFlags_::kTimeSigValid
            | ProcessContext_::StatesAndFlags_::kContTimeValid;
        if playing {
            state |= ProcessContext_::StatesAndFlags_::kPlaying;
        }
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
        if playing {
            self.frames += frames as i64;
            self.beats += frames as f64 * self.tempo / 60.0 / self.sample_rate;
        }
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
        let output = left[..frames].iter().chain(&right[..frames]);
        let peak = output.fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        let busy = self.busy();
        self.played.add(frames, peak, busy);
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // No engine holds the copy from here on. The mark clears first: a
        // copy that ended on the plugin thread has no plugin left.
        self.state.set_place(Place::Home);
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

/// The counts of a copy for its flush: the frames it played since its last
/// flush, and its quiet frames in a row. A block is quiet when its peak is
/// at [`QUIET_PEAK`] or below and the copy holds no note and no value at
/// its end: see [`OrbitInsert::busy`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Played {
    frames: u64,
    quiet: u64,
}

impl Played {
    /// Counts one block of `frames` frames with the peak `peak`.
    fn add(&mut self, frames: usize, peak: f32, busy: bool) {
        let frames = frames as u64;
        self.frames += frames;
        self.quiet = if peak <= QUIET_PEAK && !busy {
            self.quiet + frames
        } else {
            0
        };
    }

    /// True when a flush has no tail to play out: the copy ran no frame,
    /// or it was quiet for `quiet_time` frames and is not `busy`.
    fn skips(self, quiet_time: u64, busy: bool) -> bool {
        self.frames == 0 || (self.quiet >= quiet_time && !busy)
    }
}

/// A copy a flush plays out with silence: an [`Instance`], or a copy of a
/// test with a peak for each block.
trait Flushed {
    /// Runs one silent block of [`MAX_BLOCK`] frames with no clock, and
    /// gives the counts after it.
    fn silent_block(&mut self) -> Played;

    /// Turns the plugin off and on. `Err` has the reason the plugin did not
    /// turn on.
    fn restart(&mut self) -> Result<(), String>;
}

impl Flushed for Instance {
    fn silent_block(&mut self) -> Played {
        let (mut left, mut right) = ([0.0f32; MAX_BLOCK], [0.0f32; MAX_BLOCK]);
        self.drain(&mut left, &mut right);
        self.played
    }

    fn restart(&mut self) -> Result<(), String> {
        self.plugin
            .as_ref()
            .map_or(Ok(()), |plugin| plugin.restart(None))
    }
}

/// The flush of a copy with a tail: see [`Instance::flush_tail`]. The copy
/// restarts, then runs the quiet check: silence for `latency` frames, 3 s at
/// most, and [`CHECK_BLOCKS`] blocks more. A copy quiet at the end of the
/// check is done. A copy still loud runs the long flush: silence until it is
/// quiet for `quiet_time` frames, 3 s at most, then a restart and 0.1 s of
/// silence, as at its build. The flush stops between 2 blocks when `closed`
/// says so.
fn play_out(
    copy: &mut impl Flushed,
    quiet_time: u64,
    latency: u64,
    rate: u32,
    closed: &dyn Fn() -> bool,
) -> Result<(), String> {
    let mut played = Played::default();
    let longest = (f64::from(rate) * LONGEST_TAIL) as u64;
    // A latency longer than the longest tail counts as the longest tail. So
    // the check of a plugin that reports a false latency runs 3 s at most.
    let check = latency.min(longest) + (CHECK_BLOCKS * MAX_BLOCK) as u64;
    copy.restart()?;
    if !silence(copy, &mut played, closed, |ran, _| ran < check) || played.quiet > 0 {
        return Ok(());
    }
    let loud = |ran, played: Played| played.quiet < quiet_time && ran < longest;
    if !silence(copy, &mut played, closed, loud) {
        return Ok(());
    }
    copy.restart()?;
    let warm = warm_frames(rate);
    silence(copy, &mut played, closed, |ran, _| ran < warm);
    Ok(())
}

/// Runs silent blocks of a copy while `more` says so before each block:
/// `more` gets the frames of the run so far and the counts of the copy.
/// `played` keeps the counts after the last block. False when `closed`
/// stopped the run between 2 blocks.
fn silence(
    copy: &mut impl Flushed,
    played: &mut Played,
    closed: &dyn Fn() -> bool,
    more: impl Fn(u64, Played) -> bool,
) -> bool {
    let mut ran = 0;
    while more(ran, *played) {
        if closed() {
            return false;
        }
        *played = copy.silent_block();
        ran += MAX_BLOCK as u64;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A copy of a test: the peak of each silent block by the number of
    /// the block, the counts of the flush, and the blocks and the restarts
    /// the flush ran.
    struct Peaks {
        peak: fn(usize) -> f32,
        played: Played,
        blocks: usize,
        restarts: usize,
    }

    impl Flushed for Peaks {
        fn silent_block(&mut self) -> Played {
            self.played.add(MAX_BLOCK, (self.peak)(self.blocks), false);
            self.blocks += 1;
            self.played
        }

        fn restart(&mut self) -> Result<(), String> {
            self.restarts += 1;
            Ok(())
        }
    }

    /// A copy that ran no frame skips the flush, and so does a quiet copy.
    /// A copy quiet for less than its tail, or one that decays, takes a
    /// restart and the quiet check. A latency past 3 s counts as 3 s in the
    /// check. A copy that stays loud runs the long flush, 3 s at most, and a
    /// closed host stops a flush.
    #[test]
    fn a_flush_plays_out_the_tail_of_a_loud_copy_only() {
        const RATE: u32 = 48_000;
        let quarter = u64::from(RATE) / 4;
        let blocks = |frames: u64| frames.div_ceil(MAX_BLOCK as u64) as usize;
        // The build leaves its counts at 0 after its warm-up: see
        // `Loaded::activate`.
        assert!(Played::default().skips(quarter, false));
        let mut quiet = Played::default();
        for _ in 0..blocks(quarter) {
            quiet.add(MAX_BLOCK, QUIET_PEAK, false);
        }
        assert!(quiet.skips(quarter, false));
        assert!(!quiet.skips(quarter, true), "a copy that holds a note");
        let mut held = quiet;
        held.add(MAX_BLOCK, 0.0, true);
        assert_eq!(held.quiet, 0, "a block that ends with a note");

        // The blocks and the restarts of a flush of a copy with no latency.
        let flush = |peak: fn(usize) -> f32, quiet_time: u64, closed: &dyn Fn() -> bool| {
            let mut copy = Peaks {
                peak,
                played: Played::default(),
                blocks: 0,
                restarts: 0,
            };
            play_out(&mut copy, quiet_time, 0, RATE, closed).expect("the flush");
            (copy.blocks, copy.restarts)
        };
        let open = || false;
        // A copy quiet 0.25 s inside a tail of 0.5 s takes the quick path: a
        // restart and the quiet check. So does a copy that decays in the
        // quiet check.
        assert!(!quiet.skips(2 * quarter, false));
        assert_eq!(flush(|_| 0.0, 2 * quarter, &open), (CHECK_BLOCKS, 1));
        let decays = |block| [0.1, 0.01, 0.001].get(block).copied().unwrap_or(0.0);
        assert_eq!(flush(decays, quarter, &open), (CHECK_BLOCKS, 1));
        // A copy loud at the end of the check: silence until the copy is
        // quiet for its quiet time, 3 s at most, then a restart and 0.1 s.
        let warm = blocks(warm_frames(RATE));
        let late = |block| if block < 10 { 0.5 } else { 0.0 };
        assert_eq!(
            flush(late, quarter, &open),
            (10 + blocks(quarter) + warm, 2)
        );
        let longest = blocks(3 * u64::from(RATE));
        let loud = flush(|_| 0.5, quarter, &open);
        assert_eq!(loud, (CHECK_BLOCKS + longest + warm, 2));
        // A latency past 3 s counts as 3 s in the quiet check.
        let mut copy = Peaks {
            peak: |_| 0.0,
            played: Played::default(),
            blocks: 0,
            restarts: 0,
        };
        play_out(&mut copy, quarter, u64::from(u32::MAX), RATE, &open).expect("the flush");
        assert_eq!((copy.blocks, copy.restarts), (longest + CHECK_BLOCKS, 1));
        let asked = std::cell::Cell::new(0);
        let closes = || {
            asked.set(asked.get() + 1);
            asked.get() > 20
        };
        assert_eq!(flush(|_| 0.5, quarter, &closes), (20, 1));
    }
}
