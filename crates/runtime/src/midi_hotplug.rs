//! Keeping macOS's view of the MIDI ports current.
//!
//! CoreMIDI does not read the port list on request. The list is a snapshot
//! the framework builds on the process's first CoreMIDI call. The framework
//! then updates it from MIDIServer notifications, but only for a process
//! that holds a client, and only on a thread that runs a `CFRunLoop`. A
//! process with neither keeps its first list for as long as it lives.
//!
//! `midir` builds a fresh client for every enumeration, on the calling
//! thread, and none of those threads runs a loop. Without this module,
//! `MidiInput::ports()` returns the ports that existed at launch, and a
//! controller plugged in later is not listed.
//!
//! This module holds one client on one thread that pumps a run loop for the
//! life of the process. Every other thread's enumeration is then current,
//! including `midir`'s. No caller reads anything from this module.
//!
//! Everywhere else this is a no-op: ALSA and WinMM enumerate the devices
//! rather than a cache.

/// Start watching, once per process. Cheap and safe to call on every frame.
pub fn ensure_watching() {
    #[cfg(all(target_os = "macos", feature = "midi"))]
    macos::ensure_watching();
}

#[cfg(all(target_os = "macos", feature = "midi"))]
mod macos {
    use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
    use std::sync::OnceLock;

    static WATCHING: OnceLock<()> = OnceLock::new();

    /// How long one pump of the run loop may wait for a notification, and
    /// how long the thread sleeps instead when the mode has nothing to wait
    /// for: one cadence, so the two can never drift apart.
    const WATCH_CADENCE: std::time::Duration = std::time::Duration::from_millis(250);

    pub(super) fn ensure_watching() {
        WATCHING.get_or_init(|| {
            // A thread of its own: the run loop never returns, and the
            // studio's own main loop belongs to the terminal. Detached
            // deliberately - the client must outlive every enumeration,
            // and there is nothing to join at exit.
            let started = std::thread::Builder::new()
                .name("midi-hotplug".into())
                .spawn(|| {
                    // The client is what subscribes this process to
                    // MIDIServer's notifications; holding it is the whole
                    // point, so it is bound for the life of the thread.
                    let Ok(_client) = coremidi::Client::new("rustel-hotplug") else {
                        // No CoreMIDI: nothing to keep current, and the
                        // ports we can see are the ports there are.
                        return;
                    };
                    loop {
                        // Pumping the loop lets those notifications arrive.
                        // A bounded turn, not `run()`, so the thread never
                        // blocks inside one call.
                        //
                        // A mode with nothing to wait for returns `Finished`
                        // at once, and a bare loop would then spin a core.
                        // Sleep for the same cadence in that case.
                        let mode = unsafe { kCFRunLoopDefaultMode };
                        if CFRunLoop::run_in_mode(mode, WATCH_CADENCE.as_secs_f64(), false)
                            == objc2_core_foundation::CFRunLoopRunResult::Finished
                        {
                            std::thread::sleep(WATCH_CADENCE);
                        }
                    }
                })
                .is_ok();
            debug_assert!(started, "the hotplug watcher is not load-bearing");
        });
    }
}
