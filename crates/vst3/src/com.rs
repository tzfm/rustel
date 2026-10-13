//! The objects the host gives a plugin.
//!
//! A plugin calls these through the VST3 interfaces. The parameter queue
//! and the event list have a fixed size, because the plugin reads them in
//! the audio callback.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComWrapper, Interface};

/// The parameter values one audio block carries to a plugin.
pub(crate) const MAX_CHANGES: usize = 32;

/// Text from a C string field of fixed size.
pub(crate) fn chars_to_string(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|char| **char != 0)
        .map(|char| *char as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Text from a UTF-16 field of fixed size.
pub(crate) fn wide_to_string(wide: &[u16]) -> String {
    let end = wide
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}

fn write_wide(text: &str, out: &mut [u16]) {
    let Some(room) = out.len().checked_sub(1) else {
        return;
    };
    let mut len = 0;
    for (unit, slot) in text.encode_utf16().zip(&mut out[..room]) {
        *slot = unit;
        len += 1;
    }
    out[len] = 0;
}

/// A borrowed interface pointer of a host object, for a plugin call.
pub(crate) fn pointer<C: Class, I: Interface>(object: &ComWrapper<C>) -> *mut I {
    object
        .as_com_ref::<I>()
        .map_or(std::ptr::null_mut(), |interface| interface.as_ptr())
}

/// The host a plugin sees. The plugin asks for message objects here.
pub(crate) struct HostApp;

impl Class for HostApp {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        write_wide("rustel", unsafe { &mut *name });
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        _iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        let wanted: [u8; 16] = unsafe { *cid }.map(|byte| byte as u8);
        let made = if wanted == IMessage::IID {
            ComWrapper::new(Message::default())
                .to_com_ptr::<IMessage>()
                .map(|message| message.into_raw().cast())
        } else if wanted == IAttributeList::IID {
            ComWrapper::new(Attributes::default())
                .to_com_ptr::<IAttributeList>()
                .map(|list| list.into_raw().cast())
        } else {
            None
        };
        match made {
            Some(object) => {
                unsafe { *obj = object };
                kResultOk
            }
            None => {
                unsafe { *obj = std::ptr::null_mut() };
                kNoInterface
            }
        }
    }
}

/// The controller reports edits and restart requests here. There is no
/// plugin window, so the host has nothing to do with an edit.
pub(crate) struct Handler;

impl Class for Handler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for Handler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, _id: ParamID, _value: ParamValue) -> tresult {
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, _flags: int32) -> tresult {
        kResultOk
    }
}

/// A plugin state in memory.
pub(crate) struct Stream {
    state: Mutex<(Vec<u8>, usize)>,
}

impl Stream {
    pub(crate) fn new(bytes: Vec<u8>) -> ComWrapper<Self> {
        ComWrapper::new(Self {
            state: Mutex::new((bytes, 0)),
        })
    }

    /// Moves the read position back to the first byte.
    pub(crate) fn rewind(&self) {
        self.state.lock().expect("stream").1 = 0;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.state.lock().expect("stream").0.is_empty()
    }
}

