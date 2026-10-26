//! Per-thread event buffers (decision 0001 pitfall 10: no global lock on the hot path).
//!
//! Each thread owns one single-producer/single-consumer ring of 64-bit words in memory taken
//! straight from `VirtualAlloc` (hooks must never touch the process heap). The flush thread is the
//! only consumer. Records are self-delimiting: the first word says what the record is and how many
//! words it spans.

use core::cell::UnsafeCell;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};

use windows_sys::Win32::Foundation::{CloseHandle, DuplicateHandle, HANDLE};
use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, SwitchToThread, WaitForSingleObject,
};

use crate::sampling::{self, ChurnSlot, Pend};

const SYNCHRONIZE: u32 = 0x0010_0000;
pub const RING_WORDS: usize = 1 << 18; // 2 MiB per thread
const RING_MASK: usize = RING_WORDS - 1;
pub const MAX_THREADS: usize = 4096;
pub const STACK_CACHE: usize = 2048;
pub const PEND_CAP: usize = 1024;
pub const PEND_INDEX: usize = 4096;
pub const CHURN_SLOTS: usize = 256;
/// Longest record: a stack of `MAX_DEPTH` frames plus two words; markers are shorter.
pub const MAX_RECORD_WORDS: usize = 96;
/// How long a full ring makes the producer wait for the flush thread before dropping the event.
const FULL_RING_PATIENCE_NS: u64 = 20_000_000;

pub const R_STACK: u8 = 1;
pub const R_ALLOC: u8 = 2;
pub const R_FREE: u8 = 3;
pub const R_REALLOC: u8 = 4;
pub const R_MARKER: u8 = 5;
pub const R_CHURN: u8 = 6;

/// Record header: type, total words, a type-specific byte, and the thread id.
#[inline(always)]
pub const fn header(ty: u8, words: usize, extra: u8, tid: u32) -> u64 {
    ty as u64 | ((words as u64) << 8) | ((extra as u64) << 24) | ((tid as u64) << 32)
}

/// State only the owning thread touches (or the flush thread once the owner is quiesced).
pub struct Local {
    /// Hashes of stacks this thread already sent (0 = empty slot).
    pub stacks: *mut u64,
    pub pend: *mut Pend,
    pub pend_index: *mut u32,
    pub churn: *mut ChurnSlot,
    pub pend_head: u64,
    pub pend_tail: u64,
}

#[repr(C, align(64))]
pub struct ThreadBuf {
    head: AtomicUsize,
    _pad0: [u8; 56],
    tail: AtomicUsize,
    _pad1: [u8; 56],
    /// 1 while the owner is inside a hook; the stop handshake waits for 0.
    pub busy: AtomicU32,
    /// Handle that becomes signalled when the owning thread is gone (nothing can be recorded on
    /// it any more); the flush thread frees the buffer once it is drained.
    thread: HANDLE,
    /// Bytes from the ring to the end of the region: what a final stop gives back. The header page
    /// stays mapped so a thread that is late for the stop handshake never touches freed memory.
    span: usize,
    pub tid: u32,
    reg_slot: u32,
    pub dropped_overflow: AtomicU64,
    pub dropped_sampling: AtomicU64,
    pub dropped_min_lifetime: AtomicU64,
    ring: *mut u64,
    local: UnsafeCell<Local>,
}

// SAFETY: shared fields are atomics or the SPSC ring; `local` is only used by one thread at a time.
unsafe impl Sync for ThreadBuf {}

impl ThreadBuf {
    /// Producer-only state. Caller must be the owning thread, or the flush thread while the owner
    /// is known to be outside every hook.
    #[allow(clippy::mut_from_ref)]
    #[inline(always)]
    pub unsafe fn local(&self) -> &mut Local {
        &mut *self.local.get()
    }

    /// Append a record. Returns false (and counts it) if the ring stayed full too long.
    pub fn push(&self, rec: &[u64]) -> bool {
        let n = rec.len();
        let head = self.head.load(Relaxed);
        let mut waited_since = 0u64;
        while head.wrapping_sub(self.tail.load(Acquire)) + n > RING_WORDS {
            // Backpressure: the flush thread is behind. Wait a little, then give up rather than
            // risk stalling the program for good.
            let now = crate::clock::now_ns();
            if waited_since == 0 {
                waited_since = now.max(1);
                // The flusher sleeps while rings have room; wake it instead of idling until its
                // next timer tick (~15 ms), which used to cap throughput at ring-size per tick.
                crate::flusher::wake();
            } else if now - waited_since > FULL_RING_PATIENCE_NS {
                self.dropped_overflow
                    .store(self.dropped_overflow.load(Relaxed) + 1, Relaxed);
                return false;
            }
            // SAFETY: plain Win32 call.
            unsafe { SwitchToThread() };
        }
        for (i, w) in rec.iter().enumerate() {
            // SAFETY: the masked index is inside the ring and no consumer reads unpublished words.
            unsafe { *self.ring.add(head.wrapping_add(i) & RING_MASK) = *w };
        }
        self.head.store(head.wrapping_add(n), Release);
        true
    }

