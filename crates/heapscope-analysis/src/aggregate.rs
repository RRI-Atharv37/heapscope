//! Replays the operations of a trace in time order, keeping the live set, and accumulates
//! bytes / counts / live bytes per call path.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use crate::filters::Filter;
use crate::model::{MarkerInfo, NodeId, PathStats, TimelinePoint, Totals};
use crate::peak::PeakTracker;
use crate::trace::{MarkerEvent, Op, OpKind};

/// Addresses are 16-byte aligned multiples; a plain multiplicative hash would leave the low bits
/// (which pick the bucket) constant, so fold the high half back in.
#[derive(Default)]
pub(crate) struct AddrHasher(u64);

impl Hasher for AddrHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 32)
    }
}

type AddrMap<V> = HashMap<u64, V, BuildHasherDefault<AddrHasher>>;

#[derive(Clone, Copy)]
struct Block {
    size: u64,
    node: NodeId,
    ts: u64,
}

/// What one operation did to the live set.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Effect {
    Alloc {
        node: NodeId,
        size: u64,
    },
    Free {
        node: NodeId,
        size: u64,
        lifetime_ns: u64,
    },
    /// A free of memory the replay never saw allocated.
    Unmatched,
    /// An allocation landed on an address that was still live: its free was lost.
    Missed,
}

pub(crate) struct Replayer {
    live: AddrMap<Block>,
    pub bytes: u64,
    pub allocs: u64,
    /// Live (allocs, bytes) per tree node.
    pub per_node: Vec<(u64, u64)>,
}

impl Replayer {
    fn new(nodes: usize) -> Self {
        Self {
            live: AddrMap::default(),
            bytes: 0,
            allocs: 0,
            per_node: vec![(0, 0); nodes],
        }
    }

    fn release(&mut self, addr: u64, ts: u64, emit: &mut dyn FnMut(Effect)) {
        match self.live.remove(&addr) {
            Some(b) => {
                self.bytes -= b.size;
                self.allocs -= 1;
                let n = &mut self.per_node[b.node as usize];
                n.0 -= 1;
                n.1 -= b.size;
                emit(Effect::Free {
                    node: b.node,
                    size: b.size,
                    lifetime_ns: ts.saturating_sub(b.ts),
                });
            }
            None => emit(Effect::Unmatched),
        }
    }

    fn acquire(&mut self, addr: u64, op: &Op, node: NodeId, emit: &mut dyn FnMut(Effect)) {
        let block = Block {
            size: op.size,
            node,
            ts: op.ts,
        };
        if let Some(old) = self.live.insert(addr, block) {
            self.bytes -= old.size;
            self.allocs -= 1;
            let n = &mut self.per_node[old.node as usize];
            n.0 -= 1;
            n.1 -= old.size;
            emit(Effect::Missed);
        }
        self.bytes += op.size;
        self.allocs += 1;
        let n = &mut self.per_node[node as usize];
        n.0 += 1;
        n.1 += op.size;
        emit(Effect::Alloc {
            node,
            size: op.size,
        });
    }

    fn apply(&mut self, op: &Op, node: NodeId, admit: bool, emit: &mut dyn FnMut(Effect)) {
        match op.kind {
            OpKind::Alloc => {
                if admit {
                    self.acquire(op.addr, op, node, emit);
                }
            }
            OpKind::Free => self.release(op.addr, op.ts, emit),
            OpKind::Realloc => {
                if op.old_addr != 0 {
                    self.release(op.old_addr, op.ts, emit);
                }
                if admit {
                    self.acquire(op.addr, op, node, emit);
                }
            }
        }
    }
}

/// Everything a replay needs, borrowed from the analysis in progress.
pub(crate) struct Inputs<'a> {
    pub ops: &'a [Op],
    pub markers: &'a [MarkerEvent],
    /// Tree node of each stack id (index 0 is the "no stack" node).
    pub node_of: &'a [NodeId],
    /// Whether each stack id passes the module/text filters.
    pub stack_ok: &'a [bool],
    pub filter: &'a Filter,
    pub nodes: usize,
}

impl Inputs<'_> {
    fn index(&self, stack: u32) -> usize {
        let i = stack as usize;
        if i < self.node_of.len() {
            i
        } else {
            0
        }
    }
}

pub(crate) trait Observer {
    fn effect(&mut self, _op: &Op, _effect: Effect) {}
    fn after_op(&mut self, _index: usize, _op: &Op, _state: &Replayer) {}
    fn marker(&mut self, _marker: &MarkerEvent, _state: &Replayer) {}
}

