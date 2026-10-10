//! One loaded plugin library and the classes it holds.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::path::Path;

use vst3::Steinberg::*;
use vst3::{ComPtr, Interface};

use crate::com::{chars_to_string, wide_to_string};

/// The category a plugin gives its audio classes.
const AUDIO_CLASS: &str = "Audio Module Class";

/// One audio class of a plugin library.
#[derive(Clone)]
pub(crate) struct ClassInfo {
    pub cid: TUID,
    pub name: String,
    pub vendor: String,
    /// For example "Fx|Reverb" or "Instrument|Synth".
    pub categories: String,
}

impl ClassInfo {
    /// The class id as a `.vstpreset` file writes it: 32 hex characters.
    pub(crate) fn id_text(&self) -> String {
        // Windows stores the first 8 bytes in the byte order of a GUID.
        let order: [usize; 16] = if cfg!(windows) {
            [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15]
        } else {
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        };
        order
            .iter()
            .map(|at| format!("{:02X}", self.cid[*at] as u8))
            .collect()
    }
}

#[cfg(unix)]
type Entry = unsafe extern "system" fn(*mut c_void) -> u8;
type Exit = unsafe extern "system" fn() -> u8;
type GetFactory = unsafe extern "system" fn() -> *mut IPluginFactory;

/// A loaded plugin library. The plugin thread loads and drops a module.
pub(crate) struct Module {
    factory: ManuallyDrop<ComPtr<IPluginFactory>>,
    library: ManuallyDrop<libloading::Library>,
    #[cfg(target_os = "macos")]
    bundle: *const c_void,
}

// SAFETY: the plugin thread makes every call into a module. Other threads
// hold a module only to keep the library loaded.
unsafe impl Send for Module {}
unsafe impl Sync for Module {}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: *const c_void,
        buffer: *const u8,
        length: isize,
        is_directory: u8,
    ) -> *const c_void;
    fn CFBundleCreate(allocator: *const c_void, url: *const c_void) -> *const c_void;
    fn CFRelease(object: *const c_void);
}

/// Turns COM on for the life of the calling thread, one time.
///
/// A DAW on Windows has COM on in the thread that makes its plugins, and a
/// plugin counts on this. With COM off, a plugin turns COM on for its first
/// copy and off at the end of that copy. The system then unloads the COM
/// objects the plugin keeps for all its copies, and the second copy stops
/// with a memory fault.
#[cfg(windows)]
fn start_com() {
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn OleInitialize(reserved: *mut c_void) -> i32;
    }
    thread_local! {
        static STARTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if !STARTED.replace(true) {
        // COM stays on to the end of the thread, so the call has no pair.
        unsafe { OleInitialize(std::ptr::null_mut()) };
    }
}

impl Module {
    pub(crate) fn load(bundle: &Path) -> Result<Self, String> {
        let binary = crate::scan::binary_path(bundle)
            .ok_or("the bundle has no plugin library for this system")?;
        #[cfg(windows)]
        start_com();
        // Load with all symbols resolved. A missing symbol is then an
        // error here and not a crash in the audio callback.
        #[cfg(unix)]
        let (library, handle) = {
            use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
            let library = unsafe { Library::open(Some(&binary), RTLD_NOW | RTLD_LOCAL) }
                .map_err(|error| error.to_string())?;
            let handle = library.into_raw();
            (
                libloading::Library::from(unsafe { Library::from_raw(handle) }),
                handle,
            )
        };
        #[cfg(not(unix))]
        let library =
            unsafe { libloading::Library::new(&binary) }.map_err(|error| error.to_string())?;

        #[cfg(target_os = "macos")]
        let bundle = {
            use std::os::unix::ffi::OsStrExt;
            let path = bundle.as_os_str().as_bytes();
            let _ = handle;
            unsafe {
                let url = CFURLCreateFromFileSystemRepresentation(
                    std::ptr::null(),
                    path.as_ptr(),
                    path.len() as isize,
                    1,
                );
                if url.is_null() {
                    return Err("the bundle path is not valid".into());
                }
                let reference = CFBundleCreate(std::ptr::null(), url);
                CFRelease(url);
                if reference.is_null() {
                    return Err("the system did not open the bundle".into());
                }
                reference
            }
        };

        // The entry call gets the bundle on macOS, the library handle on
        // other Unix systems, and nothing on Windows.
        #[cfg(target_os = "macos")]
        let entered = Self::call_entry(&library, &[b"bundleEntry\0", b"BundleEntry\0"], bundle);
        #[cfg(all(unix, not(target_os = "macos")))]
        let entered = Self::call_entry(&library, &[b"ModuleEntry\0"], handle);
        #[cfg(windows)]
        let entered = unsafe { library.get::<Exit>(b"InitDll\0") }
            .map_or(true, |init| unsafe { init() != 0 });
        let factory = if entered {
            unsafe { library.get::<GetFactory>(b"GetPluginFactory\0") }
                .ok()
                .and_then(|get| unsafe { ComPtr::from_raw(get()) })
        } else {
            None
        };
        let Some(factory) = factory else {
            if entered {
                Self::call_exit(&library);
            }
            #[cfg(target_os = "macos")]
            unsafe {
                CFRelease(bundle)
            };
            return Err(if entered {
                "the library is not a VST3 plugin".into()
            } else {
                "the plugin refused to start".into()
            });
        };
        Ok(Self {
            factory: ManuallyDrop::new(factory),
            library: ManuallyDrop::new(library),
            #[cfg(target_os = "macos")]
            bundle,
        })
    }

