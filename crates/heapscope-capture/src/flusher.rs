//! The flush thread and the trace sink.
//!
//! This is the only code that touches the process heap on behalf of the DLL: the thread pins its
//! reentrancy guard for life, so its own allocations are invisible to the hooks. It drains every
//! thread's ring, turns raw records into trace events, sorts each batch by timestamp and hands it
//! to the chunked writer.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufWriter;
use std::os::windows::ffi::OsStringExt;
use std::sync::atomic::Ordering::*;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use heapscope_control::{error, state, ControlBlock};
use heapscope_trace::{
    AllocKind, CaptureMode, ChurnSummary, Dropped, Event, Header, StackId, TraceWriter, NO_STACK,
};
use windows_sys::Win32::System::Environment::GetCommandLineW;
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcessId, SetEvent, Sleep, WaitForSingleObject,
};

use crate::buffer::{self, ThreadBuf, R_ALLOC, R_CHURN, R_FREE, R_MARKER, R_REALLOC, R_STACK};
use crate::control::Config;
use crate::{clock, control, modules, sampling, STATE, STATE_OFF, STATE_STOPPING};

const BATCH_LIMIT: usize = 32 * 1024;
const IDLE_SLEEP_MS: u32 = 5;
/// Auto-reset event the flush thread waits on when idle; producers set it when a ring is full.
static WAKE: core::sync::atomic::AtomicIsize = core::sync::atomic::AtomicIsize::new(0);

/// 0 = run, 1 = a process exit asks the flush thread to park, 2 = parked (outside every lock).
static PARK: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Ask the flush thread to stop at a safe point and wait for it, so that the system killing it
/// during process exit cannot leave the sink locked. Returns once it is parked or after a short wait.
pub fn park() {
    PARK.store(1, SeqCst);
    wake();
    let deadline = Instant::now() + Duration::from_millis(500);
    while PARK.load(SeqCst) != 2 && Instant::now() < deadline {
        // SAFETY: plain Win32 call.
        unsafe { Sleep(1) };
    }
}

/// Wake the flush thread now.
pub fn wake() {
    let h = WAKE.load(Relaxed);
    if h != 0 {
        // SAFETY: the handle is created once, before capture starts, and never closed.
        unsafe { SetEvent(h as *mut core::ffi::c_void) };
    }
}

const STOP_WAIT: Duration = Duration::from_secs(2);
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);

pub struct Sink {
    writer: TraceWriter<BufWriter<File>>,
    stacks: HashMap<u64, StackId>,
    batch: Vec<(u64, Event)>,
    modules: HashSet<u64>,
    events: u64,
    markers_seen: u32,
    write_failed: bool,
    last_flush: Instant,
    dirty: bool,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

fn wide_to_string(p: &[u16]) -> String {
    String::from_utf16_lossy(p)
}

fn build_header(cfg: &Config) -> Header {
    let mut exe = [0u16; 1024];
    // SAFETY: the buffer is writable for its stated length; the command line pointer is valid for the process lifetime.
    let (exe_len, cmd) = unsafe {
        let n =
            GetModuleFileNameW(core::ptr::null_mut(), exe.as_mut_ptr(), exe.len() as u32) as usize;
        let c = GetCommandLineW();
        let mut len = 0;
        while *c.add(len) != 0 {
            len += 1;
        }
        (n, std::slice::from_raw_parts(c, len))
    };
    Header {
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        pid: unsafe { GetCurrentProcessId() },
        exe_path: wide_to_string(&exe[..exe_len]),
        command_line: wide_to_string(cmd),
        pointer_size: 8,
        start_unix_ns: clock::start_unix_ns(),
        capture_mode: if cfg.sample_one_in > 1 {
            CaptureMode::Sampled {
                one_in: cfg.sample_one_in,
            }
        } else {
            CaptureMode::Full
        },
        min_lifetime_ms: cfg.min_lifetime_ms,
        attached: cfg.attached,
    }
}

/// Create the trace file and install the sink. Returns a `heapscope_control::error` code on failure.
pub fn open(cfg: &Config) -> Result<(), u32> {
    if WAKE.load(Relaxed) == 0 {
        // SAFETY: plain Win32 call; a null result just leaves the timed sleep in place.
        let h = unsafe { CreateEventW(core::ptr::null(), 0, 0, core::ptr::null()) };
        WAKE.store(h as isize, Relaxed);
    }
    let path = std::ffi::OsString::from_wide(&cfg.out);
    let file = File::create(&path).map_err(|_| error::OPEN_OUTPUT)?;
    let writer = TraceWriter::new(BufWriter::with_capacity(1 << 20, file), &build_header(cfg))
        .map_err(|_| error::WRITE_HEADER)?;
    let mut sink = Sink {
        writer,
        stacks: HashMap::new(),
        batch: Vec::new(),
        modules: HashSet::new(),
        events: 0,
        markers_seen: control::get().map_or(0, |c| c.marker_seq.load(Acquire)),
        write_failed: false,
        last_flush: Instant::now(),
        dirty: false,
    };
    // An empty trace is still a valid file; also gives a killed process a readable header.
    let _ = sink.writer.flush();
    for m in modules::enumerate_loaded() {
        sink.module_event(m);
    }
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    Ok(())
}

impl Sink {
    fn stack(&self, hash: u64) -> StackId {
        self.stacks.get(&hash).copied().unwrap_or(NO_STACK)
    }

