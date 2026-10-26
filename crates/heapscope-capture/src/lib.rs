//! In-process capture library (ntdll `Rtl*Heap` hooks, see decision 0001).
//!
//! The CLI injects this DLL, either into a process it created suspended (`run`) or into one that
//! is already running (`attach`). `DllMain` reads the configuration from the control block, opens
//! the trace file, installs the hooks and starts the flush thread. From then on every heap call is
//! recorded into a per-thread ring buffer that the flush thread writes out.
//!
//! The crate is a hooking layer: almost everything is `unsafe` by nature, so two lints that would
//! only add noise are relaxed here.
#![allow(unsafe_op_in_unsafe_fn, clippy::missing_safety_doc)]
#![cfg(windows)]

mod buffer;
mod clock;
mod control;
mod exports;
mod flusher;
mod hooks;
mod modules;
mod sampling;
mod stack;

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering::*};

use heapscope_control::{error, state};
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows_sys::Win32::System::Threading::CreateThread;

pub(crate) const STATE_OFF: u32 = 0;
pub(crate) const STATE_CAPTURING: u32 = 1;
/// Hooks pass straight through while a stop is in progress.
pub(crate) const STATE_STOPPING: u32 = 2;

/// Whether the hooks record anything. Everything else hangs off this one flag.
pub(crate) static STATE: AtomicU32 = AtomicU32::new(STATE_OFF);

const DLL_PROCESS_DETACH: u32 = 0;
const DLL_PROCESS_ATTACH: u32 = 1;
const DLL_THREAD_DETACH: u32 = 3;

#[no_mangle]
pub unsafe extern "system" fn DllMain(_module: HMODULE, reason: u32, reserved: *mut c_void) -> i32 {
    match reason {
        DLL_PROCESS_ATTACH => i32::from(start()),
        DLL_THREAD_DETACH => {
            buffer::retire_current_thread();
            1
        }
        DLL_PROCESS_DETACH => {
            // A non-null `reserved` means the process is ending. Normally the final
            // `NtTerminateProcess` hook writes the footer, after every DLL (this one detaches
            // first) has freed what it holds. Without that hook this is the last chance.
            if !reserved.is_null() && !hooks::exit::installed() {
                hooks::pin_guard();
                flusher::finish_capture(true);
            }
            1
        }
        _ => 1,
    }
}

unsafe fn record_own_image_range() {
    let mut module: HMODULE = core::ptr::null_mut();
    let flags =
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    if GetModuleHandleExW(
        flags,
        record_own_image_range as *const () as *const u16,
        &mut module,
    ) != 0
        && !module.is_null()
    {
        let base = module as usize;
        let nt = base + *((base + 0x3c) as *const u32) as usize;
        let size_of_image = *((nt + 24 + 56) as *const u32) as usize;
        stack::set_own_range(base, base + size_of_image);
    }
}

unsafe extern "system" fn flush_thread(_: *mut c_void) -> u32 {
    hooks::pin_guard();
    flusher::run();
    0
}

/// Bring capture up. On failure nothing stays hooked and the control block says why.
unsafe fn start() -> bool {
    clock::init();
    record_own_image_range();
    if !hooks::init_slots() {
        return fail(error::OUT_OF_MEMORY);
    }
    // Nothing done on this thread during start-up may be recorded.
    hooks::pin_guard();
    let Some(cfg) = control::load() else {
        return fail(error::BAD_CONTROL_BLOCK);
    };
    sampling::configure(cfg.sample_one_in, cfg.min_lifetime_ms);
    stack::set_depth(cfg.stack_depth);
    if let Err(code) = flusher::open(&cfg) {
        return fail(code);
    }
    // Missing DLL notifications only cost the module log for libraries loaded later.
    let _ = modules::init();
    if !hooks::install() {
        hooks::remove();
        modules::remove();
        return fail(error::HOOK_INSTALL);
    }
    let thread = CreateThread(
        core::ptr::null(),
        0,
        Some(flush_thread),
        core::ptr::null(),
        0,
        core::ptr::null_mut(),
    );
    if thread.is_null() {
        hooks::remove();
        modules::remove();
        return fail(error::START_THREAD);
    }
    windows_sys::Win32::Foundation::CloseHandle(thread);
    STATE.store(STATE_CAPTURING, SeqCst);
    control::set_state(state::CAPTURING);
    hooks::unpin_guard();
    true
}

unsafe fn fail(code: u32) -> bool {
    STATE.store(STATE_OFF, SeqCst);
    control::set_failed(code);
    hooks::unpin_guard();
    false
}
