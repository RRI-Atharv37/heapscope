//! C ABI of the capture DLL, resolved by `include/heapscope/heapscope.h` with `GetProcAddress`.
//! Everything here goes through the same guard and sampling rules as the heap hooks, and does
//! nothing when capture is off or the thread is already inside a hook.

use core::ffi::{c_char, c_void};

use windows_sys::Win32::Foundation::{GetLastError, SetLastError};

use crate::buffer::{header, MAX_RECORD_WORDS, R_MARKER};
use crate::clock;
use crate::hooks::heap::{record_alloc, record_free, KIND_POOL};
use crate::hooks::{enter, leave};

/// Longest marker label in bytes (the rest is cut off).
const MAX_LABEL: usize = 200;

/// A custom allocator handed out `ptr`.
#[no_mangle]
pub unsafe extern "C" fn heapscope_on_alloc(ptr: *mut c_void, size: usize, _pool: *const c_void) {
    if ptr.is_null() {
        return;
    }
    if let Some(tb) = enter() {
        let err = GetLastError();
        record_alloc(tb, ptr as u64, size as u64, KIND_POOL, clock::now_ns());
        SetLastError(err);
        leave(tb);
    }
}

/// A custom allocator took `ptr` back.
#[no_mangle]
pub unsafe extern "C" fn heapscope_on_free(ptr: *mut c_void, _pool: *const c_void) {
    if ptr.is_null() {
        return;
    }
    if let Some(tb) = enter() {
        let err = GetLastError();
        record_free(tb, ptr as u64, KIND_POOL, clock::now_ns());
        SetLastError(err);
        leave(tb);
    }
}

/// Put a named marker (a labelled point in time) into the trace. `label` is a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn heapscope_marker(label: *const c_char) {
    if label.is_null() {
        return;
    }
    let mut bytes = [0u8; MAX_LABEL];
    let mut len = 0;
    while len < MAX_LABEL && *label.add(len) != 0 {
        bytes[len] = *label.add(len) as u8;
        len += 1;
    }
    if let Some(tb) = enter() {
        let err = GetLastError();
        let label_words = len.div_ceil(8);
        let mut rec = [0u64; MAX_RECORD_WORDS];
        let words = 3 + label_words;
        rec[0] = header(R_MARKER, words, 0, tb.tid);
        rec[1] = clock::now_ns();
        rec[2] = len as u64;
        for (i, chunk) in bytes[..len].chunks(8).enumerate() {
            let mut w = [0u8; 8];
            w[..chunk.len()].copy_from_slice(chunk);
            rec[3 + i] = u64::from_le_bytes(w);
        }
        tb.push(&rec[..words]);
        SetLastError(err);
        leave(tb);
    }
}
