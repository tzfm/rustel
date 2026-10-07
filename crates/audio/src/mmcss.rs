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

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Threading::{
    AVRT_PRIORITY, AVRT_PRIORITY_CRITICAL, AVRT_PRIORITY_NORMAL, AvRevertMmThreadCharacteristics,
    AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority, GetCurrentThreadId,
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
        let mut task_index: u32 = 0;
        // SAFETY: the task name is a NUL-terminated UTF-16 array that
        // outlives the call, and `task_index` is a valid out-pointer.
        let handle = unsafe { AvSetMmThreadCharacteristicsW(Self::TASK.as_ptr(), &mut task_index) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: `handle` was just returned by the characteristics call.
        unsafe {
            AvSetMmThreadPriority(handle, priority);
        }
        Some(Self {
            handle,
            thread: current_thread(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Registration and revert do not fault, whichever way the service
    /// answers on this machine (a hosted runner may lack MMCSS: that is
    /// `None`, not a failure).
    #[test]
    fn registration_attaches_and_reverts_without_faulting() {
        let render = ProAudioThread::attach_critical();
        let feeder = ProAudioThread::attach_normal();
        drop(feeder);
        drop(render);
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
