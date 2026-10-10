//! Two small VST3 plugins in one bundle, for the plugin host tests.
//!
//! The first is an effect: a gain with a beat gate. The effect has a
//! processor class and a controller class, as most plugins do. The state is
//! 16 bytes: the gain and the gate as two little-endian `f64` values.
//!
//! The second is an instrument: one sine for each note, with no input and
//! no parameters.

// The numbers of a VST3 enum are `u32` on Unix and `i32` on Windows, so a
// cast that does nothing on one system is necessary on the other.
#![allow(clippy::unnecessary_cast)]

use std::cell::{Cell, RefCell};
use std::ffi::{CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{ptr, slice};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComRef, ComWrapper, uid};

/// The name of the effect, and the name of the bundle.
pub const NAME: &str = "Rustel Fixture";
/// The name of the instrument. A note gives a sine at the pitch of the
/// note, with a peak of [`TONE_LEVEL`] at velocity 1, for the length of the
/// note.
pub const TONE_NAME: &str = "Rustel Fixture Tone";
pub const TONE_LEVEL: f32 = 0.25;
/// Parameter "Gain": the output level, 1 at the start.
pub const PARAM_GAIN: u32 = 100;
/// Parameter "Beat Gate": at 0.5 and above, the second half of each beat
/// is silent.
pub const PARAM_GATE: u32 = 101;
/// Parameter "Bypass": at 0.5 and above, the plugin writes no output, as a
/// plugin that counts on one block for input and output.
pub const PARAM_BYPASS: u32 = 102;

const PROCESSOR: TUID = uid(0x52757374, 0x656C4669, 0x78747572, 0x65507263);
const CONTROLLER: TUID = uid(0x52757374, 0x656C4669, 0x78747572, 0x6543746C);
const TONE: TUID = uid(0x52757374, 0x656C4669, 0x78747572, 0x65546F6E);
/// The processor class id as a `.vstpreset` file writes it.
const PROCESSOR_TEXT: &str = "52757374656C46697874757265507263";

/// Puts the plugin in a VST3 bundle under `folder` and returns the bundle
/// path. The plugin library sits next to the test program.
pub fn install(folder: &Path) -> PathBuf {
    let library = std::env::current_exe()
        .expect("test program path")
        .parent()
        .expect("test program folder")
        .join(format!(
            "{}rustel_vst3_fixture{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ));
    let bundle = folder.join(format!("{NAME}.vst3"));
    let contents = bundle.join("Contents");
    let binary = if cfg!(target_os = "macos") {
        std::fs::create_dir_all(contents.join("MacOS")).expect("bundle folder");
        std::fs::write(
            contents.join("Info.plist"),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
                 <key>CFBundleExecutable</key><string>{NAME}</string>\
                 <key>CFBundleIdentifier</key><string>rustel.fixture</string>\
                 <key>CFBundlePackageType</key><string>BNDL</string></dict></plist>\n"
            ),
        )
        .expect("bundle property list");
        contents.join("MacOS").join(NAME)
    } else if cfg!(windows) {
        // The VST3 name for 64-bit ARM on Windows is `arm64`.
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            arch => arch,
        };
        let arch = contents.join(format!("{arch}-win"));
        std::fs::create_dir_all(&arch).expect("bundle folder");
        arch.join(format!("{NAME}.vst3"))
    } else {
        let arch = contents.join(format!("{}-linux", std::env::consts::ARCH));
        std::fs::create_dir_all(&arch).expect("bundle folder");
        arch.join(format!("{NAME}.so"))
    };
    std::fs::copy(&library, &binary)
        .unwrap_or_else(|error| panic!("copy {}: {error}", library.display()));
    bundle
}

/// The bytes of a `.vstpreset` file that sets the gain and the gate.
pub fn preset(gain: f64, gate: f64) -> Vec<u8> {
    let mut state = gain.to_le_bytes().to_vec();
    state.extend(gate.to_le_bytes());
    let mut file = b"VST3".to_vec();
    file.extend(1i32.to_le_bytes());
    file.extend(PROCESSOR_TEXT.as_bytes());
    let data_offset = file.len() as i64 + 8;
    file.extend((data_offset + state.len() as i64).to_le_bytes());
    file.extend(&state);
    file.extend(b"List");
    file.extend(1i32.to_le_bytes());
    file.extend(b"Comp");
    file.extend(data_offset.to_le_bytes());
    file.extend((state.len() as i64).to_le_bytes());
    file
}

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let text = CString::new(src).unwrap_or_default();
    for (src, dst) in text.as_bytes_with_nul().iter().zip(dst.iter_mut()) {
        *dst = *src as c_char;
    }
    if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (src, dst) in src.encode_utf16().zip(dst.iter_mut()) {
        *dst = src as TChar;
        len += 1;
    }
    let end = len.min(dst.len() - 1);
    dst[end] = 0;
}

