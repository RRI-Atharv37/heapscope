//! Process-exit hooks, to finish the trace at the very end of the process.
//!
//! `RtlExitUserProcess` (behind `ExitProcess`, returning from `main`, `exit()`) takes the process
//! heap lock, terminates the other threads with `NtTerminateProcess(NULL)`, then runs
//! `LdrShutdownProcess`, which gives every DLL its `DLL_PROCESS_DETACH` (static destructors free
//! their memory there), and only then makes the final `NtTerminateProcess(current process)`.
//! Finishing earlier loses those frees. So the entry hook only parks the flush thread (while it
//! still can: afterwards it could block on the heap lock holding the trace lock and then be killed
//! with it), and the final call writes everything that is left, from the exiting thread.

use core::ffi::c_void;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering::*};

use minhook::MinHook;

use crate::{flusher, STATE, STATE_CAPTURING};

static ORIG_TERMINATE: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static ORIG_EXIT: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static HOOKED: AtomicBool = AtomicBool::new(false);

type ExitFn = unsafe extern "system" fn(i32);
type TerminateFn = unsafe extern "system" fn(*mut c_void, i32) -> i32;

/// Whether the final flush is done by the hook (otherwise `DllMain` has to do it).
pub fn installed() -> bool {
    HOOKED.load(Relaxed)
}

pub unsafe fn install() -> bool {
    if let Ok(orig) = MinHook::create_hook_api(
        "ntdll",
        "RtlExitUserProcess",
        hk_exit as ExitFn as *mut c_void,
    ) {
        ORIG_EXIT.store(orig, SeqCst);
    }
    match MinHook::create_hook_api(
        "ntdll",
        "NtTerminateProcess",
        hk_terminate as TerminateFn as *mut c_void,
    ) {
        Ok(orig) => {
            ORIG_TERMINATE.store(orig, SeqCst);
            HOOKED.store(true, SeqCst);
            true
        }
        Err(_) => false,
    }
}

unsafe extern "system" fn hk_exit(status: i32) {
    let orig: ExitFn = core::mem::transmute(ORIG_EXIT.load(Relaxed));
    if STATE.load(Relaxed) == STATE_CAPTURING {
        flusher::park();
    }
    orig(status)
}

unsafe extern "system" fn hk_terminate(process: *mut c_void, status: i32) -> i32 {
    let orig: TerminateFn = core::mem::transmute(ORIG_TERMINATE.load(Relaxed));
    if process as isize == -1 && STATE.load(Relaxed) == STATE_CAPTURING {
        super::pin_guard();
        flusher::finish_capture(true);
    }
    orig(process, status)
}
