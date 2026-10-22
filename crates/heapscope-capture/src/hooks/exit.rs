//! Hook on `RtlExitUserProcess`, the one function every normal process exit (`ExitProcess`,
//! returning from `main`, `exit()`) goes through. It runs on the exiting thread *before* the other
//! threads are terminated, so the final flush can use the ordinary stop path with the flush thread
//! still alive. Waiting until `DLL_PROCESS_DETACH` is too late: by then the flush thread may have
//! been killed in the middle of writing.

use core::ffi::c_void;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering::*};

use minhook::MinHook;

use crate::{flusher, STATE, STATE_CAPTURING};

static ORIG_EXIT: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

type ExitFn = unsafe extern "system" fn(i32);

pub unsafe fn install() -> bool {
    match MinHook::create_hook_api(
        "ntdll",
        "RtlExitUserProcess",
        hk_exit as ExitFn as *mut c_void,
    ) {
        Ok(orig) => {
            ORIG_EXIT.store(orig, SeqCst);
            true
        }
        Err(_) => false,
    }
}

unsafe extern "system" fn hk_exit(status: i32) {
    let orig: ExitFn = core::mem::transmute(ORIG_EXIT.load(Relaxed));
    if STATE.load(Relaxed) == STATE_CAPTURING {
        super::pin_guard();
        flusher::finish_capture(false);
    }
    orig(status)
}
