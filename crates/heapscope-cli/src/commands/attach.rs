//! `heapscope attach --pid N -o out.hstrace [--duration 30s]`
//!
//! Injects the capture DLL into a running process, records until the duration ends or Ctrl-C, then
//! switches the DLL to dormant (hooks pass through) and flushes. The DLL stays loaded and the
//! target keeps running. Only allocations made after the attach are seen, so the result is net
//! growth since attach, not exit-time leaks. Each line typed on stdin becomes a marker.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::sync::Arc;
use std::time::{Duration, Instant};

use heapscope_control::state;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE,
    TH32CS_SNAPMODULE32,
};
use windows_sys::Win32::System::Threading::{
    IsWow64Process2, OpenProcess, WaitForSingleObject, PROCESS_CREATE_THREAD,
    PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
};

use super::{absolute_output, OUTPUT_CLOSED};
use crate::launch::{self, CaptureOptions, Control, DLL_NAME};

pub struct AttachArgs {
    pub pid: u32,
    pub output: PathBuf,
    pub duration: Option<Duration>,
    pub stop_on_eof: bool,
    pub sample_one_in: u32,
    pub min_lifetime_ms: u32,
    pub stack_depth: u32,
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn on_ctrl_c(_kind: u32) -> i32 {
    INTERRUPTED.store(true, SeqCst);
    1
}

struct Process(HANDLE);

impl Process {
    fn open(pid: u32) -> Result<Self, String> {
        let rights = PROCESS_CREATE_THREAD
            | PROCESS_QUERY_INFORMATION
            | PROCESS_VM_OPERATION
            | PROCESS_VM_READ
            | PROCESS_VM_WRITE
            | SYNCHRONIZE;
        // SAFETY: plain Win32 call.
        let h = unsafe { OpenProcess(rights, 0, pid) };
        if h.is_null() {
            let e = std::io::Error::last_os_error();
            return Err(format!(
                "cannot open process {pid}: {e}\n  (it may not exist, may run elevated or protected, or belong to another user; try an elevated prompt)"
            ));
        }
        Ok(Self(h))
    }

    fn has_exited(&self) -> bool {
        // SAFETY: valid handle.
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
    }

    fn is_32_bit(&self) -> bool {
        let (mut process_machine, mut native_machine) = (0u16, 0u16);
        // SAFETY: valid handle and out pointers. A non-zero machine means WOW64 (a 32-bit process).
        unsafe {
            IsWow64Process2(self.0, &mut process_machine, &mut native_machine) != 0
                && process_machine != 0
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: handle opened in `open`.
        unsafe { CloseHandle(self.0) };
    }
}

fn already_injected(pid: u32) -> bool {
    // SAFETY: Toolhelp snapshot walk with a correctly sized entry.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
        if snap.is_null() || snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: MODULEENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;
        let mut found = false;
        let mut more = Module32FirstW(snap, &mut entry) != 0;
        while more {
            let len = entry
                .szModule
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szModule.len());
            if String::from_utf16_lossy(&entry.szModule[..len]).eq_ignore_ascii_case(DLL_NAME) {
                found = true;
                break;
            }
            more = Module32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
        found
    }
}

pub fn attach(args: AttachArgs) -> Result<ExitCode, String> {
    let dll = launch::dll_path()?;
    let process = Process::open(args.pid)?;
    if process.is_32_bit() {
        return Err(format!(
            "process {} is 32-bit; HeapScope captures 64-bit processes only",
            args.pid
        ));
    }
    if already_injected(args.pid) {
        return Err(format!(
            "process {} already has {DLL_NAME} loaded (a previous attach, or a run under HeapScope); a process can be captured once per lifetime",
            args.pid
        ));
    }
    let opts = CaptureOptions {
        output: absolute_output(&args.output)?,
        sample_one_in: args.sample_one_in,
        min_lifetime_ms: args.min_lifetime_ms,
        stack_depth: args.stack_depth,
        attached: true,
    };
    // Before anything is injected: from here on Ctrl-C asks for an orderly stop instead of killing
    // this process. (If it dies anyway, the DLL notices and goes dormant by itself.)
    // SAFETY: the handler only stores to an atomic.
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl_c), 1) };
    let ctl = Arc::new(Control::create(args.pid, &opts)?);
    let injected = launch::inject(process.0, &dll).map_err(|e| ctl.explain(e))?;
    launch::require_capturing(&ctl, injected)?;
    say!(
        "attached to process {}: capturing to {}",
        args.pid,
        opts.output.display()
    );
    say!("press Enter to add a marker; Ctrl-C to stop");

    spawn_marker_reader(Arc::clone(&ctl), args.stop_on_eof);

    let deadline = args.duration.map(|d| Instant::now() + d);
    // A closed stdout means the reader of this output is gone (a pipeline ended); stop as for Ctrl-C.
    while !INTERRUPTED.load(SeqCst)
        && !OUTPUT_CLOSED.load(Relaxed)
        && !process.has_exited()
        && deadline.map_or(true, |d| Instant::now() < d)
    {
        std::thread::sleep(Duration::from_millis(20));
    }

    // Ask the DLL to stop; it goes dormant after its final flush. If the target is gone, the DLL's
    // exit handler already did that.
    let _ = ctl
        .state
        .compare_exchange(state::CAPTURING, state::STOP_REQUESTED, SeqCst, SeqCst);
    let give_up = Instant::now() + Duration::from_secs(120);
    let settled = |s: u32| s == state::DORMANT || s == state::FAILED;
    while !settled(ctl.state.load(Acquire)) && !process.has_exited() && Instant::now() < give_up {
        std::thread::sleep(Duration::from_millis(10));
    }
    match ctl.state.load(Acquire) {
        // The exit handler in the DLL also reports dormant when the target ends, so ask the
        // process, not the DLL, whether it is still there.
        state::FAILED => return Err(ctl.failure_reason()),
        _ if process.has_exited() => say!("process {} exited while capturing", args.pid),
        state::DORMANT => say!(
            "detached from process {}; the program keeps running",
            args.pid
        ),
        _ => {
            return Err(
                "the capture DLL did not finish flushing in time; the trace may be incomplete"
                    .into(),
            )
        }
    }
    say!("heapscope: {}", launch::summary(&ctl, &opts.output));
    Ok(ExitCode::SUCCESS)
}

/// Every line on stdin becomes a marker in the trace; the line is echoed once the DLL has it.
fn spawn_marker_reader(ctl: Arc<Control>, stop_on_eof: bool) {
    let _ = std::thread::Builder::new()
        .name("markers".into())
        .spawn(move || {
            let mut n = 0u32;
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                n += 1;
                let label = if line.trim().is_empty() {
                    format!("marker {n}")
                } else {
                    line.trim().to_string()
                };
                ctl.post_marker(&label);
                let seq = ctl.marker_seq.load(Acquire);
                let until = Instant::now() + Duration::from_secs(2);
                while ctl.marker_ack.load(Acquire) != seq && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(2));
                }
                if ctl.marker_ack.load(Acquire) == seq {
                    say!("marker recorded: {label}");
                } else {
                    say!("marker NOT confirmed by the capture within 2 seconds: {label}");
                }
            }
            if stop_on_eof {
                INTERRUPTED.store(true, SeqCst);
            }
        });
}
