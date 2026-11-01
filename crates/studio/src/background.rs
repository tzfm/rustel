//! Studio housekeeping that stands below the audio path.
//!
//! On Linux the output callback usually runs without a real-time grant: a
//! terminal studio has no rtkit session to ask, and a small machine such as a
//! Raspberry Pi has none at all. Threads that serve the interface - the frame
//! loop, the catalogue worker, the device probes - then compete with the
//! callback as equals, and a busy set spends its cores on repaints while a
//! buffer misses its deadline. Demoting those threads costs nothing when
//! cores are free and hands the core to the callback exactly when it needs it.

/// Stand below the audio path. Call once from the thread being demoted;
/// raising a thread's niceness needs no privilege.
pub(crate) fn demote() {
    #[cfg(target_os = "linux")]
    // SAFETY: the call takes constant arguments and touches no memory. An
    // error, such as a system without niceness, changes nothing.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cfg(target_os = "linux")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demotion_lowers_this_threads_niceness() {
        demote();
        let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
        assert!(nice >= 9, "niceness stayed at {nice}");
    }
}
