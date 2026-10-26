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

use heapscope_control::{error, split_error, state, ControlBlock, MAGIC, VERSION};
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
        ctl.controller_pid.store(std::process::id(), Relaxed);
        // `HEAPSCOPE_TEST_CONTROL_VERSION` exists for the tests: it plays a CLI of another build,
        // to check what the user is told when the DLL does not match.
        let version = std::env::var("HEAPSCOPE_TEST_CONTROL_VERSION")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(VERSION);
        ctl.version.store(version, Relaxed);
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
        let (kind, dll_version) = split_error(self.error_code.load(Relaxed));
        if kind == error::VERSION_MISMATCH {
            return format!(
                "the capture DLL belongs to a different HeapScope build: it uses control block \
                 version {dll_version}, this heapscope.exe uses version {}. Use the \
                 heapscope_capture.dll that came with this heapscope.exe (check HEAPSCOPE_DLL)",
                self.version.load(Relaxed)
            );
        }
        if kind == error::WRITE_FAILED {
            return "writing the trace failed (disk full?); the trace is incomplete".into();
        }
        let what = match kind {
            error::BAD_CONTROL_BLOCK => "the control block was unreadable",
            error::OPEN_OUTPUT => {
                "the trace file could not be created (check the path and permissions)"
            }
            error::WRITE_HEADER => "the trace header could not be written",
            error::HOOK_INSTALL => "the heap hooks could not be installed",
            error::OUT_OF_MEMORY => "no thread-local storage slots or memory were available",
            error::START_THREAD => "the flush thread could not be started",
            error::WRITE_FAILED => "writing the trace failed",
            error::OUTPUT_IN_USE => {
                "another capture is writing to that trace file (use a different -o)"
            }
            _ => {
                "it did not start (the DLL may have been blocked by antivirus or Smart App Control)"
            }
        };
        format!("the capture DLL failed to start: {what}")
    }
}

/// `heapscope_capture.dll` next to this executable, or the full path in `HEAPSCOPE_DLL`.
///
/// The path is handed to `LoadLibraryW` *inside the target*, which searches a relative name in its
/// own directories first, so only a full path to an existing file is accepted.
pub fn dll_path() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("HEAPSCOPE_DLL").filter(|p| !p.is_empty()) {
        let path = PathBuf::from(p);
        if !path.is_absolute() {
            return Err(format!(
                "HEAPSCOPE_DLL must be a full path (like C:\\tools\\heapscope_capture.dll), not `{}`: \
                 the program being profiled would look for a relative name in its own folders",
                path.display()
            ));
        }
        if !path.is_file() {
            return Err(format!(
                "HEAPSCOPE_DLL names a file that does not exist: {}",
                path.display()
            ));
        }
        return Ok(path);
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

/// The executable `name` stands for. A name with a directory is taken as it is (made absolute); a
/// bare name is looked up in the absolute directories of `PATH` only, never in the current
/// directory, which `CreateProcess` would search first. A missing extension means `.exe`.
/// Batch files and scripts are refused: they would run `cmd.exe`, which reinterprets `& | ^ %` in
/// the arguments, and the process HeapScope would profile is `cmd.exe`, not the program.
pub fn resolve_program(name: &str) -> Result<PathBuf, String> {
    let not_found = || {
        format!(
            "cannot start '{name}': not found{}",
            if name.contains(['/', '\\']) {
                ""
            } else {
                " on PATH (the current directory is not searched; use .\\name.exe to run a program here)"
            }
        )
    };
    let with_extension = |p: PathBuf| -> Option<PathBuf> {
        if p.extension().is_some() {
            return p.is_file().then_some(p);
        }
        let exe = p.with_extension("exe");
        exe.is_file().then_some(exe)
    };
    let found = if name.contains(['/', '\\']) || name.contains(':') {
        let full = std::path::absolute(name).map_err(|_| not_found())?;
        with_extension(full)
    } else {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .filter(|d| d.is_absolute())
                .find_map(|d| with_extension(d.join(name)))
        })
    };
    let path = found.ok_or_else(not_found)?;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if ext != "exe" && ext != "com" {
        return Err(format!(
            "cannot start '{}': HeapScope profiles native programs (.exe), not {}scripts; \
             name the .exe the script starts instead",
            path.display(),
            if ext == "bat" || ext == "cmd" {
                "batch "
            } else {
                ""
            }
        ));
    }
    Ok(path)
}

/// Kills everything in it when the last handle closes, so a CLI that is killed or crashes does not
/// leave a suspended or captured child behind.
pub struct KillOnClose(OwnedHandle);

