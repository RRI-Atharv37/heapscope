//! Short-lived allocation detection.
//!
//! Two sources feed it: allocations the replay saw being freed sooner than the threshold, and the
//! `ChurnSummary` totals a `--min-lifetime` capture wrote instead of those allocations. Merging
//! both gives the same churn totals as a full capture.

use heapscope_trace::ChurnSummary;

use crate::model::{ChurnEntry, NodeId, PathStats, Totals};

/// Add the capture's folded totals to the per-site statistics. Each folded allocation was also
/// freed, so it counts as an allocation, a free and a short-lived allocation.
pub(crate) fn merge_summaries<'a>(
    summaries: impl IntoIterator<Item = &'a ChurnSummary>,
    node_of: &[NodeId],
    stats: &mut [PathStats],
    totals: &mut Totals,
) {
    for s in summaries {
        let i = node_of.get(s.stack as usize).copied().unwrap_or(node_of[0]) as usize;
        let st = &mut stats[i];
        st.allocs += s.allocs;
        st.alloc_bytes += s.bytes;
        st.frees += s.allocs;
        st.freed_bytes += s.bytes;
        st.lifetime_ns += s.total_lifetime_ns;
        st.short_allocs += s.allocs;
        st.short_bytes += s.bytes;
        st.short_lifetime_ns += s.total_lifetime_ns;
        st.max_concurrent = st.max_concurrent.max(s.max_concurrent);
        totals.allocs += s.allocs;
        totals.alloc_bytes += s.bytes;
        totals.frees += s.allocs;
        totals.freed_bytes += s.bytes;
    }
}

/// Sites ordered by how many short-lived allocations they made.
pub(crate) fn detect(stats: &[PathStats]) -> Vec<ChurnEntry> {
    let mut out: Vec<ChurnEntry> = stats
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, s)| s.short_allocs > 0)
        .map(|(i, s)| ChurnEntry {
            node: i as NodeId,
            allocs: s.short_allocs,
            bytes: s.short_bytes,
            avg_lifetime_ns: s.short_lifetime_ns / s.short_allocs,
            max_concurrent: s.max_concurrent,
            share: s.short_allocs as f64 / s.allocs.max(1) as f64,
        })
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse((e.allocs, e.bytes)));
    out
}