/// The gain and the gate, shared by the two classes' own copies.
struct Values {
    gain: AtomicU64,
    gate: AtomicU64,
    bypass: AtomicU64,
}

impl Values {
    fn new() -> Self {
        Self {
            gain: AtomicU64::new(1.0f64.to_bits()),
            gate: AtomicU64::new(0.0f64.to_bits()),
            bypass: AtomicU64::new(0.0f64.to_bits()),
        }
    }

    fn slot(&self, id: u32) -> Option<&AtomicU64> {
        match id {
            PARAM_GAIN => Some(&self.gain),
            PARAM_GATE => Some(&self.gate),
            PARAM_BYPASS => Some(&self.bypass),
            _ => None,
        }
    }

    fn get(&self, id: u32) -> f64 {
        self.slot(id)
            .map_or(0.0, |slot| f64::from_bits(slot.load(Ordering::Relaxed)))
    }

    fn set(&self, id: u32, value: f64) -> bool {
        match self.slot(id) {
            Some(slot) => {
                slot.store(value.to_bits(), Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// Reads the two values from a state stream.
    unsafe fn read(&self, stream: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(stream) }) else {
            return kInvalidArgument;
        };
        let mut bytes = [0u8; 16];
        let mut count = 0;
        let result = unsafe { stream.read(bytes.as_mut_ptr().cast(), 16, &mut count) };
        if result != kResultOk || count != 16 {
            return kResultFalse;
        }
        let value = |at: usize| f64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        self.set(PARAM_GAIN, value(0));
        self.set(PARAM_GATE, value(8));
        kResultOk
    }

    unsafe fn write(&self, stream: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(stream) }) else {
            return kInvalidArgument;
        };
        let mut bytes = self.get(PARAM_GAIN).to_le_bytes().to_vec();
        bytes.extend(self.get(PARAM_GATE).to_le_bytes());
        let mut count = 0;
        unsafe { stream.write(bytes.as_mut_ptr().cast(), 16, &mut count) }
    }
}

struct Processor {
    values: Values,
    /// Read one time, with the plugin: the audio call reads no variable.
    fault: Option<Option<std::path::PathBuf>>,
}

impl Class for Processor {
    type Interfaces = (IComponent, IAudioProcessor);
}

impl IPluginBaseTrait for Processor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

fn is_audio(media: MediaType) -> bool {
    media == MediaTypes_::kAudio as MediaType
}

impl IComponentTrait for Processor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        unsafe { *class_id = CONTROLLER };
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media: MediaType, _dir: BusDirection) -> i32 {
        i32::from(is_audio(media))
    }

    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if !is_audio(media) || index != 0 {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = media;
        bus.direction = dir;
        bus.channelCount = 2;
        copy_wstring("Stereo", &mut bus.name);
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        kResultOk
    }

    unsafe fn getRoutingInfo(
        &self,
        _input: *mut RoutingInfo,
        _output: *mut RoutingInfo,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        _media: MediaType,
        _dir: BusDirection,
        _index: i32,
        _state: TBool,
    ) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        unsafe { self.values.read(state) }
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        unsafe { self.values.write(state) }
    }
}

