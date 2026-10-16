//! The standard output of the process, kept clear of plugin text.
//!
//! A plugin is native code in this process, and prints when it wants: at
//! its load, in an audio block, from a thread of its own. One such line in
//! the output of a command, or on the screen of Studio, breaks that output.
//! So a program that hosts plugins keeps the stream for itself:
//!
//! ```text
//!   out()          a copy of the stream of the process start
//!   keep_clear()   descriptor 1 goes to the null device
//!
//!   the program ─── out() ─────────▶ terminal, pipe or file
//!   a plugin ────── descriptor 1 ──▶ null device
//! ```
//!
//! The program writes with [`out!`](crate::out) and [`outln!`](crate::outln)
//! in place of `print!` and `println!`. The two pairs go to the same place
//! before [`keep_clear`]. After the call, a `println!` goes to the null
//! device.

use std::fs::File;
use std::io::{self, IsTerminal, Write};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// A copy of the standard output the process had at its start.
pub struct Out(Mutex<Option<Stream>>);

/// The stream of [`Out`].
#[cfg(not(windows))]
pub type Stream = File;

/// The stream of [`Out`] on Windows. A console takes text as UTF-16. A byte
/// write to a console goes through the console code page, and each character
/// outside ASCII then shows as two or three wrong ones. The text is UTF-8,
/// so this stream converts the text, as the standard library does for its
/// own output. A stream that is no console, such as a pipe or a file, takes
/// the bytes as they are.
#[cfg(windows)]
pub struct Stream {
    file: File,
    console: bool,
    /// The first bytes of a character that the last write cut in two.
    cut: Vec<u8>,
}

#[cfg(windows)]
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Console::WriteConsoleW;

        /// The UTF-16 units of one console call.
        const CHUNK: usize = 8192;
        if !self.console {
            return self.file.write(bytes);
        }
        self.cut.extend_from_slice(bytes);
        // A character at the end with bytes still to come waits for them.
        let whole = match std::str::from_utf8(&self.cut) {
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            _ => self.cut.len(),
        };
        let text: Vec<u16> = String::from_utf8_lossy(&self.cut[..whole])
            .encode_utf16()
            .collect();
        self.cut.drain(..whole);
        let mut units = text.as_slice();
        while !units.is_empty() {
            let mut count = units.len().min(CHUNK);
            // The two halves of a surrogate pair go in the same call.
            if count < units.len() && (0xD800..0xDC00).contains(&units[count - 1]) {
                count -= 1;
            }
            let mut written = 0;
            // SAFETY: the handle is an open console. The pointer and the
            // count name units that live for the full call.
            let done = unsafe {
                WriteConsoleW(
                    self.file.as_raw_handle(),
                    units.as_ptr(),
                    count as u32,
                    &mut written,
                    std::ptr::null(),
                )
            };
            if done == 0 {
                return Err(io::Error::last_os_error());
            }
            if written == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            units = &units[written as usize..];
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
impl Stream {
    fn new(file: File) -> Self {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Console::GetConsoleMode;
        let mut mode = 0;
        // SAFETY: the handle is open, and the mode is a live local. Only a
        // console has a mode.
        let console = unsafe { GetConsoleMode(file.as_raw_handle(), &mut mode) } != 0;
        Self {
            file,
            console,
            cut: Vec::new(),
        }
    }

    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
            console: self.console,
            cut: Vec::new(),
        })
    }

    fn file(&self) -> &File {
        &self.file
    }
}

/// A copy of the standard output of the process, as a stream of its own.
/// The copy stays where it points when descriptor 1 moves.
#[cfg(unix)]
fn copy_of_stdout() -> io::Result<Stream> {
    use std::os::fd::AsFd;
    Ok(File::from(io::stdout().as_fd().try_clone_to_owned()?))
}

