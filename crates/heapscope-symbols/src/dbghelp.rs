//! Thin wrapper over DbgHelp. DbgHelp is single-threaded, so every call goes through `&mut self`.

use std::collections::HashSet;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::System::Diagnostics::Debug::{
    SymCleanup, SymFromAddrW, SymGetLineFromAddrW64, SymInitializeW, SymLoadModuleExW,
    SymSetOptions, IMAGEHLP_LINEW64, SYMBOL_INFOW, SYMOPT_DEFERRED_LOADS,
    SYMOPT_FAIL_CRITICAL_ERRORS, SYMOPT_LOAD_LINES, SYMOPT_NO_PROMPTS, SYMOPT_UNDNAME,
};

use crate::{ModuleInfo, Symbol};

/// DbgHelp identifies a session by a process handle but never uses it as one when we pass
/// `fInvadeProcess = FALSE`; any unique value works, so several sessions can coexist (tests).
static NEXT_HANDLE: AtomicUsize = AtomicUsize::new(0x4853_0000);

const MAX_NAME: usize = 1024;

pub(crate) struct Session {
    handle: *mut c_void,
    loaded: HashSet<u64>,
    ok: bool,
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// # Safety
/// `p` must point at a NUL-terminated UTF-16 string.
unsafe fn from_wide(p: *const u16) -> String {
    let mut n = 0;
    // SAFETY: guaranteed by the caller.
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

impl Session {
    pub(crate) fn new(search_path: Option<&str>) -> Self {
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed) as *mut c_void;
        let path = search_path.map(wide);
        // SAFETY: plain FFI; the path buffer outlives the call.
        let ok = unsafe {
            SymSetOptions(
                SYMOPT_UNDNAME
                    | SYMOPT_DEFERRED_LOADS
                    | SYMOPT_LOAD_LINES
                    | SYMOPT_FAIL_CRITICAL_ERRORS
                    | SYMOPT_NO_PROMPTS,
            );
            SymInitializeW(
                handle,
                path.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
                0,
            ) != 0
        };
        Self {
            handle,
            loaded: HashSet::new(),
            ok,
        }
    }

    fn ensure_loaded(&mut self, m: &ModuleInfo) {
        if !self.loaded.insert(m.base) {
            return;
        }
        let image = wide(&m.path);
        // SAFETY: plain FFI; `image` is NUL-terminated and outlives the call. A failure (the
        // image is gone) is not retried per address: later lookups simply find nothing.
        unsafe {
            SymLoadModuleExW(
                self.handle,
                std::ptr::null_mut(),
                image.as_ptr(),
                std::ptr::null(),
                m.base,
                u32::try_from(m.size).unwrap_or(u32::MAX),
                std::ptr::null(),
                0,
            );
        }
    }

    pub(crate) fn lookup(&mut self, m: &ModuleInfo, addr: u64) -> Option<Symbol> {
        if !self.ok {
            return None;
        }
        self.ensure_loaded(m);
        // u64 backing keeps the SYMBOL_INFOW header 8-byte aligned.
        let bytes = std::mem::size_of::<SYMBOL_INFOW>() + MAX_NAME * 2;
        let mut buf = vec![0u64; bytes.div_ceil(8)];
        let info = buf.as_mut_ptr().cast::<SYMBOL_INFOW>();
        let mut disp = 0u64;
        // SAFETY: `buf` holds the header plus MAX_NAME UTF-16 units, as MaxNameLen says.
        let function = unsafe {
            (*info).SizeOfStruct = std::mem::size_of::<SYMBOL_INFOW>() as u32;
            (*info).MaxNameLen = MAX_NAME as u32;
            if SymFromAddrW(self.handle, addr, &mut disp, info) == 0 {
                return None;
            }
            from_wide((*info).Name.as_ptr())
        };

        let mut line = IMAGEHLP_LINEW64 {
            SizeOfStruct: std::mem::size_of::<IMAGEHLP_LINEW64>() as u32,
            ..Default::default()
        };
        let mut line_disp = 0u32;
        // SAFETY: `line` carries its size; FileName points into DbgHelp memory valid until the next call.
        let (file, number) = unsafe {
            if SymGetLineFromAddrW64(self.handle, addr, &mut line_disp, &mut line) != 0
                && !line.FileName.is_null()
            {
                (Some(from_wide(line.FileName)), line.LineNumber)
            } else {
                (None, 0)
            }
        };
        Some(Symbol {
            function,
            file,
            line: number,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.ok {
            // SAFETY: the session was initialised by `new`.
            unsafe { SymCleanup(self.handle) };
        }
    }
}