pub(crate) struct Silent;
impl Observer for Silent {}

/// Apply the first `limit` operations. Markers are reported at their point in time.
pub(crate) fn replay(inp: &Inputs<'_>, limit: usize, obs: &mut dyn Observer) -> Replayer {
    let mut state = Replayer::new(inp.nodes);
    let mut next_marker = 0;
    for (i, op) in inp.ops[..limit].iter().enumerate() {
        while next_marker < inp.markers.len() && inp.markers[next_marker].ts_ns < op.ts {
            obs.marker(&inp.markers[next_marker], &state);
            next_marker += 1;
        }
        let s = inp.index(op.stack);
        let admit = inp.stack_ok[s] && inp.filter.allows_op(op.thread, op.size);
        state.apply(op, inp.node_of[s], admit, &mut |e| obs.effect(op, e));
        obs.after_op(i, op, &state);
    }
    if limit == inp.ops.len() {
        for m in &inp.markers[next_marker..] {
            obs.marker(m, &state);
        }
    }
    state
}

/// Accumulates statistics while a replay runs.
pub(crate) struct Collector {
    pub stats: Vec<PathStats>,
    pub totals: Totals,
    pub markers: Vec<MarkerInfo>,
    pub peak: PeakTracker,
    timeline: Vec<Option<TimelinePoint>>,
    t0: u64,
    span: u64,
    short_ns: u64,
    filtering: bool,
}

impl Collector {
    pub(crate) fn new(
        nodes: usize,
        ops: &[Op],
        timeline_points: usize,
        short_ns: u64,
        filtering: bool,
    ) -> Self {
        let t0 = ops.first().map_or(0, |o| o.ts);
        let t1 = ops.last().map_or(0, |o| o.ts);
        Self {
            stats: vec![PathStats::default(); nodes],
            totals: Totals::default(),
            markers: Vec::new(),
            peak: PeakTracker::default(),
            timeline: vec![None; timeline_points.max(1)],
            t0,
            span: t1 - t0,
            short_ns,
            filtering,
        }
    }

    /// The timeline in time order, one point per non-empty bucket (the largest live size in it).
    pub(crate) fn timeline(&self) -> Vec<TimelinePoint> {
        self.timeline.iter().flatten().copied().collect()
    }
}

impl Observer for Collector {
    fn effect(&mut self, _op: &Op, effect: Effect) {
        match effect {
            Effect::Alloc { node, size } => {
                let s = &mut self.stats[node as usize];
                s.allocs += 1;
                s.alloc_bytes += size;
                self.totals.allocs += 1;
                self.totals.alloc_bytes += size;
            }
            Effect::Free {
                node,
                size,
                lifetime_ns,
            } => {
                let s = &mut self.stats[node as usize];
                s.frees += 1;
                s.freed_bytes += size;
                s.lifetime_ns += lifetime_ns;
                if lifetime_ns < self.short_ns {
                    s.short_allocs += 1;
                    s.short_bytes += size;
                    s.short_lifetime_ns += lifetime_ns;
                }
                self.totals.frees += 1;
                self.totals.freed_bytes += size;
            }
            // With a filter active most frees belong to allocations that were filtered out.
            Effect::Unmatched => {
                if !self.filtering {
                    self.totals.unmatched_frees += 1;
                }
            }
            Effect::Missed => self.totals.missed_frees += 1,
        }
    }

    fn after_op(&mut self, index: usize, op: &Op, state: &Replayer) {
        self.peak.observe(index, op.ts, state.bytes, state.allocs);
        let n = self.timeline.len() as u128;
        let b = ((u128::from(op.ts - self.t0) * n) / (u128::from(self.span) + 1)) as usize;
        let last = self.timeline.len() - 1;
        let slot = &mut self.timeline[b.min(last)];
        if slot.is_none_or_smaller(state.bytes) {
            *slot = Some(TimelinePoint {
                ts_ns: op.ts,
                live_bytes: state.bytes,
                live_allocs: state.allocs,
            });
        }
    }

    fn marker(&mut self, m: &MarkerEvent, state: &Replayer) {
        self.markers.push(MarkerInfo {
            ts_ns: m.ts_ns,
            label: m.label.clone(),
            live_bytes: state.bytes,
            live_allocs: state.allocs,
        });
    }
}

trait SlotExt {
    fn is_none_or_smaller(&self, bytes: u64) -> bool;
}

impl SlotExt for Option<TimelinePoint> {
    fn is_none_or_smaller(&self, bytes: u64) -> bool {
        self.as_ref().map_or(true, |p| bytes >= p.live_bytes)
    }
}
