//! Show a file in the desktop's file manager, or a page in its browser.
//!
//! An exported bounce, a take, the folder a sample bank came from: the
//! studio knows where they are, and the desktop - when there is one - can
//! show them. Over SSH or on a bare console there is no desktop; the
//! command is missing or exits complaining, and the caller falls back to
//! putting the path on the clipboard, which works everywhere.
//!
//! A page's address can be text a score or someone else's bank file wrote,
//! so `plan` decides what the desktop is handed before anything starts:
//! only an http or https address, written out again by the URL parser. It
//! is never put on a command line for cmd or a shell to re-read: `open` and
//! `xdg-open` each get it as one argv element, and Windows hands it to the
//! shell's own `open` verb, never to `cmd /c start`, because cmd expands
//! `%variables%` before it looks at quotes and no quoting keeps an address
//! inert there.
//!
//! Under `RUSTEL_HEADLESS=1` - tests, CI - nothing is spawned at all: the
//! reveal answers at once, as if a desktop had taken the job. What a test
//! exercises is the status line and the fallback chain, not whoever is
//! running it; a keyboard probe that walks the sample browser must never
//! open a browser on the machine that runs the probe.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

/// Set to `1` by the test suites: `reveal` starts nothing and answers
/// `Ok` at once, so driving the studio's Alt+O or a reveal click in a
/// test opens nothing on the machine running the test.
pub const HEADLESS_ENV: &str = "RUSTEL_HEADLESS";

