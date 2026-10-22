//! Streaming trace reader.

use std::io::Read;

use crate::codec::Decoder;
use crate::event::EventDecoder;
use crate::header::{Footer, Header};
use crate::{
    Error, Event, Result, StackTable, CHUNK_EVENTS, CHUNK_FOOTER, CHUNK_HEADER, CHUNK_STACKS,
    CODEC_LZ4, CODEC_RAW, FORMAT_MAJOR, MAGIC, MAX_CHUNK_BYTES,
};

/// Longest call stack accepted from a file.
const MAX_STACK_DEPTH: u64 = 1 << 16;

/// Reads events one at a time; only one chunk is held in memory.
///
/// Stacks are filled in as the events that use them are reached, so `stacks()` is complete
/// for every event already returned.
pub struct TraceReader<R: Read> {
    input: R,
    header: Header,
    version: (u16, u16),
    stacks: StackTable,
    footer: Option<Footer>,
    chunk: Vec<u8>,
    pos: usize,
    decoder: EventDecoder,
    events_read: u64,
    done: bool,
}

impl<R: Read> TraceReader<R> {
    pub fn new(mut input: R) -> Result<Self> {
        let mut magic = [0u8; 8];
        if read_fully(&mut input, &mut magic)? != magic.len() || magic != MAGIC {
            return Err(Error::BadMagic);
        }
        let mut ver = [0u8; 4];
        input.read_exact(&mut ver)?;
        let major = u16::from_le_bytes([ver[0], ver[1]]);
        let minor = u16::from_le_bytes([ver[2], ver[3]]);
        if major != FORMAT_MAJOR {
            return Err(Error::UnsupportedVersion {
                found: major,
                supported: FORMAT_MAJOR,
            });
        }
        let (tag, payload) = read_chunk(&mut input)?.ok_or(Error::Truncated)?;
        if tag != CHUNK_HEADER {
            return Err(Error::Corrupt("first chunk is not the header"));
        }
        let header = Header::decode(&mut Decoder::new(&payload))?;
        Ok(Self {
            input,
            header,
            version: (major, minor),
            stacks: StackTable::default(),
            footer: None,
            chunk: Vec::new(),
            pos: 0,
            decoder: EventDecoder::default(),
            events_read: 0,
            done: false,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// `(major, minor)` of the file.
    pub fn version(&self) -> (u16, u16) {
        self.version
    }

    pub fn stacks(&self) -> &StackTable {
        &self.stacks
    }

    /// Set once the reader has returned every event of a cleanly closed capture.
    pub fn footer(&self) -> Option<&Footer> {
        self.footer.as_ref()
    }

    /// Next event, or `None` at the end. A file without a footer (the capture was killed)
    /// ends normally; check `footer()` afterwards to tell the difference.
    pub fn next_event(&mut self) -> Result<Option<Event>> {
        loop {
            if self.pos < self.chunk.len() {
                let mut d = Decoder::new(&self.chunk[self.pos..]);
                let event = self.decoder.decode(&mut d)?;
                self.pos += d.position();
                self.events_read += 1;
                return Ok(Some(event));
            }
            if self.done || !self.load_next_chunk()? {
                return Ok(None);
            }
        }
    }

    /// Read chunks until one holds events. Returns false at the end of the file.
    fn load_next_chunk(&mut self) -> Result<bool> {
        loop {
            let Some((tag, payload)) = read_chunk(&mut self.input)? else {
                self.done = true;
                return Ok(false);
            };
            match tag {
                CHUNK_STACKS => self.load_stacks(&payload)?,
                CHUNK_EVENTS => {
                    self.chunk = payload;
                    self.pos = 0;
                    self.decoder = EventDecoder::default();
                    return Ok(true);
                }
                CHUNK_FOOTER => {
                    let footer = Footer::decode(&mut Decoder::new(&payload))?;
                    if footer.event_count != self.events_read
                        || footer.stack_count != self.stacks.len() as u64
                    {
                        return Err(Error::Corrupt("footer counts do not match the content"));
                    }
                    self.footer = Some(footer);
                    self.done = true;
                    return Ok(false);
                }
                _ => return Err(Error::Corrupt("unexpected chunk type")),
            }
        }
    }

    fn load_stacks(&mut self, payload: &[u8]) -> Result<()> {
        let mut d = Decoder::new(payload);
        let count = d.u64()?;
        let mut frames = Vec::new();
        for _ in 0..count {
            let depth = d.u64()?;
            if depth > MAX_STACK_DEPTH || depth as usize > d.remaining() {
                return Err(Error::Corrupt("implausible stack depth"));
            }
            frames.clear();
            let mut prev = 0u64;
            for _ in 0..depth {
                prev = prev.wrapping_add(d.i64()? as u64);
                frames.push(prev);
            }
            self.stacks.push(&frames);
        }
        Ok(())
    }
}

impl<R: Read> Iterator for TraceReader<R> {
    type Item = Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_event() {
            Ok(event) => event.map(Ok),
            Err(e) => {
                // Do not try to resync after an error; the stream position is unknown.
                self.done = true;
                self.pos = self.chunk.len();
                Some(Err(e))
            }
        }
    }
}

fn read_fully<R: Read>(input: &mut R, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match input.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(n)
}

/// One chunk, decompressed and checksum-verified. `None` on a clean end between chunks.
fn read_chunk<R: Read>(input: &mut R) -> Result<Option<(u8, Vec<u8>)>> {
    let mut frame = [0u8; 14];
    match read_fully(input, &mut frame)? {
        0 => return Ok(None),
        14 => {}
        _ => return Err(Error::Truncated),
    }
    let tag = frame[0];
    let codec = frame[1];
    let raw_len = u32::from_le_bytes([frame[2], frame[3], frame[4], frame[5]]);
    let stored_len = u32::from_le_bytes([frame[6], frame[7], frame[8], frame[9]]);
    let crc = u32::from_le_bytes([frame[10], frame[11], frame[12], frame[13]]);
    if raw_len > MAX_CHUNK_BYTES || stored_len > MAX_CHUNK_BYTES {
        return Err(Error::Corrupt("chunk too large"));
    }
    let mut stored = vec![0u8; stored_len as usize];
    input.read_exact(&mut stored)?;
    if crc32fast::hash(&stored) != crc {
        return Err(Error::ChecksumMismatch);
    }
    let payload = match codec {
        CODEC_RAW if stored_len == raw_len => stored,
        CODEC_LZ4 => lz4_flex::block::decompress(&stored, raw_len as usize)
            .map_err(|_| Error::Corrupt("lz4 decompression failed"))?,
        _ => return Err(Error::Corrupt("unknown chunk codec")),
    };
    if payload.len() != raw_len as usize {
        return Err(Error::Corrupt("chunk length mismatch"));
    }
    Ok(Some((tag, payload)))
}
