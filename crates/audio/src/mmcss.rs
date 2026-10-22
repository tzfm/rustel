//! MMCSS "Pro Audio" scheduling on Windows.
//!
//! The thread that renders the output stream is the one thread the whole
//! product waits on: a late callback is a dropout. Windows schedules a
//! thread registered with the Multimedia Class Scheduler Service's
//! "Pro Audio" task above everything ordinary (Reaper's audio-thread
//! dropdown calls the same thing "MMCSS Pro Audio / Time Critical"; cubeb
//! does it for Firefox), and CPAL leaves its WASAPI thread at the normal
//! priority. So the callback registers itself the first time it runs.
//!
//! The studio's engine thread (score evaluation, scheduling, pad presses)
//! takes the same class one step below. Its cadence survives a busy machine,
//! but it never outranks the thread that feeds the device. With the critical
//! priority on the engine thread and none on the render thread, a long
//! evaluation could starve the callback on the same core.
//!
//! A plugin worker takes the critical priority of the render thread, because
//! the render thread waits for its jobs. A worker whose
//! `AvSetMmThreadPriority` fails takes no job. Each worker waits for its
//! next job on a kernel event of its own (`CreateEventW`,
//! `WaitForSingleObject`), and the render thread wakes it with `SetEvent`, a
//! kernel call with no lock in user mode. A worker reads its cycles
//! (`QueryThreadCycleTime`) around a job, so the pool sees a job in which
//! the system did not run the worker. The cycles count at a fixed rate only
//! on an x86 or x64 machine. `IsWow64Process2` gives the native machine of
//! the process, also for an x64 build in emulation on Arm.
//!
//! The plugin pool counts the performance cores of the process:
//! `GetLogicalProcessorInformationEx` lists each core and its efficiency
//! class, and `GetProcessAffinityMask` gives the logical processors the
//! process runs on. On a CPU with two kinds of cores, each worker selects
//! the CPU sets of the performance cores (`GetSystemCpuSetInformation`,
//! `SetThreadSelectedCpuSets`), so a job does not run on an efficiency core
//! while the render thread waits for it. The CPU sets of the process
//! (`GetProcessDefaultCpuSets`), which a tool or the user sets, limit that
//! choice.

use std::mem::offset_of;
#[cfg(not(miri))]
use std::time::Duration;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::SystemInformation::{
    CpuSetInformation, GetLogicalProcessorInformationEx, GetSystemCpuSetInformation,
    IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_I386, PROCESSOR_RELATIONSHIP,
    RelationProcessorCore, SYSTEM_CPU_SET_INFORMATION, SYSTEM_CPU_SET_INFORMATION_0_0,
    SYSTEM_CPU_SET_INFORMATION_ALLOCATED, SYSTEM_CPU_SET_INFORMATION_ALLOCATED_TO_TARGET_PROCESS,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows_sys::Win32::System::Threading::{
    AVRT_PRIORITY, AVRT_PRIORITY_CRITICAL, AVRT_PRIORITY_NORMAL, AvRevertMmThreadCharacteristics,
    AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority, GetCurrentProcess, GetCurrentThread,
    GetCurrentThreadId, GetProcessAffinityMask, GetProcessDefaultCpuSets, IsWow64Process2,
    SetThreadSelectedCpuSets,
};
#[cfg(not(miri))]
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForSingleObject},
    System::WindowsProgramming::QueryThreadCycleTime,
};

/// The calling thread's "Pro Audio" registration, reverted on drop by the
/// thread that made it.
///
/// Best-effort: where MMCSS is absent (a server SKU without the service, a
/// sandbox) `attach` answers `None` and the thread keeps its priority.
pub struct ProAudioThread {
    handle: HANDLE,
    /// The registering thread, as `GetCurrentThreadId` names it.
    thread: u32,
}

impl ProAudioThread {
    /// "Pro Audio", NUL-terminated UTF-16, as `AvSetMmThreadCharacteristicsW`
    /// wants it.
    const TASK: [u16; 10] = [
        b'P' as u16,
        b'r' as u16,
        b'o' as u16,
        b' ' as u16,
        b'A' as u16,
        b'u' as u16,
        b'd' as u16,
        b'i' as u16,
        b'o' as u16,
        0,
    ];

