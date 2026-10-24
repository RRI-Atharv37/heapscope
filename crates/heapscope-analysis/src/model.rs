//! The result types of an analysis: call tree, per-path statistics, timeline, peak.

use std::collections::HashMap;

pub type NodeId = u32;
/// The synthetic root every call path hangs from.
pub const ROOT: NodeId = 0;

/// One frame of a call path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FrameInfo {
    /// Function name, or `module+0xoffset` for an unresolved frame.
    pub name: String,
    pub module: Option<String>,
    pub file: Option<String>,
    /// 0 when unknown.
    pub line: u32,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub parent: NodeId,
    pub frame: FrameInfo,
    pub children: Vec<NodeId>,
}

/// Call paths merged into a tree. The root is the outermost caller; a node's own statistics
/// belong to allocations whose innermost user frame is that node (the allocation site).
#[derive(Clone, Debug)]
pub struct CallTree {
    nodes: Vec<Node>,
    index: HashMap<(NodeId, FrameInfo), NodeId>,
}

impl Default for CallTree {
    fn default() -> Self {
        Self::new()
    }
}

impl CallTree {
    pub fn new() -> Self {
        let root = Node {
            parent: ROOT,
            frame: FrameInfo {
                name: "<root>".into(),
                module: None,
                file: None,
                line: 0,
            },
            children: Vec::new(),
        };
        Self {
            nodes: vec![root],
            index: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 1
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// The child of `parent` for `frame`, created on first use.
    pub fn child(&mut self, parent: NodeId, frame: FrameInfo) -> NodeId {
        if let Some(&id) = self.index.get(&(parent, frame.clone())) {
            return id;
        }
        let id = self.nodes.len() as NodeId;
        self.nodes.push(Node {
            parent,
            frame: frame.clone(),
            children: Vec::new(),
        });
        self.nodes[parent as usize].children.push(id);
        self.index.insert((parent, frame), id);
        id
    }

    /// Frames from the allocation site (`id`) out to the outermost caller.
    pub fn path(&self, id: NodeId) -> Vec<&FrameInfo> {
        let mut out = Vec::new();
        let mut cur = id;
        while cur != ROOT {
            let n = self.node(cur);
            out.push(&n.frame);
            cur = n.parent;
        }
        out
    }

    /// `site <- caller <- caller ...`, at most `max_frames` frames.
    pub fn path_string(&self, id: NodeId, max_frames: usize) -> String {
        self.path(id)
            .iter()
            .take(max_frames)
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(" <- ")
    }

    /// Statistics of every node summed with its descendants.
    pub fn inclusive(&self, own: &[PathStats]) -> Vec<PathStats> {
        let mut out = own.to_vec();
        // Children always have a higher id than their parent, so one backwards sweep suffices.
        for id in (1..self.nodes.len()).rev() {
            let parent = self.nodes[id].parent as usize;
            let child = out[id];
            out[parent].add(&child);
        }
        out
    }
}

/// Totals for one allocation site.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PathStats {
    /// Allocations, including those a `--min-lifetime` capture folded into churn totals.
    pub allocs: u64,
    pub alloc_bytes: u64,
    pub frees: u64,
    pub freed_bytes: u64,
    /// Still allocated at the end of the trace.
    pub live_allocs: u64,
    pub live_bytes: u64,
    /// Share of the live set at the moment of the global peak.
    pub peak_live_allocs: u64,
    pub peak_live_bytes: u64,
    /// Sum of the lifetimes of all freed allocations.
    pub lifetime_ns: u64,
    /// Allocations freed sooner than the short-lived threshold.
    pub short_allocs: u64,
    pub short_bytes: u64,
    pub short_lifetime_ns: u64,
    /// Most folded allocations alive at once (only known for `--min-lifetime` captures).
    pub max_concurrent: u64,
}

impl PathStats {
    pub fn add(&mut self, o: &Self) {
        self.allocs += o.allocs;
        self.alloc_bytes += o.alloc_bytes;
        self.frees += o.frees;
        self.freed_bytes += o.freed_bytes;
        self.live_allocs += o.live_allocs;
        self.live_bytes += o.live_bytes;
        self.peak_live_allocs += o.peak_live_allocs;
        self.peak_live_bytes += o.peak_live_bytes;
        self.lifetime_ns += o.lifetime_ns;
        self.short_allocs += o.short_allocs;
        self.short_bytes += o.short_bytes;
        self.short_lifetime_ns += o.short_lifetime_ns;
        self.max_concurrent += o.max_concurrent;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimelinePoint {
    pub ts_ns: u64,
    pub live_bytes: u64,
    pub live_allocs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerInfo {
    pub ts_ns: u64,
    pub label: String,
    pub live_bytes: u64,
    pub live_allocs: u64,
}

/// The moment the live heap was largest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Peak {
    pub ts_ns: u64,
    pub live_bytes: u64,
    pub live_allocs: u64,
}

/// A short-lived allocation site.
#[derive(Clone, Debug, PartialEq)]
pub struct ChurnEntry {
    pub node: NodeId,
    pub allocs: u64,
    pub bytes: u64,
    pub avg_lifetime_ns: u64,
    pub max_concurrent: u64,
    /// Share of this site's allocations that were short-lived, 0.0 to 1.0.
    pub share: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub allocs: u64,
    pub alloc_bytes: u64,
    pub frees: u64,
    pub freed_bytes: u64,
    pub live_allocs: u64,
    pub live_bytes: u64,
    /// Frees of memory the trace never saw allocated (normal when the capture was attached).
    pub unmatched_frees: u64,
    /// Allocations at an address that was still live, so its free was lost.
    pub missed_frees: u64,
}

/// How the trace was captured, so consumers can tell exact numbers from estimates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureInfo {
    pub exe_path: String,
    pub command_line: String,
    pub pid: u32,
    pub pointer_size: u8,
    pub sampled: bool,
    /// 1 when every allocation was kept.
    pub sample_rate: u32,
    pub min_lifetime_ms: u32,
    pub attached: bool,
    /// The capture shut down cleanly (the trace has a footer).
    pub complete: bool,
    pub dropped_overflow: u64,
    pub dropped_min_lifetime: u64,
    pub dropped_sampling: u64,
}

impl CaptureInfo {
    /// `full`, `sampled` or `attached`.
    pub fn mode(&self) -> &'static str {
        if self.attached {
            "attached"
        } else if self.sampled {
            "sampled"
        } else {
            "full"
        }
    }

