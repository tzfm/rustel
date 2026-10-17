//! Creation and release of plugins on the process's main thread.
//!
//! On macOS a plugin may create and destroy objects in calls that AppKit
//! allows only on the main thread: some shells create a window in their
//! factory call and stop the process when the call runs elsewhere. The
//! plugin thread still owns the plugin and makes every other call; it
//! queues creation and release for the main thread and waits for the
//! answer.
//!
//! A program that owns the main thread registers it with
//! [`install_main_service`] before any other thread can load a plugin and
//! pumps [`pump_main`] while the work runs, also while it shuts down; the
//! command line does this for itself and for the studio it runs on its
//! worker thread. With no service installed, each step runs on the thread
//! that asks for it, and the other systems put no step on the main
//! thread.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

/// One creation or release, waiting for the main thread.
type Step = Box<dyn FnOnce() + Send + 'static>;

/// The steps in arrival order.
static QUEUE: Mutex<VecDeque<Step>> = Mutex::new(VecDeque::new());
/// True once the process's main thread serves the steps.
static SERVICE: AtomicBool = AtomicBool::new(false);

/// Whether plugins need the process's main thread: on macOS, yes.
pub fn needs_the_main_thread() -> bool {
    cfg!(target_os = "macos")
}

/// Registers the process's main thread as the server of the plugin steps.
/// No-op when the service is already installed or the caller is not the
/// process's main thread.
pub fn install_main_service() {
    #[cfg(target_os = "macos")]
    apple::install();
}

/// Serves the plugin steps that arrived, on the calling thread: the main
/// thread of a program with the service installed calls this every few
/// milliseconds. Does nothing on any other thread and on the other
/// systems.
pub fn pump_main() {
    #[cfg(target_os = "macos")]
    apple::pump();
}

/// Runs `step` on the process's main thread and blocks the caller until
/// the step returns. A panic of the step crosses to the calling thread as
/// a panic of its own. With no service installed, on the main thread
/// itself, and on the other systems, the step runs on the calling thread.
pub(crate) fn run<T: Send + 'static>(step: impl FnOnce() -> T + Send + 'static) -> T {
    if !needs_the_main_thread() || !SERVICE.load(Ordering::Acquire) || on_the_main_thread() {
        return step();
    }
    dispatch(step)
}

/// Queues the step for the serving thread and waits for its answer.
fn dispatch<T: Send + 'static>(step: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, finished) = mpsc::sync_channel(1);
    enqueue(Box::new(move || {
        // A panic of the step belongs to the thread that asked for it.
        let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(step));
        let _ = done.send(answer);
    }));
    match finished.recv() {
        Ok(Ok(value)) => value,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        // The queue runs each step exactly once, so a closed channel is a
        // state no path here reaches.
        Err(_) => panic!("a queued step ended with no answer"),
    }
}

/// Puts a step in the queue and tells the main thread.
fn enqueue(step: Step) {
    let mut queue = QUEUE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    queue.push_back(step);
    drop(queue);
    #[cfg(target_os = "macos")]
    apple::wake();
}

/// Serves every step of the queue on the calling thread, also a step that
/// arrives while one runs. Only the macOS pump and the tests call this.
#[cfg(any(target_os = "macos", test))]
fn drain() {
    loop {
        let step = QUEUE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front();
        let Some(step) = step else {
            return;
        };
        step();
    }
}

/// True when this thread is the process's main thread.
#[cfg(target_os = "macos")]
fn on_the_main_thread() -> bool {
    apple::on_the_main_thread()
}

/// The other systems have no main-thread service.
#[cfg(not(target_os = "macos"))]
fn on_the_main_thread() -> bool {
    false
}

/// The run loop of the macOS main thread, through the system's own
/// CoreFoundation, as `module.rs` calls the system's bundle functions.
#[cfg(target_os = "macos")]
mod apple {
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::atomic::{AtomicPtr, Ordering};

