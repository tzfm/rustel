//! Return the allocator's free memory to the operating system.
//!
//! A freed buffer does not go back to the system. glibc keeps a freed
//! block in the arena it came from, and each of the six sample loaders
//! fills its own arena. libmalloc keeps freed regions mapped until the
//! kernel asks for them. So a studio that drops its decoded previews stays
//! resident near its peak. Measured on Linux: about 100 MiB of a 238 MiB
//! resident set was freed memory, and one trim returned it.
//!
//! A trim locks each arena in turn, so a thread that allocates at that
//! time waits. Callers trim only at quiet moments and never from the audio
//! callback, which does not allocate.
//!
//! A trim releases the free blocks inside an arena, but not the free space
//! at the end of a thread's arena, and freed sample data is there. `free`
//! shrinks that space only above a threshold. glibc raises both thresholds
//! each time a large buffer is freed: up to 32 MiB for a buffer to get its
//! own mapping, and up to 64 MiB of free space for an arena to shrink. An
//! arena is 64 MiB at most, so that threshold can be the whole arena.
//!
//! [`fix_release_thresholds`] keeps both thresholds at their initial
//! value. A large buffer is then its own mapping, and its memory returns
//! when the buffer is freed. On a twelve-voice score, playing peaks at
//! 138 MiB in place of 189 MiB, for the same processor time. The command
//! line uses the same loaders and never trims, so it needs the fixed
//! thresholds too.

/// Whether [`release_free_memory`] has an allocator call to make: `true`
/// on Linux with glibc and on macOS. The Windows heap returns a large
/// block when it is freed, and has nothing more to return on request.
pub const RELEASES_FREE_MEMORY: bool = cfg!(any(
    all(target_os = "linux", target_env = "gnu"),
    target_os = "macos"
));

/// Keep both glibc release thresholds fixed at their initial 128 KiB. A
/// buffer of that size or more is then its own mapping, returned when it
/// is freed, and an arena shrinks when 128 KiB at its end is free. Setting
/// either threshold also stops glibc from raising both.
///
/// Call once at program start, before any thread allocates. The
/// thresholds belong to the whole process, so the program makes this call
/// and the library never does. Returns `true` when glibc accepted both
/// values, and `false` on other platforms.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn fix_release_thresholds() -> bool {
    // The glibc default for both thresholds.
    const THRESHOLD_BYTES: libc::c_int = 128 * 1024;
    // SAFETY: `mallopt` takes the allocator's lock itself and changes only
    // how later calls behave. It returns 0 for a value it refuses.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, THRESHOLD_BYTES) == 1
            && libc::mallopt(libc::M_TRIM_THRESHOLD, THRESHOLD_BYTES) == 1
    }
}

/// Keep the glibc release thresholds fixed. This platform has none, so
/// the call does nothing and returns `false`.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn fix_release_thresholds() -> bool {
    false
}

/// Ask the allocator to return its free pages to the operating system.
///
/// Best effort: `true` where the platform allocator has such a call, `false`
/// where there is nothing to ask.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn release_free_memory() -> bool {
    // SAFETY: `malloc_trim` takes each arena's lock itself and touches only
    // memory the allocator owns.
    unsafe {
        libc::malloc_trim(0);
    }
    true
}

/// Ask the allocator to return its free pages to the operating system.
///
/// Best effort: `true` where the platform allocator has such a call, `false`
/// where there is nothing to ask.
#[cfg(target_os = "macos")]
pub fn release_free_memory() -> bool {
    unsafe extern "C" {
        // libmalloc, part of libSystem since macOS 10.7: a null zone means
        // every zone, and a zero goal means as much as can be released.
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }
    // SAFETY: a null zone and a zero goal are the documented "all zones, as
    // much as possible" arguments; the call takes the zones' locks itself.
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
    true
}

/// Ask the allocator to return its free pages to the operating system.
///
/// Best effort: `true` where the platform allocator has such a call, `false`
/// where there is nothing to ask.
#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
pub fn release_free_memory() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // What a trim gives back depends on how the allocator's arenas happen
    // to be laid out - whether a freed block borders the top of its arena,
    // which a shared test binary does not control - so the effect is
    // measured on the running studio (the figures above), not asserted here.
    #[test]
    fn releasing_free_memory_is_safe_to_ask_for_at_any_time() {
        let supported = RELEASES_FREE_MEMORY;
        assert_eq!(release_free_memory(), supported);
        // Twice in a row, from another thread, and around live allocations,
        // changes nothing a caller can see.
        let held = vec![7u8; 1 << 20];
        let other = std::thread::spawn(release_free_memory);
        assert_eq!(release_free_memory(), supported);
        assert_eq!(other.join().unwrap(), supported);
        assert!(held.iter().all(|&byte| byte == 7));
    }

    /// The resident set of this process, in bytes.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn resident_bytes() -> usize {
        let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
        let pages: usize = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
        // SAFETY: `sysconf` reads a constant of the running system.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        pages * usize::try_from(page).unwrap()
    }

    // With fixed thresholds, a thread's freed 16 MiB buffer leaves the
    // resident set. With moving thresholds, the second buffer stays in the
    // arena. The thresholds are per process, so a child process runs this.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn fixed_thresholds_give_back_a_threads_large_buffers() {
        const CHILD: &str = "RUSTEL_FREE_MEMORY_TEST_CHILD";
        const BUFFER_BYTES: usize = 16 << 20;
        if std::env::var_os(CHILD).is_none() {
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "free_memory::tests::fixed_thresholds_give_back_a_threads_large_buffers",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                child.status.success(),
                "{}{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );
            return;
        }
        assert!(fix_release_thresholds());
        let kept = std::thread::spawn(|| {
            let before = resident_bytes();
            for _ in 0..2 {
                // The fill writes each page: untouched pages are not resident.
                let buffer = std::hint::black_box(vec![1u8; BUFFER_BYTES]);
                assert!(resident_bytes() >= before + BUFFER_BYTES / 2);
                drop(buffer);
            }
            resident_bytes().saturating_sub(before)
        })
        .join()
        .unwrap();
        assert!(
            kept < BUFFER_BYTES / 4,
            "{kept} bytes of a freed {BUFFER_BYTES}-byte buffer are still resident"
        );
    }
}