impl IAudioProcessorTrait for Processor {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        input_count: i32,
        outputs: *mut SpeakerArrangement,
        output_count: i32,
    ) -> tresult {
        let stereo = input_count == 1
            && output_count == 1
            && unsafe { *inputs == SpeakerArr::kStereo && *outputs == SpeakerArr::kStereo };
        if stereo { kResultTrue } else { kResultFalse }
    }

    unsafe fn getBusArrangement(
        &self,
        _dir: BusDirection,
        index: i32,
        arrangement: *mut SpeakerArrangement,
    ) -> tresult {
        if index != 0 {
            return kInvalidArgument;
        }
        unsafe { *arrangement = SpeakerArr::kStereo };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }

    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        if let Some(once) = &self.fault {
            let happened =
                |file: &std::path::PathBuf| file.exists() || std::fs::write(file, "").is_err();
            if !once.as_ref().is_some_and(happened) {
                std::process::abort();
            }
        }
        let data = unsafe { &*data };
        // The gain takes each value at its own frame. The gate takes its
        // last value for the full block.
        let mut gain = self.values.get(PARAM_GAIN) as f32;
        let mut gain_points = [(0usize, 0.0f32); 8];
        let mut gain_count = 0;
        if let Some(changes) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            for index in 0..unsafe { changes.getParameterCount() } {
                let Some(queue) = (unsafe { ComRef::from_raw(changes.getParameterData(index)) })
                else {
                    continue;
                };
                let id = unsafe { queue.getParameterId() };
                for point in 0..unsafe { queue.getPointCount() } {
                    let (mut offset, mut value) = (0, 0.0);
                    if unsafe { queue.getPoint(point, &mut offset, &mut value) } != kResultOk {
                        continue;
                    }
                    self.values.set(id, value);
                    if id == PARAM_GAIN && gain_count < gain_points.len() {
                        gain_points[gain_count] = (offset.max(0) as usize, value as f32);
                        gain_count += 1;
                    }
                }
            }
        }
        if data.numInputs != 1 || data.numOutputs != 1 || data.numSamples <= 0 {
            return kResultOk;
        }
        if self.values.get(PARAM_BYPASS) >= 0.5 {
            return kResultOk;
        }
        let frames = data.numSamples as usize;
        let (input, output) = unsafe { (&*data.inputs, &*data.outputs) };
        if input.numChannels != 2 || output.numChannels != 2 {
            return kResultOk;
        }
        // The beat position of the first frame, and the beats per frame.
        let mut clock = None;
        if self.values.get(PARAM_GATE) >= 0.5 && !data.processContext.is_null() {
            let context = unsafe { &*data.processContext };
            let needed = (ProcessContext_::StatesAndFlags_::kTempoValid
                | ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid)
                as u32;
            if context.state & needed == needed {
                clock = Some((
                    context.projectTimeMusic,
                    context.tempo / 60.0 / context.sampleRate,
                ));
            }
        }
        let channel = |buses: &AudioBusBuffers, channel: usize| unsafe {
            *buses.__field0.channelBuffers32.add(channel)
        };
        let mut next_point = 0;
        for frame in 0..frames {
            while next_point < gain_count && gain_points[next_point].0 <= frame {
                gain = gain_points[next_point].1;
                next_point += 1;
            }
            let open = clock
                .is_none_or(|(start, step)| (start + frame as f64 * step).rem_euclid(1.0) < 0.5);
            let level = if open { gain } else { 0.0 };
            for side in 0..2 {
                unsafe {
                    *channel(output, side).add(frame) = *channel(input, side).add(frame) * level;
                }
            }
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

struct Controller {
    values: Values,
}

impl Class for Controller {
    type Interfaces = (IEditController,);
}

impl IPluginBaseTrait for Controller {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IEditControllerTrait for Controller {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        unsafe { self.values.read(state) }
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> i32 {
        3
    }

    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        let (id, title, default) = match index {
            0 => (PARAM_GAIN, "Gain", 1.0),
            1 => (PARAM_GATE, "Beat Gate", 0.0),
            2 => (PARAM_BYPASS, "Bypass", 0.0),
            _ => return kInvalidArgument,
        };
        let info = unsafe { &mut *info };
        info.id = id;
        copy_wstring(title, &mut info.title);
        copy_wstring(title, &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.stepCount = i32::from(id != PARAM_GAIN);
        info.defaultNormalizedValue = default;
        info.unitId = 0;
        info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate;
        if id == PARAM_BYPASS {
            info.flags |= ParameterInfo_::ParameterFlags_::kIsBypass;
        }
        kResultOk
    }

    unsafe fn getParamStringByValue(&self, id: u32, value: f64, string: *mut String128) -> tresult {
        // The bypass prints words, as the switch of a real plugin does.
        let text = match (id, value >= 0.5) {
            (PARAM_BYPASS, true) => "on".to_owned(),
            (PARAM_BYPASS, false) => "off".to_owned(),
            _ => format!("{value:.2}"),
        };
        copy_wstring(&text, unsafe { &mut *string });
        kResultOk
    }

    unsafe fn getParamValueByString(
        &self,
        _id: u32,
        _string: *mut TChar,
        _value: *mut f64,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn normalizedParamToPlain(&self, _id: u32, value: f64) -> f64 {
        value
    }

    unsafe fn plainParamToNormalized(&self, _id: u32, value: f64) -> f64 {
        value
    }

    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        self.values.get(id)
    }

    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        if self.values.set(id, value) {
            kResultOk
        } else {
            kInvalidArgument
        }
    }

    unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
        kResultOk
    }

    unsafe fn createView(&self, _name: *const c_char) -> *mut IPlugView {
        ptr::null_mut()
    }
}

