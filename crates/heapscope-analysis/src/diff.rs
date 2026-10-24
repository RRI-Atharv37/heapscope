//! Snapshot diff: which allocation sites grew, shrank, appeared or disappeared in the live set
//! between two captures, or between two markers of one capture.
//!
//! Sites are matched by call-path identity (see [`crate::identity`]), so the two traces may come
//! from different runs or builds. The numbers compared are live allocations and bytes at the end
//! of the capture (or at the marker).

use std::collections::BTreeMap;

use crate::identity::{self, DEFAULT_DEPTH};
use crate::model::{Analysis, NodeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Change {
    /// Nothing live there before, something live now.
    New,
    /// Something live before, nothing now.
    Gone,
    Grew,
    Shrank,
    Unchanged,
}

impl Change {
    pub fn label(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Gone => "gone",
            Self::Grew => "grew",
            Self::Shrank => "shrank",
            Self::Unchanged => "unchanged",
        }
    }
}

/// Live allocations and bytes of one site on one side of the diff.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Live {
    pub allocs: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffEntry {
    /// The call-path identity the two sides were matched on.
    pub key: String,
    /// `site <- caller <- ...`, for display.
    pub path: String,
    /// Source position of the allocation site, when symbols had one.
    pub location: Option<String>,
    pub before: Live,
    pub after: Live,
    pub change: Change,
}

impl DiffEntry {
    pub fn delta_bytes(&self) -> i64 {
        signed(self.after.bytes) - signed(self.before.bytes)
    }

    pub fn delta_allocs(&self) -> i64 {
        signed(self.after.allocs) - signed(self.before.allocs)
    }
}