    use super::SERVICE;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopGetMain() -> *const c_void;
        fn CFRunLoopGetCurrent() -> *const c_void;
        fn CFRunLoopSourceCreate(
            allocator: *const c_void,
            order: isize,
            context: *mut SourceContext,
        ) -> *const c_void;
        fn CFRunLoopAddSource(run_loop: *const c_void, source: *const c_void, mode: *const c_void);
        fn CFRunLoopSourceSignal(source: *const c_void);
        fn CFRunLoopWakeUp(run_loop: *const c_void);
        fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: u8) -> i32;
        static kCFRunLoopDefaultMode: *const c_void;
    }

    /// The context of a version-0 run loop source. The system copies it;
    /// only the `perform` callback is of interest here.
    #[repr(C)]
    struct SourceContext {
        version: isize,
        info: *mut c_void,
        retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
        release: Option<unsafe extern "C" fn(*const c_void)>,
        copy_description: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
        equal: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> u8>,
        hash: Option<unsafe extern "C" fn(*const c_void) -> usize>,
        schedule: Option<unsafe extern "C" fn(*mut c_void, *const c_void, *const c_void)>,
        cancel: Option<unsafe extern "C" fn(*mut c_void, *const c_void, *const c_void)>,
        perform: Option<unsafe extern "C" fn(*mut c_void)>,
    }

    /// The queue's source in the main run loop. Only the main thread
    /// writes it, once, and the run loop holds the source for the rest of
    /// the process, so it needs no release.
    static SOURCE: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

    /// Serves the queue: the run loop calls this when the source is
    /// signaled.
    unsafe extern "C" fn perform(_info: *mut c_void) {
        super::drain();
    }

    pub(super) fn on_the_main_thread() -> bool {
        // SAFETY: the two calls only read which run loop owns the thread.
        unsafe { CFRunLoopGetCurrent() == CFRunLoopGetMain() }
    }

    /// Adds the queue's source to the main run loop, once, and marks the
    /// service installed. A call from a thread that is not the process's
    /// main thread, as the studio's entry on the command line's worker
    /// thread after the command line already registered, registers
    /// nothing; a program that never registered keeps its steps on the
    /// threads that ask for them.
    pub(super) fn install() {
        if !SOURCE.load(Ordering::Acquire).is_null() {
            return;
        }
        if !on_the_main_thread() {
            return;
        }
        let mut context = SourceContext {
            version: 0,
            info: ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(perform),
        };
        // SAFETY: the context is plain values and the system copies it.
        let source = unsafe { CFRunLoopSourceCreate(ptr::null(), 0, &mut context) };
        if source.is_null() {
            return;
        }
        // SAFETY: the source this thread made goes to its own run loop.
        unsafe { CFRunLoopAddSource(CFRunLoopGetMain(), source, kCFRunLoopDefaultMode) };
        SOURCE.store(source.cast_mut(), Ordering::Release);
        SERVICE.store(true, Ordering::Release);
    }

    /// One turn of the main thread: the run loop serves the queue's
    /// source, and the drain that follows takes a step that arrived after
    /// the loop looked.
    pub(super) fn pump() {
        if !on_the_main_thread() {
            return;
        }
        install();
        // SAFETY: the mode name is the system's own constant, and a turn
        // of no duration serves at most what is ready now.
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, 1) };
        super::drain();
    }

    /// Tells the main thread that a step arrived: a signaled source and a
    /// woken run loop reach a main thread that runs its loop, and a
    /// polling main thread finds the step at its next pump either way.
    pub(super) fn wake() {
        let source = SOURCE.load(Ordering::Acquire);
        if source.is_null() {
            return;
        }
        // SAFETY: the pointer is the source `install` added, and the run
        // loop is the system's own main loop.
        unsafe {
            CFRunLoopSourceSignal(source);
            CFRunLoopWakeUp(CFRunLoopGetMain());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The queue is one static, so the tests that put steps in it take
    /// this lock: they cannot serve each other's steps when the harness
    /// runs tests in parallel.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    /// With no service installed the step runs on the calling thread and
    /// gives its answer: the contract for an embedder that pumps nothing,
    /// and the behavior of every run on the other systems.
    #[test]
    fn a_step_runs_on_its_caller_with_no_service() {
        let caller = std::thread::current().id();
        let on_caller = std::thread::spawn(move || {
            let ran_on = run(|| std::thread::current().id());
            (ran_on, std::thread::current().id())
        })
        .join()
        .expect("the step");
        assert_eq!(on_caller.0, on_caller.1, "the step ran on its caller");
        assert_ne!(on_caller.0, caller, "the step did not run on this thread");
        assert_eq!(run(|| 7 + 5), 12);
    }

    /// A panic of a step is the panic of the thread that asked for it.
    #[test]
    fn a_panicking_step_panics_its_caller() {
        let panic = std::thread::spawn(move || run::<()>(|| panic!("a step fault"))).join();
        assert!(panic.is_err(), "the panic crossed to the caller");
    }

    /// A dispatched step crosses the queue to the serving thread, and a
    /// panic of one crosses back to the thread that asked for it.
    #[test]
    fn a_dispatched_step_and_its_panic_cross_to_the_caller() {
        let _alone = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let asked = std::thread::spawn(|| {
            let fault = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                dispatch::<()>(|| panic!("a dispatched fault"))
            }));
            (fault.is_err(), dispatch(|| 5 + 7))
        });
        while !asked.is_finished() {
            drain();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let (fault, answer) = asked.join().expect("the asked steps");
        assert!(fault, "a dispatched panic did not cross to the caller");
        assert_eq!(answer, 12);
    }

    /// The queue serves a step on the thread that drains it, and holds the
    /// step until one does.
    #[test]
    fn a_queued_step_runs_on_the_thread_that_drains() {
        let _alone = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (done, finished) = mpsc::channel();
        enqueue(Box::new(move || {
            let _ = done.send(std::thread::current().id());
        }));
        drain();
        let served = finished.recv().expect("the queued step");
        assert_eq!(
            served,
            std::thread::current().id(),
            "a queued step ran away from the thread that drains"
        );
    }

    /// A thread that is not the process's main thread registers no
    /// service, however many times it asks: the studio, which the command
    /// line runs on its worker thread after registering there itself,
    /// asks again, and a worker of a program that registered nowhere must
    /// not move the steps onto a thread that serves no run loop.
    #[test]
    #[cfg(all(target_os = "macos", not(miri)))]
    fn a_thread_off_the_main_thread_registers_no_service() {
        for _ in 0..2 {
            std::thread::spawn(install_main_service)
                .join()
                .expect("the registration call");
        }
        let served = std::thread::spawn(move || {
            let caller = std::thread::current().id();
            (run(|| std::thread::current().id()), caller)
        })
        .join()
        .expect("the step");
        assert_eq!(
            served.0, served.1,
            "a step ran away from its thread after an off-main registration"
        );
    }
}