    /// Register the calling thread at `priority` within the "Pro Audio"
    /// task. No heap allocation: safe to call once from an audio callback.
    pub fn attach(priority: AVRT_PRIORITY) -> Option<Self> {
        let thread = Self::register()?;
        thread.set_priority(priority);
        Some(thread)
    }

    /// The render thread's registration: nothing ordinary outranks it.
    pub fn attach_critical() -> Option<Self> {
        Self::attach(AVRT_PRIORITY_CRITICAL)
    }

    /// A feeder thread's registration: above the desktop, below the
    /// render thread.
    pub fn attach_normal() -> Option<Self> {
        Self::attach(AVRT_PRIORITY_NORMAL)
    }

    /// A plugin worker's registration: the priority of the render thread,
    /// which waits for its jobs. `None` when the service refuses the task
    /// or the priority.
    pub fn attach_worker() -> Option<Self> {
        let thread = Self::register()?;
        thread
            .set_priority(AVRT_PRIORITY_CRITICAL)
            .then_some(thread)
    }

    /// Registers the calling thread in the "Pro Audio" task.
    fn register() -> Option<Self> {
        let mut task_index: u32 = 0;
        // SAFETY: the task name is a NUL-terminated UTF-16 array that
        // outlives the call, and `task_index` is a valid out-pointer.
        let handle = unsafe { AvSetMmThreadCharacteristicsW(Self::TASK.as_ptr(), &mut task_index) };
        if handle.is_null() {
            return None;
        }
        Some(Self {
            handle,
            thread: current_thread(),
        })
    }

    /// Sets the priority within the task. False when the service refuses
    /// it.
    fn set_priority(&self, priority: AVRT_PRIORITY) -> bool {
        // SAFETY: `handle` came from the characteristics call of this
        // registration, which the drop has not reverted.
        unsafe { AvSetMmThreadPriority(self.handle, priority) != 0 }
    }

    /// Revert the registration when `current` is the thread that made it;
    /// answers whether it did.
    ///
    /// A registration only ever reaches another thread through a wrapper
    /// that promises it is dropped where it was made (the output callback's,
    /// which rests on CPAL dropping the closure on its render thread).
    /// Should that promise break, reverting from elsewhere would act on a
    /// registration the dropping thread does not own, so the revert is
    /// skipped and the registration is left to the thread that made it.
    fn release(&mut self, current: u32) -> bool {
        if current != self.thread {
            return false;
        }
        // SAFETY: `handle` came from `AvSetMmThreadCharacteristicsW` on the
        // thread that is now reverting it, as just checked, and `Drop` is
        // the only caller outside tests: reverted once.
        unsafe {
            AvRevertMmThreadCharacteristics(self.handle);
        }
        true
    }
}

/// The calling thread's id. A read of the thread's own environment block:
/// no allocation, no lock, safe on the audio thread.
fn current_thread() -> u32 {
    // SAFETY: `GetCurrentThreadId` takes no arguments and cannot fail.
    unsafe { GetCurrentThreadId() }
}

impl Drop for ProAudioThread {
    fn drop(&mut self) {
        self.release(current_thread());
    }
}

/// Each physical core of the system, with its efficiency class and the
/// logical processors it holds. `None` when the call fails.
pub fn processor_cores() -> Option<Vec<PROCESSOR_RELATIONSHIP>> {
    let mut length = 0u32;
    // SAFETY: with no buffer, the call writes the length it needs.
    unsafe {
        GetLogicalProcessorInformationEx(RelationProcessorCore, std::ptr::null_mut(), &mut length)
    };
    // Words of 8 bytes keep each record aligned.
    let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
    // SAFETY: the buffer holds `length` bytes at least.
    let read = unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            buffer.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if read == 0 {
        return None;
    }
    let bytes = buffer.as_ptr().cast::<u8>();
    let mut cores = Vec::new();
    let mut at = 0;
    while at < length as usize {
        // SAFETY: each record starts inside the `length` bytes the call
        // wrote, and a record of a core has a processor relationship after
        // its header. The reads need no alignment.
        let (size, core) = unsafe {
            let record = bytes.add(at);
            let size = offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Size);
            let body = offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Anonymous);
            (
                record.add(size).cast::<u32>().read_unaligned(),
                record
                    .add(body)
                    .cast::<PROCESSOR_RELATIONSHIP>()
                    .read_unaligned(),
            )
        };
        if size == 0 {
            break;
        }
        cores.push(core);
        at += size as usize;
    }
    Some(cores)
}