impl Class for Stream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for Stream {
    unsafe fn read(&self, buffer: *mut c_void, count: int32, done: *mut int32) -> tresult {
        let mut state = self.state.lock().expect("stream");
        let (bytes, at) = &mut *state;
        let len = usize::try_from(count).unwrap_or(0).min(bytes.len() - *at);
        if len > 0 && !buffer.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(bytes[*at..].as_ptr(), buffer.cast(), len) };
            *at += len;
        }
        if !done.is_null() {
            unsafe { *done = len as int32 };
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, count: int32, done: *mut int32) -> tresult {
        let mut state = self.state.lock().expect("stream");
        let (bytes, at) = &mut *state;
        let len = usize::try_from(count).unwrap_or(0);
        if len > 0 && !buffer.is_null() {
            let source = unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), len) };
            if bytes.len() < *at + len {
                bytes.resize(*at + len, 0);
            }
            bytes[*at..*at + len].copy_from_slice(source);
            *at += len;
        }
        if !done.is_null() {
            unsafe { *done = len as int32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let mut state = self.state.lock().expect("stream");
        let (bytes, at) = &mut *state;
        let base = match mode as IBStream_::IStreamSeekMode {
            IBStream_::IStreamSeekMode_::kIBSeekSet => 0,
            IBStream_::IStreamSeekMode_::kIBSeekCur => *at as i64,
            IBStream_::IStreamSeekMode_::kIBSeekEnd => bytes.len() as i64,
            _ => return kInvalidArgument,
        };
        *at = base.saturating_add(pos).clamp(0, bytes.len() as i64) as usize;
        if !result.is_null() {
            unsafe { *result = *at as int64 };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        unsafe { *pos = self.state.lock().expect("stream").1 as int64 };
        kResultOk
    }
}

enum Value {
    Int(i64),
    Float(f64),
    Text(Vec<u16>),
    Binary(Vec<u8>),
}

/// The values of a message between the two halves of a plugin.
#[derive(Default)]
pub(crate) struct Attributes {
    values: Mutex<HashMap<Vec<u8>, Value>>,
}

impl Attributes {
    fn set(&self, id: *const c_char, value: Value) -> tresult {
        if id.is_null() {
            return kInvalidArgument;
        }
        let key = unsafe { CStr::from_ptr(id) }.to_bytes().to_vec();
        self.values.lock().expect("attributes").insert(key, value);
        kResultOk
    }

    fn get(&self, id: *const c_char, read: impl FnOnce(&Value) -> bool) -> tresult {
        if id.is_null() {
            return kInvalidArgument;
        }
        let key = unsafe { CStr::from_ptr(id) }.to_bytes();
        match self.values.lock().expect("attributes").get(key) {
            Some(value) if read(value) => kResultOk,
            _ => kResultFalse,
        }
    }
}

impl Class for Attributes {
    type Interfaces = (IAttributeList,);
}

impl IAttributeListTrait for Attributes {
    unsafe fn setInt(&self, id: IAttributeList_::AttrID, value: int64) -> tresult {
        self.set(id, Value::Int(value))
    }

    unsafe fn getInt(&self, id: IAttributeList_::AttrID, value: *mut int64) -> tresult {
        self.get(id, |stored| match stored {
            Value::Int(stored) if !value.is_null() => {
                unsafe { *value = *stored };
                true
            }
            _ => false,
        })
    }

    unsafe fn setFloat(&self, id: IAttributeList_::AttrID, value: f64) -> tresult {
        self.set(id, Value::Float(value))
    }

    unsafe fn getFloat(&self, id: IAttributeList_::AttrID, value: *mut f64) -> tresult {
        self.get(id, |stored| match stored {
            Value::Float(stored) if !value.is_null() => {
                unsafe { *value = *stored };
                true
            }
            _ => false,
        })
    }

    unsafe fn setString(&self, id: IAttributeList_::AttrID, string: *const TChar) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut text = Vec::new();
        let mut at = 0;
        loop {
            let unit = unsafe { *string.add(at) };
            if unit == 0 {
                break;
            }
            text.push(unit);
            at += 1;
        }
        self.set(id, Value::Text(text))
    }

    unsafe fn getString(
        &self,
        id: IAttributeList_::AttrID,
        string: *mut TChar,
        size_in_bytes: uint32,
    ) -> tresult {
        self.get(id, |stored| match stored {
            Value::Text(stored) if !string.is_null() && size_in_bytes >= 2 => {
                let room = size_in_bytes as usize / 2 - 1;
                let len = stored.len().min(room);
                unsafe {
                    std::ptr::copy_nonoverlapping(stored.as_ptr(), string, len);
                    *string.add(len) = 0;
                }
                true
            }
            _ => false,
        })
    }

    unsafe fn setBinary(
        &self,
        id: IAttributeList_::AttrID,
        data: *const c_void,
        size_in_bytes: uint32,
    ) -> tresult {
        if data.is_null() {
            return kInvalidArgument;
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size_in_bytes as usize) };
        self.set(id, Value::Binary(bytes.to_vec()))
    }

    unsafe fn getBinary(
        &self,
        id: IAttributeList_::AttrID,
        data: *mut *const c_void,
        size_in_bytes: *mut uint32,
    ) -> tresult {
        // The pointer stays good until the plugin sets this value again.
        self.get(id, |stored| match stored {
            Value::Binary(stored) if !data.is_null() && !size_in_bytes.is_null() => {
                unsafe {
                    *data = stored.as_ptr().cast();
                    *size_in_bytes = stored.len() as uint32;
                }
                true
            }
            _ => false,
        })
    }
}

