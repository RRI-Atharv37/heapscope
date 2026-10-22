//! `--sample N` and `--min-lifetime`: the two ways to keep traces small.
//!
//! * Sampling keeps an allocation when a hash of its *address* falls in 1/N, so its free (same
//!   address) is kept too with no state. The header stores N so analysis can scale.
//! * Minimum lifetime holds each allocation in a small per-thread FIFO. If the same thread frees it
//!   before the threshold, neither event is written and the stack's `ChurnSummary` grows instead;
//!   otherwise the allocation is written later with its original timestamp.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use crate::buffer::{header, ThreadBuf, CHURN_SLOTS, PEND_CAP, PEND_INDEX, R_ALLOC, R_CHURN};

static ONE_IN: AtomicU32 = AtomicU32::new(1);
static MIN_LIFETIME_NS: AtomicU64 = AtomicU64::new(0);

pub fn configure(one_in: u32, min_lifetime_ms: u32) {
    ONE_IN.store(one_in.max(1), Relaxed);
    MIN_LIFETIME_NS.store(u64::from(min_lifetime_ms) * 1_000_000, Relaxed);
}

#[inline(always)]
pub fn one_in() -> u32 {
    ONE_IN.load(Relaxed)
}

#[inline(always)]
pub fn min_lifetime_ns() -> u64 {
    MIN_LIFETIME_NS.load(Relaxed)
}