    /// Consumer side: hand every complete record to `f`; returns how many records were read.
    pub fn drain(&self, mut f: impl FnMut(&[u64])) -> usize {
        let head = self.head.load(Acquire);
        let mut tail = self.tail.load(Relaxed);
        let mut scratch = [0u64; MAX_RECORD_WORDS];
        let mut count = 0;
        while tail != head {
            // SAFETY: masked index inside the ring; words below `head` are published.
            let hdr = unsafe { *self.ring.add(tail & RING_MASK) };
            let words = ((hdr >> 8) & 0xffff) as usize;
            if words == 0 || words > MAX_RECORD_WORDS || head.wrapping_sub(tail) < words {
                tail = head; // corrupt: skip what is left rather than loop forever
                break;
            }
            for (i, slot) in scratch[..words].iter_mut().enumerate() {
                // SAFETY: as above.
                *slot = unsafe { *self.ring.add(tail.wrapping_add(i) & RING_MASK) };
            }
            f(&scratch[..words]);
            tail = tail.wrapping_add(words);
            count += 1;
        }
        self.tail.store(tail, Release);
        count
    }

    pub fn is_empty(&self) -> bool {
        self.head.load(Acquire) == self.tail.load(Relaxed)
    }
}

// ---- registry ----------------------------------------------------------------------------
static REGISTRY: [AtomicPtr<ThreadBuf>; MAX_THREADS] =
    [const { AtomicPtr::new(null_mut()) }; MAX_THREADS];
static REG_HIGH: AtomicUsize = AtomicUsize::new(0);
static RECLAIMED_OVERFLOW: AtomicU64 = AtomicU64::new(0);
static RECLAIMED_SAMPLING: AtomicU64 = AtomicU64::new(0);
static RECLAIMED_MIN_LIFETIME: AtomicU64 = AtomicU64::new(0);
static THREADS_WITHOUT_BUFFER: AtomicU64 = AtomicU64::new(0);

