//! Trace events and their delta/varint encoding.
//!
//! Encoder and decoder state (previous timestamp and address) is reset at every chunk
//! boundary, so chunks decode independently of each other.

use crate::codec::{put_i64, put_str, put_u64, Decoder};
use crate::{Error, Result};

/// Index into the stack table. 0 means "no stack recorded".
pub type StackId = u32;
pub const NO_STACK: StackId = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocKind {
    Heap = 0,
    /// Custom pool/arena registered through the C API.
    Pool = 1,
    Virtual = 2,
}

impl AllocKind {
    fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Heap),
            1 => Ok(Self::Pool),
            2 => Ok(Self::Virtual),
            _ => Err(Error::Corrupt("unknown allocation kind")),
        }
    }
}

/// Kernel handle type for the reserved `HandleOpen`/`HandleClose` events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleKind {
    File,
    Key,
    Event,
    Mutant,
    Semaphore,
    Section,
    Pipe,
    Other(u8),
}

impl HandleKind {
    fn to_u8(self) -> u8 {
        match self {
            Self::File => 0,
            Self::Key => 1,
            Self::Event => 2,
            Self::Mutant => 3,
            Self::Semaphore => 4,
            Self::Section => 5,
            Self::Pipe => 6,
            Self::Other(v) => v,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::File,
            1 => Self::Key,
            2 => Self::Event,
            3 => Self::Mutant,
            4 => Self::Semaphore,
            5 => Self::Section,
            6 => Self::Pipe,
            v => Self::Other(v),
        }
    }
}

/// Totals for allocations that `min_lifetime` dropped, per stack, so no churn is lost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChurnSummary {
    pub stack: StackId,
    pub allocs: u64,
    pub bytes: u64,
    pub total_lifetime_ns: u64,
    /// Most dropped allocations of this stack alive at the same moment.
    pub max_concurrent: u64,
}

impl ChurnSummary {
    pub fn record(&mut self, bytes: u64, lifetime_ns: u64) {
        self.allocs += 1;
        self.bytes += bytes;
        self.total_lifetime_ns += lifetime_ns;
    }

    /// Combine two summaries of the same stack (for example from different threads).
    pub fn merge(&mut self, other: &Self) {
        self.allocs += other.allocs;
        self.bytes += other.bytes;
        self.total_lifetime_ns += other.total_lifetime_ns;
        self.max_concurrent = self.max_concurrent.max(other.max_concurrent);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Alloc {
        ts_ns: u64,
        thread: u32,
        kind: AllocKind,
        addr: u64,
        /// Bytes the allocator actually reserved.
        size: u64,
        /// Bytes the program asked for (differs under the Debug CRT).
        requested: u64,
        stack: StackId,
    },
    Free {
        ts_ns: u64,
        thread: u32,
        addr: u64,
        stack: StackId,
    },
    /// A free of a pool object (`AllocKind::Pool`). Pool objects live in their own address
    /// space: one may sit at the address of the heap block that contains it. (Format 1.1.)
    PoolFree {
        ts_ns: u64,
        thread: u32,
        addr: u64,
        stack: StackId,
    },
    Realloc {
        ts_ns: u64,
        thread: u32,
        old_addr: u64,
        new_addr: u64,
        old_size: u64,
        new_size: u64,
        requested: u64,
        stack: StackId,
    },
    Marker {
        ts_ns: u64,
        thread: u32,
        label: String,
    },
    ModuleLoad {
        ts_ns: u64,
        thread: u32,
        base: u64,
        size: u64,
        pdb_guid: [u8; 16],
        pdb_age: u32,
        path: String,
    },
    ModuleUnload {
        ts_ns: u64,
        thread: u32,
        base: u64,
    },
    ThreadName {
        ts_ns: u64,
        thread: u32,
        name: String,
    },
    ChurnSummary {
        ts_ns: u64,
        summary: ChurnSummary,
    },
    /// Reserved for optional kernel-handle tracking.
    HandleOpen {
        ts_ns: u64,
        thread: u32,
        handle: u64,
        kind: HandleKind,
        stack: StackId,
    },
    /// Reserved for optional kernel-handle tracking.
    HandleClose {
        ts_ns: u64,
        thread: u32,
        handle: u64,
        stack: StackId,
    },
}

const TAG_ALLOC: u8 = 1;
const TAG_FREE: u8 = 2;
const TAG_REALLOC: u8 = 3;
const TAG_MARKER: u8 = 4;
const TAG_MODULE_LOAD: u8 = 5;
const TAG_MODULE_UNLOAD: u8 = 6;
const TAG_THREAD_NAME: u8 = 7;
const TAG_CHURN: u8 = 8;
const TAG_HANDLE_OPEN: u8 = 9;
const TAG_HANDLE_CLOSE: u8 = 10;
const TAG_POOL_FREE: u8 = 11;

#[derive(Default)]
pub(crate) struct EventEncoder {
    prev_ts: u64,
    prev_addr: u64,
}

impl EventEncoder {
    fn head(&mut self, buf: &mut Vec<u8>, tag: u8, ts_ns: u64, thread: Option<u32>) {
        buf.push(tag);
        put_i64(buf, ts_ns.wrapping_sub(self.prev_ts) as i64);
        self.prev_ts = ts_ns;
        if let Some(t) = thread {
            put_u64(buf, u64::from(t));
        }
    }

