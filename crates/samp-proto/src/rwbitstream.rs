//! Seekable read+write bitstream backing the Lua `bitStream` userdata — one buffer, independent
//! read/write cursors (`setReadOffset`/`setWriteOffset`) for in-place field rewrites, MSB-first.
//! See docs/memory/samp-proto/rwbitstream.md#module

use crate::{ProtoError, Result};

#[derive(Debug, Default, Clone)]
pub struct RwBitStream {
    data: Vec<u8>,
    /// Total valid bits (the high-water mark of writes / the length of a wrapped buffer).
    num_bits: usize,
    read_pos: usize,
    write_pos: usize,
}

impl RwBitStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap an existing payload for reading (and in-place rewriting). Cursors start at 0.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let num_bits = bytes.len() * 8;
        Self {
            data: bytes,
            num_bits,
            read_pos: 0,
            write_pos: 0,
        }
    }

    /// The valid bytes (`ceil(num_bits / 8)`).
    pub fn as_bytes(&self) -> &[u8] {
        &self.data[..self.num_bits.div_ceil(8)]
    }

    pub fn into_bytes(mut self) -> Vec<u8> {
        self.data.truncate(self.num_bits.div_ceil(8));
        self.data
    }

    pub fn bit_len(&self) -> usize {
        self.num_bits
    }

    pub fn read_offset(&self) -> usize {
        self.read_pos
    }

    pub fn write_offset(&self) -> usize {
        self.write_pos
    }

    pub fn set_read_offset(&mut self, bits: usize) {
        self.read_pos = bits;
    }

    pub fn set_write_offset(&mut self, bits: usize) {
        self.write_pos = bits;
    }

    /// Bits not yet read (`num_bits - read_pos`, saturating).
    pub fn num_unread_bits(&self) -> usize {
        self.num_bits.saturating_sub(self.read_pos)
    }

    pub fn num_unread_bytes(&self) -> usize {
        self.num_unread_bits() / 8
    }

    /// Advance the read cursor without interpreting (`ignoreBits`).
    pub fn ignore_bits(&mut self, bits: usize) {
        self.read_pos += bits;
    }

    /// Clear everything (`reset`).
    pub fn reset(&mut self) {
        self.data.clear();
        self.num_bits = 0;
        self.read_pos = 0;
        self.write_pos = 0;
    }

    fn write_bit_at(&mut self, pos: usize, bit: bool) {
        let byte = pos >> 3;
        if self.data.len() <= byte {
            self.data.resize(byte + 1, 0);
        }
        let mask = 0x80u8 >> (pos & 7); // MSB-first
        if bit {
            self.data[byte] |= mask;
        } else {
            self.data[byte] &= !mask;
        }
    }

    fn read_bit_at(&self, pos: usize) -> bool {
        (self.data[pos >> 3] & (0x80u8 >> (pos & 7))) != 0
    }

    /// Write `num_bits` of `input` (MSB-first) at the write cursor, overwriting; `right_aligned` shifts a trailing partial byte to its low bits.
    fn write_bits(&mut self, input: &[u8], num_bits: usize, right_aligned: bool) {
        let mut remaining = num_bits;
        let mut offset = 0;
        while remaining > 0 {
            let chunk = remaining.min(8);
            let mut byte = input[offset];
            if chunk < 8 && right_aligned {
                byte <<= 8 - chunk;
            }
            for i in 0..chunk {
                self.write_bit_at(self.write_pos, byte & (0x80 >> i) != 0);
                self.write_pos += 1;
            }
            remaining -= chunk;
            offset += 1;
        }
        if self.write_pos > self.num_bits {
            self.num_bits = self.write_pos;
        }
    }

    /// Read `num_bits` from the read cursor into `ceil(num_bits / 8)` bytes (MSB-first, `align_right` low-aligns a partial byte); errors when short.
    fn read_bits(&mut self, num_bits: usize, align_right: bool) -> Result<Vec<u8>> {
        if self.read_pos + num_bits > self.num_bits {
            return Err(ProtoError::Exhausted {
                needed: num_bits,
                available: self.num_unread_bits(),
            });
        }
        let mut out = vec![0u8; num_bits.div_ceil(8)];
        let mut done = 0;
        while done < num_bits {
            let chunk = (num_bits - done).min(8);
            let mut byte = 0u8;
            for i in 0..chunk {
                if self.read_bit_at(self.read_pos) {
                    byte |= 0x80 >> i;
                }
                self.read_pos += 1;
            }
            if chunk < 8 && align_right {
                byte >>= 8 - chunk;
            }
            out[done / 8] = byte;
            done += chunk;
        }
        Ok(out)
    }

    pub fn write_bool(&mut self, value: bool) {
        self.write_bits(&[value as u8], 1, true);
    }
    pub fn write_u8(&mut self, value: u8) {
        self.write_bits(&[value], 8, true);
    }
    pub fn write_u16(&mut self, value: u16) {
        self.write_bits(&value.to_le_bytes(), 16, true);
    }
    pub fn write_u32(&mut self, value: u32) {
        self.write_bits(&value.to_le_bytes(), 32, true);
    }
    pub fn write_i8(&mut self, value: i8) {
        self.write_u8(value as u8);
    }
    pub fn write_i16(&mut self, value: i16) {
        self.write_u16(value as u16);
    }
    pub fn write_i32(&mut self, value: i32) {
        self.write_u32(value as u32);
    }
    pub fn write_f32(&mut self, value: f32) {
        self.write_bits(&value.to_le_bytes(), 32, true);
    }
    /// Raw bytes, no length prefix (`bitStream:writeString`).
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_bits(bytes, bytes.len() * 8, true);
    }

    pub fn read_bool(&mut self) -> Result<bool> {
        Ok(self.read_bits(1, true)?[0] != 0)
    }
    pub fn read_u8(&mut self) -> Result<u8> {
        Ok(self.read_bits(8, true)?[0])
    }
    pub fn read_u16(&mut self) -> Result<u16> {
        let b = self.read_bits(16, true)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    pub fn read_u32(&mut self) -> Result<u32> {
        let b = self.read_bits(32, true)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn read_i8(&mut self) -> Result<i8> {
        Ok(self.read_u8()? as i8)
    }
    pub fn read_i16(&mut self) -> Result<i16> {
        Ok(self.read_u16()? as i16)
    }
    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(self.read_u32()? as i32)
    }
    pub fn read_f32(&mut self) -> Result<f32> {
        let b = self.read_bits(32, true)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// Read `len` raw bytes (`bitStream:readString(len)`).
    pub fn read_bytes(&mut self, len: usize) -> Result<Vec<u8>> {
        self.read_bits(len * 8, true)
    }

    /// `bitStream:writeEncoded` — SA-MP Huffman-encoded string ([`crate::encode_string`]). Encoded
    /// strings sit at a byte boundary, so the write cursor is aligned first.
    pub fn write_encoded(&mut self, input: &[u8]) {
        self.align_write_to_byte();
        let encoded = crate::encoded::encode_string(input);
        self.write_bytes(&encoded);
    }

    /// `bitStream:readEncoded(max_len)` — inverse of [`Self::write_encoded`]; decodes from the byte-aligned read cursor to the end, consuming the rest.
    pub fn read_encoded(&mut self, max_len: usize) -> Vec<u8> {
        self.align_read_to_byte();
        let start = self.read_pos / 8;
        let decoded = crate::encoded::decode_string(self.data.get(start..).unwrap_or(&[]), max_len);
        self.read_pos = self.num_bits;
        decoded
    }

    fn align_write_to_byte(&mut self) {
        let rem = self.write_pos & 7;
        if rem != 0 {
            self.write_pos += 8 - rem;
        }
    }

    fn align_read_to_byte(&mut self) {
        let rem = self.read_pos & 7;
        if rem != 0 {
            self.read_pos += 8 - rem;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_roundtrip() {
        let mut bs = RwBitStream::new();
        bs.write_u8(0xA5);
        bs.write_u16(0xBEEF);
        bs.write_u32(0xDEAD_BEEF);
        bs.write_i32(-12_345);
        bs.write_f32(std::f32::consts::PI);
        bs.write_bool(true);
        bs.write_bytes(b"hi");

        assert_eq!(bs.read_u8().unwrap(), 0xA5);
        assert_eq!(bs.read_u16().unwrap(), 0xBEEF);
        assert_eq!(bs.read_u32().unwrap(), 0xDEAD_BEEF);
        assert_eq!(bs.read_i32().unwrap(), -12_345);
        assert_eq!(bs.read_f32().unwrap(), std::f32::consts::PI);
        assert!(bs.read_bool().unwrap());
        assert_eq!(bs.read_bytes(2).unwrap(), b"hi");
    }

    #[test]
    fn byte_aligned_matches_little_endian() {
        let mut bs = RwBitStream::new();
        bs.write_u8(220);
        bs.write_u8(18);
        bs.write_u16(5);
        assert_eq!(bs.as_bytes(), &[220, 18, 5, 0]);
    }

    #[test]
    fn set_write_offset_overwrites_in_place() {
        let mut bs = RwBitStream::from_bytes(vec![0xFF; 4]);
        bs.set_write_offset(0);
        bs.write_u16(0x1234); // overwrite first two bytes
        assert_eq!(bs.as_bytes(), &[0x34, 0x12, 0xFF, 0xFF]);
    }

    #[test]
    fn set_read_offset_and_ignore_bits() {
        let mut bs = RwBitStream::from_bytes(vec![0x11, 0x22, 0x33, 0x44]);
        bs.ignore_bits(8);
        assert_eq!(bs.read_u8().unwrap(), 0x22);
        bs.set_read_offset(24);
        assert_eq!(bs.read_u8().unwrap(), 0x44);
    }

    #[test]
    fn unaligned_bits_roundtrip() {
        let mut bs = RwBitStream::new();
        bs.write_bool(true);
        bs.write_bool(true);
        bs.write_bool(false);
        bs.write_u32(0x1234_5678);
        bs.write_u16(0xABCD);

        assert!(bs.read_bool().unwrap());
        assert!(bs.read_bool().unwrap());
        assert!(!bs.read_bool().unwrap());
        assert_eq!(bs.read_u32().unwrap(), 0x1234_5678);
        assert_eq!(bs.read_u16().unwrap(), 0xABCD);
    }

    #[test]
    fn unread_counts_and_exhaustion() {
        let mut bs = RwBitStream::from_bytes(vec![1, 2]);
        assert_eq!(bs.num_unread_bits(), 16);
        assert_eq!(bs.num_unread_bytes(), 2);
        bs.read_u8().unwrap();
        assert_eq!(bs.num_unread_bytes(), 1);
        bs.read_u8().unwrap();
        assert!(bs.read_u8().is_err());
    }

    #[test]
    fn encoded_string_roundtrips_after_aligned_field() {
        let mut bs = RwBitStream::new();
        bs.write_u8(0x42); // a preceding byte-aligned field (like ShowDialog's style)
        bs.write_encoded(b"Login to your account.");
        bs.set_read_offset(0);
        assert_eq!(bs.read_u8().unwrap(), 0x42);
        assert_eq!(bs.read_encoded(256), b"Login to your account.");
    }

    #[test]
    fn matches_writer_byte_layout() {
        // Same sequence through the verified BitStreamWriter must produce identical bytes.
        let mut w = crate::BitStreamWriter::new();
        w.write_bit(true);
        w.write_u8(0x3C);
        w.write_u16(0x1234);
        let expected = w.into_bytes();

        let mut bs = RwBitStream::new();
        bs.write_bool(true);
        bs.write_u8(0x3C);
        bs.write_u16(0x1234);
        assert_eq!(bs.as_bytes(), expected.as_slice());
    }
}