/// The physical cores of the highest efficiency class that the process runs
/// on: the performance cores of a CPU with two kinds of cores, or each core
/// of another CPU. `None` when the system does not say.
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub(crate) fn performance_cores() -> Option<usize> {
    let process = process_affinity()?;
    let cores = processor_cores()?;
    let top = cores.iter().map(|core| core.EfficiencyClass).max()?;
    // A core sits in one processor group. A process with threads in several
    // groups gets mask 0, and each core counts.
    let allowed = cores.iter().filter(|core| {
        core.EfficiencyClass == top && (process == 0 || core.GroupMask[0].Mask & process != 0)
    });
    Some(allowed.count())
}

/// The logical processors the process runs on, in its processor group. A
/// process with threads in several groups gets mask 0. `None` when the call
/// fails.
fn process_affinity() -> Option<usize> {
    let (mut process, mut system) = (0, 0);
    // SAFETY: the pseudo handle of the current process needs no close, and
    // both masks are valid out-pointers.
    let read = unsafe { GetProcessAffinityMask(GetCurrentProcess(), &mut process, &mut system) };
    (read != 0).then_some(process)
}

/// Keeps the calling thread on the performance cores of a CPU with two kinds
/// of cores: the thread selects their CPU sets, among the CPU sets of the
/// process when it has some. On a CPU with one kind of core, or when the CPU
/// sets of the process hold no performance core, the thread keeps the CPU
/// sets of the process. A plugin worker calls this at its start.
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub(crate) fn select_performance_cores() {
    let Some((process, sets)) = process_affinity().zip(cpu_sets()) else {
        return;
    };
    // Each id of the CPU sets of the process names one set of the system.
    let Some(defaults) = process_cpu_sets(sets.len()) else {
        return;
    };
    let selected = performance_cpu_sets(&sets, process, &defaults);
    if !selected.is_empty() {
        select_cpu_sets(&selected);
    }
}

/// The calling thread selects the CPU sets `ids`.
fn select_cpu_sets(ids: &[u32]) {
    // SAFETY: the pseudo handle of the calling thread needs no close, and
    // `ids` holds the count of ids the call reads.
    unsafe { SetThreadSelectedCpuSets(GetCurrentThread(), ids.as_ptr(), ids.len() as u32) };
}

/// The CPU sets of the process: the sets each of its threads runs on when
/// the thread selects none. A tool or the user sets them. Empty when the
/// process has none. `None` when the call fails.
fn process_cpu_sets(capacity: usize) -> Option<Vec<u32>> {
    let mut ids = vec![0u32; capacity];
    let mut count = 0u32;
    // SAFETY: the pseudo handle of the current process needs no close, and
    // `ids` holds the `capacity` ids the call writes at most.
    let read = unsafe {
        GetProcessDefaultCpuSets(
            GetCurrentProcess(),
            ids.as_mut_ptr(),
            ids.len() as u32,
            &mut count,
        )
    };
    ids.truncate(count as usize);
    (read != 0).then_some(ids)
}

/// The CPU sets a plugin worker selects, from each CPU set of the system,
/// the affinity mask `process` of the process and the CPU sets `defaults` of
/// the process: each set of the highest efficiency class in the mask, except
/// a set the system holds for another process. A process with CPU sets
/// limits the choice to them. Empty on a CPU with one kind of core.
fn performance_cpu_sets(
    sets: &[SYSTEM_CPU_SET_INFORMATION_0_0],
    process: usize,
    defaults: &[u32],
) -> Vec<u32> {
    let top = sets
        .iter()
        .map(|set| set.EfficiencyClass)
        .max()
        .unwrap_or(0);
    if sets.iter().all(|set| set.EfficiencyClass == top) {
        return Vec::new();
    }
    sets.iter()
        .filter(|set| {
            // SAFETY: each field of the union is the byte of the flags, and
            // each byte is a valid `u8`.
            let flags = u32::from(unsafe { set.Anonymous1.AllFlags });
            let other_process = flags & SYSTEM_CPU_SET_INFORMATION_ALLOCATED != 0
                && flags & SYSTEM_CPU_SET_INFORMATION_ALLOCATED_TO_TARGET_PROCESS == 0;
            // As for a core, mask 0 lets each set count. A group holds 64
            // logical processors at most.
            let allowed = process == 0 || (process as u64 >> set.LogicalProcessorIndex) & 1 != 0;
            // A process with no CPU sets lets each set count.
            let default = defaults.is_empty() || defaults.contains(&set.Id);
            set.EfficiencyClass == top && allowed && default && !other_process
        })
        .map(|set| set.Id)
        .collect()
}

