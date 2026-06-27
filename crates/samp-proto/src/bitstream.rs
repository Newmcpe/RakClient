//! RakNet-compatible bit stream, ported from `BitStream_WriteBits` (0x402180) and
//! `BitStream_ReadBits` (0x4022B0).
//!
//! Bit order: each source byte is packed most-significant-bit first into the stream. Multi-byte
//! integers are first laid out little-endian, then bit-packed, so a fully byte-aligned stream is
//! identical to a plain little-endian buffer.

use crate::{ProtoError, Result};

#[derive(Debug, Default, Clone)]
pub struct BitStreamWriter {
    data: Vec<u8>,
    num_bits: usize,
}

impl BitStreamWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of bits written so far.
    pub fn bit_len(&self) -> usize {
        self.num_bits
    }

    /// Core writer: packs `num_bits` from `input` (MSB-first per byte) at the current position.
    /// `right_aligned` only affects a trailing partial (`< 8`) byte, matching the binary's
    /// `rightAlignedBits` argument. `input` must hold at least `ceil(num_bits / 8)` bytes.
    fn write_bits(&mut self, input: &[u8], mut num_bits_to_write: usize, right_aligned: bool) {
        if num_bits_to_write == 0 {
            return;
        }
        let needed = (self.num_bits + num_bits_to_write).div_ceil(8);
        if self.data.len() < needed {
            self.data.resize(needed, 0);
        }
        let num_bits_used_mod8 = self.num_bits & 7;
        let mut offset = 0usize;
        while num_bits_to_write > 0 {
            let mut data_byte = input[offset];
            if num_bits_to_write < 8 && right_aligned {
                data_byte <<= 8 - num_bits_to_write;
            }
            let byte_index = self.num_bits >> 3;
            if num_bits_used_mod8 == 0 {
                self.data[byte_index] = data_byte;
            } else {
                self.data[byte_index] |= data_byte >> num_bits_used_mod8;
                let written_first = 8 - num_bits_used_mod8;
                if written_first < num_bits_to_write {
                    self.data[byte_index + 1] = data_byte << written_first;
                }
            }
            self.num_bits += num_bits_to_write.min(8);
            num_bits_to_write = num_bits_to_write.saturating_sub(8);
            offset += 1;
        }
    }

    /// Advance the write cursor by `num_bits` zero bits (protocol padding / unmodelled fields).
    pub fn write_zero_bits(&mut self, num_bits: usize) {
        if num_bits == 0 {
            return;
        }
        let needed = (self.num_bits + num_bits).div_ceil(8);
        if self.data.len() < needed {
            self.data.resize(needed, 0);
        }
        self.num_bits += num_bits;
    }

    pub fn write_bit(&mut self, value: bool) {
        self.write_bits(&[value as u8], 1, true);
    }

    /// Write the low `count` (`<= 8`) bits of `value`, MSB-first, matching RakNet
    /// `BitStream::WriteBits(&value, count, true)`.
    pub fn write_bits_low(&mut self, value: u8, count: usize) {
        self.write_bits(&[value], count, true);
    }

    /// Pad with zero bits up to the next byte boundary (RakNet `AlignWriteToByteBoundary`).
    pub fn align_to_byte(&mut self) {
        let rem = self.num_bits & 7;
        if rem != 0 {
            self.write_zero_bits(8 - rem);
        }
    }

    /// RakNet `BitStream::WriteCompressed` for an unsigned little-endian value: high zero bytes are
    /// each encoded as a single `1` bit; the first non-zero byte is preceded by a `0` bit followed by
    /// every byte from that point down to the lowest; the lowest byte is encoded as `1`+low-nibble
    /// when its high nibble is zero, otherwise `0`+full-byte.
    fn write_compressed(&mut self, input: &[u8]) {
        let mut current_byte = input.len() - 1;
        while current_byte > 0 {
            if input[current_byte] == 0 {
                self.write_bit(true);
            } else {
                self.write_bit(false);
                self.write_bits(&input[..=current_byte], (current_byte + 1) * 8, true);
                return;
            }
            current_byte -= 1;
        }
        if input[0] & 0xF0 == 0 {
            self.write_bit(true);
            self.write_bits(&[input[0]], 4, true);
        } else {
            self.write_bit(false);
            self.write_bits(&[input[0]], 8, true);
        }
    }

    pub fn write_compressed_u16(&mut self, value: u16) {
        self.write_compressed(&value.to_le_bytes());
    }

    pub fn write_compressed_u32(&mut self, value: u32) {
        self.write_compressed(&value.to_le_bytes());
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

    pub fn write_i32(&mut self, value: i32) {
        self.write_bits(&value.to_le_bytes(), 32, true);
    }

    pub fn write_f32(&mut self, value: f32) {
        self.write_bits(&value.to_le_bytes(), 32, true);
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_bits(bytes, bytes.len() * 8, true);
    }

    /// Length-prefixed string with a single `u8` length byte (SA-MP convention). Strings longer
    /// than 255 bytes are truncated to 255 to keep the length prefix well-defined.
    pub fn write_str8(&mut self, value: &str) {
        let bytes = value.as_bytes();
        let len = bytes.len().min(u8::MAX as usize);
        self.write_u8(len as u8);
        self.write_bytes(&bytes[..len]);
    }

    pub fn into_bytes(mut self) -> Vec<u8> {
        self.data.truncate(self.num_bits.div_ceil(8));
        self.data
    }
}

