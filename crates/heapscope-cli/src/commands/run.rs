//! `heapscope run -o out.hstrace -- app.exe args`

use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::process::ExitCode;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

use super::absolute_output;
use crate::launch::{self, CaptureOptions, Control};

pub struct RunArgs {
    pub output: PathBuf,
    pub program: Vec<String>,
    pub sample_one_in: u32,
    pub min_lifetime_ms: u32,
    pub stack_depth: u32,
}

pub fn run(args: RunArgs) -> Result<ExitCode, String> {
    let dll = launch::dll_path()?;
    let opts = CaptureOptions {
        output: absolute_output(&args.output)?,
        sample_one_in: args.sample_one_in,
        min_lifetime_ms: args.min_lifetime_ms,
        stack_depth: args.stack_depth,
        attached: false,
    };
    let mut program = args.program;
    // A path (as opposed to a bare name to be found on PATH) is made absolute, which also turns
    // forward slashes into the backslashes CreateProcess expects.
    if program[0].contains(['/', '\\']) {
        if let Ok(full) = std::path::absolute(&program[0]) {
            if full.is_file() {
                program[0] = full.to_string_lossy().into_owned();
            }
        }
    }
    let child = launch::spawn_suspended(&program)?;
    let started = (|| {
        let ctl = Control::create(child.pid, &opts)?;
        launch::inject(child.process.as_raw_handle() as HANDLE, &dll)
            .map_err(|e| ctl.explain(e))?;
        launch::require_capturing(&ctl)?;
        Ok::<_, String>(ctl)
    })();
    let ctl = match started {
        Ok(c) => c,
        Err(e) => {
            child.kill();
            return Err(e);
        }
    };
    // Ctrl-C reaches the whole console group; let the target decide, and keep waiting for it so the
    // trace is finalised.
    // SAFETY: documented way to ignore Ctrl-C in this process.
    unsafe { SetConsoleCtrlHandler(None, 1) };
    child.resume();
    let code = child.wait();
    eprintln!("heapscope: {}", launch::summary(&ctl, &opts.output));
    // Exit codes above 255 (NTSTATUS crashes) cannot be passed through; report them as failure.
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}
