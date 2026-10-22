//! Monotonic nanosecond clock for event timestamps, zero at capture start.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

static BASE: AtomicU64 = AtomicU64::new(0);
static FREQ: AtomicU64 = AtomicU64::new(1);
/// Nanoseconds per tick when the frequency divides 1e9 (10 MHz on current Windows), else 0.
static NS_PER_TICK: AtomicU64 = AtomicU64::new(0);

pub fn init() {
    let (mut f, mut b) = (0i64, 0i64);
    // SAFETY: both pointers are valid locals.
    unsafe {
        QueryPerformanceFrequency(&mut f);
        QueryPerformanceCounter(&mut b);
    }
    let f = f.max(1) as u64;
    FREQ.store(f, Relaxed);
    BASE.store(b as u64, Relaxed);
    NS_PER_TICK.store(
        if 1_000_000_000 % f == 0 {
            1_000_000_000 / f
        } else {
            0
        },
        Relaxed,
    );
}

#[inline]
pub fn now_ns() -> u64 {
    let mut q = 0i64;
    // SAFETY: valid local pointer.
    unsafe { QueryPerformanceCounter(&mut q) };
    let ticks = (q as u64).saturating_sub(BASE.load(Relaxed));
    match NS_PER_TICK.load(Relaxed) {
        0 => (u128::from(ticks) * 1_000_000_000 / u128::from(FREQ.load(Relaxed))) as u64,
        m => ticks * m,
    }
}

/// Wall-clock time of `now_ns() == 0` is approximated by "now minus elapsed", in Unix nanoseconds.
pub fn start_unix_ns() -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    let mut ft = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    // SAFETY: valid local pointer.
    unsafe { GetSystemTimePreciseAsFileTime(&mut ft) };
    let ticks_100ns = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
    ticks_100ns
        .saturating_sub(UNIX_EPOCH_AS_FILETIME)
        .saturating_mul(100)
        .saturating_sub(now_ns())
}