/// A message between the two halves of a plugin.
pub(crate) struct Message {
    id: Mutex<CString>,
    attributes: ComWrapper<Attributes>,
}

impl Default for Message {
    fn default() -> Self {
        Self {
            id: Mutex::new(CString::default()),
            attributes: ComWrapper::new(Attributes::default()),
        }
    }
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        // The pointer stays good until the plugin sets a new id.
        self.id.lock().expect("message id").as_ptr()
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        let text = if id.is_null() {
            CString::default()
        } else {
            unsafe { CStr::from_ptr(id) }.to_owned()
        };
        *self.id.lock().expect("message id") = text;
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        pointer(&self.attributes)
    }
}

/// The values of one parameter in one audio block, each with its frame.
pub(crate) struct Queue {
    id: AtomicU32,
    points: std::cell::UnsafeCell<[(i32, f64); MAX_POINTS]>,
    used: AtomicUsize,
}

/// The values one parameter takes in one audio block.
const MAX_POINTS: usize = 8;

// SAFETY: one thread, the audio thread, writes a queue and then calls the
// plugin, and the plugin reads the queue only in that call.
unsafe impl Send for Queue {}
unsafe impl Sync for Queue {}

impl Queue {
    fn new() -> Self {
        Self {
            id: AtomicU32::new(0),
            points: std::cell::UnsafeCell::new([(0, 0.0); MAX_POINTS]),
            used: AtomicUsize::new(0),
        }
    }

    /// Adds a value at a frame, in frame order. A value at the same frame
    /// as an earlier one takes its place. A full queue changes its last
    /// value.
    fn push(&self, offset: i32, value: f64) {
        // SAFETY: only the audio thread writes, and no read runs now.
        let points = unsafe { &mut *self.points.get() };
        let used = self.used.load(Ordering::Relaxed);
        let at = points[..used]
            .iter()
            .position(|point| point.0 > offset)
            .unwrap_or(used);
        if at > 0 && points[at - 1].0 == offset {
            points[at - 1].1 = value;
        } else if used == MAX_POINTS {
            points[MAX_POINTS - 1] = (points[MAX_POINTS - 1].0.max(offset), value);
        } else {
            points.copy_within(at..used, at + 1);
            points[at] = (offset, value);
            self.used.store(used + 1, Ordering::Relaxed);
        }
    }
}

