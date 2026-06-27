//! SA-MP field primitives layered on the raw [`BitStreamReader`]/[`BitStreamWriter`].
//!
//! These mirror `samp/events/bitstream_io.lua`: the higher-level wire types (length-prefixed and
//! fixed strings, `bool8`/`bool32`, the compressed quaternion/vector forms) that the RPC and packet
//! codecs are built from. Compressed forms are computed in `f64` to match the reference Lua's
//! double-precision arithmetic, then narrowed to the wire integer.

use crate::bitstream::{BitStreamReader, BitStreamWriter};
use crate::{Quaternion, Result, Vector3};

/// Two `f32` components (`vector2d` in the reference). SA-MP uses this for gang-zone corners and
/// textdraw positions.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vector2 {
    pub x: f32,
    pub y: f32,
}

impl BitStreamWriter {
    pub fn write_i8(&mut self, value: i8) {
        self.write_u8(value as u8);
    }

    pub fn write_i16(&mut self, value: i16) {
        self.write_u16(value as u16);
    }

    /// Two `f32` components, x/y order.
    pub fn write_vector2(&mut self, v: Vector2) {
        self.write_f32(v.x);
        self.write_f32(v.y);
    }

    /// `bool8` — a boolean stored as a full `u8` (`0`/`1`).
    pub fn write_bool8(&mut self, value: bool) {
        self.write_u8(value as u8);
    }

    /// `bool32` — a boolean stored as a full `u32` (`0`/`1`).
    pub fn write_bool32(&mut self, value: bool) {
        self.write_u32(value as u32);
    }

    /// Length-prefixed string with a single `u8` length, from raw bytes (`string8`). Input longer
    /// than 255 bytes is truncated to keep the length prefix well-defined.
    pub fn write_str8_bytes(&mut self, value: &[u8]) {
        let len = value.len().min(u8::MAX as usize);
        self.write_u8(len as u8);
        self.write_bytes(&value[..len]);
    }

    /// Length-prefixed string with a `u16` length (`string16`).
    pub fn write_str16(&mut self, value: &[u8]) {
        let len = value.len().min(u16::MAX as usize);
        self.write_u16(len as u16);
        self.write_bytes(&value[..len]);
    }

    /// Length-prefixed string with a `u32` length (`string32`).
    pub fn write_str32(&mut self, value: &[u8]) {
        self.write_u32(value.len() as u32);
        self.write_bytes(value);
    }

    /// Fixed-width string padded with zero bytes to exactly `size` bytes (`fixedString32` uses
    /// `size = 32`). Input longer than `size` is truncated.
    pub fn write_fixed_string(&mut self, value: &[u8], size: usize) {
        let n = value.len().min(size);
        self.write_bytes(&value[..n]);
        for _ in n..size {
            self.write_u8(0);
        }
    }

    /// Three `f32` components, x/y/z order.
    pub fn write_vector3(&mut self, v: Vector3) {
        self.write_f32(v.x);
        self.write_f32(v.y);
        self.write_f32(v.z);
    }

    /// A float in `[-1, 1]` stored as `u16` (`compressedFloat`): `(clamp(v) + 1) * 32767.5`.
    pub fn write_compressed_float(&mut self, value: f32) {
        let v = (value as f64).clamp(-1.0, 1.0);
        self.write_u16(((v + 1.0) * 32767.5) as u16);
    }

    /// A direction+magnitude vector (`compressedVector`): `f32` magnitude followed by three
    /// `compressedFloat` components of the unit vector. A zero vector writes only the magnitude.
    pub fn write_compressed_vector(&mut self, v: Vector3) {
        let (x, y, z) = (v.x as f64, v.y as f64, v.z as f64);
        let magnitude = (x * x + y * y + z * z).sqrt();
        self.write_f32(magnitude as f32);
        if magnitude > 0.0 {
            self.write_compressed_float((x / magnitude) as f32);
            self.write_compressed_float((y / magnitude) as f32);
            self.write_compressed_float((z / magnitude) as f32);
        }
    }