#[inline(always)]
fn mix(addr: u64) -> u64 {
    let mut x = (addr >> 4).wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Whether sampling keeps this address (always true when sampling is off).
#[inline(always)]
pub fn keep(addr: u64, one_in: u32) -> bool {
    one_in <= 1 || ((mix(addr) >> 32) * u64::from(one_in)) >> 32 == 0
}

/// An allocation waiting to see whether it is freed quickly.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Pend {
    pub addr: u64,
    pub ts: u64,
    pub size: u64,
    pub hash: u64,
    pub valid: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ChurnSlot {
    used: u64,
    hash: u64,
    allocs: u64,
    bytes: u64,
    lifetime_ns: u64,
    /// Allocations of this stack currently held.
    cur: u64,
    /// Most held at the same moment (includes some that were later kept: a slight over-estimate).
    max: u64,
}

#[inline(always)]
fn index_of(addr: u64) -> usize {
    mix(addr) as usize & (PEND_INDEX - 1)
}

unsafe fn churn_slot(tb: &ThreadBuf, hash: u64, create: bool) -> *mut ChurnSlot {
    let l = tb.local();
    let first = mix(hash) as usize & (CHURN_SLOTS - 1);
    for step in 0..8 {
        let s = l.churn.add((first + step) & (CHURN_SLOTS - 1));
        if (*s).used != 0 && (*s).hash == hash {
            return s;
        }
        if (*s).used == 0 {
            if !create {
                return core::ptr::null_mut();
            }
            *s = ChurnSlot {
                used: 1,
                hash,
                allocs: 0,
                bytes: 0,
                lifetime_ns: 0,
                cur: 0,
                max: 0,
            };
            return s;
        }
    }
    core::ptr::null_mut()
}

/// Write a held entry as a normal allocation and forget it. The caller clears `valid`.
unsafe fn write_held(tb: &ThreadBuf, e: &Pend) {
    tb.push(&[
        header(R_ALLOC, 6, 0, tb.tid),
        e.ts,
        e.addr,
        e.size,
        e.size,
        e.hash,
    ]);
    let cs = churn_slot(tb, e.hash, false);
    if !cs.is_null() {
        (*cs).cur = (*cs).cur.saturating_sub(1);
    }
}

/// Write the front entry (if it is still valid) as a normal allocation and drop it from the FIFO.
unsafe fn pop_front(tb: &ThreadBuf) {
    let l = tb.local();
    let pos = (l.pend_tail % PEND_CAP as u64) as usize;
    let e = &mut *l.pend.add(pos);
    if e.valid != 0 {
        write_held(tb, e);
        let ix = index_of(e.addr);
        if *l.pend_index.add(ix) == pos as u32 + 1 {
            *l.pend_index.add(ix) = 0;
        }
        e.valid = 0;
    }
    l.pend_tail += 1;
}

/// Write out everything that has been held for at least the threshold.
unsafe fn age_out(tb: &ThreadBuf, now: u64) {
    let limit = min_lifetime_ns();
    loop {
        // Re-read each round: `pop_front` advances the tail through the same state.
        let l = tb.local();
        if l.pend_tail >= l.pend_head {
            break;
        }
        let e = &*l.pend.add((l.pend_tail % PEND_CAP as u64) as usize);
        if e.valid != 0 && now.saturating_sub(e.ts) < limit {
            break;
        }
        pop_front(tb);
    }
}

/// Offer a fresh allocation to the FIFO. True means it is held and must not be written now.
pub unsafe fn hold(tb: &ThreadBuf, now: u64, addr: u64, size: u64, hash: u64) -> bool {
    let l = tb.local();
    if l.pend.is_null() {
        return false;
    }
    age_out(tb, now);
    let ix = index_of(addr);
    let cur = *l.pend_index.add(ix);
    if cur != 0 {
        let e = &mut *l.pend.add((cur - 1) as usize);
        if e.addr != addr {
            return false; // another held address owns this index slot: write this one normally
        }
        // The same address is still held: it was freed by another thread. Write that allocation
        // out as it was (its free is in the other thread's ring), then hold the new one.
        write_held(tb, e);
        e.valid = 0;
        *l.pend_index.add(ix) = 0;
    }
    let cs = churn_slot(tb, hash, true);
    if cs.is_null() {
        return false;
    }
    if l.pend_head - l.pend_tail >= PEND_CAP as u64 {
        pop_front(tb);
    }
    let pos = (l.pend_head % PEND_CAP as u64) as usize;
    *l.pend.add(pos) = Pend {
        addr,
        ts: now,
        size,
        hash,
        valid: 1,
    };
    l.pend_head += 1;
    *l.pend_index.add(ix) = pos as u32 + 1;
    (*cs).cur += 1;
    (*cs).max = (*cs).max.max((*cs).cur);
    true
}

/// A free arrived. True means the allocation was still held and young: both events vanish into
/// the stack's churn totals.
pub unsafe fn fold_free(tb: &ThreadBuf, now: u64, addr: u64) -> bool {
    let l = tb.local();
    if l.pend.is_null() {
        return false;
    }
    age_out(tb, now);
    let ix = index_of(addr);
    let cur = *l.pend_index.add(ix);
    if cur == 0 {
        return false;
    }
    let e = &mut *l.pend.add((cur - 1) as usize);
    if e.addr != addr {
        return false;
    }
    e.valid = 0;
    *l.pend_index.add(ix) = 0;
    let cs = churn_slot(tb, e.hash, false);
    if !cs.is_null() {
        (*cs).allocs += 1;
        (*cs).bytes += e.size;
        (*cs).lifetime_ns += now.saturating_sub(e.ts);
        (*cs).cur = (*cs).cur.saturating_sub(1);
    }
    tb.dropped_min_lifetime
        .store(tb.dropped_min_lifetime.load(Relaxed) + 1, Relaxed);
    true
}

/// Make sure a held allocation at `addr` is written before something else (a realloc) changes it.
pub unsafe fn release(tb: &ThreadBuf, addr: u64) {
    let l = tb.local();
    if l.pend.is_null() {
        return;
    }
    let ix = index_of(addr);
    let cur = *l.pend_index.add(ix);
    if cur == 0 {
        return;
    }
    let e = &mut *l.pend.add((cur - 1) as usize);
    if e.addr == addr {
        write_held(tb, e);
        e.valid = 0;
        *l.pend_index.add(ix) = 0;
    }
}

/// Write every held allocation (they are live as far as this thread knows).
pub unsafe fn flush_pending(tb: &ThreadBuf) {
    if tb.local().pend.is_null() {
        return;
    }
    loop {
        let l = tb.local();
        if l.pend_tail >= l.pend_head {
            break;
        }
        pop_front(tb);
    }
}

/// Write the accumulated per-stack totals of dropped allocations.
pub unsafe fn emit_churn(tb: &ThreadBuf) {
    let l = tb.local();
    if l.churn.is_null() {
        return;
    }
    let now = crate::clock::now_ns();
    for i in 0..CHURN_SLOTS {
        let s = &mut *l.churn.add(i);
        if s.used != 0 && s.allocs > 0 {
            tb.push(&[
                header(R_CHURN, 7, 0, tb.tid),
                now,
                s.hash,
                s.allocs,
                s.bytes,
                s.lifetime_ns,
                s.max,
            ]);
            s.allocs = 0;
            s.bytes = 0;
            s.lifetime_ns = 0;
            s.max = s.cur;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_is_all_or_one_in_n_and_stable() {
        assert!(keep(0x1000, 0) && keep(0x1000, 1));
        let n = 20u32;
        let total = 200_000u64;
        let kept = (0..total)
            .filter(|i| keep(0x1_0000_0000 + i * 16, n))
            .count() as f64;
        let expected = total as f64 / f64::from(n);
        assert!(
            (kept - expected).abs() / expected < 0.05,
            "kept {kept}, expected ~{expected}"
        );
        assert_eq!(keep(0xdead_beef0, n), keep(0xdead_beef0, n));
    }
}