    /// Numbers are estimates or lower bounds rather than exact counts.
    pub fn is_estimate(&self) -> bool {
        self.sampled || self.attached
    }
}

#[derive(Clone, Debug)]
pub struct Analysis {
    pub capture: CaptureInfo,
    pub tree: CallTree,
    /// Own statistics per tree node, indexed by `NodeId`.
    pub stats: Vec<PathStats>,
    pub totals: Totals,
    pub peak: Peak,
    pub timeline: Vec<TimelinePoint>,
    pub markers: Vec<MarkerInfo>,
    pub churn: Vec<ChurnEntry>,
    /// Threshold used to call an allocation short-lived.
    pub short_lived_ns: u64,
    pub warnings: Vec<String>,
}

impl Analysis {
    /// Multiplier that turns counts and bytes of a sampled trace into estimates (1 otherwise).
    pub fn scale(&self) -> u64 {
        u64::from(self.capture.sample_rate.max(1))
    }

    /// Nodes with own statistics, largest first by `key`.
    pub fn top_sites(&self, key: impl Fn(&PathStats) -> u64, limit: usize) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = (1..self.tree.len() as NodeId)
            .filter(|&i| key(&self.stats[i as usize]) > 0)
            .collect();
        ids.sort_by_key(|&i| std::cmp::Reverse(key(&self.stats[i as usize])));
        ids.truncate(limit);
        ids
    }
}
