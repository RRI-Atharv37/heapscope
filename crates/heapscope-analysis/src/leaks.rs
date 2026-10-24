//! Leak candidates: what is still live when the trace ends, grouped by call path and by kind of
//! object (heap / pool / virtual memory / kernel handle).
//!
//! "Still live at exit" is evidence, not proof, so the report calls these candidates. The capture
//! mode decides how to read them:
//! - `full`: live at exit. Anything the program meant to keep until exit shows up here too, which
//!   is what [`Suppressions`] are for.
//! - `attached`: allocations from before the attach are unknown, so what is live is *net growth
//!   since attach*, not leaks.
//! - `sampled`: totals are multiplied by the sampling rate and labelled as estimates.

use std::collections::HashMap;

use crate::filters::Filter;
use crate::model::{Analysis, LiveGroup, NodeId, ObjectKind};
use crate::suppressions::Suppressions;
use crate::trace::Trace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportKind {
    /// A full capture: live at exit.
    Leaks,
    /// An attached capture: live now but not necessarily leaked.
    NetGrowth,
}

/// Why an entry was left out of the candidates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuppressedBy {
    pub rule: String,
    pub reason: String,
    pub builtin: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeakEntry {
    pub node: NodeId,
    pub kind: ObjectKind,
    /// Scaled by the sampling rate for a sampled capture (handles are never sampled).
    pub allocs: u64,
    pub bytes: u64,
    pub oldest_ts_ns: u64,
    pub newest_ts_ns: u64,
    pub suppressed_by: Option<SuppressedBy>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KindTotal {
    pub kind: ObjectKind,
    pub allocs: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug)]
pub struct LeakReport {
    pub kind: ReportKind,
    /// Counts and sizes are scaled-up estimates (a sampled capture).
    pub estimated: bool,
    pub scale: u64,
    /// The capture shut down cleanly, so "end of trace" is the program's exit.
    pub complete: bool,
    /// Candidates, largest first.
    pub entries: Vec<LeakEntry>,
    /// Entries a suppression rule covered, largest first.
    pub suppressed: Vec<LeakEntry>,
    /// Totals of `entries` per kind of object; only kinds that occur.
    pub by_kind: Vec<KindTotal>,
    pub total_allocs: u64,
    pub total_bytes: u64,
    pub suppressed_allocs: u64,
    pub suppressed_bytes: u64,
    pub notes: Vec<String>,
}

impl LeakReport {
    /// Heading for the section that lists `entries`.
    pub fn title(&self) -> &'static str {
        match (self.kind, self.estimated) {
            (ReportKind::Leaks, false) => "Leak candidates (live at exit)",
            (ReportKind::Leaks, true) => "Leak candidates (live at exit, estimated)",
            (ReportKind::NetGrowth, false) => "Net growth since attach",
            (ReportKind::NetGrowth, true) => "Net growth since attach (estimated)",
        }
    }
}