#[derive(Debug, Clone)]
pub struct BitStreamReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitStreamReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    /// Bits remaining to be read.
    pub fn bits_left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.bit_pos)
    }

    /// Core reader: extracts `num_bits` (MSB-first per byte) into a fresh buffer, mirroring the
    /// binary. Returns [`ProtoError::Exhausted`] when the stream does not hold enough bits.
    fn read_bits(&mut self, num_bits_to_read: usize, align_right: bool) -> Result<Vec<u8>> {
        if num_bits_to_read == 0 {
            return Ok(Vec::new());
        }
        let total_bits = self.data.len() * 8;
        if self.bit_pos + num_bits_to_read > total_bits {
            return Err(ProtoError::Exhausted {
                needed: num_bits_to_read,
                available: total_bits - self.bit_pos,
            });
        }
        let mut output = vec![0u8; num_bits_to_read.div_ceil(8)];
        let read_offset_mod8 = self.bit_pos & 7;
        let mut bits_left = num_bits_to_read as i64;
        let mut offset = 0usize;
        loop {
            let byte_index = self.bit_pos >> 3;
            output[offset] |= self.data[byte_index] << read_offset_mod8;
            if read_offset_mod8 != 0 && bits_left > (8 - read_offset_mod8) as i64 {
                output[offset] |= self.data[byte_index + 1] >> (8 - read_offset_mod8);
            }
            bits_left -= 8;
            if bits_left < 0 {
                if align_right {
                    output[offset] >>= -bits_left;
                }
                self.bit_pos += (8 + bits_left) as usize;
            } else {
                self.bit_pos += 8;
            }
            offset += 1;
            if bits_left <= 0 {
                break;
            }
        }
        Ok(output)
    }

    /// Skip `num_bits` without interpreting them (unmodelled fields in larger packets).
    pub fn skip_bits(&mut self, num_bits: usize) -> Result<()> {
        let available = self.bits_left();
        if num_bits > available {
            return Err(ProtoError::Exhausted {
                needed: num_bits,
                available,
            });
        }
        self.bit_pos += num_bits;
        Ok(())
    }

    pub fn read_bit(&mut self) -> Result<bool> {
        Ok(self.read_bits(1, true)?[0] != 0)
    }

    /// Read `count` (`<= 8`) bits, MSB-first, returning them right-aligned in the low bits of the
    /// result (RakNet `BitStream::ReadBits(&value, count, true)`).
    pub fn read_bits_low(&mut self, count: usize) -> Result<u8> {
        Ok(self.read_bits(count, true)?[0])
    }

    /// Advance the read cursor to the next byte boundary (RakNet `AlignReadToByteBoundary`).
    pub fn align_to_byte(&mut self) {
        let rem = self.bit_pos & 7;
        if rem != 0 {
            self.bit_pos += 8 - rem;
        }
    }

    /// Inverse of [`BitStreamWriter::write_compressed_u16`]/`_u32`. `size_bytes` is the width of the
    /// original little-endian value (2 or 4).
    fn read_compressed(&mut self, size_bytes: usize) -> Result<Vec<u8>> {
        let mut output = vec![0u8; size_bytes];
        let mut current_byte = size_bytes - 1;
        while current_byte > 0 {
            if self.read_bit()? {
                output[current_byte] = 0;
            } else {
                let bytes = self.read_bits((current_byte + 1) * 8, true)?;
                output[..=current_byte].copy_from_slice(&bytes[..=current_byte]);
                return Ok(output);
            }
            current_byte -= 1;
        }
        if self.read_bit()? {
            output[0] = self.read_bits(4, true)?[0] & 0x0F;
        } else {
            output[0] = self.read_bits(8, true)?[0];
        }
        Ok(output)
    }

    pub fn read_compressed_u16(&mut self) -> Result<u16> {
        let b = self.read_compressed(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn read_compressed_u32(&mut self) -> Result<u32> {
        let b = self.read_compressed(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
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

    pub fn read_i32(&mut self) -> Result<i32> {
        let b = self.read_bits(32, true)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn read_f32(&mut self) -> Result<f32> {
        let b = self.read_bits(32, true)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read `len` raw bytes from the current (possibly non-byte-aligned) position.
    pub fn read_bytes(&mut self, len: usize) -> Result<Vec<u8>> {
        self.read_bits(len * 8, true)
    }

    /// Read exactly `num_bits` bits (consuming only that many), returning them packed MSB-first
    /// into `ceil(num_bits / 8)` bytes. Mirrors RakNet `ReadBits(.., num_bits, false)`, used for
    /// RPC bodies whose bit length is not a multiple of 8.
    pub fn read_bits_bytes(&mut self, num_bits: usize) -> Result<Vec<u8>> {
        self.read_bits(num_bits, false)
    }

    pub fn read_str8(&mut self) -> Result<String> {
        let len = self.read_u8()? as usize;
        let bytes = self.read_bytes(len)?;
        String::from_utf8(bytes).map_err(|_| ProtoError::InvalidString)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_each_primitive() {
        let mut w = BitStreamWriter::new();
        w.write_bit(true);
        w.write_bit(false);
        w.write_u8(0xA5);
        w.write_u16(0xBEEF);
        w.write_u32(0xDEAD_BEEF);
        w.write_i32(-12_345_678);
        w.write_f32(std::f32::consts::PI);
        w.write_bytes(&[1, 2, 3, 4, 5]);
        w.write_str8("hello world");
        let bytes = w.into_bytes();

        let mut r = BitStreamReader::new(&bytes);
        assert!(r.read_bit().unwrap());
        assert!(!r.read_bit().unwrap());
        assert_eq!(r.read_u8().unwrap(), 0xA5);
        assert_eq!(r.read_u16().unwrap(), 0xBEEF);
        assert_eq!(r.read_u32().unwrap(), 0xDEAD_BEEF);
        assert_eq!(r.read_i32().unwrap(), -12_345_678);
        assert_eq!(r.read_f32().unwrap(), std::f32::consts::PI);
        assert_eq!(r.read_bytes(5).unwrap(), vec![1, 2, 3, 4, 5]);
        assert_eq!(r.read_str8().unwrap(), "hello world");
    }

    #[test]
    fn aligned_u32_is_plain_little_endian() {
        let mut w = BitStreamWriter::new();
        w.write_u32(0x0000_0FD9);
        assert_eq!(w.into_bytes(), vec![0xD9, 0x0F, 0x00, 0x00]);
    }

    #[test]
    fn bit_packing_is_msb_first() {
        let mut w = BitStreamWriter::new();
        // 1011 packed MSB-first into the top nibble of the first byte.
        w.write_bit(true);
        w.write_bit(false);
        w.write_bit(true);
        w.write_bit(true);
        assert_eq!(w.into_bytes(), vec![0b1011_0000]);
    }

    #[test]
    fn unaligned_values_roundtrip() {
        let mut w = BitStreamWriter::new();
        w.write_bit(true);
        w.write_bit(true);
        w.write_bit(false);
        w.write_u32(0x1234_5678);
        w.write_u16(0xABCD);
        let bytes = w.into_bytes();

        let mut r = BitStreamReader::new(&bytes);
        assert!(r.read_bit().unwrap());
        assert!(r.read_bit().unwrap());
        assert!(!r.read_bit().unwrap());
        assert_eq!(r.read_u32().unwrap(), 0x1234_5678);
        assert_eq!(r.read_u16().unwrap(), 0xABCD);
    }

    #[test]
    fn skip_bits_matches_offset_arithmetic() {
        let mut w = BitStreamWriter::new();
        w.write_zero_bits(13);
        w.write_u16(0x7E1F);
        let bytes = w.into_bytes();

        let mut r = BitStreamReader::new(&bytes);
        r.skip_bits(13).unwrap();
        assert_eq!(r.read_u16().unwrap(), 0x7E1F);
    }

    #[test]
    fn read_past_end_errs() {
        let bytes = [0xFFu8; 2];
        let mut r = BitStreamReader::new(&bytes);
        assert_eq!(r.read_u16().unwrap(), 0xFFFF);
        assert!(matches!(
            r.read_u8(),
            Err(ProtoError::Exhausted {
                needed: 8,
                available: 0
            })
        ));
    }

    #[test]
    fn skip_past_end_errs() {
        let bytes = [0u8; 1];
        let mut r = BitStreamReader::new(&bytes);
        assert!(r.skip_bits(9).is_err());
    }

    #[test]
    fn compressed_round_trips_and_is_compact() {
        for v in [0u16, 1, 5, 0x40, 0xFF, 0x0100, 0x1234, 0xFFFF] {
            let mut w = BitStreamWriter::new();
            w.write_compressed_u16(v);
            let bytes = w.into_bytes();
            let mut r = BitStreamReader::new(&bytes);
            assert_eq!(r.read_compressed_u16().unwrap(), v, "u16 {v:#06x}");
        }
        for v in [
            0u32,
            1,
            0x0F,
            0x80,
            0xFFFF,
            0x0001_0000,
            0xDEAD_BEEF,
            u32::MAX,
        ] {
            let mut w = BitStreamWriter::new();
            w.write_compressed_u32(v);
            let bytes = w.into_bytes();
            let mut r = BitStreamReader::new(&bytes);
            assert_eq!(r.read_compressed_u32().unwrap(), v, "u32 {v:#010x}");
        }
        // Zero compresses to 6 bits (`1` high-byte skip + `1` low-nibble flag + 4 zero bits).
        let mut w = BitStreamWriter::new();
        w.write_compressed_u16(0);
        assert_eq!(w.into_bytes(), vec![0b1100_0000]);
    }

    #[test]
    fn compressed_mixed_with_other_fields() {
        let mut w = BitStreamWriter::new();
        w.write_bit(true);
        w.write_compressed_u32(300);
        w.write_bits_low(0b101, 3);
        w.align_to_byte();
        w.write_u16(0xABCD);
        let bytes = w.into_bytes();

        let mut r = BitStreamReader::new(&bytes);
        assert!(r.read_bit().unwrap());
        assert_eq!(r.read_compressed_u32().unwrap(), 300);
        assert_eq!(r.read_bits_low(3).unwrap(), 0b101);
        r.align_to_byte();
        assert_eq!(r.read_u16().unwrap(), 0xABCD);
    }

    #[test]
    fn read_str8_invalid_utf8_errs() {
        // length 1, then a lone continuation byte 0x80.
        let bytes = [0x01, 0x80];
        let mut r = BitStreamReader::new(&bytes);
        assert_eq!(r.read_str8(), Err(ProtoError::InvalidString));
    }

    #[test]
    fn str8_truncates_over_255_bytes() {
        let long = "x".repeat(300);
        let mut w = BitStreamWriter::new();
        w.write_str8(&long);
        let bytes = w.into_bytes();
        assert_eq!(bytes[0], 255);
        assert_eq!(bytes.len(), 1 + 255);
    }
}
