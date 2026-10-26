//! `heapscope run -o out.hstrace -- app.exe args`

use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::process::ExitCode;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

use super::absolute_output;
use crate::launch::{self, CaptureOptions, Control, KillOnClose};

pub struct RunArgs {
    pub output: PathBuf,
    pub program: Vec<String>,
    pub sample_one_in: u32,
    pub min_lifetime_ms: u32,
    pub stack_depth: u32,
}

pub fn run(args: RunArgs) -> Result<ExitCode, String> {
    let exe = launch::resolve_program(&args.program[0])?;
    let dll = launch::dll_path()?;
    let opts = CaptureOptions {
        output: absolute_output(&args.output)?,
        sample_one_in: args.sample_one_in,
        min_lifetime_ms: args.min_lifetime_ms,
        stack_depth: args.stack_depth,
        attached: false,
    };
    // Ctrl-C reaches the whole console group; let the target decide, and keep waiting for it so the
    // trace is finalised. Set before the child exists, so no window remains in which Ctrl-C ends
    // this process between creating the child and finishing the injection.
    // SAFETY: documented way to ignore Ctrl-C in this process.
    unsafe { SetConsoleCtrlHandler(None, 1) };

    // If this process dies in any other way (closed console, kill), the job takes the child along
    // instead of leaving it suspended or captured with nobody listening.
    let job = KillOnClose::new();
    let child = launch::spawn_suspended(&exe, &args.program)?;
    if let Some(job) = &job {
        job.add(child.process.as_raw_handle() as HANDLE);
    }
    let started = (|| {
        let ctl = Control::create(child.pid, &opts)?;
        let injected = launch::inject(child.process.as_raw_handle() as HANDLE, &dll)
            .map_err(|e| ctl.explain(e))?;
        launch::require_capturing(&ctl, injected)?;
        Ok::<_, String>(ctl)
    })();
    let ctl = match started {
        Ok(c) => c,
        Err(e) => {
            child.kill();
            return Err(e);
        }
    };
    child.resume();
    let code = child.wait();
    if let Some(job) = &job {
        job.disarm();
    }
    say_err!("heapscope: {}", launch::summary(&ctl, &opts.output));
    // Exit codes above 255 (NTSTATUS crashes) cannot be passed through; report them as failure.
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}
