//! Process creation, the control-block mapping, and DLL injection. `run` creates a suspended
//! process and injects before it executes a single instruction of its own; `attach` injects into a
//! running one. Both use [`inject`].

use std::ffi::{c_void, OsStr};
use std::io;
use std::ops::Deref;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::Ordering::*;
use std::time::Duration;

use heapscope_control::{error, state, ControlBlock, MAGIC, VERSION};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, VirtualAllocEx, VirtualFreeEx,
    FILE_MAP_ALL_ACCESS, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, CreateRemoteThread, GetExitCodeProcess, ResumeThread, TerminateProcess,
    WaitForSingleObject, CREATE_SUSPENDED, INFINITE, PROCESS_INFORMATION, STARTUPINFOW,
};

pub const DLL_NAME: &str = "heapscope_capture.dll";

/// Options both `run` and `attach` pass to the capture DLL.
pub struct CaptureOptions {
    pub output: PathBuf,
    pub sample_one_in: u32,
    pub min_lifetime_ms: u32,
    pub stack_depth: u32,
    pub attached: bool,
}

/// The CLI's view of the control block shared with the injected DLL.
pub struct Control {
    handle: HANDLE,
    view: *mut ControlBlock,
}

// SAFETY: the block consists of atomics only.
unsafe impl Send for Control {}
unsafe impl Sync for Control {}

impl Deref for Control {
    type Target = ControlBlock;
    fn deref(&self) -> &ControlBlock {
        // SAFETY: `view` is a live mapping of at least `ControlBlock::SIZE` bytes until `drop`.
        unsafe { &*self.view }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        // SAFETY: both were obtained in `create` and are released once.
        unsafe {
            UnmapViewOfFile(
                windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.view.cast(),
                },
            );
            CloseHandle(self.handle);
        }
    }
}

impl Control {
    /// Create the mapping `Local\heapscope-<pid>` and fill in the configuration.
    pub fn create(pid: u32, opts: &CaptureOptions) -> Result<Self, String> {
        let name = ControlBlock::mapping_name(pid);
        // SAFETY: plain Win32 calls with valid arguments.
        let (handle, view, already_existed) = unsafe {
            let h = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                null(),
                PAGE_READWRITE,
                0,
                ControlBlock::SIZE as u32,
                name.as_ptr(),
            );
            if h.is_null() {
                return Err(format!(
                    "cannot create the control block: {}",
                    io::Error::last_os_error()
                ));
            }
            let existed = GetLastError() == windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS;
            (
                h,
                MapViewOfFile(h, FILE_MAP_ALL_ACCESS, 0, 0, ControlBlock::SIZE).Value
                    as *mut ControlBlock,
                existed,
            )
        };
        if view.is_null() {
            // SAFETY: `handle` is valid.
            unsafe { CloseHandle(handle) };
            return Err(format!(
                "cannot map the control block: {}",
                io::Error::last_os_error()
            ));
        }
        let ctl = Control { handle, view };
        if already_existed {
            return Err("another HeapScope session already controls this process".into());
        }
        let out = opts
            .output
            .to_str()
            .ok_or("the output path is not valid Unicode")?;
        if !ctl.set_out_path(out) {
            return Err("the output path is too long".into());
        }
        ctl.sample_one_in.store(opts.sample_one_in, Relaxed);
        ctl.min_lifetime_ms.store(opts.min_lifetime_ms, Relaxed);
        ctl.stack_depth.store(opts.stack_depth, Relaxed);
        ctl.attached.store(u32::from(opts.attached), Relaxed);
        ctl.version.store(VERSION, Relaxed);
        ctl.magic.store(MAGIC, Release); // last: the DLL ignores a block without the magic
        Ok(ctl)
    }

    /// If the DLL reported why it failed to start, say that; otherwise keep the injector's message.
    pub fn explain(&self, injector_error: String) -> String {
        if self.state.load(Acquire) == state::FAILED {
            self.failure_reason()
        } else {
            injector_error
        }
    }

    /// Why the DLL could not start, in words.
    pub fn failure_reason(&self) -> String {
        let what = match self.error_code.load(Relaxed) {
            error::BAD_CONTROL_BLOCK => "the control block was unreadable",
            error::OPEN_OUTPUT => {
                "the trace file could not be created (check the path and permissions)"
            }
            error::WRITE_HEADER => "the trace header could not be written",
            error::HOOK_INSTALL => "the heap hooks could not be installed",
            error::OUT_OF_MEMORY => "no thread-local storage slots were free",
            error::START_THREAD => "the flush thread could not be started",
            error::WRITE_FAILED => "writing the trace failed (disk full?)",
            _ => {
                "it did not start (the DLL may have been blocked by antivirus or Smart App Control)"
            }
        };
        format!("the capture DLL failed to start: {what}")
    }
}

/// `heapscope_capture.dll` next to this executable, or the path in `HEAPSCOPE_DLL`.
pub fn dll_path() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("HEAPSCOPE_DLL") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate heapscope.exe: {e}"))?;
    let dll = exe.with_file_name(DLL_NAME);
    if dll.is_file() {
        Ok(dll)
    } else {
        Err(format!(
            "{} not found next to heapscope.exe ({})",
            DLL_NAME,
            dll.display()
        ))
    }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

/// Quote one argument the way `CommandLineToArgvW` and the MSVC runtime parse it.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// A process created suspended; resumed explicitly.
pub struct Child {
    pub process: OwnedHandle,
    thread: OwnedHandle,
    pub pid: u32,
}

