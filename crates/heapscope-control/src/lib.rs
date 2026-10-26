//! The control block the CLI and the injected capture DLL share through a named file mapping,
//! `Local\heapscope-<pid>`. The CLI creates it before injecting, fills in the configuration,
//! and reads the DLL's progress and statistics back; the DLL never creates it.
//!
//! Plain `#[repr(C)]` with atomics only, so both sides can use it without any locking and
//! without allocating (the DLL reads it while hooks are being installed).

use std::sync::atomic::{AtomicU32, AtomicU64};

pub const MAGIC: u32 = 0x4248_5348; // "HSHB" little endian; just needs to be recognisable
pub const VERSION: u32 = 2;
/// UTF-16 code units available for the output path, including the terminating NUL.
pub const PATH_UNITS: usize = 520;
/// Bytes available for a marker label, including the terminating NUL.
pub const LABEL_BYTES: usize = 128;

/// Values of [`ControlBlock::state`].
pub mod state {
    /// Written by the CLI: configuration is complete, the DLL may start.
    pub const PENDING: u32 = 0;
    /// Written by the DLL once hooks are installed and the trace file is open.
    pub const CAPTURING: u32 = 1;
    /// Written by the CLI: stop recording, flush, then go dormant.
    pub const STOP_REQUESTED: u32 = 2;
    /// Written by the DLL after the final flush. Hooks pass straight through; the DLL stays loaded.
    pub const DORMANT: u32 = 3;
    /// Written by the DLL when start-up failed; see `error_code`.
    pub const FAILED: u32 = 4;
}

/// Values of [`ControlBlock::error_code`] when `state == FAILED`.
pub mod error {
    pub const NONE: u32 = 0;
    pub const BAD_CONTROL_BLOCK: u32 = 1;
    pub const OPEN_OUTPUT: u32 = 2;
    pub const WRITE_HEADER: u32 = 3;
    pub const HOOK_INSTALL: u32 = 4;
    pub const OUT_OF_MEMORY: u32 = 5;
    pub const START_THREAD: u32 = 6;
    /// Writing the trace failed after start (disk full, file closed); the trace is truncated.
    pub const WRITE_FAILED: u32 = 7;
    /// The control block's magic matched but its version did not: the DLL and the CLI are from
    /// different builds. The code carries the DLL's own control version above bit 8 (see
    /// [`version_mismatch`]), so the CLI can name both.
    pub const VERSION_MISMATCH: u32 = 8;
    /// Another capture already has the output file open for writing.
    pub const OUTPUT_IN_USE: u32 = 9;
}

/// The `error_code` a DLL of control version `dll_version` reports for a block of another version.
pub const fn version_mismatch(dll_version: u32) -> u32 {
    error::VERSION_MISMATCH | (dll_version << 8)
}

/// Split an `error_code` into its kind (one of [`error`]) and the DLL version it carries (only
/// meaningful for [`error::VERSION_MISMATCH`]).
pub const fn split_error(code: u32) -> (u32, u32) {
    (code & 0xff, code >> 8)
}

/// Bytes at the start of every version of the control block that never move: [`ControlHeader`].
pub const HEADER_BYTES: usize = std::mem::size_of::<ControlHeader>();

/// The first four fields of [`ControlBlock`], in every version of it. A DLL that finds a block it
/// cannot use (another version, possibly of another size) can still read the magic and version and
/// report the mismatch through `state` and `error_code`.
#[repr(C)]
pub struct ControlHeader {
    pub magic: AtomicU32,
    pub version: AtomicU32,
    pub state: AtomicU32,
    pub error_code: AtomicU32,
}

impl ControlHeader {
    /// Say that the DLL (control version `dll_version`) cannot work with this block.
    pub fn report_version_mismatch(&self, dll_version: u32) {
        use std::sync::atomic::Ordering::{Relaxed, Release};
        self.error_code
            .store(version_mismatch(dll_version), Relaxed);
        self.state.store(state::FAILED, Release);
    }
}

