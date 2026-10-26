//! Fixed-size concurrent maps from an address to a `u64`, for the two kinds of per-address state
//! that more than one thread has to see: the allocations `--sample` kept (so their free is kept
//! too) and the allocations `--min-lifetime` holds back (so a free from another thread cancels the
//! hold). Memory comes straight from `VirtualAlloc`, never from the process heap.
//!
//! The table is cut into shards, each with its own spin lock and open-addressing probe sequence
//! (linear probing, backward-shift deletion, so there are no tombstones to clog it). A lock that
//! cannot be taken, or a shard that is full, makes the operation fail; the caller decides what is
//! safe to do then, and `lost` counts how often it happened.

use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};

use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};

const SHARDS: usize = 64;
/// Spins before a lock is given up on (a thread holding it was preempted for a long time).
const SPIN_LIMIT: u32 = 20_000;

#[repr(C)]
#[derive(Clone, Copy)]
struct Slot {
    /// 0 = empty, so keys must be non-zero.
    key: u64,
    val: u64,
}

/// What a removal found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found {
    Yes(u64),
    No,
    /// The shard lock could not be taken: nothing is known.
    Busy,
}

pub struct AddrMap {
    base: AtomicPtr<Slot>,
    /// Slots per shard minus one (a power of two minus one); 0 until created.
    mask: AtomicUsize,
    locks: [AtomicU32; SHARDS],
    counts: [AtomicU32; SHARDS],
    /// Operations that failed because a shard was full or its lock stayed taken.
    lost: AtomicU64,
}

#[inline(always)]
fn hash(key: u64) -> u64 {
    let mut x = key.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 32;
    x = x.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    x ^ (x >> 32)
}

impl AddrMap {
    pub const fn new() -> Self {
        Self {
            base: AtomicPtr::new(null_mut()),
            mask: AtomicUsize::new(0),
            locks: [const { AtomicU32::new(0) }; SHARDS],
            counts: [const { AtomicU32::new(0) }; SHARDS],
            lost: AtomicU64::new(0),
        }
    }