    fn addr(&mut self, buf: &mut Vec<u8>, addr: u64) {
        put_i64(buf, addr.wrapping_sub(self.prev_addr) as i64);
        self.prev_addr = addr;
    }

    pub(crate) fn encode(&mut self, event: &Event, buf: &mut Vec<u8>) {
        match event {
            Event::Alloc {
                ts_ns,
                thread,
                kind,
                addr,
                size,
                requested,
                stack,
            } => {
                self.head(buf, TAG_ALLOC, *ts_ns, Some(*thread));
                buf.push(*kind as u8);
                self.addr(buf, *addr);
                put_u64(buf, *size);
                put_i64(buf, size.wrapping_sub(*requested) as i64);
                put_u64(buf, u64::from(*stack));
            }
            Event::Free {
                ts_ns,
                thread,
                addr,
                stack,
            } => {
                self.head(buf, TAG_FREE, *ts_ns, Some(*thread));
                self.addr(buf, *addr);
                put_u64(buf, u64::from(*stack));
            }
            Event::PoolFree {
                ts_ns,
                thread,
                addr,
                stack,
            } => {
                self.head(buf, TAG_POOL_FREE, *ts_ns, Some(*thread));
                self.addr(buf, *addr);
                put_u64(buf, u64::from(*stack));
            }
            Event::Realloc {
                ts_ns,
                thread,
                old_addr,
                new_addr,
                old_size,
                new_size,
                requested,
                stack,
            } => {
                self.head(buf, TAG_REALLOC, *ts_ns, Some(*thread));
                self.addr(buf, *old_addr);
                self.addr(buf, *new_addr);
                put_u64(buf, *old_size);
                put_u64(buf, *new_size);
                put_i64(buf, new_size.wrapping_sub(*requested) as i64);
                put_u64(buf, u64::from(*stack));
            }
            Event::Marker {
                ts_ns,
                thread,
                label,
            } => {
                self.head(buf, TAG_MARKER, *ts_ns, Some(*thread));
                put_str(buf, label);
            }
            Event::ModuleLoad {
                ts_ns,
                thread,
                base,
                size,
                pdb_guid,
                pdb_age,
                path,
            } => {
                self.head(buf, TAG_MODULE_LOAD, *ts_ns, Some(*thread));
                put_u64(buf, *base);
                put_u64(buf, *size);
                buf.extend_from_slice(pdb_guid);
                put_u64(buf, u64::from(*pdb_age));
                put_str(buf, path);
            }
            Event::ModuleUnload {
                ts_ns,
                thread,
                base,
            } => {
                self.head(buf, TAG_MODULE_UNLOAD, *ts_ns, Some(*thread));
                put_u64(buf, *base);
            }
            Event::ThreadName {
                ts_ns,
                thread,
                name,
            } => {
                self.head(buf, TAG_THREAD_NAME, *ts_ns, Some(*thread));
                put_str(buf, name);
            }
            Event::ChurnSummary { ts_ns, summary } => {
                self.head(buf, TAG_CHURN, *ts_ns, None);
                put_u64(buf, u64::from(summary.stack));
                put_u64(buf, summary.allocs);
                put_u64(buf, summary.bytes);
                put_u64(buf, summary.total_lifetime_ns);
                put_u64(buf, summary.max_concurrent);
            }
            Event::HandleOpen {
                ts_ns,
                thread,
                handle,
                kind,
                stack,
            } => {
                self.head(buf, TAG_HANDLE_OPEN, *ts_ns, Some(*thread));
                put_u64(buf, *handle);
                buf.push(kind.to_u8());
                put_u64(buf, u64::from(*stack));
            }
            Event::HandleClose {
                ts_ns,
                thread,
                handle,
                stack,
            } => {
                self.head(buf, TAG_HANDLE_CLOSE, *ts_ns, Some(*thread));
                put_u64(buf, *handle);
                put_u64(buf, u64::from(*stack));
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct EventDecoder {
    prev_ts: u64,
    prev_addr: u64,
}

impl EventDecoder {
    fn ts(&mut self, d: &mut Decoder<'_>) -> Result<u64> {
        self.prev_ts = self.prev_ts.wrapping_add(d.i64()? as u64);
        Ok(self.prev_ts)
    }

    fn addr(&mut self, d: &mut Decoder<'_>) -> Result<u64> {
        self.prev_addr = self.prev_addr.wrapping_add(d.i64()? as u64);
        Ok(self.prev_addr)
    }

    pub(crate) fn decode(&mut self, d: &mut Decoder<'_>) -> Result<Event> {
        let tag = d.u8()?;
        let ts_ns = self.ts(d)?;
        if tag == TAG_CHURN {
            return Ok(Event::ChurnSummary {
                ts_ns,
                summary: ChurnSummary {
                    stack: d.u32()?,
                    allocs: d.u64()?,
                    bytes: d.u64()?,
                    total_lifetime_ns: d.u64()?,
                    max_concurrent: d.u64()?,
                },
            });
        }
        let thread = d.u32()?;
        Ok(match tag {
            TAG_ALLOC => {
                let kind = AllocKind::from_u8(d.u8()?)?;
                let addr = self.addr(d)?;
                let size = d.u64()?;
                let requested = size.wrapping_sub(d.i64()? as u64);
                Event::Alloc {
                    ts_ns,
                    thread,
                    kind,
                    addr,
                    size,
                    requested,
                    stack: d.u32()?,
                }
            }
            TAG_FREE => {
                let addr = self.addr(d)?;
                Event::Free {
                    ts_ns,
                    thread,
                    addr,
                    stack: d.u32()?,
                }
            }
            TAG_POOL_FREE => {
                let addr = self.addr(d)?;
                Event::PoolFree {
                    ts_ns,
                    thread,
                    addr,
                    stack: d.u32()?,
                }
            }
            TAG_REALLOC => {
                let old_addr = self.addr(d)?;
                let new_addr = self.addr(d)?;
                let old_size = d.u64()?;
                let new_size = d.u64()?;
                let requested = new_size.wrapping_sub(d.i64()? as u64);
                Event::Realloc {
                    ts_ns,
                    thread,
                    old_addr,
                    new_addr,
                    old_size,
                    new_size,
                    requested,
                    stack: d.u32()?,
                }
            }
            TAG_MARKER => Event::Marker {
                ts_ns,
                thread,
                label: d.str()?,
            },
            TAG_MODULE_LOAD => {
                let base = d.u64()?;
                let size = d.u64()?;
                let mut pdb_guid = [0u8; 16];
                pdb_guid.copy_from_slice(d.bytes(16)?);
                let pdb_age = d.u32()?;
                Event::ModuleLoad {
                    ts_ns,
                    thread,
                    base,
                    size,
                    pdb_guid,
                    pdb_age,
                    path: d.str()?,
                }
            }
            TAG_MODULE_UNLOAD => Event::ModuleUnload {
                ts_ns,
                thread,
                base: d.u64()?,
            },
            TAG_THREAD_NAME => Event::ThreadName {
                ts_ns,
                thread,
                name: d.str()?,
            },
            TAG_HANDLE_OPEN => {
                let handle = d.u64()?;
                let kind = HandleKind::from_u8(d.u8()?);
                Event::HandleOpen {
                    ts_ns,
                    thread,
                    handle,
                    kind,
                    stack: d.u32()?,
                }
            }
            TAG_HANDLE_CLOSE => Event::HandleClose {
                ts_ns,
                thread,
                handle: d.u64()?,
                stack: d.u32()?,
            },
            _ => return Err(Error::Corrupt("unknown event tag")),
        })
    }
}
