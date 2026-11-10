//! The watch of the preset folders: the events of the file system, on a
//! thread of its own, tell the host a preset came in or went out, so the
//! preset lists count a file the player saves while the studio runs, with
//! no read the player asks for and no poll of the disk. One read of the
//! folders follows each burst of events, after it goes quiet.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher};

/// What the events of a watch say to its thread, and what the host says
/// back: a file changed, or the watch ends.
pub enum Word {
    /// An event of the folders. The read waits for the quiet behind the
    /// events, so a save that says itself twice reads once.
    Changed,
    /// The host took a watch of other folders: this one ends.
    Stop,
}

/// The read waits for this long with no event behind it: the write of one
/// file says the file and the folder both, and a save says more.
const QUIET: Duration = Duration::from_millis(400);

/// True when the event says a file came in, changed or went out. A read of
/// a file says itself too, and reads count nothing.
fn counts(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// The path a watch of `folder` reads: `folder` when it is there, else the
/// folder nearest above it that is.
pub fn watched_at(folder: &Path) -> PathBuf {
    let mut at = folder;
    while !at.exists()
        && let Some(above) = at.parent()
        && above != at
    {
        at = above;
    }
    at.to_path_buf()
}

/// The watch of `folder`: the folder and everything under it, for a
/// preset folder holds a folder for each plugin, and a folder the player
/// added holds any structure at all. A folder that is missing watches the
/// folder nearest above it, that folder alone: its event says the folder
/// came, and the read that follows gives the folder a watch of its own. A
/// recursive watch there would read the whole tree, the user's home at
/// worst.
fn watch_of(folder: &Path) -> (PathBuf, RecursiveMode) {
    let at = watched_at(folder);
    let mode = if at == folder {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    (at, mode)
}

/// Watches `folders`. Once the events went quiet for [`QUIET`], the thread
/// calls `changed` once, on a thread of its own. The thread ends when the
/// answer says [`Word::Stop`], when the host that asked is gone - `alive`
/// says so - or when its channel is gone, which a dropped host does. The
/// answer is the word to the thread: `None` when the system gives no watch,
/// and the preset folders stay as they were, read when the player asks.
pub fn watch_folders(
    folders: Vec<PathBuf>,
    alive: impl Fn() -> bool + Send + 'static,
    changed: impl Fn() + Send + 'static,
) -> Option<Sender<Word>> {
    let (words, word) = mpsc::channel::<Word>();
    let answer = words.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && counts(event.kind)
        {
            let _ = words.send(Word::Changed);
        }
    })
    .ok()?;
    let watches: Vec<_> = folders.iter().map(|folder| watch_of(folder)).collect();
    for (at, mode) in &watches {
        // A path a whole-folder watch already reads takes no watch of its
        // own: a second watch of one path replaces the first.
        let read = matches!(mode, RecursiveMode::NonRecursive)
            && watches.iter().any(|(whole, other)| {
                matches!(other, RecursiveMode::Recursive) && at.starts_with(whole)
            });
        if !read {
            let _ = watcher.watch(at, *mode);
        }
    }
    std::thread::Builder::new()
        .name("rustel-vst3-watch".into())
        .spawn(move || {
            // The watch ends with the thread: the watcher lives here.
            let _watcher = watcher;
            let mut armed = false;
            loop {
                match word.recv_timeout(QUIET) {
                    Ok(Word::Changed) => armed = true,
                    Ok(Word::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => {
                        if armed {
                            armed = false;
                            changed();
                        } else if !alive() {
                            return;
                        }
                    }
                }
            }
        })
        .expect("watch thread");
    Some(answer)
}
