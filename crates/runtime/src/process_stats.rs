//! Rate-limited process CPU, memory and machine-wide CPU telemetry.
//!
//! The runtime needs four facts - this process's CPU share, the memory it
//! owns, its resident set, and the whole machine's CPU load - not a process
//! table, so each platform is read directly. Every probe may fail;
//! unavailable values remain `None`, missing machine CPU readings briefly
//! retain the last value, and none of it ever prevents playback or drawing.

use std::time::{Duration, Instant};

/// One sampled reading of this process, and of the machine it runs on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProcessStats {
    /// Share of total machine CPU capacity, 0..=100.
    ///
    /// This is `None` before the first interval completes or when the platform
    /// counter is unavailable.
    pub cpu_percent: Option<f32>,
    /// The whole machine's CPU load, 0..=100 - every process on it, not
    /// only this one. `None` before the first interval completes, or after
    /// three seconds without a valid reading. Brief gaps retain the last value.
    pub machine_cpu_percent: Option<f32>,
    /// Resident set size in bytes: every page of the process in RAM,
    /// including the executable's and shared libraries' file-backed pages
    /// (a GPU driver stack is tens of megabytes of them), and, on macOS,
    /// freed pages the allocator has marked reusable but the kernel has not
    /// yet taken back.
    pub resident_bytes: Option<u64>,
    /// Memory this process itself owns, the figure the platform's own
    /// monitor calls its memory: `phys_footprint` on macOS (Activity
    /// Monitor), the private working set on Windows (Task Manager),
    /// resident anonymous plus shared-memory pages on Linux. Unlike
    /// [`Self::resident_bytes`] it leaves out clean pages mapped from files,
    /// which the system can drop and reload at will.
    pub footprint_bytes: Option<u64>,
}

impl ProcessStats {
    /// The memory figure to show a person: the footprint where the platform
    /// reports one, the resident set otherwise.
    pub fn memory_bytes(&self) -> Option<u64> {
        self.footprint_bytes.or(self.resident_bytes)
    }
}

/// Minimum wall time between platform samples.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);
const MACHINE_CPU_GRACE: Duration = Duration::from_secs(3);

/// Rate-limited sampler holding the previous cumulative CPU-time reading.
#[derive(Debug)]
pub struct ProcessMonitor {
    cores: f64,
    last_sampled: Option<(Instant, Duration)>,
    /// Previous (idle, total) machine-wide tick counts, in whatever unit
    /// the platform counts in - only ever differenced against a later
    /// reading of its own kind, never read for its absolute value.
    last_machine_sampled: Option<(u64, u64)>,
    last_machine_valid: Option<Instant>,
    next_sample: Instant,
    latest: ProcessStats,
}

impl ProcessMonitor {
    pub fn new(now: Instant) -> Self {
        Self {
            cores: std::thread::available_parallelism()
                .map(|count| count.get() as f64)
                .unwrap_or(1.0),
            last_sampled: None,
            last_machine_sampled: None,
            last_machine_valid: None,
            next_sample: now,
            latest: ProcessStats::default(),
        }
    }

    /// Latest reading, resampling at most every 500 ms.
    pub fn sample(&mut self, now: Instant) -> ProcessStats {
        if now < self.next_sample {
            return self.latest;
        }
        self.next_sample = now + SAMPLE_INTERVAL;
        self.latest.resident_bytes = resident_bytes();
        self.latest.footprint_bytes = footprint_bytes();
        if let Some(cpu_time) = process_cpu_time() {
            if let Some((previous_wall, previous_cpu)) = self.last_sampled {
                let wall = now.saturating_duration_since(previous_wall).as_secs_f64();
                let used = cpu_time.saturating_sub(previous_cpu).as_secs_f64();
                if wall > 0.0 {
                    let share = used / (wall * self.cores.max(1.0)) * 100.0;
                    self.latest.cpu_percent = Some(share.clamp(0.0, 100.0) as f32);
                }
            }
            self.last_sampled = Some((now, cpu_time));
        }
        // Sampled on the same rate-limited tick as the process reading
        // above, rather than a timer of its own: the machine number is
        // only ever shown beside the process one, so there is no reason
        // for the two to fall out of step.
        self.sample_machine_cpu(now, machine_cpu_ticks());
        self.latest
    }

