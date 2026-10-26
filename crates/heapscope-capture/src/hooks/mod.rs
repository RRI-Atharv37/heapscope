//! Hook installation and the per-thread reentrancy guard.
//!
//! Rules (decision 0001, pitfalls 1-4): the guard is held across the original call because the
//! heap manager calls `Rtl*Heap` recursively; nothing here may touch the process heap; the guard
//! lives in a raw TEB TLS slot so it works for threads that are still starting up.

pub mod exit;
pub mod heap;

use core::sync::atomic::{AtomicU32, Ordering::*};

use crate::buffer::{self, ThreadBuf};
use crate::STATE;

const TLS_SLOTS_IN_TEB: u32 = 64;
const TEB_TLS_SLOTS_OFFSET: usize = 0x1480;

static GUARD_IDX: AtomicU32 = AtomicU32::new(u32::MAX);
static BUF_IDX: AtomicU32 = AtomicU32::new(u32::MAX);

/// Allocate the two TLS slots. They must live in the TEB's inline array (index < 64).
pub fn init_slots() -> bool {
    use windows_sys::Win32::System::Threading::TlsAlloc;
    // SAFETY: plain Win32 call.
    let (g, b) = unsafe { (TlsAlloc(), TlsAlloc()) };
    if g >= TLS_SLOTS_IN_TEB || b >= TLS_SLOTS_IN_TEB {
        return false;
    }
    GUARD_IDX.store(g, Relaxed);
    BUF_IDX.store(b, Relaxed);
    true
}

#[inline(always)]
unsafe fn slot(idx: &AtomicU32) -> *mut usize {
    let i = idx.load(Relaxed) as usize;
    if i >= TLS_SLOTS_IN_TEB as usize {
        return core::ptr::null_mut();
    }
    let teb: usize;
    core::arch::asm!("mov {}, gs:[0x30]", out(reg) teb, options(nostack, nomem, preserves_flags));
    (teb + TEB_TLS_SLOTS_OFFSET + i * 8) as *mut usize
}

/// Keep this thread out of the hooks for good (flush thread, init, exiting threads).
pub unsafe fn pin_guard() {
    let s = slot(&GUARD_IDX);
    if !s.is_null() {
        *s = 1;
    }
}

pub unsafe fn unpin_guard() {
    let s = slot(&GUARD_IDX);
    if !s.is_null() {
        *s = 0;
    }
}

/// The calling thread's buffer, if it has one.
pub unsafe fn current_buf() -> *mut ThreadBuf {
    let s = slot(&BUF_IDX);
    if s.is_null() {
        core::ptr::null_mut()
    } else {
        *s as *mut ThreadBuf
    }
}

pub unsafe fn clear_current_buf() {
    let s = slot(&BUF_IDX);
    if !s.is_null() {
        *s = 0;
    }
}

/// Start recording an event on this thread. `None` means pass straight through: capture is off,
/// this thread is already inside a hook, or no buffer could be created. Pair with [`leave`].
#[inline(always)]
pub unsafe fn enter() -> Option<&'static ThreadBuf> {
    if STATE.load(Relaxed) != crate::STATE_CAPTURING {
        return None;
    }
    let g = slot(&GUARD_IDX);
    if g.is_null() || *g != 0 {
        return None;
    }
    *g = 1;
    let bs = slot(&BUF_IDX);
    let mut tb = *bs as *mut ThreadBuf;
    if tb.is_null() {
        tb = buffer::create_for_current_thread();
        if tb.is_null() {
            *g = 0;
            return None;
        }
        *bs = tb as usize;
    }
    let tb: &'static ThreadBuf = &*tb;
    tb.busy.store(1, SeqCst);
    // Stop-request handshake: the flusher flips STATE, then waits for `busy` to clear.
    if STATE.load(SeqCst) != crate::STATE_CAPTURING {
        tb.busy.store(0, Release);
        *g = 0;
        return None;
    }
    Some(tb)
}

#[inline(always)]
pub unsafe fn leave(tb: &ThreadBuf) {
    tb.busy.store(0, Release);
    let g = slot(&GUARD_IDX);
    if !g.is_null() {
        *g = 0;
    }
}

/// Install the ntdll heap hooks and the process-termination hook. Everything is created before
/// anything is enabled. Without the termination hook capture still works, but the final flush then
/// happens in `DLL_PROCESS_DETACH` (before other DLLs free their statics), so it is not required.
pub unsafe fn install() -> bool {
    if !heap::create() {
        return false;
    }
    let _ = exit::install();
    minhook::MinHook::enable_all_hooks().is_ok()
}

pub unsafe fn remove() {
    let _ = minhook::MinHook::disable_all_hooks();
}
