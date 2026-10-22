//! LEB128 varints, zigzag deltas and a bounds-checked decoder.

use crate::{Error, Result};

pub(crate) fn put_u64(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

pub(crate) fn put_i64(buf: &mut Vec<u8>, v: i64) {
    put_u64(buf, ((v << 1) ^ (v >> 63)) as u64);
}

pub(crate) fn put_str(buf: &mut Vec<u8>, s: &str) {
    put_u64(buf, s.len() as u64);
    buf.extend_from_slice(s.as_bytes());
}

pub(crate) struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or(Error::Corrupt("unexpected end of chunk"))?;
        self.pos += 1;
        Ok(b)
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..70).step_by(7) {
            let b = self.u8()?;
            // The tenth byte may only carry the top bit of a u64.
            if shift == 63 && b > 1 {
                return Err(Error::Corrupt("varint overflow"));
            }
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(Error::Corrupt("varint too long"))
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        u32::try_from(self.u64()?).map_err(|_| Error::Corrupt("value exceeds u32"))
    }

    pub(crate) fn i64(&mut self) -> Result<i64> {
        let v = self.u64()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }

    pub(crate) fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(Error::Corrupt("unexpected end of chunk"));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub(crate) fn str(&mut self) -> Result<String> {
        let n = usize::try_from(self.u64()?).map_err(|_| Error::Corrupt("string too long"))?;
        let b = self.bytes(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| Error::Corrupt("invalid utf-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip_edges() {
        let values = [
            0u64,
            1,
            127,
            128,
            16_383,
            16_384,
            u64::from(u32::MAX),
            u64::MAX,
        ];
        let mut buf = Vec::new();
        for v in values {
            put_u64(&mut buf, v);
        }
        let mut d = Decoder::new(&buf);
        for v in values {
            assert_eq!(d.u64().unwrap(), v);
        }
        assert_eq!(d.remaining(), 0);
    }

    #[test]
    fn zigzag_roundtrip_edges() {
        let values = [0i64, 1, -1, 63, -64, i64::MAX, i64::MIN];
        let mut buf = Vec::new();
        for v in values {
            put_i64(&mut buf, v);
        }
        let mut d = Decoder::new(&buf);
        for v in values {
            assert_eq!(d.i64().unwrap(), v);
        }
    }

    #[test]
    fn overlong_varint_is_rejected() {
        let buf = [0xffu8; 11];
        assert!(Decoder::new(&buf).u64().is_err());
        let mut over = vec![0xffu8; 9];
        over.push(0x02);
        assert!(Decoder::new(&over).u64().is_err());
    }

    #[test]
    fn short_string_is_rejected() {
        let mut buf = Vec::new();
        put_u64(&mut buf, 10);
        buf.extend_from_slice(b"abc");
        assert!(Decoder::new(&buf).str().is_err());
    }
}
