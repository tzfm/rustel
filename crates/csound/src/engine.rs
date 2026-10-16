use libloading::Library;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::{Arc, OnceLock};

type Handle = *mut c_void;

struct Api {
    _library: Library,
    create: unsafe extern "C" fn(Handle) -> Handle,
    destroy: unsafe extern "C" fn(Handle),
    cleanup: unsafe extern "C" fn(Handle) -> c_int,
    option: unsafe extern "C" fn(Handle, *const c_char) -> c_int,
    host_audio: unsafe extern "C" fn(Handle, c_int, c_int),
    compile: unsafe extern "C" fn(Handle, *const c_char) -> c_int,
    start: unsafe extern "C" fn(Handle) -> c_int,
    perform: unsafe extern "C" fn(Handle) -> c_int,
    spout: unsafe extern "C" fn(Handle) -> *mut f64,
    ksmps: unsafe extern "C" fn(Handle) -> u32,
    channels: unsafe extern "C" fn(Handle) -> u32,
    sample_rate: unsafe extern "C" fn(Handle) -> f64,
    full_scale: unsafe extern "C" fn(Handle) -> f64,
    input: unsafe extern "C" fn(Handle, *const c_char),
    buffer: unsafe extern "C" fn(Handle, c_int),
    message: unsafe extern "C" fn(Handle) -> *const c_char,
    message_attr: unsafe extern "C" fn(Handle) -> c_int,
    pop: unsafe extern "C" fn(Handle),
    destroy_buffer: unsafe extern "C" fn(Handle),
}

impl Api {
    fn load() -> Result<Arc<Self>, String> {
        static API: OnceLock<Result<Arc<Api>, String>> = OnceLock::new();
        API.get_or_init(Self::open).clone()
    }

    fn open() -> Result<Arc<Self>, String> {
        let paths = if let Some(path) = std::env::var_os("RUSTEL_CSOUND_LIBRARY") {
            vec![path]
        } else {
            [
                "libcsound64.so.6.0",
                "libcsound64.so",
                "CsoundLib64.framework/CsoundLib64",
                "/Library/Frameworks/CsoundLib64.framework/CsoundLib64",
                "/opt/homebrew/lib/libcsound64.dylib",
                "/usr/local/lib/libcsound64.dylib",
                "libcsound64.dylib",
                "csound64.dll",
                "libcsound64.dll",
            ]
            .into_iter()
            .map(Into::into)
            .collect()
        };
        // The library and its dependencies execute native code. Only the host
        // environment, never a score, supplies its search path.
        let library = paths.iter().find_map(|path| unsafe { Library::new(path) }.ok())
            .ok_or("Csound 6 is not available. Install double-precision Csound 6.18 or set RUSTEL_CSOUND_LIBRARY to its shared library")?;
        // All symbols below use the Csound 6 public C ABI. Check its version and
        // MYFLT width before passing a handle or reading sample memory.
        unsafe {
            let version: unsafe extern "C" fn() -> c_int = *library
                .get(b"csoundGetVersion\0")
                .map_err(|_| "Csound has no version API")?;
            let size: unsafe extern "C" fn() -> c_int = *library
                .get(b"csoundGetSizeOfMYFLT\0")
                .map_err(|_| "Csound has no sample format API")?;
            if !(6_180..7_000).contains(&version()) || size() != 8 {
                return Err("Csound needs version 6.18 or later in the 6.x series, with double-precision samples".into());
            }
            macro_rules! symbol {
                ($name:literal) => {
                    *library
                        .get(concat!($name, "\0").as_bytes())
                        .map_err(|_| concat!("Csound is missing ", $name))?
                };
            }
            let initialize: unsafe extern "C" fn(c_int) -> c_int = symbol!("csoundInitialize");
            if initialize(3) < 0 {
                return Err("Csound could not initialize".into());
            }
            Ok(Arc::new(Self {
                create: symbol!("csoundCreate"),
                destroy: symbol!("csoundDestroy"),
                cleanup: symbol!("csoundCleanup"),
                option: symbol!("csoundSetOption"),
                host_audio: symbol!("csoundSetHostImplementedAudioIO"),
                compile: symbol!("csoundCompileOrc"),
                start: symbol!("csoundStart"),
                perform: symbol!("csoundPerformKsmps"),
                spout: symbol!("csoundGetSpout"),
                ksmps: symbol!("csoundGetKsmps"),
                channels: symbol!("csoundGetNchnls"),
                sample_rate: symbol!("csoundGetSr"),
                input: symbol!("csoundInputMessage"),
                full_scale: symbol!("csoundGet0dBFS"),
                buffer: symbol!("csoundCreateMessageBuffer"),
                message: symbol!("csoundGetFirstMessage"),
                message_attr: symbol!("csoundGetFirstMessageAttr"),
                pop: symbol!("csoundPopFirstMessage"),
                destroy_buffer: symbol!("csoundDestroyMessageBuffer"),
                _library: library,
            }))
        }
    }
}

