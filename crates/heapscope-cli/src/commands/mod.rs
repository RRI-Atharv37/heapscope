/// Write a line to stdout, tolerating a closed pipe: `println!` panics (and the release build aborts)
/// when the reader went away, which would leave a capture running with no controller.
macro_rules! say {
    ($($t:tt)*) => { $crate::commands::emit(false, format_args!($($t)*)) };
}

/// Like [`say!`] for stderr.
macro_rules! say_err {
    ($($t:tt)*) => { $crate::commands::emit(true, format_args!($($t)*)) };
}

pub mod analyze;
pub mod attach;
pub mod diff;
pub mod report;
pub mod run;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Set when a write to stdout or stderr failed: nobody is reading the output any more.
pub static OUTPUT_CLOSED: AtomicBool = AtomicBool::new(false);

pub fn emit(to_stderr: bool, line: std::fmt::Arguments<'_>) {
    let result = if to_stderr {
        writeln!(std::io::stderr(), "{line}")
    } else {
        writeln!(std::io::stdout(), "{line}")
    };
    if result.is_err() {
        OUTPUT_CLOSED.store(true, Ordering::Relaxed);
    }
}

/// Parse `500ms`, `5s`, `2m`, `1h`. A bare number uses `default_unit_ms` (1 = ms, 1000 = s).
pub fn parse_duration_ms(text: &str, default_unit_ms: u64) -> Result<u64, String> {
    let t = text.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let (digits, unit) = t.split_at(split);
    let n: u64 = digits
        .parse()
        .map_err(|_| format!("'{text}' is not a duration (examples: 500ms, 5s, 2m)"))?;
    let factor = match unit {
        "" => default_unit_ms,
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => {
            return Err(format!(
                "unknown time unit '{unit}' in '{text}' (use ms, s, m or h)"
            ))
        }
    };
    n.checked_mul(factor)
        .ok_or_else(|| format!("'{text}' is too long"))
}

/// Absolute form of the output path, because the target may run in another directory.
pub fn absolute_output(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| format!("bad output path {}: {e}", path.display()))
}

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

pub fn signed_bytes(n: i64) -> String {
    let sign = if n < 0 { '-' } else { '+' };
    format!("{sign}{}", bytes(n.unsigned_abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration_ms("500ms", 1000), Ok(500));
        assert_eq!(parse_duration_ms("30s", 1), Ok(30_000));
        assert_eq!(parse_duration_ms("2m", 1), Ok(120_000));
        assert_eq!(parse_duration_ms("7", 1000), Ok(7_000));
        assert_eq!(parse_duration_ms("7", 1), Ok(7));
        assert!(parse_duration_ms("fast", 1).is_err());
        assert!(parse_duration_ms("5x", 1).is_err());
        assert!(parse_duration_ms("", 1).is_err());
    }
}