#[repr(C)]
pub struct ControlBlock {
    pub magic: AtomicU32,
    pub version: AtomicU32,
    pub state: AtomicU32,
    pub error_code: AtomicU32,
    /// 0 or 1 = capture everything; N = keep one allocation in N.
    pub sample_one_in: AtomicU32,
    /// 0 = off.
    pub min_lifetime_ms: AtomicU32,
    /// 1 when the DLL was injected into an already running process.
    pub attached: AtomicU32,
    /// Maximum frames per call stack (0 = default).
    pub stack_depth: AtomicU32,
    /// Process id of the CLI that created the block. The DLL watches that process and stops
    /// recording (goes dormant, writes the footer) when it ends, so a CLI that dies mid-capture
    /// cannot leave the target recording for nobody. 0 = no controller (manual injection).
    pub controller_pid: AtomicU32,
    /// Incremented by the CLI after it wrote `marker_label`; the DLL records a marker per change.
    pub marker_seq: AtomicU32,
    /// The DLL copies `marker_seq` here once the marker is in the trace, so the CLI can wait for it.
    pub marker_ack: AtomicU32,
    /// Set to 1 by the DLL when the trace footer was written (a clean shutdown).
    pub footer_written: AtomicU32,
    pub events_written: AtomicU64,
    pub stacks_written: AtomicU64,
    pub bytes_written: AtomicU64,
    pub dropped_overflow: AtomicU64,
    pub dropped_min_lifetime: AtomicU64,
    pub dropped_sampling: AtomicU64,
    pub marker_label: [AtomicU32; LABEL_BYTES / 4],
    pub out_path: [AtomicU32; PATH_UNITS / 2],
}

impl ControlBlock {
    pub const SIZE: usize = std::mem::size_of::<Self>();

    pub fn mapping_name(pid: u32) -> Vec<u16> {
        format!("Local\\heapscope-{pid}\0").encode_utf16().collect()
    }

    pub fn set_out_path(&self, path: &str) -> bool {
        let units: Vec<u16> = path.encode_utf16().collect();
        if units.len() >= PATH_UNITS {
            return false;
        }
        for (i, pair) in self.out_path.iter().enumerate() {
            let lo = units.get(i * 2).copied().unwrap_or(0);
            let hi = units.get(i * 2 + 1).copied().unwrap_or(0);
            pair.store(
                u32::from(lo) | (u32::from(hi) << 16),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        true
    }

    /// UTF-16 output path, without the terminating NUL.
    pub fn out_path_units(&self) -> Vec<u16> {
        let mut out = Vec::new();
        for pair in &self.out_path {
            let v = pair.load(std::sync::atomic::Ordering::Relaxed);
            for u in [v as u16, (v >> 16) as u16] {
                if u == 0 {
                    return out;
                }
                out.push(u);
            }
        }
        out
    }

    /// Store a marker label (truncated to fit) and announce it.
    pub fn post_marker(&self, label: &str) {
        use std::sync::atomic::Ordering::{Relaxed, Release};
        let mut bytes = [0u8; LABEL_BYTES];
        let mut n = label.len().min(LABEL_BYTES - 1);
        while !label.is_char_boundary(n) {
            n -= 1;
        }
        bytes[..n].copy_from_slice(&label.as_bytes()[..n]);
        for (i, chunk) in bytes.chunks_exact(4).enumerate() {
            self.marker_label[i].store(
                u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
                Relaxed,
            );
        }
        self.marker_seq.fetch_add(1, Release);
    }

    /// The most recently posted label.
    pub fn marker_label_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for w in &self.marker_label {
            for b in w.load(std::sync::atomic::Ordering::Relaxed).to_le_bytes() {
                if b == 0 {
                    return out;
                }
                out.push(b);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;

    fn block() -> Box<ControlBlock> {
        // SAFETY: the block is plain atomics, for which all-zero is a valid value.
        unsafe { Box::new(std::mem::zeroed()) }
    }

    #[test]
    fn out_path_round_trips_including_non_ascii() {
        let b = block();
        assert!(b.set_out_path("C:\tmp\tracé.hstrace"));
        assert_eq!(
            String::from_utf16(&b.out_path_units()).unwrap(),
            "C:\tmp\tracé.hstrace"
        );
    }

    #[test]
    fn the_header_is_the_start_of_the_block_and_the_mismatch_code_carries_the_dll_version() {
        assert_eq!(HEADER_BYTES, 16);
        let b = block();
        // SAFETY: `ControlHeader` repeats the first four fields of `ControlBlock`, same layout.
        let h = unsafe { &*(&*b as *const ControlBlock as *const ControlHeader) };
        b.magic.store(MAGIC, Relaxed);
        b.version.store(9, Relaxed);
        assert_eq!(h.version.load(Relaxed), 9);
        h.report_version_mismatch(VERSION);
        assert_eq!(b.state.load(Relaxed), state::FAILED);
        assert_eq!(
            split_error(b.error_code.load(Relaxed)),
            (error::VERSION_MISMATCH, VERSION)
        );
    }

    #[test]
    fn overlong_path_is_rejected() {
        assert!(!block().set_out_path(&"x".repeat(PATH_UNITS)));
    }

    #[test]
    fn marker_label_is_truncated_on_a_char_boundary() {
        let b = block();
        b.post_marker(&"é".repeat(100));
        assert_eq!(b.marker_seq.load(Relaxed), 1);
        let label = String::from_utf8(b.marker_label_bytes()).unwrap();
        assert!(label.len() < LABEL_BYTES && label.chars().all(|c| c == 'é'));
    }
}
