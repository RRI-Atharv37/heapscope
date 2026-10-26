//! Analysis of a HeapScope trace: call tree, per-path totals, peak, timeline and churn.
//!
//! `Trace::open` reads a trace, `analyze` resolves its stacks with a [`FrameResolver`] and replays the
//! operations. Leak detection and diffing build on the same `Analysis`.

mod aggregate;
mod churn;
pub mod diff;
pub mod filters;
pub mod frames;
pub mod identity;
pub mod json;
pub mod leaks;
pub mod model;
mod peak;
pub mod suppressions;
mod trace;

pub use filters::Filter;
pub use heapscope_symbols::{Frame, FrameResolver};
pub use model::{
    Analysis, CallTree, CaptureInfo, ChurnEntry, FrameInfo, LiveGroup, MarkerInfo, Node, NodeId,
    ObjectKind, PathStats, Peak, Snapshot, TimelinePoint, Totals, ROOT,
};
pub use suppressions::{SuppressionRule, Suppressions};
pub use trace::{MarkerEvent, Trace};

use heapscope_trace::CaptureMode;

use crate::aggregate::{replay, Collector, Inputs};

#[derive(Clone, Debug)]
pub struct Options {
    pub filter: Filter,
    /// Hide allocator and C runtime frames (see [`frames`]). On by default.
    pub trim_frames: bool,
    /// Allocations freed sooner than this count as short-lived. `None`: the capture's
    /// `--min-lifetime` if it had one, else 1 ms.
    pub short_lived_ns: Option<u64>,
    /// Upper bound on timeline points.
    pub timeline_points: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            filter: Filter::default(),
            trim_frames: true,
            short_lived_ns: None,
            timeline_points: 512,
        }
    }
}

/// Where the stacks of a trace ended up in the call tree.
struct Paths {
    tree: CallTree,
    /// Leaf node of each stack id; index 0 is the "no stack" node.
    node_of: Vec<NodeId>,
    stack_ok: Vec<bool>,
}

fn build_paths(trace: &Trace, resolver: &mut dyn FrameResolver, opts: &Options) -> Paths {
    let mut tree = CallTree::new();
    let none = tree.child(
        ROOT,
        FrameInfo {
            name: "<no stack>".into(),
            module: None,
            file: None,
            line: 0,
        },
    );
    let count = trace.stacks.len();
    let mut node_of = vec![none; count + 1];
    let no_stack_ok = opts.filter.modules.is_empty() && opts.filter.text.is_none();
    let mut stack_ok = vec![no_stack_ok; count + 1];
    for id in 1..=count {
        let Some(addrs) = trace.stacks.get(id as u32) else {
            continue;
        };
        let frames: Vec<Frame> = addrs.iter().map(|&a| resolver.resolve(a)).collect();
        stack_ok[id] = opts.filter.allows_stack(&frames);
        let kept = if opts.trim_frames {
            frames::trim(&frames)
        } else {
            &frames[..]
        };
        let mut node = ROOT;
        // The tree grows from the outermost caller to the allocation site.
        for f in kept.iter().rev() {
            node = tree.child(
                node,
                FrameInfo {
                    name: f.display_name(),
                    module: f.module.clone(),
                    file: f.file.clone(),
                    line: f.line,
                },
            );
        }
        if node != ROOT {
            node_of[id] = node;
        }
    }
    Paths {
        tree,
        node_of,
        stack_ok,
    }
}

fn capture_info(trace: &Trace) -> CaptureInfo {
    let h = &trace.header;
    let dropped = trace.footer.map(|f| f.dropped).unwrap_or_default();
    CaptureInfo {
        exe_path: h.exe_path.clone(),
        command_line: h.command_line.clone(),
        pid: h.pid,
        pointer_size: h.pointer_size,
        sampled: matches!(h.capture_mode, CaptureMode::Sampled { .. }),
        sample_rate: match h.capture_mode {
            CaptureMode::Full => 1,
            CaptureMode::Sampled { one_in } => one_in,
        },
        min_lifetime_ms: h.min_lifetime_ms,
        attached: h.attached,
        complete: trace.footer.is_some(),
        dropped_overflow: dropped.overflow,
        dropped_min_lifetime: dropped.min_lifetime,
        dropped_sampling: dropped.sampling,
    }
}