/// One sounding note of the instrument.
struct ToneVoice {
    key: i16,
    /// Radians for each frame.
    step: f64,
    phase: f64,
    level: f32,
}

/// The instrument: an event input, a stereo output, and one sine for each
/// note.
struct Tone {
    sample_rate: Cell<f64>,
    voices: RefCell<Vec<ToneVoice>>,
}

impl Tone {
    fn new() -> Self {
        Self {
            sample_rate: Cell::new(48_000.0),
            // Room for each note with no allocation in the audio call.
            voices: RefCell::new(Vec::with_capacity(64)),
        }
    }

    fn apply(&self, event: &Event) {
        let mut voices = self.voices.borrow_mut();
        if event.r#type == Event_::EventTypes_::kNoteOnEvent as u16 {
            let note = unsafe { event.__field0.noteOn };
            let pitch = f64::from(note.pitch) + f64::from(note.tuning) / 100.0;
            let hertz = 440.0 * ((pitch - 69.0) / 12.0).exp2();
            if voices.len() < voices.capacity() {
                voices.push(ToneVoice {
                    key: note.pitch,
                    step: std::f64::consts::TAU * hertz / self.sample_rate.get(),
                    phase: 0.0,
                    level: note.velocity * TONE_LEVEL,
                });
            }
        } else if event.r#type == Event_::EventTypes_::kNoteOffEvent as u16 {
            // As a synth does: the end of a key ends each note on the key.
            let key = unsafe { event.__field0.noteOff }.pitch;
            voices.retain(|voice| voice.key != key);
        }
    }
}

impl Class for Tone {
    type Interfaces = (IComponent, IAudioProcessor);
}

impl IPluginBaseTrait for Tone {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for Tone {
    unsafe fn getControllerClassId(&self, _class_id: *mut TUID) -> tresult {
        kNotImplemented
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media: MediaType, dir: BusDirection) -> i32 {
        // One event input and one audio output.
        i32::from(is_audio(media) != (dir == BusDirections_::kInput as BusDirection))
    }

    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if index != 0 || unsafe { self.getBusCount(media, dir) } == 0 {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = media;
        bus.direction = dir;
        bus.channelCount = if is_audio(media) { 2 } else { 1 };
        copy_wstring("Main", &mut bus.name);
        bus.busType = BusTypes_::kMain as BusType;
        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        kResultOk
    }

    unsafe fn getRoutingInfo(
        &self,
        _input: *mut RoutingInfo,
        _output: *mut RoutingInfo,
    ) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        _media: MediaType,
        _dir: BusDirection,
        _index: i32,
        _state: TBool,
    ) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }
}

impl IAudioProcessorTrait for Tone {
    unsafe fn setBusArrangements(
        &self,
        _inputs: *mut SpeakerArrangement,
        input_count: i32,
        outputs: *mut SpeakerArrangement,
        output_count: i32,
    ) -> tresult {
        let stereo =
            input_count == 0 && output_count == 1 && unsafe { *outputs == SpeakerArr::kStereo };
        if stereo { kResultTrue } else { kResultFalse }
    }