fn align_up(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

/// Allocate and register a buffer for the calling thread. Null on failure.
pub unsafe fn create_for_current_thread() -> *mut ThreadBuf {
    let with_pending = sampling::min_lifetime_ns() != 0;
    let page = 4096;
    let off_ring = align_up(core::mem::size_of::<ThreadBuf>(), page);
    let off_stacks = off_ring + RING_WORDS * 8;
    let off_pend = off_stacks + STACK_CACHE * 8;
    let off_index = off_pend
        + if with_pending {
            PEND_CAP * core::mem::size_of::<Pend>()
        } else {
            0
        };
    let off_churn = off_index + if with_pending { PEND_INDEX * 4 } else { 0 };
    let total = align_up(
        off_churn
            + if with_pending {
                CHURN_SLOTS * core::mem::size_of::<ChurnSlot>()
            } else {
                0
            },
        page,
    );

    let region = VirtualAlloc(
        core::ptr::null(),
        total,
        MEM_COMMIT | MEM_RESERVE,
        PAGE_READWRITE,
    ) as *mut u8;
    if region.is_null() {
        THREADS_WITHOUT_BUFFER.fetch_add(1, Relaxed);
        return null_mut();
    }
    let mut thread: HANDLE = null_mut();
    let process = GetCurrentProcess();
    if DuplicateHandle(
        process,
        GetCurrentThread(),
        process,
        &mut thread,
        SYNCHRONIZE,
        0,
        0,
    ) == 0
    {
        thread = null_mut(); // such a buffer is only reclaimed by the final stop
    }
    let teb: usize;
    core::arch::asm!("mov {}, gs:[0x48]", out(reg) teb, options(nostack, nomem, preserves_flags));
    let tb = region as *mut ThreadBuf;
    let (pend, index, churn) = if with_pending {
        (
            region.add(off_pend) as *mut Pend,
            region.add(off_index) as *mut u32,
            region.add(off_churn) as *mut ChurnSlot,
        )
    } else {
        (null_mut(), null_mut(), null_mut())
    };
    core::ptr::write(
        tb,
        ThreadBuf {
            head: AtomicUsize::new(0),
            _pad0: [0; 56],
            tail: AtomicUsize::new(0),
            _pad1: [0; 56],
            busy: AtomicU32::new(0),
            thread,
            span: total - off_ring,
            tid: teb as u32,
            reg_slot: 0,
            dropped_overflow: AtomicU64::new(0),
            dropped_sampling: AtomicU64::new(0),
            dropped_min_lifetime: AtomicU64::new(0),
            ring: region.add(off_ring) as *mut u64,
            local: UnsafeCell::new(Local {
                stacks: region.add(off_stacks) as *mut u64,
                pend,
                pend_index: index,
                churn,
                pend_head: 0,
                pend_tail: 0,
            }),
        },
    );
    for (i, slot) in REGISTRY.iter().enumerate() {
        if slot
            .compare_exchange(null_mut(), tb, AcqRel, Relaxed)
            .is_ok()
        {
            (*tb).reg_slot = i as u32;
            REG_HIGH.fetch_max(i + 1, Relaxed);
            return tb;
        }
    }
    if !thread.is_null() {
        CloseHandle(thread);
    }
    VirtualFree(region as *mut _, 0, MEM_RELEASE);
    THREADS_WITHOUT_BUFFER.fetch_add(1, Relaxed);
    null_mut()
}

/// Visit every registered buffer.
pub fn for_each(mut f: impl FnMut(&ThreadBuf)) {
    for slot in &REGISTRY[..REG_HIGH.load(Relaxed)] {
        let p = slot.load(Acquire);
        if !p.is_null() {
            // SAFETY: registered buffers stay mapped until the flush thread reclaims them, and
            // that happens on the thread that calls this.
            f(unsafe { &*p });
        }
    }
}

/// True once the owning thread has fully ended: whatever it did during teardown (the loader's
/// per-thread frees run after `DLL_THREAD_DETACH`) is already in the ring.
pub fn owner_gone(tb: &ThreadBuf) -> bool {
    // SAFETY: plain Win32 call on a handle this module owns until `release`.
    !tb.thread.is_null() && unsafe { WaitForSingleObject(tb.thread, 0) } == 0
}

/// Free the buffer of an ended thread once the flush thread has fully drained it.
pub fn reclaim_if_done(tb: &ThreadBuf) -> bool {
    if !tb.is_empty() {
        return false;
    }
    RECLAIMED_OVERFLOW.fetch_add(tb.dropped_overflow.load(Relaxed), Relaxed);
    RECLAIMED_SAMPLING.fetch_add(tb.dropped_sampling.load(Relaxed), Relaxed);
    RECLAIMED_MIN_LIFETIME.fetch_add(tb.dropped_min_lifetime.load(Relaxed), Relaxed);
    release(tb);
    true
}

/// After the final drain: give the rings and caches of every thread that is outside the hooks back
/// to the system. Only the header page stays, which late threads may still touch.
pub fn decommit_all() {
    for_each(|tb| {
        if tb.busy.load(SeqCst) == 0 && tb.span != 0 {
            // SAFETY: capture is off, so no thread uses the ring or its caches (a thread that is
            // late for the stop handshake sees the state change before it gets to them).
            unsafe {
                VirtualFree(tb.ring as *mut _, tb.span, MEM_DECOMMIT);
            }
        }
    });
}

/// Events lost so far: live buffers plus those already reclaimed.
pub fn dropped_totals() -> heapscope_trace::Dropped {
    let mut d = heapscope_trace::Dropped {
        overflow: RECLAIMED_OVERFLOW.load(Relaxed),
        min_lifetime: RECLAIMED_MIN_LIFETIME.load(Relaxed),
        sampling: RECLAIMED_SAMPLING.load(Relaxed),
    };
    for_each(|tb| {
        d.overflow += tb.dropped_overflow.load(Relaxed);
        d.min_lifetime += tb.dropped_min_lifetime.load(Relaxed);
        d.sampling += tb.dropped_sampling.load(Relaxed);
    });
    d.overflow += THREADS_WITHOUT_BUFFER.load(Relaxed) + crate::modules::dropped();
    d
}

/// `DLL_THREAD_DETACH`. While capturing, nothing happens: the loader frees this thread's own
/// per-thread blocks after every DLL has seen the detach, and those frees must still be recorded.
/// The flush thread reclaims the buffer when the thread object is signalled ([`owner_gone`]).
pub unsafe fn retire_current_thread() {
    // Capture finished and nobody will ever read this buffer: give the memory back now.
    if crate::STATE.load(SeqCst) == crate::STATE_OFF {
        let tb = crate::hooks::current_buf();
        if !tb.is_null() {
            crate::hooks::clear_current_buf();
            release(&*tb);
        }
    }
}

fn release(tb: &ThreadBuf) {
    REGISTRY[tb.reg_slot as usize].store(null_mut(), Release);
    if !tb.thread.is_null() {
        // SAFETY: the handle was duplicated for this buffer and is closed exactly once, here.
        unsafe { CloseHandle(tb.thread) };
    }
    // SAFETY: the region starts at the ThreadBuf; the owning thread is the caller and has
    // already dropped its pointer to it.
    unsafe { VirtualFree(tb as *const ThreadBuf as *mut _, 0, MEM_RELEASE) };
}