/// Each CPU set of the system: one logical processor, as the calls on CPU
/// sets name it. `None` when the call fails.
fn cpu_sets() -> Option<Vec<SYSTEM_CPU_SET_INFORMATION_0_0>> {
    // With the process, the call marks the sets the system holds for it.
    // SAFETY: the pseudo handle of the current process needs no close.
    let process = unsafe { GetCurrentProcess() };
    let mut length = 0u32;
    // SAFETY: with no buffer, the call writes the length it needs.
    unsafe { GetSystemCpuSetInformation(std::ptr::null_mut(), 0, &mut length, process, 0) };
    // Words of 8 bytes keep each record aligned.
    let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
    // SAFETY: the buffer holds `length` bytes at least.
    let read = unsafe {
        GetSystemCpuSetInformation(buffer.as_mut_ptr().cast(), length, &mut length, process, 0)
    };
    if read == 0 {
        return None;
    }
    let bytes = buffer.as_ptr().cast::<u8>();
    let mut sets = Vec::new();
    let mut at = 0;
    while at + size_of::<SYSTEM_CPU_SET_INFORMATION>() <= length as usize {
        // SAFETY: the record lies inside the `length` bytes the call wrote,
        // and the read needs no alignment.
        let record = unsafe {
            bytes
                .add(at)
                .cast::<SYSTEM_CPU_SET_INFORMATION>()
                .read_unaligned()
        };
        if record.Size == 0 {
            break;
        }
        if record.Type == CpuSetInformation {
            // SAFETY: a record of this type holds a CPU set.
            sets.push(unsafe { record.Anonymous.CpuSet });
        }
        at += record.Size as usize;
    }
    Some(sets)
}

/// The cycles the calling thread ran, in user and kernel mode. A kernel
/// call with no lock in user mode. `None` when the call fails.
#[cfg(not(miri))]
pub(crate) fn thread_cycles() -> Option<u64> {
    let mut cycles = 0;
    // SAFETY: the pseudo handle of the calling thread needs no close, and
    // `cycles` is a valid out-pointer.
    let read = unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) };
    (read != 0).then_some(cycles)
}

/// True when the cycles of a thread count at a fixed rate, so a count of
/// cycles gives a time. The native machine of the process decides: an x64
/// build in emulation on Arm runs on an Arm machine.
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub(crate) fn cycles_at_fixed_rate() -> bool {
    native_machine().is_some_and(fixed_rate_cycles)
}

/// The native machine of the process, also for an x64 build in emulation on
/// Arm. `None` when the call fails.
fn native_machine() -> Option<IMAGE_FILE_MACHINE> {
    let (mut process, mut native) = (0, 0);
    // SAFETY: the pseudo handle of the current process needs no close, and
    // both machines are valid out-pointers.
    let read = unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) };
    (read != 0).then_some(native)
}

/// True when the cycles of a thread count at a fixed rate on the `native`
/// machine: an x86 or x64 machine. The cycle counter of a thread on Arm has
/// no fixed rate.
fn fixed_rate_cycles(native: IMAGE_FILE_MACHINE) -> bool {
    matches!(native, IMAGE_FILE_MACHINE_AMD64 | IMAGE_FILE_MACHINE_I386)
}

/// The wake of one plugin worker: a kernel event that resets itself when a
/// wait returns. A signal before the wait stays set, so no signal is lost.
/// Miri runs no Windows call: under Miri a worker waits in the park of the
/// standard library.
#[cfg(not(miri))]
pub(crate) struct WorkerWake(HANDLE);

// SAFETY: an event handle is valid on each thread of the process, and the
// kernel orders the calls on one event.
#[cfg(not(miri))]
unsafe impl Send for WorkerWake {}
#[cfg(not(miri))]
unsafe impl Sync for WorkerWake {}