pub fn spawn_suspended(program: &[String]) -> Result<Child, String> {
    let mut cmdline = wide(OsStr::new(
        &program
            .iter()
            .map(|a| quote_arg(a))
            .collect::<Vec<_>>()
            .join(" "),
    ));
    // SAFETY: zeroed STARTUPINFOW/PROCESS_INFORMATION are valid initial values; `cmdline` is mutable and NUL-terminated.
    unsafe {
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        if CreateProcessW(
            null(),
            cmdline.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_SUSPENDED,
            null(),
            null(),
            &si,
            &mut pi,
        ) == 0
        {
            return Err(format!(
                "cannot start '{}': {}",
                program[0],
                io::Error::last_os_error()
            ));
        }
        Ok(Child {
            process: OwnedHandle::from_raw_handle(pi.hProcess as _),
            thread: OwnedHandle::from_raw_handle(pi.hThread as _),
            pid: pi.dwProcessId,
        })
    }
}

impl Child {
    pub fn resume(&self) {
        // SAFETY: the thread handle is valid for the life of `self`.
        unsafe { ResumeThread(self.thread.as_raw_handle() as HANDLE) };
    }

    pub fn kill(&self) {
        // SAFETY: the process handle is valid for the life of `self`.
        unsafe { TerminateProcess(self.process.as_raw_handle() as HANDLE, 1) };
    }

    /// Wait for exit and return the exit code.
    pub fn wait(&self) -> u32 {
        let h = self.process.as_raw_handle() as HANDLE;
        let mut code = 0u32;
        // SAFETY: valid process handle and out pointer.
        unsafe {
            WaitForSingleObject(h, INFINITE);
            GetExitCodeProcess(h, &mut code);
        }
        code
    }
}

/// Make `process` load `dll` by running `LoadLibraryW` in a remote thread. `DllMain` has finished
/// when this returns `Ok`; whether capture really started is in the control block.
pub fn inject(process: HANDLE, dll: &Path) -> Result<(), String> {
    let path = wide(dll.as_os_str());
    let bytes = path.len() * 2;
    // SAFETY: remote allocation and thread creation on a process handle with the needed rights; the
    // remote buffer is freed after the thread finished.
    unsafe {
        let remote = VirtualAllocEx(
            process,
            null(),
            bytes,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        );
        if remote.is_null() {
            return Err(format!(
                "cannot allocate memory in the target: {}",
                io::Error::last_os_error()
            ));
        }
        let free = |remote: *mut c_void| {
            VirtualFreeEx(process, remote, 0, MEM_RELEASE);
        };
        if WriteProcessMemory(process, remote, path.as_ptr().cast(), bytes, null_mut()) == 0 {
            let e = io::Error::last_os_error();
            free(remote);
            return Err(format!("cannot write to the target: {e}"));
        }
        let kernel32 = wide(OsStr::new("kernel32.dll"));
        let Some(load) = GetProcAddress(
            GetModuleHandleW(kernel32.as_ptr()),
            c"LoadLibraryW".as_ptr().cast(),
        ) else {
            free(remote);
            return Err("LoadLibraryW not found".into());
        };
        let thread = CreateRemoteThread(
            process,
            null(),
            0,
            Some(std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                unsafe extern "system" fn(*mut c_void) -> u32,
            >(load)),
            remote,
            0,
            null_mut(),
        );
        if thread.is_null() {
            let e = io::Error::last_os_error();
            free(remote);
            return Err(format!("cannot start a thread in the target (protected process, or the target runs elevated?): {e}"));
        }
        let waited = WaitForSingleObject(thread, 30_000);
        let mut loaded = 0u32;
        windows_sys::Win32::System::Threading::GetExitCodeThread(thread, &mut loaded);
        CloseHandle(thread);
        if waited != WAIT_OBJECT_0 {
            // The loader may still be running in the target; leave the buffer alone.
            return Err(
                "the target did not finish loading the capture DLL within 30 seconds".into(),
            );
        }
        free(remote);
        if loaded == 0 {
            return Err("LoadLibrary failed in the target".into());
        }
    }
    Ok(())
}

/// Wait until the DLL reports `CAPTURING`, or fail with the reason it gave.
pub fn require_capturing(ctl: &Control) -> Result<(), String> {
    for _ in 0..200 {
        match ctl.state.load(Acquire) {
            state::CAPTURING => return Ok(()),
            state::FAILED => return Err(ctl.failure_reason()),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    Err(ctl.failure_reason())
}

/// One-line summary of a finished capture, from the control block.
pub fn summary(ctl: &Control, output: &Path) -> String {
    let events = ctl.events_written.load(Relaxed);
    let stacks = ctl.stacks_written.load(Relaxed);
    let bytes = ctl.bytes_written.load(Relaxed);
    let mut s = format!(
        "{events} events, {stacks} stacks, {:.1} MiB -> {}",
        bytes as f64 / 1_048_576.0,
        output.display()
    );
    let (overflow, folded, sampled) = (
        ctl.dropped_overflow.load(Relaxed),
        ctl.dropped_min_lifetime.load(Relaxed),
        ctl.dropped_sampling.load(Relaxed),
    );
    if folded > 0 {
        s.push_str(&format!(
            "\n  {folded} short-lived allocations folded into per-stack totals (--min-lifetime)"
        ));
    }
    if sampled > 0 {
        s.push_str(&format!("\n  {sampled} allocations skipped by --sample"));
    }
    if overflow > 0 {
        s.push_str(&format!("\n  warning: {overflow} events were lost because the program allocated faster than the trace could be written"));
    }
    if ctl.footer_written.load(Acquire) == 0 {
        s.push_str("\n  warning: the trace was not closed cleanly (the target was killed or crashed); it is readable up to the last complete block");
    }
    s
}