/// The handle stays on the thread which creates and performs it.
pub(super) struct Engine {
    api: Arc<Api>,
    handle: Handle,
}

impl Engine {
    pub(super) fn new(sources: &[String], sample_rate: u32) -> Result<Self, String> {
        if !(8_000..=384_000).contains(&sample_rate) {
            return Err("Csound sample rate must be between 8000 and 384000 Hz".into());
        }
        let api = Api::load()?;
        // Csound owns the handle and its message buffer until Drop.
        let handle = unsafe { (api.create)(std::ptr::null_mut()) };
        if handle.is_null() {
            return Err("Csound could not create an engine".into());
        }
        let mut engine = Self { api, handle };
        unsafe {
            (engine.api.buffer)(handle, 0);
            (engine.api.host_audio)(handle, 1, 0);
        }
        for option in [
            "-n".to_string(),
            "-d".into(),
            "-m0".into(),
            "--sample-accurate".into(),
            "--ksmps=1".into(),
            "--nchnls=2".into(),
            "--0dbfs=1".into(),
            format!("--sample-rate={sample_rate}"),
        ] {
            let option = CString::new(option).unwrap();
            if unsafe { (engine.api.option)(handle, option.as_ptr()) } != 0 {
                return Err(engine.error("Csound refused an audio option"));
            }
        }
        engine.compile(&format!(
            "sr={sample_rate}\nksmps=1\nnchnls=2\n0dbfs=1\n{}",
            include_str!("presets.orc")
        ))?;
        if unsafe { (engine.api.start)(handle) } != 0 {
            return Err(engine.error("Csound could not start"));
        }
        // Csound marks its startup banner as an error message too.
        engine.messages();
        // Starting first initializes preset tables before later orchestra revisions.
        for source in sources {
            engine.compile(source)?;
        }
        // These bounds justify the two sample reads in frame().
        unsafe {
            if (engine.api.ksmps)(handle) != 1
                || (engine.api.channels)(handle) != 2
                || (engine.api.sample_rate)(handle) != f64::from(sample_rate)
                || (engine.api.full_scale)(handle) != 1.0
                || (engine.api.spout)(handle).is_null()
            {
                return Err("Csound orchestra must use the host sample rate, two output channels, ksmps=1 and 0dbfs=1".into());
            }
        }
        engine.check_messages()?;
        Ok(engine)
    }

    fn compile(&mut self, source: &str) -> Result<(), String> {
        let source = CString::new(source).map_err(|_| "Csound orchestra contains a NUL byte")?;
        if unsafe { (self.api.compile)(self.handle, source.as_ptr()) } != 0 {
            return Err(self.error("Csound orchestra did not compile"));
        }
        Ok(())
    }

    pub(super) fn score(&mut self, score: &str) -> Result<(), String> {
        let score = CString::new(score).map_err(|_| "Csound score contains a NUL byte")?;
        // inputMessage consumes the string synchronously.
        unsafe { (self.api.input)(self.handle, score.as_ptr()) };
        self.check_messages()
    }

    pub(super) fn frame(&mut self) -> Result<[f32; 2], String> {
        // The engine owns a stereo, one-frame spout for its whole lifetime.
        unsafe {
            if (self.api.perform)(self.handle) != 0 {
                return Err(self.error("Csound performance stopped"));
            }
            let output = (self.api.spout)(self.handle);
            let frame = [*output as f32, *output.add(1) as f32];
            if frame.iter().any(|v| !v.is_finite()) {
                return Err("Csound produced a non-finite audio sample".into());
            }
            Ok(frame)
        }
    }

    fn messages(&mut self) -> (String, bool) {
        let mut messages = String::new();
        let mut failed = false;
        unsafe {
            loop {
                let message = (self.api.message)(self.handle);
                if message.is_null() {
                    break;
                }
                failed |= (self.api.message_attr)(self.handle) & 0x7000 == 0x1000;
                if messages.len() < 8192 {
                    let message = CStr::from_ptr(message).to_string_lossy();
                    let mut end = message.len().min(8192 - messages.len());
                    while !message.is_char_boundary(end) {
                        end -= 1;
                    }
                    messages.push_str(&message[..end]);
                }
                (self.api.pop)(self.handle);
            }
        }
        (messages, failed)
    }

    pub(super) fn check_messages(&mut self) -> Result<(), String> {
        let (messages, failed) = self.messages();
        if failed {
            Err(format!("Csound performance failed: {}", messages.trim()))
        } else {
            Ok(())
        }
    }

    fn error(&mut self, prefix: &str) -> String {
        format!("{prefix}: {}", self.messages().0.trim())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // No other thread can reach this handle.
        unsafe {
            (self.api.cleanup)(self.handle);
            (self.api.destroy_buffer)(self.handle);
            (self.api.destroy)(self.handle);
        }
    }
}
