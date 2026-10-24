//! Peak live size, and the live set at that moment.

use crate::aggregate::{replay, Inputs, Silent};
use crate::model::{PathStats, Peak};

/// Follows the live size during a replay and remembers where it was highest.
#[derive(Default)]
pub(crate) struct PeakTracker {
    pub peak: Peak,
    /// Index of the operation after which the peak was reached.
    index: Option<usize>,
}

impl PeakTracker {
    pub(crate) fn observe(&mut self, index: usize, ts_ns: u64, bytes: u64, allocs: u64) {
        // Strictly greater: the first moment the maximum was reached.
        if self.index.is_none() || bytes > self.peak.live_bytes {
            self.peak = Peak {
                ts_ns,
                live_bytes: bytes,
                live_allocs: allocs,
            };
            self.index = Some(index);
        }
    }

    /// Record each node's share of the live set at the peak into `stats`. Replays the trace up to
    /// the peak, which is cheaper than snapshotting the live set every time the peak grows.
    pub(crate) fn fill_live_set(&self, inp: &Inputs<'_>, stats: &mut [PathStats]) {
        let Some(index) = self.index else { return };
        let state = replay(inp, index + 1, &mut Silent);
        for (s, &(allocs, bytes)) in stats.iter_mut().zip(&state.per_node) {
            s.peak_live_allocs = allocs;
            s.peak_live_bytes = bytes;
        }
    }
}