#[cfg(windows)]
fn copy_of_stdout() -> io::Result<Stream> {
    use std::os::windows::io::{AsHandle, AsRawHandle};
    let stdout = io::stdout();
    let handle = stdout.as_raw_handle();
    if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the process has no standard output",
        ));
    }
    Ok(Stream::new(File::from(
        stdout.as_handle().try_clone_to_owned()?,
    )))
}

#[cfg(not(any(unix, windows)))]
fn copy_of_stdout() -> io::Result<Stream> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no copy of the standard output on this system",
    ))
}

/// The standard output the process had at its start. The first call makes
/// the copy, so the first call must come before a move of descriptor 1:
/// [`keep_clear`] sees to this.
pub fn out() -> &'static Out {
    static OUT: OnceLock<Out> = OnceLock::new();
    OUT.get_or_init(|| Out(Mutex::new(copy_of_stdout().ok())))
}

impl Out {
    /// The stream for a number of writes with no line of a different
    /// thread between them. A process with no standard output loses each
    /// write with no error, as the standard library does. So does a pipe
    /// whose reader has gone, as `head` goes after its lines.
    pub fn lock(&self) -> OutLock<'_> {
        OutLock(self.0.lock().unwrap_or_else(|error| error.into_inner()))
    }

    /// One more copy of the stream, for a writer with a buffer of its own.
    pub fn stream(&self) -> io::Result<Stream> {
        match self.lock().0.as_ref() {
            Some(stream) => stream.try_clone(),
            None => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the process has no standard output",
            )),
        }
    }

    /// True when the stream is a terminal.
    pub fn is_terminal(&self) -> bool {
        self.lock().0.as_ref().is_some_and(|stream| {
            #[cfg(windows)]
            let stream = stream.file();
            stream.is_terminal()
        })
    }

    /// The descriptor of the stream, for a terminal call that takes one.
    #[cfg(unix)]
    pub fn descriptor(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.lock().0.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// Writes the text in one piece. This is the work of [`out!`](crate::out)
    /// and [`outln!`](crate::outln), and fails as `print!` does.
    pub fn print(&self, text: std::fmt::Arguments<'_>) {
        let written = match text.as_str() {
            Some(text) => self.lock().write_all(text.as_bytes()),
            None => self.lock().write_all(text.to_string().as_bytes()),
        };
        if let Err(error) = written {
            panic!("failed printing to stdout: {error}");
        }
    }
}

/// The locked stream of [`Out::lock`].
pub struct OutLock<'a>(MutexGuard<'a, Option<Stream>>);

impl OutLock<'_> {
    /// The reader of the pipe has gone. The output ends, and the run goes
    /// on to its usual end.
    fn close(&mut self) {
        *self.0 = None;
    }
}

impl Write for OutLock<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.0.as_mut().map(|stream| stream.write(bytes)) {
            Some(Err(error)) if error.kind() == io::ErrorKind::BrokenPipe => {
                self.close();
                Ok(bytes.len())
            }
            Some(written) => written,
            None => Ok(bytes.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.0.as_mut().map(Write::flush) {
            Some(Err(error)) if error.kind() == io::ErrorKind::BrokenPipe => {
                self.close();
                Ok(())
            }
            Some(flushed) => flushed,
            None => Ok(()),
        }
    }
}

/// `print!` on the standard output of the process start: see
/// [`stdio`](crate::stdio).
#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::stdio::out().print(::std::format_args!($($arg)*))
    };
}