    /// Reserve room for `1 << total_log2` entries (at most 7/8 of them usable). False on failure.
    pub fn create(&self, total_log2: u32) -> bool {
        let per_shard = (1usize << total_log2) / SHARDS;
        let bytes = per_shard * SHARDS * core::mem::size_of::<Slot>();
        // SAFETY: plain Win32 call; the region is zero-filled, which is a valid empty table.
        let p = unsafe {
            VirtualAlloc(
                core::ptr::null(),
                bytes,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        } as *mut Slot;
        if p.is_null() {
            return false;
        }
        self.mask.store(per_shard - 1, Relaxed);
        self.base.store(p, Release);
        true
    }

    /// Give the memory back. No thread may be inside an operation.
    pub fn destroy(&self) {
        let p = self.base.swap(null_mut(), AcqRel);
        if !p.is_null() {
            // SAFETY: the region came from `create` and is released once (the pointer was swapped out).
            unsafe { VirtualFree(p as *mut _, 0, MEM_RELEASE) };
        }
    }

    pub fn lost(&self) -> u64 {
        self.lost.load(Relaxed)
    }

    fn lock(&self, shard: usize) -> bool {
        for _ in 0..SPIN_LIMIT {
            if self.locks[shard]
                .compare_exchange_weak(0, 1, Acquire, Relaxed)
                .is_ok()
            {
                return true;
            }
            core::hint::spin_loop();
        }
        self.lost.fetch_add(1, Relaxed);
        false
    }

    fn unlock(&self, shard: usize) {
        self.locks[shard].store(0, Release);
    }

    /// Shard number and the slots of that shard; `None` when the table does not exist.
    fn shard(&self, key: u64) -> Option<(usize, *mut Slot, usize, usize)> {
        let base = self.base.load(Acquire);
        if base.is_null() {
            return None;
        }
        let mask = self.mask.load(Relaxed);
        let h = hash(key);
        let shard = (h >> 58) as usize % SHARDS;
        // SAFETY: the shard's slots lie inside the region `create` made.
        let slots = unsafe { base.add(shard * (mask + 1)) };
        Some((shard, slots, h as usize & mask, mask))
    }

    /// Set `key` to `val`. False if the table is missing or full, or the lock stayed taken.
    pub fn insert(&self, key: u64, val: u64) -> bool {
        let Some((shard, slots, mut i, mask)) = self.shard(key) else {
            return false;
        };
        if !self.lock(shard) {
            return false;
        }
        let mut done = false;
        for _ in 0..=mask {
            // SAFETY: `i` is masked into the shard, and the shard lock is held.
            let s = unsafe { &mut *slots.add(i) };
            if s.key == key {
                s.val = val;
                done = true;
                break;
            }
            if s.key == 0 {
                let n = self.counts[shard].load(Relaxed) as usize;
                if (n + 1) * 8 <= (mask + 1) * 7 {
                    *s = Slot { key, val };
                    self.counts[shard].store(n as u32 + 1, Relaxed);
                    done = true;
                }
                break;
            }
            i = (i + 1) & mask;
        }
        self.unlock(shard);
        if !done {
            self.lost.fetch_add(1, Relaxed);
        }
        done
    }

    /// Remove `key` and return its value. With `only_if`, the entry is removed only when it holds
    /// that value (an entry of someone else stays, and the answer is `No`).
    pub fn remove(&self, key: u64, only_if: Option<u64>) -> Found {
        let Some((shard, slots, start, mask)) = self.shard(key) else {
            return Found::No;
        };
        if !self.lock(shard) {
            return Found::Busy;
        }
        let mut found = Found::No;
        let mut i = start;
        for _ in 0..=mask {
            // SAFETY: as in `insert`.
            let s = unsafe { *slots.add(i) };
            if s.key == 0 {
                break;
            }
            if s.key == key {
                let wanted = match only_if {
                    None => true,
                    Some(v) => v == s.val,
                };
                if wanted {
                    found = Found::Yes(s.val);
                    // SAFETY: lock held; indices are masked into the shard.
                    unsafe { delete_at(slots, i, mask) };
                    let n = self.counts[shard].load(Relaxed);
                    self.counts[shard].store(n.saturating_sub(1), Relaxed);
                }
                break;
            }
            i = (i + 1) & mask;
        }
        self.unlock(shard);
        found
    }
}

/// Backward-shift deletion: close the hole at `hole` by moving back every later entry of the same
/// cluster that is not already at its ideal slot.
unsafe fn delete_at(slots: *mut Slot, mut hole: usize, mask: usize) {
    let mut j = hole;
    loop {
        j = (j + 1) & mask;
        let s = *slots.add(j);
        if s.key == 0 {
            break;
        }
        let ideal = hash(s.key) as usize & mask;
        // Movable when its ideal slot is not strictly between the hole and itself.
        if (j.wrapping_sub(ideal) & mask) >= (j.wrapping_sub(hole) & mask) {
            *slots.add(hole) = s;
            hole = j;
        }
    }
    *slots.add(hole) = Slot { key: 0, val: 0 };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn behaves_like_a_hash_map_under_a_long_mixed_workload() {
        let m = AddrMap::new();
        assert!(m.create(12)); // 64 slots per shard: small enough to cluster and shift a lot
        let mut model: HashMap<u64, u64> = HashMap::new();
        let mut x = 0x1234_5678_9abc_def1u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for step in 0..200_000u64 {
            let key = (next() % 700) + 1;
            if next() % 2 == 0 {
                let ok = m.insert(key, step);
                if ok {
                    model.insert(key, step);
                } else {
                    // A full shard refuses; the model must not hold it either.
                    assert!(!model.contains_key(&key));
                }
            } else {
                let want = model.remove(&key).map_or(Found::No, Found::Yes);
                assert_eq!(m.remove(key, None), want, "step {step} key {key}");
            }
        }
        for key in 1..=700 {
            let want = model.remove(&key).map_or(Found::No, Found::Yes);
            assert_eq!(m.remove(key, None), want);
        }
        m.destroy();
    }

    #[test]
    fn only_if_leaves_other_entries_alone() {
        let m = AddrMap::new();
        assert!(m.create(12));
        assert!(m.insert(0x1000, 7));
        assert_eq!(m.remove(0x1000, Some(8)), Found::No);
        assert_eq!(m.remove(0x1000, Some(7)), Found::Yes(7));
        assert_eq!(m.remove(0x1000, None), Found::No);
        m.destroy();
    }

    #[test]
    fn a_full_shard_refuses_instead_of_looping() {
        let m = AddrMap::new();
        assert!(m.create(6)); // one slot per shard: 7/8 of 1 rounds down to nothing usable
        assert!(!m.insert(0x2000, 1));
        assert!(m.lost() >= 1);
        m.destroy();
    }

    #[test]
    fn threads_inserting_and_removing_distinct_keys_do_not_interfere() {
        let m = std::sync::Arc::new(AddrMap::new());
        assert!(m.create(16));
        let handles: Vec<_> = (0..4u64)
            .map(|t| {
                let m = m.clone();
                std::thread::spawn(move || {
                    for round in 0..20_000u64 {
                        let key = (t << 40) | ((round % 512) + 1);
                        assert!(m.insert(key, round));
                        assert_eq!(m.remove(key, None), Found::Yes(round));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        m.destroy();
    }
}
