//! Module load/unload log, so a trace can be symbolised later on another machine (pitfall 9).
//!
//! `LdrRegisterDllNotification` tells us about every load and unload, including DLLs that are gone
//! again by the time the program exits. The callback runs under the loader lock, so it only copies
//! a few fixed-size fields into a queue the flush thread empties. The PDB identity (GUID + age from
//! the CodeView debug record) is read while the image is certainly mapped.

use core::ffi::c_void;
use core::ptr::null_mut;

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Memory::{VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE};
use windows_sys::Win32::System::ProcessStatus::{
    K32EnumProcessModules, K32GetModuleFileNameExW, K32GetModuleInformation, MODULEINFO,
};
use windows_sys::Win32::System::Threading::{
    AcquireSRWLockExclusive, GetCurrentProcess, ReleaseSRWLockExclusive, SRWLOCK,
};

const QUEUE_CAP: usize = 512;
const PATH_UNITS: usize = 260;

#[repr(C)]
#[derive(Clone, Copy)]
struct Raw {
    loaded: u32,
    size: u32,
    base: u64,
    ts: u64,
    age: u32,
    path_len: u32,
    guid: [u8; 16],
    path: [u16; PATH_UNITS],
}

pub struct ModuleEvent {
    pub loaded: bool,
    pub ts: u64,
    pub base: u64,
    pub size: u64,
    pub guid: [u8; 16],
    pub age: u32,
    pub path: String,
}

static mut QUEUE: *mut Raw = null_mut();
static mut QUEUE_LEN: usize = 0;
static mut QUEUE_LOCK: SRWLOCK = SRWLOCK { Ptr: null_mut() };

#[repr(C)]
struct UnicodeString {
    length: u16,
    _max: u16,
    buffer: *const u16,
}

#[repr(C)]
struct NotificationData {
    _flags: u32,
    full_name: *const UnicodeString,
    _base_name: *const UnicodeString,
    base: *mut c_void,
    size: u32,
}

type NotifyFn = unsafe extern "system" fn(u32, *const NotificationData, *mut c_void);
type UnregisterFn = unsafe extern "system" fn(*mut c_void) -> i32;
static COOKIE: core::sync::atomic::AtomicPtr<c_void> =
    core::sync::atomic::AtomicPtr::new(null_mut());
type RegisterFn = unsafe extern "system" fn(u32, NotifyFn, *mut c_void, *mut *mut c_void) -> i32;

/// Allocate the queue and start listening. False if the notification API is unavailable.
pub unsafe fn init() -> bool {
    QUEUE = VirtualAlloc(
        core::ptr::null(),
        QUEUE_CAP * core::mem::size_of::<Raw>(),
        MEM_COMMIT | MEM_RESERVE,
        PAGE_READWRITE,
    ) as *mut Raw;
    if QUEUE.is_null() {
        return false;
    }
    let ntdll: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
    let Some(f) = GetProcAddress(
        GetModuleHandleW(ntdll.as_ptr()),
        c"LdrRegisterDllNotification".as_ptr().cast(),
    ) else {
        return false;
    };
    let register: RegisterFn = core::mem::transmute(f);
    let mut cookie = null_mut();
    let ok = register(0, callback, null_mut(), &mut cookie) >= 0;
    if ok {
        COOKIE.store(cookie, core::sync::atomic::Ordering::SeqCst);
    }
    ok
}

/// Stop listening. Needed when start-up fails and the DLL is about to be unloaded: a callback
/// pointing into unmapped code would crash the target on its next DLL load.
pub unsafe fn remove() {
    let cookie = COOKIE.swap(null_mut(), core::sync::atomic::Ordering::SeqCst);
    if cookie.is_null() {
        return;
    }
    let ntdll: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
    if let Some(f) = GetProcAddress(
        GetModuleHandleW(ntdll.as_ptr()),
        c"LdrUnregisterDllNotification".as_ptr().cast(),
    ) {
        let unregister: UnregisterFn = core::mem::transmute(f);
        unregister(cookie);
    }
}

unsafe extern "system" fn callback(reason: u32, data: *const NotificationData, _ctx: *mut c_void) {
    let loaded = match reason {
        1 => true,  // LDR_DLL_NOTIFICATION_REASON_LOADED
        2 => false, // LDR_DLL_NOTIFICATION_REASON_UNLOADED
        _ => return,
    };
    let Some(d) = data.as_ref() else { return };
    let mut ev = Raw {
        loaded: u32::from(loaded),
        size: d.size,
        base: d.base as u64,
        ts: crate::clock::now_ns(),
        age: 0,
        path_len: 0,
        guid: [0; 16],
        path: [0; PATH_UNITS],
    };
    if let Some(name) = d.full_name.as_ref() {
        let n = (name.length as usize / 2).min(PATH_UNITS);
        if !name.buffer.is_null() {
            core::ptr::copy_nonoverlapping(name.buffer, ev.path.as_mut_ptr(), n);
            ev.path_len = n as u32;
        }
    }
    if loaded {
        if let Some((guid, age)) = pdb_identity(d.base as usize) {
            ev.guid = guid;
            ev.age = age;
        }
    }
    AcquireSRWLockExclusive(&raw mut QUEUE_LOCK);
    if QUEUE_LEN < QUEUE_CAP {
        *QUEUE.add(QUEUE_LEN) = ev;
        QUEUE_LEN += 1;
    }
    ReleaseSRWLockExclusive(&raw mut QUEUE_LOCK);
}