/// `println!` on the standard output of the process start: see
/// [`stdio`](crate::stdio).
#[macro_export]
macro_rules! outln {
    () => {
        $crate::stdio::out().print(::std::format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::stdio::out().print(::std::format_args!("{}\n", ::std::format_args!($($arg)*)))
    };
}

/// Points descriptor 1 of the process at the null device, to the end of the
/// process. A line a plugin prints then reaches no one, from any thread and
/// at any time. The program keeps its output: [`out`] made its copy of the
/// stream before. False when the system refused, and the stream is then as
/// it was.
///
/// A program calls this at its start, before the first plugin load. Only
/// the first call does the work: a second move closes the stream that a
/// loaded plugin keeps as its output. A test must not call this: the test
/// runner reports on descriptor 1.
pub fn keep_clear() -> bool {
    static DONE: OnceLock<bool> = OnceLock::new();
    *DONE.get_or_init(|| {
        let _ = out();
        // A line that waits in the buffer of the standard library goes
        // first.
        let _ = io::stdout().flush();
        to_null()
    })
}

/// The path that reaches the stream of [`out`], when `path` names the
/// standard output of the process as `/dev/stdout` does. After
/// [`keep_clear`] such a path opens the null device, and a file sent down a
/// pipe is lost. So a program asks before that call, and opens the answer
/// in place of `path`.
#[cfg(unix)]
pub fn path_to_out(path: &std::path::Path) -> Option<std::path::PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    let named = std::fs::metadata(path).ok()?;
    let lock = out().lock();
    let stream = lock.0.as_ref()?;
    let kept = stream.metadata().ok()?;
    let same = (named.dev(), named.ino()) == (kept.dev(), kept.ino());
    same.then(|| format!("/dev/fd/{}", stream.as_raw_fd()).into())
}

#[cfg(unix)]
fn to_null() -> bool {
    use std::os::fd::AsRawFd;
    let Ok(null) = File::options().write(true).open("/dev/null") else {
        return false;
    };
    // SAFETY: the two descriptors are open. Descriptor 1 keeps the null
    // device after `null` ends.
    unsafe { libc::dup2(null.as_raw_fd(), 1) == 1 }
}

#[cfg(windows)]
fn to_null() -> bool {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::{STD_OUTPUT_HANDLE, SetStdHandle};
    let Ok(null) = File::options().write(true).open("NUL") else {
        return false;
    };
    // The handle stays open to the end of the process. A plugin with a C
    // runtime of its own reads the output slot at its load and keeps the
    // value.
    let handle = null.into_raw_handle();
    // The slot is what the standard library and a later C runtime read.
    // SAFETY: the handle is open.
    let moved = unsafe { SetStdHandle(STD_OUTPUT_HANDLE, handle) } != 0;
    // Descriptor 1 of the C runtime of this program is a different thing:
    // a plugin that shares the runtime prints through the descriptor. In a
    // console program the runtime then puts a copy of the handle in the
    // slot. The copy stays open too.
    // SAFETY: the runtime takes the open handle for a new descriptor, and
    // descriptor 1 becomes a copy of that descriptor.
    unsafe {
        let descriptor = libc::open_osfhandle(handle as libc::intptr_t, 0);
        if descriptor >= 0 {
            libc::dup2(descriptor, 1);
        }
    }
    moved
}

#[cfg(not(any(unix, windows)))]
fn to_null() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A write through the lock of a process with no stream is lost with no
    /// error, and a stream takes each byte.
    #[test]
    fn a_missing_stream_takes_each_write_and_a_stream_gets_the_bytes() {
        let none = Out(Mutex::new(None));
        none.print(format_args!("lost {}\n", 1));
        assert!(none.lock().write_all(b"lost").is_ok());
        assert!(!none.is_terminal());
        assert!(none.stream().is_err());

        let path = std::env::temp_dir().join(format!("rustel-stdio-{}", std::process::id()));
        let file = File::create(&path).expect("file");
        #[cfg(windows)]
        let file = Stream::new(file);
        let out = Out(Mutex::new(Some(file)));
        out.print(format_args!("one {}\n", 1));
        out.print(format_args!("two\n"));
        let mut copy = out.stream().expect("copy");
        copy.write_all("three ✓\n".as_bytes()).expect("write");
        assert!(!out.is_terminal());
        drop((out, copy));
        let written = std::fs::read_to_string(&path).expect("read");
        let _ = std::fs::remove_file(&path);
        assert_eq!(written, "one 1\ntwo\nthree ✓\n");
    }
}