impl Class for Queue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for Queue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.load(Ordering::Relaxed)
    }

    unsafe fn getPointCount(&self) -> int32 {
        self.used.load(Ordering::Relaxed) as int32
    }

    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        let used = self.used.load(Ordering::Relaxed);
        match usize::try_from(index) {
            Ok(index) if index < used && !offset.is_null() && !value.is_null() => {
                // SAFETY: the audio thread does not write in a plugin call.
                let point = unsafe { (&*self.points.get())[index] };
                unsafe {
                    *offset = point.0;
                    *value = point.1;
                }
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn addPoint(&self, _offset: int32, _value: ParamValue, _index: *mut int32) -> tresult {
        kResultFalse
    }
}

/// The parameter values of one audio block. The queues are made one time,
/// so a new value costs no allocation.
pub(crate) struct Changes {
    queues: Vec<ComWrapper<Queue>>,
    used: AtomicUsize,
}

impl Changes {
    pub(crate) fn new() -> ComWrapper<Self> {
        ComWrapper::new(Self {
            queues: (0..MAX_CHANGES)
                .map(|_| ComWrapper::new(Queue::new()))
                .collect(),
            used: AtomicUsize::new(0),
        })
    }

    /// Adds a value of a parameter at a frame of the next block. Returns
    /// false when the block has no room for one more parameter.
    pub(crate) fn push(&self, id: u32, offset: i32, value: f64) -> bool {
        let used = self.used.load(Ordering::Relaxed);
        let slot = self.queues[..used]
            .iter()
            .position(|queue| queue.id.load(Ordering::Relaxed) == id)
            .unwrap_or(used);
        let Some(queue) = self.queues.get(slot) else {
            return false;
        };
        if slot == used {
            queue.id.store(id, Ordering::Relaxed);
            queue.used.store(0, Ordering::Relaxed);
            self.used.store(used + 1, Ordering::Relaxed);
        }
        queue.push(offset, value);
        true
    }

    pub(crate) fn clear(&self) {
        self.used.store(0, Ordering::Relaxed);
    }
}

impl Class for Changes {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for Changes {
    unsafe fn getParameterCount(&self) -> int32 {
        self.used.load(Ordering::Relaxed) as int32
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        let used = self.used.load(Ordering::Relaxed);
        match usize::try_from(index) {
            Ok(index) if index < used => pointer(&self.queues[index]),
            _ => std::ptr::null_mut(),
        }
    }

    unsafe fn addParameterData(
        &self,
        _id: *const ParamID,
        _index: *mut int32,
    ) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }
}

/// The notes one audio block carries to a plugin.
pub(crate) const MAX_EVENTS: usize = 64;

/// The note events of one audio block. The list has a fixed size, so a new
/// event costs no allocation. The audio thread fills the list before the
/// plugin call and the plugin reads the list in that call.
pub(crate) struct Events {
    events: std::cell::UnsafeCell<[Event; MAX_EVENTS]>,
    used: AtomicUsize,
}

// SAFETY: one thread, the audio thread, writes the list and then calls the
// plugin, and the plugin reads the list only in that call.
unsafe impl Send for Events {}
unsafe impl Sync for Events {}

impl Events {
    pub(crate) fn new() -> ComWrapper<Self> {
        ComWrapper::new(Self {
            // SAFETY: an event is numbers only, and all zero is an event.
            events: std::cell::UnsafeCell::new(unsafe { std::mem::zeroed() }),
            used: AtomicUsize::new(0),
        })
    }

    /// Adds an event. Returns false when the block is full.
    pub(crate) fn push(&self, event: Event) -> bool {
        let used = self.used.load(Ordering::Relaxed);
        if used == MAX_EVENTS {
            return false;
        }
        // SAFETY: only the audio thread writes, and no read runs now.
        unsafe { (&mut *self.events.get())[used] = event };
        self.used.store(used + 1, Ordering::Relaxed);
        true
    }

    pub(crate) fn clear(&self) {
        self.used.store(0, Ordering::Relaxed);
    }

    /// Puts the events in the order of their frames. An end goes before a
    /// start at the same frame: the end is of a note that started before,
    /// and a plugin ends a note by its key.
    pub(crate) fn sort(&self) {
        let used = self.used.load(Ordering::Relaxed);
        // SAFETY: only the audio thread writes, and no read runs now.
        let events = unsafe { &mut (&mut *self.events.get())[..used] };
        let place = |event: &Event| {
            let start = event.r#type == Event_::EventTypes_::kNoteOnEvent as u16;
            (event.sampleOffset, start)
        };
        // An insertion sort: the list is short, and the sort makes no
        // allocation.
        for at in 1..events.len() {
            let mut slot = at;
            while slot > 0 && place(&events[slot - 1]) > place(&events[slot]) {
                events.swap(slot - 1, slot);
                slot -= 1;
            }
        }
    }
}

impl Class for Events {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for Events {
    unsafe fn getEventCount(&self) -> int32 {
        self.used.load(Ordering::Relaxed) as int32
    }

    unsafe fn getEvent(&self, index: int32, event: *mut Event) -> tresult {
        let used = self.used.load(Ordering::Relaxed);
        match usize::try_from(index) {
            Ok(index) if index < used && !event.is_null() => {
                // SAFETY: the audio thread does not write in a plugin call.
                unsafe { *event = (&*self.events.get())[index] };
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn addEvent(&self, _event: *mut Event) -> tresult {
        kResultFalse
    }
}
