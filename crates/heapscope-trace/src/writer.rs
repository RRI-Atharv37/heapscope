//! Chunked, LZ4-compressed trace writer.

use std::collections::HashMap;
use std::io::Write;

use crate::codec::{put_i64, put_u64};
use crate::event::{EventEncoder, StackId, NO_STACK};
use crate::header::{Dropped, Footer, Header};
use crate::{
    Event, Result, CHUNK_EVENTS, CHUNK_FOOTER, CHUNK_HEADER, CHUNK_STACKS, CODEC_LZ4, CODEC_RAW,
    FORMAT_MAJOR, FORMAT_MINOR, MAGIC,
};

/// Raw bytes of events buffered before a chunk is written.
const CHUNK_TARGET: usize = 256 << 10;

pub struct TraceWriter<W: Write> {
    out: W,
    stack_ids: HashMap<Box<[u64]>, StackId>,
    pending_stacks: Vec<u8>,
    pending_stack_count: u64,
    events: Vec<u8>,
    encoder: EventEncoder,
    event_count: u64,
    bytes_written: u64,
}

impl<W: Write> TraceWriter<W> {
    pub fn new(mut out: W, header: &Header) -> Result<Self> {
        out.write_all(&MAGIC)?;
        out.write_all(&FORMAT_MAJOR.to_le_bytes())?;
        out.write_all(&FORMAT_MINOR.to_le_bytes())?;
        let mut payload = Vec::new();
        header.encode(&mut payload);
        let mut w = Self {
            out,
            stack_ids: HashMap::new(),
            pending_stacks: Vec::new(),
            pending_stack_count: 0,
            events: Vec::with_capacity(CHUNK_TARGET + 1024),
            encoder: EventEncoder::default(),
            event_count: 0,
            bytes_written: (MAGIC.len() + 4) as u64,
        };
        w.write_chunk(CHUNK_HEADER, &payload, false)?;
        Ok(w)
    }

    /// Intern a call stack (innermost frame first) and return its id. An empty stack is
    /// `NO_STACK`. The stack is written before the first chunk of events that can use it.
    pub fn intern_stack(&mut self, frames: &[u64]) -> StackId {
        if frames.is_empty() {
            return NO_STACK;
        }
        if let Some(&id) = self.stack_ids.get(frames) {
            return id;
        }
        let id = StackId::try_from(self.stack_ids.len() + 1).expect("more than u32::MAX stacks");
        put_u64(&mut self.pending_stacks, frames.len() as u64);
        let mut prev = 0u64;
        for &f in frames {
            put_i64(&mut self.pending_stacks, f.wrapping_sub(prev) as i64);
            prev = f;
        }
        self.pending_stack_count += 1;
        self.stack_ids.insert(frames.into(), id);
        id
    }

    pub fn write_event(&mut self, event: &Event) -> Result<()> {
        self.encoder.encode(event, &mut self.events);
        self.event_count += 1;
        if self.events.len() >= CHUNK_TARGET {
            self.flush_chunks()?;
        }
        Ok(())
    }

    /// Bytes handed to the underlying writer so far (events still buffered are excluded).
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Write everything buffered and the footer, and return the underlying writer.
    pub fn finish(mut self, dropped: Dropped) -> Result<W> {
        self.flush_chunks()?;
        let footer = Footer {
            event_count: self.event_count,
            stack_count: self.stack_ids.len() as u64,
            dropped,
        };
        let mut payload = Vec::new();
        footer.encode(&mut payload);
        self.write_chunk(CHUNK_FOOTER, &payload, false)?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn flush_chunks(&mut self) -> Result<()> {
        if self.pending_stack_count > 0 {
            let mut payload = Vec::with_capacity(self.pending_stacks.len() + 10);
            put_u64(&mut payload, self.pending_stack_count);
            payload.extend_from_slice(&self.pending_stacks);
            self.write_chunk(CHUNK_STACKS, &payload, true)?;
            self.pending_stacks.clear();
            self.pending_stack_count = 0;
        }
        if !self.events.is_empty() {
            let payload = std::mem::take(&mut self.events);
            self.write_chunk(CHUNK_EVENTS, &payload, true)?;
            self.events = payload;
            self.events.clear();
            self.encoder = EventEncoder::default();
        }
        Ok(())
    }

    fn write_chunk(&mut self, tag: u8, raw: &[u8], compress: bool) -> Result<()> {
        let compressed;
        let mut codec = CODEC_RAW;
        let mut stored = raw;
        if compress && raw.len() >= 64 {
            compressed = lz4_flex::block::compress(raw);
            if compressed.len() < raw.len() {
                codec = CODEC_LZ4;
                stored = &compressed;
            }
        }
        let mut frame = [0u8; 14];
        frame[0] = tag;
        frame[1] = codec;
        frame[2..6].copy_from_slice(&(raw.len() as u32).to_le_bytes());
        frame[6..10].copy_from_slice(&(stored.len() as u32).to_le_bytes());
        frame[10..14].copy_from_slice(&crc32fast::hash(stored).to_le_bytes());
        self.out.write_all(&frame)?;
        self.out.write_all(stored)?;
        self.bytes_written += (frame.len() + stored.len()) as u64;
        Ok(())
    }
}