    fn sample_machine_cpu(&mut self, now: Instant, reading: Option<(u64, u64)>) {
        let (percent, previous) = step_machine_cpu(self.last_machine_sampled, reading);
        self.last_machine_sampled = previous;
        if let Some(percent) = percent {
            self.latest.machine_cpu_percent = Some(percent);
            self.last_machine_valid = Some(now);
        } else if self
            .last_machine_valid
            .is_none_or(|last| now.saturating_duration_since(last) >= MACHINE_CPU_GRACE)
        {
            self.latest.machine_cpu_percent = None;
        }
    }
}

/// One step of the machine-wide differencing: given the previous raw
/// reading, if any, and this tick's probe (which may have failed), returns
/// the percent to show and the reading to keep for next time.
///
/// Pure so the state machine - no previous reading yet, a reading that
/// failed, two readings with no time between them - can be tested without
/// a live machine; `machine_cpu_ticks` supplies the only platform-specific
/// half.
fn step_machine_cpu(
    previous: Option<(u64, u64)>,
    reading: Option<(u64, u64)>,
) -> (Option<f32>, Option<(u64, u64)>) {
    let Some(reading) = reading else {
        // Restart the raw comparison after a failure. The monitor separately
        // retains the last valid display value for a short grace period.
        return (None, None);
    };
    let percent = previous.and_then(|previous| machine_cpu_share(previous, reading));
    (percent, Some(reading))
}

/// Percent CPU busy across the whole machine between two (idle, total)
/// tick readings, in whatever unit the platform counts in - jiffies,
/// FILETIME ticks, Mach scheduler ticks. `None` when no time has passed
/// between the readings, which a stalled clock could otherwise turn into
/// a division by zero.
fn machine_cpu_share(previous: (u64, u64), current: (u64, u64)) -> Option<f32> {
    let idle_delta = current.0.saturating_sub(previous.0);
    let total_delta = current.1.saturating_sub(previous.1);
    if total_delta == 0 {
        return None;
    }
    let busy_delta = total_delta.saturating_sub(idle_delta);
    let share = busy_delta as f64 / total_delta as f64 * 100.0;
    Some(share.clamp(0.0, 100.0) as f32)
}

/// Total CPU time this process has consumed across all its threads.
#[cfg(unix)]
fn process_cpu_time() -> Option<Duration> {
    // SAFETY: `getrusage` writes a plain `rusage` through the pointer and
    // reads nothing else. The zeroed value is a valid initial state.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    if status != 0 {
        return None;
    }
    let convert = |time: libc::timeval| {
        Duration::from_secs(time.tv_sec.max(0) as u64)
            + Duration::from_micros(time.tv_usec.max(0) as u64)
    };
    Some(convert(usage.ru_utime) + convert(usage.ru_stime))
}

#[cfg(windows)]
fn process_cpu_time() -> Option<Duration> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that must not be
    // closed. `GetProcessTimes` writes four initialized FILETIME values and
    // retains none of their pointers.
    let status = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    if status == 0 {
        return None;
    }
    let ticks = filetime_ticks(kernel).saturating_add(filetime_ticks(user));
    Some(Duration::from_nanos(ticks.saturating_mul(100)))
}

#[cfg(windows)]
fn filetime_ticks(time: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

#[cfg(not(any(unix, windows)))]
fn process_cpu_time() -> Option<Duration> {
    None
}

/// Idle and total ticks for the whole machine, from the summary line
/// `/proc/stat` keeps ahead of its per-core ones.
#[cfg(target_os = "linux")]
fn machine_cpu_ticks() -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().next()?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values: Vec<u64> = fields.filter_map(|field| field.parse().ok()).collect();
    // user, nice, system, idle, iowait, irq, softirq, steal, guest,
    // guest_nice. iowait is the CPU waiting on disk rather than doing
    // anything, so it counts as idle for "how busy is the machine".
    let idle = *values.get(3)? + values.get(4).copied().unwrap_or(0);
    let total = values.iter().sum();
    Some((idle, total))
}