fn headless() -> bool {
    std::env::var_os(HEADLESS_ENV).is_some_and(|value| value == "1")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevealTarget {
    /// Select this file in the file manager.
    File(PathBuf),
    /// Open this folder in the file manager.
    Folder(PathBuf),
    /// Open this address in the browser.
    Url(String),
}

impl RevealTarget {
    /// What goes on the clipboard when the desktop cannot help.
    pub fn text(&self) -> String {
        match self {
            Self::File(path) | Self::Folder(path) => path.display().to_string(),
            Self::Url(url) => url.clone(),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::File(path) | Self::Folder(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            Self::Url(url) => url.clone(),
        }
    }
}

/// Where a sample bank's first file lives, as something a person can look
/// at: a local folder, or the repository page for a `github:` manifest
/// (raw file addresses do not render in a browser; the tree page does).
pub fn bank_location(url: &str) -> Option<RevealTarget> {
    if url.starts_with("file:") {
        return file_url_path(url).map(RevealTarget::File);
    }
    // A file the pinned library holds on the strudel CDN points at a
    // listing the CDN refuses (`403 Forbidden`); the library knows the
    // repository each collection really lives in, and that page renders.
    // A collection's bare root - no file under it - has neither a listing
    // the CDN serves nor a file to find anywhere else.
    if let Some(rest) = url.strip_prefix("https://strudel.b-cdn.net/")
        && !rest.trim_end_matches('/').contains('/')
    {
        return None;
    }
    let url = rustel_runtime::samples::upstream_file_url(url).unwrap_or_else(|| url.to_owned());
    if let Some(rest) = url.strip_prefix("https://raw.githubusercontent.com/") {
        let mut parts = rest.splitn(4, '/');
        let (user, repo, branch, path) =
            (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        let directory = path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
        return Some(RevealTarget::Url(format!(
            "https://github.com/{user}/{repo}/tree/{branch}/{directory}"
        )));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        let parent = url
            .rsplit_once('/')
            .map(|(dir, _)| dir)
            .unwrap_or(url.as_str());
        return Some(RevealTarget::Url(format!("{parent}/")));
    }
    None
}

/// The file a `file:` address names, when it is there.
///
/// Two spellings arrive here. A standard file URL - what a score's local
/// import is registered under, `file:///C:/My%20Drums/bd.wav` - goes back
/// through the URL parser, which undoes the percent-encoding and, on
/// Windows, the slash before the drive; stripping the scheme by hand left
/// `/C:/My%20Drums/bd.wav`, which is nowhere. The raw spelling a scanned
/// folder is registered under - `file://` and the path as the system prints
/// it, spaces and backslashes kept - is taken as it stands, since decoding
/// it would turn a file really named `100%25.wav` into a different one.
/// The raw spelling is tried first, and only as an absolute path: a URL
/// with a host, `file://server/share/x.wav`, is not a path relative to
/// wherever the studio happened to start.
fn file_url_path(url: &str) -> Option<PathBuf> {
    let raw = url
        .strip_prefix("file://")
        .map(PathBuf::from)
        .filter(|raw| raw.is_absolute() && raw.exists());
    raw.or_else(|| {
        url::Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.to_file_path().ok())
            .filter(|path| path.exists())
    })
}

/// A reveal in progress: the desktop's answer arrives when it gives one -
/// when the command exits, or when ShellExecute returns - which on a
/// working desktop is immediate.
pub struct Reveal {
    pub target: RevealTarget,
    receiver: Receiver<Result<(), String>>,
}

impl Reveal {
    pub fn poll(&self) -> Option<Result<(), String>> {
        self.receiver.try_recv().ok()
    }
}

/// Why a reveal did not start.
#[derive(Debug)]
pub enum RevealError {
    /// The address is not one to hand to a desktop; the text says what is
    /// wrong with it.
    Refused(String),
    /// There is nothing to ask: no desktop, or its command is missing.
    NoDesktop(std::io::Error),
}

/// Ask the desktop.
///
/// An address `plan` refuses is refused first, headless or not: it is
/// never handed to anything. `NoDesktop` means there was nothing to ask.
///
/// Headless (`RUSTEL_HEADLESS=1`), there is nothing to ask and nothing to
/// wait for: the answer is already in the pipe, so the caller's status
/// walks `opening …` and then `opened …` on the next poll exactly as it
/// does with a desktop on the other end.
pub fn reveal(target: RevealTarget) -> Result<Reveal, RevealError> {
    let launch = plan(&target, Desktop::HERE).map_err(RevealError::Refused)?;
    let (sender, receiver) = channel();
    if headless() {
        let _ = sender.send(Ok(()));
    } else {
        start(launch, sender).map_err(RevealError::NoDesktop)?;
    }
    Ok(Reveal { target, receiver })
}

/// Which desktop a reveal asks. A value rather than only a `cfg`, so what
/// each one would be handed is decided, and tested, on every platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Desktop {
    /// Finder and the default browser, through `open`.
    Mac,
    /// Explorer, and the Windows shell's `open` verb.
    Windows,
    /// Everything else, through `xdg-open`.
    Freedesktop,
}

impl Desktop {
    const HERE: Self = if cfg!(target_os = "macos") {
        Self::Mac
    } else if cfg!(windows) {
        Self::Windows
    } else {
        Self::Freedesktop
    };

    /// Every desktop, for the tests that hold each one to the same rule.
    #[cfg(test)]
    const ALL: [Self; 3] = [Self::Mac, Self::Windows, Self::Freedesktop];
}

/// What a reveal starts, decided before anything is started.
#[derive(Debug, PartialEq, Eq)]
enum Launch {
    /// `program` with each of `args` as one argv element of its own. No
    /// command line is built for them, so nothing splits or expands them on
    /// the way; `xdg-open` is itself a shell script, but it receives the
    /// address as `$1`.
    Program {
        program: &'static str,
        args: Vec<OsString>,
    },
    /// Explorer with this one argument exactly as written. Explorer parses
    /// its own command line and misreads the quoting Rust would add (see
    /// [`explorer_select_argument`]); it is not a shell, and runs nothing.
    Explorer(OsString),
    /// This address, opened by the Windows shell's `open` verb:
    /// `ShellExecuteW(NULL, "open", address, NULL, NULL, SW_SHOWNORMAL)`.
    /// No command line is built, so there is nothing for cmd to read.
    ShellOpen(String),
}

/// What `desktop` is asked to start to show `target`, or why an address is
/// refused. Nothing is started and nothing is asked here.
fn plan(target: &RevealTarget, desktop: Desktop) -> Result<Launch, String> {
    let program = |program: &'static str, args: Vec<OsString>| Launch::Program { program, args };
    Ok(match (target, desktop) {
        (RevealTarget::File(path), Desktop::Mac) => program("open", vec!["-R".into(), path.into()]),
        (RevealTarget::File(path), Desktop::Windows) => {
            Launch::Explorer(explorer_select_argument(path))
        }
        (RevealTarget::File(path), Desktop::Freedesktop) => {
            program("xdg-open", vec![path.parent().unwrap_or(path).into()])
        }
        (RevealTarget::Folder(path), Desktop::Mac) => program("open", vec![path.into()]),
        (RevealTarget::Folder(path), Desktop::Windows) => Launch::Explorer(explorer_path(path)),
        (RevealTarget::Folder(path), Desktop::Freedesktop) => {
            program("xdg-open", vec![path.into()])
        }
        (RevealTarget::Url(url), desktop) => {
            let address = browser_address(url)?;
            match desktop {
                Desktop::Mac => program("open", vec![address.into()]),
                Desktop::Windows => Launch::ShellOpen(address),
                Desktop::Freedesktop => program("xdg-open", vec![address.into()]),
            }
        }
    })
}

/// The address a browser is handed for `url`, or why there is none.
///
/// Only http and https: a bank's page is a web page, and every other
/// scheme - `file:`, `javascript:`, `data:`, `ms-settings:`, whatever else
/// the desktop has a handler for - asks the desktop to run something, not
/// to show something.
///
/// Every page revealed is a folder of a registered bank's files, as
/// [`bank_location`] finds it, so the address is read the way the sample
/// fetch reads that bank's files: [`rustel_runtime::sample_fetch::web_address`],
/// where a `#` is part of a folder's name. What is handed on is that
/// parser's spelling, never the text as written - one ASCII token, with no
/// space or quote in it.
fn browser_address(url: &str) -> Result<String, String> {
    rustel_runtime::sample_fetch::web_address(url).map(String::from)
}

/// Start what [`plan`] decided. The desktop's answer arrives on `sender`
/// when it gives one, which on a working desktop is at once.
fn start(launch: Launch, sender: Sender<Result<(), String>>) -> std::io::Result<()> {
    let mut command = match launch {
        Launch::Program { program, args } => {
            let mut command = Command::new(program);
            command.args(args);
            command
        }
        #[cfg(windows)]
        Launch::Explorer(argument) => {
            use std::os::windows::process::CommandExt;
            let mut command = Command::new("explorer");
            command.raw_arg(argument);
            command
        }
        #[cfg(windows)]
        Launch::ShellOpen(address) => return shell_open(address, sender),
        #[cfg(not(windows))]
        Launch::Explorer(_) | Launch::ShellOpen(_) => {
            unreachable!("only the Windows desktop is planned through Explorer or its shell")
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    answer_on_thread(sender, move || match child.wait() {
        // Windows Explorer answers 1 whatever happened; only a missing
        // command is a real refusal there.
        Ok(status) if status.success() || cfg!(windows) => Ok(()),
        Ok(status) => Err(format!("the desktop said no ({status})")),
        Err(error) => Err(error.to_string()),
    })
}

/// Wait for the desktop's answer on a thread of its own, and send it: a
/// desktop may take a moment to find its browser, and the studio does not
/// wait for it.
fn answer_on_thread(
    sender: Sender<Result<(), String>>,
    ask: impl FnOnce() -> Result<(), String> + Send + 'static,
) -> std::io::Result<()> {
    thread::Builder::new()
        .name("studio-reveal".into())
        .spawn(move || {
            let _ = sender.send(ask());
        })?;
    Ok(())
}

/// Open `address` with the Windows shell's `open` verb, answering on a
/// thread of its own like every other launch.
#[cfg(windows)]
fn shell_open(address: String, sender: Sender<Result<(), String>>) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |text: &str| -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    /// ShellExecuteW returns a value above this on success, and an error
    /// code at or below it: see the Return value section of
    /// <https://learn.microsoft.com/windows/win32/api/shellapi/nf-shellapi-shellexecutew>.
    const SHELL_EXECUTE_OK_ABOVE: isize = 32;

    let verb = wide("open");
    let file = wide(&address);
    answer_on_thread(sender, move || {
        // The shell can hand the address to an extension it reaches
        // through COM, so COM comes up first, as ShellExecute's
        // documentation asks.
        // SAFETY: the reserved argument is null and the flags are
        // documented ones; a success is paired with `CoUninitialize`
        // below, on this same thread.
        let com = unsafe {
            CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        // SAFETY: `verb` and `file` are NUL-terminated UTF-16 buffers
        // that live across the call; an interior NUL would only cut the
        // address short, and `web_address` refuses one anyway. The
        // window, parameters and directory may all be null.
        let instance = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if com >= 0 {
            // SAFETY: balances the successful `CoInitializeEx` above.
            unsafe { CoUninitialize() };
        }
        let code = instance as isize;
        if code > SHELL_EXECUTE_OK_ABOVE {
            Ok(())
        } else {
            Err(format!("the desktop said no (ShellExecute error {code})"))
        }
    })
}

/// Path string Explorer will accept: absolute when possible, native
/// backslashes, no `\\?\` prefix (Explorer cannot navigate those).
fn explorer_path(path: &Path) -> OsString {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let raw = resolved.to_string_lossy();
    let stripped = if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        raw.into_owned()
    };
    OsString::from(stripped.replace('/', "\\"))
}

/// `/select,"C:\path\to\file"` as one raw CreateProcess argument.
///
/// Rust's normal `.arg` quotes the whole token on Windows; Explorer then
/// fails to parse `/select,…` and falls back to Documents. `raw_arg`
/// hands the bytes through unchanged. Forward slashes are converted too -
/// Explorer reads each `/segment` as a switch and likewise opens Documents.
fn explorer_select_argument(path: &Path) -> OsString {
    let mut argument = OsString::from("/select,\"");
    argument.push(explorer_path(path));
    argument.push("\"");
    argument
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A score's local import is registered under a standard file URL, a
    /// scanned folder under the raw path: both reach the file, spaces and
    /// all, and a raw name that only looks encoded is not decoded away.
    #[test]
    fn both_spellings_of_a_local_file_find_it() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("My Drums");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("deep bass.wav");
        std::fs::write(&file, b"RIFF").unwrap();
        let file = file.canonicalize().unwrap();
        let url = url::Url::from_file_path(&file).unwrap().to_string();
        assert!(url.contains("%20"), "the fixture is really encoded: {url}");
        let found = |url: &str| match bank_location(url) {
            Some(RevealTarget::File(path)) => path.canonicalize().ok(),
            _ => None,
        };
        assert_eq!(found(&url), Some(file.clone()), "the standard file URL");
        assert_eq!(
            found(&format!("file://{}", file.display())),
            Some(file.clone()),
            "the raw spelling"
        );

        let literal = folder.join("100%25.wav");
        std::fs::write(&literal, b"RIFF").unwrap();
        // And a file the decoded spelling would name, so both exist.
        std::fs::write(folder.join("100%.wav"), b"RIFF").unwrap();
        let literal = literal.canonicalize().unwrap();
        assert_eq!(
            found(&format!("file://{}", literal.display())),
            Some(literal),
            "a raw name holding a percent sign is that file, not its decoding"
        );
        // A URL with a host names no path on this machine. Not asked on
        // Windows, where the answer is a network share and the question a
        // network lookup.
        #[cfg(not(windows))]
        assert_eq!(bank_location("file://server/share/x.wav"), None);
    }

    #[test]
    fn a_banks_first_file_points_at_somewhere_a_person_can_look() {
        assert_eq!(
            bank_location(
                "https://raw.githubusercontent.com/tidalcycles/dirt-samples/master/bd/BT0A0A7.wav"
            ),
            Some(RevealTarget::Url(
                "https://github.com/tidalcycles/dirt-samples/tree/master/bd".into()
            ))
        );
        assert_eq!(
            bank_location("https://example.com/kits/909/bd.wav"),
            Some(RevealTarget::Url("https://example.com/kits/909/".into()))
        );
        assert_eq!(bank_location("file:///nowhere/at/all/bd.wav"), None);
        let here = std::env::current_dir().unwrap().join("Cargo.toml");
        assert_eq!(
            bank_location(&format!("file://{}", here.display())),
            Some(RevealTarget::File(here))
        );
        assert_eq!(bank_location("gm_piano"), None);
    }

    /// A pinned bank held on the strudel CDN is revealed at the repository
    /// the CDN mirrors, because the CDN refuses directory listings. The CDN
    /// folder is the branch's root, `tidal-drum-machines` keeps `machines/`
    /// in the file path, and VCSL arrives uppercase.
    #[test]
    fn a_cdn_bank_is_revealed_at_its_repository_not_at_a_forbidden_listing() {
        assert_eq!(
            bank_location("https://strudel.b-cdn.net/uzu-drumkit/brk/1_brk_switchangel.wav"),
            Some(RevealTarget::Url(
                "https://github.com/tidalcycles/uzu-drumkit/tree/main/brk".into()
            ))
        );
        assert_eq!(
            bank_location(
                "https://strudel.b-cdn.net/tidal-drum-machines/machines/RolandTR909/bd/BT0A0A7.wav"
            ),
            Some(RevealTarget::Url(
                "https://github.com/ritchse/tidal-drum-machines/tree/main/machines/RolandTR909/bd"
                    .into()
            ))
        );
        assert_eq!(
            bank_location("https://strudel.b-cdn.net/VCSL/Strings/x.wav"),
            Some(RevealTarget::Url(
                "https://github.com/sgossner/VCSL/tree/master/Strings".into()
            ))
        );
        assert_eq!(
            bank_location("https://strudel.b-cdn.net/Dirt-Samples/casio/high.wav"),
            Some(RevealTarget::Url(
                "https://github.com/tidalcycles/Dirt-Samples/tree/master/casio".into()
            ))
        );
        // A CDN address that is not one of the pinned collections is left
        // alone, and one of them with no file under it is not a listing
        // anybody asked for.
        assert_eq!(
            bank_location("https://strudel.b-cdn.net/otherwise/here.wav"),
            Some(RevealTarget::Url(
                "https://strudel.b-cdn.net/otherwise/".into()
            ))
        );
        assert_eq!(
            bank_location("https://strudel.b-cdn.net/uzu-drumkit/"),
            None
        );
    }

    #[test]
    fn a_missing_desktop_is_an_error_not_a_hang() {
        // A launcher that cannot exist: `start` fails at once, which `reveal`
        // reports as `NoDesktop`, and nothing is left to answer later.
        let (sender, receiver) = channel();
        let missing = Launch::Program {
            program: "rustel-no-such-desktop-command",
            args: vec!["https://example.com/kits/909/".into()],
        };
        assert!(start(missing, sender).is_err());
        assert!(
            receiver.try_recv().is_err(),
            "a launcher that never started sends no answer"
        );
        assert_eq!(
            RevealTarget::File(PathBuf::from("/x/y/take.wav")).describe(),
            "take.wav"
        );
    }

    #[test]
    fn explorer_paths_use_backslashes_and_drop_the_verbatim_prefix() {
        let verbatim = PathBuf::from(r"\\?\C:\Users\me\set\takes\take.wav");
        let text = explorer_path(&verbatim);
        let text = text.to_string_lossy();
        assert_eq!(text, r"C:\Users\me\set\takes\take.wav");
        assert!(!text.contains('/'), "{text}");
        assert!(!text.contains(r"\\?\"), "{text}");

        let forward = PathBuf::from(r"C:/Users/me/Documents/take.wav");
        let text = explorer_path(&forward);
        let text = text.to_string_lossy();
        assert_eq!(text, r"C:\Users\me\Documents\take.wav");
    }

    /// Headless, nothing is spawned and nothing is asked of the desktop:
    /// the reveal is already answered, so a test driving Alt+O gets the
    /// same status walk a desktop would give it - and no browser opens on
    /// the machine running the test.
    #[test]
    fn headless_reveal_answers_without_starting_anything() {
        // SAFETY: process-global environment, mutated only here, while this
        // test holds it; `cargo test` runs this crate's unit tests one at a
        // time unless told otherwise, and no other test reads this variable.
        unsafe { std::env::set_var(HEADLESS_ENV, "1") };
        assert!(headless());

        let pending = reveal(RevealTarget::Url(
            "https://github.com/tidalcycles/uzu-drumkit/tree/main/bd".into(),
        ))
        .expect("headless, there is no spawn to fail");
        assert_eq!(
            pending.target.describe(),
            "https://github.com/tidalcycles/uzu-drumkit/tree/main/bd"
        );
        // The answer is already in the pipe: the first poll is the one a
        // desktop would eventually send.
        assert_eq!(pending.poll(), Some(Ok(())));

        // Headless or not, an address that is not a web page is refused
        // before anything else is decided, and says why.
        match reveal(RevealTarget::Url("javascript:alert(1)".into())) {
            Err(RevealError::Refused(why)) => assert!(why.contains("javascript:"), "{why}"),
            Err(RevealError::NoDesktop(error)) => panic!("refused as a missing desktop: {error}"),
            Ok(_) => panic!("a javascript: address was handed to the desktop"),
        }

        unsafe { std::env::remove_var(HEADLESS_ENV) };
        assert!(!headless());
    }

    /// What each desktop is handed to open `address`: the one argv element of
    /// `open` or `xdg-open`, or the Windows shell's `open` verb - never a
    /// command line, and never cmd.
    fn opening(address: &str, desktop: Desktop) -> Launch {
        match desktop {
            Desktop::Mac => Launch::Program {
                program: "open",
                args: vec![address.into()],
            },
            Desktop::Windows => Launch::ShellOpen(address.to_owned()),
            Desktop::Freedesktop => Launch::Program {
                program: "xdg-open",
                args: vec![address.into()],
            },
        }
    }

    /// Every desktop is handed the URL parser's spelling of the address, whole,
    /// as the only thing it is asked to open: a space or a quote cannot split
    /// it, and whitespace around it is dropped. How each character is written
    /// is the fetch boundary's rule (`sample_fetch::web_address`) and is tested
    /// there; what the reveal adds is who gets the result, and how.
    #[test]
    fn a_url_reveal_hands_each_desktop_the_parsed_address_as_one_argument() {
        for (written, parsed) in [
            (
                "https://github.com/tidalcycles/uzu-drumkit/tree/main/bd",
                "https://github.com/tidalcycles/uzu-drumkit/tree/main/bd",
            ),
            (
                "HTTPS://Example.COM/kits/Crash 18\"/\n",
                "https://example.com/kits/Crash%2018%22/",
            ),
        ] {
            for desktop in Desktop::ALL {
                assert_eq!(
                    plan(&RevealTarget::Url(written.into()), desktop),
                    Ok(opening(parsed, desktop)),
                    "{written:?} on {desktop:?}"
                );
            }
        }
    }

    /// A `#` in a bank's folder is part of the folder's name, as it is when
    /// the bank's files are fetched. `https://host/kit #2/` read any other way
    /// is the page `kit ` with a fragment `2/`: the wrong folder, on the host
    /// and on GitHub alike.
    #[test]
    fn a_folder_with_a_hash_in_its_name_opens_that_folder() {
        for (file, folder, opened) in [
            (
                "https://host/kit #2/bd.wav",
                "https://host/kit #2/",
                "https://host/kit%20%232/",
            ),
            (
                "https://raw.githubusercontent.com/user/drums/main/kit #2/bd.wav",
                "https://github.com/user/drums/tree/main/kit #2",
                "https://github.com/user/drums/tree/main/kit%20%232",
            ),
        ] {
            let target = bank_location(file).expect(file);
            assert_eq!(target, RevealTarget::Url(folder.into()));
            for desktop in Desktop::ALL {
                assert_eq!(
                    plan(&target, desktop),
                    Ok(opening(opened, desktop)),
                    "{file} on {desktop:?}"
                );
            }
        }
    }

    /// A bank's address is score text. `&` and `%CMDCMDLINE:~-1%` are cmd
    /// syntax but only path characters to a browser, so they reach every
    /// desktop unchanged, as one argument and never on a command line.
    #[test]
    fn cmd_syntax_in_an_address_is_one_argument_never_a_command_line() {
        for written in [
            "https://evil.example/a&calc&/",
            "https://evil.example/%CMDCMDLINE:~-1%&calc&%CMDCMDLINE:~-1%/",
            "https://evil.example/%CMDCMDLINE:~0,1%&calc&%CMDCMDLINE:~0,1%/",
            "https://evil.example/a|calc|^&/",
        ] {
            assert_eq!(
                url::Url::parse(written).unwrap().as_str(),
                written,
                "the parser keeps this address as written"
            );
            for desktop in Desktop::ALL {
                assert_eq!(
                    plan(&RevealTarget::Url(written.into()), desktop),
                    Ok(opening(written, desktop)),
                    "{written} on {desktop:?}"
                );
            }
        }
    }

    /// Only a web page is opened. Any other scheme asks the desktop to run
    /// something - a program, a script, a settings page, an inline document -
    /// and text that names no scheme or no host is no address at all. Each
    /// refusal says what was wrong, on every desktop.
    #[test]
    fn only_an_http_address_is_handed_to_the_desktop() {
        for (written, says) in [
            ("file:///C:/Windows/System32/calc.exe", "a file: address"),
            ("javascript:alert(1)", "a javascript: address"),
            ("ms-settings:privacy", "a ms-settings: address"),
            (
                "data:text/html,<script>alert(1)</script>",
                "a data: address",
            ),
            ("calc.exe", "names no scheme"),
            ("https://", "names no host"),
        ] {
            for desktop in Desktop::ALL {
                let refusal = plan(&RevealTarget::Url(written.into()), desktop)
                    .expect_err(&format!("{written:?} on {desktop:?}"));
                assert!(refusal.contains(says), "{written:?}: {refusal}");
            }
        }
    }

    /// A file or a folder goes to the file manager as a path, one argument:
    /// Finder selects the file, Explorer selects it through its own
    /// `/select,` syntax, and Linux opens the folder that holds it.
    #[test]
    fn a_file_or_folder_reveal_hands_the_file_manager_one_path() {
        let file = PathBuf::from("/x/y/take.wav");
        let folder = PathBuf::from("/x/y");
        let program = |program, args: &[&str]| Launch::Program {
            program,
            args: args.iter().map(OsString::from).collect(),
        };
        assert_eq!(
            plan(&RevealTarget::File(file.clone()), Desktop::Mac),
            Ok(program("open", &["-R", "/x/y/take.wav"]))
        );
        assert_eq!(
            plan(&RevealTarget::File(file), Desktop::Freedesktop),
            Ok(program("xdg-open", &["/x/y"]))
        );
        assert_eq!(
            plan(&RevealTarget::Folder(folder.clone()), Desktop::Mac),
            Ok(program("open", &["/x/y"]))
        );
        assert_eq!(
            plan(&RevealTarget::Folder(folder), Desktop::Freedesktop),
            Ok(program("xdg-open", &["/x/y"]))
        );

        let windows = PathBuf::from(r"C:\Users\me\set\takes\take 1.wav");
        assert_eq!(
            plan(&RevealTarget::File(windows), Desktop::Windows),
            Ok(Launch::Explorer(
                r#"/select,"C:\Users\me\set\takes\take 1.wav""#.into()
            ))
        );
        assert_eq!(
            plan(
                &RevealTarget::Folder(PathBuf::from(r"C:/Users/me/set")),
                Desktop::Windows
            ),
            Ok(Launch::Explorer(r"C:\Users\me\set".into()))
        );
    }
}