/// Take everything the callback queued since the last call.
pub fn take_events() -> Vec<ModuleEvent> {
    // SAFETY: the queue is only touched under its lock.
    unsafe {
        if QUEUE.is_null() {
            return Vec::new();
        }
        AcquireSRWLockExclusive(&raw mut QUEUE_LOCK);
        let out: Vec<ModuleEvent> = (0..QUEUE_LEN).map(|i| convert(&*QUEUE.add(i))).collect();
        QUEUE_LEN = 0;
        ReleaseSRWLockExclusive(&raw mut QUEUE_LOCK);
        out
    }
}

fn convert(r: &Raw) -> ModuleEvent {
    ModuleEvent {
        loaded: r.loaded != 0,
        ts: r.ts,
        base: r.base,
        size: u64::from(r.size),
        guid: r.guid,
        age: r.age,
        path: String::from_utf16_lossy(&r.path[..r.path_len as usize]),
    }
}

/// Every module loaded right now (used once at start; later changes come from the callback).
pub fn enumerate_loaded() -> Vec<ModuleEvent> {
    let mut out = Vec::new();
    // SAFETY: Win32 calls with correctly sized local buffers.
    unsafe {
        let process = GetCurrentProcess();
        let mut mods = vec![null_mut::<c_void>(); 1024];
        let mut needed = 0u32;
        if K32EnumProcessModules(
            process,
            mods.as_mut_ptr() as *mut HMODULE,
            (mods.len() * 8) as u32,
            &mut needed,
        ) == 0
        {
            return out;
        }
        let count = (needed as usize / 8).min(mods.len());
        let ts = crate::clock::now_ns();
        for &m in &mods[..count] {
            let mut info: MODULEINFO = core::mem::zeroed();
            if K32GetModuleInformation(
                process,
                m as HMODULE,
                &mut info,
                core::mem::size_of::<MODULEINFO>() as u32,
            ) == 0
            {
                continue;
            }
            let mut path = [0u16; 1024];
            let len = K32GetModuleFileNameExW(
                process,
                m as HMODULE,
                path.as_mut_ptr(),
                path.len() as u32,
            ) as usize;
            let (guid, age) = pdb_identity(info.lpBaseOfDll as usize).unwrap_or(([0; 16], 0));
            out.push(ModuleEvent {
                loaded: true,
                ts,
                base: info.lpBaseOfDll as u64,
                size: u64::from(info.SizeOfImage),
                guid,
                age,
                path: String::from_utf16_lossy(&path[..len]),
            });
        }
    }
    out
}

/// Read `T` from our own address space without risking a fault if the page is gone.
unsafe fn read<T: Copy>(addr: usize) -> Option<T> {
    let mut v = core::mem::MaybeUninit::<T>::uninit();
    let mut got = 0usize;
    let ok = ReadProcessMemory(
        GetCurrentProcess(),
        addr as *const c_void,
        v.as_mut_ptr() as *mut c_void,
        core::mem::size_of::<T>(),
        &mut got,
    );
    (ok != 0 && got == core::mem::size_of::<T>()).then(|| v.assume_init())
}

/// GUID + age from the module's CodeView (RSDS) debug record, as stored in the image.
pub unsafe fn pdb_identity(base: usize) -> Option<([u8; 16], u32)> {
    const IMAGE_DEBUG_TYPE_CODEVIEW: u32 = 2;
    let nt = base + read::<u32>(base + 0x3c)? as usize;
    if read::<u32>(nt)? != 0x0000_4550 {
        return None;
    }
    let optional = nt + 24;
    if read::<u16>(optional)? != 0x20b {
        return None; // PE32+ only: the target is 64-bit
    }
    let debug_dir = optional + 0x70 + 6 * 8;
    let rva = read::<u32>(debug_dir)? as usize;
    let size = read::<u32>(debug_dir + 4)? as usize;
    for i in 0..(size / 28).min(16) {
        let entry = base + rva + i * 28;
        if read::<u32>(entry + 12)? != IMAGE_DEBUG_TYPE_CODEVIEW {
            continue;
        }
        let raw = base + read::<u32>(entry + 20)? as usize;
        if read::<[u8; 4]>(raw)? != *b"RSDS" {
            continue;
        }
        return Some((read::<[u8; 16]>(raw + 4)?, read::<u32>(raw + 20)?));
    }
    None
}