/// Group the live set of `analysis` into a leak report.
pub fn detect(analysis: &Analysis, suppressions: &Suppressions) -> LeakReport {
    let cap = &analysis.capture;
    let scale = analysis.scale();
    let kind = if cap.attached {
        ReportKind::NetGrowth
    } else {
        ReportKind::Leaks
    };

    let mut entries = Vec::new();
    let mut suppressed = Vec::new();
    for g in &analysis.live_groups {
        // Only sampled allocations are scaled; every handle event is recorded.
        let factor = if g.kind == ObjectKind::Handle {
            1
        } else {
            scale
        };
        let suppressed_by = suppressions
            .matching(&analysis.tree, g.node)
            .map(|r| SuppressedBy {
                rule: r.spec(),
                reason: r.reason.clone(),
                builtin: r.builtin,
            });
        let entry = LeakEntry {
            node: g.node,
            kind: g.kind,
            allocs: g.allocs * factor,
            bytes: g.bytes * factor,
            oldest_ts_ns: g.oldest_ts_ns,
            newest_ts_ns: g.newest_ts_ns,
            suppressed_by,
        };
        if entry.suppressed_by.is_some() {
            suppressed.push(entry);
        } else {
            entries.push(entry);
        }
    }
    let order = |a: &LeakEntry, b: &LeakEntry| {
        b.bytes
            .cmp(&a.bytes)
            .then(b.allocs.cmp(&a.allocs))
            .then(a.node.cmp(&b.node))
            .then(a.kind.cmp(&b.kind))
    };
    entries.sort_by(order);
    suppressed.sort_by(order);

    let mut kinds: Vec<KindTotal> = Vec::new();
    for e in &entries {
        match kinds.iter_mut().find(|k| k.kind == e.kind) {
            Some(k) => {
                k.allocs += e.allocs;
                k.bytes += e.bytes;
            }
            None => kinds.push(KindTotal {
                kind: e.kind,
                allocs: e.allocs,
                bytes: e.bytes,
            }),
        }
    }
    kinds.sort_by_key(|k| k.kind);

    let mut notes = Vec::new();
    match kind {
        ReportKind::NetGrowth => notes.push(
            "attached capture: memory allocated before the attach is not tracked, so these are \
             allocations made since attach that are still live, not confirmed leaks"
                .to_string(),
        ),
        ReportKind::Leaks if !cap.complete => notes.push(
            "the capture did not shut down cleanly, so 'live at exit' means live when the trace \
             ends"
                .to_string(),
        ),
        ReportKind::Leaks => {}
    }
    if cap.sampled {
        notes.push(format!(
            "sampled capture (1 in {}): counts and sizes are estimates, and a small leak can be \
             missed entirely",
            cap.sample_rate
        ));
    }
    if cap.dropped_overflow > 0 {
        notes.push(format!(
            "{} events were lost to full capture buffers: a lost free shows up as a leak and a lost \
             allocation hides one",
            cap.dropped_overflow
        ));
    }
    if analysis.totals.missed_frees > 0 {
        notes.push(format!(
            "{} allocations landed on an address that was still live (their free was not seen); \
             the earlier block is not reported",
            analysis.totals.missed_frees
        ));
    }

    LeakReport {
        kind,
        estimated: cap.sampled,
        scale,
        complete: cap.complete,
        total_allocs: entries.iter().map(|e| e.allocs).sum(),
        total_bytes: entries.iter().map(|e| e.bytes).sum(),
        suppressed_allocs: suppressed.iter().map(|e| e.allocs).sum(),
        suppressed_bytes: suppressed.iter().map(|e| e.bytes).sum(),
        entries,
        suppressed,
        by_kind: kinds,
        notes,
    }
}

/// Handles opened and never closed, grouped by call path.
pub(crate) fn live_handles(
    trace: &Trace,
    node_of: &[NodeId],
    stack_ok: &[bool],
    filter: &Filter,
) -> Vec<LiveGroup> {
    // handle value -> (node, opened at)
    let mut open: HashMap<u64, (NodeId, u64)> = HashMap::new();
    for h in &trace.handles {
        if !h.open {
            open.remove(&h.handle);
            continue;
        }
        let s = if (h.stack as usize) < node_of.len() {
            h.stack as usize
        } else {
            0
        };
        if stack_ok[s] && filter.allows_op(h.thread, 0) {
            open.insert(h.handle, (node_of[s], h.ts));
        }
    }
    let mut groups: HashMap<NodeId, LiveGroup> = HashMap::new();
    for &(node, ts) in open.values() {
        let g = groups.entry(node).or_insert(LiveGroup {
            node,
            kind: ObjectKind::Handle,
            allocs: 0,
            bytes: 0,
            oldest_ts_ns: ts,
            newest_ts_ns: ts,
        });
        g.allocs += 1;
        g.oldest_ts_ns = g.oldest_ts_ns.min(ts);
        g.newest_ts_ns = g.newest_ts_ns.max(ts);
    }
    let mut out: Vec<LiveGroup> = groups.into_values().collect();
    out.sort_by_key(|g| g.node);
    out
}
