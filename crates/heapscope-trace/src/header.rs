//! File header (written first) and footer (written on clean shutdown).

use crate::codec::{put_str, put_u64, Decoder};
use crate::{Error, Result};

/// How much of the program's allocation traffic the trace holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    /// Every allocation.
    Full,
    /// One allocation in `one_in`, chosen by address hash; its free is kept too.
    Sampled { one_in: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub tool_version: String,
    pub pid: u32,
    pub exe_path: String,
    pub command_line: String,
    /// 4 or 8.
    pub pointer_size: u8,
    /// Wall-clock time of `ts_ns == 0`, nanoseconds since the Unix epoch.
    pub start_unix_ns: u64,
    pub capture_mode: CaptureMode,
    /// Allocations freed sooner than this are folded into `ChurnSummary` events. 0 = off.
    pub min_lifetime_ms: u32,
    /// The tool joined a running process, so allocations made earlier are missing.
    pub attached: bool,
}

impl Header {
    /// True when the trace holds only part of the traffic, which a CI gate must not trust.
    pub fn is_partial(&self) -> bool {
        self.attached || matches!(self.capture_mode, CaptureMode::Sampled { .. })
    }

    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        put_str(buf, &self.tool_version);
        put_u64(buf, u64::from(self.pid));
        put_str(buf, &self.exe_path);
        put_str(buf, &self.command_line);
        buf.push(self.pointer_size);
        put_u64(buf, self.start_unix_ns);
        match self.capture_mode {
            CaptureMode::Full => buf.push(0),
            CaptureMode::Sampled { one_in } => {
                buf.push(1);
                put_u64(buf, u64::from(one_in));
            }
        }
        put_u64(buf, u64::from(self.min_lifetime_ms));
        buf.push(u8::from(self.attached));
    }

    /// Trailing bytes are ignored so a minor version can append fields.
    pub(crate) fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        let tool_version = d.str()?;
        let pid = d.u32()?;
        let exe_path = d.str()?;
        let command_line = d.str()?;
        let pointer_size = d.u8()?;
        if pointer_size != 4 && pointer_size != 8 {
            return Err(Error::Corrupt("pointer size must be 4 or 8"));
        }
        let start_unix_ns = d.u64()?;
        let capture_mode = match d.u8()? {
            0 => CaptureMode::Full,
            1 => {
                let one_in = d.u32()?;
                if one_in == 0 {
                    return Err(Error::Corrupt("sampling rate of zero"));
                }
                CaptureMode::Sampled { one_in }
            }
            _ => return Err(Error::Corrupt("unknown capture mode")),
        };
        let min_lifetime_ms = d.u32()?;
        let attached = d.u8()? != 0;
        Ok(Self {
            tool_version,
            pid,
            exe_path,
            command_line,
            pointer_size,
            start_unix_ns,
            capture_mode,
            min_lifetime_ms,
            attached,
        })
    }
}

/// Events the capture side lost, so analysis can say how complete the trace is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dropped {
    /// Lost to full buffers.
    pub overflow: u64,
    /// Allocation/free pairs folded into `ChurnSummary` by `min_lifetime_ms`.
    pub min_lifetime: u64,
    /// Allocations skipped by sampling.
    pub sampling: u64,
}

/// Present only when the capture shut down cleanly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Footer {
    pub event_count: u64,
    pub stack_count: u64,
    pub dropped: Dropped,
}

impl Footer {
    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        put_u64(buf, self.event_count);
        put_u64(buf, self.stack_count);
        put_u64(buf, self.dropped.overflow);
        put_u64(buf, self.dropped.min_lifetime);
        put_u64(buf, self.dropped.sampling);
    }

    pub(crate) fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            event_count: d.u64()?,
            stack_count: d.u64()?,
            dropped: Dropped {
                overflow: d.u64()?,
                min_lifetime: d.u64()?,
                sampling: d.u64()?,
            },
        })
    }
}