/// Idle and total ticks for the whole machine, from the host's aggregate
/// scheduler counters.
#[cfg(target_os = "macos")]
fn machine_cpu_ticks() -> Option<(u64, u64)> {
    // SAFETY: `host_statistics` fills `HOST_CPU_LOAD_INFO_COUNT` words of
    // the `host_cpu_load_info` it is handed, which is exactly the size of
    // the zeroed value below. `mach_host_self` acquires a send right; release
    // it after the call, including when the statistics query fails.
    unsafe {
        let mut info: libc::host_cpu_load_info = std::mem::zeroed();
        let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
        let host = mach2::mach_init::mach_host_self();
        let status = libc::host_statistics(
            host,
            libc::HOST_CPU_LOAD_INFO,
            std::ptr::addr_of_mut!(info).cast(),
            &mut count,
        );
        let _ = mach2::mach_port::mach_port_deallocate(mach2::traps::mach_task_self(), host);
        if status != libc::KERN_SUCCESS {
            return None;
        }
        let idle = u64::from(info.cpu_ticks[libc::CPU_STATE_IDLE as usize]);
        let total = info.cpu_ticks.iter().map(|&tick| u64::from(tick)).sum();
        Some((idle, total))
    }
}

/// Idle and total ticks for the whole machine, from the counters
/// `GetSystemTimes` aggregates across every processor.
#[cfg(windows)]
fn machine_cpu_ticks() -> Option<(u64, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetSystemTimes;

    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `GetSystemTimes` writes three initialized FILETIME values and
    // retains none of their pointers.
    let status = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
    if status == 0 {
        return None;
    }
    // Kernel time already includes idle time, so kernel + user is the
    // machine's whole busy-plus-idle total rather than double-counting it.
    let total = filetime_ticks(kernel).saturating_add(filetime_ticks(user));
    Some((filetime_ticks(idle), total))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn machine_cpu_ticks() -> Option<(u64, u64)> {
    None
}

#[cfg(target_os = "macos")]
fn resident_bytes() -> Option<u64> {
    // SAFETY: `task_info` fills `MACH_TASK_BASIC_INFO_COUNT` words of the
    // `mach_task_basic_info` it is handed, which is exactly the size of the
    // zeroed value below. `mach_task_self` borrows the current process task port.
    unsafe {
        let mut info: libc::mach_task_basic_info = std::mem::zeroed();
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
        let status = libc::task_info(
            mach2::traps::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            std::ptr::addr_of_mut!(info).cast(),
            &mut count,
        );
        // The struct is `repr(packed)`, so the field is read through a copy
        // rather than by reference.
        (status == libc::KERN_SUCCESS).then_some(info.resident_size)
    }
}

#[cfg(target_os = "linux")]
fn resident_bytes() -> Option<u64> {
    // Field two of `statm` is the resident set in pages.
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages = statm.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    // SAFETY: `sysconf` reads a process-wide constant and writes nothing.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size).ok()?;
    Some(pages.saturating_mul(page_size))
}

#[cfg(windows)]
fn resident_bytes() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?,
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    // SAFETY: the pseudo-handle is valid for the current process and needs no
    // close. The byte count exactly describes `counters`; the API retains no
    // pointer.
    let status = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    (status != 0).then_some(counters.WorkingSetSize as u64)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn resident_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
fn footprint_bytes() -> Option<u64> {
    // SAFETY: `proc_pid_rusage` fills the `rusage_info_v2` it is handed for
    // the flavor that names that struct, and retains no pointer. The zeroed
    // value is a valid initial state.
    unsafe {
        let mut info: libc::rusage_info_v2 = std::mem::zeroed();
        let status = libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V2,
            std::ptr::addr_of_mut!(info).cast(),
        );
        (status == 0).then_some(info.ri_phys_footprint)
    }
}

#[cfg(target_os = "linux")]
fn footprint_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    linux_footprint_kib(&status).map(|kib| kib.saturating_mul(1024))
}

/// Resident anonymous plus shared-memory pages, in KiB, from the text of
/// `/proc/<pid>/status`: what the process owns, where `VmRSS` also counts
/// the pages it maps from files.
#[cfg(any(target_os = "linux", test))]
fn linux_footprint_kib(status: &str) -> Option<u64> {
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kib| kib.parse::<u64>().ok())
    };
    // Kernels before 4.5 have no breakdown; the resident set stands in.
    let anonymous = field("RssAnon:")?;
    Some(anonymous.saturating_add(field("RssShmem:").unwrap_or(0)))
}