    fn module_event(&mut self, m: modules::ModuleEvent) {
        if m.loaded {
            if !self.modules.insert(m.base) {
                return;
            }
            self.batch.push((
                m.ts,
                Event::ModuleLoad {
                    ts_ns: m.ts,
                    thread: 0,
                    base: m.base,
                    size: m.size,
                    pdb_guid: m.guid,
                    pdb_age: m.age,
                    path: m.path,
                },
            ));
        } else if self.modules.remove(&m.base) {
            self.batch.push((
                m.ts,
                Event::ModuleUnload {
                    ts_ns: m.ts,
                    thread: 0,
                    base: m.base,
                },
            ));
        }
    }

    fn handle(&mut self, rec: &[u64]) {
        let hdr = rec[0];
        let (ty, extra, tid) = (hdr as u8, (hdr >> 24) as u8, (hdr >> 32) as u32);
        match ty {
            R_STACK => {
                let n = extra as usize;
                if rec.len() >= 2 + n {
                    let id = self.writer.intern_stack(&rec[2..2 + n]);
                    self.stacks.insert(rec[1], id);
                }
            }
            R_ALLOC if rec.len() >= 6 => {
                let kind = if extra == 1 {
                    AllocKind::Pool
                } else {
                    AllocKind::Heap
                };
                let ev = Event::Alloc {
                    ts_ns: rec[1],
                    thread: tid,
                    kind,
                    addr: rec[2],
                    size: rec[3],
                    requested: rec[4],
                    stack: self.stack(rec[5]),
                };
                self.batch.push((rec[1], ev));
            }
            R_FREE if rec.len() >= 3 => {
                self.batch.push((
                    rec[1],
                    Event::Free {
                        ts_ns: rec[1],
                        thread: tid,
                        addr: rec[2],
                        stack: NO_STACK,
                    },
                ));
            }
            R_REALLOC if rec.len() >= 8 => {
                let ev = Event::Realloc {
                    ts_ns: rec[1],
                    thread: tid,
                    old_addr: rec[2],
                    new_addr: rec[3],
                    old_size: rec[4],
                    new_size: rec[5],
                    requested: rec[6],
                    stack: self.stack(rec[7]),
                };
                self.batch.push((rec[1], ev));
            }
            R_MARKER if rec.len() >= 3 => {
                let len = (rec[2] as usize).min((rec.len() - 3) * 8);
                let bytes: Vec<u8> = rec[3..]
                    .iter()
                    .flat_map(|w| w.to_le_bytes())
                    .take(len)
                    .collect();
                let label = String::from_utf8_lossy(&bytes).into_owned();
                self.batch.push((
                    rec[1],
                    Event::Marker {
                        ts_ns: rec[1],
                        thread: tid,
                        label,
                    },
                ));
            }
            R_CHURN if rec.len() >= 7 => {
                let summary = ChurnSummary {
                    stack: self.stack(rec[2]),
                    allocs: rec[3],
                    bytes: rec[4],
                    total_lifetime_ns: rec[5],
                    max_concurrent: rec[6],
                };
                self.batch.push((
                    rec[1],
                    Event::ChurnSummary {
                        ts_ns: rec[1],
                        summary,
                    },
                ));
            }
            _ => {}
        }
    }

    /// Push complete chunks to disk now and then, so a killed target leaves a readable trace.
    fn flush_periodically(&mut self) {
        if self.dirty && !self.write_failed && self.last_flush.elapsed() >= FLUSH_INTERVAL {
            let _ = self.writer.flush();
            self.last_flush = Instant::now();
            self.dirty = false;
        }
    }

    /// Sort the batch by time and write it out.
    fn write_batch(&mut self) {
        self.batch.sort_by_key(|(ts, _)| *ts);
        for (_, ev) in self.batch.drain(..) {
            if self.write_failed {
                continue;
            }
            if self.writer.write_event(&ev).is_err() {
                self.write_failed = true;
                control::set_failed(error::WRITE_FAILED);
            } else {
                self.events += 1;
                self.dirty = true;
            }
        }
    }