#[cfg(not(miri))]
impl WorkerWake {
    /// `None` when the system makes no event.
    pub(crate) fn new() -> Option<Self> {
        // SAFETY: an event with no security attributes and no name, which
        // resets itself and starts unset.
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        (!event.is_null()).then_some(Self(event))
    }

    pub(crate) fn signal(&self) {
        // SAFETY: the handle is open until the drop.
        unsafe { SetEvent(self.0) };
    }

    /// Waits for a signal, `timeout` at most. The timeout rounds up to
    /// whole milliseconds, so a short wait does not spin.
    pub(crate) fn wait(&self, timeout: Option<Duration>) {
        let millis = timeout.map_or(INFINITE, |timeout| {
            u32::try_from(timeout.as_micros().div_ceil(1_000)).unwrap_or(INFINITE - 1)
        });
        // SAFETY: the handle is open until the drop.
        unsafe { WaitForSingleObject(self.0, millis) };
    }
}

#[cfg(not(miri))]
impl Drop for WorkerWake {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateEventW`. Each worker holds the
        // pool that holds its wake, so no thread waits on the event when it
        // closes.
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::SystemInformation::{
        IMAGE_FILE_MACHINE_ARM64, IMAGE_FILE_MACHINE_ARMNT, IMAGE_FILE_MACHINE_UNKNOWN,
        SYSTEM_CPU_SET_INFORMATION_0_0_0,
    };
    use windows_sys::Win32::System::Threading::GetThreadSelectedCpuSets;

    /// Registration and revert do not fault, whichever way the service
    /// answers on this machine (a hosted runner may lack MMCSS: that is
    /// `None`, not a failure).
    #[test]
    fn registration_attaches_and_reverts_without_faulting() {
        let render = ProAudioThread::attach_critical();
        let feeder = ProAudioThread::attach_normal();
        let worker = ProAudioThread::attach_worker();
        drop(worker);
        drop(feeder);
        drop(render);
        // The process runs on one core of the top class at least.
        assert!(performance_cores().is_some_and(|cores| cores >= 1));
    }

    /// A worker selects each CPU set of the highest efficiency class in the
    /// affinity mask and the CPU sets of the process, except a set the
    /// system holds for another process. A CPU with one kind of core gives
    /// no selection. Each logical processor of this machine is one CPU set
    /// of its class. A thread holds the CPU sets it selects.
    #[test]
    fn a_worker_selects_the_cpu_sets_of_the_performance_cores() {
        // The ids of the CPU sets start at 256, one for each logical
        // processor.
        let set = |id: u32, class: u8, flags: u32| SYSTEM_CPU_SET_INFORMATION_0_0 {
            Id: id,
            LogicalProcessorIndex: (id - 256) as u8,
            EfficiencyClass: class,
            Anonymous1: SYSTEM_CPU_SET_INFORMATION_0_0_0 {
                AllFlags: flags as u8,
            },
            ..Default::default()
        };
        // 2 performance cores with 2 hardware threads each, then 2
        // efficiency cores.
        let hybrid = [
            set(256, 1, 0),
            set(257, 1, 0),
            set(258, 1, 0),
            set(259, 1, 0),
            set(260, 0, 0),
            set(261, 0, 0),
        ];
        assert_eq!(performance_cpu_sets(&hybrid, 0, &[]), [256, 257, 258, 259]);
        assert_eq!(
            performance_cpu_sets(&hybrid, 0b11_1111, &[]),
            [256, 257, 258, 259]
        );
        // The affinity mask of the process limits the choice.
        assert_eq!(performance_cpu_sets(&hybrid, 0b11_0110, &[]), [257, 258]);
        // The CPU sets of the process limit the choice. With no performance
        // core among them, the worker keeps them.
        assert_eq!(
            performance_cpu_sets(&hybrid, 0, &[257, 258, 260]),
            [257, 258]
        );
        assert!(performance_cpu_sets(&hybrid, 0, &[260, 261]).is_empty());
        // A set the system holds for another process stays out. A set it
        // holds for this process stays in.
        let mut held = hybrid;
        held[0] = set(256, 1, SYSTEM_CPU_SET_INFORMATION_ALLOCATED);
        held[1] = set(
            257,
            1,
            SYSTEM_CPU_SET_INFORMATION_ALLOCATED
                | SYSTEM_CPU_SET_INFORMATION_ALLOCATED_TO_TARGET_PROCESS,
        );
        assert_eq!(performance_cpu_sets(&held, 0, &[]), [257, 258, 259]);
        // A CPU with one kind of core keeps the CPU sets of the process.
        assert!(performance_cpu_sets(&[set(256, 0, 0), set(257, 0, 0)], 0, &[]).is_empty());
        assert!(performance_cpu_sets(&[], 0, &[]).is_empty());
        // Each logical processor of a core is one CPU set of its class.
        let cores = processor_cores().expect("the cores");
        let sets = cpu_sets().expect("the CPU sets");
        let processors: u32 = cores
            .iter()
            .map(|core| core.GroupMask[0].Mask.count_ones())
            .sum();
        assert_eq!(sets.len(), processors as usize);
        assert!(sets.iter().all(|set| cores.iter().any(|core| {
            let group = core.GroupMask[0];
            group.Group == set.Group
                && (group.Mask >> set.LogicalProcessorIndex) & 1 == 1
                && core.EfficiencyClass == set.EfficiencyClass
        })));
        // The CPU sets the calling thread selects.
        let selected = || {
            let mut ids = [0u32; 64];
            let mut count = 0;
            // SAFETY: the pseudo handle of the calling thread needs no close,
            // and the call writes `ids.len()` ids at most.
            let read = unsafe {
                GetThreadSelectedCpuSets(
                    GetCurrentThread(),
                    ids.as_mut_ptr(),
                    ids.len() as u32,
                    &mut count,
                )
            };
            assert_ne!(read, 0);
            ids[..count as usize].to_vec()
        };
        // Each machine applies a selection: the thread holds the one set it
        // selects. A null list then clears the selection.
        select_cpu_sets(&[sets[0].Id]);
        assert_eq!(selected(), [sets[0].Id]);
        // SAFETY: the pseudo handle of the calling thread needs no close,
        // and a null list with a count of 0 clears the selection.
        unsafe { SetThreadSelectedCpuSets(GetCurrentThread(), std::ptr::null(), 0) };
        // The thread holds the sets of the performance cores on a CPU with
        // two kinds of cores, and no set on another CPU.
        select_performance_cores();
        let process = process_affinity().expect("the mask of the process");
        let defaults = process_cpu_sets(sets.len()).expect("the CPU sets of the process");
        assert_eq!(selected(), performance_cpu_sets(&sets, process, &defaults));
    }

    /// The cycles of a thread count at a fixed rate on an x86 or x64
    /// machine. An Arm machine, native or under an x64 build in emulation,
    /// and a machine the system does not name have no such rate. This
    /// process runs on an x64 or an Arm machine.
    #[test]
    fn only_x86_and_x64_cycles_count_at_a_fixed_rate() {
        assert!(fixed_rate_cycles(IMAGE_FILE_MACHINE_AMD64));
        assert!(fixed_rate_cycles(IMAGE_FILE_MACHINE_I386));
        assert!(!fixed_rate_cycles(IMAGE_FILE_MACHINE_ARM64));
        assert!(!fixed_rate_cycles(IMAGE_FILE_MACHINE_ARMNT));
        assert!(!fixed_rate_cycles(IMAGE_FILE_MACHINE_UNKNOWN));
        // The system names the native machine of this process: x64, or Arm
        // under an Arm build or an x64 build in emulation.
        let native = native_machine().expect("the native machine");
        assert!(matches!(
            native,
            IMAGE_FILE_MACHINE_AMD64 | IMAGE_FILE_MACHINE_ARM64
        ));
        assert_eq!(cycles_at_fixed_rate(), native == IMAGE_FILE_MACHINE_AMD64);
    }

    /// Only the registering thread reverts: a registration dropped on any
    /// other thread skips the revert. A stand-in with a null handle keeps
    /// the check independent of whether MMCSS answers on this machine; the
    /// revert it does attempt fails harmlessly on that handle.
    #[test]
    fn only_the_registering_thread_reverts() {
        let registering = current_thread();
        let elsewhere = registering.wrapping_add(1);
        let mut stand_in = ProAudioThread {
            handle: std::ptr::null_mut(),
            thread: registering,
        };
        assert!(
            !stand_in.release(elsewhere),
            "a drop on another thread skips the revert"
        );
        assert!(
            stand_in.release(registering),
            "the registering thread reverts"
        );
        // Dropped here: the registering thread, reverting the null handle.
    }
}
