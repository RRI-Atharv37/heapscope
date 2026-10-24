//! Call-stack capture and per-thread de-duplication.
//!
//! The hook captures raw return addresses; a stack is sent to the flush thread only the first
//! time a thread uses it (a small per-thread cache keyed by a 64-bit hash). Events then carry just
//! the hash, which keeps an allocation record to 48 bytes.

use core::ffi::c_void;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

use windows_sys::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;

use crate::buffer::{header, ThreadBuf, R_STACK, STACK_CACHE};

pub const MAX_DEPTH: usize = 64;
pub const DEFAULT_DEPTH: u32 = 32;
/// Frames above the hook that belong to us and the unwinder; captured so they can be dropped.
const OWN_FRAME_SLACK: usize = 5;

static DEPTH: AtomicU32 = AtomicU32::new(DEFAULT_DEPTH);
static OWN_LO: AtomicUsize = AtomicUsize::new(0);
static OWN_HI: AtomicUsize = AtomicUsize::new(0);

pub fn set_depth(depth: u32) {
    let d = if depth == 0 {
        DEFAULT_DEPTH
    } else {
        depth.min(MAX_DEPTH as u32)
    };
    DEPTH.store(d, Relaxed);
}

/// Address range of this DLL's image, used to strip our own frames (pitfall 5).
pub fn set_own_range(lo: usize, hi: usize) {
    OWN_LO.store(lo, Relaxed);
    OWN_HI.store(hi, Relaxed);
}

/// Capture the caller's stack, innermost frame first, without any frame inside this DLL.
/// Returns the number of frames written to `out`.
#[inline(never)]
pub unsafe fn capture(out: &mut [u64; MAX_DEPTH]) -> usize {
    let depth = DEPTH.load(Relaxed) as usize;
    let mut raw = [null_mut::<c_void>(); MAX_DEPTH + OWN_FRAME_SLACK];
    let total = RtlCaptureStackBackTrace(
        0,
        (depth + OWN_FRAME_SLACK) as u32,
        raw.as_mut_ptr(),
        null_mut(),
    ) as usize;
    let (lo, hi) = (OWN_LO.load(Relaxed), OWN_HI.load(Relaxed));
    let mut start = 0;
    for (i, f) in raw[..total.min(OWN_FRAME_SLACK)].iter().enumerate() {
        let a = *f as usize;
        if a >= lo && a < hi {
            start = i + 1;
        }
    }
    let n = (total - start).min(depth);
    for i in 0..n {
        out[i] = raw[start + i] as u64;
    }
    n
}

pub fn hash(frames: &[u64]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &f in frames {
        h ^= f;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        h ^= h >> 29;
    }
    if h == 0 {
        1
    } else {
        h
    }
}

/// Make sure the flush thread knows `frames`, and return their hash (0 = no stack could be sent).
pub unsafe fn define(tb: &ThreadBuf, frames: &[u64]) -> u64 {
    if frames.is_empty() {
        return 0;
    }
    let h = hash(frames);
    let local = tb.local();
    let mask = STACK_CACHE - 1;
    let first = h as usize & mask;
    let mut empty = None;
    for step in 0..8 {
        let i = (first + step) & mask;
        let v = *local.stacks.add(i);
        if v == h {
            return h;
        }
        if v == 0 {
            empty = Some(i);
            break;
        }
    }
    let mut rec = [0u64; MAX_DEPTH + 2];
    rec[0] = header(R_STACK, frames.len() + 2, frames.len() as u8, tb.tid);
    rec[1] = h;
    rec[2..2 + frames.len()].copy_from_slice(frames);
    if !tb.push(&rec[..frames.len() + 2]) {
        return 0;
    }
    // Cache slot: the empty one found, or (all 8 probes taken) overwrite the first.
    *local.stacks.add(empty.unwrap_or(first)) = h;
    h
}
