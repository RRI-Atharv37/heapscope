//! The `.hstrace` file format shared by the capture DLL and the analysis tools.
//!
//! A file is a magic, a format version, then CRC-checked chunks: one header chunk, any
//! number of stack-table and event chunks, and a footer chunk written on clean shutdown.
//! A capture that crashes leaves a readable file without a footer. See
//! `docs/trace-format.md` for the byte-level spec.

mod codec;
mod event;
mod header;
mod reader;
mod stack_table;
mod writer;

use std::fmt;
use std::io;

pub use event::{AllocKind, ChurnSummary, Event, HandleKind, StackId, NO_STACK};
pub use header::{CaptureMode, Dropped, Footer, Header};
pub use reader::{Limits, TraceReader};
pub use stack_table::StackTable;
pub use writer::TraceWriter;

pub const MAGIC: [u8; 8] = *b"HSTRACE\0";
/// Bumped for any change an older reader cannot skip (new event tags, new chunk layouts).
pub const FORMAT_MAJOR: u16 = 1;
/// Bumped for compatible additions (fields appended to the header chunk).
pub const FORMAT_MINOR: u16 = 1;

/// Upper bound for one chunk, so a corrupt length cannot trigger a huge allocation.
pub(crate) const MAX_CHUNK_BYTES: u32 = 64 << 20;

pub(crate) const CHUNK_HEADER: u8 = 1;
pub(crate) const CHUNK_STACKS: u8 = 2;
pub(crate) const CHUNK_EVENTS: u8 = 3;
pub(crate) const CHUNK_FOOTER: u8 = 4;

pub(crate) const CODEC_RAW: u8 = 0;
pub(crate) const CODEC_LZ4: u8 = 1;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    BadMagic,
    UnsupportedVersion {
        found: u16,
        supported: u16,
    },
    /// The file ends inside a chunk (for example the process was killed mid-write).
    Truncated,
    ChecksumMismatch,
    Corrupt(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::BadMagic => f.write_str("not a HeapScope trace (bad magic)"),
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "trace format version {found} is not supported (this build reads version {supported})"
            ),
            Self::Truncated => f.write_str("trace file is truncated"),
            Self::ChecksumMismatch => f.write_str("trace chunk failed its checksum"),
            Self::Corrupt(what) => write!(f, "trace file is corrupt: {what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Self::Truncated
        } else {
            Self::Io(e)
        }
    }
}
