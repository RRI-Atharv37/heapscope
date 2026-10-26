//! Runtime control block, DLL side: configuration delivered at start, and the `start`/`stop`/
//! `dormant` state the CLI watches. The CLI creates the mapping (`Local\heapscope-<pid>`) before
//! injecting; for manual injection or debugging the same options can come from environment
//! variables (`HEAPSCOPE_OUT`, `HEAPSCOPE_SAMPLE`, `HEAPSCOPE_MIN_LIFETIME_MS`).
//!
//! Rule: the DLL is never unloaded from a live process. Stopping makes the hooks pass straight
//! through (`crate::STATE`), so a thread that is inside a hook never loses its code.

use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering::*};

use heapscope_control::{ControlBlock, ControlHeader, HEADER_BYTES, MAGIC, VERSION};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Environment::GetEnvironmentVariableW;
use windows_sys::Win32::System::Memory::{MapViewOfFile, OpenFileMappingW, FILE_MAP_ALL_ACCESS};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

static CTL: AtomicPtr<ControlBlock> = AtomicPtr::new(null_mut());

pub struct Config {
    /// UTF-16 path of the trace file, no terminating NUL.
    pub out: Vec<u16>,
    pub sample_one_in: u32,
    pub min_lifetime_ms: u32,
    pub attached: bool,
    pub stack_depth: u32,
}

pub fn get() -> Option<&'static ControlBlock> {
    let p = CTL.load(Acquire);
    // SAFETY: the pointer is a mapped view that is never unmapped.
    unsafe { p.as_ref() }
}

pub fn set_state(state: u32) {
    if let Some(c) = get() {
        c.state.store(state, Release);
    }
}

pub fn set_failed(code: u32) {
    if let Some(c) = get() {
        c.error_code.store(code, Relaxed);
        c.state.store(heapscope_control::state::FAILED, Release);
    }
}

fn env_var(name: &str) -> Option<Vec<u16>> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut buf = vec![0u16; heapscope_control::PATH_UNITS];
    // SAFETY: `wide` is NUL-terminated and `buf` is writable for its full length.
    let n = unsafe { GetEnvironmentVariableW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) }
        as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    buf.truncate(n);
    Some(buf)
}

fn env_u32(name: &str) -> u32 {
    env_var(name)
        .and_then(|v| String::from_utf16(&v).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// If `header` is a HeapScope control block of another version, tell the CLI so: it would
/// otherwise wait for a DLL that never answers and blame antivirus.
unsafe fn tell_version_mismatch(header: *const ControlHeader) {
    if let Some(h) = header.as_ref() {
        if h.magic.load(Acquire) == MAGIC && h.version.load(Relaxed) != VERSION {
            h.report_version_mismatch(VERSION);
        }
    }
}

/// Map the control block of this process, if the CLI made one.
fn open_mapping() -> Option<&'static ControlBlock> {
    // SAFETY: plain Win32 calls; the view is never unmapped.
    unsafe {
        let name = ControlBlock::mapping_name(GetCurrentProcessId());
        let h = OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, name.as_ptr());
        if h.is_null() {
            return None;
        }
        let view = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, ControlBlock::SIZE);
        let p = view.Value as *mut ControlBlock;
        if p.is_null() {
            // The block is smaller than this build's: another version, or something that is not
            // ours. Its first fields are the same in every version, so look at just those.
            let head = MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, HEADER_BYTES);
            CloseHandle(h);
            tell_version_mismatch(head.Value as *const ControlHeader);
            return None;
        }
        CloseHandle(h); // the view keeps the mapping alive
        let c = &*p;
        if c.magic.load(Acquire) != MAGIC {
            return None;
        }
        if c.version.load(Relaxed) != VERSION {
            tell_version_mismatch(p as *const ControlHeader);
            return None;
        }
        CTL.store(p, Release);
        Some(c)
    }
}

pub fn load() -> Option<Config> {
    if let Some(c) = open_mapping() {
        let out = c.out_path_units();
        if out.is_empty() {
            return None;
        }
        return Some(Config {
            out,
            sample_one_in: c.sample_one_in.load(Relaxed),
            min_lifetime_ms: c.min_lifetime_ms.load(Relaxed),
            attached: c.attached.load(Relaxed) != 0,
            stack_depth: c.stack_depth.load(Relaxed),
        });
    }
    Some(Config {
        out: env_var("HEAPSCOPE_OUT")?,
        sample_one_in: env_u32("HEAPSCOPE_SAMPLE"),
        min_lifetime_ms: env_u32("HEAPSCOPE_MIN_LIFETIME_MS"),
        attached: false,
        stack_depth: env_u32("HEAPSCOPE_STACK_DEPTH"),
    })
}