fn signed(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

#[derive(Clone, Debug)]
pub struct DiffOptions {
    /// Frames from the allocation site that identify a path.
    pub depth: usize,
    /// Hide entries whose live bytes changed by less than this.
    pub min_change_bytes: u64,
    /// Keep sites that did not change at all.
    pub include_unchanged: bool,
    /// Frames to show in `DiffEntry::path`.
    pub display_frames: usize,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            depth: DEFAULT_DEPTH,
            min_change_bytes: 0,
            include_unchanged: false,
            display_frames: 6,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Diff {
    /// Largest change in live bytes first.
    pub entries: Vec<DiffEntry>,
    pub before: Live,
    pub after: Live,
    /// One side or both is scaled up from a sampled capture.
    pub estimated: bool,
    pub notes: Vec<String>,
}

impl Diff {
    pub fn count(&self, change: Change) -> usize {
        self.entries.iter().filter(|e| e.change == change).count()
    }

    pub fn delta_bytes(&self) -> i64 {
        signed(self.after.bytes) - signed(self.before.bytes)
    }
}

/// A moment in an analysed trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    /// The live set at the marker with this index in `Analysis::markers`.
    Marker(usize),
    /// The live set when the trace ends.
    End,
}

/// Index of the first marker called `label`.
pub fn find_marker(analysis: &Analysis, label: &str) -> Option<usize> {
    analysis.markers.iter().position(|m| m.label == label)
}

/// Live `(allocs, bytes)` per tree node at `point`.
fn live_at(a: &Analysis, point: Point) -> Result<Vec<(NodeId, u64, u64)>, String> {
    match point {
        Point::End => Ok(a
            .stats
            .iter()
            .enumerate()
            .filter(|(_, s)| s.live_allocs > 0)
            .map(|(i, s)| (i as NodeId, s.live_allocs, s.live_bytes))
            .collect()),
        Point::Marker(i) => a
            .snapshots
            .get(i)
            .map(|s| s.live.clone())
            .ok_or_else(|| format!("the trace has no marker number {i}")),
    }
}

type ByKey = BTreeMap<String, (Live, NodeId)>;

fn by_identity(a: &Analysis, live: &[(NodeId, u64, u64)], depth: usize) -> ByKey {
    let scale = a.scale();
    let mut out = ByKey::new();
    for &(node, allocs, bytes) in live {
        let key = identity::path_key(&a.tree, node, depth);
        let e = out.entry(key).or_insert((Live::default(), node));
        e.0.allocs += allocs * scale;
        e.0.bytes += bytes * scale;
    }
    out
}

fn location(a: &Analysis, node: NodeId) -> Option<String> {
    let f = &a.tree.node(node).frame;
    let file = f.file.as_deref()?;
    Some(if f.line > 0 {
        format!("{file}:{}", f.line)
    } else {
        file.to_string()
    })
}

fn compare(
    a: (&Analysis, Point),
    b: (&Analysis, Point),
    opts: &DiffOptions,
) -> Result<Diff, String> {
    let (before, after) = (a.0, b.0);
    let live_a = live_at(before, a.1)?;
    let live_b = live_at(after, b.1)?;
    let ka = by_identity(before, &live_a, opts.depth);
    let kb = by_identity(after, &live_b, opts.depth);

    let mut entries = Vec::new();
    let mut keys: Vec<&String> = ka.keys().chain(kb.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let (live_before, node_before) = ka
            .get(key)
            .map_or((Live::default(), None), |(l, n)| (*l, Some(*n)));
        let (live_after, node_after) = kb
            .get(key)
            .map_or((Live::default(), None), |(l, n)| (*l, Some(*n)));
        let change = match (
            live_before.bytes > 0 || live_before.allocs > 0,
            live_after.bytes > 0 || live_after.allocs > 0,
        ) {
            (false, true) => Change::New,
            (true, false) => Change::Gone,
            (true, true) => {
                match (live_after.bytes.cmp(&live_before.bytes))
                    .then(live_after.allocs.cmp(&live_before.allocs))
                {
                    std::cmp::Ordering::Greater => Change::Grew,
                    std::cmp::Ordering::Less => Change::Shrank,
                    std::cmp::Ordering::Equal => Change::Unchanged,
                }
            }
            (false, false) => continue,
        };
        let entry = DiffEntry {
            key: key.clone(),
            path: match node_after
                .map(|n| (after, n))
                .or(node_before.map(|n| (before, n)))
            {
                Some((an, n)) => an.tree.path_string(n, opts.display_frames),
                None => key.clone(),
            },
            location: node_after
                .and_then(|n| location(after, n))
                .or_else(|| node_before.and_then(|n| location(before, n))),
            before: live_before,
            after: live_after,
            change,
        };
        if change == Change::Unchanged {
            if !opts.include_unchanged {
                continue;
            }
        } else if entry.delta_bytes().unsigned_abs() < opts.min_change_bytes {
            continue;
        }
        entries.push(entry);
    }
    entries.sort_by(|x, y| {
        y.delta_bytes()
            .unsigned_abs()
            .cmp(&x.delta_bytes().unsigned_abs())
            .then(
                y.delta_allocs()
                    .unsigned_abs()
                    .cmp(&x.delta_allocs().unsigned_abs()),
            )
            .then(x.key.cmp(&y.key))
    });

    let total = |live: &[(NodeId, u64, u64)], scale: u64| Live {
        allocs: live.iter().map(|l| l.1).sum::<u64>() * scale,
        bytes: live.iter().map(|l| l.2).sum::<u64>() * scale,
    };

    let mut notes = Vec::new();
    for (which, an) in [("first", before), ("second", after)] {
        if an.capture.attached {
            notes.push(format!(
                "the {which} capture was attached: memory allocated before the attach is not \
                 tracked, so its numbers are net changes since attach"
            ));
        }
        if an.capture.sampled {
            notes.push(format!(
                "the {which} capture was sampled (1 in {}): its numbers are scaled estimates",
                an.capture.sample_rate
            ));
        }
        if !an.capture.complete && matches!(if which == "first" { a.1 } else { b.1 }, Point::End) {
            notes.push(format!(
                "the {which} capture did not shut down cleanly, so its end state is where the trace stops"
            ));
        }
    }
    if before.capture.sample_rate != after.capture.sample_rate {
        notes.push(format!(
            "the captures were sampled at different rates (1 in {} and 1 in {}); small differences \
             may be sampling noise",
            before.capture.sample_rate, after.capture.sample_rate
        ));
    }

    Ok(Diff {
        entries,
        before: total(&live_a, before.scale()),
        after: total(&live_b, after.scale()),
        estimated: before.capture.sampled || after.capture.sampled,
        notes,
    })
}

/// The live set at the end of `before` against the live set at the end of `after`.
pub fn diff(before: &Analysis, after: &Analysis, opts: &DiffOptions) -> Diff {
    compare((before, Point::End), (after, Point::End), opts)
        .expect("end-of-trace snapshots always exist")
}

/// Two points of one capture, for example two markers around a code path.
pub fn diff_points(
    analysis: &Analysis,
    from: Point,
    to: Point,
    opts: &DiffOptions,
) -> Result<Diff, String> {
    compare((analysis, from), (analysis, to), opts)
}