#[cfg(windows)]
fn footprint_bytes() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS_EX2 {
        cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>()).ok()?,
        ..PROCESS_MEMORY_COUNTERS_EX2::default()
    };
    // SAFETY: the pseudo-handle is valid for the current process and needs no
    // close. The byte count exactly describes `counters`, whose layout begins
    // with the plain counters the signature names; the API retains no pointer.
    let status = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            std::ptr::addr_of_mut!(counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    };
    // The resident set stands in when this counter is unavailable.
    (status != 0 && counters.PrivateWorkingSetSize != 0)
        .then_some(counters.PrivateWorkingSetSize as u64)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn footprint_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_sample_has_no_cpu_share_and_the_second_is_bounded() {
        let started = Instant::now();
        let mut monitor = ProcessMonitor::new(started);
        assert_eq!(monitor.sample(started).cpu_percent, None);

        let mut sink = 0u64;
        for value in 0..2_000_000u64 {
            sink = sink.wrapping_add(value.wrapping_mul(2_654_435_761));
        }
        assert_ne!(sink, u64::MAX);

        let later = started + SAMPLE_INTERVAL;
        let stats = monitor.sample(later.max(Instant::now()));
        if let Some(cpu) = stats.cpu_percent {
            assert!((0.0..=100.0).contains(&cpu), "cpu share was {cpu}");
        }
    }

    #[test]
    fn sampling_is_rate_limited_between_intervals() {
        let started = Instant::now();
        let mut monitor = ProcessMonitor::new(started);
        monitor.sample(started);
        let first = monitor.latest;
        assert_eq!(monitor.sample(started), first);
    }

    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    #[test]
    fn process_counters_are_available_on_supported_platforms() {
        assert!(process_cpu_time().is_some(), "process CPU time unavailable");
        let bytes = resident_bytes().expect("resident set size");
        assert!(bytes > 1024 * 1024, "implausible resident size {bytes}");
        let (idle, total) = machine_cpu_ticks().expect("machine CPU ticks unavailable");
        assert!(
            total >= idle,
            "idle ticks cannot exceed the machine's total"
        );
    }

    #[test]
    fn the_linux_footprint_is_anonymous_plus_shared_memory_not_file_pages() {
        let status = "Name:\trustel\nVmRSS:\t  243944 kB\nRssAnon:\t  148988 kB\nRssFile:\t   94000 kB\nRssShmem:\t     956 kB\nThreads:\t22\n";
        assert_eq!(linux_footprint_kib(status), Some(148_988 + 956));
        // A kernel with no breakdown has no footprint; the resident set
        // stands in for it.
        assert_eq!(linux_footprint_kib("VmRSS:\t 1000 kB\n"), None);
    }

    #[test]
    fn the_memory_shown_is_the_footprint_where_there_is_one() {
        let mut stats = ProcessStats {
            resident_bytes: Some(300),
            footprint_bytes: Some(120),
            ..ProcessStats::default()
        };
        assert_eq!(stats.memory_bytes(), Some(120));
        stats.footprint_bytes = None;
        assert_eq!(stats.memory_bytes(), Some(300));
        assert_eq!(ProcessStats::default().memory_bytes(), None);
    }

    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    #[test]
    fn the_displayed_memory_is_available_on_supported_platforms() {
        let now = Instant::now();
        let stats = ProcessMonitor::new(now).sample(now);
        #[cfg(not(windows))]
        assert!(stats.footprint_bytes.is_some(), "footprint unavailable");

        let displayed = stats.memory_bytes().expect("process memory");
        if let Some(footprint) = stats.footprint_bytes {
            assert_eq!(displayed, footprint);
        } else {
            assert_eq!(Some(displayed), stats.resident_bytes);
        }
        assert!(
            displayed > 1024 * 1024,
            "implausible memory size {displayed}"
        );
    }

    #[test]
    fn the_machine_share_is_none_before_the_first_reading() {
        // The first reading has no earlier reading to compare against, so it
        // reports nothing for the interval before it.
        let (percent, kept) = step_machine_cpu(None, Some((100, 1_000)));
        assert_eq!(percent, None);
        assert_eq!(kept, Some((100, 1_000)));
    }

    #[test]
    fn the_machine_share_is_none_after_a_failed_probe() {
        // A failed probe resets the raw comparison, independently of the
        // display's grace period.
        let (percent, kept) = step_machine_cpu(Some((100, 1_000)), None);
        assert_eq!(percent, None);
        assert_eq!(kept, None);

        // The reading after the gap starts a fresh pair, same as the
        // very first reading ever taken.
        let (percent, kept) = step_machine_cpu(kept, Some((140, 1_400)));
        assert_eq!(percent, None);
        assert_eq!(kept, Some((140, 1_400)));
    }

    #[test]
    fn the_machine_share_is_busy_ticks_over_total_ticks() {
        // Ten idle ticks out of a hundred total ticks elapsed is ninety
        // percent busy.
        let (percent, kept) = step_machine_cpu(Some((50, 500)), Some((60, 600)));
        assert_eq!(percent, Some(90.0));
        assert_eq!(kept, Some((60, 600)));

        // A quiet machine: almost every elapsed tick was idle.
        let (percent, _) = step_machine_cpu(Some((500, 1_000)), Some((598, 1_100)));
        assert_eq!(percent, Some(2.0));
    }

    #[test]
    fn the_machine_share_is_none_when_no_time_has_elapsed() {
        // Two readings with an identical total would divide by zero if
        // taken at face value; a stalled clock is not a machine at zero
        // percent busy, it is nothing to report.
        let (percent, kept) = step_machine_cpu(Some((10, 100)), Some((10, 100)));
        assert_eq!(percent, None);
        assert_eq!(kept, Some((10, 100)));
    }

    #[test]
    fn machine_cpu_display_debounces_missing_and_repeated_readings() {
        let now = Instant::now();
        let mut monitor = ProcessMonitor::new(now);
        monitor.sample_machine_cpu(now, None);
        assert_eq!(monitor.latest.machine_cpu_percent, None);
        monitor.sample_machine_cpu(now, Some((10, 100)));
        assert_eq!(monitor.latest.machine_cpu_percent, None);
        let good = now + SAMPLE_INTERVAL;
        monitor.sample_machine_cpu(good, Some((30, 200)));
        assert_eq!(monitor.latest.machine_cpu_percent, Some(80.0));
        monitor.sample_machine_cpu(good + SAMPLE_INTERVAL, Some((30, 200)));
        assert_eq!(monitor.latest.machine_cpu_percent, Some(80.0));
        monitor.sample_machine_cpu(good + SAMPLE_INTERVAL * 2, None);
        assert_eq!(monitor.latest.machine_cpu_percent, Some(80.0));
        monitor.sample_machine_cpu(good + MACHINE_CPU_GRACE - Duration::from_millis(1), None);
        assert_eq!(monitor.latest.machine_cpu_percent, Some(80.0));
        monitor.sample_machine_cpu(good + MACHINE_CPU_GRACE, None);
        assert_eq!(monitor.latest.machine_cpu_percent, None);
        monitor.sample_machine_cpu(good + MACHINE_CPU_GRACE + SAMPLE_INTERVAL, Some((50, 300)));
        assert_eq!(monitor.latest.machine_cpu_percent, None);
        let recovered = good + MACHINE_CPU_GRACE + SAMPLE_INTERVAL * 2;
        monitor.sample_machine_cpu(recovered, Some((140, 400)));
        assert_eq!(monitor.latest.machine_cpu_percent, Some(10.0));
        monitor.sample_machine_cpu(recovered + SAMPLE_INTERVAL, None);
        monitor.sample_machine_cpu(recovered + SAMPLE_INTERVAL * 2, Some((150, 500)));
        assert_eq!(monitor.latest.machine_cpu_percent, Some(10.0));
        monitor.sample_machine_cpu(recovered + SAMPLE_INTERVAL * 3, Some((150, 600)));
        assert_eq!(monitor.latest.machine_cpu_percent, Some(100.0));
    }

    #[cfg(windows)]
    #[test]
    fn filetime_combines_both_halves_as_windows_100ns_ticks() {
        use windows_sys::Win32::Foundation::FILETIME;

        assert_eq!(
            filetime_ticks(FILETIME {
                dwLowDateTime: 0x89ab_cdef,
                dwHighDateTime: 0x0123_4567,
            }),
            0x0123_4567_89ab_cdef
        );
    }
}