    /// A normalised quaternion (`normQuat`): one sign bit each for `w/x/y/z`, then the magnitudes of
    /// `x/y/z` as `u16` (`abs * 65535`). `w` is reconstructed on read, so only its sign is sent.
    pub fn write_norm_quat(&mut self, q: Quaternion) {
        self.write_bit(q.w < 0.0);
        self.write_bit(q.x < 0.0);
        self.write_bit(q.y < 0.0);
        self.write_bit(q.z < 0.0);
        self.write_u16((q.x.abs() as f64 * 65535.0) as u16);
        self.write_u16((q.y.abs() as f64 * 65535.0) as u16);
        self.write_u16((q.z.abs() as f64 * 65535.0) as u16);
    }
}

impl BitStreamReader<'_> {
    pub fn read_i8(&mut self) -> Result<i8> {
        Ok(self.read_u8()? as i8)
    }

    pub fn read_i16(&mut self) -> Result<i16> {
        Ok(self.read_u16()? as i16)
    }

    /// Two `f32` components, x/y order.
    pub fn read_vector2(&mut self) -> Result<Vector2> {
        Ok(Vector2 {
            x: self.read_f32()?,
            y: self.read_f32()?,
        })
    }

    /// `bool8` — true when the `u8` is non-zero.
    pub fn read_bool8(&mut self) -> Result<bool> {
        Ok(self.read_u8()? != 0)
    }

    /// `bool32` — true when the `u32` is non-zero.
    pub fn read_bool32(&mut self) -> Result<bool> {
        Ok(self.read_u32()? != 0)
    }

    /// Length-prefixed string with a `u16` length (`string16`). Returns the raw bytes (callers
    /// transcode, e.g. via [`crate::decode_cp1251`]).
    pub fn read_str16(&mut self) -> Result<Vec<u8>> {
        let len = self.read_u16()? as usize;
        self.read_bytes(len)
    }

    /// Length-prefixed string with a `u32` length (`string32`).
    pub fn read_str32(&mut self) -> Result<Vec<u8>> {
        let len = self.read_u32()? as usize;
        self.read_bytes(len)
    }

    /// Length-prefixed string with a `u8` length, returned as raw bytes.
    pub fn read_str8_bytes(&mut self) -> Result<Vec<u8>> {
        let len = self.read_u8()? as usize;
        self.read_bytes(len)
    }

    /// Fixed-width string of `size` bytes, with trailing zero bytes stripped (`fixedString32` uses
    /// `size = 32`).
    pub fn read_fixed_string(&mut self, size: usize) -> Result<Vec<u8>> {
        let mut bytes = self.read_bytes(size)?;
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        Ok(bytes)
    }

    /// Three `f32` components, x/y/z order.
    pub fn read_vector3(&mut self) -> Result<Vector3> {
        Ok(Vector3 {
            x: self.read_f32()?,
            y: self.read_f32()?,
            z: self.read_f32()?,
        })
    }

    /// Inverse of [`BitStreamWriter::write_compressed_float`].
    pub fn read_compressed_float(&mut self) -> Result<f32> {
        let raw = self.read_u16()? as f64;
        Ok((raw / 32767.5 - 1.0) as f32)
    }

    /// Inverse of [`BitStreamWriter::write_compressed_vector`].
    pub fn read_compressed_vector(&mut self) -> Result<Vector3> {
        let magnitude = self.read_f32()? as f64;
        if magnitude == 0.0 {
            return Ok(Vector3::default());
        }
        let x = self.read_compressed_float()? as f64 * magnitude;
        let y = self.read_compressed_float()? as f64 * magnitude;
        let z = self.read_compressed_float()? as f64 * magnitude;
        Ok(Vector3 {
            x: x as f32,
            y: y as f32,
            z: z as f32,
        })
    }

    /// Inverse of [`BitStreamWriter::write_norm_quat`]. `w` is reconstructed from the unit-length
    /// constraint (`w = ±sqrt(1 - x² - y² - z²)`), clamped at zero to stay real.
    pub fn read_norm_quat(&mut self) -> Result<Quaternion> {
        let w_neg = self.read_bit()?;
        let x_neg = self.read_bit()?;
        let y_neg = self.read_bit()?;
        let z_neg = self.read_bit()?;
        let mut x = self.read_u16()? as f64 / 65535.0;
        let mut y = self.read_u16()? as f64 / 65535.0;
        let mut z = self.read_u16()? as f64 / 65535.0;
        if x_neg {
            x = -x;
        }
        if y_neg {
            y = -y;
        }
        if z_neg {
            z = -z;
        }
        let diff = (1.0 - x * x - y * y - z * z).max(0.0);
        let mut w = diff.sqrt();
        if w_neg {
            w = -w;
        }
        Ok(Quaternion {
            x: x as f32,
            y: y as f32,
            z: z as f32,
            w: w as f32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_and_string_widths_roundtrip() {
        let mut w = BitStreamWriter::new();
        w.write_bool8(true);
        w.write_bool32(false);
        w.write_str16(b"hello");
        w.write_str32(b"world!");
        w.write_fixed_string(b"abc", 8);
        let bytes = w.into_bytes();

        let mut r = BitStreamReader::new(&bytes);
        assert!(r.read_bool8().unwrap());
        assert!(!r.read_bool32().unwrap());
        assert_eq!(r.read_str16().unwrap(), b"hello");
        assert_eq!(r.read_str32().unwrap(), b"world!");
        assert_eq!(r.read_fixed_string(8).unwrap(), b"abc");
    }

    #[test]
    fn fixed_string_truncates_and_pads() {
        let mut w = BitStreamWriter::new();
        w.write_fixed_string(b"abcdefghij", 4); // truncated to 4
        let bytes = w.into_bytes();
        assert_eq!(bytes, b"abcd");
    }

    #[test]
    fn compressed_float_roundtrips_within_tolerance() {
        for v in [-1.0f32, -0.5, 0.0, 0.25, 0.999, 1.0] {
            let mut w = BitStreamWriter::new();
            w.write_compressed_float(v);
            let bytes = w.into_bytes();
            let mut r = BitStreamReader::new(&bytes);
            let got = r.read_compressed_float().unwrap();
            assert!((got - v).abs() < 1.0 / 32767.0, "v={v} got={got}");
        }
    }

    #[test]
    fn compressed_vector_roundtrips_direction_and_magnitude() {
        let v = Vector3 {
            x: 3.0,
            y: -4.0,
            z: 0.0,
        };
        let mut w = BitStreamWriter::new();
        w.write_compressed_vector(v);
        let bytes = w.into_bytes();
        let mut r = BitStreamReader::new(&bytes);
        let got = r.read_compressed_vector().unwrap();
        assert!((got.x - v.x).abs() < 0.01, "x {got:?}");
        assert!((got.y - v.y).abs() < 0.01, "y {got:?}");
        assert!((got.z - v.z).abs() < 0.01, "z {got:?}");
    }

    #[test]
    fn compressed_vector_zero_writes_magnitude_only() {
        let mut w = BitStreamWriter::new();
        w.write_compressed_vector(Vector3::default());
        let bytes = w.into_bytes();
        assert_eq!(bytes.len(), 4); // just the f32 magnitude (0.0)
        let mut r = BitStreamReader::new(&bytes);
        assert_eq!(r.read_compressed_vector().unwrap(), Vector3::default());
    }

    #[test]
    fn norm_quat_roundtrips_within_tolerance() {
        let q = Quaternion {
            x: 0.5,
            y: -0.5,
            z: 0.5,
            w: 0.5,
        };
        let mut w = BitStreamWriter::new();
        w.write_norm_quat(q);
        let bytes = w.into_bytes();
        let mut r = BitStreamReader::new(&bytes);
        let got = r.read_norm_quat().unwrap();
        assert!((got.x - q.x).abs() < 0.001, "x {got:?}");
        assert!((got.y - q.y).abs() < 0.001, "y {got:?}");
        assert!((got.z - q.z).abs() < 0.001, "z {got:?}");
        assert!((got.w - q.w).abs() < 0.001, "w {got:?}");
    }

    #[test]
    fn vector3_roundtrips_exactly() {
        let v = Vector3 {
            x: 1.5,
            y: -2.25,
            z: 100.0,
        };
        let mut w = BitStreamWriter::new();
        w.write_vector3(v);
        let bytes = w.into_bytes();
        let mut r = BitStreamReader::new(&bytes);
        assert_eq!(r.read_vector3().unwrap(), v);
    }
}