    unsafe fn getBusArrangement(
        &self,
        dir: BusDirection,
        index: i32,
        arrangement: *mut SpeakerArrangement,
    ) -> tresult {
        if index != 0 || dir != BusDirections_::kOutput as BusDirection {
            return kInvalidArgument;
        }
        unsafe { *arrangement = SpeakerArr::kStereo };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kResultFalse
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        0
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        self.sample_rate.set(unsafe { (*setup).sampleRate });
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let data = unsafe { &*data };
        if data.numOutputs != 1 || data.numSamples <= 0 {
            return kResultOk;
        }
        let frames = data.numSamples as usize;
        let output = unsafe { &*data.outputs };
        if output.numChannels != 2 {
            return kResultOk;
        }
        let (left, right) = unsafe {
            (
                slice::from_raw_parts_mut(*output.__field0.channelBuffers32, frames),
                slice::from_raw_parts_mut(*output.__field0.channelBuffers32.add(1), frames),
            )
        };
        let events = unsafe { ComRef::from_raw(data.inputEvents) };
        let count = events
            .as_ref()
            .map_or(0, |events| unsafe { events.getEventCount() });
        let mut next = 0;
        for frame in 0..frames {
            // Each event takes effect at its own frame.
            while next < count {
                let mut event: Event = unsafe { std::mem::zeroed() };
                let events = events.as_ref().expect("event list");
                if unsafe { events.getEvent(next, &mut event) } != kResultOk
                    || event.sampleOffset as usize > frame
                {
                    break;
                }
                self.apply(&event);
                next += 1;
            }
            let mut sample = 0.0;
            for voice in self.voices.borrow_mut().iter_mut() {
                sample += voice.phase.sin() as f32 * voice.level;
                voice.phase += voice.step;
            }
            left[frame] = sample;
            right[frame] = sample;
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

/// The classes of the bundle: the class id, the category, the name and the
/// subcategories.
const CLASSES: [(TUID, &str, &str, &str); 3] = [
    (PROCESSOR, "Audio Module Class", NAME, "Fx"),
    (CONTROLLER, "Component Controller Class", NAME, ""),
    (TONE, "Audio Module Class", TONE_NAME, "Instrument|Synth"),
];

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        let Some((cid, category, name, kinds)) = CLASSES.get(index as usize) else {
            return kInvalidArgument;
        };
        let info = unsafe { &mut *info };
        info.cid = *cid;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring(category, &mut info.category);
        copy_cstring(name, &mut info.name);
        info.classFlags = 0;
        copy_cstring(kinds, &mut info.subCategories);
        copy_cstring("rustel", &mut info.vendor);
        copy_cstring("1", &mut info.version);
        copy_cstring("VST 3.7", &mut info.sdkVersion);
        kResultOk
    }
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = unsafe { &mut *info };
        copy_cstring("rustel", &mut info.vendor);
        copy_cstring("", &mut info.url);
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        CLASSES.len() as i32
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        let Some((cid, category, name, _)) = CLASSES.get(index as usize) else {
            return kInvalidArgument;
        };
        let info = unsafe { &mut *info };
        info.cid = *cid;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
        copy_cstring(category, &mut info.category);
        copy_cstring(name, &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        let instance = match unsafe { *(cid as *const TUID) } {
            PROCESSOR => ComWrapper::new(Processor {
                values: Values::new(),
                fault: fault_in_audio(),
            })
            .to_com_ptr::<FUnknown>(),
            CONTROLLER => ComWrapper::new(Controller {
                values: Values::new(),
            })
            .to_com_ptr::<FUnknown>(),
            TONE => ComWrapper::new(Tone::new()).to_com_ptr::<FUnknown>(),
            _ => None,
        };
        let Some(instance) = instance else {
            return kInvalidArgument;
        };
        let unknown = instance.as_ptr();
        unsafe { ((*(*unknown).vtbl).queryInterface)(unknown, iid as *const TUID, obj) }
    }
}

#[cfg(windows)]
#[unsafe(no_mangle)]
extern "system" fn InitDll() -> bool {
    true
}

#[cfg(windows)]
#[unsafe(no_mangle)]
extern "system" fn ExitDll() -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn bundleEntry(_bundle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn bundleExit() -> bool {
    true
}

#[cfg(all(unix, not(target_os = "macos")))]
#[unsafe(no_mangle)]
extern "system" fn ModuleEntry(_library: *mut c_void) -> bool {
    true
}

#[cfg(all(unix, not(target_os = "macos")))]
#[unsafe(no_mangle)]
extern "system" fn ModuleExit() -> bool {
    true
}

/// With this variable set, the plugin stops its process, as a plugin with
/// a fault does: in the first audio block of the effect with the value
/// [`ABORT_IN_AUDIO`], and at load with each other value. The value
/// `audio:` and a file path gives the fault one time: the plugin makes the
/// file at its fault, and a process that finds the file has no fault.
pub const ABORT_ENV: &str = "RUSTEL_VST3_FIXTURE_ABORT";
pub const ABORT_IN_AUDIO: &str = "audio";

/// The fault in audio the variable asks for: `None` for no such fault, and
/// the file of a fault that happens one time.
fn fault_in_audio() -> Option<Option<std::path::PathBuf>> {
    let value = std::env::var_os(ABORT_ENV)?;
    let value = value.to_str()?.strip_prefix(ABORT_IN_AUDIO)?;
    match value.strip_prefix(':') {
        Some(file) => Some(Some(file.into())),
        None => value.is_empty().then_some(None),
    }
}

#[unsafe(no_mangle)]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    if std::env::var_os(ABORT_ENV).is_some() && fault_in_audio().is_none() {
        std::process::abort();
    }
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory2>()
        .map(|factory| factory.upcast::<IPluginFactory>())
        .map_or(ptr::null_mut(), |factory| factory.into_raw())
}