impl KillOnClose {
    pub fn new() -> Option<Self> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        // SAFETY: plain Win32 calls; the zeroed limit structure is a valid initial value.
        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return None;
            }
            let job = OwnedHandle::from_raw_handle(job as _);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = windows_sys::Win32::System::JobObjects::SetInformationJobObject(
                job.as_raw_handle() as HANDLE,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of_val(&info) as u32,
            );
            (ok != 0).then_some(Self(job))
        }
    }

    pub fn add(&self, process: HANDLE) -> bool {
        // SAFETY: both handles are valid. Fails if the process is in a job that forbids nesting.
        unsafe {
            windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                self.0.as_raw_handle() as HANDLE,
                process,
            ) != 0
        }
    }

    /// Normal end: let anything the target left running keep running.
    pub fn disarm(&self) {
        use windows_sys::Win32::System::JobObjects::{
            JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        };
        // SAFETY: as in `new`.
        unsafe {
            let info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            SetInformationJobObject(
                self.0.as_raw_handle() as HANDLE,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of_val(&info) as u32,
            );
        }
    }
}

/// A process created suspended; resumed explicitly.
pub struct Child {
    pub process: OwnedHandle,
    thread: OwnedHandle,
    pub pid: u32,
}

/// Create `program[0]` (already resolved to a full path) suspended, with the rest as arguments.
pub fn spawn_suspended(exe: &Path, program: &[String]) -> Result<Child, String> {
    let mut args = vec![exe.to_string_lossy().into_owned()];
    args.extend(program.iter().skip(1).cloned());
    let mut cmdline = wide(OsStr::new(
        &args
            .iter()
            .map(|a| quote_arg(a))
            .collect::<Vec<_>>()
            .join(" "),
    ));
    let application = wide(exe.as_os_str());
    // SAFETY: zeroed STARTUPINFOW/PROCESS_INFORMATION are valid initial values; `cmdline` is mutable and NUL-terminated.
    unsafe {
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        if CreateProcessW(
            application.as_ptr(),
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

/// What the remote `LoadLibraryW` thread did. Whether capture really started is decided by the
/// control block, not by this: the thread's 32-bit exit code is only the low half of an `HMODULE`
/// (a legitimate base can have zero there), and a slow loader can outlast the wait.
#[derive(Clone, Copy)]
pub struct Injected {
    /// The thread ended (as opposed to still running when we stopped waiting).
    pub finished: bool,
    /// `LoadLibraryW` returned something non-zero in its low 32 bits.
    pub returned_module: bool,
}

/// Make `process` load `dll` by running `LoadLibraryW` in a remote thread. Returns once the thread
/// ended or after 30 seconds; the caller then asks [`require_capturing`].
pub fn inject(process: HANDLE, dll: &Path) -> Result<Injected, String> {
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
            return Ok(Injected {
                finished: false,
                returned_module: false,
            });
        }
        free(remote);
        Ok(Injected {
            finished: true,
            returned_module: loaded != 0,
        })
    }
}

/// Wait until the DLL reports `CAPTURING`, or fail with the reason it gave. The control block is
/// the judge: a thread that finished with a null result may be a truncated `HMODULE`.
pub fn require_capturing(ctl: &Control, injected: Injected) -> Result<(), String> {
    // A finished load that returned nothing gets a short grace period; a loader that is still
    // running (antivirus scanning the DLL, say) gets the time it asked for.
    let polls = if injected.finished && !injected.returned_module {
        100
    } else if injected.finished {
        500
    } else {
        3000
    };
    for _ in 0..polls {
        match ctl.state.load(Acquire) {
            state::CAPTURING => return Ok(()),
            state::FAILED => return Err(ctl.failure_reason()),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    if !injected.finished {
        return Err(
            "the target did not finish loading the capture DLL within 30 seconds, and the DLL \
             never reported in"
                .into(),
        );
    }
    if !injected.returned_module {
        return Err(
            "LoadLibrary failed in the target: the DLL could not be loaded (a missing \
                    dependency, or blocked by antivirus or Smart App Control)"
                .into(),
        );
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
    if ctl.state.load(Acquire) == state::FAILED
        && split_error(ctl.error_code.load(Relaxed)).0 == error::WRITE_FAILED
    {
        s.push_str("\n  warning: writing the trace failed (disk full?); the trace ends where the writing stopped");
    } else if ctl.footer_written.load(Acquire) == 0 {
        s.push_str("\n  warning: the trace was not closed cleanly (the target was killed or crashed); it is readable up to the last complete block");
    }
    s
}