    #[cfg(unix)]
    fn call_entry(library: &libloading::Library, names: &[&[u8]], argument: *const c_void) -> bool {
        for name in names {
            if let Ok(entry) = unsafe { library.get::<Entry>(name) } {
                return unsafe { entry(argument.cast_mut()) } != 0;
            }
        }
        // An old plugin has no entry call.
        true
    }

    fn call_exit(library: &libloading::Library) {
        let names: &[&[u8]] = if cfg!(target_os = "macos") {
            &[b"bundleExit\0", b"BundleExit\0"]
        } else if cfg!(windows) {
            &[b"ExitDll\0"]
        } else {
            &[b"ModuleExit\0"]
        };
        for name in names {
            if let Ok(exit) = unsafe { library.get::<Exit>(name) } {
                unsafe { exit() };
                return;
            }
        }
    }

    /// The audio classes of the library.
    pub(crate) fn classes(&self) -> Vec<ClassInfo> {
        let factory = &*self.factory;
        let detailed = factory.cast::<IPluginFactory2>();
        let wide = factory.cast::<IPluginFactory3>();
        let mut vendor = String::new();
        let mut info: PFactoryInfo = unsafe { std::mem::zeroed() };
        if unsafe { factory.getFactoryInfo(&mut info) } == kResultOk {
            vendor = chars_to_string(&info.vendor);
        }
        let mut classes = Vec::new();
        for index in 0..unsafe { factory.countClasses() } {
            let mut basic: PClassInfo = unsafe { std::mem::zeroed() };
            if unsafe { factory.getClassInfo(index, &mut basic) } != kResultOk
                || chars_to_string(&basic.category) != AUDIO_CLASS
            {
                continue;
            }
            let mut class = ClassInfo {
                cid: basic.cid,
                name: chars_to_string(&basic.name),
                vendor: vendor.clone(),
                categories: String::new(),
            };
            let mut more: PClassInfo2 = unsafe { std::mem::zeroed() };
            if let Some(detailed) = &detailed
                && unsafe { detailed.getClassInfo2(index, &mut more) } == kResultOk
            {
                class.categories = chars_to_string(&more.subCategories);
                let own = chars_to_string(&more.vendor);
                if !own.is_empty() {
                    class.vendor = own;
                }
            }
            let mut unicode: PClassInfoW = unsafe { std::mem::zeroed() };
            if let Some(wide) = &wide
                && unsafe { wide.getClassInfoUnicode(index, &mut unicode) } == kResultOk
            {
                let name = wide_to_string(&unicode.name);
                if !name.is_empty() {
                    class.name = name;
                }
            }
            classes.push(class);
        }
        classes
    }

    /// Gives the plugin library the host object, if the library asks for one.
    pub(crate) fn set_host(&self, host: *mut FUnknown) {
        if let Some(factory) = self.factory.cast::<IPluginFactory3>() {
            unsafe { factory.setHostContext(host) };
        }
    }

    /// Makes one object of a class.
    pub(crate) fn create<I: Interface>(&self, cid: &TUID) -> Option<ComPtr<I>> {
        let mut object = std::ptr::null_mut();
        let result = unsafe {
            self.factory
                .createInstance(cid.as_ptr(), I::IID.as_ptr().cast(), &mut object)
        };
        if result != kResultOk {
            return None;
        }
        unsafe { ComPtr::from_raw(object.cast()) }
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // The factory goes first, then the exit call, then the library.
        unsafe { ManuallyDrop::drop(&mut self.factory) };
        Self::call_exit(&self.library);
        #[cfg(target_os = "macos")]
        unsafe {
            CFRelease(self.bundle)
        };
        unsafe { ManuallyDrop::drop(&mut self.library) };
    }
}