/// Resolve the stacks of `trace` and replay it.
pub fn analyze(trace: &Trace, resolver: &mut dyn FrameResolver, opts: &Options) -> Analysis {
    let paths = build_paths(trace, resolver, opts);
    let capture = capture_info(trace);
    let mut warnings = trace.warnings.clone();

    let short_ns = opts
        .short_lived_ns
        .unwrap_or(if capture.min_lifetime_ms > 0 {
            u64::from(capture.min_lifetime_ms) * 1_000_000
        } else {
            1_000_000
        });
    let filtering = !opts.filter.is_empty();

    let inputs = Inputs {
        ops: &trace.ops,
        markers: &trace.markers,
        node_of: &paths.node_of,
        stack_ok: &paths.stack_ok,
        filter: &opts.filter,
        nodes: paths.tree.len(),
    };
    let mut collector = Collector::new(
        paths.tree.len(),
        &trace.ops,
        opts.timeline_points,
        short_ns,
        filtering,
    );
    let end = replay(&inputs, trace.ops.len(), &mut collector);
    collector.peak.fill_live_set(&inputs, &mut collector.stats);

    for (s, &(allocs, bytes)) in collector.stats.iter_mut().zip(&end.per_node) {
        s.live_allocs = allocs;
        s.live_bytes = bytes;
    }
    collector.totals.live_allocs = end.allocs;
    collector.totals.live_bytes = end.bytes;

    if opts.filter.cannot_filter_summaries() {
        if !trace.churn.is_empty() {
            warnings.push(
                "thread and size filters cannot apply to the totals of a --min-lifetime capture; \
                 those totals are left out"
                    .into(),
            );
        }
    } else {
        let kept = trace.churn.iter().map(|(_, s)| s).filter(|s| {
            paths
                .stack_ok
                .get(s.stack as usize)
                .copied()
                .unwrap_or(false)
        });
        churn::merge_summaries(
            kept,
            &paths.node_of,
            &mut collector.stats,
            &mut collector.totals,
        );
    }

    if capture.attached {
        warnings.push(
            "attached capture: allocations made before the attach are missing, so these are net \
             changes since attach, not exact totals"
                .into(),
        );
    }
    if capture.sampled {
        warnings.push(format!(
            "sampled capture (1 in {}): counts and sizes are estimates after scaling by {}",
            capture.sample_rate, capture.sample_rate
        ));
    }
    if capture.dropped_overflow > 0 {
        warnings.push(format!(
            "{} events were lost to full capture buffers; totals are lower bounds",
            capture.dropped_overflow
        ));
    }
    if capture.min_lifetime_ms > 0 {
        warnings.push(format!(
            "--min-lifetime {} ms was used: short-lived allocations are totals only, so the peak is \
             slightly understated",
            capture.min_lifetime_ms
        ));
    }

    // Sums saturate instead of wrapping. A sum cannot honestly reach u64::MAX, so a field at the
    // ceiling means the trace named sizes that do not fit.
    let t = &collector.totals;
    let saturated = [
        t.alloc_bytes,
        t.freed_bytes,
        t.live_bytes,
        collector.peak.peak.live_bytes,
    ]
    .contains(&u64::MAX)
        || collector
            .stats
            .iter()
            .any(|s| [s.alloc_bytes, s.freed_bytes, s.live_bytes].contains(&u64::MAX));
    if saturated {
        warnings.push(
            "byte totals exceed 2^64 and are capped at the maximum; the trace contains impossible \
             allocation sizes (damaged or hand-made), so sizes and the peak are not reliable"
                .into(),
        );
    }

    let churn = churn::detect(&collector.stats);
    let mut live_groups = end.live_groups();
    live_groups.extend(leaks::live_handles(
        trace,
        &paths.node_of,
        &paths.stack_ok,
        &opts.filter,
    ));
    Analysis {
        capture,
        stats: std::mem::take(&mut collector.stats),
        totals: collector.totals,
        peak: collector.peak.peak,
        timeline: collector.timeline(),
        markers: std::mem::take(&mut collector.markers),
        snapshots: std::mem::take(&mut collector.snapshots),
        live_groups,
        churn,
        short_lived_ns: short_ns,
        tree: paths.tree,
        warnings,
    }
}