    /// Markers the CLI asked for, DLL load/unload events, then every ring. Returns events seen.
    fn drain_all(&mut self) -> usize {
        let mut seen = 0;
        if let Some(c) = control::get() {
            let seq = c.marker_seq.load(Acquire);
            if seq != self.markers_seen {
                self.markers_seen = seq;
                let label = String::from_utf8_lossy(&c.marker_label_bytes()).into_owned();
                let ts = clock::now_ns();
                self.batch.push((
                    ts,
                    Event::Marker {
                        ts_ns: ts,
                        thread: 0,
                        label,
                    },
                ));
                c.marker_ack.store(seq, Release);
                seen += 1;
            }
        }
        for m in modules::take_events() {
            self.module_event(m);
            seen += 1;
        }
        loop {
            let mut got = 0;
            buffer::for_each(|tb| {
                // The owner has ended: nothing more can arrive, so write what it still holds back.
                let gone = buffer::owner_gone(tb);
                if gone {
                    // SAFETY: the owning thread is gone; the flush thread is now the only producer.
                    unsafe {
                        sampling::flush_pending(tb);
                        sampling::emit_churn(tb);
                    }
                }
                got += tb.drain(|rec| self.handle(rec));
                if gone {
                    buffer::reclaim_if_done(tb);
                }
            });
            seen += got;
            if got == 0 || self.batch.len() >= BATCH_LIMIT {
                break;
            }
        }
        self.write_batch();
        seen
    }

    fn publish_stats(&self, c: &ControlBlock) {
        let d = buffer::dropped_totals();
        c.events_written.store(self.events, Relaxed);
        c.stacks_written.store(self.stacks.len() as u64, Relaxed);
        c.bytes_written.store(self.writer.bytes_written(), Relaxed);
        c.dropped_overflow.store(d.overflow, Relaxed);
        c.dropped_min_lifetime.store(d.min_lifetime, Relaxed);
        c.dropped_sampling.store(d.sampling, Relaxed);
    }
}

/// Entry point of the flush thread (called after the guard is pinned).
pub fn run() {
    loop {
        if PARK.load(SeqCst) == 1 {
            PARK.store(2, SeqCst);
            loop {
                // SAFETY: plain Win32 call; the process is about to end.
                unsafe { Sleep(1000) };
            }
        }
        let (seen, stop) = {
            let mut guard = SINK.lock().unwrap_or_else(|e| e.into_inner());
            let Some(sink) = guard.as_mut() else { return };
            let seen = sink.drain_all();
            sink.flush_periodically();
            if let Some(c) = control::get() {
                sink.publish_stats(c);
            }
            (
                seen,
                control::get().is_some_and(|c| c.state.load(Acquire) == state::STOP_REQUESTED),
            )
        };
        if stop {
            // SAFETY: called from the flush thread with its guard pinned.
            unsafe { finish_capture(false) };
            return;
        }
        if seen == 0 {
            // SAFETY: plain Win32 call.
            unsafe {
                match WAKE.load(Relaxed) {
                    0 => Sleep(IDLE_SLEEP_MS),
                    h => {
                        WaitForSingleObject(h as *mut core::ffi::c_void, IDLE_SLEEP_MS);
                    }
                }
            }
        }
    }
}

fn wait_until_idle() {
    let deadline = Instant::now() + STOP_WAIT;
    loop {
        let mut busy = false;
        buffer::for_each(|tb: &ThreadBuf| busy |= tb.busy.load(SeqCst) != 0);
        if !busy || Instant::now() > deadline {
            return;
        }
        // SAFETY: plain Win32 call.
        unsafe { Sleep(1) };
    }
}

/// Stop recording, write everything that is left and the footer, and go dormant.
/// `exiting` means the process is shutting down: every other thread is already dead.
pub unsafe fn finish_capture(exiting: bool) {
    if STATE.swap(STATE_STOPPING, SeqCst) == STATE_OFF {
        STATE.store(STATE_OFF, SeqCst);
        return;
    }
    if !exiting {
        wait_until_idle();
    }
    let mut guard = if exiting {
        // The flush thread may have been killed holding the lock; do not hang the exit for it.
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            match SINK.try_lock() {
                Ok(g) => break g,
                Err(std::sync::TryLockError::Poisoned(e)) => break e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => Sleep(1),
                Err(_) => return,
            }
        }
    } else {
        SINK.lock().unwrap_or_else(|e| e.into_inner())
    };
    let Some(mut sink) = guard.take() else { return };
    buffer::for_each(|tb| {
        sampling::flush_pending(tb);
        sampling::emit_churn(tb);
    });
    while sink.drain_all() > 0 {}
    let dropped: Dropped = buffer::dropped_totals();
    if let Some(c) = control::get() {
        sink.publish_stats(c);
    }
    if sink.writer.finish(dropped).is_ok() {
        if let Some(c) = control::get() {
            c.footer_written.store(1, Release);
        }
    }
    STATE.store(STATE_OFF, SeqCst);
    if !exiting {
        buffer::decommit_all();
        // SAFETY: no callback can run once this returns, and the flush thread is the caller.
        modules::remove();
    }
    control::set_state(state::DORMANT);
    // Keep the lock released and the DLL loaded: threads may still be inside hook code.
    drop(guard);
}
