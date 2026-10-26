//! A trace read into memory in the shape analysis wants: ordered allocation operations plus the
//! side information (modules, markers, churn totals).

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use heapscope_symbols::ModuleInfo;
use heapscope_trace::{
    AllocKind, ChurnSummary, Error, Event, Footer, Header, StackId, StackTable, TraceReader,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpKind {
    Alloc,
    Free,
    /// `old_addr` is freed and `addr` allocated with `size` bytes.
    Realloc,
}

/// One heap operation, flattened from the event stream.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Op {
    pub ts: u64,
    pub kind: OpKind,
    pub thread: u32,
    pub addr: u64,
    pub old_addr: u64,
    pub size: u64,
    pub stack: StackId,
    pub alloc_kind: AllocKind,
}

/// A kernel handle opened (`open`) or closed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HandleOp {
    pub ts: u64,
    pub thread: u32,
    pub handle: u64,
    pub stack: StackId,
    pub open: bool,
}

#[derive(Clone, Debug)]
pub struct MarkerEvent {
    pub ts_ns: u64,
    pub label: String,
}

pub struct Trace {
    pub header: Header,
    /// `None` when the capture was killed before it could write one.
    pub footer: Option<Footer>,
    pub stacks: StackTable,
    pub(crate) ops: Vec<Op>,
    pub(crate) handles: Vec<HandleOp>,
    pub churn: Vec<(u64, ChurnSummary)>,
    pub markers: Vec<MarkerEvent>,
    pub modules: Vec<ModuleInfo>,
    pub thread_names: HashMap<u32, String>,
    pub warnings: Vec<String>,
}

impl Trace {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let file = std::fs::File::open(path)?;
        Self::read(std::io::BufReader::new(file))
    }

    /// Read a whole trace. A trace cut off in the middle (the process was killed) is returned
    /// with every event before the cut and a warning; other damage is an error.
    pub fn read<R: Read>(input: R) -> Result<Self, Error> {
        let mut reader = TraceReader::new(input)?;
        let mut t = Self {
            header: reader.header().clone(),
            footer: None,
            stacks: StackTable::default(),
            ops: Vec::new(),
            handles: Vec::new(),
            churn: Vec::new(),
            markers: Vec::new(),
            modules: Vec::new(),
            thread_names: HashMap::new(),
            warnings: Vec::new(),
        };
        loop {
            match reader.next_event() {
                Ok(Some(e)) => t.push(e),
                Ok(None) => break,
                Err(Error::Truncated) => {
                    t.warnings.push(
                        "the trace file ends in the middle of a chunk; the tail is lost".into(),
                    );
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        t.footer = reader.footer().copied();
        t.stacks = reader.stacks().clone();
        if t.footer.is_none() && t.warnings.is_empty() {
            t.warnings
                .push("the capture did not shut down cleanly (no footer)".into());
        }
        // Stable: operations that share a timestamp keep their file order.
        t.ops.sort_by_key(|o| o.ts);
        t.handles.sort_by_key(|h| h.ts);
        t.markers.sort_by_key(|m| m.ts_ns);
        Ok(t)
    }

    pub fn op_count(&self) -> usize {
        self.ops.len()
    }

    fn push(&mut self, e: Event) {
        match e {
            Event::Alloc {
                ts_ns,
                thread,
                kind,
                addr,
                size,
                stack,
                ..
            } => self.ops.push(Op {
                ts: ts_ns,
                kind: OpKind::Alloc,
                thread,
                addr,
                old_addr: 0,
                size,
                stack,
                alloc_kind: kind,
            }),
            Event::Free {
                ts_ns,
                thread,
                addr,
                stack,
            } => self.ops.push(Op {
                ts: ts_ns,
                kind: OpKind::Free,
                thread,
                addr,
                old_addr: 0,
                size: 0,
                stack,
                alloc_kind: AllocKind::Heap,
            }),
            Event::PoolFree {
                ts_ns,
                thread,
                addr,
                stack,
            } => self.ops.push(Op {
                ts: ts_ns,
                kind: OpKind::Free,
                thread,
                addr,
                old_addr: 0,
                size: 0,
                stack,
                alloc_kind: AllocKind::Pool,
            }),
            Event::Realloc {
                ts_ns,
                thread,
                old_addr,
                new_addr,
                new_size,
                stack,
                ..
            } => self.ops.push(Op {
                ts: ts_ns,
                kind: OpKind::Realloc,
                thread,
                addr: new_addr,
                old_addr,
                size: new_size,
                stack,
                // The replay gives a realloc the kind of the block it replaces.
                alloc_kind: AllocKind::Heap,
            }),
            Event::Marker { ts_ns, label, .. } => self.markers.push(MarkerEvent { ts_ns, label }),
            Event::ModuleLoad {
                base,
                size,
                pdb_guid,
                pdb_age,
                path,
                ..
            } => self.modules.push(ModuleInfo {
                base,
                size,
                pdb_guid,
                pdb_age,
                path,
            }),
            Event::ThreadName { thread, name, .. } => {
                self.thread_names.insert(thread, name);
            }
            Event::ChurnSummary { ts_ns, summary } => self.churn.push((ts_ns, summary)),
            Event::HandleOpen {
                ts_ns,
                thread,
                handle,
                stack,
                ..
            } => self.handles.push(HandleOp {
                ts: ts_ns,
                thread,
                handle,
                stack,
                open: true,
            }),
            Event::HandleClose {
                ts_ns,
                thread,
                handle,
                stack,
            } => self.handles.push(HandleOp {
                ts: ts_ns,
                thread,
                handle,
                stack,
                open: false,
            }),
            Event::ModuleUnload { .. } => {}
        }
    }
}
