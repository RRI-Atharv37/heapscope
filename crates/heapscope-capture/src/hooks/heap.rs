//! Inline hooks on the ntdll heap entry points (decision 0001). Every `malloc`, `operator new`,
//! `HeapAlloc`, `LocalAlloc`, `CoTaskMemAlloc`... of a process ends in one of these three
//! functions, whatever CRT the program was built with.
//!
//! Ordering rules (pitfalls 1 and 2): the guard is held across the original call, a free is
//! logged *before* the real free (the address may be reused by another thread the moment the real
//! free returns), and a realloc is stamped *before* the call for the same reason.

use core::ffi::c_void;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering::*};

use minhook::MinHook;
use windows_sys::Win32::Foundation::{GetLastError, SetLastError};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

use super::{enter, leave};
use crate::buffer::{header, ThreadBuf, R_ALLOC, R_FREE, R_REALLOC};
use crate::{clock, sampling, stack};

pub const KIND_HEAP: u8 = 0;
pub const KIND_POOL: u8 = 1;

static ORIG_ALLOC: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static ORIG_REALLOC: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static ORIG_FREE: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static RTL_SIZE_HEAP: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

type AllocFn = unsafe extern "system" fn(*mut c_void, u32, usize) -> *mut c_void;
type ReallocFn = unsafe extern "system" fn(*mut c_void, u32, *mut c_void, usize) -> *mut c_void;
type FreeFn = unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> u8;
type SizeFn = unsafe extern "system" fn(*mut c_void, u32, *const c_void) -> usize;

/// Create (but do not enable) the three hooks.
pub unsafe fn create() -> bool {
    fn hook(name: &str, detour: *mut c_void, slot: &AtomicPtr<c_void>) -> bool {
        // SAFETY: `detour` has the signature of the function it replaces (see the type aliases).
        match unsafe { MinHook::create_hook_api("ntdll", name, detour) } {
            Ok(orig) => {
                slot.store(orig, SeqCst);
                true
            }
            Err(_) => false,
        }
    }
    let ntdll: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
    let size_fn = GetProcAddress(
        GetModuleHandleW(ntdll.as_ptr()),
        c"RtlSizeHeap".as_ptr().cast(),
    );
    match size_fn {
        Some(f) => RTL_SIZE_HEAP.store(f as *mut c_void, SeqCst),
        None => return false,
    }
    hook(
        "RtlAllocateHeap",
        hk_alloc as AllocFn as *mut c_void,
        &ORIG_ALLOC,
    ) && hook(
        "RtlReAllocateHeap",
        hk_realloc as ReallocFn as *mut c_void,
        &ORIG_REALLOC,
    ) && hook("RtlFreeHeap", hk_free as FreeFn as *mut c_void, &ORIG_FREE)
}

#[inline(never)]
pub(crate) unsafe fn record_alloc(tb: &ThreadBuf, addr: u64, size: u64, kind: u8, ts: u64) {
    if !sampling::keep(addr, sampling::one_in()) {
        tb.dropped_sampling
            .store(tb.dropped_sampling.load(Relaxed) + 1, Relaxed);
        return;
    }
    let mut frames = [0u64; stack::MAX_DEPTH];
    let n = stack::capture(&mut frames);
    let hash = stack::define(tb, &frames[..n]);
    if kind == KIND_HEAP
        && sampling::min_lifetime_ns() != 0
        && sampling::hold(tb, ts, addr, size, hash)
    {
        return;
    }
    tb.push(&[header(R_ALLOC, 6, kind, tb.tid), ts, addr, size, size, hash]);
}

#[inline(never)]
pub(crate) unsafe fn record_free(tb: &ThreadBuf, addr: u64, ts: u64) {
    if !sampling::keep(addr, sampling::one_in()) {
        return;
    }
    if sampling::min_lifetime_ns() != 0 && sampling::fold_free(tb, ts, addr) {
        return;
    }
    tb.push(&[header(R_FREE, 3, 0, tb.tid), ts, addr]);
}

/// `old == 0` is a plain allocation. `ts` was taken before the real call.
#[inline(never)]
unsafe fn record_realloc(
    tb: &ThreadBuf,
    ts: u64,
    old: u64,
    new: u64,
    old_size: u64,
    new_size: u64,
) {
    if old == 0 {
        record_alloc(tb, new, new_size, KIND_HEAP, ts);
        return;
    }
    let one_in = sampling::one_in();
    match (sampling::keep(old, one_in), sampling::keep(new, one_in)) {
        (true, true) => {
            sampling::release(tb, old);
            let mut frames = [0u64; stack::MAX_DEPTH];
            let n = stack::capture(&mut frames);
            let hash = stack::define(tb, &frames[..n]);
            tb.push(&[
                header(R_REALLOC, 8, 0, tb.tid),
                ts,
                old,
                new,
                old_size,
                new_size,
                new_size,
                hash,
            ]);
        }
        (true, false) => {
            sampling::release(tb, old);
            tb.push(&[header(R_FREE, 3, 0, tb.tid), ts, old]);
        }
        (false, true) => record_alloc(tb, new, new_size, KIND_HEAP, ts),
        (false, false) => {
            tb.dropped_sampling
                .store(tb.dropped_sampling.load(Relaxed) + 1, Relaxed);
        }
    }
}

unsafe extern "system" fn hk_alloc(heap: *mut c_void, flags: u32, size: usize) -> *mut c_void {
    let orig: AllocFn = core::mem::transmute(ORIG_ALLOC.load(Relaxed));
    let tracked = enter();
    let p = orig(heap, flags, size);
    if let Some(tb) = tracked {
        if !p.is_null() {
            let err = GetLastError();
            record_alloc(tb, p as u64, size as u64, KIND_HEAP, clock::now_ns());
            SetLastError(err);
        }
        leave(tb);
    }
    p
}

unsafe extern "system" fn hk_realloc(
    heap: *mut c_void,
    flags: u32,
    old: *mut c_void,
    size: usize,
) -> *mut c_void {
    let orig: ReallocFn = core::mem::transmute(ORIG_REALLOC.load(Relaxed));
    let tracked = enter();
    let (ts, old_size) = match tracked {
        Some(_) => {
            let err = GetLastError();
            let size_of_old = if old.is_null() {
                0
            } else {
                let f: SizeFn = core::mem::transmute(RTL_SIZE_HEAP.load(Relaxed));
                match f(heap, 0, old) {
                    usize::MAX => 0,
                    s => s as u64,
                }
            };
            let ts = clock::now_ns();
            SetLastError(err);
            (ts, size_of_old)
        }
        None => (0, 0),
    };
    let p = orig(heap, flags, old, size);
    if let Some(tb) = tracked {
        if !p.is_null() {
            let err = GetLastError();
            record_realloc(tb, ts, old as u64, p as u64, old_size, size as u64);
            SetLastError(err);
        }
        leave(tb);
    }
    p
}

unsafe extern "system" fn hk_free(heap: *mut c_void, flags: u32, p: *mut c_void) -> u8 {
    let orig: FreeFn = core::mem::transmute(ORIG_FREE.load(Relaxed));
    let tracked = if p.is_null() { None } else { enter() };
    if let Some(tb) = tracked {
        let err = GetLastError();
        record_free(tb, p as u64, clock::now_ns());
        SetLastError(err);
    }
    let r = orig(heap, flags, p);
    if let Some(tb) = tracked {
        leave(tb);
    }
    r
}
